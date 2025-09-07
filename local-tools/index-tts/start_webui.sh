#!/bin/bash

# IndexTTS WebUI 启动脚本

echo "🚀 IndexTTS WebUI 启动脚本"
echo "==========================="

# 检查当前目录
if [ ! -f "webui.py" ]; then
    echo "❌ 错误: 请在 IndexTTS 项目根目录运行此脚本"
    exit 1
fi

# 检查是否已安装
if [ ! -d ".venv" ]; then
    echo "❌ 虚拟环境不存在，请先运行安装脚本："
    echo "   ./install.sh"
    exit 1
fi

# 检查关键文件
echo "🔍 检查环境..."
required_files=(
    "checkpoints/bigvgan_generator.pth"
    "checkpoints/bpe.model"
    "checkpoints/gpt.pth"
)

missing_files=()
for file in "${required_files[@]}"; do
    if [ ! -f "$file" ]; then
        missing_files+=("$file")
    fi
done

if [ ${#missing_files[@]} -gt 0 ]; then
    echo "❌ 缺少必要的模型文件:"
    for file in "${missing_files[@]}"; do
        echo "  - $file"
    done
    echo ""
    echo "请先运行安装脚本下载模型文件："
    echo "   ./install.sh"
    exit 1
fi

echo "✅ 环境检查通过"

# 解析命令行参数
PORT=7860
HOST="127.0.0.1"
BACKGROUND=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --port)
            PORT="$2"
            shift 2
            ;;
        --host)
            HOST="$2"
            shift 2
            ;;
        --public)
            HOST="0.0.0.0"
            shift
            ;;
        --background|-d)
            BACKGROUND=true
            shift
            ;;
        --help|-h)
            echo "用法: $0 [选项]"
            echo ""
            echo "选项:"
            echo "  --port PORT        指定端口 (默认: 7860)"
            echo "  --host HOST        指定主机地址 (默认: 127.0.0.1)"
            echo "  --public           允许公网访问 (等同于 --host 0.0.0.0)"
            echo "  --background, -d   后台运行"
            echo "  --help, -h         显示此帮助信息"
            echo ""
            echo "示例:"
            echo "  $0                 # 本地启动，端口7860"
            echo "  $0 --port 8080     # 指定端口8080"
            echo "  $0 --public        # 允许公网访问"
            echo "  $0 -d --port 8080  # 后台运行在端口8080"
            echo ""
            exit 0
            ;;
        *)
            echo "❌ 未知参数: $1"
            echo "使用 --help 查看帮助"
            exit 1
            ;;
    esac
done

# 检查端口是否被占用
if command -v netstat &> /dev/null; then
    if netstat -tlnp 2>/dev/null | grep -q ":$PORT "; then
        echo "⚠️  端口 $PORT 已被占用"
        echo "   请使用 --port 指定其他端口，或停止占用端口的进程"
        exit 1
    fi
elif command -v ss &> /dev/null; then
    if ss -tlnp 2>/dev/null | grep -q ":$PORT "; then
        echo "⚠️  端口 $PORT 已被占用"
        echo "   请使用 --port 指定其他端口，或停止占用端口的进程"
        exit 1
    fi
fi

# 创建日志目录
if [ ! -d "logs" ]; then
    mkdir -p logs
fi

# 启动信息
echo ""
echo "📋 启动配置:"
echo "   • 地址: http://$HOST:$PORT"
echo "   • 模式: $([ "$BACKGROUND" = true ] && echo "后台运行" || echo "前台运行")"
echo "   • 日志: logs/webui.log"
echo ""

# 设置PyTorch库路径
echo "🔧 设置环境变量..."
PYTORCH_LIB_PATH=$(uv run python -c "import torch; import os; print(os.path.join(os.path.dirname(torch.__file__), 'lib'))" 2>/dev/null)
if [ -n "$PYTORCH_LIB_PATH" ]; then
    export LD_LIBRARY_PATH="$PYTORCH_LIB_PATH:$LD_LIBRARY_PATH"
    echo "   PyTorch库路径: $PYTORCH_LIB_PATH"
fi

if [ "$HOST" = "0.0.0.0" ]; then
    echo "🌐 公网访问模式已启用"
    echo "   外部设备可通过以下地址访问："
    # 尝试获取本机IP
    if command -v hostname &> /dev/null; then
        LOCAL_IP=$(hostname -I | awk '{print $1}' 2>/dev/null || echo "YOUR_IP")
        if [ "$LOCAL_IP" != "YOUR_IP" ]; then
            echo "   http://$LOCAL_IP:$PORT"
        fi
    fi
    echo "   http://localhost:$PORT (本机)"
    echo ""
fi

# 后台运行
if [ "$BACKGROUND" = true ]; then
    echo "🔄 启动后台服务..."
    
    # 创建PID文件
    PID_FILE="logs/webui.pid"
    
    # 检查是否已有实例运行
    if [ -f "$PID_FILE" ]; then
        OLD_PID=$(cat "$PID_FILE")
        if kill -0 "$OLD_PID" 2>/dev/null; then
            echo "⚠️  检测到已有实例运行 (PID: $OLD_PID)"
            echo "   停止现有实例..."
            kill "$OLD_PID"
            sleep 2
        fi
    fi
    
    # 启动后台进程
    nohup uv run python webui.py --host "$HOST" --port "$PORT" > logs/webui.log 2>&1 &
    NEW_PID=$!
    echo $NEW_PID > "$PID_FILE"
    
    # 等待启动
    sleep 3
    if kill -0 "$NEW_PID" 2>/dev/null; then
        echo "✅ 后台服务启动成功 (PID: $NEW_PID)"
        echo "💡 使用以下命令管理服务:"
        echo "   查看日志: tail -f logs/webui.log"
        echo "   停止服务: kill $NEW_PID"
        echo "   或使用:   pkill -f 'webui.py'"
    else
        echo "❌ 后台服务启动失败"
        echo "   请查看日志文件: logs/webui.log"
        exit 1
    fi
else
    # 前台运行
    echo "🚀 启动 IndexTTS WebUI..."
    echo "💡 使用 Ctrl+C 停止服务"
    echo "💡 日志同时输出到: logs/webui.log"
    echo ""
    
    # 启动并输出到日志文件
    uv run python webui.py --host "$HOST" --port "$PORT" 2>&1 | tee logs/webui.log
fi