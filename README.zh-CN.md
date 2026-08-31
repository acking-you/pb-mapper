<div align="center">

<img src="docs/assets/poster.png" alt="pb-mapper — Agent Harness 时代的 Remote Control 基础设施" width="800" />

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

> **Agent Harness 时代的 Remote Control 基础设施。**

Agent harness 已经可以写代码、调用工具并持续执行长流程，但它仍然需要一条
狭窄、持久且可治理的通道进入私有运行环境。**pb-mapper** 提供的正是这层网络
原语：只暴露一个公网 relay 端口，在端口背后注册任意多项带 key 的 TCP/UDP
服务，并在不公开每项服务、不下发 relay 根密钥的前提下委派访问能力。

pb-mapper 负责传输字节；隧道后的服务仍然负责自身的应用层认证与授权。

## 为什么适合 Agent Harness

- **紧凑、易于嵌入**：一个 Rust 二进制同时包含 relay、register、connect、
  status 与 admin 角色；Linux 版本以小型自包含压缩包分发，不依赖语言运行时。
- **一个公网端口，多条控制路径**：注册、订阅、状态查询与管理请求都进入同一个
  relay 端口；新增一项 remote-control endpoint 不需要再开放一个公网监听端口。
- **委派权限，而不是下发根密钥**：唯一的 32 字节管理员密钥留在 relay 上，
  relay 为 harness 或 workload 签发可续期、可过期的 `pbmt1_` 凭据；每把凭据
  都拥有隔离的命名空间。
- **可以中止实时访问**：凭据过期、吊销、认证状态重置与根密钥轮换都会关闭受影响
  的在线控制连接和数据连接。
- **原生性能，真实生产经验**：数据路径由 Rust + Tokio 实现，支持 TCP 与 UDP，
  已长期用于自身产品和真实游戏服务器流量；端到端测试覆盖传输/加密组合与完整凭据
  生命周期。

## 一个 Relay，连接多项私有服务

![pb-mapper architecture](docs/assets/architecture-flow.svg)

```text
 私有环境 A ── register "app" ──┐
 私有环境 B ── register "shell" ├──► pb-mapper relay :7666 ◄── agent harnesses
 私有环境 C ── register "tools" ┘             一个公网端口
```

- `pb-mapper register` 运行在私有 TCP/UDP 服务旁，把 service name 注册到 relay。
- `pb-mapper connect` 运行在 agent 或操作者一侧，把已注册服务暴露到本地地址。
- relay 按命名空间与 service name 匹配两端，再进行双向数据转发。
- 不同临时凭据可以使用相同的 service name，彼此不可见，也不会发生冲突。

因此，一台 relay 可以同时作为远程 agent runtime、coding harness、私有 API、
模型网关、浏览器控制端点、开发机和运维工具的会合层。

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
| 网络能力 | TCP、UDP、按隧道 keep-alive、可选的转发数据加密 |

### Rust SDK

```toml
pb-mapper = "0.5"
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
