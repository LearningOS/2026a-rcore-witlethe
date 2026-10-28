//! Implementation of [`MapArea`] and [`MemorySet`].
use super::{frame_alloc, FrameTracker};
use super::{PTEFlags, PageTable, PageTableEntry};
use super::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use super::{StepByOne, VPNRange};
use crate::config::{
    MEMORY_END, PAGE_SIZE, TRAMPOLINE, TRAP_CONTEXT_BASE, USER_STACK_SIZE,
};
use crate::sync::UPSafeCell;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::asm;
use lazy_static::*;
use riscv::register::satp;

extern "C" {
    fn stext();
    fn etext();
    fn srodata();
    fn erodata();
    fn sdata();
    fn edata();
    fn sbss_with_stack();
    fn ebss();
    fn ekernel();
    fn strampoline();
}

lazy_static! {
    /// The kernel's initial address space.
    ///
    /// It contains identity mappings for the kernel code, data, physical memory,
    /// the trampoline page, and dynamically inserted kernel stacks.
    pub static ref KERNEL_SPACE: Arc<UPSafeCell<MemorySet>> =
        Arc::new(unsafe { UPSafeCell::new(MemorySet::new_kernel()) });
}

/// An address space consisting of a root page table and its mapped virtual-memory areas.
pub struct MemorySet {
    page_table: PageTable,
    areas: Vec<MapArea>,
}

impl MemorySet {
    /// [new_bare ()] Create an address space with an empty page-table hierarchy and no `MapArea`s.
    ///
    /// The caller is responsible for adding mappings needed by that address space.
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
        }
    }

    /// [token self] Return the SATP token for this address space.
    ///
    /// The token can be stored in a task control block or passed to address
    /// translation helpers, but does not by itself activate the address space.
    pub fn token(&self) -> usize {
        self.page_table.token()
    }

    /// [map_user_range mutself start_vpn len permission] Map a user-accessible range beginning at [start_vpn].
    ///
    /// [len] is rounded up to whole pages. The range is added only if [len] is
    /// nonzero, it fits the supported user address space, and every VPN in it is
    /// unmapped. Return `true` on success and `false` otherwise. Page-aligned
    /// mapped frames are recorded in a new `MapArea` with [permission].
    pub fn map_user_range(
        &mut self,
        start_vpn: VirtPageNum,
        len: usize,
        permission: MapPermission,
    ) -> bool {
        if len == 0 {
            return false;
        }

        let page_count = len.div_ceil(PAGE_SIZE);
        let Some(end_vpn) = start_vpn.0.checked_add(page_count) else {
            return false;
        };

        if end_vpn > 1 << 27 {
            return false;
        }

        for vpn in VPNRange::new(start_vpn, VirtPageNum(end_vpn)) {
            if self
                .page_table
                .translate(vpn)
                .is_some_and(|pte| pte.is_valid())
            {
                return false;
            }
        }

        self.push(
            MapArea::new(
                VirtAddr::from(start_vpn),
                VirtAddr::from(VirtPageNum(end_vpn)),
                MapType::Framed,
                permission,
            ),
            None,
        );

        true
    }

    /// [unmap_user_range mutself start_vpn len] Unmap and remove an existing framed user range.
    ///
    /// [len] is rounded up to whole pages. Return `true` only when the exact
    /// `[start_vpn, start_vpn + len]` interval belongs to one `MapArea` of
    /// `MapType::Framed`; then the mappings and physical frames are released.
    /// Return `false` for zero length, overflow, or a nonmatching area.
    pub fn unmap_user_range(
        &mut self,
        start_vpn: VirtPageNum,
        len: usize,
    ) -> bool {
        if len == 0 {
            return false;
        }

        let page_count = len.div_ceil(PAGE_SIZE);
        let Some(end_vpn) = start_vpn.0.checked_add(page_count) else {
            return false;
        };

        let Some(index) = self.areas.iter().position(|area| {
            area.map_type == MapType::Framed
                && area.vpn_range.get_start() == start_vpn
                && area.vpn_range.get_end() == VirtPageNum(end_vpn)
        }) else {
            return false;
        };

        let mut area = self.areas.remove(index);
        area.unmap(&mut self.page_table);
        true
    }

    /// [insert_framed_area mutself start_va end_va permission] Insert a new framed mapping area.
    ///
    /// [start_va] and [end_va] are rounded to the enclosing virtual page range.
    /// This method assumes that the target range does not conflict with existing mappings.
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
    ) {
        self.push(
            MapArea::new(start_va, end_va, MapType::Framed, permission),
            None,
        );
    }

    /// [remove_area_with_start_vpn mutself start_vpn] Remove the mapping area whose first VPN is [start_vpn].
    ///
    /// The corresponding page-table mappings and any framed physical frames
    /// are released. If no area starts at [start_vpn], this is a no-op.
    pub fn remove_area_with_start_vpn(&mut self, start_vpn: VirtPageNum) {
        if let Some((idx, area)) = self
            .areas
            .iter_mut()
            .enumerate()
            .find(|(_, area)| area.vpn_range.get_start() == start_vpn)
        {
            area.unmap(&mut self.page_table);
            self.areas.remove(idx);
        }
    }

    /// [push mutself map_area data] Add [map_area] to this address space and establish all of its mappings.
    ///
    /// When [data] is present, copy it into the newly allocated framed pages.
    /// This method assumes that no virtual page in [map_area] is already mapped.
    fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>) {
        map_area.map(&mut self.page_table);
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data);
        }
        self.areas.push(map_area);
    }

    /// [map_trampoline mutself] Map the fixed trampoline page into this address space.
    ///
    /// The trampoline is managed directly as a page-table entry and is not
    /// recorded in the `areas` list.
    fn map_trampoline(&mut self) {
        self.page_table.map(
            VirtAddr::from(TRAMPOLINE).into(),
            PhysAddr::from(strampoline as usize).into(),
            PTEFlags::R | PTEFlags::X,
        );
    }

    /// [new_kernel ()] Create the initial kernel address space without kernel-stack mappings.
    ///
    /// This function maps the kernel text, read-only data, writable data/BSS,
    /// physical memory, and trampoline in identity mode where applicable.
    /// Kernel stacks are inserted later by their allocation paths.
    pub fn new_kernel() -> Self {
        let mut memory_set = Self::new_bare();
        // map trampoline
        memory_set.map_trampoline();
        // map kernel sections
        info!(".text [{:#x}, {:#x})", stext as usize, etext as usize);
        info!(".rodata [{:#x}, {:#x})", srodata as usize, erodata as usize);
        info!(".data [{:#x}, {:#x})", sdata as usize, edata as usize);
        info!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as usize, ebss as usize
        );
        info!("mapping .text section");
        memory_set.push(
            MapArea::new(
                (stext as usize).into(),
                (etext as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::X,
            ),
            None,
        );
        info!("mapping .rodata section");
        memory_set.push(
            MapArea::new(
                (srodata as usize).into(),
                (erodata as usize).into(),
                MapType::Identical,
                MapPermission::R,
            ),
            None,
        );
        info!("mapping .data section");
        memory_set.push(
            MapArea::new(
                (sdata as usize).into(),
                (edata as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        info!("mapping .bss section");
        memory_set.push(
            MapArea::new(
                (sbss_with_stack as usize).into(),
                (ebss as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        info!("mapping physical memory");
        memory_set.push(
            MapArea::new(
                (ekernel as usize).into(),
                MEMORY_END.into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        memory_set
    }

    /// [from_elf elf_data] Build a user address space from an ELF image.
    ///
    /// Map loadable ELF segments, a guard area and user stack, the TrapContext,
    /// and the trampoline. Return `(memory_set, user_stack_top, entry_point)`.
    pub fn from_elf(elf_data: &[u8]) -> (Self, usize, usize) {
        let mut memory_set = Self::new_bare();
        // map trampoline
        memory_set.map_trampoline();
        // map program headers of elf, with U flag
        let elf = xmas_elf::ElfFile::new(elf_data).unwrap();
        let elf_header = elf.header;
        let magic = elf_header.pt1.magic;
        assert_eq!(magic, [0x7f, 0x45, 0x4c, 0x46], "invalid elf!");
        let ph_count = elf_header.pt2.ph_count();
        let mut max_end_vpn = VirtPageNum(0);
        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Load {
                let start_va: VirtAddr = (ph.virtual_addr() as usize).into();
                let end_va: VirtAddr =
                    ((ph.virtual_addr() + ph.mem_size()) as usize).into();
                let mut map_perm = MapPermission::U;
                let ph_flags = ph.flags();
                if ph_flags.is_read() {
                    map_perm |= MapPermission::R;
                }
                if ph_flags.is_write() {
                    map_perm |= MapPermission::W;
                }
                if ph_flags.is_execute() {
                    map_perm |= MapPermission::X;
                }
                let map_area =
                    MapArea::new(start_va, end_va, MapType::Framed, map_perm);
                max_end_vpn = map_area.vpn_range.get_end();
                memory_set.push(
                    map_area,
                    Some(
                        &elf.input[ph.offset() as usize
                            ..(ph.offset() + ph.file_size()) as usize],
                    ),
                );
            }
        }
        // map user stack with U flags
        let max_end_va: VirtAddr = max_end_vpn.into();
        let mut user_stack_bottom: usize = max_end_va.into();
        // guard page
        user_stack_bottom += PAGE_SIZE;
        let user_stack_top = user_stack_bottom + USER_STACK_SIZE;
        memory_set.push(
            MapArea::new(
                user_stack_bottom.into(),
                user_stack_top.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
            ),
            None,
        );
        // used in sbrk
        memory_set.push(
            MapArea::new(
                user_stack_top.into(),
                user_stack_top.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
            ),
            None,
        );
        // map TrapContext
        memory_set.push(
            MapArea::new(
                TRAP_CONTEXT_BASE.into(),
                TRAMPOLINE.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        (
            memory_set,
            user_stack_top,
            elf.header.pt2.entry_point() as usize,
        )
    }

    /// [from_existed_user user_space] Copy an existing user address space into a new address space.
    ///
    /// All recorded mappings are reproduced, including framed data and trap/user-stack
    /// pages, and the contents of every framed page are copied. This produces a
    /// private copy suitable for `fork`; future writes do not share the copied frames.
    pub fn from_existed_user(user_space: &Self) -> Self {
        let mut memory_set = Self::new_bare();
        // map trampoline
        memory_set.map_trampoline();
        // copy data sections/trap_context/user_stack
        for area in user_space.areas.iter() {
            let new_area = MapArea::from_another(area);
            memory_set.push(new_area, None);
            // copy data from another space
            for vpn in area.vpn_range {
                let src_ppn = user_space.translate(vpn).unwrap().ppn();
                let dst_ppn = memory_set.translate(vpn).unwrap().ppn();
                dst_ppn
                    .get_bytes_array()
                    .copy_from_slice(src_ppn.get_bytes_array());
            }
        }
        memory_set
    }

    /// [activate self] Activate this address space on the current hart.
    ///
    /// Write the root page-table token to the `satp` CSR and execute an
    /// address-space fence to invalidate stale TLB entries.
    pub fn activate(&self) {
        let satp = self.page_table.token();
        unsafe {
            satp::write(satp);
            asm!("sfence.vma");
        }
    }

    /// [translate self vpn] Look up the page-table entry for virtual page number [vpn].
    ///
    /// Return the corresponding `PageTableEntry`, or `None` when the VPN is not
    /// present in this address space's page-table hierarchy.
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.page_table.translate(vpn)
    }

    /// [recycle_data_pages mutself] Remove every recorded `MapArea` from this address space.
    ///
    /// Dropping the areas releases their framed physical `FrameTracker`s. This
    /// method is normally used when the address space is being retired; the page
    /// table structure itself is released when the containing `MemorySet` is dropped.
    pub fn recycle_data_pages(&mut self) {
        self.areas.clear();
    }

    /// [shrink_to mutself start new_end] Shrink the `MapArea` beginning at virtual address [start].
    ///
    /// [new_end] is rounded up to a page boundary. Pages above it are unmapped
    /// and released. Return `true` if an area starts at [start.floor()], and
    /// `false` if no matching area exists.
    #[allow(unused)]
    pub fn shrink_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.floor())
        {
            area.shrink_to(&mut self.page_table, new_end.ceil());
            true
        } else {
            false
        }
    }

    /// [append_to mutself start new_end] Extend the `MapArea` beginning at virtual address [start].
    ///
    /// [new_end] is rounded up to a page boundary. New pages between the old end
    /// and [new_end] are mapped according to the area's permission. Return `true`
    /// only if an area starts at [start.floor()]; otherwise return `false`.
    #[allow(unused)]
    pub fn append_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.floor())
        {
            area.append_to(&mut self.page_table, new_end.ceil());
            true
        } else {
            false
        }
    }
}
/// A contiguous virtual-memory area controlled by a `MemorySet`.
///
/// A `MapArea` records its virtual page range, mapping type, permissions, and
/// owned physical frames (for framed mappings).
pub struct MapArea {
    vpn_range: VPNRange,
    data_frames: BTreeMap<VirtPageNum, FrameTracker>,
    map_type: MapType,
    map_perm: MapPermission,
}

impl MapArea {
    /// [new start_va end_va map_type map_perm] Create a contiguous virtual-memory area.
    ///
    /// The start is floor-rounded and the end is ceiling-rounded to virtual pages.
    pub fn new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        map_type: MapType,
        map_perm: MapPermission,
    ) -> Self {
        let start_vpn: VirtPageNum = start_va.floor();
        let end_vpn: VirtPageNum = end_va.ceil();
        Self {
            vpn_range: VPNRange::new(start_vpn, end_vpn),
            data_frames: BTreeMap::new(),
            map_type,
            map_perm,
        }
    }
    /// [from_another another] Create an empty copy of another area's metadata and permissions.
    ///
    /// Physical frame ownership is not copied; the returned area initially has
    /// no frames and is intended to be populated into a new address space.
    pub fn from_another(another: &Self) -> Self {
        Self {
            vpn_range: VPNRange::new(
                another.vpn_range.get_start(),
                another.vpn_range.get_end(),
            ),
            data_frames: BTreeMap::new(),
            map_type: another.map_type,
            map_perm: another.map_perm,
        }
    }
    /// [map_one mutself page_table vpn] Map one virtual page in this area.
    ///
    /// `Identical` areas use the VPN as the physical page number; `Framed` areas
    /// allocate and track a separate physical frame.
    pub fn map_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        let ppn: PhysPageNum;
        match self.map_type {
            MapType::Identical => {
                ppn = PhysPageNum(vpn.0);
            }
            MapType::Framed => {
                let frame = frame_alloc().unwrap();
                ppn = frame.ppn;
                self.data_frames.insert(vpn, frame);
            }
        }
        let pte_flags = PTEFlags::from_bits(self.map_perm.bits).unwrap();
        page_table.map(vpn, ppn, pte_flags);
    }
    /// [unmap_one mutself page_table vpn] Unmap one virtual page and release its framed physical frame, if any.
    pub fn unmap_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        if self.map_type == MapType::Framed {
            self.data_frames.remove(&vpn);
        }
        page_table.unmap(vpn);
    }
    /// [map mutself page_table] Establish mappings for every virtual page in this area.
    pub fn map(&mut self, page_table: &mut PageTable) {
        for vpn in self.vpn_range {
            self.map_one(page_table, vpn);
        }
    }
    /// [unmap mutself page_table] Remove mappings and owned physical frames for every page in this area.
    pub fn unmap(&mut self, page_table: &mut PageTable) {
        for vpn in self.vpn_range {
            self.unmap_one(page_table, vpn);
        }
    }
    /// [shrink_to mutself page_table new_end] Shrink this area to exclude pages at or after [new_end].
    /// [new_end] must be a virtual page number and pages above it are released.
    #[allow(unused)]
    pub fn shrink_to(
        &mut self,
        page_table: &mut PageTable,
        new_end: VirtPageNum,
    ) {
        for vpn in VPNRange::new(new_end, self.vpn_range.get_end()) {
            self.unmap_one(page_table, vpn)
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    /// [append_to mutself page_table new_end] Extend this area through virtual page [new_end] (exclusive).
    ///
    /// Every newly introduced VPN is mapped using this area's mapping type and permissions.
    #[allow(unused)]
    pub fn append_to(
        &mut self,
        page_table: &mut PageTable,
        new_end: VirtPageNum,
    ) {
        for vpn in VPNRange::new(self.vpn_range.get_end(), new_end) {
            self.map_one(page_table, vpn)
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    /// [copy_data mutself page_table data] Copy [data] from the start of this framed area's virtual range.
    ///
    /// This is valid only for `MapType::Framed`. [data] may be shorter than the
    /// area and is copied across page boundaries; newly allocated pages are
    /// assumed to be zero-initialized.
    pub fn copy_data(&mut self, page_table: &mut PageTable, data: &[u8]) {
        assert_eq!(self.map_type, MapType::Framed);
        let mut start: usize = 0;
        let mut current_vpn = self.vpn_range.get_start();
        let len = data.len();
        loop {
            let src = &data[start..len.min(start + PAGE_SIZE)];
            let dst = &mut page_table
                .translate(current_vpn)
                .unwrap()
                .ppn()
                .get_bytes_array()[..src.len()];
            dst.copy_from_slice(src);
            start += PAGE_SIZE;
            if start >= len {
                break;
            }
            current_vpn.step();
        }
    }
}

/// Mapping strategy used by a `MapArea`.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum MapType {
    /// VPN is mapped directly to the same physical page number.
    Identical,
    /// Each VPN is mapped to a separately allocated physical frame owned by the area.
    Framed,
}

bitflags! {
    /// Permissions and user-accessibility flags for a mapped area.
    pub struct MapPermission: u8 {
        /// Allow reads from the mapped pages.
        const R = 1 << 1;
        /// Allow writes to the mapped pages.
        const W = 1 << 2;
        /// Allow instruction fetches from the mapped pages.
        const X = 1 << 3;
        /// Mark the mapped pages accessible in U mode.
        const U = 1 << 4;
    }
}

/// [remap_test ()] Verify selected kernel-section permissions in the remapped kernel address space.
///
/// This diagnostic asserts that text is not writable, read-only data is not writable,
/// and data is not executable. It is available for kernel-space debugging.
#[allow(unused)]
pub fn remap_test() {
    let mut kernel_space = KERNEL_SPACE.exclusive_access();
    let mid_text: VirtAddr = ((stext as usize + etext as usize) / 2).into();
    let mid_rodata: VirtAddr =
        ((srodata as usize + erodata as usize) / 2).into();
    let mid_data: VirtAddr = ((sdata as usize + edata as usize) / 2).into();
    assert!(!kernel_space
        .page_table
        .translate(mid_text.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_rodata.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_data.floor())
        .unwrap()
        .executable(),);
    println!("remap_test passed!");
}
