# 共享可变性、Arc 与锁：从时间轮重构中得到的判断顺序

面向场景：你在设计一个数据结构时，发现「好像得加个锁」，但不确定这个锁到底是需求还是自己造出来的。

> **Code Version**: pb-mapper 工作区，`feat/temporary-credential-auth`（2026-08-21）
>
> 相关文档：[Send/Sync/Pin 与 async 状态机深度解析](./rust-async-send-sync-pin-deep-dive.md)
> 覆盖 `async fn` 如何编译成状态机、`Pin` 为什么必要。本文讲的是它的另一面：
> **数据结构层面**该不该共享、该不该加锁。

## 1. 问题从哪里来

重构 `src/common/auth/timing_wheel.rs` 时，中间某一版长成这样：

```rust
struct Queues {
    levels: Vec<std::sync::Mutex<VecDeque<Vec<Link>>>>,  // 每一级一把锁
}
```

时间轮的每一级都上了一把锁。而这个时间轮是被 auth actor **独占**的——`Leases`
从头到尾以 `&mut Leases` 传递，从未进过 `Arc`。既然没有任何并发，这些锁是从哪来的？

答案是我自己造出来的：我让 `Link::drop` 自己把下一跳投递进桶里。`Drop::drop`
只有 `&mut self`（指向 Link 自己），拿不到轮子的 `&mut`，所以只能让 Link 持有一个
`Weak<Queues>` 共享轮子——**一旦共享，就必须加锁**。

去掉共享，锁就自己消失了：让 `Drop` 只保留数据，由 `tick` 拿着 `&mut self` 投递。

这件事暴露出一个常见的思维捷径：

> ❌ 「共享可变 → 加锁」

它跳过了两个更该先问的问题。本文把正确的判断顺序拆出来。

## 2. 三个正交的机制

先把三件经常被混为一谈的事分开。它们各管一件事，互不替代。

| 机制 | 管什么 | 典型工具 |
|---|---|---|
| **所有权 / 借用** | 谁能改、谁能读 | `&mut T` / `&T` |
| **`Arc`** | 生命周期：owner 何时死 | `Arc<T>` / `Rc<T>` |
| **`Send` / `Sync`** | 跨线程访问的安全性 | marker trait，编译器自动推导 |
| **内部可变性** | 通过 `&T` 修改 | `Cell` / `RefCell` / `Atomic` / `Mutex` |

```
        「我能改它吗」            「它还活着吗」        「换线程安全吗」
             │                        │                     │
        借用规则                    Arc/Rc              Send/Sync
             │                        │                     │
             └──── 三者独立，缺一不可，且不能互相代替 ────────┘
```

一个具体的反直觉例子：`Vec<u8>` 本身就是 `Send + Sync`，但这**不代表**你可以
不用 `Arc` 就把它共享给 `tokio::spawn` 的任务。`Send + Sync` 解决的是安全性，
`'static` 生命周期要求得靠 `Arc` 解决。反过来，`Arc<Cell<i32>>` 生命周期没问题，
但因为 `Cell` 不是 `Sync`，一样过不了 `spawn`。

## 3. Send 与 Sync 到底是什么

两个 marker trait，不含任何方法，编译器**自动推导**（结构体所有字段都满足则它满足）：

- **`Send`**：这个值可以**移动**到另一个线程。
- **`Sync`**：这个值可以被**多个线程同时引用**。

第二条有个更精确、也更好用的等价定义：

```
T: Sync   ⟺   &T: Send
```

即「把它的引用发给别的线程是否安全」。这比「线程安全」这种模糊说法准确得多。

### 3.1 为什么必须区分这两件事

看 `Cell`：

```rust
let c = Cell::new(0);
thread::spawn(move || c.set(1));   // 独占移过去 —— 安全，故 Cell: Send
// 但：
thread::scope(|s| {
    s.spawn(|| c.set(1));          // 两个线程同时 set
    s.spawn(|| c.set(2));          // 数据竞争，故 Cell: !Sync
});
```

`Cell::set` 只是一条普通写入，没有任何同步。**移动**给一个线程完全安全（原线程
再也碰不到它）；**同时借给两个线程**就是 UB。这恰好是 `Send` 与 `Sync` 的分界。

### 3.2 Sync 只赋予「共享读」，不赋予「共享写」

这是最容易混淆的一点。大多数类型（`u64`、`String`、`Vec<T>`、`HashMap`）都是
`Send + Sync`——但你拿到 `&Vec<u8>` 依然改不了它：

```rust
let shared = Arc::new(vec![1_u8, 2, 3]);
shared.push(4);
// error[E0596]: cannot borrow data in an `Arc` as mutable
```

`Vec<u8>` 是 `Sync` 的**原因**正是「通过 `&T` 改不了它」。它 `Sync` 是因为它老实,
不是因为它做了同步。于是 `Sync` 的类型分成两类：

| 类别 | 例子 | 为什么 Sync |
|---|---|---|
| **老实类型** | `u64`, `Vec<T>`, `HashMap` | 通过 `&T` 根本改不了，无从竞争 |
| **内部可变 + 自带同步** | `Mutex<T>`, `AtomicU64`, `RwLock<T>` | 通过 `&T` 能改，但访问被串行化 |

`Cell<T>` 是第三类：通过 `&T` 能改，**且不带同步**——所以它被排除在 `Sync` 之外。
三类划清，`Sync` 的定义就自洽了。

> 💡 **Key Point**：需要锁的条件不是「T 不是 Sync」，而是**「我要通过 `&T` 修改它」**。

### 3.3 Mutex 的类型学意义

```
T: Send   ──[ 包一层 Mutex ]──>   Mutex<T>: Sync
```

**锁的作用就是把「可移动」升级成「可共享」。** 所以 `Mutex<Cell<i32>>` 是 `Sync`
的——一个类型不是 `Sync` 从来不是死路。

## 4. 为什么 Rc 既不 Send 也不 Sync

根因只有一个：**`Rc` 的引用计数是普通 `usize` 加减，非原子**。那正是它比 `Arc` 快的
全部原因。但这一个根因导致两个独立后果：

**`!Sync`**（直觉的那一半）：`Rc::clone` 只要 `&self`，而它会 `count += 1`。两个线程
各持 `&Rc` 同时 clone，两次非原子递增丢一次计数 → 提前释放 → use-after-free。

**`!Send`**（更微妙，也更关键）：你可能想「整个移过去不就独占了吗」。但
**`Rc` 从来不是唯一的那一份**：

```rust
let here = Rc::new(0);
let there = here.clone();             // 两个 handle，一个共享的非原子计数
thread::spawn(move || drop(there));   // 那边 count -= 1
drop(here);                           // 这边 count -= 1，同时进行
```

`there` 移走了，`here` 还在原线程。两边同时递减同一个非原子计数 → 泄漏或双重释放。

> 💡 **Key Point**：`Rc: Send` 不安全，不是因为被移动的那一份，而是因为
> **留在原地的那些**。类型系统无法表达「仅当这是最后一份 handle 时才允许移动」,
> 所以只能整个禁掉。

对比 `Cell` 就完整了：

| | 计数 | Send | Sync |
|---|---|---|---|
| `Rc<T>` | 非原子 | ✗ 别的 handle 会同时改计数 | ✗ `&Rc` 就能 clone |
| `Arc<T>` | 原子 | ✓（需 `T: Send + Sync`）| ✓（同）|
| `Cell<T>` | — | ✓ 移走后原线程什么都不剩 | ✗ `&Cell` 就能 set |

`Cell: Send` 而 `Rc: !Send`，差别正在于「移走后原线程手里还有没有东西」。

## 5. Arc 管的是生命周期，不是安全性

既然 `Vec<u8>` 本身就 `Send + Sync`，为什么还需要 `Arc`？直接 `&Vec` 不行吗？

**在能证明作用域的场景里，确实不需要**：

```rust
let data = vec![1_u8, 2, 3];
thread::scope(|s| {
    s.spawn(|| println!("{:?}", &data));   // 零 Arc
    s.spawn(|| println!("{:?}", &data));
});
println!("still owned: {:?}", data);       // 依然是 owner
```

`thread::scope` 保证所有子线程在它返回前 join，所以编译器**能证明** `data` 活得更久。

`tokio::spawn` 和 `thread::spawn` 则不然——任务是 detached 的，何时结束由运行时决定。
编译器无法证明任何栈上的东西活得比它久，于是有了 `'static` 约束。满足它只有两条路：

1. 把所有权 `move` 进去 → 只有一个任务能拿到，没法共享；
2. 用 `Arc` → 所有权归引用计数集体所有，**没有任何栈帧是它的 owner**，
   于是每个持有者天然满足 `'static`。

```
                能证明作用域              不能（detached / 'static）
   只读        &T（零成本）                  Arc<T>
   要改        &mut T（零成本）              Arc<Mutex<T>>
```

> 💡 **Key Point**：`Arc` 把生命周期问题转成运行时引用计数，代价是一次原子加减。
> 这跟 `T` 是否 `Sync` 无关——`Sync` 只决定 `Arc<T>` 能不能 `Send`。

## 6. tokio::spawn 的签名从哪来

```rust
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where F: Future + Send + 'static, F::Output: Send + 'static
```

三个约束各有其因：

- **`Send`**：tokio 多线程调度器有 work-stealing——空闲 worker 会从别的 worker
  队列里偷任务。你的 future 可能在线程 A 上 poll 一次、挂起、然后在线程 B 上 poll
  下一次。它是被**移动**过去的，故需 `Send`。
- **`'static`**：future 存活时间由运行时决定，不受调用处作用域约束，不能借用栈上的东西。
- **`F::Output: Send`**：结果要从 worker 线程送回 `JoinHandle` 的等待方。

对 `async fn`，编译器把它编译成状态机，**所有跨 `.await` 存活的局部变量都成为该状态机
的字段**。于是「future 是否 `Send`」= 「这些字段是否全部 `Send`」。这就是为什么一个
持有 `Rc` 的 async 函数无法 `spawn`——哪怕只在两个 `.await` 之间用了一下。

（状态机的展开细节见
[Send/Sync/Pin 与 async 状态机深度解析 §3](./rust-async-send-sync-pin-deep-dive.md)。）

逃逸口：`spawn_local` 没有 `Send` 约束，代价是任务被钉在单线程 `LocalSet` 上，
拿不到 work-stealing。

## 7. 判断顺序

把上面几节合起来，得到一个可执行的检查表。**顺序很重要**——跳过前两问就会得到
第 1 节那种每级一把锁的东西。

```
① 这里为什么是共享的？能不能改成独占？
      │
      ├─ 能 ──> 用 &mut T，到此结束（零成本，无锁）
      │
      ↓ 不能
② 共享是否真的要跨线程？
      │
      ├─ 不必 ──> Cell / RefCell（零成本 / 一个计数器）
      │
      ↓ 必须（Send/Sync 约束逼上来了）
③ 要改的是什么粒度？
      │
      ├─ 单个整数/指针 ──> Atomic（无锁）
      │
      └─ 一段临界区 ────> Mutex / RwLock
```

第 ① 步最容易被跳过，而它恰恰是收益最大的一步：**共享是可以被设计掉的**。

## 8. 落到 pb-mapper 的真实代码

### 8.1 被设计掉的锁：时间轮的 Queues

第 1 节那版每级一把 `Mutex`，走的是「`Drop` 里投递 → 需要共享 `Queues` → 加锁」。
现在 `Link::Relay` 只是纯数据，`tick` 拿 `&mut self` 自己投递
（`src/common/auth/timing_wheel.rs`）：

```rust
Link::Relay { level, slot, next } => self.file(level as usize, slot as usize, *next),
```

停在第 ① 步。`Queues` 类型、`Weak<Queues>`、以及那一排锁全部消失。

### 8.2 无锁的共享可变：AuthLease.expires_at

```rust
pub struct AuthLease {
    expires_at: AtomicU64,   // src/common/auth.rs
    ...
}
```

lease 通过 `Arc` 共享（请求侧持 `Weak`，时间轮持强引用），续期要改 `expires_at`,
所以是货真价实的「跨线程共享可变」。但它只读写一个 `u64`，停在第 ③ 步的 `Atomic` 分支,
不需要锁。

### 8.3 无法避免的锁：Timer.callback

```rust
pub(super) struct Timer {
    callback: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
```

这把锁走完了全部三步，每一步都无路可退：

```
① 能独占吗？ 不能 —— 续期时 retire/reap 两条路径共享同一个 Timer，
             且「最后一个引用被丢弃时触发」这个语义本身就是引用计数
② 能只单线程吗？ 不能 —— 推导链如下
③ 能用 Atomic 吗？ 不能 —— FnOnce 只能按值调用，必须把整个 Box 移出来
```

第 ② 步的推导链值得完整写出来，它是本文所有概念的汇合点：

```
tokio::spawn(run_auth_actor(...))        要求 future: Send
  → actor future 跨 .await 持有 Leases    ⇒ Leases: Send
  → Leases 持有 Arc<Timer>                ⇒ Arc<Timer>: Send
  → Arc<T>: Send 需要 T: Send + Sync      ⇒ Timer: Sync
  → Timer: Sync 需要 callback 字段: Sync
  → Cell 不是 Sync，Mutex<T> 是（当 T: Send）
```

（`Arc<T>: Send` 为何要 `T: Sync`：clone 出去后另一个线程通过它拿到 `&T`，
那正是 `Sync` 管的事；同时也要 `T: Send`，因为那个线程可能持有最后一份引用
并在自己那里析构 `T`。）

不过实际开销接近零：**绝大多数 timer 从不加锁触发**。`Drop for Timer` 有
`&mut self`，走 `Mutex::get_mut()`：

```rust
impl Drop for Timer {
    fn drop(&mut self) {
        let callback = self.callback.get_mut().take();   // 无锁
        run(callback);
    }
}
```

只有显式提前 `fire()`（revoke、GC）才真正 lock，而那时也无人竞争。

### 8.4 停在第一步：Leases.stages

```rust
pub(super) struct Leases {
    stages: HashMap<KeyId, Stages>,   // 没有 Arc，没有锁
}
```

`HashMap` 是 `Send + Sync`，但这在这里毫不相关——actor 独占 `Leases`，所有方法都是
`&mut self`。**`HashMap` 是不是 `Sync` 根本不影响这个决定。**

### 8.5 必要的锁：AuthStateInner

```rust
struct AuthStateInner {
    slots: RwLock<Box<[SlotHot]>>,
    cold: RwLock<HashMap<KeyId, ColdMetadata>>,
    ...
}
```

它被 `Arc` 共享给请求处理路径和 actor 两边，双方都要改，且要保护的是「查槽位 →
校验 generation → 改状态」这样的临界区而非单个整数。三步走完，`RwLock` 是对的。

## 9. 为什么用 parking_lot

标准库的 `Mutex`/`RwLock` 带 **poisoning**：持锁线程 panic 后，锁被标记为「有毒」,
后续 `lock()` 返回 `Err`。本项目从不利用这个信号——迁移前每个调用点都是同一句样板：

```rust
lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
```

即「无论如何都取出内部值」，等于把 poisoning 显式关掉。`src/common/auth.rs` 里还为此
养了一个 `recover_lock` 辅助函数专门抹掉 `LockResult`。

`parking_lot` 不做 poisoning，于是：

- `lock()` 直接返回 guard，所有样板和 `recover_lock` 一起消失；
- FFI 侧 `claim_key` 里那条「state is poisoned」错误分支变成不可达，直接删掉——
  少一个永远不会发生的错误码；
- 未竞争时是纯自旋 + 无系统调用，比 std 的 futex 路径更快；
- 锁本身不必为 poisoning 保留状态，`Mutex<T>` 只占一个字节加 `T`。

代价：panic 后锁会正常释放，其他线程可能看到中间状态的值。本项目原先就用
`into_inner()` 接受了这个行为，所以迁移是纯简化，语义不变。

## 10. 小结

- **`Arc` 管生命周期，`Send`/`Sync` 管跨线程安全性，内部可变性管「通过 `&T` 修改」。**
  三件事正交，不能互相代替。
- `Sync` 只赋予共享**读**。需要锁的条件是「我要通过 `&T` 改它」，而非「T 不是 Sync」。
- `Mutex` 的类型学意义：把 `Send` 升级成 `Sync`。
- 判断顺序：**能不能不共享 → 是否真要跨线程 → 粒度是整数还是临界区**。
  第一步收益最大，也最常被跳过。
- 一把锁如果无法说清它走完了这三步，它大概是被设计出来的，而不是需求。
