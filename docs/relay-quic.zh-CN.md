# 可选 QUIC 中继

中继传输默认仍为 TCP。注册端、连接端可以分别选择 QUIC，同一服务允许 TCP 与 QUIC 客户端混用。业务的 `Transport::Tcp/Udp`、数据加密 `codec` 和直连策略 `Auto/RelayOnly` 与中继传输独立。

## 启用

以下命令假设已经按认证文档配置 `MSG_HEADER_KEY` 或密钥文件：

```sh
pb-mapper server --port 7666 --quic-listen 0.0.0.0:7667
pb-mapper register tcp --server relay.example.com:7666 --relay-transport quic --key web --addr 127.0.0.1:8080
pb-mapper connect tcp --server relay.example.com:7666 --relay-transport quic --key web --addr 127.0.0.1:9090
```

保留 TCP 7666，并开放 UDP 7667。直连协调默认仍使用 UDP 7666；QUIC 中继必须使用另一个 UDP 端口。如果 NAT 对外映射的 QUIC 端口不同，增加 `--quic-advertise-port PUBLIC_PORT`。公布的 IP 使用客户端实际连接的中继 IP，因此 QUIC 与 TCP 必须由同一公网 IP 提供。

`--relay-only` 禁用端点间直连，不禁用显式配置的 QUIC 中继。测试强制中继时可在注册端、连接端添加该选项。旧命令和旧节点继续使用 TCP；显式选择 QUIC 后，服务端未启用 QUIC、证书不符或 UDP 不通都会报告错误，不会悄悄改用 TCP。恢复 TCP 使用 `--relay-transport tcp`。

当前 QUIC 建连先用一次经过 PBM2 认证的 TCP 请求获取 UDP 端口和证书，再建立 QUIC；**TCP 端口仍是首次连接和重连的必要条件**。注册控制、订阅、数据流、SDK 状态和管理请求随后走 QUIC。

## Rust SDK

```rust,ignore
use pb_mapper::{Client, ClientConfig, RelayTransport};

let client = Client::new(ClientConfig {
    server: "relay.example.com:7666".into(),
    credential: credential_string,
    keep_alive: true,
    namespace: None,
})?.with_relay_transport(RelayTransport::Quic)?;

// 原有 register/connect/register_with_options/connect_with_options 接口不变。
// 隧道句柄的 diagnostics().relay_transport 标明中继传输选择。
let snapshot = client.relay_diagnostics().await;
println!("{}", serde_json::to_string(&snapshot)?);
```

`with_relay_transport` 返回一个配置好的新 Client；从新 Client 创建的注册、连接、状态查询与 admin 使用该传输。已有 Client/隧道保留原配置。`clone()` 共享同一连接池和凭据状态；切换传输会复制当前凭据，独立维护后续凭据变更。原有 `ClientConfig`、`RegisterRequest`、`ConnectRequest` 结构体字面量无需新增字段。

管理员请求保持每次调用重新解析地址及原有 `Error::Address` 错误类型；QUIC 仅复用与本次解析结果匹配的连接。已经发送的管理操作不会因传输失败而自动重放。

SDK facade 默认编译 `p2p` 和 `relay-quic`，但中继默认值仍为 TCP。仅需 TCP 可设置 `default-features = false`；只需 QUIC 中继可加 `features = ["relay-quic"]`。使用底层 `pb-mapper-client` / `pb-mapper-server` crate 时显式启用 `relay-quic`。未编译此 feature 时，选择 QUIC 返回配置错误。

嵌入式服务端使用 `run_server_on_listener_with_options` 和 `RelayServerOptions { direct, quic: Some(QuicServerConfig { listen, advertise_port: 0 }) }`。已有服务端启动 API 保持 TCP 行为。

## Node SDK

```ts
const client = new Client({
  server: "relay.example.com:7666",
  credential: process.env.MSG_HEADER_KEY!,
  relayTransport: "quic", // 省略时为 "tcp"
});
const registration = await client.register({
  key: "web", localAddr: "127.0.0.1:8080", transport: "tcp",
});
await registration.waitReady();
console.log(registration.diagnostics().relayTransport);
console.log(await client.relayDiagnostics());
```

## 可观测性

`relay_diagnostics()` / `relayDiagnostics()` 按需读取 Quinn 统计，不启动统计任务。字段如下（Node 为 camelCase）：

| 字段 | 含义 |
| --- | --- |
| `transport` | 配置的中继传输；不是当前业务流的 direct/relay 路径 |
| `connections_created` | 该池成功建立的 QUIC 连接累计数 |
| `streams_opened` / `streams_reused` | 打开的业务/控制流累计数，以及其中复用已有连接的流数 |
| `last_error` | 最近一次连接建立或开流失败；下次成功清除，不代表所有业务状态 |
| `connections` | 池中当前未关闭的 QUIC 连接；流持有的已移出池的连接不包含在内 |
| `connection_id`, `peer_addr` | 本进程连接标识、实际 UDP 对端 |
| `rtt_us`, `congestion_window`, `mtu` | RTT 微秒、拥塞窗口字节、当前路径 MTU 字节 |
| `sent_packets`, `lost_packets`, `congestion_events` | Quinn 路径累计包数、判定丢失包数、拥塞事件 |
| `sent_bytes`, `received_bytes` | UDP 累计字节，含协议开销与重传，不是业务净流量 |

TCP 返回空连接列表和零 QUIC 计数，避免给默认路径添加计数开销。JS 数值使用 Number；极大累计值受其整数精度限制。隧道 `diagnostics()` 原有恢复、事件、direct/relay 状态继续可用。CLI 运行日志每 30 秒输出隧道与中继快照；服务端还记录 `relay_quic_listening`、`relay_quic_authenticated`，客户端记录 `relay_quic_connected`。

## 实现与边界

- 复用仓库已有 Quinn/rustls 和 rcgen 依赖；借鉴 proxy-everything 的连接复用及传输适配方式，保持 pb-mapper 自己的协议、权限和路由管理。
- 每个客户端池按凭据、Tokio runtime、网络代次及中继 TCP 地址隔离连接，最多缓存 16 个连接，满时淘汰最早缓存项。控制失败或 SDK 请求超时后，后续请求重新建连，避免等待旧 QUIC 连接的 60 秒空闲超时；不会重放已经发送的管理操作。淘汰、网络变化及恢复只影响后续池选择，已有流独立持有连接和 UDP 驱动。
- 冷连接建立与统计缓存使用独立锁；网络握手或流额度等待不阻塞诊断查询，也不阻塞其他已缓存连接的开流。单次 QUIC 建连（包括 TCP 发现）总预算 5 秒，多地址竞争使用同一候选预算；TCP 的原有 2 秒多地址候选预算保持不变。
- 一条已认证 QUIC 连接可承载多个双向流。每个流重用原 PBM2 鉴权、命名空间、注册、订阅及转发流程。没有应用数据重试/重放；中继两侧可以采用不同传输。
- TLS 1.3、独立 ALPN、精确证书固定、首次 PBM2 凭据绑定 TLS exporter；禁用 0-RTT、单向流和 QUIC DATAGRAM。UDP 业务仍使用现有有序数据报封装。
- 服务端最多 64 个并行握手、128 个已认证 QUIC 连接，每连接最多 1024 个双向流；认证前仅开放 1 个流。握手超时 5 秒，连接接收窗口 32 MiB，单流窗口 8 MiB，空闲超时 60 秒，客户端每 15 秒保活。窗口表示流量控制额度，不是预分配内存。
- 撤销凭据立即取消原有授权流并停止接收新流；连接最多保留 3 秒关闭宽限，以便已提交的管理员密钥轮换响应完成发送。直连会话仍由端点持有，不受中继凭据撤销影响。
- TCP/QUIC 在转发循环外分派，循环单态化；QUIC 大变体装箱，默认 TCP 不创建连接池。未启用服务端 QUIC 时不创建 UDP endpoint。逐块活动通知改为任务内记录，不增加逐包任务、排队或复制。
- 加密 TCP 数据流只把完整帧之间的 EOF 识别为正常半关闭；截断帧仍报错，控制消息与 UDP 帧仍严格读取。这保证混合传输时反向尾部数据能够排空。

## 复现验证

```sh
cargo test -p pb-mapper-cli --all-features --test quic_relay --test direct_e2e
cargo run -p pb-mapper-cli --release --example relay_transport_bench
```

`quic_relay` 覆盖并行多流复用、单流关闭不影响同连接其他流、混合传输加密转发、UDP 数据报、明确拒绝降级、证书与 exporter 校验、凭据撤销、管理员密钥轮换、中继重启、旧 UDP 路径永久黑洞后的恢复，以及发现握手停滞时诊断仍能返回。`direct_e2e` 包含经 QUIC 中继协调的直连建立和协调器丢失测试。`sdk_e2e` 使用原有配置/请求字面量，验证启用 QUIC 的服务端不会改变 SDK 默认 TCP 行为；CLI 与 Node 测试也验证默认值。

基准使用两个独立客户端、一个内嵌中继及 echo 后端，先预热，再校验 64 MiB 回传和 200 次分段写入的 8 字节 RPC；不计入建连耗时。回环结果不能预测公网丢包和多流并发下的性能。

### 本机测量（2026-10-26）

环境：Windows 11 Enterprise 10.0.26300，AMD Ryzen 9 9950X（16 核 / 32 线程），rustc 1.99.1 / LLVM 23.1.1，x86_64-pc-windows-msvc，仓库 `release` profile，本地回环、同进程、无模拟网络损伤。基线为拉取后的 `6f7281f`，对比此次改动的构建。

原有 `test_delay::large_history_then_repeated_small_exchanges` 使用单线程 Tokio test runtime，校验 20 MiB 回传后测量 200 次小 RPC。最初单次基线 p50 为未加密 145.2 µs / 加密 262.6 µs，改动后单次为 200.4 µs / 392.5 µs；为排查这次差异，随后交替运行保留的旧、新可执行文件各三次，并重复一次固定到同一逻辑 CPU（affinity `0x100`）的三组测量：

| 条件 | codec | 基线 p50 范围（µs） | 改动后 p50 范围（µs） | 基线 p95 范围（µs） | 改动后 p95 范围（µs） |
| --- | --- | --- | --- | --- | --- |
| 默认调度 | false | 147.1–174.3 | 135.8–188.1 | 184.5–236.5 | 173.6–233.6 |
| 默认调度 | true | 292.0–451.5 | 277.0–420.0 | 389.2–748.6 | 338.5–573.6 |
| 固定 CPU | false | 145.5–199.8 | 133.1–193.8 | 202.8–251.8 | 138.0–254.0 |
| 固定 CPU | true | 292.1–430.6 | 269.4–411.7 | 324.4–461.0 | 330.4–461.5 |

结果存在明显运行间波动，没有得到稳定的 TCP 退化证据，也不足以证明加速或绝对零开销。这里的零成本设计指转发循环中的静态分派、默认 TCP 不分配 QUIC 池，以及移除逐块活动队列。

新增 `relay_transport_bench` 使用默认多线程 Tokio runtime（本机 32 workers），一次测量结果如下。吞吐按 `64 MiB / 完整回传耗时` 计算，不将双向字节翻倍：

| 中继 | codec | 64 MiB 回传（s） | 业务吞吐（MiB/s） | RPC p50（µs） | RPC p95（µs） |
| --- | --- | --- | --- | --- | --- |
| TCP | false | 6.878 | 9.3 | 463.2 | 850.8 |
| TCP | true | 2.506 | 25.5 | 651.0 | 811.8 |
| QUIC | false | 1.605 | 39.9 | 570.2 | 902.1 |
| QUIC | true | 1.751 | 36.5 | 584.2 | 1040.9 |

单次回环吞吐与延迟表现不同，且 TCP 加密/不加密结果也反映调度、缓冲及分帧的影响；不能据此声称 QUIC 在所有网络更快。仍需部署环境的 RTT、丢包和多流负载测量后再考虑修改默认值。
