# VibeVoice 集成 pb-mapper 技术文档

## 架构概览

```
Flutter UI ← Rinf Signals → Rust Actor ← PyO3 → Python VibeVoice
```

## 核心组件

### 1. 依赖配置

```toml
# Cargo.toml
pyo3 = { version = "0.20", features = ["auto-initialize"] }
hound = "3.5"      # WAV文件处理
tokio = { version = "1.0", features = ["rt", "rt-multi-thread"] }
```

### 2. Python调用封装

```rust
pub struct VibeVoiceEngine {
    py_module: Option<Py<PyAny>>,
    model_loaded: bool,
}

impl VibeVoiceEngine {
    // 初始化Python环境和模块路径
    pub fn new() -> PyResult<Self>
    
    // 异步加载模型（在线程池中执行）
    pub async fn load_model(&mut self, model: &str) -> Result<(), Box<dyn Error>>
    
    // 异步合成语音（返回Vec<f32>音频数据）
    pub async fn synthesize(&self, text: &str, speakers: &[String]) -> Result<Vec<f32>, Box<dyn Error>>
}
```

### 3. 音频处理

```rust
pub struct AudioProcessor;

impl AudioProcessor {
    // 保存WAV文件
    pub fn save_wav(audio: &[f32], sample_rate: u32, path: &Path) -> Result<(), Box<dyn Error>>
    
    // 编码为内存WAV
    pub fn encode_wav_to_bytes(audio: &[f32], sample_rate: u32) -> Result<Vec<u8>, Box<dyn Error>>
}
```

### 4. Actor集成

```rust
pub struct PbMapperActor {
    tts_engine: Option<VibeVoiceEngine>,
}

// 处理TTS请求信号
impl Notifiable<StartTTSServiceRequest> for PbMapperActor
impl Notifiable<SynthesizeSpeechRequest> for PbMapperActor
```

### 5. UI信号定义

```rust
// Dart → Rust
#[derive(Deserialize, DartSignal)]
pub struct StartTTSServiceRequest {
    pub model_name: String,
}

#[derive(Deserialize, DartSignal)] 
pub struct SynthesizeSpeechRequest {
    pub text: String,
    pub speakers: Vec<String>,
}

// Rust → Dart
#[derive(Serialize, RustSignal)]
pub struct SpeechSynthesisResult {
    pub success: bool,
    pub message: String,
    pub file_path: Option<String>,
}
```

## 流式音频输出实现

### 1. Python流式生成器

```python
def synthesize_speech_streaming(text: str, voice_samples: list, cfg_scale: float = 1.3):
    """流式生成音频数据，边生成边返回"""
    # 模型推理（现有代码）
    inputs = processor(
        text=[text],
        voice_samples=[voice_samples],
        padding=True,
        return_tensors="pt",
        return_attention_mask=True,
    )
    
    # 这里需要修改VibeVoice生成过程以支持流式输出
    # 当前VibeVoice不直接支持流式，但可以通过分块处理实现
    with torch.no_grad():
        outputs = model.generate(
            **inputs,
            max_new_tokens=None,
            cfg_scale=cfg_scale,
            tokenizer=processor.tokenizer,
            generation_config={'do_sample': False},
            verbose=False,
        )
    
    # 获取音频数据
    audio_tensor = outputs.speech_outputs[0]
    if audio_tensor.dtype == torch.bfloat16:
        audio_tensor = audio_tensor.float()
    audio_data = audio_tensor.cpu().numpy().astype(np.float32)
    
    # 分块返回音频数据（模拟流式输出）
    chunk_size = 24000  # 1秒的音频数据
    for i in range(0, len(audio_data), chunk_size):
        chunk = audio_data[i:i + chunk_size]
        yield chunk.tolist()  # 转为Python list便于PyO3传输
```

### 2. Rust流式接收和播放

```rust
use pyo3::prelude::*;
use pyo3::types::PyIterator;
use cpal::{Device, Stream, StreamConfig, SampleRate};
use std::sync::mpsc;
use tokio::task;

pub struct StreamingTTSEngine {
    py_module: Option<Py<PyAny>>,
    audio_device: Option<Device>,
    audio_stream: Option<Stream>,
}

impl StreamingTTSEngine {
    pub fn new() -> PyResult<Self> {
        Ok(Self {
            py_module: None,
            audio_device: Self::init_audio_device()?,
            audio_stream: None,
        })
    }

    fn init_audio_device() -> Result<Option<Device>, Box<dyn std::error::Error>> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let device = host.default_output_device()
            .ok_or("No audio output device available")?;
        Ok(Some(device))
    }

    pub async fn synthesize_streaming(
        &mut self,
        text: &str,
        voice_samples: Vec<String>,
        output_path: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let mut complete_audio = Vec::<f32>::new();
        
        // 启动音频播放线程
        let audio_handle = self.start_audio_playback(rx)?;
        
        // 在后台线程调用Python流式生成
        let py_module = self.py_module.as_ref().unwrap().clone();
        let text = text.to_string();
        let tx_clone = tx.clone();
        
        task::spawn_blocking(move || {
            Python::with_gil(|py| -> PyResult<()> {
                let generator = py_module.call_method1(
                    py,
                    "synthesize_speech_streaming",
                    (text, voice_samples, 1.3f32),
                )?;
                
                let iterator = generator.as_ref(py).iter()?;
                for chunk_result in iterator {
                    let chunk_py = chunk_result?;
                    let chunk: Vec<f32> = chunk_py.extract()?;
                    
                    // 发送音频块到播放线程
                    if tx_clone.send(chunk.clone()).is_err() {
                        break; // 接收端已关闭
                    }
                    
                    // 累积完整音频用于保存
                    complete_audio.extend(chunk);
                }
                Ok(())
            })
        }).await??;
        
        // 关闭发送端，等待播放完成
        drop(tx);
        audio_handle.join().map_err(|_| "Audio playback thread panicked")?;
        
        // 保存完整音频文件
        self.save_audio(&complete_audio, 24000, output_path)?;
        
        Ok(())
    }

    fn start_audio_playback(
        &self,
        rx: mpsc::Receiver<Vec<f32>>,
    ) -> Result<std::thread::JoinHandle<()>, Box<dyn std::error::Error>> {
        let device = self.audio_device.as_ref().unwrap().clone();
        
        let handle = std::thread::spawn(move || {
            use cpal::traits::{DeviceTrait, StreamTrait};
            use std::collections::VecDeque;
            use std::sync::{Arc, Mutex};
            
            let config = StreamConfig {
                channels: 1,
                sample_rate: SampleRate(24000),
                buffer_size: cpal::BufferSize::Default,
            };
            
            let audio_buffer = Arc::new(Mutex::new(VecDeque::<f32>::new()));
            let audio_buffer_clone = audio_buffer.clone();
            
            // 创建音频流
            let stream = device.build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut buffer = audio_buffer_clone.lock().unwrap();
                    for sample in data.iter_mut() {
                        *sample = buffer.pop_front().unwrap_or(0.0);
                    }
                },
                |err| eprintln!("Audio stream error: {}", err),
            ).expect("Failed to create audio stream");
            
            stream.play().expect("Failed to play audio stream");
            
            // 接收音频块并填充缓冲区
            while let Ok(chunk) = rx.recv() {
                let mut buffer = audio_buffer.lock().unwrap();
                for sample in chunk {
                    buffer.push_back(sample);
                }
            }
            
            // 等待缓冲区播放完毕
            loop {
                {
                    let buffer = audio_buffer.lock().unwrap();
                    if buffer.is_empty() {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
        
        Ok(handle)
    }

    fn save_audio(&self, audio: &[f32], sample_rate: u32, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        use hound::{WavSpec, WavWriter};
        
        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        
        let mut writer = WavWriter::create(path, spec)?;
        for &sample in audio {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;
        
        Ok(())
    }
}
```

### 3. Actor集成流式TTS

```rust
// 新增流式TTS请求信号
#[derive(Deserialize, DartSignal)]
pub struct StartStreamingTTSRequest {
    pub text: String,
    pub voice_samples: Vec<String>,
    pub output_path: String,
}

#[derive(Serialize, RustSignal)]
pub struct StreamingTTSProgress {
    pub progress: f32,        // 进度百分比
    pub is_playing: bool,     // 是否正在播放
    pub is_complete: bool,    // 是否完成
    pub output_file: Option<String>, // 输出文件路径
}

impl PbMapperActor {
    async fn listen_to_start_streaming_tts(mut self_addr: Address<Self>) {
        let receiver = StartStreamingTTSRequest::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }
}

#[async_trait]
impl Notifiable<StartStreamingTTSRequest> for PbMapperActor {
    async fn notify(&mut self, msg: StartStreamingTTSRequest, _: &Context<Self>) {
        // 发送开始信号
        StreamingTTSProgress {
            progress: 0.0,
            is_playing: false,
            is_complete: false,
            output_file: None,
        }.send_signal_to_dart();
        
        // 执行流式合成
        match self.streaming_tts_engine.synthesize_streaming(
            &msg.text,
            msg.voice_samples,
            &msg.output_path,
        ).await {
            Ok(_) => {
                StreamingTTSProgress {
                    progress: 100.0,
                    is_playing: false,
                    is_complete: true,
                    output_file: Some(msg.output_path),
                }.send_signal_to_dart();
            }
            Err(e) => {
                // 发送错误信号
                tracing::error!("Streaming TTS failed: {}", e);
            }
        }
    }
}
```

### 4. 依赖配置更新

```toml
# Cargo.toml 新增音频依赖
cpal = "0.15"           # 音频播放
hound = "3.5"           # WAV文件处理
crossbeam = "0.8"       # 线程间通信
```

## 实现要点

1. **Python环境**: 应用启动时设置VibeVoice虚拟环境路径到Python sys.path
2. **异步执行**: 所有Python调用在tokio线程池中执行，避免阻塞UI
3. **音频格式**: Python返回的音频数据转换为Vec<f32>，然后用Rust库保存为WAV
4. **错误处理**: 完整的错误传播链从Python到Rust到Flutter UI
5. **流式播放**: 使用cpal库实现实时音频播放，同时累积完整音频用于保存
6. **内存管理**: 合理控制音频缓冲区大小，避免内存溢出
7. **线程协调**: 音频生成、播放、保存在不同线程中执行，通过channel通信

## 部署

- 打包 `local-tools/VibeVoice/.venv` 目录
- 确保Python 3.12运行时可用
- 确保系统有可用的音频输出设备