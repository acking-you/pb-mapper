#!/bin/bash

# VibeVoice 一键安装脚本

set -e

echo "🎙️ VibeVoice 一键安装脚本"
echo "=========================="

# 检查当前目录
if [ ! -f "pyproject.toml" ]; then
    echo "❌ 错误: 请在 VibeVoice 项目根目录运行此脚本"
    exit 1
fi

echo "📍 当前目录: $(pwd)"

# 检查 uv 是否已安装
if ! command -v uv &> /dev/null; then
    echo "📦 正在安装 uv..."
    curl -LsSf https://astral.sh/uv/install.sh | sh
    source $HOME/.cargo/env
fi

echo "✅ uv 已安装"

# 创建虚拟环境并安装依赖
echo "🔧 创建虚拟环境并安装依赖..."
uv sync

echo "📂 检查安装结果..."
if [ -d ".venv" ]; then
    echo "✅ 虚拟环境创建成功: .venv/"
else
    echo "❌ 虚拟环境创建失败"
    exit 1
fi

# 测试 VibeVoice 导入
echo "🧪 测试 VibeVoice 导入..."
if uv run python -c "import vibevoice; print('✅ VibeVoice 导入成功!')"; then
    echo "🎉 安装完成!"
    echo ""
    echo "📋 可用命令:"
    echo "  • 测试导入: uv run python -c 'import vibevoice'"
    echo "  • 启动1.5B模型演示: uv run python demo/gradio_demo.py --model_path microsoft/VibeVoice-1.5B"
    echo "  • 启动Large模型演示: uv run python demo/gradio_demo.py --model_path microsoft/VibeVoice-Large"
    echo "  • 查看演示脚本: ./run_demo.sh"
else
    echo "❌ VibeVoice 导入失败"
    exit 1
fi