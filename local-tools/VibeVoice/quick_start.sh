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
echo "1. Gradio Web 界面 (推荐，功能完整的本地服务)"
echo "   • 提供完整的 Web UI 界面"
echo "   • 支持实时语音合成和播放"
echo "   • 可以通过浏览器访问和使用"
echo ""
echo "2. 文件批处理推理 (适合批量处理)"
echo "   • 从文件读取对话脚本"
echo "   • 支持多说话人对话合成"
echo "   • 适合自动化脚本调用"
echo ""

read -p "选择一个选项 (1-2): " choice

case $choice in
    1)
        echo "🚀 启动 Gradio Web 界面..."
        echo "📖 启动后将在浏览器中自动打开，或手动访问显示的地址"
        echo "💡 使用 Ctrl+C 停止服务"
        echo ""
        read -p "选择端口 (默认7860，直接回车使用默认): " port
        if [ -z "$port" ]; then
            port=7860
        fi
        echo "🚀 启动在端口 $port..."
        uv run python demo/gradio_demo.py --model_path vibevoice/VibeVoice-1.5B --port $port
        ;;
    2)
        echo "🚀 运行文件批处理推理..."
        echo "💡 使用示例："
        echo "   uv run python demo/inference_from_file.py --model_path vibevoice/VibeVoice-1.5B --input_file your_script.txt"
        echo ""
        echo "📝 脚本文件格式示例："
        echo "   Speaker1: 你好，今天天气真不错！"
        echo "   Speaker2: 是啊，很适合出去走走。"
        echo ""
        read -p "是否查看帮助信息? (y/n): " show_help
        if [ "$show_help" = "y" ] || [ "$show_help" = "Y" ]; then
            uv run python demo/inference_from_file.py --help
        else
            echo "请按照上述格式准备脚本文件，然后运行相应命令。"
        fi
        ;;
    *)
        echo "❌ 无效选择"
        exit 1
        ;;
esac