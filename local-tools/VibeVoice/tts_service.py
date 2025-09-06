"""
VibeVoice 轻量级 Web 服务
基于 FastAPI，去除 Gradio 依赖
"""

import argparse
import json
import tempfile
import time
from pathlib import Path
from typing import List, Dict, Any
import numpy as np
import soundfile as sf
import torch
from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse, JSONResponse
from pydantic import BaseModel
import uvicorn

from vibevoice.modular.modeling_vibevoice_inference import VibeVoiceForConditionalGenerationInference
from vibevoice.processor.vibevoice_processor import VibeVoiceProcessor
from transformers import set_seed


class TTSRequest(BaseModel):
    text: str
    speakers: List[str] = ["Alice", "Bob"]
    cfg_scale: float = 1.3


class TTSResponse(BaseModel):
    success: bool
    message: str
    audio_file: str = None
    duration: float = None


class VibeVoiceService:
    def __init__(self, model_path: str, device: str = "cuda"):
        self.model_path = model_path
        self.device = device
        self.model = None
        self.processor = None
        self.model_loaded = False
        
    def load_model(self):
        """加载模型和处理器"""
        print(f"🔄 Loading model from {self.model_path}")
        
        # 设备配置
        if self.device == "cuda" and torch.cuda.is_available():
            load_dtype = torch.bfloat16
            attn_impl = "sdpa"  # 使用 sdpa 避免 FlashAttention2 依赖
        else:
            self.device = "cpu"
            load_dtype = torch.float32
            attn_impl = "sdpa"
            
        print(f"📱 Device: {self.device}, dtype: {load_dtype}")
        
        # 加载处理器
        self.processor = VibeVoiceProcessor.from_pretrained(self.model_path)
        
        # 加载模型
        self.model = VibeVoiceForConditionalGenerationInference.from_pretrained(
            self.model_path,
            torch_dtype=load_dtype,
            device_map=self.device if self.device != "cpu" else None,
            attn_implementation=attn_impl,
        )
        
        if self.device == "cpu":
            self.model = self.model.to("cpu")
            
        self.model.eval()
        
        # 配置推理步数
        self.model.set_ddpm_inference_steps(num_steps=5)
        
        self.model_loaded = True
        print("✅ Model loaded successfully")
        
    def synthesize_speech(self, text: str, speakers: List[str], cfg_scale: float = 1.3) -> str:
        """合成语音并返回临时文件路径"""
        if not self.model_loaded:
            raise HTTPException(status_code=400, detail="Model not loaded")
            
        start_time = time.time()
        
        # 格式化脚本
        lines = text.strip().split('\n')
        formatted_lines = []
        
        for i, line in enumerate(lines):
            if line.strip():
                speaker_idx = i % len(speakers)
                if not line.startswith('Speaker'):
                    formatted_lines.append(f"Speaker {speaker_idx}: {line.strip()}")
                else:
                    formatted_lines.append(line.strip())
                    
        formatted_script = '\n'.join(formatted_lines)
        
        # 准备语音样本（这里简化处理，实际应该加载真实的语音文件）
        # 为演示目的创建虚拟语音样本
        voice_samples = []
        for _ in speakers:
            # 创建简单的噪声作为语音样本占位符
            dummy_sample = np.random.randn(24000).astype(np.float32) * 0.1
            voice_samples.append(dummy_sample)
        
        # 处理输入
        inputs = self.processor(
            text=[formatted_script],
            voice_samples=[voice_samples],
            padding=True,
            return_tensors="pt",
            return_attention_mask=True,
        )
        
        # 移动到设备
        if self.device != "cpu":
            for k, v in inputs.items():
                if torch.is_tensor(v):
                    inputs[k] = v.to(self.device)
        
        # 生成音频
        with torch.no_grad():
            outputs = self.model.generate(
                **inputs,
                max_new_tokens=None,
                cfg_scale=cfg_scale,
                tokenizer=self.processor.tokenizer,
                generation_config={'do_sample': False},
                verbose=False,
            )
        
        # 转换为音频数据
        # VibeVoice 输出通常是 VibeVoiceGenerationOutput 对象
        if hasattr(outputs, 'speech_outputs'):
            # 关键！音频数据在 speech_outputs 中，不是 sequences！
            audio_tensor = outputs.speech_outputs[0]  # 取第一个batch
        elif hasattr(outputs, 'audio'):
            # 如果输出有 audio 属性
            audio_tensor = outputs.audio
        elif torch.is_tensor(outputs):
            # 如果直接是tensor
            audio_tensor = outputs
        else:
            # 尝试获取第一个元素
            if hasattr(outputs, '__getitem__'):
                audio_tensor = outputs[0]
            else:
                raise ValueError(f"Unsupported output type: {type(outputs)}")
        
        # 转换tensor为numpy
        if torch.is_tensor(audio_tensor):
            if audio_tensor.dtype == torch.bfloat16:
                audio_tensor = audio_tensor.float()
            audio_data = audio_tensor.cpu().numpy().astype(np.float32)
        else:
            audio_data = np.array(audio_tensor, dtype=np.float32)
            
        # 确保是1D数组
        if len(audio_data.shape) > 1:
            audio_data = audio_data.squeeze()
            
        # 保存为临时WAV文件
        temp_file = tempfile.NamedTemporaryFile(delete=False, suffix='.wav')
        sf.write(temp_file.name, audio_data, 24000)
        
        generation_time = time.time() - start_time
        duration = len(audio_data) / 24000
        
        print(f"🎵 Generated {duration:.2f}s audio in {generation_time:.2f}s")
        
        return temp_file.name, duration


# 全局服务实例
tts_service = None


def create_app() -> FastAPI:
    app = FastAPI(
        title="VibeVoice TTS Service",
        description="轻量级 VibeVoice 文本转语音服务",
        version="1.0.0"
    )
    
    @app.get("/")
    async def root():
        return {"message": "VibeVoice TTS Service", "status": "running"}
    
    @app.post("/tts", response_model=TTSResponse)
    async def text_to_speech(request: TTSRequest):
        """文本转语音接口"""
        global tts_service
        
        if tts_service is None:
            raise HTTPException(status_code=503, detail="TTS service not initialized")
            
        try:
            audio_file, duration = tts_service.synthesize_speech(
                text=request.text,
                speakers=request.speakers,
                cfg_scale=request.cfg_scale
            )
            
            return TTSResponse(
                success=True,
                message="Speech synthesis completed",
                audio_file=audio_file,
                duration=duration
            )
            
        except Exception as e:
            raise HTTPException(status_code=500, detail=f"Synthesis failed: {str(e)}")
    
    @app.get("/audio/{filename}")
    async def get_audio(filename: str):
        """获取生成的音频文件"""
        file_path = Path(filename)
        if file_path.exists():
            return FileResponse(file_path, media_type="audio/wav")
        else:
            raise HTTPException(status_code=404, detail="Audio file not found")
    
    @app.get("/health")
    async def health_check():
        """健康检查"""
        global tts_service
        return {
            "status": "healthy" if tts_service and tts_service.model_loaded else "not ready",
            "model_loaded": tts_service.model_loaded if tts_service else False
        }
    
    return app


def main():
    global tts_service
    
    parser = argparse.ArgumentParser(description="VibeVoice TTS Web Service")
    parser.add_argument(
        "--model_path",
        type=str,
        default="microsoft/VibeVoice-1.5B",
        help="Path to VibeVoice model"
    )
    parser.add_argument(
        "--device",
        type=str,
        default="cuda" if torch.cuda.is_available() else "cpu",
        help="Device for inference"
    )
    parser.add_argument(
        "--host",
        type=str,
        default="127.0.0.1",
        help="Host to bind to"
    )
    parser.add_argument(
        "--port",
        type=int,
        default=8000,
        help="Port to listen on"
    )
    
    args = parser.parse_args()
    
    set_seed(42)
    
    print("🎙️ VibeVoice TTS Web Service")
    print("============================")
    
    # 初始化服务
    tts_service = VibeVoiceService(args.model_path, args.device)
    tts_service.load_model()
    
    # 创建应用
    app = create_app()
    
    print(f"🚀 Starting server on {args.host}:{args.port}")
    print(f"📖 API文档: http://{args.host}:{args.port}/docs")
    print(f"🧪 测试接口: http://{args.host}:{args.port}/health")
    
    # 启动服务器
    uvicorn.run(
        app,
        host=args.host,
        port=args.port,
        log_level="info"
    )


if __name__ == "__main__":
    main()