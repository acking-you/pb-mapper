# 远程桌面重设计与性能改造 · 开发文档

这里存放 PR #51 后续改造的全部开发文档：已确认的设计稿、性能分析、实现清单，以及实现时使用的需求、决策、接口契约和工作计划。PR #51 仍因真机流畅度阻塞合并；这些文档描述的改造完成并经用户在真机上认可之前，阻塞不解除。

截至 2026-11-07：设计已由用户确认；[`plan/WORKPLAN.md`](plan/WORKPLAN.md) 的 9 个工作包，以及对抗性审查发现问题的修复，都已在分支 `claude/remote-desktop-overhaul` 上实现。合并后的分支通过了 `cargo fmt`、`cargo clippy`、全部 Rust 测试（含被忽略的慢链路验收用例）、`cargo doc`、FFI 发布构建、Android 的 `cargo ndk` 检查、`flutter analyze`、Flutter 测试和 Android 单元测试，APK 与 Windows 发布构建、验收工具构建也已从合并后的分支构建成功；[`plan/WORKPLAN.md`](plan/WORKPLAN.md) §3.3 里需要部署环境或 Windows 交互的检查（`deployed selftest`、`input_acceptance`、辅助服务重装路径）和真机验收由用户完成。PR #51 仍保持合并阻塞，直到用户在真机上认可流畅度。

## 先看这三份

| 文档 | 内容 |
| --- | --- |
| [设计稿](design/README.md) | 15 张已确认的画板：桌面远程桌面页、首次开启、添加电脑、查看器及其状态、手机远程页、扫码、横屏查看器、键盘、手势、组件、图标系统、标题栏修正 |
| [流畅度优化优先级（借鉴 RustDesk）](perf-priorities.md) | 为什么直连也卡：发送信用绑在手机呈现 ACK 上、每秒一个 IDR、12 Mbps / 90 fps 目标；按 P0–P2 排序的改法和验收方法 |
| [UI 重设计实现清单](ui-redesign-checklist.md) | 设计要点、图标统一、标题栏按钮重叠修正、需要新增的能力、实现顺序 |

## 实现依据（`plan/`）

实现阶段以这几份为准，冲突时的优先级：`WORKPLAN` 里标 **AMEND** 的条目 > 两份 `CONTRACT` > `DECISIONS` > `BRIEF`。两份 `CONTRACT` 已按对抗性审查后的修复更新，改动的章节列在各自开头。

| 文档 | 内容 |
| --- | --- |
| [BRIEF.md](plan/BRIEF.md) | 原始需求：性能项 P0-1…P2-1 与 P1-4、UI 重设计、新能力、图标规则、标题栏修正、要更新的文档、工程规则 |
| [DECISIONS.md](plan/DECISIONS.md) | 32 条跨模块决策（D1–D32）：协议 v2 与 ALPN、`pbd1` 邀请与证书固定、接收即归还信用、无限 GOP、手机视频不经 Dart、手势、记住的电脑加密存储等 |
| [CONTRACT-perf.md](plan/CONTRACT-perf.md) | 性能架构契约：常量表、线协议 v2、主机会话与信用、QoS、采集节拍与编码器、输入授权边界、查看端、FFI、原生解码、Dart 存储、测试 |
| [CONTRACT-ui.md](plan/CONTRACT-ui.md) | UI 契约：外壳与导航、共享指示、记住的电脑与保险库、远程页、查看器（桌面 / 手机）、交互规范、图标、标题栏、文案、测试 |
| [WORKPLAN.md](plan/WORKPLAN.md) | 9 个工作包的归属文件、依赖、验收命令、合并顺序（G0–G3）与真机验收步骤 |
| [CRITIQUE.md](plan/CRITIQUE.md) | 对两份契约和工作计划的审查意见；已全部处理，处理方式见 WORKPLAN 末尾 |

## 设计过程材料

- [`proposals/`](proposals/)：评审前的 5 份候选架构。性能三份（贴近 RustDesk、改动最小、以故障模式为主），UI 两份（结构、交互）。契约在它们的基础上合成，评审结论见契约第 1 节。
- [`code-maps/`](code-maps/)：动手前对 PR #51（`a41eaa7`）的 9 份代码地图：主机流水线、查看端与 FFI、Dart 流水线、原生解码、UI 页面、设计规范、文档与协议、RustDesk 交互、依赖与能力。行号对应 `a41eaa7`。

## 说明

- 文中的本机路径已替换为占位符：`<repo>` 是本仓库检出目录，`<rustdesk checkout>` 是 RustDesk 上游 `5f015c9` 的本地副本，`<package worktrees>` 是各工作包的工作树，`<scratchpad>` 是撰写时的临时目录。
- 性能文档和实现清单最初写在协作文档里，这里是转成 Markdown 的副本。
