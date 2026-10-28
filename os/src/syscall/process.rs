//! Process management syscalls
use alloc::sync::Arc;

use crate::{
    config::PAGE_SIZE,
    loader::get_app_data_by_name,
    mm::{
        translated_byte_buffer_for_write, translated_refmut, translated_str,
        MapPermission, VirtAddr,
    },
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next, TaskControlBlock,
    },
    timer::get_time_us,
};

/// User-visible elapsed-time structure returned by `get_time`.
#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel:pid[{}] sys_yield", current_task().unwrap().pid.0);
    suspend_current_and_run_next();
    0
}

/// [sys_getpid ()] Return the current process identifier.
pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

/// [sys_fork ()] Fork a child process and return its PID to the parent.
///
/// Return zero to the child through the modified child TrapContext. This
/// implementation returns the child PID to the parent and `0` to the child.
pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);

    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);

    new_pid as isize
}

/// [sys_exec path] Replace the current process address space with the ELF named by [path].
/// Return `0` on success and `-1` when the name is unknown.
pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(data) = get_app_data_by_name(path.as_str()) {
        let task = current_task().unwrap();
        task.exec(data);
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    trace!(
        "kernel::pid[{}] sys_waitpid [{}]",
        current_task().unwrap().pid.0,
        pid
    );
    let task = current_task().unwrap();

    // Find a child process while holding the current PCB lock exclusively.
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
    }

    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie()
            && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// [sys_get_time ts tz] Write the current time as seconds and microseconds to user address [ts].
///
/// [tz] is reserved and ignored. The destination may span multiple pages, so
/// use checked address translation and copy each contiguous writable slice.
/// Return `0` on success, or `-1` if the destination is invalid.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel:pid[{}] sys_get_time", current_task().unwrap().pid.0);

    let us = get_time_us();
    let value = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };

    let len = core::mem::size_of::<TimeVal>();
    let Some(buffers) = translated_byte_buffer_for_write(
        current_user_token(),
        ts.cast::<u8>(),
        len,
    ) else {
        return -1;
    };

    let src = (&value as *const TimeVal).cast::<u8>();
    let mut offset = 0;
    for dst in buffers {
        unsafe {
            core::ptr::copy_nonoverlapping(
                src.add(offset),
                dst.as_mut_ptr(),
                dst.len(),
            );
        }
        offset += dst.len();
    }

    0
}

/// [sys_mmap start len prot] Map [len] bytes at user virtual address [start] with protection [prot].
///
/// [start] must be page-aligned and [len] nonzero. [prot] may contain only the
/// read, write, and execute bits (`1`, `2`, `4`) and must contain at least one.
/// The mapped range must end within the supported SV39 user address space.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel:pid[{}] sys_mmap", current_task().unwrap().pid.0);

    if start % PAGE_SIZE != 0
        || len == 0
        || prot & !0x7 != 0
        || prot & 0x7 == 0
        || start
            .checked_add(len)
            .map_or(true, |end| end > (1usize << 38))
    {
        return -1;
    }

    let permission =
        MapPermission::from_bits_truncate((prot << 1) as u8) | MapPermission::U;
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if inner.memory_set.map_user_range(
        VirtAddr::from(start).floor(),
        len,
        permission,
    ) {
        0
    } else {
        -1
    }
}

/// [sys_munmap start len] Unmap a user range beginning at [start] with page-aligned length [len].
///
/// The range must be an exact existing framed `MapArea`; partial unmapping and
/// unmapping a range belonging to other areas is rejected.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_munmap", current_task().unwrap().pid.0);

    if start % PAGE_SIZE != 0
        || len == 0
        || len % PAGE_SIZE != 0
        || start
            .checked_add(len)
            .map_or(true, |end| end > (1usize << 38))
    {
        return -1;
    }

    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if inner
        .memory_set
        .unmap_user_range(VirtAddr::from(start).floor(), len)
    {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// [sys_spawn path] Create a child process that executes the ELF program named by [path].
///
/// The child is created from a fresh address space, rather than by copying the
/// parent's address space. Return the child's PID on success, or `-1` if the
/// application name is invalid.
pub fn sys_spawn(path: *const u8) -> isize {
    let current = current_task().unwrap();
    trace!("kernel:pid[{}] sys_spawn", current.pid.0);

    let name = translated_str(current_user_token(), path);
    let Some(elf_data) = get_app_data_by_name(name.as_str()) else {
        return -1;
    };

    let child = Arc::new(TaskControlBlock::new(elf_data));
    let child_pid = child.pid.0;

    let mut parent_inner = current.inner_exclusive_access();
    child.inner_exclusive_access().parent = Some(Arc::downgrade(&current));
    parent_inner.children.push(child.clone());
    drop(parent_inner);

    add_task(child);
    child_pid as isize
}

/// [sys_set_priority prio] Set the priority of the current process.
///
/// Valid priorities satisfy [prio] >= 2. Return [prio] on success and `-1`
/// when the value is outside that range.
pub fn sys_set_priority(prio: isize) -> isize {
    let current = current_task().unwrap();
    trace!(
        "kernel:pid[{}] sys_set_priority prio={}",
        current.pid.0,
        prio
    );

    if prio < 2 {
        return -1;
    }

    let mut inner = current.inner_exclusive_access();
    inner.priority = prio as usize;
    prio
}
