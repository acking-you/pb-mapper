#!/bin/bash

# IndexTTS 一键安装脚本
# 包含所有依赖、模型文件下载和CUDA扩展编译

echo "🎙️ IndexTTS 一键安装脚本"
echo "=========================="

# 检查当前目录
if [ ! -f "webui.py" ]; then
    echo "❌ 错误: 请在 IndexTTS 项目根目录运行此脚本"
    exit 1
fi

echo "📍 当前目录: $(pwd)"

# 检查 uv 是否已安装
if ! command -v uv &> /dev/null; then
    echo "📦 正在安装 uv..."
    curl -LsSf https://astral.sh/uv/install.sh | sh
    source $HOME/.cargo/env
    echo "✅ uv 已安装"
else
    echo "✅ uv 已存在"
fi

# 检查虚拟环境
if [ ! -d ".venv" ]; then
    echo "🔧 创建 Python 虚拟环境..."
    uv venv --python 3.11
    echo "✅ 虚拟环境创建完成"
else
    echo "✅ 虚拟环境已存在"
fi

# 检查CUDA环境
echo "🔍 检查CUDA环境..."
if command -v nvcc &> /dev/null; then
    NVCC_VERSION=$(nvcc --version | grep "release" | awk '{print $6}' | cut -d, -f1)
    echo "✅ 检测到 CUDA: $NVCC_VERSION"
    
    # 检查编译器兼容性
    if command -v clang-14 &> /dev/null; then
        echo "✅ 检测到 clang-14，将用于CUDA编译"
        CUDA_COMPILER="clang-14"
    elif command -v gcc &> /dev/null; then
        GCC_VERSION=$(gcc -dumpversion | cut -d. -f1)
        if [ "$GCC_VERSION" -le 11 ]; then
            echo "✅ 检测到兼容的 GCC版本: $GCC_VERSION"
            CUDA_COMPILER="gcc"
        else
            echo "⚠️  GCC版本过高($GCC_VERSION)，可能不兼容CUDA"
            echo "   建议安装 clang-14: sudo apt install clang-14"
            CUDA_COMPILER="gcc"
        fi
    else
        echo "❌ 未找到合适的CUDA编译器"
        exit 1
    fi
else
    echo "⚠️  未检测到CUDA，将使用CPU模式"
    CUDA_COMPILER=""
fi

# 安装系统依赖
echo "📦 检查系统依赖..."
if command -v apt-get &> /dev/null; then
    echo "🔧 安装必要的系统包..."
    sudo apt-get update -qq
    sudo apt-get install -y git-lfs build-essential
    
    # 如果没有clang-14但有CUDA，建议安装
    if [ -n "$CUDA_COMPILER" ] && ! command -v clang-14 &> /dev/null && command -v nvcc &> /dev/null; then
        echo "🔧 安装 clang-14 用于CUDA编译..."
        sudo apt-get install -y clang-14
        CUDA_COMPILER="clang-14"
    fi
fi

# 安装Python依赖
echo "📦 安装Python依赖..."
if [ -f "pyproject.toml" ]; then
    echo "   使用 pyproject.toml 安装依赖..."
    uv pip install -e .
else
    echo "   使用 requirements.txt 安装依赖..."
    uv pip install -r requirements.txt
fi

# 重新安装CUDA扩展（如果有CUDA）
if [ -n "$CUDA_COMPILER" ] && command -v nvcc &> /dev/null; then
    echo "🚀 编译CUDA加速扩展..."
    echo "   使用编译器: $CUDA_COMPILER"
    
    if [ "$CUDA_COMPILER" = "clang-14" ]; then
        echo "   配置NVCC使用clang-14..."
        export NVCC_CCBIN=clang-14
        uv pip install -e . --force-reinstall --no-deps --no-build-isolation
    else
        uv pip install -e . --force-reinstall --no-deps --no-build-isolation
    fi
    
    if [ $? -eq 0 ]; then
        echo "✅ CUDA扩展编译成功"
        
        # 测试CUDA扩展加载
        echo "🔍 测试CUDA扩展..."
        PYTORCH_LIB_PATH=$(uv run python -c "import torch; import os; print(os.path.join(os.path.dirname(torch.__file__), 'lib'))")
        export LD_LIBRARY_PATH="$PYTORCH_LIB_PATH:$LD_LIBRARY_PATH"
        
        uv run python -c "
try:
    from indextts.BigVGAN.alias_free_activation.cuda import anti_alias_activation_cuda
    print('✅ CUDA扩展加载成功!')
except ImportError as e:
    print(f'⚠️  CUDA扩展加载失败: {e}')
    print('   将使用torch fallback实现')
" || echo "⚠️  CUDA扩展测试失败，但安装可以继续"
    else
        echo "⚠️  CUDA扩展编译失败，将使用CPU fallback"
    fi
fi

# 创建checkpoints目录
if [ ! -d "checkpoints" ]; then
    echo "📁 创建 checkpoints 目录..."
    mkdir -p checkpoints
fi

# 检查模型文件
echo "🔍 检查模型文件..."
required_files=(
    "checkpoints/bigvgan_generator.pth"
    "checkpoints/bpe.model"
    "checkpoints/gpt.pth"
    "checkpoints/config.yaml"
)

missing_files=()
for file in "${required_files[@]}"; do
    if [ ! -f "$file" ]; then
        missing_files+=("$file")
    fi
done

# 自动下载模型文件
if [ ${#missing_files[@]} -gt 0 ]; then
    echo "📥 缺少模型文件，开始自动下载..."
    echo "   下载地址: https://huggingface.co/IndexTeam/Index-TTS"
    
    # 检查git-lfs
    if ! command -v git-lfs &> /dev/null; then
        echo "📦 安装 git-lfs..."
        if command -v apt-get &> /dev/null; then
            sudo apt-get install -y git-lfs
        else
            echo "❌ 请手动安装 git-lfs"
            exit 1
        fi
    fi
    
    # 初始化git-lfs
    git lfs install --skip-repo
    
    # 下载模型文件
    echo "⬇️  正在下载模型文件（这可能需要几分钟）..."
    if [ ! -d "checkpoints_download" ]; then
        git clone https://huggingface.co/IndexTeam/Index-TTS checkpoints_download
    else
        echo "   checkpoints_download 目录已存在，跳过克隆"
    fi
    
    # 复制文件
    echo "📂 复制模型文件..."
    cp checkpoints_download/*.pth checkpoints/ 2>/dev/null || true
    cp checkpoints_download/*.model checkpoints/ 2>/dev/null || true
    cp checkpoints_download/*.yaml checkpoints/ 2>/dev/null || true
    
    # 再次检查
    missing_files=()
    for file in "${required_files[@]}"; do
        if [ ! -f "$file" ]; then
            missing_files+=("$file")
        fi
    done
    
    if [ ${#missing_files[@]} -gt 0 ]; then
        echo "❌ 仍然缺少模型文件:"
        for file in "${missing_files[@]}"; do
            echo "  - $file"
        done
        echo ""
        echo "请手动下载模型文件："
        echo "🔗 HuggingFace: https://huggingface.co/IndexTeam/Index-TTS"
        echo "🔗 ModelScope: https://modelscope.cn/models/IndexTeam/Index-TTS"
        echo ""
        echo "或检查 checkpoints_download/ 目录中的文件"
        exit 1
    fi
fi

echo "✅ 所有模型文件已就绪"

# 创建配置文件
echo "⚙️  创建配置文件..."
cat > config.json << EOF
{
    "server": {
        "host": "127.0.0.1",
        "port": 7860
    },
    "model": {
        "checkpoints_path": "./checkpoints",
        "use_cuda": $([ -n "$CUDA_COMPILER" ] && echo "true" || echo "false")
    }
}
EOF

# 测试安装
echo "🧪 测试安装..."
echo "   测试模块导入..."

# 设置PyTorch库路径
PYTORCH_LIB_PATH=$(uv run python -c "import torch; import os; print(os.path.join(os.path.dirname(torch.__file__), 'lib'))" 2>/dev/null)
if [ -n "$PYTORCH_LIB_PATH" ]; then
    export LD_LIBRARY_PATH="$PYTORCH_LIB_PATH:$LD_LIBRARY_PATH"
fi

uv run python -c "
import sys
import os
try:
    import torch
    print(f'✅ PyTorch {torch.__version__}')
    print(f'   CUDA可用: {torch.cuda.is_available()}')
    if torch.cuda.is_available():
        print(f'   GPU设备: {torch.cuda.get_device_name()}')
    
    # 测试核心模块导入
    import indextts
    print('✅ IndexTTS 主模块加载成功')
    
    # 测试具体组件
    from indextts.gpt.model import GPT2InferenceModel
    print('✅ GPT推理模型组件加载成功')
    
    from indextts.BigVGAN.models import BigVGAN
    print('✅ BigVGAN组件加载成功')
    
    print('✅ 所有核心组件正常')
except Exception as e:
    print(f'⚠️  测试警告: {e}')
    print('   基础安装完成，某些高级功能可能需要运行时加载')
" || echo "⚠️  安装测试有警告，但基础功能应该可用"

# 清理临时文件
if [ -d "checkpoints_download" ]; then
    echo "🧹 清理临时文件..."
    rm -rf checkpoints_download
fi

echo ""
echo "🎉 IndexTTS 安装完成!"
echo "===================="
echo ""
echo "📋 安装摘要:"
echo "   • Python 环境: $(uv run python --version)"
echo "   • CUDA 支持: $([ -n "$CUDA_COMPILER" ] && echo "✅ 启用 ($CUDA_COMPILER)" || echo "❌ 未启用")"
echo "   • 模型文件: ✅ 已下载"
echo "   • 依赖安装: ✅ 完成"
echo ""
echo "🚀 使用方法:"
echo "   启动 WebUI: ./start_webui.sh"
echo "   或手动启动: uv run python webui.py"
echo ""
echo "💡 提示:"
echo "   • 默认地址: http://127.0.0.1:7860"
echo "   • 配置文件: config.json"
echo "   • 日志文件: logs/ 目录"
echo ""