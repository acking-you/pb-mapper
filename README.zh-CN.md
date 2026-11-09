<div align="center">

<img src="docs/assets/poster.zh-CN.png" alt="pb-mapper — 隧道、远程桌面与一套 SDK：最前面是远程桌面查看器，正显示另一台电脑和质量监测；后面是桌面应用的星系，画着这台电脑、中继和每条隧道；Android 手机上是远程页，列出记住的电脑" width="800" />

<p>
  <a href="https://www.rust-lang.org/"><img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-000000?logo=rust&logoColor=white"></a>
  <a href="https://tokio.rs/"><img alt="Tokio" src="https://img.shields.io/badge/Async-Tokio-3873AD"></a>
  <a href="https://flutter.dev/"><img alt="Flutter" src="https://img.shields.io/badge/UI-Flutter-02569B?logo=flutter&logoColor=white"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-blue.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/releases"><img alt="Release" src="https://img.shields.io/github/v/release/acking-you/pb-mapper?logo=github&color=success"></a>
  <a href="https://crates.io/crates/pb-mapper"><img alt="crates.io" src="https://img.shields.io/crates/v/pb-mapper?logo=rust"></a>
  <a href="https://www.npmjs.com/package/pb-mapper"><img alt="npm" src="https://img.shields.io/npm/v/pb-mapper?logo=npm"></a>
  <a href="https://github.com/acking-you/pb-mapper/actions/workflows/release.yml"><img alt="Build" src="https://github.com/acking-you/pb-mapper/actions/workflows/release.yml/badge.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/stargazers"><img alt="Stars" src="https://img.shields.io/github/stars/acking-you/pb-mapper?style=social"></a>
</p>

<p>
  <a href="README.md">English</a> ·
  <a href="README.zh-CN.md"><b>中文</b></a>
</p>

</div>

---

**pb-mapper** 用来访问 NAT 后面的私有 TCP / UDP 服务，也能沿同样的路径查看并操控另一台电脑的屏幕。在服务旁边运行 `register`，在需要用它的地方运行 `connect`，服务就出现在本地端口上。每条连接能直连就直连：两端在同一**局域网**时先走局域网，否则走 UDP 打洞的 **P2P QUIC 直连**；两条都走不通时，在发送任何业务字节之前**自动回退到中继**。路径不需要手动选：CLI、应用和 SDK 按连接自动决定，并在中继重启、断网和网络切换时自动维持隧道。

```text
                 ┌──── 直连：UDP 打洞 + QUIC ────────────────┐
 你的应用 ─► connect                                          register ─► 私有服务
   :9090         └──── 回退：relay.example.com:7666 ──────────┘
```

**远程桌面**走的也是这些路径：在应用里共享一台 Windows 电脑，就能从另一台电脑或手机上查看并操控它，画面由硬件 H.265/H.264 编码（Windows 和 Android 上原生解码），键鼠是真实输入。**Rust SDK** 把这一切都交给你自己的应用：隧道、远程桌面的编码画面（交给你自己的解码器）、键盘鼠标输入、状态流和自动恢复。

## 为什么选 pb-mapper

**🧭 任何 TCP / UDP 服务，都走最好的那条路。** `register` 和 `connect` 就是隧道的全部 API。局域网发现、打洞、双向认证 QUIC、经 TCP 或 QUIC 的中继回退、断线重连、DNS 刷新、网络切换后的恢复都在里面自动完成，TCP 和 UDP 一视同仁。一个进程可以同时维护任意多条隧道，每个 Client 各用各的凭据。

**🔌 一个 TCP 端口承载全部中继，一个 UDP 端口让所有服务自由打洞。** `7666/tcp` 承载所有服务的注册、订阅、状态查询、管理和中继数据；`7666/udp` 为所有服务协调打洞。新增服务不用再开任何端口。

**🔑 自带密钥管理、隔离和事件监听。** 一把管理员密钥签发可续期、可吊销的 `pbmt1_` 临时凭据，每把凭据拥有独立命名空间，租户、设备或 agent 会话只能看到自己的服务。每条隧道都提供状态流和事件历史（直连建立、回退中继、中继断开、网络切换），接入任何 App 都很直接，agent harness 的 remote control 正是它的主场景。

**⚡ 两条路都快。** 作者实测：P2P 直连可以把交互延迟压到 **20 ms 以内**；回退到中继时，延迟和稳定性与 frp「一个端口对应一段映射」的模式持平。实际表现取决于两端网络和中继所在位置。

**🖥️ 远程桌面，走同样的路。** 在 Windows 电脑上打开远程控制，把邀请以文本或二维码交给对方，就能从另一台电脑或手机上查看并操控它。主机用硬件编码，两端都支持时用 H.265，否则用 H.264，默认按远程显示器的原始分辨率传输；没有原生解码器的查看端（macOS、Linux、iOS 上的应用，或没装 Media Foundation 的 Windows）收到的是 JPEG，最高 8 FPS。键鼠是真实输入，可选安装的 Windows 辅助服务还能操作 UAC 提示和本人的锁屏。每个会话都是 TLS 1.3，邀请里固定了主机证书，单凭中继凭据既看不到画面也控制不了。和每条隧道一样，它先试局域网，再直连，最后中继；连接断开后最多重试两分钟，点返回后会话暂停，最多保留十分钟。

**🧩 一套 SDK 全部搞定。** Rust crate `pb-mapper` 承载隧道；开启 `desktop` feature 后还包含完整的远程桌面：几行代码起一个主机；查看端的编码视频单元（H.265、H.264 或 JPEG，各带关键帧标记、generation、帧号和时间戳）直接交给平台自己的解码器；键盘、鼠标与文本输入；每条隧道、每个主机和查看端的状态流与指标流；以及 `Supervisor`——把应用自己的重连策略做成了库，带着查看端熬过卡顿、主机重启和 H.265 解码失败（自动退回 H.264）。桌面端和手机上的应用都基于同一套 SDK。Node SDK 覆盖隧道部分。

## 快速开始

**1. 在有公网 IP 的机器上运行 relay：**

```bash
cargo install pb-mapper-cli --locked   # 或从 Releases 下载二进制
pb-mapper server                       # 7666/tcp 中继，7666/udp 打洞
```

在防火墙和云安全组放行 **7666/tcp 和 7666/udp**。首次启动时 relay 会在认证状态目录生成随机管理员密钥 `admin.key`（Linux 系统服务为 `/var/lib/pb-mapper/auth`）。

**2. 为两端签发凭据：**

```bash
export MSG_HEADER_KEY="$(sudo cat /var/lib/pb-mapper/auth/admin.key)"
pb-mapper admin --server relay.example.com:7666 key issue --ttl 24h --label laptop
```

**3. 用打印出的 `pbmt1_…` 凭据发布服务并访问它：**

```bash
# 服务所在机器
MSG_HEADER_KEY='pbmt1_…' pb-mapper register tcp --key web --addr 127.0.0.1:8080 --server relay.example.com:7666
# 需要访问的机器
MSG_HEADER_KEY='pbmt1_…' pb-mapper connect tcp --key web --addr 127.0.0.1:9090 --server relay.example.com:7666
```

现在访问 `127.0.0.1:9090` 就是访问那项服务。`pb-mapper status sessions` 可以查看每条连接走的是哪条路径。

## 连接如何选择路径

以下条件全部满足时走直连，前三条都是默认值：

1. relay 提供打洞服务（默认开启，使用与 TCP 相同的端口号）；
2. 提供端允许直连（`register` 默认允许）；
3. 消费端请求直连（`connect` 默认 `--path auto`）；
4. 两端网络确实能打通。

任一条件不满足就走中继。切换发生在**提供端打开本地服务之前、任何业务字节发送之前**，因此不会重放任何数据，后端也只会被拨号一次。回退之后，这条隧道接下来的连接会直接走中继一段时间（10 秒起，翻倍增长到 5 分钟；直连成功或网络切换时清零），所以打不通的网络只付出一次尝试的代价，而不是每条连接都等一次。

需要时可以在任意一侧关闭直连：

| 位置 | 参数 | 效果 |
| --- | --- | --- |
| Relay | `pb-mapper server --relay-only` | 不开 UDP 端口，所有连接走中继 |
| 提供端 | `pb-mapper register --relay-only …` | 这项服务只能经中继访问 |
| 消费端 | `pb-mapper connect --relay-only …` | 这条隧道始终走中继 |

relay 只公布 UDP 端口号，客户端把它和自己拨通 relay 时用的地址配对，所以 1:1 NAT、端口转发都不用额外配置。如果 UDP 端口被映射到别处，用 `--direct-advertise 公网IP:端口` 指定；用 `--direct-listen` 绑定其他本地地址。

同一局域网内的直连默认走 QUIC，消费端也可以选择 TCP（`connect --lan-transport tcp`，或应用里的「设置 → 直连 → 局域网传输」）：先局域网 TCP，再局域网 QUIC，再互联网 QUIC，最后中继。提供端按需提供 TCP，而且只提交局域网地址（`register --no-lan-tcp` 可拒绝），所以打洞的互联网路径始终是 QUIC。TCP 需要提供端防火墙允许入站 TCP；不通时会话在 300 ms 内改走局域网 QUIC。

需要了解的边界：

- **直连会话归两端所有。** relay 重启或凭据吊销只阻止新的建连，不会切断已建立的直连；停止本地隧道才会关闭它。中继连接则在吊销时立即关闭。
- **并非所有网络都能打通。** 对称 NAT、UDP 被封锁等情况走中继。已测试的场景见[设计说明](docs/issue-13-direct-connect-design.zh-CN.md)与[验收记录](docs/issue-13-validation.zh-CN.md)。
- **pb-mapper 只负责传输字节。** 隧道后面的服务仍然负责自己的认证与授权。

## SDK

### Rust

```toml
[dependencies]
pb-mapper = "0.11"
tokio = { version = "1", features = ["full"] }
```

```rust
use pb_mapper::{Client, ClientConfig, ConnectRequest, RegisterRequest, Transport};

let client = Client::new(ClientConfig {
    server: "relay.example.com:7666".into(),
    credential: std::env::var("MSG_HEADER_KEY")?,
    keep_alive: true,
    namespace: None,
})?;

// 设备端：发布本地服务
let registration = client.register(RegisterRequest {
    key: "web".into(),
    local_addr: "127.0.0.1:8080".into(),
    transport: Transport::Tcp,
    codec: false,
    force_namespace: false,
}).await?;
registration.wait_ready().await?;

// 任意位置：在本地端口访问它，直连优先
let connection = client.connect(ConnectRequest {
    key: "web".into(),
    local_addr: "127.0.0.1:9090".into(),
    transport: Transport::Tcp,
}).await?;
connection.wait_ready().await?;

// 监听状态变化：Starting → Connected → Retrying → …
let mut status = connection.subscribe();
while status.changed().await.is_ok() {
    println!("{:?}", *status.borrow());
}
```

每项能力都有可运行的示例，见 [`crates/pb-mapper/examples/`](crates/pb-mapper/examples)。建议从 `cargo run -p pb-mapper --example quickstart` 开始，它不需要你自己的 relay：

| 示例 | 内容 |
| --- | --- |
| [`quickstart`](crates/pb-mapper/examples/quickstart.rs) | 内嵌 relay、发布 echo 服务、直连往返，并打印实际路径 |
| [`register_tcp`](crates/pb-mapper/examples/register_tcp.rs) / [`connect_tcp`](crates/pb-mapper/examples/connect_tcp.rs) | TCP 隧道两端，实时显示直连与中继的连接数 |
| [`udp`](crates/pb-mapper/examples/udp.rs) | UDP 隧道两端，例如游戏服务器 |
| [`path_options`](crates/pb-mapper/examples/path_options.rs) | `allow_direct: false`、`PathPolicy::RelayOnly`、空闲超时 |
| [`watch_events`](crates/pb-mapper/examples/watch_events.rs) | 状态流、事件历史、直连实时阶段、JSON 诊断 |
| [`status`](crates/pb-mapper/examples/status.rs) | 命名空间内的服务、控制连接与会话 |
| [`admin_credentials`](crates/pb-mapper/examples/admin_credentials.rs) | 签发、查看、显示密钥、续期、列出、吊销、回收凭据 |
| [`admin_inventory`](crates/pb-mapper/examples/admin_inventory.rs) | 所有租户的服务与连接、清理卡住的连接、轮换根密钥 |
| [`remote_control`](crates/pb-mapper/examples/remote_control.rs) | agent harness 模式：每个会话一把凭据、相互隔离、结束时吊销 |

关闭 `p2p` feature（`default-features = false`）可得到不含 QUIC 的纯中继客户端。

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

const conn = await client.connect({
  key: "web", localAddr: "127.0.0.1:9090", transport: "tcp",
});
await conn.waitReady();

// 事件监听：每次变化 resolve 一次，隧道终止后循环结束
for (let s = conn.status(); s !== "stopped" && !s.startsWith("failed");) {
  s = await conn.nextStatus();
  console.log(s, conn.diagnostics().events.at(-1));
}
```

预编译覆盖 Linux（x64、arm64，glibc）、macOS（x64、arm64）与 Windows x64，支持 Node.js 18+ 与 Bun 1.1+。完整 API 见 [`js/README.md`](js/README.md)。

### 其他接入面

| 接入面 | 能力 |
| --- | --- |
| CLI `pb-mapper` | 一个二进制包含 `server`、`register`、`connect`、`status`、`admin` |
| Agent Skill | 端到端安装并验收 relay、register 与 connect（见下文） |
| Flutter 应用 | 基于 C ABI，覆盖 Windows、macOS、Linux、Android 与 iOS：隧道与 CLI 走同样的路径，还能管理中继上的服务与凭据，并使用远程桌面 |
| Relay 库 | `pb-mapper-server`，可把 relay 内嵌进自己的进程 |

## 远程桌面

共享一台电脑的屏幕，并从另一台设备操控它，用的是隧道已有的中继、凭据和路径；中继不需要任何新东西。

| 角色 | 目前支持 |
| --- | --- |
| 主机 | 装有 Media Foundation 的 Windows：GPU H.265 或 H.264；普通桌面，或经可选的辅助服务连同 UAC 提示和本人的锁屏 |
| 应用里的查看端 | Windows 与 Android 原生解码 H.265/H.264；macOS、Linux 和 iOS（尚未验证）以 JPEG 显示画面，最高 8 FPS |
| SDK 里的查看端 | 任何系统，解码器由你提供 |

**在应用里**：在 Windows 上打开**远程桌面**，选择**开启远程控制**（或**只共享普通桌面**，不安装辅助服务）。私下复制 `pbd1.` 邀请，或把二维码给手机扫：持有邀请的人都能查看，操控是主机另外授予的权限。在另一台设备上选择**添加电脑**（手机上是**扫码添加**或**粘贴邀请**），再点**连接**。查看器的**显示**菜单（手机上在**更多**里）可以设置分辨率、最高帧率和编解码器，打开质量监测（速度、帧率、延迟、码率、编解码器、分辨率），在桌面端操控 Windows 电脑时，还能把对方的分辨率改成**适配本机**。

**在你的应用里：**

```toml
pb-mapper = { version = "0.11", features = ["desktop"] }
```

```rust
use std::time::Duration;

use pb_mapper::desktop::{
    Button, Decoders, Host, HostOptions, Input, Invitation, Pull, Resolution, Viewer, ViewerConfig,
};
use pb_mapper::ConnectOptions;

// 主机（Windows）：共享主显示器，并允许操控
let mut options = HostOptions::new("desktop:office");
options.allow_control = true;
let host = Host::builder(options).start(&client).await?;
let invitation = host.invitation().encode(); // pbd1.…：务必保密

// 查看端（任何系统）：只提供你的解码器能解的；默认用远程显示器的原始分辨率
let decoders = Decoders { h264: Some((3840, 2160)), h265: None };
let config = ViewerConfig::for_decoders(decoders, Resolution::default(), 60.0, None);
let invitation = Invitation::decode(&invitation)?;
let viewer = Viewer::connect(&client, &invitation, ConnectOptions::default(), config).await?;

// 画面：Annex B 访问单元，在独立线程上交给你的解码器
let consumer = viewer.video_sink().attach()?;
std::thread::spawn(move || {
    let mut capacity = 1 << 20;
    loop {
        match consumer.next(Duration::from_millis(100), capacity) {
            Pull::Unit(unit) => {
                // 解码 unit.bytes（unit.codec、unit.keyframe、unit.pts_us）并显示，然后：
                consumer.presented(unit.generation, unit.frame_id);
            }
            Pull::NeedCapacity(needed) => capacity = needed,
            Pull::Closed => break,
            Pull::Timeout | Pull::Paused => {}
        }
    }
});

// 键盘鼠标，按主机显示器的像素坐标发送。输入需要已呈现的画面和操控权，
// 操控权由主机异步授予（前提是主机允许操控）
while !viewer.status().fresh {
    tokio::time::sleep(Duration::from_millis(10)).await;
}
viewer.request_control(true)?;
while !viewer.status().controlling {
    tokio::time::sleep(Duration::from_millis(10)).await;
}
let view = viewer.view();
if let Some(point) = view.display_point(0.5, 0.5) {
    let click = |down| Input::Button { button: Button::Left, down, drag_start: false };
    viewer.input(view.generation, &[point, click(true), click(false)])?;
}
```

`Host::watch` 和 `Viewer::watch` 提供状态流，`supervisor::Supervisor` 让查看端保持连接，`trust::TrustFile` 让主机的邀请在重启后依然有效。六个示例覆盖了这些用法；`cargo run -p pb-mapper --features desktop --example desktop_loopback` 在任意系统上于一个进程内跑起 relay、主机和查看端。API 与解码方法见 SDK 的 [Remote desktop 一节](crates/pb-mapper/README.md#remote-desktop)，辅助服务、视频管线与实测数据见 [docs/remote-desktop.md](docs/remote-desktop.md)。

暂不支持：macOS、Linux 或手机作主机（主机已按平台拆分，为它们留好了位置）、音频、剪贴板、文件传输，以及同时多个查看端。辅助服务在 Windows 默认安全桌面策略下的 UAC 验收，以及锁屏时输入密码的验收，都还未完成。

## 凭据与安全

| 凭据 | 持有者 | 权限 |
| --- | --- | --- |
| 管理员密钥（32 字节） | Relay 运维者或可信的凭据分发自动化 | 签发、查看、续期、吊销、轮换；检查所有命名空间 |
| 临时 `pbmt1_` 凭据 | 单个租户、设备、harness 或会话 | 只能在自己的命名空间内注册、连接和查询 |

- 临时凭据由管理员密钥、持久化的 relay 实例 ID 和 key ID 派生，relay 只保存生命周期元数据，不保存密钥本身。过期、吊销与根密钥轮换会立即作用于现有中继连接。
- V2 协议在加密的首个请求内完成认证，不额外增加往返。直连会话运行在 QUIC 上，双方证书互相固定并绑定到会话授权。`register --codec` 为中继数据增加 AES-256-GCM 加密。
- 凭据是预共享的，不是公钥身份体系。如需证书身份或对流量分析的防护，请在其上叠加 TLS 或其他应用协议。详见[认证设计](docs/authentication-v2.zh-CN.md)与[数据加密兼容性](docs/data-encryption-compatibility.md)。

## 部署

- **Agent Skill**：直接告诉你的编码 agent
  `Fetch and follow instructions from https://raw.githubusercontent.com/acking-you/pb-mapper/master/skills/pb-mapper-suite/INSTALL.md`，
  它会安装 CLI、部署 relay、配置 register/connect 并完成验收。
- **Docker**：`docker compose -f docker/docker-compose.yml up -d` 同时发布 `7666/tcp` 与 `7666/udp`，见 [`DOCKER_README.md`](DOCKER_README.md)。
- **systemd**：relay、register、connect 的 unit 文件在 [`services/`](services/readme.md)。
- **预编译二进制**：Linux、macOS、Windows 的 CLI 与 UI 见 [Releases](https://github.com/acking-you/pb-mapper/releases)。

## 从 0.7 升级

现在所有位置的默认值都是「直连优先、中继兜底」。原有命令继续可用：`--enable-direct`、`--allow-direct`、`--path auto` 仍然被接受，连接旧版 relay 的客户端会直接走中继。想保持 0.7 的行为，就给 `server`、`register` 或 `connect` 加上 `--relay-only`。SDK 中，Node 设 `allowDirect: false` / `path: "relay-only"`，Rust 用 `RegisterOptions { allow_direct: false, .. }` / `PathPolicy::RelayOnly`。0.7 客户端不会把公布的端口号与 relay 地址配对，因此即使开启了直连，连接默认配置的 0.8 relay 也会回退中继；升级客户端，或用 `--direct-advertise 公网IP:7666` 启动 relay 即可。

## 文档

- [可选 QUIC 中继](docs/relay-quic.zh-CN.md)：默认 TCP、CLI 开关、Rust/Node SDK、可观测接口与性能实测
- [使用手册](docs/user-guide.zh-CN.md)：所有命令与参数
- [远程桌面](docs/remote-desktop.md)（英文）：应用、Windows 辅助服务、SDK、视频管线、实测与限制
- [弱网恢复](docs/weak-network-recovery.md)：超时、资源边界与故障测试
- [认证与 V2 协议](docs/authentication-v2.zh-CN.md)
- [直连设计说明](docs/issue-13-direct-connect-design.zh-CN.md)
- [更新日志](CHANGELOG.md)

仓库结构：`crates/` 是 Rust workspace（core、auth、protocol、server、client、远程桌面、`pb-mapper` SDK 门面、Node-API 绑定、CLI 与 testkit）；`js/` 是 npm 包；`ui/` 是 Flutter 应用及其 C ABI；`skills/` 是 agent 工作流；`docker/`、`services/`、`scripts/` 是打包与运维工具。

```bash
make build-pb-mapper
cargo test --workspace --all-features
```

## 许可证

[MIT](LICENSE)
