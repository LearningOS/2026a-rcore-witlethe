//!Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::*;

/// Moderate stride constant used to avoid excessive integer wraparound and division error.
pub const BIG_STRIDE: usize = 1 << 20;

/// A task-manager queue containing runnable `TaskControlBlock`s.
pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
}

/// Compare stride values with unsigned wraparound semantics.
///
/// A distance no greater than half the address-space value range is considered
/// to be in the forward direction, while the other half represents wraparound.
fn stride_less(a: usize, b: usize) -> bool {
    let delta = a.wrapping_sub(b);
    delta != 0 && delta > (usize::MAX >> 1)
}

/// A stride scheduler over the runnable task queue.
impl TaskManager {
    /// [new ()] Create an empty task-manager queue.
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
        }
    }

    /// [add task] Append [task] to the end of the runnable queue.
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push_back(task);
    }

    /// [fetch ()] Take the runnable process with the smallest stride out of the ready queue.
    ///
    /// After selecting a process, advance its stride by
    /// `BIG_STRIDE / priority`. Wrapping addition makes stride values robust to
    /// long-running processes.
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        if self.ready_queue.is_empty() {
            return None;
        }

        let mut best_index = 0;
        for index in 1..self.ready_queue.len() {
            let best_stride =
                self.ready_queue[best_index].inner_exclusive_access().stride;
            let candidate_stride =
                self.ready_queue[index].inner_exclusive_access().stride;
            if stride_less(candidate_stride, best_stride) {
                best_index = index;
            }
        }

        let task = self.ready_queue.remove(best_index).unwrap();
        let mut inner = task.inner_exclusive_access();
        let pass = BIG_STRIDE / inner.priority;
        inner.stride = inner.stride.wrapping_add(pass.max(1));
        drop(inner);
        Some(task)
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// [add_task task] Add [task] to the global runnable queue.
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// [fetch_task ()] Select and remove the smallest-stride task from the global queue.
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}
