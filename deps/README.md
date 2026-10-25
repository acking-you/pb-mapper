# 第三方源码维护

Issue #13 新增直接依赖使用自己的 GitHub fork 和 submodule，维护分支统一为 `codex/pb-mapper-issue-13`。已有依赖和新增传递依赖继续由 Cargo 管理；同一上游仓库内的配套 crate 自然包含在该 submodule 中。

三个库已接入产品 Cargo manifest 和 Cargo.lock，通过 path + version 构建，p2p 功能实际使用这些 fork。现有 `uni-stream`、`kanal` 的接入方式未调整。

## 固定版本

| 本地目录 | fork | 上游 / 基线 tag | 固定 commit | 许可证 |
|---|---|---|---|---|
| `deps/quinn` | [acking-you/quinn](https://github.com/acking-you/quinn/tree/codex/pb-mapper-issue-13) | [quinn-rs/quinn](https://github.com/quinn-rs/quinn), `quinn-proto-0.11.19` | `70135460fecf8dae7597b00c8ede3f15d69a36f8` | MIT OR Apache-2.0 |
| `deps/rcgen` | [acking-you/rcgen](https://github.com/acking-you/rcgen/tree/codex/pb-mapper-issue-13) | [rustls/rcgen](https://github.com/rustls/rcgen), `v0.14.10` | `f4a3b165c467c070d5c74b86acc2c3f0a0be10ab` | MIT OR Apache-2.0 |
| `deps/stun-proto` | [acking-you/stun-proto](https://github.com/acking-you/stun-proto/tree/codex/pb-mapper-issue-13) | [ystreet/stun-proto](https://github.com/ystreet/stun-proto), `v2.0.3` | `0c465c121ae42ced958c5096740329edec46522b` | MIT OR Apache-2.0 |

三个 fork 的固定提交均已推送。Quinn 在稳定 tag 上增加了 Windows IPv4 源地址提示的兼容修复（[PR #1](https://github.com/acking-you/quinn/pull/1)，提交先发布于 `codex/windows-pktinfo-fallback`，目标维护分支不变）；其余两个库没有源码补丁。Quinn 仓库包含 `quinn 0.11.12`、`quinn-proto 0.11.19`、`quinn-udp 0.5.16`，选用该 tag 是为了纳入 0.11 系列后续修复。stun-proto 仓库包含配套 `stun-types`，使用其 re-export，无需额外直接依赖。

rustls 作为 Quinn 的传递依赖由 Cargo 管理，通过 `quinn::rustls` 使用，不单独声明直接依赖、添加 patch 或维护 submodule。iroh 保留为架构参考；成熟度评估及具体复用边界见 [设计方案](../docs/issue-13-direct-connect-design.zh-CN.md#3-传输选型与依赖)。此前比较的 rustls、iroh、webrtc 已不在本项目 submodule 中，远端 fork 保留。

## 初始化与直接修改

从主项目根目录运行以下命令，构建始终使用主仓库记录的 gitlink：

```sh
git submodule update --init deps/quinn deps/rcgen deps/stun-proto
```

普通 submodule 初始化会进入 detached HEAD，即使 `.gitmodules` 配了 `branch`。需要修改源码时显式切换维护分支，以 Quinn 为例：

```sh
cd deps/quinn
git fetch origin
git switch codex/pb-mapper-issue-13
# 新克隆没有本地分支时：
# git switch --track origin/codex/pb-mapper-issue-13
git remote -v
# 新克隆没有 upstream 时：
# git remote add upstream https://github.com/quinn-rs/quinn.git
```

当前本地三个 submodule 已有 `upstream` remote；remote 配置不会随 gitlink 传播，新机器按表中地址添加。维护时先检查工作区与分支提交，再修改。完成上游要求的格式检查及相关测试后，先提交、推送 fork，再更新主仓库 gitlink：

```sh
# 在 deps/quinn 中，完成检查并提交修改后：
git push origin codex/pb-mapper-issue-13
cd ../..
git add deps/quinn
# 随主项目变更提交 gitlink，并记录补丁原因及上游 issue/PR。
```

更新上游使用 `git fetch upstream` 后在维护分支合并明确的 tag/commit，处理冲突并验证；不要让 CI 使用 `git submodule update --remote` 自动漂移，也不要对已共享维护分支做不必要的 force push。修改发布前必须确保 gitlink 指向 fork 中可获取的提交。

stun-proto 社区规模较小，保留 fork 的直接修复能力，并跟踪上游版本和维护情况；升级需验证事务、重传、来源校验与共享 socket 的适配，不按 stars 自动替换协议实现。

## Cargo 接入

**根 Cargo.toml** 已将三个仓库加入 `[workspace].exclude`，防止其内部 workspace 成员被主项目误纳入；`[workspace.dependencies]` 中使用：

```toml
quinn = { path = "deps/quinn/quinn", version = "0.11.12", default-features = false, features = ["runtime-tokio", "rustls-ring"] }
rcgen = { path = "deps/rcgen/rcgen", version = "0.14.10", default-features = false, features = ["ring"] }
stun-proto = { path = "deps/stun-proto/stun-proto", version = "2.0.3" }
```

实际使用它们的 crate 再声明 `quinn.workspace = true` 等依赖，并归入默认关闭的 `p2p` feature。运行时仍须显式启用 `auto`。TLS 配置代码使用 `use quinn::rustls;`，与 Quinn 内部使用同一个 rustls 包；不声明 `rustls.workspace` 或 `[patch.crates-io].rustls`。Quinn 内部 proto/udp 和 stun-proto 内部 stun-types 已有 path 关系，无须为这些传递 crate 再 fork 一个仓库。

通过 `cargo metadata`/`cargo tree` 检查三个直接依赖 source 为本地路径、rustls 由 registry 解析且只有预期的一份、没有意外启用 aws-lc 或系统证书验证。依赖锁定后运行受影响测试，CI 已把 `deps/*` 和 `.gitmodules` 纳入 Rust/Node 变更触发范围；仅调整 gitlink 也必须验收。

以上 path 方案适用于本仓库构建二进制。**发布 crates.io SDK 需要另行完成依赖发布闭环**：根 `[patch.crates-io]` 不会作为库的依赖配置传播给使用者；`path + version` 在发布时会解析 registry 版本。若 fork 有行为修改，应先合入上游并依赖包含修复的版本，或发布明确命名的 fork 包并统一改依赖，不能把尚未发布的本地修复当作下游已经获得。当前没有发布任何第三方 crate；Quinn 的 Windows 修复仅由固定 Git/submodule 依赖提供，不应宣称 crates.io 用户已获得该补丁。

## 已执行的检查

独立验证项目位于主项目同级 `pb-mapper-references/dependency-check/`，使用 Rust 1.99.1 和上述三个 submodule 路径，不单独声明或 patch rustls。它通过 `quinn::rustls` 构造 TLS 1.3 配置，将 rcgen 证书交给 Quinn QUIC 配置，并完成 STUN Binding 请求、响应及 XOR-MAPPED-ADDRESS 编码/解析。`cargo metadata --locked` 确认三个直接依赖及同仓配套 crate 来自本地路径，rustls 只有 registry 的 0.23.46 一份，未解析 iroh 或 WebRTC 栈。

该检查没有启动数据连接；其中无客户端认证的配置仅用于验证库接口，不能用于正式直连实现。正式实现必须按 [设计方案](../docs/issue-13-direct-connect-design.zh-CN.md) 验证双方身份。依赖 smoke check 不代表 NAT 穿透、吞吐、平台兼容或完整产品测试通过。

另在 `deps/stun-proto` 执行 `cargo +1.99.1 test -p stun-proto --lib --locked`，66 个上游库测试通过，覆盖 agent、认证及统计逻辑；没有改动第三方源码。

实际直连与中继验收见 [测试记录](../docs/issue-13-validation.zh-CN.md)。
