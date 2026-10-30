# 功能实现

本实验实现了 `sys_get_time`, `sys_mmap`, `sys_unmap` 系统调用的迁移；实现了 `sys_spawn` 系统调用；同时实现了 stride 调度算法
其中 `sys_mmap`, `sys_unmap` 继续使用我们之前提供的用于映射任务地址空间的接口，但是转变为通过进程控制块的 inner 结构获取地址空间，而不是使用 current

同时，本实验还实现了 stride 调度算法，向 PCB 添加 mutable 字段 `stride` 和 `priority` 以记录调度算法所需的额外状态信息
修改 `TaskManager` 中的 `fetch` 接口来变更选择最优进程的逻辑

补全一些接口文档，修改一些逻辑段格式


# 问答作业

stride 算法原理非常简单，但是有一个比较大的问题
例如两个 pass = 10 的进程，使用 8bit 无符号整形储存 stride， p1.stride = 255, p2.stride = 250，在 p2 执行一个时间片后，理论上下一次应该 p1 执行
1. 实际情况是轮到 `p1` 执行吗？为什么？

不一定会轮到 `p1`：这是 8 位无符号数溢出与普通数值比较共同导致的
初始为 `p1.stride = 255`、`p2.stride = 250` 时，`p2` 先执行一个时间片并加 10 后，p2 的值变成 `(250 + 10) mod 256 = 4`
若直接用 8 位无符号数比较，则 p2 的 4 小于 p1 的 255，于是调度器会继续选择 p2，逻辑上反而认为 p1 的 255 是更大的 stride；因此不能直接进行普通小于比较。为避免溢出误判，需要采用带环绕语义的比较器，即把两个值之间的差距限制在半个数值空间内来判断先后

我们之前要求进程优先级 >= 2 其实就是为了解决这个问题
可以证明， 在不考虑溢出的情况下 , 在进程优先级全部 >= 2 的情况下，如果严格按照算法执行，那么 STRIDE_MAX – STRIDE_MIN <= BigStride / 2
2. 为什么进程优先级全部 >= 2 时，`STRIDE_MAX - STRIDE_MIN <= BigStride / 2`？

stride 调度中执行一次的增量为 `pass = BigStride / priority`
当所有优先级至少为 2 时，每个进程每次增加的 `pass` 不超过 `BigStride / 2`
同时，在较短时间区间内真正并发参与比较的两个进程差值不会无限积累：每次调度都会选择当前最小值并只使该进程的 stride 增加一个 `pass`
因此任意时刻，最大最小 stride 的差不会超过一个最大允许增量，即不超过 `BigStride / 2`；这保证所有同时存在的 stride 可以被视为落在同一个半数值区间内，环绕比较仍能正确区分大小

3. 补全带溢出语义的 `partial_cmp`

```rust
use core::cmp::Ordering;

struct Stride(u64);

impl PartialOrd for Stride {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let delta = self.0.wrapping_sub(other.0);

        if delta == 0 {
            Some(Ordering::Equal)
        } else if delta > (u64::MAX >> 1) {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Greater)
        }
    }
}

impl PartialEq for Stride {
    fn eq(&self, other: &Self) -> bool {
        false
    }
}
```

# 荣誉准则

1. 在完成本次实验的过程（含此前学习的过程）中，我曾与他人就与本实验相关的内容做过交流（如有）：

    - （无）

2. 此外，我参考了以下资料：

    - rCore-Tutorial 操作系统教学文档与课程源码
    - Rust 官方文档及相关标准库实现

3. 我独立完成了本次实验除以上交流和参考资料之外的全部工作，包括代码与文档。我清楚地知道，以上交流或参考可能会在一定程度上降低实验难度，并可能影响最终评分。

4. 我从未使用、复制或改写过他人的实验代码，也不会向他人复制或公开本实验的代码和报告。我提交到本实验评测系统的代码没有破坏或妨碍任何计算机系统正常运行的意图。我理解并遵守课程纪律；如违反规定，本实验成绩将按课程规则处理。