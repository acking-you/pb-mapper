# Actor Model in Rinf

## What is the Actor Model?

The Actor Model is a concurrent computation model that treats "Actors" as the fundamental units of computation. Each Actor is an independent entity that encapsulates its own state and behavior, and communicates with other Actors through asynchronous message passing.

## Core Concepts of the Actor Model with Concrete Examples

### 1. Encapsulation
Each Actor encapsulates its internal state. External entities cannot directly access an Actor's internal data, ensuring state safety and preventing race conditions.

**Concrete Example**:
```rust
// User session Actor encapsulating all user session state
pub struct UserSessionActor {
    user_id: String,                // User ID - private state
    login_time: SystemTime,         // Login time - private state
    message_count: u32,             // Message count - private state
    is_online: bool,                // Online status - private state
    friends: HashSet<String>,       // Friend list - private state
    _owned_tasks: JoinSet<()>,      // Internal task queue management
}

impl UserSessionActor {
    // Provide controlled state access through public methods
    pub fn get_user_id(&self) -> &String {
        &self.user_id
    }
    
    pub fn is_user_online(&self) -> bool {
        self.is_online
    }
    
    // State can only be modified internally through message handling
    async fn handle_login(&mut self, _: LoginMessage, _: &Context<Self>) {
        self.is_online = true;
        self.login_time = SystemTime::now();
        // Notify friends that user is online
        self.notify_friends_online().await;
    }
    
    async fn handle_message(&mut self, _: ChatMessage, _: &Context<Self>) {
        self.message_count += 1;
        // Update last activity time
        self.last_activity = SystemTime::now();
    }
}
```

**Key Characteristics**:
- All state fields are private and cannot be directly modified from outside
- State changes can only occur through internal message handling functions
- Controlled read-only access methods are provided without exposing mutable references

### 2. Message Passing
Actors can only communicate through asynchronous messages. No shared memory avoids the complexity of traditional concurrent programming and provides a loosely coupled system architecture.

**Concrete Example**:
```rust
// Define different types of messages
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

// User manager Actor handling different types of messages
#[async_trait]
impl Notifiable<UserLogin> for UserManagerActor {
    async fn notify(&mut self, msg: UserLogin, _: &Context<Self>) {
        // Handle user login request
        if self.authenticate_user(&msg.user_id, &msg.password).await {
            // Create new user session Actor
            let session_actor = UserSessionActor::new(msg.user_id.clone());
            let session_context = Context::new();
            let session_addr = session_context.address();
            
            // Start user session Actor
            spawn(session_context.run(session_actor));
            
            // Save session address in user manager
            self.active_sessions.insert(msg.user_id, session_addr);
            
            // Send login success response
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
        // Send message to specified user
        if let Some(session_addr) = self.active_sessions.get(&msg.recipient_id) {
            // Asynchronously send message to target user's Actor through address
            session_addr.notify(ChatMessage {
                sender_id: self.current_user_id.clone(),
                content: msg.content,
            }).await;
        }
    }
}
```

**Key Characteristics**:
- Messages are data structures containing all necessary information for processing
- Messages are sent asynchronously through Actor addresses without waiting for response
- Each Actor can handle multiple types of messages
- Message queues ensure sequential message processing

### 3. Address
Each Actor has a unique address identifier. Other components send messages to Actors through their addresses, enabling location transparency and dynamic discovery.

**Concrete Example**:
```rust
// Example of Actor address usage
pub async fn create_actors() {
    // 1. Create user manager Actor
    let user_manager_context = Context::new();
    let user_manager_addr = user_manager_context.address();  // Get address
    let user_manager = UserManagerActor::new(user_manager_addr.clone());
    spawn(user_manager_context.run(user_manager));
    
    // 2. Create chat room Actor and pass user manager address
    let chat_room_context = Context::new();
    let chat_room_addr = chat_room_context.address();  // Get address
    let chat_room = ChatRoomActor::new(chat_room_addr.clone(), user_manager_addr.clone());
    spawn(chat_room_context.run(chat_room));
    
    // 3. Use other Actor addresses in chat room Actor
    impl ChatRoomActor {
        async fn handle_group_message(&mut self, msg: GroupMessage, _: &Context<Self>) {
            // Query all online users from user manager
            let online_users = self.user_manager_addr
                .notify_and_await(GetOnlineUsers {}).await;
            
            // Send group message to each online user
            for user_id in online_users.users {
                if let Some(session_addr) = self.user_sessions.get(&user_id) {
                    // Send message using saved address
                    session_addr.notify(GroupChatMessage {
                        sender_id: msg.sender_id.clone(),
                        content: msg.content.clone(),
                        timestamp: SystemTime::now(),
                    }).await;
                }
            }
        }
        
        // Dynamically discover and connect to new Actors
        async fn add_user_to_room(&mut self, user_id: String) {
            // Request user session address from user manager
            let session_addr = self.user_manager_addr
                .notify_and_await(GetUserSession { user_id: user_id.clone() })
                .await
                .session_addr;
                
            // Save address for future use
            self.user_sessions.insert(user_id, session_addr);
        }
    }
}

// Address as unique identifier and communication endpoint for Actors
struct ChatRoomActor {
    room_id: String,
    user_manager_addr: Address<UserManagerActor>,  // User manager address
    user_sessions: HashMap<String, Address<UserSessionActor>>,  // User session address mapping
    message_history: Vec<ChatMessage>,
}
```

**Key Characteristics**:
- Address is the unique identifier and communication endpoint for Actors
- Addresses can be saved and passed to enable Actor communication
- Supports location transparency without knowing Actor's specific implementation location
- Addresses can be passed between Actors to enable dynamic discovery

### 4. Concurrency Independence
Each Actor processes messages independently. Multiple Actors can be executed concurrently, simplifying multi-threaded programming complexity.

**Concrete Example**:
```rust
// Concurrent execution of multiple independent Actors
pub async fn create_actors() {
    // 1. Create multiple independent user session Actors
    let mut user_actors = Vec::new();
    
    for user_id in ["user1", "user2", "user3", "user4"] {
        let context = Context::new();
        let addr = context.address();
        let actor = UserSessionActor::new(user_id.to_string(), addr.clone());
        spawn(context.run(actor));
        user_actors.push(addr);
    }
    
    // 2. Create file processor Actor
    let file_processor_context = Context::new();
    let file_processor_addr = file_processor_context.address();
    let file_processor = FileProcessorActor::new(file_processor_addr.clone());
    spawn(file_processor_context.run(file_processor));
    
    // 3. Create database Actor
    let database_context = Context::new();
    let database_addr = database_context.address();
    let database = DatabaseActor::new(database_addr.clone());
    spawn(database_context.run(database));
    
    // 4. All Actors run concurrently and independently
    // User session Actors handle chat messages
    // File processor Actor handles file uploads/downloads
    // Database Actor handles data persistence
    
    // Each Actor has its own message queue and execution context
    println!("All actors started and running concurrently");
}

// Each Actor independently processes its own message queue
impl UserSessionActor {
    async fn run_message_loop(&mut self) {
        // Each Actor independently processes messages without blocking other Actors
        while let Some(message) = self.message_queue.recv().await {
            match message {
                Message::Chat(chat_msg) => {
                    self.handle_chat_message(chat_msg).await;
                    // Processing chat messages doesn't affect other user sessions
                }
                Message::FileTransfer(file_msg) => {
                    self.handle_file_transfer(file_msg).await;
                    // File transfer is processed within current Actor
                }
                Message::StatusUpdate(status_msg) => {
                    self.handle_status_update(status_msg).await;
                    // Status updates don't affect other Actors
                }
            }
        }
    }
}

// File processor Actor example
struct FileProcessorActor {
    pending_transfers: HashMap<String, FileTransferState>,
    max_concurrent_transfers: usize,
    current_transfers: usize,
}

#[async_trait]
impl Notifiable<StartFileTransfer> for FileProcessorActor {
    async fn notify(&mut self, msg: StartFileTransfer, _: &Context<Self>) {
        // Independently handle file transfer requests
        if self.current_transfers < self.max_concurrent_transfers {
            self.current_transfers += 1;
            let transfer_id = generate_transfer_id();
            
            // Asynchronously process file transfer without blocking new message reception
            spawn(async move {
                self.process_file_transfer(msg.file_path, msg.recipient).await;
            });
        } else {
            // Queue processing without affecting other Actors
            self.pending_transfers.insert(
                generate_transfer_id(), 
                FileTransferState::Pending(msg)
            );
        }
    }
}
```

**Key Characteristics**:
- Each Actor has independent message queues and execution contexts
- Actors don't block each other and can execute concurrently
- Message processing is sequential, but different Actors can process in parallel
- Simplifies multi-threaded programming by avoiding locks and synchronization primitives

## Actor Implementation in Rinf

### Basic Structure

```rust
// Define Actor struct
pub struct CountingActor {
    count: i32,           // State variable
    _owned_tasks: JoinSet<()>, // Manage sub-tasks
}

// Implement Actor trait
impl Actor for CountingActor {}

// Actor initialization
impl CountingActor {
    pub fn new(self_addr: Address<Self>) -> Self {
        let mut owned_tasks = JoinSet::new();
        // Start listening task
        owned_tasks.spawn(Self::listen_to_button_click(self_addr));
        CountingActor {
            count: 0,
            _owned_tasks: owned_tasks,
        }
    }
}
```

### Message Handling

```rust
// Handle messages from Dart
#[async_trait]
impl Notifiable<SampleNumberInput> for CountingActor {
    async fn notify(&mut self, msg: SampleNumberInput, _: &Context<Self>) {
        // Update internal state
        self.count += 7;
        
        // Send response to Dart
        SampleNumberOutput {
            current_number: self.count,
        }.send_signal_to_dart();
    }
}
```

### Actor Creation and Management

```rust
// Create and start Actors
pub async fn create_actors() {
    // Create Actor context
    let counting_context = Context::new();
    let counting_addr = counting_context.address();
    
    // Instantiate Actor
    let counting_actor = CountingActor::new(counting_addr);
    
    // Start Actor
    spawn(counting_context.run(counting_actor));
}
```

## The Role of Address

In Rinf's Actor Model, `Address` is a crucial concept:

1. **Actor Identifier**: `Address` is the unique identifier of an Actor, used to locate specific Actor instances in the system
2. **Message Sending Endpoint**: Other components send messages to Actors through their addresses
3. **Asynchronous Communication Channel**: `Address` maintains an internal message queue that supports asynchronous message passing

```rust
// 1. Create Actor context and address
let context = Context::new();
let addr = context.address();  // Get the Actor's address

// 2. Send messages through the address
addr.notify(SomeMessage { data: 42 }).await;

// 3. Actors can also store addresses of other Actors for communication
struct MyActor {
    other_actor: Address<OtherActor>,  // Store address of another Actor
}

impl MyActor {
    async fn notify(&mut self, _: SomeMessage, _: &Context<Self>) {
        // Send message to another Actor
        self.other_actor.notify(AnotherMessage {}).await;
    }
}
```

## Application Scenarios

1. **State Management**: GUI application state management
2. **Concurrent Processing**: Handling large numbers of concurrent requests
3. **Business Logic Separation**: Encapsulating different business logic in different Actors
4. **Asynchronous Task Processing**: Background tasks, scheduled tasks, etc.

## Advantages of the Actor Model

1. **State Safety**: Each Actor manages its own state, avoiding concurrent access issues
2. **Scalability**: Easily create multiple Actor instances to handle different tasks
3. **Modularity**: Each Actor has a single responsibility, making maintenance and testing easier
4. **Asynchronous Friendly**: Naturally supports asynchronous operations without blocking other Actors
5. **Fault Tolerance**: The crash of one Actor does not affect other Actors

## Comparison with C++26 Sender/Receiver Model

### What is the C++ Sender/Receiver Model?

The Sender/Receiver concurrency model in C++26 is a modern asynchronous programming paradigm based on three key abstractions: schedulers, senders, and receivers:

- **Sender**: An object that describes work. Senders are "lazy" - they don't start executing until connected to a receiver and submitted for execution. A sender is said to "send" values, errors, or a stopped signal when an operation completes.

- **Receiver**: An object that represents the continuation or callback that will be executed when a sender completes. Receivers define how to handle three completion signals:
  - `set_value()`: Used for successful completion with result values
  - `set_error()`: Used for error completion with error information
  - `set_stopped()`: Used for cancellation

- **Scheduler**: A lightweight handle representing a strategy for scheduling work onto an execution resource. The scheduler concept is defined by a single sender algorithm, `schedule`, which returns a sender that completes on the execution resource determined by the scheduler.

They work together through a well-defined interface where a sender is connected to a receiver using the `connect` operation to form an operation state, which is then submitted to an executor for execution using the `start` operation.

### Concrete C++26 Sender/Receiver Examples

#### Example 1: Basic Sender/Receiver Usage

```cpp
#include <iostream>
#include <execution>
#include <this_thread>

// Create a simple sender that sends an integer value
auto sender = std::execution::just(42);

// Define a receiver to handle results
class SimpleReceiver {
public:
    // Handle successful completion
    void set_value(int value) {
        std::cout << "Received value: " << value << std::endl;
    }
    
    // Handle error case
    void set_error(std::exception_ptr e) {
        std::cout << "Error occurred" << std::endl;
    }
    
    // Handle cancellation case
    void set_stopped() {
        std::cout << "Operation was cancelled" << std::endl;
    }
};

// Usage example
int main() {
    auto receiver = SimpleReceiver{};
    
    // Connect sender and receiver
    auto operation_state = std::execution::connect(sender, receiver);
    
    // Start the operation
    std::execution::start(operation_state);
    
    return 0;
}
```

#### Example 2: Chained Operations and Transformations

```cpp
#include <iostream>
#include <execution>
#include <this_thread>
#include <string>

int main() {
    // Create a chain of operations
    auto sender = std::execution::just(10)
        | std::execution::then([](int x) {
            return x * 2;  // Multiply 10 by 2 to get 20
        })
        | std::execution::then([](int x) {
            return std::to_string(x);  // Convert to string "20"
        })
        | std::execution::then([](std::string s) {
            return "Result: " + s;  // Add prefix
        });
    
    // Execute using sync_wait and get the result
    auto result = std::this_thread::sync_wait(std::move(sender));
    
    if (result) {
        std::cout << "Final result: " << std::get<0>(*result) << std::endl;
        // Output: Final result: Result: 20
    }
    
    return 0;
}
```

#### Example 3: Using Scheduler in Different Execution Contexts

```cpp
#include <iostream>
#include <execution>
#include <this_thread>

int main() {
    // Get system scheduler
    auto scheduler = std::execution::get_system_scheduler();
    
    // Create an operation on a specific scheduler
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

### Key Differences

1. **Design Philosophy**
   - **Actor Model**: Based on "active" entities, where each Actor is an independent execution unit that actively processes messages
   - **Sender/Receiver Model**: Based on "passive" data flow, where data flows through pipes and is processed by receivers

2. **State Management**
   - **Actor Model**: Each Actor owns and manages its own state
   - **Sender/Receiver Model**: Typically stateless, with data passed through pipelines

3. **Communication Mechanism**
   - **Actor Model**: Actors actively send messages to each other through asynchronous message passing
   - **Sender/Receiver Model**: Data flows through connected sender-receiver pipelines, moving from source to destination

4. **Execution Model**
   - **Actor Model**: Each Actor executes independently with its own execution context, capable of long-running operations
   - **Sender/Receiver Model**: Data-driven, receivers are invoked when data is available, typically short-lived operations

5. **Error Handling**
   - **Actor Model**: Errors are handled internally by Actors or forwarded to dedicated error-handling Actors
   - **Sender/Receiver Model**: Uses dedicated `set_error()` channel for error propagation, with chainable error handling

### Similarities

1. **Asynchronicity**: Both support asynchronous programming patterns
2. **Composability**: Both can compose complex operations from simpler components
3. **Decoupling**: Producer and consumer components are decoupled

### Application Scenarios Comparison

#### Typical Actor Model Use Case

**Chat System User Session Management**:
```rust
// Each user session is an Actor maintaining its own state
struct UserSessionActor {
    user_id: String,
    connection_status: ConnectionStatus,
    message_history: Vec<Message>,
    last_activity: SystemTime,
}

#[async_trait]
impl Notifiable<IncomingMessage> for UserSessionActor {
    async fn notify(&mut self, msg: IncomingMessage, _: &Context<Self>) {
        // Update user state
        self.last_activity = SystemTime::now();
        self.message_history.push(msg.content.clone());
        
        // Process message and send response
        match msg.content {
            MessageContent::Chat(text) => {
                // Broadcast to other users
                self.broadcast_message(&text).await;
            }
            MessageContent::StatusUpdate(status) => {
                self.connection_status = status;
                // Notify friends of status change
                self.notify_friends().await;
            }
        }
    }
}
```

**Characteristics**:
- Long-lived entities maintaining session state
- Actively responds to various message types
- Handles complex conversation logic and state transitions

#### Typical Sender/Receiver Use Case

**Sensor Data Processing Pipeline**:
```cpp
// Processing sensor data pipeline
auto sensor_data = sensor_stream();  // Data source

// Transform stage 1: Data cleaning
auto cleaned_data = then(sensor_data, [](SensorReading reading) {
    return clean_sensor_data(reading);
});

// Transform stage 2: Data analysis
auto analyzed_data = then(cleaned_data, [](CleanedData data) {
    return analyze_data(data);
});

// Transform stage 3: Result aggregation
auto final_result = then(analyzed_data, [](AnalysisResult result) {
    return aggregate_results(result);
});

// Start the entire pipeline
sync_wait(final_result);
```

**Characteristics**:
- Data flows through multiple transformation stages
- Each stage is typically stateless pure function
- Operations are short-lived, completing once data is processed

### When to Choose Which Model

- **Choose Actor Model when**:
  - You need to maintain long-lived state
  - Entities need to actively respond to multiple message types
  - Complex state transitions and session management is required
  - Building GUI applications, game engines, or long-running services

- **Choose Sender/Receiver Model when**:
  - You need to build data processing pipelines
  - Operations can be expressed as a series of transformations
  - Tasks are stateless, processing independent data items
  - Efficient data stream processing is needed, such as ETL pipelines or image processing

Although both are important tools in modern concurrent programming, they solve different problems and are suitable for different scenarios.

## Related Resources

### Actor Model
- [Actor Model Wikipedia](https://en.wikipedia.org/wiki/Actor_model)
- [Actix Documentation](https://actix.rs/)
- [Erlang Actor Model](https://www.erlang.org/doc/reference_manual/processes.html)

### C++26 Sender/Receiver
- [P2300R0: std::execution](https://wg21.link/P2300)
- [Lewis Baker's Blog on Coroutines](https://lewissbaker.github.io/)
- [How to Use the Sender/Receiver Framework in C++ to Create a Simple HTTP Server](https://www.youtube.com/watch?v=Nnwanj5Ocrw)

## Summary

Rinf adopts the Actor Model because it is well-suited for GUI application state management. By distributing application state across multiple independent Actors, safe and efficient concurrent processing can be achieved while maintaining clear and maintainable code.