# 认证体系与 V2 协议

## 背景与目标

pb-mapper 的注册、订阅、状态与管理流量共用一个公网端口。0.4 版本不改变这一
连接模型，也不引入类似 TLS 的额外握手，而是在原有对称密钥体系内增加两级权限：

- 一把 32 字节管理员密钥拥有中继的全部权限；
- 可续期、可提前吊销、自动过期的 `pbmt1_` 临时凭据只能查看、注册和连接自己的
  命名空间；
- V2 第一个加密帧同时完成鉴权与请求传输，不增加一次网络往返；
- 过期、吊销与根密钥轮换会主动关闭受影响的控制连接和数据连接。

V2 是预共享密钥协议。当系统还需要证书身份、公开信任链或对流量分析的额外防护时，
TLS 仍然有独立价值，V2 不替代公钥 PKI。

## 核心模型

| 概念 | 含义 |
| --- | --- |
| 管理员密钥 | 唯一的 32 字节根凭据，可管理密钥并查看全部命名空间。 |
| 临时凭据 | `pbmt1_...` 字符串，携带 key ID 与派生后的 32 字节 secret。 |
| Key ID | 64 位 `generation:u32 | slot:u32`，用于直接定位固定槽位。 |
| 命名空间 | 管理员默认为 `0`；临时凭据属于签发时指定的命名空间，未指定时就是自己的 key ID。 |
| 凭据租约 | 同一凭据认证出的连接共同观察的取消对象。 |

临时 secret 由管理员密钥、持久化 server instance ID 与 key ID 通过
HKDF-SHA256 派生。服务端固定槽位只存生命周期元数据与弱引用，不存临时 secret。

## 端到端数据流

```mermaid
sequenceDiagram
    participant C as register/connect/admin CLI
    participant R as pb-mapper relay
    participant A as auth actor
    participant M as connection manager

    C->>R: V2 前缀 + 加密后的首个请求
    R->>R: 派生双向密钥并验证加密帧
    R->>A: 校验 key ID、generation、状态与过期时间
    A-->>R: 命名空间 + 凭据租约弱引用
    alt 管理操作
        R->>A: 签发/续期/吊销/状态查询
        A-->>R: WAL fsync 后的结果
    else 业务操作
        R->>M: 命名空间内的注册/订阅/查询
        M-->>R: 结果或稳定错误码
    end
    R-->>C: 同一连接上的加密响应
```

register 进程长期保持的控制连接只在建立时认证一次。后续每次业务请求拉起的新数据
TCP 连接仍有自己的 V2 首帧，但不会在其上再做多轮鉴权交换。

## V2 帧结构

### 首帧前缀

新客户端先写入 32 字节明文路由前缀：

| 字节数 | 字段 |
| ---: | --- |
| 4 | Magic `PBM2` |
| 1 | 版本 `2` |
| 1 | Flags，当前必须为 `0` |
| 2 | Reserved，当前必须为 `0` |
| 8 | 大端 key ID；`0` 表示管理员 |
| 16 | connection salt：8 字节 Unix 时间戳 + 8 字节随机数 |

前缀不承担保密作用，但会作为每个加密帧的 AAD 被完整认证。未知版本、flags、
reserved 值或超出五分钟时钟偏差窗口的时间戳会在请求分发前被拒绝。未认证的首个
加密请求上限为 64 KiB；鉴权完成后的后续帧仍沿用正常协议上限。

### 双向密钥与计数器

HKDF-SHA256 使用 connection salt 作为 salt，凭据的 32 字节 secret 作为 IKM，
分别以 `pb-mapper-v2-c2s` 与 `pb-mapper-v2-s2c` 派生两个 AES-256-GCM 密钥。
因此两个方向都从计数器 0 开始，也不会重复使用同一密钥与 nonce 组合。

每个加密帧由 8 字节大端计数器、4 字节密文长度、密文与 16 字节 GCM tag 组成。
96 位 nonce 是四个零字节加 64 位计数器。AAD 包含完整首帧前缀、方向字节、计数器
与密文长度。计数器不连续、认证失败、帧过大或计数器耗尽都会关闭连接。

首个请求使用 C2S counter 0，首个响应使用 S2C counter 0；后续控制消息由同一组
有状态 reader/writer 从 counter 1 继续。

### 重放检测

服务端对 `(key_id, connection_salt)` 做指纹，并在同一个临界区内完成两个轮换的
1 MiB Bloom filter 的检查与写入，覆盖当前与上一个 600 秒窗口，使首帧允许的
最大未来时间戳无法在过滤器遗忘后继续重放。疑似重复是
`connection_salt_replayed`。中继不会用已经用过的首响 nonce 加密该错误，所以
对端可能看到解密失败/EOF 而不是可读错误帧。一次性 admin CLI 只在读到该错误、
或请求尚未写出时换 salt 重试一次，不会在响应丢失后再发，以免 `key issue`
这类非幂等命令被执行两次。
会修改状态的管理员请求还会在分发前把精确指纹写入加密 WAL；该记录自服务端接受起
在十分钟内跨重启、跨 compact 保留，不能通过等待 Bloom 窗口结束或重启进程来重放
旧操作。客户端首帧时间戳仍用于新鲜度检查，但不决定这条记录保留多久。

中继还会把每个被接纳的首帧追加到认证状态目录下的 `connection.replay`，启动时重新
载入其中最近十分钟的记录，所以重启不会让重放窗口重新打开。每个中继有一个写入线程，
把同时到达的首帧用一次写入、一次同步追加进去，同步完成后才接纳其中每一个。追加到一半
时崩溃会让日志的最后一条记录被截断。那次接纳从未被确认，所以中继启动时丢弃它，在 warn
级别记 `replay_log_cut_off_record_dropped`，给出文件和丢弃的字节数，并把日志重写为
完整记录。除此之外这份日志 fail closed：启动时读不出来；以截断记录结尾，而其中的记录
给出的时间比中继时钟超前十分钟以上（最后一条之前就已损坏的日志、不是重放日志的文件，
或时钟回拨超过十分钟之前写下的日志）；或追加失败后无法回滚、新建文件后无法同步目录、
compact 失败、写入线程无法启动时，中继会以 `connection_replay_store_unavailable` 拒绝
所有 protocol-v2 首帧，直到它带着一份可读的日志重启。原因只记一次，在 error 级别：
事件 `replay_log_unavailable` 给出文件、失败的步骤和操作系统错误（回滚失败时，还给出
它要撤销的那次追加错误）。删除这样的 `connection.replay` 后重启即可恢复接纳，代价是
忘掉最近十分钟内的首帧。追加失败但日志保持原样时（例如磁盘已满，或日志所在目录无法
创建或打开），拒绝这次追加携带的那些首帧，同样返回 `connection_replay_store_unavailable`，
下一次追加会再试；追加持续失败期间，中继在 error 级别只记一次
`replay_log_append_failed`，给出文件、步骤和错误，直到某次追加成功之后再次失败才会再记。

根密钥轮换或认证状态重置之后，进程会在内存里保留上一份根密钥和 instance id。
仍用旧临时凭据加密的首帧会用这份材料解密，从而把稳定的
`temporary_key_rotated` 错误返回给客户端，而不是解密失败。

## 临时凭据生命周期

### 签发与续期

签发会寻找空槽位、递增 generation、派生 secret、写入加密 WAL 并 `fsync`，随后才
把凭据返回给管理员。续期不换 key ID 与凭据文本，只把绝对过期时间设为从现在起的
给定 TTL（可以比原来早，也可以比原来晚）并把新版本任务放入时间轮，旧任务到期时因
版本不匹配而被忽略。缩短后的 key 会在新的截止时间像普通过期一样结束，该截止时间
在重启后依然有效。调用方丢失凭据文本时也可以靠续期找回；`key reveal` 返回相同文本
且不改变过期时间。

```bash
export MSG_HEADER_KEY="$(sudo cat /var/lib/pb-mapper/auth/admin.key)"
pb-mapper admin --server relay.example.com:7666 \
  key issue --ttl 24h --label home-web
pb-mapper admin --server relay.example.com:7666 \
  key renew 4294967296 --ttl 7d
```

默认最短 TTL 为 10 秒，最长为 30 天，服务端可调整最大值。最大 TTL 不在 10 秒到 365 天
之间，或 key 容量不在 1 到 1,048,576 之间时，中继拒绝启动，在动状态目录之前返回
`auth_config_invalid`：CLI 参数和环境变量本就限定在这些范围内，SDK 程序自己构造的
`AuthConfig` 也做同样的检查。

### 多个 key 共用的命名空间

默认情况下，一个 key 拥有以自身 key ID 命名的命名空间，持有该 key 的所有设备共用它。
如果希望一个租户每台设备一个 key，移除设备时只吊销一个 key、其他设备不受影响，就先
分配命名空间，再向其中签发：

```bash
pb-mapper admin --server relay.example.com:7666 namespace allocate
pb-mapper admin --server relay.example.com:7666 \
  key issue --ttl 24h --label laptop --namespace 1
```

同一命名空间内的 key 能看到相同的服务，并各自独立地吊销、续期和过期。命名空间比其中
的 key 活得更久，之后签发的 key 仍会看到相同的服务。只有管理员可以分配和签发；向分配器
没有发放过的命名空间签发会返回 `namespace_not_allocated`，命名空间 `0` 和任何 key 自身
的命名空间都属于这种情况。

可分配的命名空间是 `1` 到 `2^32 - 1`，即 key ID 布局中的 generation `0`。任何 key 的
generation 都不会是 `0`，因此两类命名空间不会冲突。每个编号只发放一次：高水位先写入并
落盘，然后才返回编号，崩溃或响应丢失最多浪费一个编号，绝不会重复。高水位保存两份，一份在
单独的文件中，一份在加密状态中，所以根密钥轮换、认证状态重置、从损坏的 snapshot 执行重置以及
丢失该文件都不会影响它，调用方可以在这些情况之后继续使用已保存的命名空间。只有两份都丢失才会
丢失高水位，实际上就是丢失状态目录。此后向已保存的编号签发会返回
`namespace_not_allocated`，直到分配器再次越过该编号，并且分配器可能重复发放旧编号。调用方应将
该错误视为中继丢失了状态，并在自己一侧保证编号唯一。

具备上述能力的中继会在 `admin status` 的 `capabilities` 中列出 `key_namespaces`，SDK 的
`Admin::supports_key_namespaces` 读取的就是它。旧版中继不报告任何能力，并以
`request_decode_failed` 拒绝这两个请求，不产生副作用。

未指定命名空间签发的 key 的存储格式与之前完全相同。把中继回退到不支持命名空间的版本时，
状态仍然可读，但该版本会忽略 key 的命名空间，这类 key 会落到自身 key ID 对应的命名空间：
仍然隔离，但不再与同伴共享服务。它的第一次 compact（五分钟内）会把不含命名空间的状态写回，
此后再升级也无法恢复：这些 key 必须重新签发。

### 到期、吊销与 GC

四层层级时间轮持有每个活动凭据租约的强 `Arc`；前台认证状态只持有 `Weak`。
到期或管理员吊销会取消租约，相关控制任务和数据转发任务立即释放 TCP 连接。短暂保留
tombstone 以给出稳定错误后，槽位可以复用。显式 `key gc` 可立即清理非活动槽位。

### 根密钥轮换与状态重置

根密钥轮换先用新密钥写空 snapshot，同时保留有上限的审计历史，持久化
`admin.key`，再把密钥与管理员 lease 作为一次状态变更切换。这是全局作废：所有临时
凭据都会立刻失效，包括其他命名空间里尚未过期的 key，并用这些 key 建立的活动连接
会被取消。之后如果 first flight 仍能用上一轮根密钥解密，会得到客户端可读的
`temporary_key_rotated`。无法解密的 first flight（输错、外站或损坏的凭据）则是
`protocol_v2_decrypt_failed`，且不会附带加密错误帧，因为中继推导不出对端能打开
的 session。`temporary_key_invalid` 只用于派生之后材料仍不匹配的进程内校验。
CLI 在发请求前保存候选 key，完成后再用
新 key 执行一次 `admin status` 验证。未指定 `--key-file` 时，恢复副本默认写到
`$XDG_CONFIG_HOME/pb-mapper`（或 `$HOME/.config/pb-mapper`；Windows 上没有 `HOME`
时为 `%USERPROFILE%\.config\pb-mapper`），不要求本机能写 `/var/lib`。这些变量都
没有设置、或只有相对路径时，命令在联系中继之前就拒绝执行，并要求指定 `--key-file`。

`auth-state reset --confirm` 同样会清空临时凭据，并轮换 server instance ID。这样即使
原槽位表损坏或丢失，旧凭据也不会因为未来复用了相同 key ID 而重新有效。

## 命名空间与权限边界

临时凭据只能在自己的命名空间（签发时指定的命名空间，或其 key ID 对应的命名空间）
执行 `register`、`connect` 与 `status`。它不能签发或
查看其他 key、进入其他命名空间、修改 legacy 策略、重置状态或轮换管理员密钥。

管理员默认使用命名空间 0；通过 `--namespace <namespace>` 可以查看或连接临时命名空间。
管理员要在临时命名空间内注册服务时还必须显式使用 `--force`，避免误把业务服务挂到
错误租户。

临时凭据的 service name 限制为 1 到 128 个 ASCII 字节，字符集为
`[A-Za-z0-9._:-]`。服务端分别按命名空间限制 service 数、单 service 注册连接数、
活动 stream 数与新建 stream 速率。

| 方案 | 内存与查询 | 提前吊销 | 命名空间隔离 | 网络成本 |
| --- | --- | --- | --- | --- |
| 无状态签名 token | 服务端状态少 | 仍需 deny list | 依赖 token claim | 一个请求 |
| 通用 HashMap | 动态分配与哈希 | 直接删除 | 直接 | 一个请求 |
| 固定槽位 + 派生 secret | 固定热内存、O(1) 查找 | 直接取消槽位租约 | key ID 或已分配的 namespace | 一个请求 |

当前方案明确接受有上限的服务端状态，以换取确定的提前吊销与活动连接硬关闭。

吊销在 key 要开始新操作的地方生效：首帧认证（覆盖 TCP 与 QUIC 上的 `register`、订阅和
stream 连接）以及直连协调。中继转发的 stream 在任一端的 key 失效时关闭。直连建立有所
不同，因为提交发生在两个端点之间。key 被吊销后，中继不再为它发起或接续建立过程，并取消
其作为消费方进行中的建立过程。端点已持有的 grant 只对那一个会话有效，到其建立截止时间
为止（最多 8 秒），此后不再授权任何东西。已建立的直连会话属于端点，不受吊销影响；要关闭它
需要停止持有它的端点。

## 持久化与安全模式

Linux 系统服务默认目录是 `/var/lib/pb-mapper/auth`，权限为 `0700`。无特权
Linux、macOS 与 Windows 桌面二进制默认写到用户可写的应用目录，而不是
`/var/lib`：

| 文件 | 用途 |
| --- | --- |
| `admin.key` | 根凭据，权限 `0600` |
| `admin.key.next` | 轮换进行中的暂存根密钥 |
| `server-instance-id` | 16 字节持久派生身份 |
| `server-instance-id.next` | reset 进行中的暂存实例 ID |
| `namespace-allocator` | 已发放的最后一个命名空间（十进制文本）。不是机密，reset 与根密钥轮换都不会替换它。加密状态中保存第二份 |
| `auth.snapshot` | AES-256-GCM 加密的紧凑槽位快照 |
| `auth.wal` | 带长度前缀、逐条加密的 mutation 与 audit |

变更只有在 WAL 同步成功后才对外确认。每五分钟，如果自上次 compact 以来 snapshot
所含内容有变化（一次 WAL 追加、key 到期或被移除、管理员重放声明离开窗口），或中继
在此期间启动过，后台 actor 就原子替换 snapshot 并截断 WAL；空闲的状态不写任何东西。
snapshot 同时保存有上限的审计历史和仍有效的管理员重放声明，compact 不会丢弃这些安全
记录。actor 把每条命令和每次 compact 放到异步运行时的 blocking 线程池上，按到达顺序
一次一个地执行，启动时的文件操作也在那里完成，所以它们的写入和同步不会拖住中继运行时
上的其他任务。无效文件头、完整性验证失败、WAL 截断、schema 不匹配或 compact 失败
都会进入 safe mode：临时凭据全部 fail closed，管理员仍可查看状态并执行显式 reset。
在 Windows 以外，WAL 追加若无法把 `auth.wal` 的权限设为 `0600`，则什么也不写，以
`temporary_key_store_unavailable` 拒绝，不进入 safe mode。
`namespace-allocator` 损坏的影响范围更小：分配命名空间和向命名空间签发会以
`namespace_allocator_unavailable` 失败，已有 key 和普通签发不受影响。删除该文件即可解除，这表示
让中继继续使用加密状态中的那一份。如果中继同时处于 safe mode，那一份也已丢失：应从备份恢复该文件
而不是删除它，否则编号可能重复。

Flutter 启动服务端时使用应用配置目录下的 `auth/` 子目录，并且只有 TCP listener 与
认证状态都初始化成功后才会报告 running；桌面和移动端无需写 `/var/lib`，初始化失败
时也不会出现虚假的运行状态。

## 管理命令与输出

```bash
pb-mapper admin --server relay.example.com:7666 status
pb-mapper admin --server relay.example.com:7666 key list --page-size 100
pb-mapper admin --server relay.example.com:7666 key show 4294967296
pb-mapper admin --server relay.example.com:7666 key reveal 4294967296
pb-mapper admin --server relay.example.com:7666 service list --namespace 4294967296
pb-mapper admin --server relay.example.com:7666 connection list --all
pb-mapper admin --server relay.example.com:7666 connection retire echo --all
pb-mapper admin --server relay.example.com:7666 legacy-protocol set deny
pb-mapper admin --server relay.example.com:7666 auth-state reset --confirm
pb-mapper admin --server relay.example.com:7666 root-key rotate
```

`--output human|json|ndjson` 控制展示格式。默认每页 100，最大 1000；`--all` 自动翻完
所有页面并保留选定的输出格式。大列表应选择 NDJSON 流式输出；JSON 输出单个合并文档，
human 输出单个合并表格。稳定错误结构包含 `code`、`message`、
`retryable` 与 `server_time`。

日志记录 auth stage、key ID、peer 与 reason，但不记录凭据。相同
`(peer IP, key ID, reason)` 每分钟最多直接输出 5 次，下一窗口汇总被抑制的数量。

## 迁移与兼容性

新客户端固定发送 V2。0.4 服务端默认暂时接受旧帧，方便滚动升级；确认
`active_legacy_connections` 归零后，可执行 `legacy-protocol set deny`。必须先升级中继、
再升级客户端，因为 0.3 中继无法识别 V2 首帧 magic。显式配置的
`PB_MAPPER_LEGACY_PROTOCOL` 会先去除首尾空白，并且只能是 `allow` 或 `deny`；
无效值会 fail closed 为 `deny`。

新安装会随机生成管理员密钥。中继自身与安装脚本在未配置新 key 或环境变量时，如果
发现旧的 `/var/lib/pb-mapper-server/msg_header_key`，会将其复制到新路径，保留现有
业务连接。`--use-machine-msg-header-key` 只作为明确的兼容选项继续存在。

Docker 必须持久化 `/var/lib/pb-mapper/auth`；否则重建容器会产生新管理员密钥，并且
无法读取先前认证状态。

## 运维排障

### 续期后临时凭据仍被拒绝

1. 执行 `admin status`，确认 `safe_mode=false`。
2. 执行 `key show <id>`，确认状态为 active 并核对绝对过期时间。
3. 从结构化日志区分 generation 不匹配、已过期与 V2 解密失败。
4. 如果只是凭据文本复制错误，执行 `key reveal <id>` 重新配置；续期本身不会换凭据。

### 服务端进入 safe mode

1. 先完整保留 auth 目录用于诊断。
2. 确认 key、instance ID、snapshot 与 WAL 是否来自同一份服务器状态。
3. 使用管理员 key 查询 `admin status`；管理员通道仍然可用。
4. 无法恢复时执行 `auth-state reset --confirm`，再重新签发业务凭据。该操作会轮换
   instance ID，并断开旧业务。

### 禁用 legacy 后仍有旧客户端

1. 在 `admin status` 查看 legacy policy、当前连接数与最后连接时间。
2. 如果业务尚未升级，可短暂改回 `allow`，但应尽快升级客户端。
3. 新客户端的服务端日志应显示协议 `V2`；仍增长的 legacy 计数可以定位旧 binary。

## 代码索引

- 凭据格式与进程配置：`crates/pb-mapper-core/src/checksum.rs` 与
  `crates/pb-mapper-core/src/checksum/`
- 共享模型：`crates/pb-mapper-auth/src/lib.rs`（常量与模块总览）、`types.rs`、
  `lease.rs`、`state.rs`，以及收录全部失败码的 `codes.rs`
- 生命周期 actor：`crates/pb-mapper-auth/src/actor/`
- 持久化：`crates/pb-mapper-auth/src/persistence/`
- runtime facade、租约调度与时间轮：`crates/pb-mapper-auth/src/runtime.rs`、
  `leases.rs`、`timing_wheel.rs`
- V2 session facade、frame、限流与 replay 模块：
  `crates/pb-mapper-protocol/src/secure.rs` 与
  `crates/pb-mapper-protocol/src/secure/`
- 中继状态、runtime loop 与连接分发：`crates/pb-mapper-server/src/routing.rs`、
  `runtime/`、`connection.rs`
- 管理请求执行：`crates/pb-mapper-server/src/admin.rs`
- 统一 CLI、中继的认证参数与管理员命令模块：
  `crates/pb-mapper-cli/src/bin/pb-mapper.rs`、
  `crates/pb-mapper-cli/src/bin/pb-mapper/server.rs`、
  `crates/pb-mapper-cli/src/bin/pb-mapper/admin.rs`

## 总结

0.4 在保持单端口与长控制连接模型的同时，把根管理权限与业务访问权限拆开。临时凭据
可续期、可吊销、按命名空间隔离，鉴权仍然包含在第一个业务请求内。需要明确保留的
边界是：V2 解决预共享密钥下的帧认证与权限控制，证书身份仍属于 TLS 层。
