//! Process management syscalls
use crate::{
    task::{exit_current_and_run_next, suspend_current_and_run_next, syscall_count},
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("[kernel] Application exited with code {}", exit_code);
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
    unsafe {
        *ts = TimeVal {
            sec: us / 1_000_000,
            usec: us % 1_000_000,
        };
    }
    0
}

// TODO: implement the syscall
/// [sys_trace(0, _id, _)] return a byte of unsigned integar at [_id]
/// [sys_trace(1, _id, _data)] write [_data] as u8 to address [_id] and return [0] on success
/// [sys_trace(2, _id, _)] return count of calls to syscall specified by [_id]
/// [sys_trace(_, _, _)] failed with return value [-1]
pub fn sys_trace(_trace_request: usize, _id: usize, _data: usize) -> isize {
    trace!("kernel: sys_trace");
    match _trace_request {
        0 => {
            let addr = _id as *const u8;
            let val = unsafe { addr.read() };
            val as isize
        }
        1 => {
            let addr = _id as *mut u8;
            unsafe {
                addr.write(_data as u8);
            }
            0
        }
        2 => isize::try_from(syscall_count(_id)).expect("syscall count exceeds isize::MAX"),
        _ => -1,
    }
}
