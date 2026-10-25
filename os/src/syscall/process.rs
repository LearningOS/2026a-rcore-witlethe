//! Process management syscalls
use crate::{
    config::PAGE_SIZE,
    mm::{
        translated_byte_buffer_for_write, translated_byte_read, translated_byte_write,
        MapPermission, VirtAddr,
    },
    task::{
        change_program_brk, current_user_token, exit_current_and_run_next,
        map_current_user_memory, suspend_current_and_run_next, syscall_count,
        unmap_current_user_memory,
    },
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// get time with second and microsecond
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");

    let us = get_time_us();
    let value = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };

    let len = core::mem::size_of::<TimeVal>();

    let Some(buffers) = translated_byte_buffer_for_write(
        current_user_token(),
        ts as *const u8,
        len,
    ) else {
        return -1;
    };

    let src = &value as *const TimeVal as *const u8;
    let mut offset = 0;

    for dst in buffers {
        let n = dst.len();

        unsafe {
            core::ptr::copy_nonoverlapping(
                src.add(offset),
                dst.as_mut_ptr(),
                n,
            );
        }

        offset += n;
    }

    0
}

/// [sys_trace(0, _id, _)] return a byte of unsigned integar at [_id]
/// [sys_trace(1, _id, _data)] write [_data] as u8 to address [_id] and return [0] on success
/// [sys_trace(2, _id, _)] return count of calls to syscall specified by [_id]
/// [sys_trace(_, _, _)] failed with return value [-1]
pub fn sys_trace(_trace_request: usize, _id: usize, _data: usize) -> isize {
    trace!("kernel: sys_trace");
    match _trace_request {
        0 => {
            let addr = _id as *const u8;
            if let Some(byte) = translated_byte_read(current_user_token(), addr) {
                byte as isize
            } else {
                -1
            }
        }
        1 => {
            let addr = _id as *mut u8;
            if translated_byte_write(current_user_token(), addr, _data as u8) { 0 } else { -1 }
        }
        2 => isize::try_from(syscall_count(_id)).expect("syscall count exceeds isize::MAX"),
        _ => -1,
    }
}

/// [sys_mmap start len prot] allocate [len] bytes at user address [start] with protection [prot].
/// Return [0] on success, or [-1] if the arguments are invalid or the range is already mapped.
pub fn sys_mmap(_start: usize, _len: usize, _prot: usize) -> isize {
    trace!("kernel: sys_mmap");
    if _start % PAGE_SIZE != 0
        || _len == 0
        || _prot & !0x7 != 0
        || _prot & 0x7 == 0
        || _start
            .checked_add(_len)
            .map_or(true, |end| end > (1usize << 38))
    {
        return -1;
    }

    let permission = MapPermission::from_bits_truncate((_prot << 1) as u8)
        | MapPermission::U;
    if map_current_user_memory(VirtAddr::from(_start).floor(), _len, permission) {
        0
    } else {
        -1
    }
}

/// [sys_munmap start len] unmap the user range beginning at [start] with length [len].
/// Return [0] on success, or [-1] if arguments are invalid or the mapping does not match.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!("kernel: sys_munmap");
    if _start % PAGE_SIZE != 0
        || _len == 0
        || _len % PAGE_SIZE != 0
        || _start
            .checked_add(_len)
            .map_or(true, |end| end > (1usize << 38))
    {
        return -1;
    }

    if unmap_current_user_memory(VirtAddr::from(_start).floor(), _len) {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
