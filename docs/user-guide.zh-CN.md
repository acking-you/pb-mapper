# pb-mapper 使用手册

[English](user-guide.md) | [中文](user-guide.zh-CN.md)

## 概览

pb-mapper 通过“服务 key”将本地 TCP/UDP 服务暴露到公网服务器，提供三款 CLI 二进制与可选的 Flutter GUI。

## 环境准备

- 可选：Flutter SDK（用于 `ui/` 图形界面）
- 可选：Docker/Compose（容器部署见 `DOCKER_README.md`）

## 安装（推荐）

从 GitHub Releases 下载预编译二进制并解压：

- Releases：https://github.com/acking-you/pb-mapper/releases

每个二进制单独打包：

- `pb-mapper-server-<version>-<target>.tar.gz` / `.zip`
- `pb-mapper-server-cli-<version>-<target>.tar.gz` / `.zip`
- `pb-mapper-client-cli-<version>-<target>.tar.gz` / `.zip`

解压后添加到 PATH 或从解压目录直接运行。

## 从源码编译（可选）

### Rust 二进制

需要 Rust 工具链（版本以 `rust-toolchain.toml` 为准）。

编译所有 Rust 二进制：

```bash
cargo build --release
```

仅编译服务器（Makefile）：

```bash
make build-pb-mapper-server
```

交叉编译 musl 服务器：

```bash
make build-pb-mapper-server-x86_64_musl
```

二进制产物位于 `target/release/`（例如 `pb-mapper-server`）。

### Flutter UI（可选）

```bash
cd ui
flutter run
```

## 运行（CLI）

如已加入 PATH 可直接运行，否则前面加 `./`。

### 1）启动中心服务器

```bash
pb-mapper-server --pb-mapper-port 7666
```

可选参数：

- `--use-ipv6`：开启 IPv6 监听
- `--keep-alive`：开启 TCP keep-alive
- `--use-machine-msg-header-key`：基于当前机器 hostname + MAC 派生 `MSG_HEADER_KEY`，
  并写入 `/var/lib/pb-mapper-server/msg_header_key`

### 基于机器信息派生 `MSG_HEADER_KEY`（可选）

如果你希望每台部署机器都使用各自唯一的 key（而不是内置默认 key），可以这样启动服务端：

```bash
pb-mapper-server --pb-mapper-port 7666 --use-machine-msg-header-key
```

该参数会完成：

- 基于 hostname + MAC 地址派生稳定的 32 字节 key
- 自动设置当前服务端进程的 `MSG_HEADER_KEY`
- 将 key 持久化到 `/var/lib/pb-mapper-server/msg_header_key`

随后在 `pb-mapper-server-cli` / `pb-mapper-client-cli` 中使用同一 key：

```bash
export MSG_HEADER_KEY="$(cat /var/lib/pb-mapper-server/msg_header_key)"
pb-mapper-server-cli --pb-mapper-server "your-server:7666" tcp-server --key "my-service" --addr "127.0.0.1:8080"
```

### 2）注册本地服务

注册 TCP 服务：

```bash
pb-mapper-server-cli --pb-mapper-server "your-server:7666" \
  tcp-server \
  --key "my-service" \
  --addr "127.0.0.1:8080"
```

注册 UDP 服务：

```bash
pb-mapper-server-cli --pb-mapper-server "your-server:7666" \
  udp-server \
  --key "my-udp" \
  --addr "127.0.0.1:8211"
```

如需启用 AES-256-GCM 的转发消息加密，请在子命令之前加入 `--codec`（例如：`pb-mapper-server-cli --codec tcp-server ...`）。

### 3）远程客户端连接

```bash
pb-mapper-client-cli --pb-mapper-server "your-server:7666" \
  tcp-server \
  --key "my-service" \
  --addr "127.0.0.1:9090"
```

完成后，远程机器可通过 `127.0.0.1:9090` 访问目标服务。

### 状态命令

```bash
pb-mapper-server-cli --pb-mapper-server "your-server:7666" status remote-id
pb-mapper-server-cli --pb-mapper-server "your-server:7666" status keys
```

## 运行（GUI）

Flutter UI 可用于启动服务器、注册服务与建立连接。启动方式：

```bash
cd ui
flutter run
```

## 环境变量

- `PB_MAPPER_SERVER`：CLI 默认服务器地址
- `PB_MAPPER_KEEP_ALIVE`：启用 TCP keep-alive（设置为 `ON`）
- `RUST_LOG`：日志级别，例如 `info` 或 `debug`

## Docker 部署

服务器容器部署请见 [`DOCKER_README.md`](../DOCKER_README.md)。
