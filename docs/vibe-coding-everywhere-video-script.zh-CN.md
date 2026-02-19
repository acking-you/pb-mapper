# Vibe Coding Everywhere：开源组合 HAPI + pb-mapper，解放双手让 AI 随时随地帮你写代码

> 视频时长：约 10-15 分钟
> 适用平台：B 站
> 配套资源：PPT (`docs/assets/vibe-coding-everywhere.pptx`)、封面 (`docs/assets/vibe-coding-everywhere-cover.svg`)

---

## Part 1: 开场 Hook + 成果演示（~2 min）

### 【画面：手机屏幕录屏 + 电脑屏幕同步】

**旁白：**

你有没有遇到过这种情况——

用 Claude Code 或者 Codex 写代码，写到一半，需要去倒杯水、遛个狗、或者干脆出门办事。回来一看，Agent 早就停在那里等你审批权限了，整个工作流直接中断。

> 💡 PPT 第 2 页：痛点展示

如果我告诉你，你可以掏出手机，随时随地继续操控 AI Agent 写代码呢？

**【演示最终效果】**

看，我现在在手机浏览器里打开了一个 Web 页面。我输入一条消息："帮我给这个函数加上错误处理"——

（画面切到电脑屏幕）

电脑上的 Claude Code 立刻开始执行。弹出权限请求？我在手机上点一下"允许"就行。

代码改好了，测试通过，全程我没碰电脑。

> 💡 PPT 第 3 页：解决方案总览

这就是今天要教你搭建的 **Vibe Coding Everywhere** 环境。用到两个开源项目：
- **HAPI**：远程控制 AI Agent
- **pb-mapper**：网络穿透，让你在任何地方都能访问

下面我们从零开始。

---

## Part 2: HAPI 项目介绍（~2 min）

### 【画面：PPT 架构图 + 浏览器展示 HAPI 仓库】

**旁白：**

> 💡 PPT 第 4 页：HAPI 是什么

先说 HAPI。它的全称是 **Human API**，核心理念很简单——它不替换你的 AI Agent，而是通过 Hook、MCP、SDK 这些旁路机制，"监听"并"控制" Agent 的行为。

架构分三层：

1. **CLI 层**：你的 AI Agent（Claude Code、Codex 等）运行在这里
2. **Hub 层**：中间枢纽，负责消息中转和状态管理
3. **Web 层**：你在浏览器里看到的控制界面

> 💡 PPT 第 5 页：HAPI 核心能力

HAPI 有两种工作模式：

- **Local 模式**：你坐在电脑前，直接用终端操作，Hub 在后台默默同步状态
- **Remote 模式**：你离开电脑，切到手机上操作，Hub 把你的指令转发给 Agent

关键是——**切换模式不会中断会话**。你在 Local 模式下写到一半，切成 Remote，手机上接着来，完全无缝。

而且 HAPI 支持多种 Agent：Claude Code、Codex、Gemini CLI、OpenCode，不挑食。

---

## Part 3: 本地快速搭建 HAPI（~3 min）

### 【画面：终端操作录屏】

**旁白：**

> 💡 PPT 第 6 页：本地搭建 HAPI

好，动手搭建。HAPI 用 Bun 运行时，所以第一步先装 Bun：

```bash
curl -fsSL https://bun.sh/install | bash
```

然后 clone HAPI 仓库：

```bash
git clone https://github.com/humanapi-corp/hapi.git
cd hapi
```

安装依赖并构建单文件可执行程序：

```bash
bun install
bun run build:single-exe
```

构建完成后，把产物路径加到 PATH 里。具体路径看你的系统，一般在 `dist/` 目录下。

现在试一下：

```bash
hapi
```

HAPI 会自动启动 Claude Code，同时在后台跑一个 Hub 服务。

打开浏览器，访问 `http://localhost:3006`——

**【画面：浏览器打开 HAPI Web 界面】**

这就是 HAPI 的 Web 控制台。左边是对话历史，右边可以发消息、审批权限。

我在这里输入一条消息试试："列出当前目录的文件"——

（画面切到终端）

看，Claude Code 收到了指令，开始执行。它要调用 `ls` 命令，弹出权限请求——我在 Web 界面点"允许"——执行完成，结果同步显示在 Web 上。

到这里，Local 模式已经跑通了。但有个问题——

---

## Part 4: 让 HAPI Web 在 Everywhere 可访问（~4 min）

### 【画面：PPT + 终端操作】

**旁白：**

> 💡 PPT 第 7 页：localhost 限制

Hub 默认监听 `localhost:3006`，只有本机能访问。你拿手机连，连不上。

怎么办？这就是 pb-mapper 登场的时候了。

> 💡 PPT 第 8 页：pb-mapper 介绍

pb-mapper 是一个 Rust 写的网络穿透工具。它的特点是：

- **单端口多服务映射**：一个公网端口可以映射多个内网服务
- **Rust 实现**：性能好，延迟接近直连
- **可选加密**：支持传输加密，保护数据安全
- **跨平台**：Linux、macOS、Windows、Android、iOS 都支持

它的工作原理很简单：你有一台公网服务器，pb-mapper-server 跑在上面。你的 PC 通过 pb-mapper 把本地服务"注册"上去，然后手机通过 pb-mapper "订阅"这个服务，就能访问了。

### Step 1：部署 pb-mapper-server 到公网服务器

> 💡 PPT 第 9 页：一键部署 pb-mapper-server

这一步最简单。如果你用 Claude Code 或者其他 AI 编程助手，直接用 pb-mapper 项目提供的部署 skill：

```
/pb-mapper-server-deploy
```

AI 会自动帮你：
1. 编译 pb-mapper-server
2. 通过 SSH 上传到你的服务器
3. 配置 systemd 服务
4. 启动并验证

整个过程全自动，你只需要提供服务器地址和 SSH 密钥。

### Step 2：PC 端注册 HAPI Hub 服务

> 💡 PPT 第 10 页：注册 + 订阅

在你的 PC 上打开 pb-mapper 的 Flutter UI App。

操作很直观：
1. 填入公网服务器地址（就是刚才部署 pb-mapper-server 的那台）
2. 设置一个服务 Key，比如 `hapi-hub`
3. 本地服务地址填 `127.0.0.1:3006`（HAPI Hub 的地址）
4. 选择 TCP 协议
5. 点击"注册"

注册成功后，你的 HAPI Hub 就通过 pb-mapper 暴露到公网了。

### Step 3（推荐）：手机端订阅服务

在手机上安装 pb-mapper Flutter UI App（Android/iOS 都有）。

打开 App：
1. 填入同一台公网服务器地址
2. 输入服务 Key：`hapi-hub`
3. 设置本地监听端口，比如 `3006`
4. 点击"订阅"

现在，手机上访问 `http://localhost:3006` 就能打开 HAPI Web 了！

### Step 3 备选：不装 App，直接公网访问

> 如果你不想在手机上装 App，还有一种方式——

在 pb-mapper-server 所在的公网服务器上，用 AI 编程助手执行：

```
/pb-mapper-client-cli-deploy
```

这会在服务器上部署一个 client-cli，订阅 `hapi-hub` 服务并监听一个公网端口。

然后你在手机浏览器里直接输入 `http://你的服务器IP:端口` 就能访问 HAPI Web。

> ⚠️ 注意：这种方式会把 HAPI Hub 直接暴露在公网，建议配合加密和访问控制使用。

---

## Part 5: 完整演示 + 总结（~2 min）

### 【画面：手机 + 电脑分屏录制】

**旁白：**

好，现在来一个完整的演示。

我现在在外面，只有手机。打开浏览器，进入 HAPI Web。

我发一条消息："帮我在 src/utils/ 下新建一个 retry.rs，实现一个带指数退避的重试函数"。

（画面切到电脑屏幕）

电脑上的 Claude Code 收到了任务，开始分析、写代码。

它需要创建文件——权限请求弹出来了。我在手机上点"允许"。

文件创建好了，代码写完了。Claude Code 还自动跑了一下 `cargo check`，编译通过。

整个过程，我只用手机操作了两次：发消息、批准权限。代码在电脑上自动完成。

> 💡 PPT 第 11 页：完整链路图

回顾一下整条链路：

```
手机浏览器 → pb-mapper（网络穿透） → HAPI Hub → AI Agent（Claude Code） → 代码执行
```

> 💡 PPT 第 12 页：总结 + 链接

**总结：**

- **HAPI** 负责远程控制——让你在任何设备上操控 AI Agent
- **pb-mapper** 负责网络穿透——让内网服务在任何地方可达
- 两者结合 = **Vibe Coding Everywhere**

项目链接：
- HAPI：`https://github.com/humanapi-corp/hapi`
- pb-mapper：`https://github.com/pysrc/pb-mapper`

如果觉得有用，别忘了一键三连。我们下期见！

---

## 附录：关键命令速查

| 步骤 | 命令 |
|------|------|
| 安装 Bun | `curl -fsSL https://bun.sh/install \| bash` |
| 构建 HAPI | `cd hapi && bun install && bun run build:single-exe` |
| 启动 HAPI | `hapi` |
| 部署 pb-mapper-server | AI 助手中执行 `/pb-mapper-server-deploy` |
| 部署 client-cli（备选） | AI 助手中执行 `/pb-mapper-client-cli-deploy` |
| HAPI Web 地址 | `http://localhost:3006` |
