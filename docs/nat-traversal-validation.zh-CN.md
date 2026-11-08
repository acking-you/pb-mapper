# 多层 NAT 验收

测试日期：2026-10-28。环境为 Windows x86_64 上的 WSL2 Linux
6.6.87.2-microsoft-standard-WSL2、Rust 1.99.1。以下是功能验收，不是性能基准，
也不能代表任意运营商网络的打洞成功率。

## 可重现拓扑

[`scripts/test_nat_traversal.py`](../scripts/test_nat_traversal.py) 在 Linux 创建独立
network namespace，提供端、消费端分别经过两层 NAT。协调/中继位于独立外侧
bridge；端点私网之间没有 IPv4 路由，IPv6 只在专项场景允许 UDP。

```text
consumer -- NAT c1 -- NAT c2 -- 外侧 bridge / relay -- NAT p2 -- NAT p1 -- provider
```

稳定映射场景显式配置端口保持的地址转换，转发规则仅允许出站流及其 conntrack
返回流。不能把裸 Linux MASQUERADE 一概视为端点无关映射：首个入站包造成的
conntrack 元组冲突可能使之后的出站包换端口。目的地址相关映射场景独立配置，
避免把这两种行为混为一谈。

```bash
cargo build --locked -p pb-mapper-cli --features port-mapping
sudo python3 scripts/test_nat_traversal.py --binary target/debug/pb-mapper
```

需要 `iproute2`、`iptables`、`iputils-ping` 和 Python 3；可选 `tcpdump` 仅记录包头。
脚本等待注册、监听和租约事件，不靠固定睡眠判断就绪；退出时停止测试进程并删除
本次创建的命名空间。每次使用随机独立凭据和临时认证目录。日志位于
`target/nat-acceptance/`，CI 的 **NAT traversal** workflow 上传同名日志 artifact。

## 结果与对照

| 场景 | 当前实现结果 |
|---|---|
| 两端各串联两层稳定映射 NAT | TCP 65,536 字节和 UDP 1,200 字节均 direct，内容完整 |
| 提供端向 STUN 使用端口 41000，向消费端使用 41001；消费端允许新来源 | direct，实际选择 `198.18.0.3:41001` |
| 消费端 IPv4/IPv6 UDP 全部阻断 | relay，TCP 内容完整 |
| IPv4 UDP 阻断，端点 IPv6 路由可达 | direct，实际选择 IPv6 对端 |
| PCP 代理同时控制两层 NAT，提供端无法主动发公网 UDP | direct，实际选择映射端口 42000；短租约续租、网络变化重建、停止释放均成功 |

TCP 校验 SHA-256：
`7daca2095d0438260fa849183dfc67faa459fdf4936e1bc91eec6b281b27e4c2`。
UDP 校验 SHA-256：
`fa4b8dd5fb8abf8243d6ad379e1666ee886862fac05dc3f05a61d2c01c527afe`。
每条 TCP 业务连接只拨号一次后端。映射场景包含一次预热请求，因此总计两次后端拨号。

使用 GitHub Release **v0.9.8** 的 Linux musl 包作对照，并核对其发布的 SHA-256：
`6a9d821d7660395da7161e0dfd37baa89e126e94a77ae15000ce8b5dc2ca2090`。
相同稳定映射拓扑下旧版也能 direct；相同目的地址相关映射拓扑下旧版回退 relay，
本次实现通过认证 Ping 来源的反向挑战获得实际映射并 direct。

## 覆盖边界

PCP 代理是验收脚本中的测试网关，确实在两层 Linux 路由器安装、删除 DNAT 规则。
它证明客户端发现、映射使用与租约生命周期；不代表普通家用 UPnP 能控制运营商 NAT。
NAT-PMP/UPnP 由同一个 `portmapper` 库提供，本次没有真实硬件互操作测试。

Rust 定向测试覆盖探测认证、nonce、授权过期、候选上限与 IPv6 保留；既有集成测试
继续覆盖 StartDirect 提交边界、后端只拨号一次、协调器丢失后直连存活、撤销凭据及
本地停止。QUIC 业务数据收发路径没有修改。

### 恢复通知与候选缓存回归

#42 的回归使用独立回环中继、SDK 注册/连接和 TCP echo。网络代数不变时，注入
中继传输失败与协议恢复通知，等待连接 worker 的健康探测唤醒后再建立业务连接，
fallback 次数仍为 1；随后通知真实网络变化，下一次连接允许重试 direct，次数变为 2。
修复前同一测试在中继恢复后已得到 2。整个用例限制在首次 10 秒冷却内，并使用独立
网络通知源，不改变实际网络或干扰并行测试。同时待处理的网络变化与中继恢复会合并，
但保留网络变化的语义。

#44 的回归等待回环 STUN 收到真实 Binding Request，在响应前使缓存失效，再返回
旧映射。后续两个并发查询必须共用一次新发现、得到新映射；同代数缓存命中不再发包，
共享网络代数变化也能使空闲缓存失效。#43 的候选测试保留网关映射、STUN 地址与 IPv6，
并验证候选去重及 8 个上限；它现在按开发机实测的网卡构成（WLAN 带一个稳定 IPv6
与一个 /128 临时地址、wintun、Hyper-V 交换机及大量虚拟网卡）验证局域网 IPv4 紧随
STUN 地址、每个 /64 只占一个名额。

```bash
cargo test -p pb-mapper-client --all-features --lib --locked
```

## 局域网先行（2026-11-08）

设计见 [直连设计第 5 节](issue-13-direct-connect-design.zh-CN.md#5-网络身份和资源边界)。
Rust 定向测试覆盖：开发机网卡构成下的候选顺序；同网证据的强、弱与无（同一全局
/64 含 Windows /128 临时地址、同一公网地址加子网、仅子网，以及相同地址、虚拟网卡
子网、仅公网地址）；先行规则的纯函数；一个回环端到端用例——注入把回环当作局域网
网卡的网卡表，断言 `link.kind == lan`、IPv4、强证据、各阶段耗时与 STUN 结果、
QUIC RTT 读数、只握手一次，以及会话结束后链路记录与连接克隆一并释放。协议测试
确认不带 `link` 的报告与以前逐字节相同、未知种类可以解析。

局域网优先可以关闭（默认开启）：先行规则的纯函数测试覆盖关闭、局域网路径已回应
（弱证据或无证据时同样等待）、子网撞号（无回应即不等待）三种情形；关闭时证据照常
上报，但探测保持对端提交的顺序、没有补发。`RegisterOptions` / `ConnectOptions` 的
默认值、CLI `--no-lan-first`、应用旧 config.json 读作开启、helper 不写出默认值，
各有一个测试。

同一回环场景（中继、提供端、消费端都在本机，Windows 11）的建连时间：

| 版本 | setup_ms |
|---|---|
| 765788c | 稳态 140–144 ms，冷启动 184 ms；每次约 4 次 QUIC 握手 |
| 本次，无同网证据 | 稳态 20–22 ms，冷启动 59 ms；回应即拨号，6 条本机路径全部握手 |
| 本次，注入局域网、强证据 | 冷启动 30–44 ms；只握手一次 |

差别来自不再等待下一个 120 ms 探测 tick。开发机上 netdev 一次读取需 1.2–3.2 秒，
因此它只在后台线程运行，建连始终使用 if-addrs 的地址。

### 真机验收步骤

1. 手机与 PC 连同一 Wi-Fi，手机查看 PC 的远程桌面或连接 PC 共享的服务。隧道路径
   标签应为“局域网”，提示里是 IPv4 或同网段 IPv6 地址、个位数毫秒的往返时间，
   建立用时约 300 ms 以内；画质面板的链路 RTT 同样为个位数毫秒。
2. 在手机“日志”页找到 `session_path_selected` 行，应为 `kind=lan`，`peer` 为
   PC 的 192.168.x 地址或同一 /64 的 IPv6，`evidence` 为 `public_ip_subnet` 或
   `ipv6_prefix`。
3. 关闭手机 Wi-Fi 改用蜂窝：标签应回到“直连”或“中继”，建连不慢于以前。
4. 路由器开启 AP 隔离（客户端隔离）时：局域网探测无回应，其他路径最多晚 150 ms
   拨号，结果与以前相同。
5. iOS/macOS 首次直连会弹出本地网络权限，说明文字来自 `NSLocalNetworkUsageDescription`；
   拒绝后局域网发送被系统拒绝，先行立即结束，仍可经互联网直连或中继。
6. 中继已更新时，“中继”页的会话显示“局域网”；旧中继丢弃该字段，只显示“直连”。
7. A/B：在**两端**关闭局域网优先——应用里是 设置 →“直连”→“优先尝试局域网”，
   CLI 是 `register` / `connect` 加 `--no-lan-first`——再重启隧道、重新开始共享并
   重新连接远程桌面（开关只在启动时读取）。只关一端时，另一端仍按局域网优先探测与
   等待，结果看不出差别。关闭后路径照样可以是“局域网”，但选中的是最先完成握手的
   路径，握手次数可能多于一次；`session_path_selected` 的 `evidence` 照常出现，
   `lan_missed` 计数可能增加，这是预期。对比两组的 `setup_ms`、握手次数与
   `lan_first_pong_ms`，再把开关打开恢复默认。

## Android 蜂窝网络真机验收（2026-11-06）

版本 `0.10.2+927`，Vivo V2405DA / Android API 36，Windows 11 提供端。
手机 Wi-Fi 关闭，CipherRelay VPN 保持运行，pb-mapper 应用绕过 VPN。
应用显示 direct，提供端记录 10 次 direct 路径事件，对端 IPv6 与手机蜂窝
地址一致，用户确认业务流量正常。脱敏环境、时间和安装包摘要见
[`android-cellular-927.json`](validation/android-cellular-927.json)。

此前本地 926 应用实际在 VPN 接管范围内，出现 STUN 超时并在提交前回退
中继；shell 中的 SDK 0.7.0 和 0.10.2 都绕过该 VPN，均完成 IPv6 直连及
HTTP 200，不能据此推断新 SDK 退化。修正应用绕过规则后验证成功；本轮
没有修改 P2P 协议，也不声称任意 VPN 路径都不能直连。

这些事件对应普通业务隧道，不能证明远程桌面的视频流畅度。用户随后明确
反馈直连下桌面仍严重卡顿、不足 30 帧，明显不如 RustDesk（用户观察，尚非
仪器采样）。**桌面性能验收未通过，解决前不得合并 PR #51。**
待查明事项、代码入口与后续验收要求见
[性能交接](validation/desktop-input-backpressure.md#performance-handoff-2026-11-06)。
