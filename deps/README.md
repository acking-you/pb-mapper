# 第三方源码维护

`deps/` 下的五个仓库统一使用 `dev` 作为维护分支和 GitHub 默认分支，`.gitmodules` 也指向 `dev`。Quinn、rcgen、stun-proto 和 Kanal 是 fork，另保留 `upstream` 分支跟踪纯上游源码；uni-stream 是自研仓库，没有外部上游或 `upstream` 分支。

Quinn、rcgen、stun-proto 通过 `path + version` 接入 Cargo，构建使用子模块源码。Kanal 和 uni-stream 的子模块用于源码维护，产品仍使用 registry 中的 `kanal-plus 0.2.0` 和 `uni-stream 0.3.5`；切换维护分支不会改变已发布包，也不会自动升级产品的依赖。

## 固定版本

| 本地目录 | 维护仓库 | 上游 / 基线 | 固定 commit | 许可证 |
|---|---|---|---|---|
| `deps/quinn` | [acking-you/quinn](https://github.com/acking-you/quinn/tree/dev) | [quinn-rs/quinn](https://github.com/quinn-rs/quinn), `main@12fe8094`（开发版） | `91a87cdb65f8ccec42eaa83b6323ae3ea42a7f38` | MIT OR Apache-2.0 |
| `deps/rcgen` | [acking-you/rcgen](https://github.com/acking-you/rcgen/tree/dev) | [rustls/rcgen](https://github.com/rustls/rcgen), `main@b820c438`（开发版） | `b4a65cb06038656800b38fe592923b06ad0df77a` | MIT OR Apache-2.0 |
| `deps/stun-proto` | [acking-you/stun-proto](https://github.com/acking-you/stun-proto/tree/dev) | [ystreet/stun-proto](https://github.com/ystreet/stun-proto), `v2.0.4 / main@9e47ca00` | `66dc8e0ae7179393d6b80373582893fa8bb0283e` | MIT OR Apache-2.0 |
| `deps/kanal` | [acking-you/kanal](https://github.com/acking-you/kanal/tree/dev) | [fereidani/kanal](https://github.com/fereidani/kanal)，本地扩展发布为 `kanal-plus 0.2.0` | `e79d95d5163446c23d53befbc778aaf6d7d34e42` | MIT |
| `deps/uni-stream` | [acking-you/uni-stream](https://github.com/acking-you/uni-stream/tree/dev) | 自研，`0.3.5` 发布后的已合并维护修复 | `796efc8c938e0eedb5006c2512fc5cf810fbbca4` | MIT |

固定提交均须先推送到各自仓库，再更新 pb-mapper 的 gitlink。2026-10-26 已把这三个 fork 的上游 `main` 合并到 `dev`，保留维护分支的 CI 配置。Quinn 仓库现在包含 `quinn 0.12.0`、`quinn-proto 0.12.0`、`quinn-udp 0.6.3`；rcgen 为 `0.15.0`；stun-proto 和同仓 `stun-types` 为 `2.0.4`。本次合并时 Quinn 0.12.0、rcgen 0.15.0 尚无发布 tag，这里固定的是上游开发提交，不是稳定发布版本。

Quinn 保留 Windows IPv4 源地址提示兼容修复（fork [PR #1](https://github.com/acking-you/quinn/pull/1)，上游 [PR #2958](https://github.com/quinn-rs/quinn/pull/2958)），并采用已验证的新接口适配。另外使用 `parking_lot` 维护连接状态锁并保留 panic poisoning；发送端只在背压后创建可写等待 future。新增共享 Tokio socket 入口，避免重复注册 reactor。`fast-apple-datapath` 为显式启用的优化：仅在已验证的 Darwin 27（本次实测 macOS 27.0.1）且批量 API 符号齐备时自动启用，其他系统维持原路径；不支持的系统调用会关闭批量路径，已生成的分段报文不会被错误拼成一个数据报。rcgen 库源码与上游一致；stun-proto 另更新了基准的时间接口和示例的错误构造，以通过严格 Clippy，协议源码未添加本地补丁。

macOS 已验证的 Apple 快路径按同一 socket 上观察到的报文积压自动调整。新 socket 使用普通 `recvmsg`；连续三次成功接收、期间没有读空，才启用 `recvmsg_x` 批量接收。批量模式连续四次只收到单个报文时回到普通接收。普通模式读空会清除连续成功计数；批量模式暂时读空保留原选择，避免每一波突发流量都重新升档。发送端仍按实际分段选择：单报文用 `sendmsg`，已分段的一组报文用 `sendmsg_x`。该策略不依赖应用请求长度，没有计时器、负载探测系统调用或等待凑批；状态与批量 API 的可用性开关分离，因此负载变化不会重新启用因系统错误关闭的能力。

批量机制现在通过内部 `DatagramIo` trait 静态选择，关联的 `ReceivePolicy` 分离负载策略；普通和固定批量策略都是零大小类型，只有 Apple 自适应策略保存计数。使用 const generic 选择单次/批量调用，没有 `dyn Trait`、新增逐包分配、队列或探测调用。Linux/Android 使用原生 `recvmmsg` 和支持时的 GSO/GRO；Windows 使用 `WSASendMsg` 的 USO 与按 Winsock provider 解析的 `WSARecvMsg`。Windows URO 保留显式启用，因为驱动缺失分段元数据的 [上游问题 #2041](https://github.com/quinn-rs/quinn/issues/2041) 仍未解决。iOS 默认使用公开的 `sendmsg`/`recvmsg`，没有自动开启未验证的私有批量 API。普通接收平台现在只预留一个接收缓冲区；Windows 的单个缓冲区仍可承载显式开启的 URO 合并数据。

pb-mapper 适配 Quinn 0.12 的独占接收端与 `UdpSender`：Quinn 驱动拥有唯一接收端，通过 fork 的 `wrap_tokio_udp_socket` 共享同一个 Tokio UDP socket 和 reactor 注册，额外引用只负责 STUN / 探测发送；QUIC 数据仍原地分流，没有新增逐包任务、队列或拷贝。rcgen 0.15 返回的 `PrivateKeyDer` 直接保存在身份状态中，并在构造 TLS 配置时显式克隆。精确证书固定、TLS exporter 绑定以及直连提交/回退规则不变。

各 fork 的 `dev` 用于后续开发和直接修复；`upstream` 只跟踪纯上游源码。需要贡献上游时，从合适的上游目标分支建立独立分支，只挑选相关修复，再向对应上游仓库提 PR。stun-proto 仓库包含配套 `stun-types`，使用其 re-export，无需额外直接依赖。

Kanal 的 `dev` 继承原 `dev/pb-mapper` 上已发布的 `kanal-plus 0.2.0`，保留批量接收和 owned stream 扩展。uni-stream 的 `dev` 继承原 `master` 上已合并的 DNS 和 MSRV 修复；当前 registry 的 `uni-stream 0.3.5` 来自 `c041f85`，这些后续维护提交尚不能视为该发布包的一部分。本次未调整这两个依赖。

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
quinn = { path = "deps/quinn/quinn", version = "0.12.0", default-features = false, features = ["runtime-tokio", "rustls-ring", "fast-apple-datapath"] }
rcgen = { path = "deps/rcgen/rcgen", version = "0.15.0", default-features = false, features = ["ring"] }
stun-proto = { path = "deps/stun-proto/stun-proto", version = "2.0.4" }
kanal = { package = "kanal-plus", version = "0.2.0" }
uni-stream = "0.3.5"
```

实际使用它们的 crate 再声明 `quinn.workspace = true` 等依赖。直接连接相关的三个 path 依赖由 `p2p` feature 管理；当前产品默认尝试直连，并在提交前失败时回退中继。TLS 配置代码使用 `use quinn::rustls;`，与 Quinn 内部使用同一个 rustls 包；不声明 `rustls.workspace` 或 `[patch.crates-io].rustls`。Quinn 内部 proto/udp 和 stun-proto 内部 stun-types 已有 path 关系，无须为这些传递 crate 再 fork 一个仓库。

通过 `cargo metadata`/`cargo tree` 检查三个直接依赖 source 为本地路径、rustls 由 registry 解析且只有预期的一份、没有意外启用 aws-lc 或系统证书验证。依赖锁定后运行受影响测试，CI 已把 `deps/*` 和 `.gitmodules` 纳入 Rust/Node 变更触发范围；仅调整 gitlink 也必须验收。

以上 path 方案适用于本仓库构建二进制。本次使用的 Quinn / rcgen 开发版还必须确认相应 registry 版本已发布，才能发布依赖它们的 SDK。**发布 crates.io SDK 需要另行完成依赖发布闭环**：根 `[patch.crates-io]` 不会作为库的依赖配置传播给使用者；`path + version` 在发布时会解析 registry 版本。若 fork 有行为修改，应先合入上游并依赖包含修复的版本，或发布明确命名的 fork 包并统一改依赖，不能把尚未发布的本地修复当作下游已经获得。Kanal 已通过 `kanal-plus 0.2.0` 发布；Quinn、rcgen 和 stun-proto 没有发布单独命名的 fork 包。Quinn 的 Windows 修复仅由固定 Git/submodule 依赖提供，不应宣称 crates.io 用户已获得该补丁。

## 本次升级的性能验证

环境：Apple M3 Pro、macOS 27.0.1（26A434）/ Darwin 27.0.0、arm64、Rust 1.99.1，release 构建且 `CARGO_PROFILE_RELEASE_DEBUG=0`。基线为 Quinn `6cbaeaaa`，该轮固定批量版为 `7c131852`（以下数据记录自动调整之前的版本）；两个版本使用同一份基准代码，固定 loopback 地址族，每个端点 2 个 Tokio worker，AES-128-GCM、ACK threshold 10、有序 `read_chunks`。每项交替顺序运行 3 次。下表是 **MiB/s 中位数（最小–最大）**，不是公网带宽或整个 pb-mapper 的端到端收益。

| 场景 | 原版 0.11 | 优化版 0.12 | 中位数变化 |
| --- | ---: | ---: | ---: |
| IPv4，4 条串行流，每条 256 MiB | 217.33（214.07–224.61） | 256.52（255.35–256.77） | +18.0% |
| IPv6，4 条串行流，每条 256 MiB | 196.62（194.61–197.31） | 232.90（231.97–233.42） | +18.5% |
| IPv4，32 条流、并发 8，每条 96 MiB | 207.17（205.89–210.94） | 244.01（242.66–244.36） | +17.8% |
| IPv4，10 万次串行请求，上下行各 1 KiB | 20.22（20.09–20.70） | 19.58（19.51–19.67） | -3.2% |

大流量场景提升约 18%，短请求吞吐下降约 3%。仅合并上游、未加优化的短请求对照中位数为 19.77 MiB/s，与优化版接近；这组数据不能证明每个场景都改善，也不足以给某一项锁优化单独分配收益。单流 IPv4 的进程 CPU 时间中位数从 9.70 s 降至 8.19 s；并发场景峰值 RSS 中位数约从 11.38 MiB 增至 12.27 MiB，包含双方端点和基准统计数据。

最初的 1000 次短请求过短，并发 32 MiB 测试也出现明显波动，因此分别延长到 10 万次和 96 MiB/流。原始样本没有删除，完整数据见 [results.json](../docs/benchmarks/2026-10-26-quinn/results.json) 和 [raw.txt](../docs/benchmarks/2026-10-26-quinn/raw.txt)。其中下载 P99 仅是读取该流剩余数据的耗时，不能当成请求往返延迟；0 µs 表示低于直方图精度。Linux/Windows 尚无这组性能对照，不能套用 macOS 的提升比例。

复现时，在临时目录用 `git archive` 分别展开 Quinn 的 `6cbaeaaa` 和 `7c131852`，把 `7c131852` 的 `bench/src/{lib,stats}.rs`、`bench/src/bin/bulk.rs` 复制到基线，并把基线 `bench/Cargo.toml` 的 Tokio feature 从 `rt` 改为 `rt-multi-thread`；保留基线自己的其余 manifest 配置。各自执行 `CARGO_PROFILE_RELEASE_DEBUG=0 cargo build --locked --release -p bench --bin bulk`；优化版追加 `--features quinn/fast-apple-datapath`。保存两个 `target/release/bulk`，交替运行下列命令各 3 次；macOS 可用 `/usr/bin/time -l` 记录进程 CPU 时间和峰值 RSS：

```sh
bulk --ipv4 --workers 2 --download-size 256M --streams 4
bulk --workers 2 --download-size 256M --streams 4
bulk --ipv4 --workers 2 --download-size 96M --streams 32 --max_streams 8
bulk --ipv4 --workers 2 --upload-size 1024 --download-size 1024 --streams 100000
```

## 自适应接收与平台 trait 验证

这一轮先实现按 socket 积压切换接收方式，再抽取平台机制与策略。代码见 [batch.rs](quinn/quinn-udp/src/batch.rs) 和各平台 `PlatformIo`；没有修改 socket 所有权、上层异步运行时或发送报文边界。Apple 批量接收单独保留栈帧，普通接收不再预留整组消息数组；Linux 的固定批量后端直接内联进接收入口。

同一台 M3 Pro / macOS 27.0.1 / Rust 1.99.1 上，先前自适应版 `365fe202` 的 56 个样本中，IPv4 1 KiB 串行请求中位数为 19.71 MiB/s，原版 0.11 为 19.91 MiB/s，固定批量版为 18.98 MiB/s。接近旧版，但不能说完整恢复了旧版表现；该轮并发大流量也有下降，完整记录保存在 [results.json](../docs/benchmarks/2026-10-26-quinn-adaptive/results.json) 和 [raw.txt](../docs/benchmarks/2026-10-26-quinn-adaptive/raw.txt)。这轮起所有版本都使用新的完整请求计时，并排除直方图汇总时间，不能直接与上一节的计时口径混算。

trait 重构另进行 60 次交替测量，每项每版本 4 次，仍为 loopback、每端点 2 个 worker、AES-128-GCM、ACK threshold 10。下表是 **MiB/s 中位数（最小–最大）**。固定批量为 `7c131852`，抽象前为 `365fe202`，trait 版的 macOS 产品代码为 `18eb2c71`；随后提交仅调整 Linux 内联、WASI 测试条件与 CI 配置，不改变本机测量代码。

| 场景 | 固定批量 | 自适应、抽象前 | trait 版 |
| --- | ---: | ---: | ---: |
| IPv4，10 万次串行请求，上下行各 1 KiB | 19.61（19.31–20.19） | 19.20（18.38–19.23） | 19.45（19.39–19.82） |
| IPv4，10 万次请求、并发 32，上下行各 1 KiB | 87.47（84.62–89.64） | 86.00（78.01–89.99） | 86.91（80.51–88.66） |
| IPv6，10 万次串行请求，上下行各 1 KiB | 18.86（16.60–19.96） | 18.70（18.25–20.05） | 19.97（19.08–20.12） |
| IPv4，4 条串行流，每条 256 MiB | 241.76（234.43–247.19） | 242.49（230.87–244.08） | 232.78（209.96–255.40） |
| IPv4，32 条流、并发 8，每条 96 MiB | 234.84（229.91–245.96） | 244.46（234.01–255.85） | 242.33（225.49–248.18） |

IPv4 串行短请求的完整请求 P99 中位数由抽象前 95 µs 变为 76 µs，IPv6 为 98.5 µs → 65 µs。这些是四次运行各自 P99 的中位数，不是合并直方图。样本区间有重叠，不能据此宣称所有负载都更快。尤其单流大流量首轮中位数下降约 4%；因此只对该场景追加 8 轮交替复核，抽象前为 245.60（215.17–252.95），trait 版为 250.96（239.60–269.87）MiB/s，方向发生反转。共享桌面的调度波动仍明显，既不能把首轮下降确定为代码回退，也不能把复核上涨确定为稳定收益。

所有样本均保留：[trait 对照数据](../docs/benchmarks/2026-10-26-quinn-adaptive/traits.json)、[原始输出](../docs/benchmarks/2026-10-26-quinn-adaptive/traits-raw.txt)、[大流量复核](../docs/benchmarks/2026-10-26-quinn-adaptive/bulk-followup.json)、[复核原始输出](../docs/benchmarks/2026-10-26-quinn-adaptive/bulk-followup-raw.txt)。早期探测方案及因桌面负载干扰而停止的实验记录在 [exploratory.json](../docs/benchmarks/2026-10-26-quinn-adaptive/exploratory.json)，不用于最终收益结论。

“零成本”在这里指静态分发、无需负载策略的平台不保存额外状态，以及没有新增逐包分配或探测操作，不表示所有目标的机器码或产物字节完全不变。[汇编记录](../docs/benchmarks/2026-10-26-quinn-adaptive/assembly.txt)显示：Windows MSVC 的 `recv` / `try_send` 入口归一化符号后指令相同；Linux 仍直接执行原生批量接收，内联布局有变化；本机 Apple `recv` 入口自身的栈帧从 9,168 字节降到 48 字节，后者不包括被调用函数的栈帧。工具链和构建条件见 [assembly.json](../docs/benchmarks/2026-10-26-quinn-adaptive/assembly.json)。这些静态证据不等于 Linux、Windows、Android 或 iOS 的实机吞吐提升。

复现时分别展开上述提交，所有版本统一复制最终版 `bench/src/{lib,stats}.rs` 与 `bench/src/bin/bulk.rs`，使用独立 `CARGO_TARGET_DIR` 构建 release 二进制。原版 0.11 只把自身 manifest 的 Tokio `rt` feature 改成 `rt-multi-thread`，不要复制新 workspace 的 manifest。固定批量、自适应和 trait 版均开启 `quinn/fast-apple-datapath`。按需运行统一采集器：

```sh
python3 docs/benchmarks/2026-10-26-quinn-adaptive/measure.py --mode traits \
  --batch /path/to/fixed-batch --adaptive /path/to/pre-trait --trait /path/to/trait \
  --out /tmp/quinn-traits-new --repeats 4
python3 docs/benchmarks/2026-10-26-quinn-adaptive/measure.py --mode bulk \
  --adaptive /path/to/pre-trait --trait /path/to/trait \
  --out /tmp/quinn-bulk-new --repeats 8
```

本轮本机验证：Quinn 严格 Clippy、UDP 普通与自适应路径测试、36 个运行时测试通过；pb-mapper 全 workspace 严格 Clippy、client 的 36 个单元测试和 3 个 direct_e2e 测试通过；Node 12 个测试通过，addon 3,193,504 字节，低于 5.5 MiB。Windows GNU、Linux GNU、Android arm64、iOS arm64 和 simulator 均通过库目标严格交叉 Clippy；iOS 27 模拟器实际执行普通 UDP 收发测试通过，私有批量路径未启用。WASI 库与测试编译也已验证。记录见 [validation.txt](../docs/benchmarks/2026-10-26-quinn-adaptive/validation.txt)。最终提交的跨平台原生及 Android 模拟器验证见 [Quinn CI](https://github.com/acking-you/quinn/actions/runs/40335255059)；另新增移动目标持续检查。[Coverage](https://github.com/acking-you/quinn/actions/runs/40335254809) 在三个平台执行完整覆盖率测试，并要求将 LCOV 报告保存为 GitHub Actions 附件，报告缺失会失败。Codecov 在切换到 GitHub OIDC 后仍返回 `Repository not found`；因此外部发布改为在服务端激活 fork 并设置仓库变量 `CODECOV_ENABLED=true` 后启用，启用后上传失败仍会使 CI 失败。未配置外部服务不会跳过覆盖率测试或 GitHub 报告保存。

## 已执行的检查

本次上游升级与性能改动：

- 主仓库格式检查、严格全 workspace Clippy 和全 feature 测试通过；Darwin 批量路径实际启用后，另通过 client 的 36 个单元测试和 CLI 的 3 个 direct_e2e 测试。
- [Windows 原生验收](https://github.com/acking-you/pb-mapper/actions/runs/40303784019)通过：client/server/CLI 严格 Clippy、client 单元测试及 direct_e2e。Quinn 跨平台验证记录见 [CI](https://github.com/acking-you/quinn/actions/runs/40303636277)。
- Quinn 的 [Coverage 工作流](https://github.com/acking-you/quinn/actions/runs/40303636380)在 Ubuntu 生成报告后，因 fork 没有配置 Codecov token 而上传失败；这与此前合并提交的失败原因一致，其余覆盖率矩阵任务被取消，不能算作通过。
- Quinn、rcgen、stun-proto 的格式、严格 Clippy 和相关测试通过。Quinn 另覆盖锁中毒、发送背压、共享 socket、UDP 分段、空报文与批量关闭后的数据报边界。
- Node release 构建和 12 个 JS 测试通过；addon 为 3,176,992 字节，低于 5.5 MiB 限制。3 个发布包边界测试通过。
- 关闭默认 feature 后的 CLI 依赖树不包含 Quinn、rcgen、stun-proto；正常构建只使用 registry 的 rustls 0.23.46，未引入 aws-lc 或系统证书验证。

以下为本次升级之前的分支迁移检查记录：

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
