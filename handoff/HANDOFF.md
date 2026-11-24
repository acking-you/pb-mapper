# 交接：pb-mapper 全 PC 端远程桌面性能优化（截至 2026-11-25）

本文给接手的 AI。照着做即可完成全部剩余工作：PR 的 CR 修复、CI、按序安全合并、发布 0.13.0。

---

## 0. 我们在做什么

**项目**：pb-mapper（GitHub `acking-you/pb-mapper`）。它是一个 Rust 写的隧道和中继，带远程桌面功能和 Flutter 客户端（Windows/macOS/Linux/Android/iOS）。仓库规则写在 `CLAUDE.md`（即 `AGENTS.md`），必须遵守。

**任务来源**：仓库所有者（下称"用户"）提出了以下要求，按时间先后：

1. 分析 airhand（<https://github.com/sqlpxc/airhand>）。它是一个宣称能跑 120 fps 的远程桌面，要看它做了哪些优化而 pb-mapper 没做。
2. 在 pb-mapper 支持的**所有 PC 端**做最佳优化，包括需要协议配合的改动：
   - Windows 主机和观看端
   - macOS 主机和观看端
   - Linux 观看端。Linux 没有主机后端，这次不新增。
3. 用户原话："不要考虑要不要大改之类的，只要你认为有效果的都应该优化"。改动大不是推迟的理由，但没有实际效果的不做。
4. 可以拆成多个 PR。我判断可以合并就合并，无需用户逐个批准。
5. 最后发一个新版本 **0.13.0**，用户会在设备上做全方位测试。
6. 用户的额度用完了，所以交接给你。

**已经完成的前置工作**：PR #61、#62 已合并。修了 Windows 采集空转，修了 Windows 观看端 GPU 等待时的自旋，去掉了无人观看时共享界面的空闲计算。

**整体流程**：
1. 调研。10 个方向，每个方向先由一个 agent 对照 airhand 源码提方案，再由另一个 agent 独立评审。
2. 规划成 47 个 PR，计划在 `data/pc-plan.json`。
3. 分批实现。每个 PR 经过：实现 → 两路审查（正确性、效果与完整性）→ 核实审查意见后修复。
4. 每批合进一个整合分支，在本机跑全量验证。
5. 做成"叠加 PR"（第 2 节）开出，跑 CI，按顺序 squash 合并。

**这次工作的硬约束**（详见 `CLAUDE.md`）：
- 远程桌面消息拒绝未知字段，所以新字段或消息只能发给列出了对应 capability 的对端。ALPN 保持 `pb-mapper/desktop/2`。
- `HELPER_PROTOCOL` 和两个 `.v<n>` 管道名必须一起改。
- 默认零开销；数据通路上不能逐包复制、排队或起线程。
- 操作系统 API 只能出现在 `crates/pb-mapper-desktop/src/platform/<os>/` 下。
- 数据通路的改动要用 release 构建测量前后。docs 和 CHANGELOG 里**不能出现未经实测的数字**。
- 用户可见的改动要在 `CHANGELOG.md` 的 `## [Unreleased]` 下加一条。中英文文档要成对修改。

---

## 1. 当前状态一览

| 批次 | 内容 | 状态 |
| --- | --- | --- |
| 第 1 批 | 12 个 PR，#63–#74 | **已全部合并**，master 在 `2059150` |
| 第 2 批 | 18 个 PR，#75–#92，叠加 | **已开出，未合并**。CI 正在重跑（原因见第 4 节） |
| 第 3 批 | 9 个 PR，#94–#102，叠加在 #92 之上 | **已开出，未合并**。CI 在跑 |
| 未交付 | helper-push、relative-pointer、standby-session、第 4 批（AV1 等） | 见第 3.3 节 |
| 发布 | 0.13.0 | **未做** |

**另外**：#93（"Keep pb-mapper reachable while the display sleeps"）是**用户自己**开的 PR，和本工作无关。不要合并它，也不要改它；合并顺序上避开它。如果它和本工作冲突，问用户。

### 第 1 批（已合并，供参考）

| PR | 内容 |
| --- | --- |
| #63 | 采集按显示器呈现时间对齐；新增 Backend/VideoSource 接缝 |
| #64 | Windows 主机防休眠 |
| #65 | helper 管道整块读写 |
| #66 | 用 SendInput 注入指针 |
| #67 | 码率模型约 2 s 内切换 |
| #68 | fps cap 记住解码器能力 |
| #69 | 直连失败冷却期共享 |
| #70 | Linux 中继 TCP 调优 |
| #71 | Mac/Linux 解码统计 |
| #72 | Linux 观看端在 GPU 上转换 |
| #73 | 原生光标 |
| #74 | 像素对齐 |

### 第 2 批（#75–#92，按合并顺序）

| PR | 分支 | 内容 |
| --- | --- | --- |
| #75 | wave2/01-mac-session-check-cache | Mac：会话和几何检查在 16 ms 内复用 |
| #76 | wave2/02-mac-wake-display | Mac：唤醒已熄的屏幕 |
| #77 | wave2/03-mac-vt-submit-thread | Mac：VideoToolbox 调用移到独立提交线程 |
| #78 | wave2/04-mac-vt-rate-control | Mac：QP 上限 51；丢掉的最后一帧重发 |
| #79 | wave2/05-mac-capture-cadence | Mac：按刷新率设 SCK 采集间隔（修 144 Hz 屏上只有 48 fps） |
| #80 | wave2/06-win-mf-output-events | Windows：编码器事件直接唤醒线程 C |
| #81 | wave2/07-win-encoder-settings | Windows：H.264 改成 High + CABAC，并重写 SPS |
| #82 | wave2/08-win-capture-convert | Windows：直接从 DXGI 画面转换；支持旋转的屏幕 |
| #83 | wave2/09-win-present-waiter | Windows：采集线程等待呈现（推送式采集） |
| #84 | wave2/10-win-hdr-desktop | Windows：HDR 桌面按 SDR 白点显示 |
| #85 | wave2/11-win-still-refinement | Windows：静止画面锐化。**默认对任何编码器都不开**，等真机测试列出编码器 |
| #86 | wave2/12-helper-probe-cache | helper 缓存 H.265 探测结果。HELPER_PROTOCOL 升到 11 |
| #87 | wave2/13-viewer-feedback | 新增 capability `arrival_time` 和 `keyframe_seq`（单向到达时间） |
| #88 | wave2/14-receipt-rate-control | 按回执控制码率 |
| #89 | wave2/15-reconnect-start-rate | 重连沿用上次结束时的码率 |
| #90 | wave2/16-pointer-cadence | 指针按会话帧率上报；悬停不重绘 |
| #91 | wave2/17-mac-viewer-layer | Mac 观看端原生图层，**可选**（`--dart-define=presenter=layer`） |
| #92 | wave2/18-linux-presenting-gpu | Linux 在负责显示的那块 GPU 上解码 |

### 第 3 批（#94–#102，按合并顺序，接在 #92 之后）

| PR | 分支 | 内容 |
| --- | --- | --- |
| #94 | wave3/01-viewer-refresh | 刷新率精确跟随窗口所在的显示器 |
| #95 | wave3/02-decoder-install-hints | 提示安装 HEVC 扩展或 GStreamer 解码器 |
| #96 | wave3/03-viewer-scroll | 滚轮一格对一格；高精度和惯性滚动（capability `wheel_hires`）。HELPER_PROTOCOL 升到 **12** |
| #97 | wave3/04-mac-vt-padded-capture | Mac：非 16 对齐的尺寸补齐后在 SPS 里裁剪 |
| #98 | wave3/05-encoder-throughput-cap | 按编码器实测吞吐限制 pace |
| #99 | wave3/06-host-direct-bbr | 实测 BBR 和 Cubic，**结论保留 Cubic**（只留下测量工具和记录） |
| #100 | wave3/07-linux-video-plane | Linux 原生视频平面，**可选**（`--dart-define=presenter=plane`） |
| #101 | wave3/08-mac-viewer-enhance | Mac 放大锐化。依赖可选的 layer presenter |
| #102 | wave3/09-linux-plane-scaling | Linux 平面缩放质量 |

每个 PR 的 body 里都有："Summary"、改动列表、本机验证结果、测量数据、"Needs CI or a device"（只有 CI 或真机才能验证的项）。

---

## 2. 分支结构（务必先理解）

- **整合分支**：本地的 `perf2/wave2`、`perf2/wave3`，已推到 origin。它们是把各 PR 原始分支逐个 `git merge` 得到的，冲突已手工解决，并在本机跑过全量验证。
- **叠加 PR 分支**：`wave2/NN-*` 和 `wave3/NN-*` 上，每个 PR 只有**一个提交**。这个提交的 **tree 恰好等于整合分支合入该 PR 后的那个 merge 的 tree**。它的父提交是前一个 PR 的提交；第 2 批第一个 PR 的父提交是 master。所以：
  - 每个 PR 在 GitHub 上看到的 diff 正好是它自己的改动。
  - 每个 PR 的 CI 验证的正好是"合到这里时"的完整代码。
  - 栈顶 `wave3/09-linux-plane-scaling` 的 tree == `perf2/wave3` 的 tree。
- **原始 PR 分支**：`perf2/<name>`，是各 PR 实现时的原始提交历史。其中未交付的三个和两个整合分支已推到 origin（第 6 节列出），其余只在用户这台机器的本地仓库里，供查阅。
- **为什么合并前要重设父提交**：master 上是 squash 提交，不包含栈里的历史。GitHub 自动把下一个 PR 改基到 master 后，会以老 base 做三方合并，同一处改动两边各有一份，就会冲突。所以每次合并前，把该 PR 分支重建为"同一个 tree、父提交是当前 master"，强推，再 squash 合并。squash 之后 master 的 tree 就等于那个已验证的 tree。`scripts/merge_stack.py` 就是做这件事的，第 1 批 12 个 PR 都是这样合的，每次都核对过 tree 一致。

---

## 3. 剩余工作（按顺序做）

### 3.1 第 2、3 批 PR 的 CI 与 CR

1. **看 CI**：`gh pr checks <N>`，对 #75–#92、#94–#102 逐个看。
   - 叠加 PR 的 base 不是 master，所以 GitHub 只会跑 push 触发的 CI（Syntax Check 全套，包括 rust-windows-desktop、macos-app、linux-app 等），不会跑 pull_request 触发的那套。
   - 只有 #75 和改了 crates 的 PR 会跑 NAT traversal。
2. **已知问题**：
   - **#80–#82 在 Windows 上编译失败（已修）**：`Atomic::fetch_update` 在 Rust 1.99 被弃用，CI 用 `-D warnings`。我已把这三个提交 tree 里 `crates/pb-mapper-desktop/src/platform/windows/video/codec.rs` 的 `count.fetch_update(` 改成 `count.try_update(`，重建了整个栈并强推（`scripts/restack_fix.py`）。#83 及以上的 tree 没有变。CI 正在重跑，确认它变绿。
   - **#87 上的 Windows CI 失败过一次**：`codec::present_waiter::tests::a_picture_that_waited_goes_only_once_the_thread_looked_past_it` 在 `stale()` 里等了 5 s 也没等到唤醒。
     - 同一段代码在 Windows 上另外 9 次运行都通过；在 Mac 上单独跑 300 次、满负载下整个模块跑 150 次都没复现。
     - 我检查过 `present_waiter.rs` 的 arm/run/looked/publish，以及 `platform/windows/waiter.rs` 的唤醒原语（自动重置事件 + `pending` 标志），没找到原因。
     - **待你处理**：如果重跑后还出现，按"可能丢唤醒"排查。生产上的影响可能是偶发卡顿：Windows 主机上线程 C 漏掉一次唤醒，要等到下一个 deadline（约 250 ms 的 liveness）。
   - **CI 资源**：macOS runner 并发有限，强推后会排很久。用 `gh run list` 找出 headSha 已不是任何分支 head 的运行，`gh run cancel` 掉（我做过两次）。
3. **CR**：
   - 每个 PR 都已有两路 AI 审查加修复，审查记录在 `data/wave{2,3}-result.json` 的 `reviews` 和 `fix`。第 3 批的 reviews 字段是空的，因为工作流被中途停止，只保留了 impl 和 fix 结果；修复记录在 `fix.fixed` 里。
   - 如果有人类或 bot 的评论，逐条核实后修复。**修复方法**：在对应 PR 分支（或 `perf2/<name>`）上改，然后重建其上的整个栈。用 `scripts/restack_fix.py`，或参照它用 `git commit-tree` 保持其他 tree 不变。改完后在本机重跑 `scripts/verify_all.sh`。

### 3.2 安全合并（CI 全绿之后）

依次执行（脚本里的 scratchpad 路径要改成 `handoff/` 下的实际路径）：

```bash
python3 -I handoff/scripts/merge_stack.py handoff/data/wave2-prs.json handoff/data/wave2-squash.json 75 92
python3 -I handoff/scripts/merge_stack.py handoff/data/wave3-prs.json handoff/data/wave3-squash.json 94 102
```

`merge_stack.py` 对每个 PR 依次：
1. 把分支重建为"同一 tree、父提交是 origin/master"，经 **SSH** 强推。
2. 等 GitHub 改基到 master，并显示可以合并。
3. `gh pr merge N --squash --subject "<标题> (#N)" --body "<散文正文>\n\nCo-Authored-By: ..."`。
4. `git fetch` 后核对 `origin/master^{tree}` 等于该分支的 tree，不等就停。

合并规范（仓库惯例）：
- 只允许 squash。
- 标题写成 `<PR 标题> (#N)`。
- 正文是一段手写的散文，已写好在 `data/wave{2,3}-squash.json`。
- 合并后分支会被自动删除。
- 没有 auto-merge，也没有分支保护。

合并后取消 master 上已经过时的中间 CI 运行，只留最后一个提交的运行，并确认它是绿的。

### 3.3 未交付的工作（可选，按用户"有效果就做"的原则）

**helper-push**（分支 `perf2/helper-push`，已审查和修复，基于 `perf2/wave2`）：
- 作用：helper 的 worker 主动推送编码单元、新画面通知和指针，线程 C 不再长轮询管道。
- 与 #96（viewer-scroll）冲突：两者都把 HELPER_PROTOCOL 定为 12；helper-push 重写了 `oop/client.rs` 的 RemoteBackend 和整个测试模块。
- 做法：
  - 合并时把两者合成同一个协议版本（12 尚未发布，可以共用），或者升到 13。13 的话，两个 `.v<n>` 管道名、文档和 CHANGELOG 要一起改。
  - 把 #96 的测试 `protocol_12_carries_scrolls_and_the_hires_wheel` 移植到 helper-push 的新测试框架。
  - 冲突文件：`oop/client.rs`、`platform/windows/helper/{mod,readiness}.rs`、`docs/architecture.md`、`docs/remote-desktop.md`。
- 这些代码只能在 Windows CI 上编译。

**relative-pointer**（`perf2/relative-pointer`）和 **standby-session**（`perf2/standby-session`）：
- 已实现，**还没审查**。relative-pointer 的实现者标记为未完成。
- 合并前必须做完审查和修复，并做 `verify_all`。

**第 4 批，未开始**。brief 在 `data/pc-plan.json` 的 `prs[]`，按 name 查：
- `av1-viewers`：AV1 能力协商，以及三个观看端解码。
- `av1-windows-host`：Windows MF AV1 编码。
- `win-phase-locked-decimation`：按呈现节拍抽帧，需要改 helper 协议。
- `disposable-frames`：可丢弃帧。
- `win-viewer-ffmpeg-decode`：Windows 观看端附带 LGPL 的 FFmpeg DLL 来解 HEVC/AV1。**用户还没决定**（涉及授权和专利），不要自行做或合并，先问用户。

### 3.4 发布 0.13.0

照 `skills/pb-mapper-release-pipeline/SKILL.md` 做。要点：

1. 在 master 上把版本从 0.12.1 改成 0.13.0：
   - `Cargo.toml` 的 workspace version，以及 7 个 `pb-mapper-*` 依赖的版本号。`deps/quinn` 自己的 0.12.x 不要动。
   - `js/package.json`、`js/index.js`（54 处版本检查）、`js/bun.lock`（4 处）。
   - `ui/pubspec.yaml` 改成 `0.13.0+931`。
   - `docs/ui-updates.md`：931 已被使用，下一个分发要 ≥ 932。
   - `CHANGELOG.md` 的 `## [Unreleased]` 改成 `## [0.13.0] - <日期>`。
   - 然后 `cargo update --workspace`。
   - `scripts/bump_release.py` 能做以上全部，但用前先看它的正则还匹配不匹配当前文本。
2. 发布前在 master 上跑一次本地全量验证（见第 5 节）。
3. 提交 `Release 0.13.0: <summary>`，推送 master，`git tag -a v0.13.0 -m "Release v0.13.0"`，推送 tag。
4. tag 会触发 CLI、UI、Docker、SDK（crates.io 和 npm）四个发布工作流，逐个确认成功：
   - `gh release view v0.13.0`
   - `gh release view v0.13.0-ui`
   - `cargo info pb-mapper@0.13.0`
   - `npm view pb-mapper@0.13.0 version`
5. 失败就修复后发 0.13.1。**绝不复用 tag**。
6. CHANGELOG 里要写清楚：这个版本用 helper 协议 12，Windows 后台服务需要在 app 里点一次 **Update**。

### 3.5 发布后交给用户的真机测试清单

（汇总自各 PR 的 "Needs CI or a device"。）

- **Mac 主机**（要给终端或 app 授予屏幕录制权限，并保持亮屏、不锁屏）：
  - 144 Hz 屏上 60/120 fps 观看端的实际帧率（#79）。
  - `sck_held_back_change` 门槛测试（#79）。
  - 会话检查耗时（#75）。
  - 唤醒熄屏（#76）。
  - padded capture 的 Phase 0 门槛（#97）。
- **Mac 观看端**：layer presenter 从解码到上屏的延迟（#91）。测完决定是否把它设为默认；#101 依赖它。
- **Windows 主机**：
  - 呈现对齐和推送式采集的延迟（#63/#83）。
  - High + CABAC 在各 GPU 厂商上的回退路径（#81）。
  - HDR（#84）。
  - 静止画面锐化，测完要把编码器加入启用名单（#85）。
  - helper 协议 11/12 下后台服务 Update 后的行为。
- **Linux 观看端**：
  - GPU 转换和 dma-buf（#72）。
  - 负责显示的 GPU 选择（#92）。
  - 视频平面，测完决定是否设为默认（#100/#102）。
- **网络**：真实链路上验证按回执控制码率（#88）。限速解除后码率恢复要 4–5 s，原来是 1–2 s；这是用拥塞期间帧率不掉换来的，记录见 `docs/benchmarks/2026-11-24-receipt-rate-control/`。

---

## 4. 已知风险与取舍

- **只在 CI 编译过**：Windows 的 Rust、helper、C++ runner 和 Linux runner 都只在 CI 上编译过，没有在真机上运行过。
- **默认关闭的功能**：#85 静止锐化、#91 Mac layer、#100 Linux plane、#101（依赖 #91）。原因是不在真机上测就打开风险太大。
- **测量后没采用的方案**：
  - #78：H.265 标准码率控制、timestamp 起始码率模型。
  - #99：BBR，在 policer 下丢包是 Cubic 的数倍到数百倍。
  - 更早的 keyframe supersede、线程 C 优先级。
- **helper 协议在这个版本里升了两次（11、12）**，发版只体现最终的 12。
- 第 4 节提到的 #87 偶发测试失败。

---

## 5. 环境与工具注意事项（这台 Mac）

- **git 推送**：`origin` 是 HTTPS，git 用的是钥匙串里的旧 token，没有 `workflow` 权限，推送改了 `.github/workflows/*` 的分支会被拒。`gh` 自己的 token 已经补上了 `workflow`，但 git 不用它。**统一经 SSH 推送**：`git push git@github.com:acking-you/pb-mapper.git <branch>...`。
- **Flutter**：在 `~/Developer/flutter/bin`，版本 3.50.1；CI 用的是 3.44.9。
  - 本机 `app_test.dart` 里 "the title bar never overflows while this computer is viewed" 必定失败，`dart format` 会报约 20 个无关文件。这是 SDK 版本漂移，不是回归。格式只检查自己改过的文件。
  - 跑 Flutter 会改动 `ui/pubspec.lock`、`ui/analysis_options.yaml`、`ui/macos/Flutter/*.xcconfig`，并新建 `ui/macos/Podfile`。提交前还原：`git checkout -- ui/pubspec.lock ui/analysis_options.yaml ui/macos/Flutter; rm -f ui/macos/Podfile`。
- **其他工具**：没装 `bun`，Node 的检查只能靠 CI。`python3` 是 3.9。
- **本地全量验证**：`scripts/verify_all.sh`（会调用 `verify.sh`），依次跑 fmt、clippy、workspace test、doc、desktop_loopback，再跑 Flutter 的 gen-l10n、analyze、test，最后还原 Flutter 改动的文件。脚本里的路径变量 `S` 要改掉。
- **只能在 CI 上验证的**：Windows 代码（rust-windows-desktop）、Linux runner（linux-app）、iOS（ios-app）、NAT traversal（需要 Linux + sudo）。
- **合并冲突辅助脚本**：
  - `merge_wave.sh`：按顺序 merge；只在 CHANGELOG 和 docs/README 冲突时自动保留两边。
  - `union.py`：冲突块保留两边。
  - `union_fn.py`、`glue.py`：两边各自新增函数或测试时用，中间补上闭合。
  - `table3.py`：Markdown 表格冲突按行三方合并。
- **建栈和开 PR**：`stack_wave.py`（按整合分支的 merge 建叠加提交）、`open_prs.py`、`pr_bodies.py`。

---

## 6. 文件清单（本分支 `handoff/` 目录）

- `HANDOFF.md`：本文。
- `data/pc-plan.json`：47 个 PR 的完整 brief，字段有 design、verification、measurement、files、protocol_changes。
- `data/pc-research.json`：10 个方向的调研和评审原文。
- `data/wave{1,2,3}.json`：各批次的 brief。
- `data/wave{1,2,3}-result.json`：实现、审查、修复记录，包括每个 PR 的 device_only 和 remaining。
- `data/wave{1,2,3}-titles.json`、`data/wave{1,2,3}-squash.json`：PR 标题和 squash 正文。
- `data/wave{1,2,3}-prs.json`：PR 编号、分支和标题，供 `merge_stack.py` 使用。
- `scripts/`：上面提到的全部脚本，加上 `bump_release.py` 和原工作流脚本 `lanes-workflow.js`（供参考）。

已推到 origin 的相关分支：
- `handoff/pc-optimization`：本文所在分支。不要合进 master。
- `perf2/wave2`、`perf2/wave3`：整合分支。
- `perf2/helper-push`、`perf2/relative-pointer`、`perf2/standby-session`：未交付的工作。
- `wave2/*`、`wave3/*`：开着的 PR 分支。

其余 `perf2/<name>` 原始分支只在用户这台机器的本地仓库里，没有推送。
