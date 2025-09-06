#!/bin/bash

# VibeVoice 直接调用使用示例

echo "🎙️ VibeVoice Direct Usage Examples"
echo "=================================="

# 检查语音样本是否存在
VOICES_DIR="demo/voices"
if [ ! -d "$VOICES_DIR" ]; then
    echo "❌ 语音样本目录不存在: $VOICES_DIR"
    exit 1
fi

echo "📂 可用的语音样本:"
ls -1 "$VOICES_DIR"/*.wav | while read file; do
    echo "  $(basename "$file")"
done
echo

# 示例1: 单个说话者
echo "🎭 示例1: 单个说话者"
echo "命令:"
CMD1="uv run python direct_usage_example.py --text 'Speaker 1: Hello, this is a test of VibeVoice single speaker synthesis.' --voice_samples demo/voices/en-Alice_woman.wav --speakers Alice --output output_single.wav"
echo "$CMD1"
echo

# 示例2: 两个说话者对话
echo "🎭 示例2: 两个说话者对话"
echo "命令:"
CMD2="uv run python direct_usage_example.py --text 'Speaker 1: Welcome to our AI podcast demonstration! Speaker 2: Thanks for having me. This is exciting! Speaker 1: Lets explore how VibeVoice can generate natural speech. Speaker 2: The technology is truly remarkable!' --voice_samples demo/voices/en-Alice_woman.wav demo/voices/en-Carter_man.wav --speakers Alice Carter --output output_dialogue.wav --cfg_scale 1.3"
echo "$CMD2"
echo

# 示例3: 多个说话者（中英混合）
echo "🎭 示例3: 多个说话者（中英混合）"
echo "命令:"
CMD3="uv run python direct_usage_example.py --text 'Speaker 1: Hello everyone, welcome to our multilingual demo. Speaker 2: 大家好，我是第二个说话者。 Speaker 3: Hi there, I am the third speaker in this conversation. Speaker 4: 这是一个很有趣的技术演示！' --voice_samples demo/voices/en-Alice_woman.wav demo/voices/zh-Xinran_woman.wav demo/voices/en-Frank_man.wav demo/voices/zh-Bowen_man.wav --speakers Alice Xinran Frank Bowen --output output_multilingual.wav"
echo "$CMD3"
echo

echo "💡 使用说明:"
echo "• --text: 要合成的文本，可以使用 'Speaker N:' 格式指定说话者"
echo "• --voice_samples: 语音样本文件路径，每个说话者一个文件"
echo "• --speakers: 说话者名称，必须与语音样本数量匹配"
echo "• --output: 输出音频文件路径"
echo "• --cfg_scale: 生成控制比例 (默认1.3)"
echo "• --model_path: 模型路径 (默认microsoft/VibeVoice-1.5B)"
echo "• --device: 设备选择 (默认自动选择cuda或cpu)"
echo

read -p "选择要运行的示例 (1-3) 或按Enter查看帮助: " choice

case $choice in
    1)
        echo "🚀 运行示例1..."
        eval $CMD1
        ;;
    2)
        echo "🚀 运行示例2..."
        eval $CMD2
        ;;
    3)
        echo "🚀 运行示例3..."
        eval $CMD3
        ;;
    *)
        echo "📖 查看完整帮助信息:"
        uv run python direct_usage_example.py --help
        ;;
esac