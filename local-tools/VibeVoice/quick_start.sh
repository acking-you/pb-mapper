#!/bin/bash

# VibeVoice 快速使用指南

echo "🎙️ VibeVoice 快速使用指南"
echo "=========================="
echo ""

if [ ! -d ".venv" ]; then
    echo "❌ 虚拟环境不存在，请先运行: ./install.sh"
    exit 1
fi

echo "📋 可用选项:"
echo ""
echo "1. 直接调用示例 (推荐用于 PyO3 集成)"
echo "   uv run python direct_usage_example.py"
echo ""
echo "2. 轻量级 Web 服务 (FastAPI)"
echo "   uv run python tts_service.py --host 127.0.0.1 --port 8000"
echo ""
echo "3. 原版 Gradio 演示 (功能完整但依赖较重)"
echo "   uv run python demo/gradio_demo.py --model_path microsoft/VibeVoice-1.5B"
echo ""

read -p "选择一个选项 (1-3): " choice

case $choice in
    1)
        echo "🚀 运行直接调用示例..."
        uv run python direct_usage_example.py
        ;;
    2)
        echo "🚀 启动轻量级 Web 服务..."
        echo "📖 启动后访问 http://127.0.0.1:8000/docs 查看 API 文档"
        uv run python tts_service.py --host 127.0.0.1 --port 8000
        ;;
    3)
        echo "🚀 启动 Gradio 演示..."
        uv run python demo/gradio_demo.py --model_path microsoft/VibeVoice-1.5B --share
        ;;
    *)
        echo "❌ 无效选择"
        exit 1
        ;;
esac