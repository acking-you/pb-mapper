# Bug 修复记录

本文记录 pb-mapper 常见问题的发现过程与修复方案。

## 1) Flutter UI 在检测 pb-mapper 可用性时卡顿

- 现象：打开 UI 后，状态检测会导致界面卡顿甚至无响应。
- 发现过程：复现时观察到 FFI 调用阻塞主线程。
- 根因：FFI 调用运行在 UI isolate，阻塞了渲染线程。
- 修复：将所有 FFI 调用移到后台 isolate（`compute`），并增加轻量缓存与轮询，保持 UI 流畅。

## 2) Linux 启动报错 `libpb_mapper_ffi.so` 找不到

- 现象：启动时报错 `Failed to load dynamic library 'libpb_mapper_ffi.so'`。
- 发现过程：`flutter build linux` 后运行 bundle 即复现。
- 根因：FFI 动态库未被编译/拷贝到应用 bundle。
- 修复：增加各平台的构建脚本与 Makefile 目标，统一编译并把 FFI 库放到正确位置（Release 流水线同样复用）。

## 3) `Cannot start a runtime from within a runtime`

- 现象：使用自定义域名解析时触发 `trust-dns-resolver` panic。
- 发现过程：输入域名后立刻崩溃并显示 runtime 嵌套错误。
- 根因：同步解析路径在 async runtime 内部调用了 `block_on`。
- 修复：新增异步解析路径，并在运行时中禁用同步解析逻辑。

## 4) Register/Connect 状态卡片不自动更新

- 现象：Register/Connect 后卡片状态不变化，必须手动刷新。
- 发现过程：复现并确认 UI 没有触发状态重新拉取。
- 根因：操作完成后没有刷新状态，且依赖了过期缓存。
- 修复：在操作后进行短轮询刷新，且在可用性检测时加入重试轮询。

## 5) 视频下载连接频繁重置且日志刷屏

- 现象：访问某视频会频繁断开并刷 `checksum`/`connection reset` 错误。
- 发现过程：追踪到 `forward.rs` 的读写错误与频繁重连。
- 根因：转发流程未支持半关闭，正常 EOF 被当作错误处理。
- 修复：增加半关闭（`shutdown`）支持，允许双向转发自然结束，并将预期断开（EOF/Reset/BrokenPipe）降级为 debug 日志。
