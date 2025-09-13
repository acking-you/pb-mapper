# IndexTTS 快速启动指南

IndexTTS 工业级可控零样本文本转语音系统

## 🚀 快速开始

### 1. 一键安装
```bash
./install.sh
```

安装脚本会自动完成：
- ✅ uv 包管理器安装
- ✅ Python 3.11 虚拟环境创建
- ✅ 系统依赖安装 (git-lfs, build-essential, clang-14)
- ✅ Python 依赖安装
- ✅ CUDA 扩展编译（如果有GPU）
- ✅ 模型文件自动下载（约3GB）
- ✅ 环境测试验证

### 2. 启动 WebUI
```bash
./start_webui.sh
```

## 📋 使用选项

### 启动选项
```bash
./start_webui.sh --port 8080        # 指定端口
./start_webui.sh --public           # 允许公网访问
./start_webui.sh --background       # 后台运行
./start_webui.sh -d --port 8080     # 后台运行指定端口
```

### 管理后台服务
```bash
# 查看日志
tail -f logs/webui.log

# 停止后台服务
pkill -f 'webui.py'

# 或使用PID
kill $(cat logs/webui.pid)
```

## 🔧 系统要求

- Python 3.11+
- CUDA 12.0+ (可选，用于GPU加速)
- 至少 8GB 内存
- 约 5GB 磁盘空间（包含模型文件）

## 💡 功能特性

- 🎯 **零样本TTS**: 无需训练即可克隆任意说话人声音
- 🚀 **GPU加速**: 支持CUDA加速推理
- 🌐 **Web界面**: 友好的Gradio用户界面
- 🔧 **完全自动化**: 一键安装所有依赖和模型
- 📱 **跨平台**: 支持Linux/Windows/macOS

## 🛠️ 故障排除

### CUDA编译问题
如遇到CUDA编译错误，脚本会自动：
1. 检测并使用clang-14编译器
2. 回退到CPU实现
3. 提供详细错误信息

### 端口占用
```bash
# 检查端口占用
netstat -tlnp | grep 7860

# 使用不同端口
./start_webui.sh --port 8080
```

### 模型文件问题
如自动下载失败，可手动下载：
```bash
git lfs install
git clone https://huggingface.co/IndexTeam/Index-TTS checkpoints_temp
cp checkpoints_temp/*.pth checkpoints/
cp checkpoints_temp/*.model checkpoints/
```

## 📁 文件结构

```
.
├── install.sh          # 一键安装脚本
├── start_webui.sh       # WebUI启动脚本
├── webui.py             # 主程序
├── checkpoints/         # 模型文件目录
├── logs/                # 日志文件目录
├── config.json          # 配置文件
└── .venv/               # Python虚拟环境
```

---

🎙️ **IndexTTS** - 让AI语音合成更简单！