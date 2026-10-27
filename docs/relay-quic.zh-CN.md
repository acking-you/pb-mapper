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

// 切换前先问：经 TCP 询问中继是否提供 QUIC，None 表示不提供。
let offered: Option<u16> = client.relay_quic_port().await?;
```

`with_relay_transport` 返回一个配置好的新 Client；从新 Client 创建的注册、连接、状态查询与 admin 使用该传输。已有 Client/隧道保留原配置。同一进程内，同一中继地址、同一传输的所有 Client 共享一个 endpoint：DNS 缓存、建连预算，以及 QUIC 连接池（池内按凭据区分连接）。因此一个进程的全部隧道经同一条 QUIC 连接复用，而不是每个 Client 各开一条。切换传输会复制当前凭据，独立维护后续凭据变更。原有 `ClientConfig`、`RegisterRequest`、`ConnectRequest` 结构体字面量无需新增字段。

中继未开启 QUIC（包括不认识该请求的旧版中继）时，QUIC 请求返回 `Error::Remote { code: QUIC_UNAVAILABLE, retryable: false, .. }`；隧道按永久拒绝结束，`wait_ready` 返回失败，而不是无限重试。但本进程曾经经 QUIC 连上过这个中继时，这多半是中继重启或回滚时没带上 QUIC：拒绝改为可重试（`retryable: true`），运行中的隧道按拒绝退避等待，QUIC 恢复后自行恢复。

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
console.log(client.relayTransport(), await client.relayQuicPort()); // "quic" 7667；不提供时为 null
```

## 图形界面

- **设置 › 中继服务器**：地址与凭据之下是「中继传输」，在 TCP 与 QUIC 间选择。下方一行说明中继上次的回答：提供 QUIC 时给出 UDP 端口；选了 QUIC 而中继不提供时用警示色说明隧道无法经 QUIC 连接它；尚未得到回答时说明 QUIC 的含义。保存后，只有在中继已经经由新传输回应时，才列出仍在旧传输上运行的隧道并询问是否重新启动以切换；选「稍后」则它们保持原传输，直到下次启动。配置串携带传输选择，旧版本会忽略该字段。
- **中继页**：头部在在线状态旁显示所用传输。使用 QUIC 时多出「往返时延」「丢包率」两项指标，并用一行小字给出连接数、流数与复用数、MTU、拥塞窗口及最近一次 QUIC 错误；这些来自本机连接，不向中继发请求。使用 TCP 而中继提供 QUIC 时，给出「改用 QUIC」。
- **本机中继**：可开启「提供 QUIC」并指定单独的 UDP 端口（默认 7667；不能与 TCP 端口相同，后者的 UDP 用于直连协调）。运行时显示 QUIC 端口；端口被占用在启动时就报错，而不是让中继任务在后台退出。
- **隧道**：经中继的路径标签带上传输，如「中继 · QUIC」「仅中继 · QUIC」；TCP 为默认，不额外标注。启动隧道前先经 TCP 向中继问一次 QUIC（一次带认证的请求，替代原先会被中继记为登录失败的裸连接）：连不上、凭据被永久拒绝，或选了 QUIC 而中继不提供，都直接报错；超时或可重试的拒绝交给隧道自己的重试。
- **FFI**：`pb_mapper_update_config` 增加 `relay_transport`（`tcp`/`quic`），在 `data.move` 中返回仍在旧传输上的隧道；`pb_mapper_start_server` 增加 `quic_port`（0 表示不开启）；`pb_mapper_relay_call_json` 增加 `link` 操作。窗口自己的状态检查与中继页请求都经所选传输发出，并另经 TCP 询问 QUIC 端口（`quicPort`、`quicKnown`）；这一询问只在首次检查、设置变更或点击 Test 时发出，周期检查沿用上次回答，因为旧版中继会把它记为一条警告。例外是选了 QUIC 而中继经 QUIC 完全无应答（不可达或超时）时：此时再经 TCP 问一次，若 TCP 能答且中继提供 QUIC，错误直接指出是哪个 UDP 端口不通（多为防火墙或安全组只放行了 TCP），而不是笼统地说中继无应答。延迟只计服务列表请求本身。窗口持有一个按中继设置缓存的 Client，使这些请求与隧道共用同一条 QUIC 连接。

## 可观测性

`relay_diagnostics()` / `relayDiagnostics()` 按需读取 Quinn 统计，不启动统计任务。字段如下（Node 为 camelCase）：

| 字段 | 含义 |
| --- | --- |
| `transport` | 配置的中继传输；不是当前业务流的 direct/relay 路径 |
| `connections_created` | 该池成功建立的 QUIC 连接累计数 |
| `streams_opened` / `streams_reused` | 打开的业务/控制流累计数，以及其中复用已有连接的流数 |
| `last_error` | 最近一次连接建立或开流失败；下次成功清除，不代表所有业务状态 |
| `connections` | 当前未关闭的 QUIC 连接：池中可分配的，以及已移出池但仍承载流的 |
| `connection_id`, `peer_addr` | 本进程连接标识、实际 UDP 对端 |
| `pooled` | 是否仍分配给新流；`false` 表示已退出连接池，只为已有流保持 |
| `rtt_us`, `congestion_window`, `mtu` | RTT 微秒、拥塞窗口字节、当前路径 MTU 字节 |
| `sent_packets`, `lost_packets`, `congestion_events` | Quinn 路径累计包数、判定丢失包数、拥塞事件 |
| `sent_bytes`, `received_bytes` | UDP 累计字节，含协议开销与重传，不是业务净流量 |

TCP 返回空连接列表和零 QUIC 计数，避免给默认路径添加计数开销。JS 数值使用 Number；极大累计值受其整数精度限制。隧道 `diagnostics()` 原有恢复、事件、direct/relay 状态继续可用。CLI 运行日志每 30 秒输出隧道与中继快照；服务端还记录 `relay_quic_listening`、`relay_quic_authenticated`，客户端记录 `relay_quic_connected`。

## 实现与边界

- 复用仓库已有 Quinn/rustls 和 rcgen 依赖；借鉴 proxy-everything 的连接复用及传输适配方式，保持 pb-mapper 自己的协议、权限和路由管理。
- 连接池在进程内按中继地址与传输共享，池内按凭据、Tokio runtime、网络代次及中继 TCP 地址区分连接；另一 runtime 的连接留给它自己，不会被互相驱逐。最多 16 个连接分配新流，满时最早的一个退役。一条连接上的流接近每连接 1024 的上限时另建一条，而不是排队等待流额度。
- 最后一个流结束 30 秒后仍没有新流的连接被关闭（每 10 秒检查一次），退役的连接也一样。连接池对它建立的每条连接都持有强引用，直到亲自关闭：Quinn 在最后一个句柄释放时立即关闭连接并丢弃尚未送达的数据，若由退役连接上最后一个流的释放来关闭，刚写完的响应尾部会丢失。排在同一次冷建连后面的调用者共享它的失败，不再各自重新等待 5 秒。客户端保活只为承载流的连接维持 NAT 映射；空闲连接不再一直占用中继的准入名额和 UDP 端口。
- 只有传输失败或超时使连接池失效，中继给出的拒绝不会；失效只退役 2 秒内没有收到中继任何数据包（包括确认）的连接，后续请求在新连接上进行，避免等待旧 QUIC 连接的 60 秒空闲超时。仍有回音的连接是通的，失败另有原因（例如服务端的本地服务慢），保留它，免得它承载的长连接流与替代连接各占一个中继名额。凭据被撤销后，复用连接不再接受新流，只读的状态查询会在新连接上重问一次，从而直接得到中继的结构化拒绝；不会重放已经发送的管理操作。淘汰、网络变化及恢复只影响后续池选择，已有流独立持有连接和 UDP 驱动。
- 隧道健康探测先解析一次中继地址，域名无法解析报告为 DNS 失败而不是传输失败。IPv6 链路本地地址保留 scope id。
- 冷连接建立与统计缓存使用独立锁；网络握手或流额度等待不阻塞诊断查询，也不阻塞其他已缓存连接的开流。单次 QUIC 建连（包括 TCP 发现）总预算 5 秒，多地址竞争使用同一候选预算；TCP 的原有 2 秒多地址候选预算保持不变。
- 一条已认证 QUIC 连接可承载多个双向流。每个流重用原 PBM2 鉴权、命名空间、注册、订阅及转发流程。没有应用数据重试/重放；中继两侧可以采用不同传输。
- 发现阶段的结构化拒绝保留服务端的 `code`、`message` 和 `retryable`。凭据被永久拒绝时，注册和连接工作线程结束，SDK 就绪等待返回失败；可重试拒绝继续使用拒绝退避，网络故障使用传输退避。状态与管理 API 也保留结构化错误。
- TLS 1.3、独立 ALPN、精确证书固定、首次 PBM2 凭据绑定 TLS exporter；禁用 0-RTT、单向流和 QUIC DATAGRAM。UDP 业务仍使用现有有序数据报封装。
- 服务端最多 64 个并行握手、128 个已认证 QUIC 连接；单个临时凭据最多占 16 个，管理员密钥（运维常在多台机器共用）只受总数限制。每连接最多 1024 个双向流；认证前仅开放 1 个流。握手超时 5 秒，单流窗口 4 MiB（与 TCP 自动调优的缓冲相当）；中继侧认证前连接接收窗口只有 256 KiB，认证后提升到 64 MiB，大量不认证的握手无法占住内存。客户端只为自己打开的流缓冲，连接级接收窗口不设上限。空闲超时 60 秒，客户端每 15 秒保活。窗口表示流量控制额度，不是预分配内存。
- 同一客户端的全部隧道共用一条连接，已收到而未转发的字节无论属于哪个流都占用连接窗口。读端停止读取的下载会把它那个流的窗口占满：连接窗口是单流窗口的 16 倍，因此 16 个以内这样停住的流不会拖住同一连接上的其他隧道（包括注册的心跳）。初版的 8 MiB / 32 MiB 只容得下 4 个，第 5 个起新流要等 5 秒建流超时。
- 握手名额用掉一半后，来源地址未经验证的 Initial 先收到无状态 Retry：伪造源地址的报文占不到名额，正常客户端多一个往返。
- 已认证连接容量满额时返回可重试的 `quic_connection_capacity`，单个凭据超额时返回可重试的 `quic_credential_capacity`，均使用拒绝退避。拒绝消息在原握手预算内等待 QUIC 发送确认，再关闭连接；不会提前释放握手名额，也不会增加无界排空任务。
- QUIC 准入阶段的认证失败与 TCP 写入同一安全日志（`auth_failed`，带频率抑制）；TLS exporter 绑定不符记为 `auth_stage = "quic_binding"`。QUIC 与 TCP 监听不同地址族时启动告警。
- 关闭中继时等待 QUIC 监听任务及其所有连接任务结束，进程内重启不会撞上仍被持有的认证状态锁或 UDP 端口。
- 撤销凭据立即取消原有授权流并停止接收新流；连接最多保留 3 秒关闭宽限，以便已提交的管理员密钥轮换响应完成发送。直连会话仍由端点持有，不受中继凭据撤销影响。
- TCP/QUIC 在转发循环外分派，循环单态化；QUIC 大变体装箱，默认 TCP 不创建连接池。未启用服务端 QUIC 时不创建 UDP endpoint。逐块活动通知改为任务内记录，不增加逐包任务、排队或复制。
- 加密 TCP 数据流只把完整帧之间的 EOF 识别为正常半关闭；截断帧仍报错，控制消息与 UDP 帧仍严格读取。这保证混合传输时反向尾部数据能够排空。

## 复现验证

```sh
cargo test -p pb-mapper-cli --all-features --test quic_relay --test direct_e2e
cargo run -p pb-mapper-cli --release --example relay_transport_bench
```

`quic_relay` 覆盖并行多流复用、单流关闭不影响同连接其他流、停住的下载不拖住同连接其他隧道、混合传输加密转发、UDP 数据报、明确拒绝降级、证书与 exporter 校验、凭据撤销、管理员密钥轮换、中继重启、旧 UDP 路径永久黑洞后的恢复，以及发现握手停滞时诊断仍能返回。`quic_retired_carrier`（独立进程，因为网络变化是进程级的）验证退役连接上最后一个流的响应尾部完整送达。`direct_e2e` 包含经 QUIC 中继协调的直连建立和协调器丢失测试。`sdk_e2e` 使用原有配置/请求字面量，验证启用 QUIC 的服务端不会改变 SDK 默认 TCP 行为；CLI 与 Node 测试也验证默认值。

基准使用两个独立客户端、一个内嵌中继及 echo 后端，先预热，再校验 64 MiB 回传和 200 次分段写入的 8 字节 RPC；不计入建连耗时。回环结果不能预测公网丢包和多流并发下的性能。

### 本机测量（2026-10-26）

以下是初版 PR 使用 Quinn 0.11 时的历史测量。分支现已同步主线的 Quinn 0.12、rcgen 0.15 与 UDP 性能改进，并完成接口适配；本轮未重新测量中继吞吐，以下数字不能作为更新后版本的性能结论。

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
