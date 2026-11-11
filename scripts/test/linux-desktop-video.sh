#!/usr/bin/env bash
# Decodes recorded clips through the Linux viewer's GStreamer module
# (ui/linux/runner/desktop_video_gst.cc), outside Flutter: the FFI's test
# source plays them, the module decodes them, and a harness stands in for
# Flutter's raster thread (ui/linux/test/desktop_video_test.cc).
#
# Needs: the GStreamer development files (libgstreamer1.0-dev,
# libgstreamer-plugins-base1.0-dev), an H.264 decoder element (gst-libav's
# avdec_h264, or a hardware one), h264parse/h265parse (gst-plugins-bad),
# ffmpeg with libx264 (libx265 adds the H.265 case), cmake.
#
#   scripts/test/linux-desktop-video.sh             hardware decoders first
#   scripts/test/linux-desktop-video.sh --software  hardware decoders ranked out
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FFI="${CARGO_TARGET_DIR:-${ROOT_DIR}/target}/debug/libpb_mapper_ffi.so"
OUT="${ROOT_DIR}/target/linux-desktop-video"
CLIPS="${OUT}/clips"
mkdir -p "${CLIPS}"

if [ "${1:-}" = "--software" ]; then
  # GStreamer's own override: a decoder ranked NONE is never chosen.
  export GST_PLUGIN_FEATURE_RANK="vah264dec:NONE,vah265dec:NONE,nvh264dec:NONE,nvh265dec:NONE,vaapih264dec:NONE,vaapih265dec:NONE,v4l2slh264dec:NONE,v4l2slh265dec:NONE,v4l2h264dec:NONE,v4l2h265dec:NONE"
fi

cargo build --manifest-path "${ROOT_DIR}/Cargo.toml" -p pb-mapper-ffi --features desktop-test-source

# Test patterns as a desktop host sends them: no B-frames, an access unit
# delimiter and the parameter sets before every keyframe, one a second.
clip() {
  local source="$1" frames="$2" out="$3"
  shift 3
  [ -s "${out}" ] || ffmpeg -nostdin -v error -y -f lavfi -i "${source}" -frames:v "${frames}" -pix_fmt yuv420p \
    "$@" "${out}"
}
h264=(-c:v libx264 -profile:v baseline -preset ultrafast -tune zerolatency
  -x264-params "keyint=60:min-keyint=60:scenecut=0:repeat-headers=1:aud=1" -f h264)
clip "testsrc2=size=1280x720:rate=60" 120 "${CLIPS}/h264-720.h264" "${h264[@]}"
clip "testsrc2=size=1920x1080:rate=60" 120 "${CLIPS}/h264-1080.h264" "${h264[@]}"
clip "testsrc2=size=3840x2160:rate=60" 120 "${CLIPS}/h264-2160.h264" "${h264[@]}"
# Noise at a fine quantizer: a keyframe larger than the module's first buffer.
clip "testsrc2=size=1920x1080:rate=60,noise=alls=30:allf=t+u" 60 "${CLIPS}/h264-large.h264" "${h264[@]}" -qp 22
# Not grep -q: under pipefail, ffmpeg would fail on the closed pipe.
if ffmpeg -hide_banner -encoders 2>/dev/null | grep libx265 > /dev/null; then
  clip "testsrc2=size=1280x720:rate=60" 120 "${CLIPS}/h265-720.h265" -c:v libx265 -preset ultrafast -tune zerolatency \
    -x265-params "keyint=60:min-keyint=60:scenecut=0:bframes=0:repeat-headers=1:aud=1:log-level=error" -f hevc
fi

cmake -S "${ROOT_DIR}/ui/linux/test" -B "${OUT}/build" -DCMAKE_BUILD_TYPE=Release > /dev/null
cmake --build "${OUT}/build"
"${OUT}/build/desktop_video_test" "${FFI}" "${CLIPS}"
