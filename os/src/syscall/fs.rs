//! File and filesystem-related syscalls
use crate::mm::translated_byte_buffer;
use crate::sbi::console_getchar;
use crate::task::{
    current_task, current_user_token, suspend_current_and_run_next,
};

const FD_STDIN: usize = 0;
const FD_STDOUT: usize = 1;

/// write buf of length `len`  to a file with `fd`
/// [sys_write fd buf len] Write up to [len] bytes from user memory to [fd].
///
/// Only file descriptor `1` is supported. Invalid descriptors or inaccessible
/// user buffers return `-1`; successful output returns the copied byte count.
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_write", current_task().unwrap().pid.0);

    match fd {
        FD_STDOUT => {
            let buffers =
                translated_byte_buffer(current_user_token(), buf, len);

            for buffer in buffers {
                print!("{}", core::str::from_utf8(buffer).unwrap());
            }

            len as isize
        }
        _ => {
            panic!("Unsupported fd in sys_write!");
        }
    }
}

/// [sys_read fd buf len] Read one byte from the standard input into user memory.
///
/// Reading is currently supported only for file descriptor `0`, with [len]
/// equal to one. The calling task is suspended while input is unavailable.
pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_read", current_task().unwrap().pid.0);

    match fd {
        FD_STDIN => {
            assert_eq!(len, 1, "Only support len = 1 in sys_read!");

            let mut c: usize;
            loop {
                c = console_getchar();
                if c == 0 {
                    suspend_current_and_run_next();
                    continue;
                } else {
                    break;
                }
            }
            let ch = c as u8;

            let mut buffers =
                translated_byte_buffer(current_user_token(), buf, len);

            unsafe {
                buffers[0].as_mut_ptr().write_volatile(ch);
            }

            1
        }
        _ => {
            panic!("Unsupported fd in sys_read!");
        }
    }
}
