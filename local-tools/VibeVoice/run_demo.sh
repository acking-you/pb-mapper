#!/bin/bash

echo "VibeVoice Demo Script"
echo "====================="

echo "1. Testing VibeVoice import..."
uv run python -c "import vibevoice; print('✓ VibeVoice imported successfully!')"

if [ $? -eq 0 ]; then
    echo "✓ VibeVoice is ready!"
    echo
    echo "Available commands:"
    echo "  1. Launch Gradio demo (1.5B model):"
    echo "     uv run python demo/gradio_demo.py --model_path microsoft/VibeVoice-1.5B --share"
    echo
    echo "  2. Launch Gradio demo (Large model):"  
    echo "     uv run python demo/gradio_demo.py --model_path microsoft/VibeVoice-Large --share"
    echo
    echo "  3. Test inference from file (1 speaker):"
    echo "     uv run python demo/inference_from_file.py --model_path microsoft/VibeVoice-Large --txt_path demo/text_examples/1p_abs.txt --speaker_names Alice"
    echo
    echo "  4. Test inference from file (2 speakers):"
    echo "     uv run python demo/inference_from_file.py --model_path microsoft/VibeVoice-Large --txt_path demo/text_examples/2p_music.txt --speaker_names Alice Frank"
    echo
    echo "Available text examples:"
    ls demo/text_examples/
else
    echo "❌ VibeVoice import failed. Check if dependencies are fully installed."
fi