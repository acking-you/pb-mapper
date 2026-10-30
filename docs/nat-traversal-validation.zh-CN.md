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
并验证候选去重及 8 个上限。

```bash
cargo test -p pb-mapper-client --all-features --lib --locked
```
