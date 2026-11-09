# Installed input and asynchronous video regression

**Merge block lifted — 2026-11-10:** the owner tested the stage7 build (Windows
`8f800b3`, Android `ae3df37`), reported that everything works, and asked for
PR #51 to be merged. This rests on the owner's own use, not on the instrumented
phone run proposed in `docs/remote-desktop.md`. The record of 2026-11-06
follows unchanged.

**Merge blocked — 2026-11-06:** the user reports that remote desktop remains
severely choppy even with direct connectivity, below 30 fps, while RustDesk is
visibly smoother with a higher frame rate. These are user observations, not an
instrumented comparison. Physical-phone fluidity acceptance has failed. Do not
merge PR #51 until the performance issue is resolved and real-device acceptance
passes. See [the performance handoff](#performance-handoff-2026-11-06).

Physical-phone testing of Android ARM64 0.10.2+923 exposed unusable clicks,
jerky pointer movement and an invalid-invitation retry loop. Build 923 used the
same application source as 922. The previous synthetic input and local video
checks did not establish that the installed SYSTEM worker could inject clicks.

## Reproduction on 2026-11-06

Environment: Windows 11, Ryzen 9 9950X, RTX 5070 Ti. A production 922 FFI viewer
connected to the installed background host and targeted a dedicated owned Tk
window. Every input checked foreground ownership and pointer hit-testing. No
input was sent to another application. Video ACKs in this probe simulate
consumption; they do not measure native presentation or phone latency.

The actual Windows pointer reached the intended position in 47–63 ms in two
local runs. Neither button-down nor button-up reached the window. A diagnostic
helper reported `SendInput` failure with Win32 5 (`ERROR_ACCESS_DENIED`), session
1, System integrity (`0x4000`) and `TokenUIAccess=0`. The generic Enigo error's
mention of UIPI alone was not sufficient to diagnose the cause.

The first candidate set `TokenUIAccess=1` on the restricted worker. Installed
acceptance disproved this diagnosis: the worker reported UIAccess=1 but still
returned Win32 5 and delivered neither button event. That token change is removed.

The desktop handle lacked `DESKTOP_JOURNALPLAYBACK`. A separate ordinary-user
process reproduced the boundary on the same owned window: binding a fresh thread
with the previous `0x181` access mask made `SendInput` return zero and Win32 5;
adding only journal-playback access (`0x1a1`) sent both events and the window
observed button-down and button-up. No token changes or desktop ACL changes were
needed. RustDesk's desktop binding requests `GENERIC_WRITE`, which includes this
right; pb-mapper now requests the specific additional right. See
[Microsoft's desktop access mapping](https://learn.microsoft.com/en-us/windows/win32/winstation/desktop-security-and-access-rights)
and [RustDesk's Windows binding](https://github.com/rustdesk/rustdesk/blob/master/src/platform/windows.cc).

The worker retains its restricted SYSTEM token without UIAccess, protected image,
owner/login binding, authenticated IPC, removed privileges and one-process job.
An opt-in Windows regression binds using the production access mask and submits
a zero-distance relative mouse move. It checks actual OS acceptance without
clicking, typing, moving the pointer or switching the visible desktop. The
installed final worker passed end-to-end event delivery below; ordinary-window
success alone does not establish secure-desktop or lock-screen operation.

Input failure now ends the control grant. It remains visible across subsequent
captures, queued button releases and stale presentation ACKs. The UI does not
automatically reacquire control after this failure. A deliberate new control
request can clear it.

## Reconnection

The connection page previously cleared its invitation after connecting, then
allowed an empty invitation to start an endless network-retry loop. It now
keeps the input in page memory, disables empty submission, and treats native
`INVALID_ARGUMENT` as terminal. Native validation precedes configuration waits
and connection ownership. The capability is not saved to a new persistent
store or written into logs.

The widget test exercises connect, navigation, manual disconnect, and reconnect
with the original input. Store tests cover terminal validation, cancellation,
transport retries, input ordering and explicit recovery after native input
failure. These are separate from real installed-service acceptance.

## Asynchronous encoder backpressure

The sender previously stopped calling the encoder when presentation credit was
exhausted. Windows' asynchronous Media Foundation encoder requires output to be
drained even when no new capture is submitted. A controlled worker regression
with a 128 KiB IDR demonstrated an already-completed output remaining unobserved
for over 250 ms. The candidate separates draining from submission; that test
drained in 2.4 ms with zero additional submissions.

An eight-second synthetic asynchronous encoder test used real relay/TLS,
60 fps source cadence and 350 ms delayed feedback on this Windows machine.
Separating draining reduced expired encoder outputs from 4 to 0 and keyframes
from 14 to 9; received frames changed from 154 to 157. The eight-frame credit
limit was unchanged. These measurements demonstrate the codec boundary fix,
not phone FPS or motion-to-photon latency.

Congestion delay also included local waiting for credit, while baseline
calibration started at transmission. Both now start at transmission; the local
250 ms expiry rule remains separate. A single optional startup status probe is
echoed directly by the viewer's network reader/writer, before decoder or display
work. It calibrates propagation delay without acknowledging any video, retiring
presentation debt, refreshing a control lease or enlarging the queue. Old
viewers ignore the optional field and retain frame-based calibration; new
viewers only send the echo after a host advertises it.

A second test distinguishes network propagation from consumer delay. A bounded,
pipelined TCP proxy delays each direction independently and imposes no bandwidth
limit. The host connects directly to the relay; the viewer connects through the
proxy. The synthetic source targets 4 Mbps / 60 fps. Windows debug results:

| Condition | Before network probe | After network probe |
| --- | --- | --- |
| 20 ms network RTT, 8 s | 452 consumed, 4 Mbps, 0 dropped | 451 consumed, 4 Mbps, 0 dropped |
| 350 ms network RTT, 16 s | 294 consumed, 256 kbps at 6 s / 1.3065 Mbps at end, 13 IDRs, 3 dropped | 310 consumed, stable 4 Mbps, 3 IDRs, 1 dropped |
| 20 ms RTT + 67 ms per-frame consumer, 8 s | 104 consumed, 256 kbps, 3 dropped | 104 consumed, 256 kbps, bounded queues |

Both columns include the earlier encoder-drain and send-clock fixes. All three
post-drain conditions had zero expired encoder outputs. The eight-frame and
512 KiB flight limits remain unchanged, so high-RTT throughput is still bounded
by the outstanding-frame limit. These tests establish neither native Android
presentation FPS nor RustDesk-equivalent end-to-end latency.

## Acceptance status

- Rust desktop: 46 library tests and two direct/relay session tests passed;
  workspace formatting and all-target/all-feature Clippy passed with warnings
  denied. Three explicit performance/slow-link tests and the interactive desktop
  permission regression are opt-in.
- Flutter: 22 focused desktop tests and analysis passed.
- FFI: all 39 library tests passed, including malformed-invitation/configuration
  cancellation.
- Encoder drain: asynchronous worker regression and real-TLS comparison passed.
- Legacy feedback peer: the explicit slow-link test ignores the new probe,
  reduces to 256 kbps under pressure, recovers to 9.057 Mbps / 960×540 by second
  30, and stops 4.18 seconds after presentation ACKs cease. Detailed counters,
  source references and commands are in
  [the video evidence](desktop-video-backpressure.json).
- Restricted UIAccess candidate: installed and failed real button delivery;
  UIAccess is removed. Desktop-handle access was reproduced with a controlled
  before/after fixture. The explicit native desktop-handle regression passed.
- Build 0.10.2+925: the complete protected Windows installation matches every
  packaged file hash and runs Automatic/Running. Its ordinary user UI reports
  background sharing enabled. Production 922 and 925 FFI viewers verified real
  left/right clicks, held drag, wheel, keyboard and text on the owned window.
  The high-resolution 925 direct input probe observed all 45 positions at both
  30 and 60 Hz; 60 Hz mean was 3.117 ms, p95 4.758 ms, maximum 6.224 ms on this
  same-machine path. These are actual cursor observations, not video latency.
- The same 925 FFI handle connected direct, disconnected, then connected through
  the deployed relay three times with the original invitation. Every session
  delivered fresh 1920×1080 H264 access units. ACKs simulate consumption and do
  not measure display FPS. These probes use an isolated TCP override; the saved
  QUIC app profile is unchanged. The standalone saved-QUIC probe still fails with
  Windows socket error 10054, as previously recorded; no broader cause is claimed.
- Build 925 Android ARM64/x64 APKs passed signing, identity, increasing version,
  ABI, 16 KiB alignment and upgrade checks. The x64 APK installed and launched on
  the Android 36 emulator with no crash-buffer entries. This is package smoke
  coverage only. [Installed input and reconnect evidence](desktop-925-installed-input.json).
- Physical-phone fluidity was subsequently rejected by the user; see the
  handoff below. Remote secure-desktop input and measured independent WAN
  performance remain unverified by these local checks.

No invitations, relay credentials or screen contents are included here.

## Performance handoff (2026-11-06)

### 用户要求与阻塞状态

用户在确认移动网络可以直连后反馈：即使直连，远程桌面仍明显卡顿，
“30 帧都不到”；RustDesk 的画面更流畅、帧率也更高。这是用户报告的体验与
帧率判断，目前没有同条件帧时间采样或 RustDesk 对照数据，不能当作实测基准。
问题优先级为 **P0 / 合并阻塞**：**流畅度问题没有解决并通过真机验收之前，
不得合并 PR #51。** 连通性、编译、单元测试通过不能解除这个阻塞。

**2026-11-10 更新：** 房主测试 stage7 构建（Windows `8f800b3`、安卓 `ae3df37`）后
反馈一切正常，并要求合并 PR #51，合并阻塞据此解除。这一认可基于房主的实际
使用，没有补录下文所述的真机帧时间采样与 RustDesk 对照。

用户要求本轮只记录、提交与交接，不继续优化。后续接手者应独立完成定位与
修复，不依赖本轮代理继续工作。本轮没有定位视频卡顿的最终根因。

### 接手起点与已确认边界

- 分支：`codex/issue-50-remote-desktop`，PR #51，代码基线
  `6a695dea4e11ddb8492f8ca846b7187809029fd4`，安装包 `0.10.2+927`。
  主工作目录可能在其他分支；操作前核对工作树，不混入其他任务的改动。
- Windows 11 / Ryzen 9 9950X / RTX 5070 Ti；手机 Vivo V2405DA / Android API 36，
  ARM64 APK versionCode 2927。完整安装包摘要和脱敏验收见
  [移动网络与日志验收](android-cellular-927.json)。
- 移动网络验收时 Wi-Fi 关闭，CipherRelay VPN 保持运行，pb-mapper 被排除于
  VPN 接管范围。此前 926 的应用 UID 实际经过 VPN，STUN 超时后回退中继；
  绕过规则生效后，927 应用显示 direct，提供端日志中的 10 次 direct 事件
  对端匹配手机蜂窝 IPv6，用户确认业务流量正常。
- 上述日志对应普通 `llm-frontend` 隧道，证明该环境能直连；它不测量视频帧率，
  也不能代替对发生卡顿的那一次**桌面会话**的路径、RTT 和丢包核验。
  用户报告桌面在直连时也卡顿，应保留此事实，后续补齐会话级证据。
- 同手机 shell 路由上的 SDK 0.7.0 与 0.10.2 均完成 IPv6 direct 和 HTTP 200。
  该对照与应用此前的 VPN 路由不同；没有证据支持把问题归结为 SDK 版本退化。
  `c9bdc37..6a695de` 的 client/protocol/core 源码相同，本轮未修改 P2P 协议。
- 已修复桌面输入权限、异步编码器输出排空和反馈基线；历史合成测试不能
  证明 Android 实际显示流畅。八帧 / 512 KiB 的在途上限仍在，可能限制高 RTT
  吞吐，但它尚未被证实是本次真机卡顿的根因。
- 927 默认持久化 INFO 日志，位于配置目录的 `logs/`，分片保留约一小时。
  复现后及时保存脱敏证据；该日志不是完整的视频性能追踪。

### 待查明与优化顺序

1. **先建立同条件基线。** 同一 Windows 主机、手机、网络、分辨率、刷新率、
   相近画质和动态画面分别运行 pb-mapper 与 RustDesk，记录版本、路径、
   编解码器、实际码率、温度和持续时间。记录真实呈现 FPS、帧间隔分布、
   长停顿、端到端延迟与输入到显示延迟；不能用配置 FPS、接收帧数或模拟 ACK
   代替屏幕呈现。跨端时间戳需先处理时钟偏差，必要时使用外部拍摄对照。
2. **贯通视频全链路。** 用同一帧标识关联采集、编码提交/输出、发送/接收、
   FFI/Dart 交接、MediaCodec 入队/输出、纹理消费及反馈返回；按阶段统计耗时、
   排队深度、丢弃原因、IDR 请求、码率与分辨率变化。先复用已有指标，避免
   逐帧刷盘改变被测性能。
3. **按证据排查主机与传输。** 查看采集节拍、硬件编码配置和异步输出排空，
   以及实际 RTT/丢包、信用耗尽、窗口与码率控制是否误把本地等待当作拥塞。
   不能直接放大队列或伪造呈现 ACK 来提高数字；过期画面和输入安全边界必须保留。
4. **按证据排查 Android 呈现。** 查看压缩帧跨 FFI/MethodChannel 的复制与调度、
   MediaCodec 缓冲和输出释放节拍、SurfaceTexture/Flutter 消费、反馈发送时机，
   是否出现串行等待、积压、频繁关键帧恢复或热降频。原生输出回调与最终屏幕
   呈现并非同一个测量点，需要解释指标的边界。
5. **优化后重复真机对照。** 对同一动态场景提供前后数据和 RustDesk 对照，
   同时覆盖重连与慢消费者，确认有界队列、恢复行为和输入权限未退化。

关键入口（均相对仓库根目录）：

| 阶段 | 文件 |
| --- | --- |
| 主机与编码 | `crates/pb-mapper-desktop/src/host.rs`、`worker.rs`、`video/windows.rs` |
| 接收、帧队列和反馈 | `crates/pb-mapper-desktop/src/viewer.rs`、`media.rs`、`congestion.rs` |
| 原生 FFI | `ui/native/pb_mapper_ffi/src/desktop.rs` |
| Flutter 状态和呈现 | `ui/lib/src/state/desktop_store.dart`、`ui/lib/src/ffi/desktop_video.dart`、`ui/lib/src/pages/desktop_viewer.dart` |
| Android 解码与纹理 | `ui/android/app/src/main/kotlin/com/pb_mapper/ui/DesktopVideo.kt` |

### 解除合并阻塞需要的证据

必须有真机动态画面的稳定帧率、帧间隔与延迟记录，解释当前“不足 30 帧”
卡顿的根因，且同条件下达到用户认可的流畅度与 RustDesk 对照表现。30 fps
不能被当成自动通过门槛，平均帧率也不能掩盖长停顿或严重画质退化。
补齐实测证据并获得用户对体验的认可后，才可重新评估合并；本轮状态为未通过。
（2026-11-10：房主已认可体验，阻塞解除，见上文“用户要求与阻塞状态”。）
远程安全桌面、SCM/系统重启、休眠恢复等既有未验收项仍然有效。
