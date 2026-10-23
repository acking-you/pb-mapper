<div align="center">

<img src="docs/assets/poster.png" alt="pb-mapper — 随手拉一根虚拟网线：选择直连时端到端优先，无法直连时回退中继" width="800" />

<p>
  <a href="https://www.rust-lang.org/"><img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-000000?logo=rust&logoColor=white"></a>
  <a href="https://tokio.rs/"><img alt="Tokio" src="https://img.shields.io/badge/Async-Tokio-3873AD"></a>
  <a href="https://flutter.dev/"><img alt="Flutter" src="https://img.shields.io/badge/UI-Flutter-02569B?logo=flutter&logoColor=white"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-blue.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/releases"><img alt="Release" src="https://img.shields.io/github/v/release/acking-you/pb-mapper?logo=github&color=success"></a>
  <a href="https://github.com/acking-you/pb-mapper/actions/workflows/release.yml"><img alt="Build" src="https://github.com/acking-you/pb-mapper/actions/workflows/release.yml/badge.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/actions/workflows/docker-publish.yml"><img alt="Docker Image" src="https://github.com/acking-you/pb-mapper/actions/workflows/docker-publish.yml/badge.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/stargazers"><img alt="Stars" src="https://img.shields.io/github/stars/acking-you/pb-mapper?style=social"></a>
</p>

<p>
  <a href="README.md">English</a> ·
  <a href="README.zh-CN.md"><b>中文</b></a>
</p>

</div>

---

> **随手拉一根虚拟网线，接到任何服务。**

**pb-mapper** 是一层连接协调层。在私有 TCP/UDP 服务旁运行 `register`，在需要
使用它的地方运行 `connect`，这项服务就出现在本地端口上，就像两台机器之间拉了
一根网线。一个公网 relay 端口负责认证两端、按命名空间隔离，并决定字节怎么走。

- **默认走中继。** 所有连接都可以经 relay 转发：Rust + Tokio 实现，支持 TCP 与
  UDP，每段可选 AES-256-GCM。不主动选择，就不协商任何新能力。
- **选择直连时端到端优先。** 选择直连的连接先尝试端到端路径：在同一个 UDP
  socket 上打洞，再建立双向认证的 QUIC。之后业务字节不经过 relay，会话归两端
  持有，协调服务掉线不会切断它。
- **无法直连时回退中继。** 如果在建连预算内没有可用的直连路径，同一连接会在
  发送任何业务字节之前回退到 relay。一旦已为它授权了提供端，回退就锁定该提供端，
  其后端只会被拨号一次。

pb-mapper 负责传输字节；隧道后的服务仍然负责自身的应用层认证与授权。

## 为什么使用它

- **一个公网端口，多项服务**：注册、订阅、状态查询与管理请求都进入同一个 relay
  端口；新增服务不需要再开放公网监听端口。
- **委派权限，而不是下发根密钥**：唯一的 32 字节管理员密钥留在 relay 上，为每个
  使用方签发可续期、可过期的 `pbmt1_` 凭据；每把凭据拥有隔离的命名空间。
- **可以中止实时访问**：凭据过期、吊销、认证状态重置与根密钥轮换会关闭受影响的
  中继连接，并拒绝新的直连建立。
- **紧凑、易于嵌入**：一个 Rust 二进制包含 relay、register、connect、status 与
  admin 角色；Rust 与 Node SDK 内嵌同一套客户端。
- **适合远程控制场景**：agent harness、编码会话、私有 API、模型网关、开发机与
  游戏服务器都在通过它进入私有运行环境。

弱网恢复行为、资源边界与故障注入覆盖见 [弱网恢复设计](docs/weak-network-recovery.md)。

## 一个 Relay，连接多项私有服务

![pb-mapper architecture](docs/assets/architecture-flow.svg)

```text
 私有环境 A ── register "app" ──┐
 私有环境 B ── register "shell" ├──► pb-mapper relay :7666 ◄── connect
 私有环境 C ── register "tools" ┘             一个公网端口
```

- relay 按命名空间与 service name 匹配两端。
- 不同临时凭据可以使用相同的 service name，彼此不可见，也不会发生冲突。

## 连接如何选择路径

是否直连按每次连接决定，并且三方都同意才会尝试：

| Relay 服务端 | `register` | `connect` | 路径 |
| --- | --- | --- | --- |
| 默认 | 任意 | 任意 | 中继 |
| `--enable-direct` | 默认 | 任意 | 中继 |
| `--enable-direct` | `--allow-direct` | 默认（`--path relay-only`） | 中继 |
| `--enable-direct` | `--allow-direct` | `--path auto` | 直连优先，提交前失败回退中继 |

```sh
pb-mapper server --port 7666 --enable-direct \
  --direct-listen 0.0.0.0:10811 --direct-advertise 公网IP:10811
pb-mapper register tcp --key demo --addr 127.0.0.1:8080 \
  --server relay.example.com:7666 --allow-direct
pb-mapper connect tcp --key demo --addr 127.0.0.1:9090 \
  --server relay.example.com:7666 --path auto
pb-mapper status sessions --server relay.example.com:7666
```

除 TCP relay 端口外，还需放行公布的 UDP 端口。凭据配置方式不变；省略这些选项
即保持原有协议和控制方式，已有客户端和部署不受影响。

需要了解的边界：

- **回退只有一个边界。** 提供端只有在消费端提交直连后才拨号后端。提交前任何
  失败都回退中继；提交后绝不把业务重放到中继，避免后端副作用重复。
- **直连会话归两端所有。** 停止 relay 或吊销凭据只阻止新的建连，不关闭已建立的
  直连；停止本地隧道才会关闭。`status sessions` 返回带时间与 stale 标记的端点
  观测，而不是存活证明。
- **并非所有网络都能打通。** 对称 NAT、UDP 封锁等网络使用中继。目前已验证：
  两层独立 Linux NAT 之间 TCP、UDP 直连成功；阻断 UDP 后回退中继且后端没有重复
  连接；一次 macOS 到公网服务器的测试回退到了中继。详见 [验收记录](docs/issue-13-validation.zh-CN.md)
  与 [设计说明](docs/issue-13-direct-connect-design.zh-CN.md)。

Rust SDK 开启 `p2p` feature 后使用 `register_with_options` / `connect_with_options`；
Node 提供 `allowDirect`、`path` 与 `sessionsJson()`。Flutter UI 及其 C ABI 继续使用中继。

## 安装 Agent Skill

直接告诉你的 agent：

```text
Fetch and follow instructions from https://raw.githubusercontent.com/acking-you/pb-mapper/master/skills/pb-mapper-suite/INSTALL.md
```

## 面向自动化的凭据模型

| 凭据 | 适合的持有者 | 权限边界 |
| --- | --- | --- |
| 管理员密钥 | Relay 操作者或可信的凭据分发自动化 | 签发、查看、续期、吊销、轮换并检查所有命名空间 |
| 临时 `pbmt1_` 凭据 | 单个 harness、租户、设备或 workload | 只能注册、连接和查看自己的命名空间 |

V2 协议在加密的首个请求内完成认证，不额外增加一次握手往返。临时凭据由管理员
密钥、持久化 server instance ID 与 key ID 派生；relay 保存的是生命周期元数据，
而不是每把临时密钥的副本。注册服务时增加 `--codec`，还可以启用可选的
AES-256-GCM 转发数据加密。

pb-mapper 使用预共享凭据，不提供公钥身份体系。如果还需要基于证书的端点身份或
对流量分析的防护，应在其上使用 TLS 或其他应用协议。准确的安全边界见
[认证设计](docs/authentication-v2.zh-CN.md)。

## 当前已经提供的接入面

| 接入面 | 当前能力 |
| --- | --- |
| 统一 CLI | `server`、`register`、`connect`、`status`、`admin` 五种角色 |
| Agent Skills | 通过 `pb-mapper-suite` 完成安装、relay、register/connect 与验收，release 工作流保持独立 |
| 运维能力 | Linux systemd、安装脚本、Docker 镜像、状态查询与管理员级服务/连接清单 |
| 原生嵌入 | Rust crate `pb-mapper`（`Client`：register / connect / status / admin）、Flutter 使用的 C ABI、`js/` 下的 Node-API 包 |
| 网络能力 | TCP、UDP、可选 QUIC 直连与中继回退、按隧道 keep-alive、可选的转发数据加密 |

### Rust SDK

```toml
pb-mapper = "0.7"
```

填入已部署的 relay 地址和凭据后即可 register、connect、查 status，以及用管理员凭据调用全部 admin 接口。示例见 `crates/pb-mapper/examples/`。

### CLI 与 Relay Server

可以直接从 crates.io 安装统一 CLI：

```bash
cargo install pb-mapper-cli --locked
```

crate 名称是 `pb-mapper-cli`，安装后的可执行文件名为 `pb-mapper`。它同时包含
`server`、`register`、`connect`、`status` 和 `admin` 角色，无需另外安装
server 可执行文件。

Relay 实现也以 `pb-mapper-server` library crate 形式发布，便于需要内嵌
relay 的 Rust 应用使用。

### TypeScript（Node-API）

```bash
npm install pb-mapper
```

```ts
import { Client } from "pb-mapper";
const client = new Client({
  server: "relay.example.com:7666",
  credential: process.env.MSG_HEADER_KEY!,
});
const admin = client.admin();
```

## Roadmap：Harness 原生的 Remote Control

当前版本已经提供安全网络、Rust client SDK 与 Node-API 包。仍待完成：

- 基于现有 `pb-mapper-suite` 工作流扩展更多 harness 专用 adapter 与凭据自动化；
- 继续压缩 client-only Node 插件（linux-x64 上 `release-node` + strip 已经低于 **5 MB**）；
- 面向无法加载原生 Node-API 插件的浏览器与 edge runtime adapter；
- 面向远程模型 runtime、tool server、私有 API、开发机与浏览器控制端点的
  harness adapter 和示例。

以上均为路线图，不属于当前已发布的兼容性承诺。

## 构建与文档

```bash
make build-pb-mapper
cargo test
```

- 使用手册：[`docs/user-guide.zh-CN.md`](docs/user-guide.zh-CN.md)
- 认证与 V2 协议：[`docs/authentication-v2.zh-CN.md`](docs/authentication-v2.zh-CN.md)
- Docker 服务端指南：[`DOCKER_README.md`](DOCKER_README.md)
- English docs：[`README.md`](README.md)、[`docs/user-guide.md`](docs/user-guide.md)

仓库结构：

- `crates/` — Rust workspace：core、auth、protocol、server、client、SDK facade（`pb-mapper`）、Node-API（`pb-mapper-node`）、CLI 与 testkit
- `js/` — 包装 Node-API 插件的 JS 包（用 bun 构建）
- `ui/` — Flutter UI 与原生 C ABI bridge
- `skills/` — agent 可读取的部署与 release 工作流
- `docs/` — 架构、认证、使用手册与项目素材
- `docker/`、`services/`、`scripts/` — 打包和运维工具

## 许可证

基于 [MIT License](LICENSE) 发布。
