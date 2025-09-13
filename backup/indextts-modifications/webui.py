import json
import os
import sys
import threading
import time
import shutil
from datetime import datetime
import re

import warnings
warnings.filterwarnings("ignore", category=FutureWarning)
warnings.filterwarnings("ignore", category=UserWarning)

current_dir = os.path.dirname(os.path.abspath(__file__))
sys.path.append(current_dir)
sys.path.append(os.path.join(current_dir, "indextts"))

import argparse
parser = argparse.ArgumentParser(description="IndexTTS WebUI")
parser.add_argument("--verbose", action="store_true", default=False, help="Enable verbose mode")
parser.add_argument("--port", type=int, default=7860, help="Port to run the web UI on")
parser.add_argument("--host", type=str, default="127.0.0.1", help="Host to run the web UI on")
parser.add_argument("--model_dir", type=str, default="checkpoints", help="Model checkpoints directory")
parser.add_argument("--input_dir", type=str, default="input", help="Directory to save uploaded reference audio files")
cmd_args = parser.parse_args()

if not os.path.exists(cmd_args.model_dir):
    print(f"Model directory {cmd_args.model_dir} does not exist. Please download the model first.")
    sys.exit(1)

# Create input directory for reference audio files
os.makedirs(cmd_args.input_dir, exist_ok=True)

for file in [
    "bigvgan_generator.pth",
    "bpe.model",
    "gpt.pth",
    "config.yaml",
]:
    file_path = os.path.join(cmd_args.model_dir, file)
    if not os.path.exists(file_path):
        print(f"Required file {file_path} does not exist. Please download it.")
        sys.exit(1)

import gradio as gr

from indextts.infer import IndexTTS
from tools.i18n.i18n import I18nAuto

i18n = I18nAuto(language="zh_CN")
MODE = 'local'
tts = IndexTTS(model_dir=cmd_args.model_dir, cfg_path=os.path.join(cmd_args.model_dir, "config.yaml"),)

# Log startup information
print("=" * 50)
print("🎙️ IndexTTS WebUI 启动成功")
print(f"📁 参考音频输入目录: {cmd_args.input_dir}")
print(f"💾 上传的音频文件将自动保存到: {cmd_args.input_dir}/")

# Check existing reference audio files
def get_reference_audio_files():
    """Get list of audio files in the input directory"""
    audio_extensions = ['.wav', '.mp3', '.m4a', '.flac', '.aac', '.ogg']
    audio_files = []
    if os.path.exists(cmd_args.input_dir):
        for file in os.listdir(cmd_args.input_dir):
            if any(file.lower().endswith(ext) for ext in audio_extensions):
                audio_files.append(os.path.join(cmd_args.input_dir, file))
    return sorted(audio_files)

def _sanitize_filename(name: str) -> str:
    """Sanitize filename to avoid dangerous characters and paths."""
    # Remove directory components and invalid characters
    name = os.path.basename(name.strip())
    # Replace invalid chars with underscore
    name = re.sub(r"[^\w\-. ]", "_", name)
    # Avoid empty name
    return name or "ref_audio"

def save_uploaded_audio(audio_file, custom_name: str | None = None):
    """Save uploaded audio file to input directory and return the saved path.

    If custom_name is provided, use it (respecting extension if present).
    Otherwise, use a timestamped default.
    """
    if audio_file is None:
        return None

    audio_extensions = ['.wav', '.mp3', '.m4a', '.flac', '.aac', '.ogg']

    try:
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        original_name = os.path.basename(audio_file)
        orig_base, orig_ext = os.path.splitext(original_name)

        dest_filename = None
        if custom_name and custom_name.strip():
            custom_name = _sanitize_filename(custom_name)
            base, ext = os.path.splitext(custom_name)
            if ext == "":
                ext = orig_ext if orig_ext.lower() in audio_extensions else ".wav"
            # Validate extension
            if ext.lower() not in audio_extensions:
                print(f"⚠️ 不支持的扩展名 {ext}，将使用原始扩展名或 .wav")
                ext = orig_ext if orig_ext.lower() in audio_extensions else ".wav"
            if base == "":
                base = f"ref_{timestamp}"
            dest_filename = f"{base}{ext}"
        else:
            # default naming: ref_{timestamp}_{orig_base}{orig_ext}
            base = f"ref_{timestamp}_{orig_base if orig_base else 'audio'}"
            ext = orig_ext if orig_ext.lower() in audio_extensions else ".wav"
            dest_filename = f"{base}{ext}"

        dest_path = os.path.join(cmd_args.input_dir, dest_filename)
        # Avoid overwrite: if exists, append timestamp suffix
        if os.path.exists(dest_path):
            base_no_ext, ext = os.path.splitext(dest_filename)
            dest_path = os.path.join(cmd_args.input_dir, f"{base_no_ext}_{timestamp}{ext}")

        shutil.copy2(audio_file, dest_path)
        print(f"📁 参考音频已保存: {dest_path}")
        return dest_path
    except Exception as e:
        print(f"❌ 保存音频文件失败: {e}")
        return audio_file

# Display existing reference audio files
existing_files = get_reference_audio_files()
if existing_files:
    print(f"📂 发现 {len(existing_files)} 个现有参考音频文件:")
    for i, file in enumerate(existing_files[:5], 1):  # Show first 5 files
        print(f"   {i}. {os.path.basename(file)}")
    if len(existing_files) > 5:
        print(f"   ... 还有 {len(existing_files) - 5} 个文件")
else:
    print("📂 当前无参考音频文件，上传音频后将自动保存")
print("=" * 50)


os.makedirs("outputs/tasks",exist_ok=True)
os.makedirs("prompts",exist_ok=True)

with open("tests/cases.jsonl", "r", encoding="utf-8") as f:
    example_cases = []
    for line in f:
        line = line.strip()
        if not line:
            continue
        example = json.loads(line)
        example_cases.append([os.path.join("tests", example.get("prompt_audio", "sample_prompt.wav")),
                              example.get("text"), ["普通推理", "批次推理"][example.get("infer_mode", 0)]])

def gen_single(prompt, selected_reference, text, infer_mode, max_text_tokens_per_sentence=120, sentences_bucket_max_size=4,
                *args, progress=gr.Progress()):
    # Determine which audio to use for generation
    final_prompt = get_final_prompt_audio(prompt, selected_reference)
    
    if final_prompt is None:
        raise gr.Error("请上传参考音频或选择已有的参考音频文件")
    
    output_path = None
    if not output_path:
        output_path = os.path.join("outputs", f"spk_{int(time.time())}.wav")
    # set gradio progress
    tts.gr_progress = progress
    do_sample, top_p, top_k, temperature, \
        length_penalty, num_beams, repetition_penalty, max_mel_tokens = args
    kwargs = {
        "do_sample": bool(do_sample),
        "top_p": float(top_p),
        "top_k": int(top_k) if int(top_k) > 0 else None,
        "temperature": float(temperature),
        "length_penalty": float(length_penalty),
        "num_beams": num_beams,
        "repetition_penalty": float(repetition_penalty),
        "max_mel_tokens": int(max_mel_tokens),
        # "typical_sampling": bool(typical_sampling),
        # "typical_mass": float(typical_mass),
    }
    if infer_mode == "普通推理":
        output = tts.infer(final_prompt, text, output_path, verbose=cmd_args.verbose,
                           max_text_tokens_per_sentence=int(max_text_tokens_per_sentence),
                           **kwargs)
    else:
        # 批次推理
        output = tts.infer_fast(final_prompt, text, output_path, verbose=cmd_args.verbose,
            max_text_tokens_per_sentence=int(max_text_tokens_per_sentence),
            sentences_bucket_max_size=(sentences_bucket_max_size),
            **kwargs)
    return gr.update(value=output,visible=True)

def update_prompt_audio(audio_file, custom_name):
    """Handle uploaded audio and save it to input directory, then refresh list.

    The filename is required but prefilled; if empty, we auto-generate one.
    """
    if audio_file is not None:
        # Ensure a filename exists; if not, prefill a sensible default
        if not custom_name or not str(custom_name).strip():
            custom_name = f"ref_{time.strftime('%Y%m%d_%H%M%S')}"

        saved_path = save_uploaded_audio(audio_file, custom_name)
        # refresh dropdown choices
        files = get_reference_audio_files()
        choices = [os.path.basename(f) for f in files] if files else []
        value = os.path.basename(saved_path) if saved_path and os.path.exists(saved_path) else (choices[0] if choices else None)
        # also update the textbox to reflect the final saved filename
        filename_value = os.path.basename(saved_path) if saved_path else custom_name
        # Important: cannot programmatically set file input value in browsers;
        # clear the upload field and guide users to the dropdown/reference.
        return None, gr.update(choices=choices, value=value), gr.update(value=filename_value), os.path.basename(saved_path)
    # no file uploaded; keep as is
    files = get_reference_audio_files()
    choices = [os.path.basename(f) for f in files] if files else []
    return audio_file, gr.update(choices=choices), gr.update(), ""

def refresh_reference_list():
    """Refresh the reference audio dropdown list"""
    reference_files = get_reference_audio_files()
    reference_choices = [os.path.basename(f) for f in reference_files] if reference_files else []
    return gr.update(choices=reference_choices, value=reference_choices[0] if reference_choices else None)

def select_reference_audio(selected_filename):
    """Load selected reference audio file"""
    if selected_filename:
        full_path = os.path.join(cmd_args.input_dir, selected_filename)
        if os.path.exists(full_path):
            return full_path
    return None

def get_final_prompt_audio(uploaded_audio, selected_reference):
    """Determine which audio to use for generation.

    Prefer selected reference (from library) over transient uploaded audio, so
    users can rename and pick from the library explicitly.
    """
    if selected_reference:
        full_path = os.path.join(cmd_args.input_dir, selected_reference)
        if os.path.exists(full_path):
            return full_path
    if uploaded_audio is not None:
        return uploaded_audio
    return None

with gr.Blocks(title="IndexTTS Demo") as demo:
    mutex = threading.Lock()
    gr.HTML('''
    <h2><center>IndexTTS: An Industrial-Level Controllable and Efficient Zero-Shot Text-To-Speech System</h2>
    <h2><center>(一款工业级可控且高效的零样本文本转语音系统)</h2>
<p align="center">
<a href='https://arxiv.org/abs/2502.05512'><img src='https://img.shields.io/badge/ArXiv-2502.05512-red'></a>
</p>
    ''')
    with gr.Tab("音频生成"):
        with gr.Row():
            os.makedirs("prompts",exist_ok=True)
            with gr.Column(scale=1):
                # Reference audio upload (for importing into library)
                prompt_audio = gr.Audio(
                    label="上传参考音频（可先试听，保存入库后用于生成）",
                    key="prompt_audio",
                    sources=["upload","microphone"],
                    type="filepath"
                )
                with gr.Row():
                    filename_input = gr.Textbox(
                        label="自定义文件名（必填）",
                        placeholder="例如：my_ref 或 my_ref.wav（不含路径）",
                        value=f"ref_{time.strftime('%Y%m%d_%H%M%S')}.wav",
                        key="filename_input",
                    )
                save_to_lib_btn = gr.Button("💾 保存到参考库", size="sm")
                saved_filename = gr.Textbox(
                    label="已保存文件名",
                    interactive=False,
                    placeholder="点击上方“保存到参考库”后显示",
                    key="saved_filename",
                )
                
                # Reference audio selection from input folder
                reference_files = get_reference_audio_files()
                reference_choices = [os.path.basename(f) for f in reference_files] if reference_files else []
                reference_dropdown = gr.Dropdown(
                    choices=reference_choices,
                    label="或选择已有参考音频",
                    value=reference_choices[0] if reference_choices else None,
                    interactive=True,
                    key="reference_dropdown"
                )
                
                refresh_button = gr.Button("🔄 刷新音频列表", size="sm")
                
            with gr.Column(scale=2):
                input_text_single = gr.TextArea(label="文本",key="input_text_single", placeholder="请输入目标文本", info="当前模型版本{}".format(tts.model_version or "1.0"))
                infer_mode = gr.Radio(choices=["普通推理", "批次推理"], label="推理模式",info="批次推理：更适合长句，性能翻倍",value="普通推理")        
                gen_button = gr.Button("生成语音", key="gen_button",interactive=True, variant="primary")
            
        output_audio = gr.Audio(label="生成结果", visible=True,key="output_audio")
        with gr.Accordion("高级生成参数设置", open=False):
            with gr.Row():
                with gr.Column(scale=1):
                    gr.Markdown("**GPT2 采样设置** _参数会影响音频多样性和生成速度详见[Generation strategies](https://huggingface.co/docs/transformers/main/en/generation_strategies)_")
                    with gr.Row():
                        do_sample = gr.Checkbox(label="do_sample", value=True, info="是否进行采样")
                        temperature = gr.Slider(label="temperature", minimum=0.1, maximum=2.0, value=1.0, step=0.1)
                    with gr.Row():
                        top_p = gr.Slider(label="top_p", minimum=0.0, maximum=1.0, value=0.8, step=0.01)
                        top_k = gr.Slider(label="top_k", minimum=0, maximum=100, value=30, step=1)
                        num_beams = gr.Slider(label="num_beams", value=3, minimum=1, maximum=10, step=1)
                    with gr.Row():
                        repetition_penalty = gr.Number(label="repetition_penalty", precision=None, value=10.0, minimum=0.1, maximum=20.0, step=0.1)
                        length_penalty = gr.Number(label="length_penalty", precision=None, value=0.0, minimum=-2.0, maximum=2.0, step=0.1)
                    max_mel_tokens = gr.Slider(label="max_mel_tokens", value=600, minimum=50, maximum=tts.cfg.gpt.max_mel_tokens, step=10, info="生成Token最大数量，过小导致音频被截断", key="max_mel_tokens")
                    # with gr.Row():
                    #     typical_sampling = gr.Checkbox(label="typical_sampling", value=False, info="不建议使用")
                    #     typical_mass = gr.Slider(label="typical_mass", value=0.9, minimum=0.0, maximum=1.0, step=0.1)
                with gr.Column(scale=2):
                    gr.Markdown("**分句设置** _参数会影响音频质量和生成速度_")
                    with gr.Row():
                        max_text_tokens_per_sentence = gr.Slider(
                            label="分句最大Token数", value=120, minimum=20, maximum=tts.cfg.gpt.max_text_tokens, step=2, key="max_text_tokens_per_sentence",
                            info="建议80~200之间，值越大，分句越长；值越小，分句越碎；过小过大都可能导致音频质量不高",
                        )
                        sentences_bucket_max_size = gr.Slider(
                            label="分句分桶的最大容量（批次推理生效）", value=4, minimum=1, maximum=16, step=1, key="sentences_bucket_max_size",
                            info="建议2-8之间，值越大，一批次推理包含的分句数越多，过大可能导致内存溢出",
                        )
                    with gr.Accordion("预览分句结果", open=True) as sentences_settings:
                        sentences_preview = gr.Dataframe(
                            headers=["序号", "分句内容", "Token数"],
                            key="sentences_preview",
                            wrap=True,
                        )
            advanced_params = [
                do_sample, top_p, top_k, temperature,
                length_penalty, num_beams, repetition_penalty, max_mel_tokens,
                # typical_sampling, typical_mass,
            ]
        
        if len(example_cases) > 0:
            if reference_choices:
                # Preselect a reference audio from the library for examples
                example_rows = []
                default_ref = reference_choices[0]
                for e in example_cases:
                    # e = [prompt_audio_path, text, infer_mode]
                    example_rows.append([e[0], default_ref, e[1], e[2]])
                gr.Examples(
                    examples=example_rows,
                    inputs=[prompt_audio, reference_dropdown, input_text_single, infer_mode],
                )
            else:
                # No reference files available; bind without dropdown
                gr.Examples(
                    examples=example_cases,
                    inputs=[prompt_audio, input_text_single, infer_mode],
                )

    def on_input_text_change(text, max_tokens_per_sentence):
        if text and len(text) > 0:
            text_tokens_list = tts.tokenizer.tokenize(text)

            sentences = tts.tokenizer.split_sentences(text_tokens_list, max_tokens_per_sentence=int(max_tokens_per_sentence))
            data = []
            for i, s in enumerate(sentences):
                sentence_str = ''.join(s)
                tokens_count = len(s)
                data.append([i, sentence_str, tokens_count])
            
            return {
                sentences_preview: gr.update(value=data, visible=True, type="array"),
            }
        else:
            import pandas as pd
            df = pd.DataFrame([], columns=["序号", "分句内容", "Token数"])
            return {
                sentences_preview: gr.update(value=df)
            }

    input_text_single.change(
        on_input_text_change,
        inputs=[input_text_single, max_text_tokens_per_sentence],
        outputs=[sentences_preview]
    )
    max_text_tokens_per_sentence.change(
        on_input_text_change,
        inputs=[input_text_single, max_text_tokens_per_sentence],
        outputs=[sentences_preview]
    )
    
    # Handle explicit save-to-library action (decoupled from file selection)
    def on_save_to_library(audio_file, custom_name):
        if audio_file is None:
            raise gr.Error("请先选择或录制参考音频")
        if not custom_name or not str(custom_name).strip():
            raise gr.Error("请填写自定义文件名")
        saved_path = save_uploaded_audio(audio_file, custom_name)
        files = get_reference_audio_files()
        choices = [os.path.basename(f) for f in files] if files else []
        value = os.path.basename(saved_path) if saved_path and os.path.exists(saved_path) else (choices[0] if choices else None)
        # Clear uploader value (cannot set new filename visually due to browser limits)
        return gr.update(value=None), gr.update(choices=choices, value=value), os.path.basename(saved_path)

    save_to_lib_btn.click(
        on_save_to_library,
        inputs=[prompt_audio, filename_input],
        outputs=[prompt_audio, reference_dropdown, saved_filename]
    )
    
    # Handle refresh button for reference audio list
    refresh_button.click(
        refresh_reference_list,
        inputs=[],
        outputs=[reference_dropdown]
    )
    
    # Handle reference audio selection
    reference_dropdown.change(
        select_reference_audio,
        inputs=[reference_dropdown],
        outputs=[]  # We don't need to update UI, just internal state
    )

    gen_button.click(gen_single,
                     inputs=[prompt_audio, reference_dropdown, input_text_single, infer_mode,
                             max_text_tokens_per_sentence, sentences_bucket_max_size,
                             *advanced_params,
                     ],
                     outputs=[output_audio])


if __name__ == "__main__":
    demo.queue(20)
    demo.launch(server_name=cmd_args.host, server_port=cmd_args.port)
