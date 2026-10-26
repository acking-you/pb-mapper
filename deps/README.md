# 第三方源码维护

`deps/` 下的五个仓库统一使用 `dev` 作为维护分支和 GitHub 默认分支，`.gitmodules` 也指向 `dev`。Quinn、rcgen、stun-proto 和 Kanal 是 fork，另保留 `upstream` 分支跟踪纯上游源码；uni-stream 是自研仓库，没有外部上游或 `upstream` 分支。

Quinn、rcgen、stun-proto 通过 `path + version` 接入 Cargo，构建使用子模块源码。Kanal 和 uni-stream 的子模块用于源码维护，产品仍使用 registry 中的 `kanal-plus 0.2.0` 和 `uni-stream 0.3.5`；切换维护分支不会改变已发布包，也不会自动升级产品的依赖。

## 固定版本

| 本地目录 | 维护仓库 | 上游 / 基线 | 固定 commit | 许可证 |
|---|---|---|---|---|
| `deps/quinn` | [acking-you/quinn](https://github.com/acking-you/quinn/tree/dev) | [quinn-rs/quinn](https://github.com/quinn-rs/quinn), `quinn-proto-0.11.19` | `6cbaeaaa10182068f086239b62e8274db00ff08e` | MIT OR Apache-2.0 |
| `deps/rcgen` | [acking-you/rcgen](https://github.com/acking-you/rcgen/tree/dev) | [rustls/rcgen](https://github.com/rustls/rcgen), `v0.14.10` | `fd4687899528c7651ea0b0f433efd528e07daae5` | MIT OR Apache-2.0 |
| `deps/stun-proto` | [acking-you/stun-proto](https://github.com/acking-you/stun-proto/tree/dev) | [ystreet/stun-proto](https://github.com/ystreet/stun-proto), `v2.0.3` | `01028487efb43cc17262ed94a59a58b01bd73696` | MIT OR Apache-2.0 |
| `deps/kanal` | [acking-you/kanal](https://github.com/acking-you/kanal/tree/dev) | [fereidani/kanal](https://github.com/fereidani/kanal)，本地扩展发布为 `kanal-plus 0.2.0` | `e79d95d5163446c23d53befbc778aaf6d7d34e42` | MIT |
| `deps/uni-stream` | [acking-you/uni-stream](https://github.com/acking-you/uni-stream/tree/dev) | 自研，`0.3.5` 发布后的已合并维护修复 | `796efc8c938e0eedb5006c2512fc5cf810fbbca4` | MIT |

固定提交均须先推送到各自仓库，再更新 pb-mapper 的 gitlink。Quinn 在稳定 tag 上增加了 Windows IPv4 源地址提示的兼容修复（[PR #1](https://github.com/acking-you/quinn/pull/1)，由 `dev` 完整继承）；rcgen 和 stun-proto 保留原稳定 tag 的库源码。Quinn 仓库包含 `quinn 0.11.12`、`quinn-proto 0.11.19`、`quinn-udp 0.5.16`，选用该 tag 是为了纳入 0.11 系列后续修复。stun-proto 仓库包含配套 `stun-types`，使用其 re-export，无需额外直接依赖。

各 fork 的 `dev` 用于后续开发和直接修复；`upstream` 由原来未加本地补丁的 `main` 改名，只跟踪上游源码。分支改名不会自动同步上游，也不会把上游 `main` 的新版本引入当前稳定基线。需要贡献上游时，从合适的上游目标分支建立独立分支，只挑选相关修复，再向对应上游仓库提 PR。

Quinn、rcgen 和 stun-proto 的迁移只调整工作流分支条件，保留原固定版本的库源码。Kanal 的 `dev` 继承原 `dev/pb-mapper` 上已发布的 `kanal-plus 0.2.0`，保留批量接收和 owned stream 扩展。uni-stream 的 `dev` 继承原 `master` 上已合并的 DNS 和 MSRV 修复；当前 registry 的 `uni-stream 0.3.5` 来自 `c041f85`，这些后续维护提交尚不能视为该发布包的一部分。

已合并的工作分支及纯上游分支副本已清理。Kanal 和 uni-stream 各保留一个 `codex/cipherrelay-v1.2.22-deps` 分支：它们含有独立未合并的依赖调整，不能作为无用分支删除，也没有混入 pb-mapper 的维护基线。

rustls 作为 Quinn 的传递依赖由 Cargo 管理，通过 `quinn::rustls` 使用，不单独声明直接依赖、添加 patch 或维护 submodule。iroh 保留为架构参考；成熟度评估及具体复用边界见 [设计方案](../docs/issue-13-direct-connect-design.zh-CN.md#3-传输选型与依赖)。此前比较的 rustls、iroh、webrtc 已不在本项目 submodule 中，远端 fork 保留。

## 初始化与直接修改

从主项目根目录运行以下命令，构建始终使用主仓库记录的 gitlink：

```sh
git submodule update --init deps/uni-stream deps/kanal deps/quinn deps/rcgen deps/stun-proto
```

普通 submodule 初始化会进入 detached HEAD，即使 `.gitmodules` 配了 `branch`。需要修改源码时显式切换维护分支，以 Quinn 为例：

```sh
cd deps/quinn
git fetch origin
git switch dev
# 新克隆没有本地分支时：
# git switch --track origin/dev
git remote -v
# 新克隆没有 upstream 时：
# git remote add upstream https://github.com/quinn-rs/quinn.git
```

四个 fork 使用 `upstream` remote 指向表中的外部上游；uni-stream 只需要 `origin`。remote 配置不会随 gitlink 传播，新机器按表中地址添加。维护时先检查工作区与分支提交，再修改。完成格式检查及相关测试后，先提交、推送依赖仓库，再更新主仓库 gitlink：

```sh
# 在 deps/quinn 中，完成检查并提交修改后：
git push origin dev
cd ../..
git add deps/quinn
# 随主项目变更提交 gitlink，并记录补丁原因及上游 issue/PR。
```

`upstream` 分支与名为 `upstream` 的 remote 是不同概念：`origin/upstream` 是 fork 中保留的上游分支，`upstream/main` 是外部上游的主分支。同步纯上游分支时运行（不适用于 uni-stream）：

```sh
git fetch upstream
git switch upstream
git merge --ff-only upstream/main
git push origin upstream
git switch dev
```

更新产品依赖时，再在维护分支合并明确的上游 tag/commit，处理冲突并验证；不要让 CI 使用 `git submodule update --remote` 自动漂移，也不要对已共享维护分支做不必要的 force push。修改发布前必须确保 gitlink 指向 fork 中可获取的提交。

stun-proto 社区规模较小，保留 fork 的直接修复能力，并跟踪上游版本和维护情况；升级需验证事务、重传、来源校验与共享 socket 的适配，不按 stars 自动替换协议实现。

## Cargo 接入

**根 Cargo.toml** 已将五个仓库加入 `[workspace].exclude`，防止依赖仓库的成员被主项目误纳入；`[workspace.dependencies]` 中使用：

```toml
quinn = { path = "deps/quinn/quinn", version = "0.11.12", default-features = false, features = ["runtime-tokio", "rustls-ring"] }
rcgen = { path = "deps/rcgen/rcgen", version = "0.14.10", default-features = false, features = ["ring"] }
stun-proto = { path = "deps/stun-proto/stun-proto", version = "2.0.3" }
kanal = { package = "kanal-plus", version = "0.2.0" }
uni-stream = "0.3.5"
```

实际使用它们的 crate 再声明 `quinn.workspace = true` 等依赖。直接连接相关的三个 path 依赖由 `p2p` feature 管理；当前产品默认尝试直连，并在提交前失败时回退中继。TLS 配置代码使用 `use quinn::rustls;`，与 Quinn 内部使用同一个 rustls 包；不声明 `rustls.workspace` 或 `[patch.crates-io].rustls`。Quinn 内部 proto/udp 和 stun-proto 内部 stun-types 已有 path 关系，无须为这些传递 crate 再 fork 一个仓库。

通过 `cargo metadata`/`cargo tree` 检查三个直接依赖 source 为本地路径、rustls 由 registry 解析且只有预期的一份、没有意外启用 aws-lc 或系统证书验证。依赖锁定后运行受影响测试，CI 已把 `deps/*` 和 `.gitmodules` 纳入 Rust/Node 变更触发范围；仅调整 gitlink 也必须验收。

以上 path 方案适用于本仓库构建二进制。**发布 crates.io SDK 需要另行完成依赖发布闭环**：根 `[patch.crates-io]` 不会作为库的依赖配置传播给使用者；`path + version` 在发布时会解析 registry 版本。若 fork 有行为修改，应先合入上游并依赖包含修复的版本，或发布明确命名的 fork 包并统一改依赖，不能把尚未发布的本地修复当作下游已经获得。Kanal 已通过 `kanal-plus 0.2.0` 发布；Quinn、rcgen 和 stun-proto 没有发布单独命名的 fork 包。Quinn 的 Windows 修复仅由固定 Git/submodule 依赖提供，不应宣称 crates.io 用户已获得该补丁。

## 已执行的检查

2026-10-26 分支迁移在 macOS arm64、Rust 1.99.1、Bun 1.4.0 上验证：

- 五个子模块的 gitlink 均与各仓库远端 `dev` 一致，工作区干净；`.gitmodules` 和本地分支配置均指向 `dev`。
- 主仓库格式检查、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、`cargo test --workspace --all-features --locked` 全部通过。
- Node release 构建、12 个 JS 测试和原生模块体积检查通过；3 个发布包边界测试通过。Cargo.toml 和 Cargo.lock 未改变。
- 各子模块格式检查通过。Quinn、uni-stream 的严格 Clippy 通过；stun-proto / stun-types 的库目标严格 Clippy 通过。
- 额外的依赖独立检查仍存在原有告警：rcgen 的 `chunks_exact_to_as_chunks`、Kanal 的 `needless_borrow`，以及 stun-proto 基准测试调用已弃用的 `handle_stun_message`。这些检查在 `-D warnings` 下未通过；迁移没有修改相关源码，也没有降低仓库的检查要求。

以下为先前的依赖接入验证记录：

独立验证项目位于主项目同级 `pb-mapper-references/dependency-check/`，使用 Rust 1.99.1 和上述三个 submodule 路径，不单独声明或 patch rustls。它通过 `quinn::rustls` 构造 TLS 1.3 配置，将 rcgen 证书交给 Quinn QUIC 配置，并完成 STUN Binding 请求、响应及 XOR-MAPPED-ADDRESS 编码/解析。`cargo metadata --locked` 确认三个直接依赖及同仓配套 crate 来自本地路径，rustls 只有 registry 的 0.23.46 一份，未解析 iroh 或 WebRTC 栈。

该检查没有启动数据连接；其中无客户端认证的配置仅用于验证库接口，不能用于正式直连实现。正式实现必须按 [设计方案](../docs/issue-13-direct-connect-design.zh-CN.md) 验证双方身份。依赖 smoke check 不代表 NAT 穿透、吞吐、平台兼容或完整产品测试通过。

另在 `deps/stun-proto` 执行 `cargo +1.99.1 test -p stun-proto --lib --locked`，66 个上游库测试通过，覆盖 agent、认证及统计逻辑；没有改动第三方源码。

实际直连与中继验收见 [测试记录](../docs/issue-13-validation.zh-CN.md)。
