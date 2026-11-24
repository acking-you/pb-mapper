# Linux video plane scaling: checks and measurements

The Linux viewer's video plane (`ui/linux/runner/desktop_video_plane.cc`)
scaling its picture into the placed rectangle with `PlaneGl`
(`ui/linux/runner/desktop_video_gl.cc`): area-averaged when shown smaller,
from mipmaps below a third, and, with **Sharpen enlarged picture** on, a
clamped Catmull-Rom enlargement with luma sharpening (CAS at airhand's
peaks). No Linux machine was at hand: everything here ran on a 12-core Apple
M6 with 24 GiB, macOS 27.0 (26A425), through Apple's OpenGL 4.1 core profile,
which runs on Metal, with no display involved
([environment.txt](environment.txt)). So these are the runner's own GL code
and shaders, on the desktop GL path a GDK context takes, but not a Linux
driver, not GDK's frame clock and not a screen. Other agents were building in
parallel worktrees throughout.

## The harness

[planecheck.cc](planecheck.cc), built and run by [run.sh](run.sh), compiles
the runner's `desktop_video_gl.cc` unchanged, as the app builds it (C++14,
`-Wall -Werror`, `-O3`), against [macos-gl/](macos-gl/), a stand-in for
libepoxy's headers over Apple's OpenGL in which EGL answers that it has
nothing (a converter made as GLX's, `Owner::kOwnGlx`, never asks it). The
pictures are drawn with Core Text: lines of text from 9 to 14 pixels,
proportional and monospaced, dark on light and, on the right third, light on
dark, with one- and two-pixel rules, a red line and a grey ramp. Each is
converted to NV12 as a host sends it (BT.709, video range, chroma the mean
of each 2x2), and the runner's own converter turns that into the RGBA
texture the plane draws, as on Linux. The CPU references are
[ui/linux/test/plane_reference.h](../../../ui/linux/test/plane_reference.h),
which the clip harness CI runs also uses. Output:
[planecheck.txt](planecheck.txt); crops: [crops/](crops/).

## Checks

A 1280x720 text picture in a framebuffer 48 pixels wider and 40 taller,
each draw read back and compared over its visible part with its filter's CPU
reference, and everything else checked black:

| Placement | Filter drawn | PSNR against the reference | Largest difference |
| --- | --- | --- | --- |
| 1:1, cut to a clip | bilinear (a copy) | identical | 0 |
| 0.5x, 0.375x | area | 66.7, 66.2 dB | 1 |
| 0.75x with Sharpen at High | area, no sharpening | 67.7 dB | 1 |
| 0.25x | areaMipmapped, against the exact area average | 41.8 dB | 14 |
| 1.5x, Sharpen off | bilinear | 62.6 dB | 1 |
| 1:1, Medium | sharpen | identical | 0 |
| 2x past the window, the picture's corner in view, High | upscaleSharpen | 66.5 dB | 3 |
| 1.5x cut to a clip, Low | upscaleSharpen | 63.3 dB | 2 |
| 1.25x, Medium, grey still | upscaleSharpen | 59.2 dB | 1 |

At 0.25x the mipmapped filter came to 41.8 dB of the exact area average
where bilinear, the plane's filter before, came to 22.1 dB. After that draw
the picture's minifying filter was linear again and the converter's still
(its own 16-tap reduction) matched the area average at 65.9 dB, so it read
the picture, not stale mipmaps. Twenty timed draws gave twenty GPU times,
and turning the timing off dropped them; turning the enhancement off freed
its texture with no GL error. The clip harness's own checks of the plane
(`CheckPlaneDrawing`, which CI runs on llvmpipe in GL ES and desktop GL) also
passed when built into a scratch driver against the same stand-in, on a
synthetic NV12 pattern; that run is not recorded here.

## Quality

SSIM of luma (8x8 windows, a pixel apart) against a Lanczos-3 reference of
the same picture computed on the CPU, and, for shrinking, PSNR against the
exact area average.

A 2560x1440 picture shown smaller, the plane's filter against bilinear, which
the plane drew at every scale before:

| Scale | Size | Filter | SSIM, plane | SSIM, bilinear | PSNR to area average, plane | PSNR, bilinear |
| --- | --- | --- | --- | --- | --- | --- |
| 0.75x | 1920x1080 | area | 0.9845 | 0.9825 | 40.3 dB | 35.9 dB |
| 0.5x | 1280x720 | area | 0.9840 | 0.9755 | 37.2 dB | 70.1 dB |
| 0.375x | 960x540 | area | 0.9816 | 0.8839 | 50.3 dB | 23.0 dB |
| 0.25x | 640x360 | areaMipmapped | 0.9741 | 0.8231 | 41.0 dB | 21.2 dB |

At exactly half size, with the picture on whole pixels, each bilinear tap
falls between four source pixels and is their mean, so bilinear is the
2x2 area average there; the area filter, which weighs a few more pixels, is
a little softer than that but closer to Lanczos. Between whole ratios, and
below half, bilinear skips pixels and aliases text (0.88 and 0.82 SSIM at
0.375x and 0.25x), which the area filter does not.

A 1920x1080 picture shown larger, the top left 1280x720 of the window,
with the mean luma gradient (0-255 a pixel) as edge contrast:

| Scale | Level | Filter | SSIM | Edge contrast |
| --- | --- | --- | --- | --- |
| 1.5x | Off | bilinear | 0.9760 | 16.48 |
| 1.5x | Low | upscaleSharpen | 0.9890 | 19.76 |
| 1.5x | Medium | upscaleSharpen | 0.9895 | 20.40 |
| 1.5x | High | upscaleSharpen | 0.9895 | 21.17 |
| 1.5x | Lanczos-3 | | | 21.63 |
| 2x | Off | bilinear | 0.9737 | 10.40 |
| 2x | Low | upscaleSharpen | 0.9867 | 12.69 |
| 2x | Medium | upscaleSharpen | 0.9871 | 13.03 |
| 2x | High | upscaleSharpen | 0.9871 | 13.43 |
| 2x | Lanczos-3 | | | 14.32 |

Crops of the same text: [2x, Off](crops/2x-off.png),
[Low](crops/2x-low.png), [Medium](crops/2x-medium.png),
[High](crops/2x-high.png); [0.5x area](crops/0.5x-area.png),
[bilinear](crops/0.5x-bilinear.png) and [Lanczos-3](crops/0.5x-lanczos.png).

## GPU time

The plane's own `GL_TIME_ELAPSED` queries (what `planeScaleGpuP50Ms` and
`planeScaleGpuP95Ms` report), from the clear's end to the last pass's, the
whole window filled with the picture, 300 draws after 30, each finished
before the next. Apple's GL on the M6, in ms:

| Picture | Window | Level | Filter | p50 | p95 |
| --- | --- | --- | --- | --- | --- |
| 1920x1080 | 1920x1080 | Off | bilinear | 0.011 | 0.047 |
| 1920x1080 | 1920x1080 | Medium | sharpen | 0.068 | 0.072 |
| 1920x1080 | 2560x1440 | Off | bilinear | 0.058 | 0.066 |
| 1920x1080 | 2560x1440 | Medium | upscaleSharpen | 0.465 | 0.482 |
| 1920x1080 | 3840x2160 | Off | bilinear | 0.177 | 0.191 |
| 1920x1080 | 3840x2160 | Medium | upscaleSharpen | 1.109 | 1.128 |
| 2560x1440 | 3840x2160 | High | upscaleSharpen | 1.092 | 1.114 |
| 2560x1440 | 1920x1080 | Off | area | 0.077 | 0.083 |
| 3840x2160 | 1920x1080 | Off | area | 0.182 | 0.203 |
| 3840x2160 | 1280x720 | Off | area | 0.156 | 0.170 |
| 3840x2160 | 960x540 | Off | areaMipmapped, a new picture each draw | 0.202 | 0.218 |
| 3840x2160 | 960x540 | Off | areaMipmapped, one picture redrawn | 0.185 | 0.199 |

The enlargement costs a pass at the visible region's size plus the
sharpening at the window's; a redraw of one picture skips the mipmaps.

## GLSL ES 1.00

Apple's GL takes no GLSL ES, so the plane's shaders as the runner builds
them for a GL ES context (`GDK_GL=gles`) were dumped by
[shaderdump.cc](shaderdump.cc) and compiled, linked and drawn once each in
WebGL 1 and WebGL 2 by the page [es100check.sh](es100check.sh) writes
([es100check.py](es100check.py)), in Chromium, whose WebGL validates GLSL
ES 1.00 through ANGLE: every program compiled and linked with an empty log
and drew with no GL error ([es100check.txt](es100check.txt)). CI's clip
harness compiles and draws them on llvmpipe in a real GL ES context.

## Still to measure on a Linux device

What the plane procedure in
[the acceptance procedure](../../../scripts/test/desktop-ui-acceptance.md#linux-viewer)
lists: the same text remote at 0.375x, 0.5x, 0.75x, 1.5x and 2x with
Sharpen off and on, captured with `grim` (Wayland) or `import -window root`
(X11) and compared side by side and by SSIM against a CPU Lanczos reference
of the same frame; `planeScaleGpuP50Ms`/`P95Ms` with `intel_gpu_top` or
`nvidia-smi dmon` beside them on an Intel iGPU and an NVIDIA desktop; and
the presented fps and `planeIntervalP95Ms`, which must not change, at 120
frames a second.
