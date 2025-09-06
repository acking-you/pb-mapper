#!/bin/bash

# VibeVoice TTS 服务测试脚本

TTS_HOST="127.0.0.1"
TTS_PORT="8000"
BASE_URL="http://${TTS_HOST}:${TTS_PORT}"

echo "🎙️ VibeVoice TTS 服务测试"
echo "========================"
echo "🌐 服务地址: $BASE_URL"
echo ""

# 检查服务健康状态
echo "1️⃣ 检查服务健康状态..."
health_response=$(curl -s -X GET "$BASE_URL/health")
echo "响应: $health_response"

# 解析健康状态
if echo "$health_response" | grep -q '"status":"healthy"'; then
    echo "✅ 服务状态正常"
else
    echo "❌ 服务状态异常"
    exit 1
fi

echo ""

# 测试TTS合成
echo "2️⃣ 测试文本转语音..."
tts_request='{
    "text": "Speaker 1: Hello, this is a test of VibeVoice integration with pb-mapper.\nSpeaker 2: The voice quality sounds quite natural and realistic!",
    "speakers": ["Alice", "Bob"],
    "cfg_scale": 1.3
}'

echo "请求内容:"
echo "$tts_request" | jq . 2>/dev/null || echo "$tts_request"
echo ""

echo "🔄 发送TTS请求..."
tts_response=$(curl -s -X POST "$BASE_URL/tts" \
  -H "Content-Type: application/json" \
  -d "$tts_request")

echo "响应: $tts_response"

# 解析TTS响应
if echo "$tts_response" | grep -q '"success":true'; then
    echo "✅ TTS合成成功"
    
    # 提取音频文件路径
    audio_file=$(echo "$tts_response" | grep -o '"/tmp/[^"]*\.wav"' | tr -d '"' || echo "")
    duration=$(echo "$tts_response" | grep -o '"duration":[0-9.]*' | cut -d: -f2)
    
    if [ ! -z "$audio_file" ]; then
        echo "🎵 音频文件: $audio_file"
        echo "⏱️ 音频时长: ${duration}s"
        
        # 检查文件是否存在
        if [ -f "$audio_file" ]; then
            echo "✅ 音频文件生成成功"
            echo "📊 文件大小: $(du -h "$audio_file" | cut -f1)"
            echo ""
            echo "🎧 你可以播放这个音频文件:"
            echo "   play $audio_file"
            echo "   或在浏览器中访问: $BASE_URL/audio/$(basename "$audio_file")"
        else
            echo "❌ 音频文件不存在"
        fi
    fi
else
    echo "❌ TTS合成失败"
    # 尝试解析错误信息
    error_detail=$(echo "$tts_response" | grep -o '"detail":"[^"]*"' | cut -d'"' -f4)
    if [ ! -z "$error_detail" ]; then
        echo "错误详情: $error_detail"
    fi
fi

echo ""
echo "3️⃣ API 文档地址: $BASE_URL/docs"
echo "🔗 你可以在浏览器中打开上述链接查看完整的API文档"