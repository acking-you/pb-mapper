# Rinf中的Actor模型详解

## 什么是Actor模型？

Actor模型是一种并发计算模型，它将"Actor"作为基本的计算单元。每个Actor都是一个独立的实体，封装了自己的状态和行为，并通过异步消息传递与其他Actor通信。

## Actor模型的核心概念与具体示例

### 1. 封装性 (Encapsulation)
每个Actor封装自己的内部状态，外部无法直接访问Actor的内部数据，确保状态安全，避免竞态条件。

**具体示例**：
```rust
// 用户会话Actor，封装了用户的所有会话状态
pub struct UserSessionActor {
    user_id: String,                // 用户ID - 私有状态
    login_time: SystemTime,         // 登录时间 - 私有状态
    message_count: u32,             // 消息计数 - 私有状态
    is_online: bool,                // 在线状态 - 私有状态
    friends: HashSet<String>,       // 好友列表 - 私有状态
    _owned_tasks: JoinSet<()>,      // 内部队列任务管理
}

impl UserSessionActor {
    // 通过公共方法提供受控的状态访问
    pub fn get_user_id(&self) -> &String {
        &self.user_id
    }
    
    pub fn is_user_online(&self) -> bool {
        self.is_online
    }
    
    // 状态只能通过消息处理内部修改
    async fn handle_login(&mut self, _: LoginMessage, _: &Context<Self>) {
        self.is_online = true;
        self.login_time = SystemTime::now();
        // 通知好友该用户已上线
        self.notify_friends_online().await;
    }
    
    async fn handle_message(&mut self, _: ChatMessage, _: &Context<Self>) {
        self.message_count += 1;
        // 更新最后活动时间
        self.last_activity = SystemTime::now();
    }
}
```

**关键特点**：
- 所有状态字段都是私有的，无法从外部直接修改
- 状态变更只能通过Actor内部的消息处理函数进行
- 提供受控的只读访问方法，但不暴露可变引用

### 2. 消息传递 (Message Passing)
Actor之间只能通过异步消息进行通信，不共享内存，避免传统并发编程的复杂性，提供松耦合的系统架构。

**具体示例**：
```rust
// 定义不同类型的消息
#[derive(Deserialize, DartSignal)]
pub struct UserLogin {
    pub user_id: String,
    pub password: String,
}

#[derive(Deserialize, DartSignal)]
pub struct SendMessage {
    pub recipient_id: String,
    pub content: String,
}

#[derive(Deserialize, DartSignal)]
pub struct UserLogout {
    pub user_id: String,
}

// 用户管理Actor处理不同类型的消息
#[async_trait]
impl Notifiable<UserLogin> for UserManagerActor {
    async fn notify(&mut self, msg: UserLogin, _: &Context<Self>) {
        // 处理用户登录请求
        if self.authenticate_user(&msg.user_id, &msg.password).await {
            // 创建新的用户会话Actor
            let session_actor = UserSessionActor::new(msg.user_id.clone());
            let session_context = Context::new();
            let session_addr = session_context.address();
            
            // 启动用户会话Actor
            spawn(session_context.run(session_actor));
            
            // 保存会话地址到用户管理器中
            self.active_sessions.insert(msg.user_id, session_addr);
            
            // 发送登录成功响应
            LoginResponse {
                success: true,
                message: "Login successful".to_string(),
            }.send_signal_to_dart();
        }
    }
}

#[async_trait]
impl Notifiable<SendMessage> for UserManagerActor {
    async fn notify(&mut self, msg: SendMessage, _: &Context<Self>) {
        // 向指定用户发送消息
        if let Some(session_addr) = self.active_sessions.get(&msg.recipient_id) {
            // 通过地址异步发送消息给目标用户的Actor
            session_addr.notify(ChatMessage {
                sender_id: self.current_user_id.clone(),
                content: msg.content,
            }).await;
        }
    }
}
```

**关键特点**：
- 消息是数据结构，包含处理所需的所有信息
- 消息通过Actor地址异步发送，发送方不需要等待响应
- 每个Actor可以处理多种类型的消息
- 消息队列保证了消息处理的顺序性

### 3. 地址 (Address)
每个Actor有唯一的地址标识符，其他组件通过地址向Actor发送消息，实现位置透明性和动态发现。

**具体示例**：
```rust
// Actor地址的使用示例
pub async fn create_actors() {
    // 1. 创建用户管理Actor
    let user_manager_context = Context::new();
    let user_manager_addr = user_manager_context.address();  // 获取地址
    let user_manager = UserManagerActor::new(user_manager_addr.clone());
    spawn(user_manager_context.run(user_manager));
    
    // 2. 创建聊天室Actor，并传递用户管理器地址
    let chat_room_context = Context::new();
    let chat_room_addr = chat_room_context.address();  // 获取地址
    let chat_room = ChatRoomActor::new(chat_room_addr.clone(), user_manager_addr.clone());
    spawn(chat_room_context.run(chat_room));
    
    // 3. 在聊天室Actor中使用其他Actor的地址
    impl ChatRoomActor {
        async fn handle_group_message(&mut self, msg: GroupMessage, _: &Context<Self>) {
            // 向用户管理器查询所有在线用户
            let online_users = self.user_manager_addr
                .notify_and_await(GetOnlineUsers {}).await;
            
            // 向每个在线用户发送群消息
            for user_id in online_users.users {
                if let Some(session_addr) = self.user_sessions.get(&user_id) {
                    // 使用保存的地址发送消息
                    session_addr.notify(GroupChatMessage {
                        sender_id: msg.sender_id.clone(),
                        content: msg.content.clone(),
                        timestamp: SystemTime::now(),
                    }).await;
                }
            }
        }
        
        // 动态发现并连接新的Actor
        async fn add_user_to_room(&mut self, user_id: String) {
            // 请求用户管理器获取用户会话地址
            let session_addr = self.user_manager_addr
                .notify_and_await(GetUserSession { user_id: user_id.clone() })
                .await
                .session_addr;
                
            // 保存地址以供后续使用
            self.user_sessions.insert(user_id, session_addr);
        }
    }
}

// 地址作为Actor间通信的唯一标识
struct ChatRoomActor {
    room_id: String,
    user_manager_addr: Address<UserManagerActor>,  // 用户管理器地址
    user_sessions: HashMap<String, Address<UserSessionActor>>,  // 用户会话地址映射
    message_history: Vec<ChatMessage>,
}
```

**关键特点**：
- Address是Actor的唯一标识和通信端点
- 可以保存和传递地址以实现Actor间通信
- 支持位置透明性，无需知道Actor的具体实现位置
- 地址可以在Actor间传递，实现动态发现

### 4. 并发独立性 (Concurrency Independence)
每个Actor独立处理消息，可以并发执行多个Actor，简化多线程编程复杂性。

**具体示例**：
```rust
// 并发执行多个独立的Actor
pub async fn create_actors() {
    // 1. 创建多个独立的用户会话Actor
    let mut user_actors = Vec::new();
    
    for user_id in ["user1", "user2", "user3", "user4"] {
        let context = Context::new();
        let addr = context.address();
        let actor = UserSessionActor::new(user_id.to_string(), addr.clone());
        spawn(context.run(actor));
        user_actors.push(addr);
    }
    
    // 2. 创建文件处理Actor
    let file_processor_context = Context::new();
    let file_processor_addr = file_processor_context.address();
    let file_processor = FileProcessorActor::new(file_processor_addr.clone());
    spawn(file_processor_context.run(file_processor));
    
    // 3. 创建数据库Actor
    let database_context = Context::new();
    let database_addr = database_context.address();
    let database = DatabaseActor::new(database_addr.clone());
    spawn(database_context.run(database));
    
    // 4. 所有Actor并发独立运行
    // 用户会话Actor处理聊天消息
    // 文件处理Actor处理文件上传下载
    // 数据库Actor处理数据持久化
    
    // 每个Actor都有自己的消息队列和执行上下文
    println!("All actors started and running concurrently");
}

// 每个Actor独立处理自己的消息队列
impl UserSessionActor {
    async fn run_message_loop(&mut self) {
        // 每个Actor独立处理消息，不会阻塞其他Actor
        while let Some(message) = self.message_queue.recv().await {
            match message {
                Message::Chat(chat_msg) => {
                    self.handle_chat_message(chat_msg).await;
                    // 处理聊天消息不会影响其他用户会话
                }
                Message::FileTransfer(file_msg) => {
                    self.handle_file_transfer(file_msg).await;
                    // 文件传输在当前Actor内处理
                }
                Message::StatusUpdate(status_msg) => {
                    self.handle_status_update(status_msg).await;
                    // 状态更新不影响其他Actor
                }
            }
        }
    }
}

// 文件处理Actor示例
struct FileProcessorActor {
    pending_transfers: HashMap<String, FileTransferState>,
    max_concurrent_transfers: usize,
    current_transfers: usize,
}

#[async_trait]
impl Notifiable<StartFileTransfer> for FileProcessorActor {
    async fn notify(&mut self, msg: StartFileTransfer, _: &Context<Self>) {
        // 独立处理文件传输请求
        if self.current_transfers < self.max_concurrent_transfers {
            self.current_transfers += 1;
            let transfer_id = generate_transfer_id();
            
            // 异步处理文件传输，不会阻塞接收新消息
            spawn(async move {
                self.process_file_transfer(msg.file_path, msg.recipient).await;
            });
        } else {
            // 队列处理，不影响其他Actor
            self.pending_transfers.insert(
                generate_transfer_id(), 
                FileTransferState::Pending(msg)
            );
        }
    }
}
```

**关键特点**：
- 每个Actor有独立的消息队列和执行上下文
- Actor间不会相互阻塞，可以并发执行
- 消息处理是顺序的，但不同Actor可以并行处理
- 简化了多线程编程，避免了锁和同步原语

## Rinf中Actor的实现

### 基本结构

```rust
// 定义Actor结构体
pub struct CountingActor {
    count: i32,           // 状态变量
    _owned_tasks: JoinSet<()>, // 管理子任务
}

// 实现Actor trait
impl Actor for CountingActor {}

// Actor初始化
impl CountingActor {
    pub fn new(self_addr: Address<Self>) -> Self {
        let mut owned_tasks = JoinSet::new();
        // 启动监听任务
        owned_tasks.spawn(Self::listen_to_button_click(self_addr));
        CountingActor {
            count: 0,
            _owned_tasks: owned_tasks,
        }
    }
}
```

### 消息处理

```rust
// 处理来自Dart的消息
#[async_trait]
impl Notifiable<SampleNumberInput> for CountingActor {
    async fn notify(&mut self, msg: SampleNumberInput, _: &Context<Self>) {
        // 更新内部状态
        self.count += 7;
        
        // 发送响应到Dart
        SampleNumberOutput {
            current_number: self.count,
        }.send_signal_to_dart();
    }
}
```

### Actor创建和管理

```rust
// 创建并启动Actor
pub async fn create_actors() {
    // 创建Actor上下文
    let counting_context = Context::new();
    let counting_addr = counting_context.address();
    
    // 实例化Actor
    let counting_actor = CountingActor::new(counting_addr);
    
    // 启动Actor
    spawn(counting_context.run(counting_actor));
}
```

## Address的作用

在Rinf的Actor模型中，`Address`是一个非常重要的概念：

1. **Actor标识符**：`Address`是Actor的唯一标识，用于在系统中定位特定的Actor实例
2. **消息发送端点**：其他组件通过`Address`向Actor发送消息
3. **异步通信通道**：`Address`内部维护了一个消息队列，支持异步消息传递

```rust
// 1. 创建Actor上下文和地址
let context = Context::new();
let addr = context.address();  // 获取该Actor的地址

// 2. 通过地址发送消息
addr.notify(SomeMessage { data: 42 }).await;

// 3. Actor内部也可以保存其他Actor的地址用于通信
struct MyActor {
    other_actor: Address<OtherActor>,  // 保存其他Actor的地址
}

impl MyActor {
    async fn notify(&mut self, _: SomeMessage, _: &Context<Self>) {
        // 向其他Actor发送消息
        self.other_actor.notify(AnotherMessage {}).await;
    }
}
```

## Actor模型的应用场景

1. **状态管理**：GUI应用的状态管理
2. **并发处理**：处理大量并发请求
3. **业务逻辑分离**：将不同业务逻辑封装在不同Actor中
4. **异步任务处理**：后台任务、定时任务等

## Actor模型的优势

1. **状态安全**：每个Actor管理自己的状态，避免并发访问问题
2. **可扩展性**：可以轻松创建多个Actor实例处理不同任务
3. **模块化**：每个Actor职责单一，便于维护和测试
4. **异步友好**：天然支持异步操作，不会阻塞其他Actor
5. **容错性**：一个Actor的崩溃不会影响其他Actor

## Actor模型与C++26 Sender/Receiver模型的对比

### 什么是C++26 Sender/Receiver模型

C++26中的Sender/Receiver是一种现代化的异步编程模型，基于三个核心抽象：调度器(Scheduler)、发送者(Sender)和接收者(Receiver)：

- **Sender**：表示异步操作的对象。Senders是"惰性"的 - 它们在连接到Receiver并提交给执行器之前不会开始执行。
  Sender负责描述要执行的工作，当操作完成时会"发送"值、错误或停止信号。

- **Receiver**：表示当Sender完成时将执行的回调。Receiver定义了如何处理三种完成信号：
  - `set_value()`：用于成功完成操作并传递结果值
  - `set_error()`：用于完成错误操作并传递错误信息
  - `set_stopped()`：用于表示操作被取消

- **Scheduler**：表示执行资源上调度工作的策略的轻量级句柄。
  Scheduler的概念由单一Sender算法`schedule`定义，该算法返回一个Sender，
  该Sender将在由Scheduler确定的执行资源上完成。

它们通过明确定义的接口一起工作，其中Sender使用`connect`操作连接到Receiver以形成操作状态，
然后使用`start`操作提交给执行器进行执行。

### C++26 Sender/Receiver具体示例

#### 示例1：基本的Sender/Receiver使用

```cpp
#include <iostream>
#include <execution>
#include <this_thread>

// 创建一个简单的 sender，发送一个整数值
auto sender = std::execution::just(42);

// 定义一个 receiver 来处理结果
class SimpleReceiver {
public:
    // 处理成功完成的情况
    void set_value(int value) {
        std::cout << "Received value: " << value << std::endl;
    }
    
    // 处理错误情况
    void set_error(std::exception_ptr e) {
        std::cout << "Error occurred" << std::endl;
    }
    
    // 处理取消情况
    void set_stopped() {
        std::cout << "Operation was cancelled" << std::endl;
    }
};

// 使用示例
int main() {
    auto receiver = SimpleReceiver{};
    
    // 连接 sender 和 receiver
    auto operation_state = std::execution::connect(sender, receiver);
    
    // 启动操作
    std::execution::start(operation_state);
    
    return 0;
}
```

#### 示例2：链式操作和转换

```cpp
#include <iostream>
#include <execution>
#include <this_thread>
#include <string>

int main() {
    // 创建一个链式操作
    auto sender = std::execution::just(10)
        | std::execution::then([](int x) {
            return x * 2;  // 将10乘以2得到20
        })
        | std::execution::then([](int x) {
            return std::to_string(x);  // 转换为字符串"20"
        })
        | std::execution::then([](std::string s) {
            return "Result: " + s;  // 添加前缀
        });
    
    // 使用 sync_wait 执行并获取结果
    auto result = std::this_thread::sync_wait(std::move(sender));
    
    if (result) {
        std::cout << "Final result: " << std::get<0>(*result) << std::endl;
        // 输出: Final result: Result: 20
    }
    
    return 0;
}
```

#### 示例3：使用调度器在不同执行上下文中

```cpp
#include <iostream>
#include <execution>
#include <this_thread>

int main() {
    // 获取系统调度器
    auto scheduler = std::execution::get_system_scheduler();
    
    // 创建一个在特定调度器上执行的操作
    auto sender = std::execution::schedule(scheduler)
        | std::execution::then([]() {
            std::cout << "Running on thread: " 
                      << std::this_thread::get_id() << std::endl;
            return 42;
        });
    
    auto result = std::this_thread::sync_wait(std::move(sender));
    
    if (result) {
        std::cout << "Result: " << std::get<0>(*result) << std::endl;
    }
    
    return 0;
}
```

### 主要区别

1. **设计哲学**
   - **Actor模型**：基于"主动"实体，每个Actor是独立的执行单元，主动处理消息
   - **Sender/Receiver模型**：基于"被动"数据流，数据在管道中流动，被接收者处理

2. **状态管理**
   - **Actor模型**：每个Actor拥有并管理自己的状态
   - **Sender/Receiver模型**：通常是无状态的，数据通过管道传递

3. **通信方式**
   - **Actor模型**：Actor通过异步消息传递通信，主动向其他Actor发送消息
   - **Sender/Receiver模型**：通过连接Sender和Receiver形成数据流管道，数据自动流向Receiver

4. **执行模型**
   - **Actor模型**：每个Actor独立执行，有自己的执行上下文，可以长期运行
   - **Sender/Receiver模型**：数据驱动，Receiver在有数据时被调用，操作通常短生命周期

5. **错误处理**
   - **Actor模型**：Actor内部处理错误或向特定错误处理Actor转发
   - **Sender/Receiver模型**：使用专门的`set_error()`通道处理错误，可以链式错误处理

### 相似之处

1. **异步性**：两者都支持异步编程
2. **组合性**：都可以组合复杂的操作
3. **解耦**：生产者和消费者之间解耦

### 适用场景对比

#### Actor模型的典型应用场景

**实时聊天系统中的用户会话**：
```rust
// 每个用户会话是一个Actor，维护自己的状态
struct UserSessionActor {
    user_id: String,
    connection_status: ConnectionStatus,
    message_history: Vec<Message>,
    last_activity: SystemTime,
}

#[async_trait]
impl Notifiable<IncomingMessage> for UserSessionActor {
    async fn notify(&mut self, msg: IncomingMessage, _: &Context<Self>) {
        // 更新用户状态
        self.last_activity = SystemTime::now();
        self.message_history.push(msg.content.clone());
        
        // 处理消息并发送响应
        match msg.content {
            MessageContent::Chat(text) => {
                // 发送给其他用户
                self.broadcast_message(&text).await;
            }
            MessageContent::StatusUpdate(status) => {
                self.connection_status = status;
                // 通知好友状态变更
                self.notify_friends().await;
            }
        }
    }
}
```

**特点**：
- 长生命周期的实体，维持会话状态
- 主动响应多种类型的消息
- 处理复杂的会话逻辑和状态转换

#### Sender/Receiver的典型应用场景

**传感器数据处理流水线**：
```cpp
// 处理传感器数据流水线
auto sensor_data = sensor_stream();  // 数据源

// 转换阶段1：数据清洗
auto cleaned_data = then(sensor_data, [](SensorReading reading) {
    return clean_sensor_data(reading);
});

// 转换阶段2：数据分析
auto analyzed_data = then(cleaned_data, [](CleanedData data) {
    return analyze_data(data);
});

// 转换阶段3：结果聚合
auto final_result = then(analyzed_data, [](AnalysisResult result) {
    return aggregate_results(result);
});

// 启动整个流水线
sync_wait(final_result);
```

**特点**：
- 数据流经多个转换阶段，从一端流入，另一端流出
- 每个阶段通常是无状态的纯函数
- 操作短生命周期，处理完即结束

### 何时选择哪种模型

- **选择Actor模型**当：
  - 你需要维护长生命周期状态
  - 实体需要主动响应多种类型的消息
  - 需要处理复杂的会话逻辑和状态转换
  - 构建GUI应用、游戏引擎、长时间运行的服务

- **选择Sender/Receiver模型**当：
  - 你需要构建数据处理流水线
  - 操作可以表示为一系列转换步骤
  - 任务是无状态的，每次处理独立数据项
  - 需要高效的数据流处理，如ETL流程、图像处理

虽然两者都是现代并发编程的重要工具，但解决的问题和适用场景有所不同。

## 相关参考资料

### Actor模型
- [Actor Model Wikipedia](https://en.wikipedia.org/wiki/Actor_model)
- [Actix Documentation](https://actix.rs/)
- [Erlang Actor Model](https://www.erlang.org/doc/reference_manual/processes.html)

### C++26 Sender/Receiver
- [P2300R0: std::execution](https://wg21.link/P2300)
- [Lewis Baker's Blog on Coroutines](https://lewissbaker.github.io/)
- [How to Use the Sender/Receiver Framework in C++ to Create a Simple HTTP Server](https://www.youtube.com/watch?v=Nnwanj5Ocrw)

## 总结

Rinf采用Actor模型是因为它非常适合GUI应用的状态管理。通过将应用状态分散到多个独立的Actor中，可以实现安全、高效的并发处理，同时保持代码的清晰和可维护性。