#!/bin/bash

# VibeVoice 一键安装脚本 (修复版)

set -e

echo "🎙️ VibeVoice 一键安装脚本 (修复版)"
echo "=================================="

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

# 检查CUDA环境
echo "🔍 检查CUDA环境..."
HAS_GPU=$(nvidia-smi &> /dev/null && echo "true" || echo "false")
HAS_NVCC=$(nvcc --version &> /dev/null && echo "true" || echo "false")

if [ "$HAS_GPU" = true ]; then
    echo "✅ 检测到NVIDIA GPU"
    if [ "$HAS_NVCC" = true ]; then
        echo "✅ CUDA开发环境完整"
        USE_GPU=true
        BUILD_FLASH_ATTN=true
        
        # 根据nvcc位置设置正确的CUDA_HOME
        NVCC_PATH=$(which nvcc)
        if [ "$NVCC_PATH" = "/usr/bin/nvcc" ]; then
            export CUDA_HOME=/usr
            echo "🔧 设置CUDA_HOME=/usr (基于nvcc在/usr/bin)"
        elif [ "$NVCC_PATH" = "/usr/local/cuda/bin/nvcc" ]; then
            export CUDA_HOME=/usr/local/cuda
            echo "🔧 设置CUDA_HOME=/usr/local/cuda (基于nvcc在/usr/local/cuda/bin)"
        else
            # 从nvcc路径推断CUDA_HOME (去掉/bin/nvcc部分)
            CUDA_HOME_DETECTED=$(dirname $(dirname $NVCC_PATH))
            export CUDA_HOME=$CUDA_HOME_DETECTED
            echo "🔧 设置CUDA_HOME=$CUDA_HOME (基于nvcc路径: $NVCC_PATH)"
        fi
        
        # 验证CUDA_HOME设置是否正确
        if [ ! -f "$CUDA_HOME/bin/nvcc" ] && [ ! -f "/usr/bin/nvcc" ]; then
            echo "⚠️  CUDA_HOME验证失败，flash-attn可能无法编译"
        else
            echo "✅ CUDA_HOME验证成功"
        fi
    else
        echo "⚠️  有GPU但缺少CUDA编译器，将尝试预编译版本"
        USE_GPU=true
        BUILD_FLASH_ATTN=false
    fi
else
    echo "⚠️  未检测到NVIDIA GPU，使用CPU版本"
    USE_GPU=false
    BUILD_FLASH_ATTN=false
fi

# 创建虚拟环境
echo "🔧 创建虚拟环境..."
uv venv --python 3.12

# 激活虚拟环境并安装基础依赖
echo "📦 安装基础依赖..."
source .venv/bin/activate

if [ "$USE_GPU" = true ]; then
    echo "🚀 安装GPU版本依赖..."
    # 安装GPU版本的torch
    uv pip install torch --index-url https://download.pytorch.org/whl/cu124
    
    if [ "$BUILD_FLASH_ATTN" = true ]; then
        echo "🔨 从源码编译flash-attn..."
        # 确保有wheel包用于编译
        uv pip install wheel
        
        # 先安装其他依赖
        uv pip install accelerate==1.6.0 transformers==4.51.3 llvmlite>=0.40.0 numba>=0.57.0 diffusers tqdm numpy scipy librosa ml-collections absl-py gradio av aiortc
        
        # 编译并安装flash-attn
        echo "⚡ 编译flash-attn (这可能需要几分钟)..."
        if uv pip install flash-attn --no-build-isolation; then
            echo "✅ flash-attn编译安装成功"
        else
            echo "❌ flash-attn编译失败，继续安装其他组件..."
            echo "  模型仍可正常运行，但可能速度稍慢"
        fi
    else
        echo "📦 尝试安装预编译的flash-attn..."
        # 先安装其他依赖
        uv pip install accelerate==1.6.0 transformers==4.51.3 llvmlite>=0.40.0 numba>=0.57.0 diffusers tqdm numpy scipy librosa ml-collections absl-py gradio av aiortc
        
        # 确保有wheel包
        uv pip install wheel
        
        # 尝试不同的flash-attn安装策略
        echo "🔄 尝试多种flash-attn安装方法..."
        
        # 方法1: 预编译wheel
        if uv pip install flash-attn --find-links https://flash-attn.s3.amazonaws.com/releases/flash_attn-2.8.3+cu124torch2.6cxx11abiFALSE-cp312-cp312-linux_x86_64.whl 2>/dev/null; then
            echo "✅ flash-attn预编译版本安装成功"
        # 方法2: 从PyPI安装预编译版本
        elif pip install flash-attn --no-build-isolation 2>/dev/null; then
            echo "✅ flash-attn PyPI版本安装成功"  
        # 方法3: 跳过flash-attn
        else
            echo "⚠️  flash-attn安装失败，继续安装其他组件..."
            echo "  模型仍可正常运行，但可能速度稍慢"
            echo "  如需最佳性能，可手动安装CUDA Toolkit:"
            echo "  sudo apt install nvidia-cuda-toolkit"
        fi
    fi
else
    echo "💻 安装CPU版本依赖..."
    # 先安装CPU版本的torch
    uv pip install torch --index-url https://download.pytorch.org/whl/cpu
    # 安装其他依赖但跳过flash-attn
    uv pip install accelerate==1.6.0 transformers==4.51.3 llvmlite>=0.40.0 numba>=0.57.0 diffusers tqdm numpy scipy librosa ml-collections absl-py gradio av aiortc
    echo "⚠️  注意: 已跳过flash-attn安装，使用CPU模式运行"
fi

# 安装当前包
echo "📋 安装VibeVoice包..."
uv pip install -e .

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
    echo "  • 快速开始: ./quick_start.sh"
    echo "  • Gradio界面: uv run python demo/gradio_demo.py --model_path vibevoice/VibeVoice-1.5B"
    echo "  • 自定义端口: uv run python demo/gradio_demo.py --model_path vibevoice/VibeVoice-1.5B --port 8080"
    echo "  • 批量处理: uv run python demo/inference_from_file.py --model_path vibevoice/VibeVoice-1.5B --input_file script.txt"
    if [ "$USE_GPU" = true ]; then
        echo ""
        echo "🚀 GPU模式运行提示:"
        echo "  • 已启用GPU加速，推理速度较快"
        
        # 检查flash-attn是否成功安装
        if uv run python -c "import flash_attn; print('flash-attn已安装')" 2>/dev/null; then
            echo "  • ✅ flash-attn已安装，享受最优性能"
            echo "  • 模型将自动使用flash_attention_2实现"
        else
            echo "  • ⚠️  flash-attn未安装，使用标准attention实现"
            echo "  • 如需最优性能，确保CUDA开发工具包完整安装"
        fi
    else
        echo ""
        echo "⚠️  CPU模式运行提示:"
        echo "  • 推理速度较慢，建议使用较短的文本"
        echo "  • 如需GPU加速，请安装NVIDIA GPU驱动和CUDA工具包"
    fi
else
    echo "❌ VibeVoice 导入失败，正在尝试修复..."
    # 尝试安装缺失的依赖
    uv pip install soundfile
    if uv run python -c "import vibevoice; print('✅ VibeVoice 导入成功!')"; then
        echo "🎉 修复成功！"
    else
        echo "❌ 安装失败，请检查错误信息"
        exit 1
    fi
fi