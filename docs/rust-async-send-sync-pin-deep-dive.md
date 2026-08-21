# Rust 并发安全（Send/Sync/Pin）与 async 状态机深度解析

> 历史设计文档：反映 2025 年某时期的实现，其中的行号与代码引用可能已漂移
> （拆分为多 crate 后所有路径都已移到 `crates/` 下），仅供设计意图参考。
> 当前实现请以代码为准。

面向场景：你在实现网络转发（如 pb-mapper）时需要理解 **为什么某些 future 必须 `Send + 'static`**、为什么 `async fn` 能跨 `.await` 持有借用、以及 `Pin` 如何保证自引用安全。

> **Code Version**: pb-mapper 本地工作区（2026-01-17）

## 1. 引言：问题从哪里来
异步网络系统通常会做两类事情：
1) 在当前任务中直接 `.await` 一段 I/O 逻辑；
2) 把一段 I/O 逻辑丢进 `tokio::spawn` 交给运行时调度。

这两类路径的 **安全边界不同**：
- 直接 `.await`：只需保证**生命周期有效**；
- `tokio::spawn`：必须满足 **`Send + 'static`**。

这也是为什么 pb-mapper 在 `StreamForward` 上必须保证 Send（见 `forward_local_to_remote`），否则 `tokio::spawn` 会拒绝编译。`tokio::spawn` 的调用点在本项目的 client/server 侧都能看到。`src/local/client/mod.rs:164`、`src/local/server/mod.rs:309`。

> 💡 **Key Point**：
> “能不能 await” 与 “能不能 spawn” 是两套不同的安全约束。前者关注生命周期，后者关注跨线程与跨栈帧存活。

接下来我们从底层并发安全角度解释 **Send / Sync / Pin / async 状态机**，并结合 pb-mapper 的真实代码说明。

### 1.1 阅读路线（建议）
- **先读 §3 和 §6**：理解 async 状态机与 Pin，建立心智模型  
- **再读 §4/§5**：理解 Send/'static 与 spawn 约束  
- **最后读 §8~§10**：落地到 split/into_split 的工程决策  

### 1.2 总体流程图（从概念到约束）
```
async fn
  └─ 编译为状态机 (Future)
       ├─ .await -> 需要 Pin 保证地址稳定
       └─ spawn? -> 需要 Send + 'static
```

---

## 2. 背景与术语（先统一语言）

### 2.1 术语速查
> 📝 **Terminology**
> - **Send**：一个值能否安全地跨线程移动。
> - **Sync**：一个类型的共享引用（`&T`）能否跨线程共享。
> - **'static**：值不含对栈帧的借用；可在程序任意时刻存在。
> - **Pin**：保证内存位置固定，用于自引用安全。
> - **Future 状态机**：`async fn` 编译后的结构体，内部保存跨 `.await` 需要的变量。

### 2.2 Send/Sync/'static 的关系
它们是 **正交约束**：
- `Send` 不代表 `'static`（可以 Send 但带借用）。
- `'static` 不代表 `Send`（比如内部含 `Rc`）。
- `Sync` 针对共享引用，`Send` 针对所有权移动。

> ⚠️ **Gotcha**：
> “有所有权就不需要证明 Send”是错误的。所有权只让 `'static` 更容易成立，但 **仍然要满足 Send**。

### 2.3 前置知识（建议）
- Rust move/borrow 的基本规则  
- `async/await` 的语法与 `.await` 暂停点概念  
- `tokio::spawn` 与执行器的基本模型  

---

## 3. async 的“栈帧”到底是什么
本质：**async fn 会被编译成一个状态机结构体**。每个 `.await` 是一个状态，跨 `.await` 存活的局部变量会变成字段。

### 3.0 Overview：异步调度循环
```
executor.poll(future)
   ├─ Ready  -> 完成
   └─ Pending -> 注册 waker -> 等待 I/O -> 再次 poll
```
这个循环决定了：future 会被反复 poll，因此需要**稳定地址**（Pin）与**明确的生命周期边界**（'static/借用）。

### 3.1 状态机的直观模型
```
async fn f() {
    let mut x = 1;
    foo().await;
    x += 1;
}

==> 状态机（伪结构）

struct F { state: State, x: u64, inner: Option<FooFuture> }
```

- `x` 需要跨 `.await` 活着，所以变成字段。
- `inner` 保存 `foo().await` 的 future。

> 💡 **Key Point**：
> async 的“栈帧”就是这个状态机结构体，不是 OS 线程栈。

### 3.2 为什么 `.await` 后借用仍有效
如果一个 future 在 `.await` 期间持有借用，只要 **外层状态机还活着**，借用就有效。因为状态机本身被 Pin 住，不会移动。

这就是为什么在 pb-mapper 里 **直接 `.await`** `forward_local_to_remote(...)` 是合法的：它持有了 `split()` 的借用半边，但外层任务还活着，所以生命周期是安全的。

> ⏭️ 接下来我们把“生命周期安全”扩展到“跨线程安全”，引出 Send/Sync 与 spawn 约束。

---

## 4. Send：跨线程移动为什么需要证明

### 4.1 Send 的“并发安全”含义
`Send` 的核心是：**把值移动到另一个线程不会导致数据竞争或悬垂引用**。

- `Rc<T>` 不是 Send：它的引用计数不是原子操作。
- `Arc<T>` 是 Send：引用计数是原子操作。

### 4.2 “Send 但非 'static” 的例子
```rust
fn make_future<'a>(x: &'a mut u64) -> impl Future<Output=()> + Send + 'a {
    async move { *x += 1; }
}
```
- 这个 future **不是 `'static`**，因为它借用 `x`。
- 但它仍可 `Send`，因为 `&mut u64` 跨线程移动是安全的（生命周期仍由调用方保证）。

> 🤔 **Think About**：
> “跨线程移动”与“能活多久”完全是两个问题。

> ⏭️ 下一节会解释：为什么 `tokio::spawn` 同时要求 `Send + 'static`，而不是只要其中之一。

---

## 5. 为什么 `tokio::spawn` 需要 `Send + 'static`

### 5.1 `spawn` 的约束来源
`tokio::spawn` 会把任务交给运行时调度：
- 任务可能在**任意线程**执行（需要 `Send`）
- 任务可能在当前函数返回后继续存在（需要 `'static`）

因此要求：`Future + Send + 'static`。

### 5.2 pb-mapper 里的实际调用点
在 pb-mapper 中，`tokio::spawn` 包裹了 `handle_local_stream` / `handle_stream`：
- `src/local/client/mod.rs:164`
- `src/local/server/mod.rs:309`

这些 async 函数内部会 `.await StreamForward::forward_local_to_remote`，因此 **forward 的 future 必须是 Send**，否则外层任务就不是 Send。

> ⚠️ **Gotcha**：
> “只要 forward 本身是 async 就行”是错的。**它必须满足外层 spawn 对 Send 的要求**。

> ⏭️ 下面进入 Pin：即使满足 Send/'static，async 仍需要 Pin 来保障“地址稳定”。

---

## 6. Pin：为什么 async 需要它
简述：`Pin` 不是“锁”，而是**对“是否允许移动”的编译期约束**。async 状态机之所以需要它，是因为编译器会生成可能**自引用**的结构体，而自引用一旦移动地址就会失效。

### 6.0 Overview：Pin 与 poll API 的关系
在 Rust async I/O 里，核心 trait 的 poll 方法都要求 `Pin<&mut Self>`。这是一种“强制开发者承认地址稳定性”的 API 设计：

```rust
trait AsyncRead {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>>;
}
```

这也解释了为什么 uni-stream 的 `UdpStreamReadHalf`/`UdpStream` 实现里，`poll_read` 的签名都是 `Pin<&mut Self>`（`deps/uni-stream/src/udp.rs:527`、`deps/uni-stream/src/udp.rs:585`）。  
**poll 需要 Pin 的根因：执行器会反复 poll，不能让 self 的地址在 poll 之间变化。**

### 6.1 Overview：自引用为什么会出现
async 状态机常常“内部持有对自身字段的借用”。例如外层 future 持有 `inner`，而 `inner` 又借用外层的 `x`：

```
[堆上的 Task]
+--------------------+
| Outer Future       |
|  - x: u64          |
|  - inner: &x ----- |----> (指向同一块内存)
+--------------------+
```

一旦 `Outer Future` 被移动到新地址，`inner` 里的引用就悬垂。

> 💡 **Key Point**：
> Pin 的目标不是“禁止修改”，而是**保证地址稳定**，从而让这种编译器生成的自引用合法。

### 6.2 什么时候会发生“移动”？
在 Rust 里，“移动”是默认语义。**只要你把值按值传递、赋值、放进容器或从函数返回，都会发生移动**。

```rust
let a = String::from("hello");
let b = a;            // move：a 被移动到 b

fn take(x: String) {} // move into function
take(b);

let mut v = Vec::new();
v.push(String::from("x")); // move into Vec
```

还有一种更隐蔽的移动：**容器扩容**。例如 Vec 在扩容时会搬迁元素。

```rust
let mut v = Vec::with_capacity(1);
v.push(String::from("a"));
v.push(String::from("b")); // 可能触发 reallocate -> 元素整体移动
```

> ⚠️ **Gotcha**：
> “我没有显式移动，所以不会移动”是错的。Rust 的移动是默认语义。

### 6.3 Pin 是“怎么做到不被移动”的？
Pin 不是运行时锁，而是**类型系统 + API 约束**：

- `Pin<&mut T>` 对 **`T: !Unpin`** 的类型，不允许你拿到普通的 `&mut T`。  
  也就是说，你无法调用会移动 `T` 的 API（比如 `mem::replace`）。
- 只有在 `T: Unpin` 时，`Pin` 才“退化”为普通引用。

示例：
```rust
use std::pin::Pin;

async fn work() {}

let mut fut = Box::pin(work()); // Pin<Box<Future>>

// ❌ 不能把 pinned future 整体拿出来移动
// let moved = *fut;

// ✅ 可以安全地 poll，因为 poll 接受 Pin<&mut T>
```

> 💡 **Key Point**：
> Pin 不是禁止“所有操作”，而是**禁止会导致地址变化的操作**。

### 6.4 Unpin：Pin 什么时候“失效”
大多数普通类型都实现了 `Unpin`，这意味着它们“即使被 Pin 了，也可以安全移动”。

```rust
use std::pin::Pin;

let mut x = 123u64;
let mut p = Pin::new(&mut x); // u64: Unpin

// 你仍然可以拿到 &mut u64（因为 Unpin）
let r: &mut u64 = Pin::get_mut(p);
```

而 async future 通常 **不是 Unpin**，因为它们可能包含自引用。

> 📝 **Terminology**：
> **Unpin** 表示“即使被 Pin 住，也允许移动”。Pin 只对 `!Unpin` 类型有约束意义。

### 6.5 错误示例：自引用 + 移动导致灾难

#### 错误示例 A：自引用结构体（编译器直接拒绝）
```rust
struct SelfRef<'a> {
    buf: String,
    slice: &'a str,
}

fn bad() -> SelfRef<'static> {
    let buf = String::from("hello");
    let slice = &buf[..];
    SelfRef { buf, slice } // ❌ buf 不够长，借用无效
}
```
这段代码无法编译，因为 `slice` 试图借用 `buf`，但 `buf` 会在返回时被移动。

#### 错误示例 B：手动移动 pinned value
```rust
use std::pin::Pin;

async fn work() {}
let mut fut = Box::pin(work());

// ❌ 不能通过 replace 交换 pinned future（需要 &mut T）
// std::mem::replace(&mut *fut, work());
```
`Pin` 让你拿不到 `&mut T`，所以编译器阻止这类“可能导致移动”的操作。

> ⚠️ **Gotcha**：
> Pin 不是 “运行时检测”，而是**编译期限制**。要绕过它必须 `unsafe`，也就意味着你要自己承担移动后悬垂的责任。

### 6.6 真实案例：uni-stream 的 UDP 超时与 Pin
在 uni-stream 的 `UdpStreamReadHalf` 中，超时计时器使用 `Pin<Box<Sleep>>` 存放：`deps/uni-stream/src/udp.rs:547`。原因是 `tokio::time::Sleep` 是 `!Unpin` 类型，poll/reset 都需要 `Pin<&mut Sleep>`。

真实调用路径：
1) `impl AsyncRead for UdpStreamReadHalf` 的 `poll_read` 接收 `Pin<&mut Self>` 并直接传递给内部读取逻辑（`deps/uni-stream/src/udp.rs:585`）。  
2) `recv_datagram` 在超时路径里调用 `self.timeout.as_mut()`（`deps/uni-stream/src/udp.rs:615`），之后调用 `reset`（`deps/uni-stream/src/udp.rs:639`）。

这意味着：**一旦 `UdpStreamReadHalf` 被 poll 过，它里面的 `Sleep` 绝对不能再被移动**。否则计时器在 runtime 的内部指针会悬垂。

ASCII 流程图：
```
poll_read()
  └── impl_inner::poll_read(self: Pin<&mut UdpStreamReadHalf>)
      └── timeout.poll(...) / timeout.reset(...)
          └── Sleep 被注册到 runtime 的 timer wheel
```

> 💡 **Key Point**：
> `Pin<Box<Sleep>>` 把 Sleep 固定在堆上，只要 Box 不被移动，Sleep 的地址就稳定；即使 `UdpStreamReadHalf` 自身被移动，Sleep 也不会被移动。

### 6.7 “如果不用 Pin 会怎样”：真实的编译报错场景
假设我们直接把 `Sleep` 放在结构体里，并在 `poll_read` 里调用 `poll`：

```rust
use tokio::time::Sleep;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::ReadBuf;

struct BadReadHalf {
    timeout: Sleep,
}

impl tokio::io::AsyncRead for BadReadHalf {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // ❌ 这里会报错：Sleep 不是 Unpin，不能通过 &mut 调用 poll
        // self.timeout.poll_unpin(cx);
        Poll::Ready(Ok(()))
    }
}
```

问题点在于：`Sleep` 需要 `Pin<&mut Sleep>`，而 `self.timeout` 只是一个普通字段。  
如果你强行拿 `&mut self.timeout`，编译器会拒绝，因为 **你没有证明整个 `BadReadHalf` 已经被固定在内存地址上**。

在 uni-stream 的实现中，`timeout: Pin<Box<Sleep>>` 直接把字段固定，从而绕开“投影 Pin”的复杂性，并且能在 `recv_datagram` 里安全调用 `reset`。  
这就是 `Pin` 在真实 async I/O 中最常见的用法：**把 `!Unpin` 的状态机/计时器固定在堆上**，再把结构体当作正常值使用。

> ⚠️ **Gotcha**：
> 你可以用 `pin-project` 或 `unsafe` 自己投影 `Pin<&mut Self>` 到 `Pin<&mut Field>`，但这需要你保证“字段不会移动”。用 `Pin<Box<T>>` 是更简单、也更常见的工程做法。

### 6.8 自引用 Vec 的 front：如何安全处理
问题场景：你想在结构体里 **保存指向 `Vec` 内部元素的引用/指针**，例如“总是指向 `front` 的数据”。

```rust
struct Bad {
    buf: Vec<u8>,
    front: *const u8, // 指向 buf[0]
}

impl Bad {
    fn new() -> Self {
        let mut buf = vec![1, 2, 3];
        let front = buf.as_ptr();
        Self { buf, front }
    }

    fn push(&mut self, v: u8) {
        self.buf.push(v); // 可能触发 reallocate -> front 失效
    }
}
```

这个例子**编译能过**，但逻辑上不安全：`push` 可能导致 `Vec` 扩容，`front` 指针悬垂。

> ⚠️ **Gotcha**：
> Pin 只保证 `buf` 这个字段本身的地址不变，**不会保证 `buf` 的内部堆内存不移动**。

安全处理的几种方式：

**方案 A：存索引，不存指针/引用**（最安全、推荐）  
```rust
struct Good {
    buf: Vec<u8>,
    front_idx: usize, // 0
}

impl Good {
    fn front(&self) -> Option<&u8> {
        self.buf.get(self.front_idx)
    }
}
```

**方案 B：固定容量，禁止扩容**（需要你守约）  
```rust
let mut buf = Vec::with_capacity(1024);
buf.extend_from_slice(&[1, 2, 3]);
let front = buf.as_ptr();
// 之后只允许写入不超过 1024 的元素
```
这不靠类型系统保证，需要你在逻辑上维护“不再扩容”的不变量。

**方案 C：把元素单独固定**（稳定地址）  
```rust
let mut buf: Vec<Pin<Box<u8>>> = Vec::new();
buf.push(Box::pin(1));
let front: *const u8 = &*buf[0]; // 元素地址稳定
```

**方案 D：转成不可增长的切片**  
```rust
let buf = vec![1,2,3].into_boxed_slice(); // Box<[u8]>
let front = buf.as_ptr();
// 不可再 push，地址稳定
```

> 💡 **Key Point**：
> 只要你需要“内部元素地址稳定”，就不能依赖普通 Vec 的 push；要么避免自引用、要么保证不扩容、要么把元素独立固定。

---

## 7. 为什么 Box 之后更“容易”通过

### 7.1 Box 并不会免除 Send 证明
`Box<dyn Future + Send>` **仍然要求内部 future 是 Send**。我们此前已经遇到“boxed 仍然提示 future !Send”的报错。

Box 的作用是：
- **类型擦除**（trait object）
- **避免复杂生命周期/泛型推导**（单点证明）

### 7.2 为什么 RPITIT（async fn in trait）会掉链子
> 📝 **Terminology**：
> **RPITIT** = *Return Position Impl Trait In Trait*，指在 trait 方法返回位置写 `-> impl Trait`。  
> **async fn in trait** 是 RPITIT 的语法糖：它会被编译器“降糖”为返回一个 **opaque future** 的 `-> impl Future`。

`async fn in trait` 会生成 **opaque future**，并且对泛型生命周期需要“全称证明”。
当 future 捕获 `ReadHalf<'a>` / `WriteHalf<'a>` 这类借用时，编译器很难证明：
> 对任意 `'a`，该 future 都是 Send。

这就是之前出现 “lifetime bound not satisfied / AsyncRead not general enough” 的核心原因。

> 💡 **Key Point**：
> 我们把 RPITIT 和 async fn in trait 放在一起，是因为**后者就是前者在 async 场景下的具体形态**。

> 💡 **Key Point**：
> 这不是“实际不安全”，而是 **类型系统当前推导能力的限制**。

> ⏭️ 这类限制在工程上通常通过 “boxed future” 或 “owned halves” 规避，下一节直接落地到 split/into_split。

---

## 8. 借用 split vs owned split（`into_split` 的意义）

### 8.1 借用 split 的特点
- `split()` 得到的是 **借用半边**（`ReadHalf<'a>`）
- 只能在当前任务内 `.await`
- 无法直接 `spawn`

### 8.2 owned split 的特点
- `into_split()` 得到的是 **owned halves**（`OwnedReadHalf`）
- 满足 `'static`，更容易满足 `Send + 'static`
- 可以安全丢给 `tokio::spawn`

在 uni-stream 中，我们已经添加了这个接口：
- `StreamSplit::into_split`（`deps/uni-stream/src/stream.rs:20`）
- TCP 实现与 UDP 实现（`deps/uni-stream/src/stream.rs:98`）
- UDP owned halves 与 guard 结构（`deps/uni-stream/src/udp.rs:458`）

> 📝 **Terminology**：
> **Owned halves** 指的是读写半边“拥有底层资源”，不再依赖外部借用。

---

## 9. 结合 pb-mapper 的实战解读

### 9.1 为什么 `StreamForward` 要返回 `Pin<Box<dyn Future + Send>>`
`StreamForward` 被 `tokio::spawn` 的路径间接调用，因此返回 future 必须 `Send`。
我们用 Box 来规避 RPITIT 对生命周期/Send 证明的推导限制。

参考：`src/common/message/forward.rs:311`。

### 9.2 如果未来要“零分配”怎么办
要做到真正的“零分配 + spawn”，必须：
- 使用 `into_split()` 获取 owned halves（`'static`）
- 避免 trait-level `async fn` 的 opaque future 推导问题
- 在可能的情况下用 `impl Future + Send + 'static` 返回值

否则一旦牵扯借用半边，就会回到 RPITIT 限制与 `tokio::spawn` 冲突的问题。

---

## 10. 设计权衡与实践建议

| 方案 | 优点 | 代价 | 适用场景 |
|---|---|---|---|
| `split()` + 直接 `.await` | 零拷贝，简单 | 不能 spawn | 同一任务内处理 |
| `split()` + Box | 可 spawn（类型擦除） | 堆分配 | 需要 spawn + 借用半边 |
| `into_split()` + spawn | 可 spawn，'static | 需要 owned halves | 高并发、跨线程 |

> ⚠️ **Gotcha**：
> 如果你看到 `Send` 报错，不要第一时间怪数据结构，而是先检查：
> 1) 你的 future 是否被 `spawn` 包裹；
> 2) 是否捕获了非 `'static` 借用；
> 3) 是否触发了 RPITIT 的推导限制。

---

## 11. Code Index（可直接跳转）
- `src/local/client/mod.rs:164`（client 侧 spawn）
- `src/local/server/mod.rs:309`（server 侧 spawn）
- `src/common/message/forward.rs:311`（StreamForward 返回 boxed Send future）
- `deps/uni-stream/src/stream.rs:20`（StreamSplit + into_split 关联类型）
- `deps/uni-stream/src/udp.rs:458`（UDP owned halves + guard）

---

## 12. References
- Rust async/await 语义（编译为状态机）
- Tokio `spawn` 的 `Send + 'static` 约束

> 注：本文所有代码引用均来自本仓库上述文件路径与行号。
