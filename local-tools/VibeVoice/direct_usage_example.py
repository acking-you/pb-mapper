#!/usr/bin/env python3
"""
VibeVoice 直接调用示例
展示如何直接使用 Python 模块进行文本转语音
"""

import argparse
import torch
import numpy as np
import soundfile as sf
import librosa
from pathlib import Path

from vibevoice.modular.modeling_vibevoice_inference import VibeVoiceForConditionalGenerationInference
from vibevoice.processor.vibevoice_processor import VibeVoiceProcessor
from transformers import set_seed


def load_voice_sample(voice_path: str, target_sr: int = 24000) -> np.ndarray:
    """加载语音样本文件"""
    try:
        audio, sr = sf.read(voice_path)
        if len(audio.shape) > 1:
            audio = np.mean(audio, axis=1)  # 转为单声道
        if sr != target_sr:
            audio = librosa.resample(audio, orig_sr=sr, target_sr=target_sr)
        return audio.astype(np.float32)
    except Exception as e:
        raise ValueError(f"Failed to load voice sample from {voice_path}: {e}")


def main():
    # 解析命令行参数
    parser = argparse.ArgumentParser(description="VibeVoice Direct Usage Example")
    parser.add_argument("--text", type=str, required=True, help="Text to synthesize")
    parser.add_argument("--voice_samples", type=str, nargs="+", required=True, 
                       help="Path to voice sample files (one for each speaker)")
    parser.add_argument("--speakers", type=str, nargs="+", default=["Alice", "Bob"],
                       help="Speaker names (default: Alice Bob)")
    parser.add_argument("--model_path", type=str, default="microsoft/VibeVoice-1.5B",
                       help="Path to VibeVoice model")
    parser.add_argument("--device", type=str, 
                       default="cuda" if torch.cuda.is_available() else "cpu",
                       help="Device for inference")
    parser.add_argument("--output", type=str, default="output_example.wav",
                       help="Output audio file path")
    parser.add_argument("--cfg_scale", type=float, default=1.3,
                       help="CFG scale for generation")
    
    args = parser.parse_args()
    
    # 验证参数
    if len(args.voice_samples) != len(args.speakers):
        raise ValueError(f"Number of voice samples ({len(args.voice_samples)}) must match number of speakers ({len(args.speakers)})")
    
    # 设置随机种子
    set_seed(42)
    
    print(f"🎙️ VibeVoice Direct Usage Example")
    print(f"📱 Device: {args.device}")
    print(f"📦 Model: {args.model_path}")
    print(f"🎭 Speakers: {', '.join(args.speakers)}")
    print(f"🎵 Voice samples: {', '.join(args.voice_samples)}")
    print()
    
    # 加载模型和处理器
    print("🔄 Loading model...")
    processor = VibeVoiceProcessor.from_pretrained(args.model_path)
    
    model = VibeVoiceForConditionalGenerationInference.from_pretrained(
        args.model_path,
        torch_dtype=torch.bfloat16 if args.device == "cuda" else torch.float32,
        device_map=args.device if args.device != "cpu" else None,
        attn_implementation="sdpa",  # 避免 FlashAttention2 依赖
    )
    
    if args.device == "cpu":
        model = model.to("cpu")
    
    model.eval()
    model.set_ddpm_inference_steps(num_steps=5)
    
    print("✅ Model loaded successfully")
    print()
    
    # 准备文本
    print(f"📝 Text to synthesize:")
    print(args.text)
    print()
    
    # 加载语音样本
    print("🎵 Loading voice samples...")
    voice_samples = []
    for i, voice_path in enumerate(args.voice_samples):
        print(f"  Loading {args.speakers[i]}: {voice_path}")
        try:
            sample = load_voice_sample(voice_path)
            voice_samples.append(sample)
            print(f"    ✅ Loaded: {sample.shape} samples")
        except Exception as e:
            print(f"    ❌ Failed: {e}")
            return 1
    
    # 处理输入
    print("🔧 Processing inputs...")
    inputs = processor(
        text=[args.text],
        voice_samples=[voice_samples],
        padding=True,
        return_tensors="pt",
        return_attention_mask=True,
    )
    
    # 移动到设备
    if args.device != "cpu":
        for k, v in inputs.items():
            if torch.is_tensor(v):
                inputs[k] = v.to(args.device)
    
    # 生成音频
    print("🎙️ Generating speech...")
    import time
    start_time = time.time()
    
    with torch.no_grad():
        outputs = model.generate(
            **inputs,
            max_new_tokens=None,
            cfg_scale=args.cfg_scale,
            tokenizer=processor.tokenizer,
            generation_config={'do_sample': False},
            verbose=False,
        )
    
    generation_time = time.time() - start_time
    
    # 处理输出
    # VibeVoice 输出通常是 VibeVoiceGenerationOutput 对象
    print(f"🔍 Output type: {type(outputs)}")
    print(f"🔍 Available attributes: {[attr for attr in dir(outputs) if not attr.startswith('_')]}")
    
    if hasattr(outputs, 'speech_outputs'):
        # 关键！音频数据在 speech_outputs 中
        audio_tensor = outputs.speech_outputs[0]  # 取第一个batch
        print("📦 Using outputs.speech_outputs[0] (CORRECT)")
        print(f"📊 Audio tensor shape: {audio_tensor.shape}")
    elif hasattr(outputs, 'audio'):
        # 如果输出有 audio 属性
        audio_tensor = outputs.audio
        print("📦 Using outputs.audio")
    elif hasattr(outputs, 'sequences'):
        # 这个是文本tokens，不是音频！
        print("⚠️  Warning: sequences contains text tokens, not audio!")
        audio_tensor = outputs.sequences
        print("📦 Using outputs.sequences (may be wrong)")
    elif torch.is_tensor(outputs):
        # 如果直接是tensor
        audio_tensor = outputs
        print("📦 Using outputs directly (tensor)")
    else:
        # 尝试获取第一个元素
        if hasattr(outputs, '__getitem__'):
            audio_tensor = outputs[0]
            print("📦 Using outputs[0]")
        else:
            raise ValueError(f"Unsupported output type: {type(outputs)}")
    
    # 转换tensor为numpy
    if torch.is_tensor(audio_tensor):
        if audio_tensor.dtype == torch.bfloat16:
            audio_tensor = audio_tensor.float()
        audio_data = audio_tensor.cpu().numpy().astype(np.float32)
        print(f"📊 Final audio data shape: {audio_data.shape}")
    else:
        audio_data = np.array(audio_tensor, dtype=np.float32)
        print(f"📊 Audio data type: {type(audio_tensor)}")
    
    # 确保是1D数组
    if len(audio_data.shape) > 1:
        audio_data = audio_data.squeeze()
    
    # 保存音频文件
    output_path = Path(args.output)
    sf.write(output_path, audio_data, 24000)
    
    duration = len(audio_data) / 24000
    
    print(f"✅ Generation completed!")
    print(f"⏱️ Generation time: {generation_time:.2f} seconds")
    print(f"🎵 Audio duration: {duration:.2f} seconds")
    print(f"💾 Output saved to: {output_path.absolute()}")
    print()
    print("🎧 You can now play the generated audio file!")
    
    return 0


if __name__ == "__main__":
    exit(main())