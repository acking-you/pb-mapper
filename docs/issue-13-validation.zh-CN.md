# Issue #13 验收记录

测试日期：2026-10-23。实现与边界见 [设计说明](issue-13-direct-connect-design.zh-CN.md)。没有对生产 7666 服务做替换或重启。双 NAT 和公网回退测试完成后，已停止对应临时进程、删除独立凭据和认证目录、清理 network namespace/bridge 及专用防火墙规则。随后按用户要求重新使用 12111/10811 启动独立 HTTP 演示，保留运行供用户查看；生产 7666 始终保留。

## 本地核心验收

- `direct_e2e.rs`：TCP 32 KiB 双向往返、独立连接的 256 KiB 半关闭排空、UDP 1/1200/8000 字节边界；服务端关闭后继续传输，重启后恢复两端观测，本地 stop 关闭已有直连；撤销凭据不关闭已有直连，但同凭据新建 auto 连接的 readiness 必须立即报错，其他 namespace 看不到观测。
- 客户端 direct 单元集成测试：丢弃 QUIC 包时走真实中继、payload 往返且只拨一次后端；相互证书与 TLS exporter 绑定；Ready 阶段不拨后端，StartDirect 后丢失 Started、迟到 RelaySelect 不再确认中继或重复拨后端。
- 服务端状态测试：跨 namespace、错误 secret、一用 relay attach、乱序报告、报告不能授予中继权限，重启后的观测不能恢复许可；无直连提供端时不提前消费中继订阅的限流额度。
- 默认消息精确编码检查、现有 data-v2/旧 codec 和凭据撤销回归；关闭服务端能力或提供端不允许直连时使用原有中继。
- Node 同一进程默认 connect 与 `path: "auto"` 分别选择中继和直连，注册 `allowDirect`、诊断和 sessions 查询通过；原有用例保留。

## Linux 双 NAT 实测

在授权服务器创建临时 network namespace；只使用测试 TCP 12111 / UDP 10811 和独立凭据、认证目录。两个端点位于不同私网，各有独立 Linux MASQUERADE 路由器。私网之间禁止路由；路由器过滤未匹配的入站 UDP，只有出站建立的 conntrack 映射允许返回。

| 端点 | 私网 | NAT 外侧 |
|---|---|---|
| 消费端 | `10.203.13.2` | `10.203.15.10` |
| 提供端 | `10.203.14.2` | `10.203.15.20` |
| 协调/STUN | 测试 bridge `10.203.15.1` | TCP 12111 / UDP 10811 |

云主机公网 IP 不在本机接口上；为避免把云 NAT 回环混入此测试，两台测试路由器把公布的 STUN 地址定向到本机测试 bridge，反向 NAT 仍还原原 STUN 来源。候选由实际 UDP socket 的 Binding 发现，未把两端私网地址改成直连地址。

| 场景 | 结果 | 首次 setup + 往返时间 |
|---|---|---|
| TCP 256 KiB，双方 NAT | 日志确认 `path=direct`，payload 完整 | 0.198 s |
| UDP 1200 B，双方 NAT | 日志确认 `path=direct`，边界完整 | 0.181 s |
| 在消费端阻断全部出站 UDP，再开 TCP | `path=relay`，payload 完整 | 2.060 s |

后端计数为 2：第一次直连和第二次回退各一次，没有探测或回退导致的重复业务连接。这些单次值是验收证据，不是延迟分位或吞吐基准。

## 实际公网链路

本机 macOS ARM64 → 授权服务器 Linux x86_64，以 TCP 12111 协调/中继，UDP 10811 STUN。使用原有凭据格式的独立测试 key，提供端显式允许直连，消费端选择 auto。

| 场景 | 实际结果 |
|---|---|
| TCP 256 KiB | 2.449 s，直连尝试超时后中继往返成功 |
| UDP 1200 B | 2.395 s，直连尝试超时后中继往返成功 |
| 启用直连的同一服务器，显式 relay-only | 原有中继往返成功 |

本机成功取得 STUN 映射，但这条公网链路没有选中直连。尚未确定限制来自哪一层 NAT/防火墙，不将此结果表述为公网打洞成功；它验证的是真实网络下有界回退。双 NAT 成功也不代表所有目的地址相关映射或 UDP 封锁都能穿透。

## 本机 HTTP 演示

本机 Python HTTP 服务 → 本机 `register tcp --allow-direct` → QUIC → 本机 `connect tcp --path auto` → 浏览器 `http://127.0.0.1:18081/`。线上测试服务只负责协调、STUN 和失败时的中继。所有演示进程、凭据和日志独立保存于被 Git 忽略的 `target/direct-http-demo/`，其中 `stop.py` 可停止本机与远端的演示进程。

消费端日志确认 `session_path_selected path=direct`，选择本机网卡候选地址。浏览器下载 1,048,576 字节并校验 SHA-256，一次实测为 56.9 ms。初次请求曾回退到 relay，之后请求选中 direct；演示页面直接读取日志并展示计数，不隐藏回退记录。两端在同一台机器，因此这项验收证明 HTTP 经过真实 QUIC 直连传输，不作为跨公网 NAT 打洞成功的证据。

## 合并前复查（第二轮）

- 服务端选择提供端：较新的仅中继注册不再遮住较旧的可直连注册（服务端状态测试覆盖）；已获准的回退中继在 namespace 满额时仍保留原有槽位。
- 进程凭据更新只修改进程内状态，不再写环境变量；运行中的根密钥轮换与 UI 改配置不会与 DNS 等线程的环境读取竞争。相关测试不再需要修改环境。
- 直连收包改为 Quinn 驱动原地分流，GRO 拼包中夹杂的控制包有单元测试覆盖；新增 relay 邮箱变体装箱，ConnTask 回到 master 的 240 字节。
- 本机 `cargo test` 覆盖 core/auth/server/client 全部单元测试，以及 CLI 的 direct_e2e、regression、sdk_e2e、temporary_credential_e2e、network_recovery、data_compatibility、test_delay。

本机回环吞吐（macOS ARM64、release、三个角色同一进程、单连接写 512 MiB 共 3 次，多轮重复）：直连约 130–180 MiB/s，新旧两种收包实现差异在噪声范围内；同环境 TCP 中继约 1.0–1.6 GiB/s。采样显示直连瓶颈是 macOS 上每个 QUIC 包一次 `sendmsg`/`recvmsg`（无 GSO/GRO），Quinn 的 `fast-apple-datapath` 也没有改善。新收包路径的收益在支持批量接收与 GRO 的 Linux 上，本次未测量。这是回环数据，不代表公网链路；直连在公网上的价值是绕开 relay 这一跳及其带宽，而不是回环吞吐更高。

## 构建与回归

- Rust 1.99.1；`cargo fmt --all`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`。
- `cargo test --workspace --all-features`，含 FFI 原有测试与新功能测试。
- `cargo check -p pb-mapper-client --no-default-features`，验证不引入 p2p 的 SDK 仍可构建。
- `bun run build:release` + `bun test`：11 个 Node 用例通过；本机 macOS ARM64 addon 3,127,088 字节，保留 CI 原有 5.5 MiB 限制。
- `cargo zigbuild --bin pb-mapper --release --target x86_64-unknown-linux-musl`：交叉构建并用于上述 Linux 实测。

测试只证明已列出的环境与边界；本次不宣称大规模压力测试、持续路径最优选择或运行中无损切换。三个 fork 没有源码补丁，固定 gitlink 与维护分支见 [依赖说明](../deps/README.md)。
