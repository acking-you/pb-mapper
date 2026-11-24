# Mac VideoToolbox padded capture: samples, checks, the dwell A/B and the device runs still due

A Mac host now codes a picture whose width or height is not a multiple of
16 (a 14-inch MacBook Pro's 3024x1964 or a 13-inch MacBook Air's 2940x1912
at Native, 1920x1080, a scaled size) in a frame padded to the next
multiple of 16: ScreenCaptureKit draws the display into its top left
(`destinationRect`), the VideoToolbox session codes the padded frame, and
each keyframe's SPS goes out cropped to the content
([H.265 and H.264](../../remote-desktop.md#h265-and-h264),
[Host: capture and encoding](../../remote-desktop.md#host-capture-and-encoding)).
Everything here ran on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0
(26A425) and Rust 1.99.1, with other builds running on the machine at the
same time (load averages from about 2 to 10). No run here captured the
screen: its display slept or its session was locked throughout. What
ScreenCaptureKit hands a host at these sizes, which decides whether a host
gains anything, is still to be seen on a device (below).

## Parameter sets

`video_toolbox_parameter_sets` (`platform/macos/tests.rs`, ignored) has
VideoToolbox encode one keyframe at each coded size a Mac host pads
1920x1080, 3024x1964 and 3340x2160 to (1920x1088, 3024x1968, 3344x2160),
in the three configurations a session takes: H.264 with low-latency rate
control (Constrained High asked for), H.264 without it (High), and H.265
with it (Main), and prints the parameter sets in front of it:
[`parameter_sets.txt`](parameter_sets.txt), from
`cargo test -p pb-mapper-desktop --all-features --lib -- --ignored
--nocapture video_toolbox_parameter_sets`. `codec::sps`'s `PADDED` samples
are these bytes. VideoToolbox writes each SPS at the coded size it was
given, with no cropping; this run's Constrained High SPS carries
`constraint_set4` and `constraint_set5` (`0x0c`).

## Checks

- `cargo test -p pb-mapper-desktop --all-features --lib codec::sps`: each
  sample's SPS shows its coded size and, cropped, its content's, cropped
  from the right and the bottom by half the padding in chroma samples;
  every other field of an H.264 SPS, and every bit of an H.265 SPS before
  and after its conformance window, is as VideoToolbox wrote it, and so is
  every other set; cropping again changes nothing and cropping to the
  coded size gives VideoToolbox's SPS back; every H.265 SPS and its crop
  write back byte for byte. H.265 SPS built bit by bit with up to six
  sub-layers, their profiles and levels present or not, are read to their
  window; what would not show the content exactly is refused (a larger
  size, an odd number of rows or columns to crop, an empty size, 4:4:4,
  field coding, an SPS cut before its window, the other codec's SPS, sets
  without an SPS, JPEG).
- `core_media_shows_a_cropped_sps_at_its_content_size`: Core Media
  (`CMVideoFormatDescriptionCreateFromH264ParameterSets` and
  `...HEVCParameterSets`) reads each sample at its coded size and each
  crop at its content's: 1920x1080, 3024x1964 and 3340x2160.
- `a_padded_session_sends_the_parameter_sets_of_its_content_size`: at
  1920x1080 (padded at the bottom), 2216x1440 (at the right) and
  2940x1912 (at both), in each configuration above, a padded session's
  keyframe carries, byte for byte, the parameter sets a session at the
  content's own size writes, whose SPS already crops those rows and
  columns: a Mac host sent right-edge crops before this change at widths
  that are not a multiple of 16, and no viewer gets a header it did not
  get before. A session that says it encodes in software (a virtual
  machine's may; low-latency sessions say nothing) is reported and not
  compared. With the crop moved to the left and top, this test fails.
- `a_padded_frame_decodes_to_its_content`: a 2944x1920 NV12 picture, grey
  2940x1912 content with a white last row and column and black beyond,
  encoded by a session of `Frame::padded(2940, 1912)` and decoded by
  `VTDecompressionSession` from the parameter sets in front of the
  keyframe, comes out 2940x1912 with the white row and column at its
  edges, in H.264 and H.265. With the crop left out of the encoder it
  comes out 2944x1920 and the test fails. With the crop moved to the left
  and top instead, VideoToolbox's decoder still gave the top-left
  2940x1912 and the test passed: it takes the crop's size and not its
  edges, so the `codec::sps` tests and the parameter-set comparison above
  are what pin the crop to the right and bottom edges.
- `a_padded_session_crops_its_parameter_sets_once_per_format`: the crop is
  read at the first keyframe and not for the units after it, none of
  which carries a parameter set; a second keyframe of the same session
  came with the same format and read nothing. The encoder counts its
  crops and, apart, the formats they were for: an encoder that cropped
  again for every keyframe fails it.
- `an_sps_that_will_not_crop_fails_the_capture_and_turns_padding_off`: a
  session whose SPS cannot crop to its content (an odd number of rows)
  fails its first keyframe as `CaptureFailed`, not as the codec's failure,
  and `encoder::pads()` is false after.
- `a_padded_capture_draws_into_the_top_left`: a padded frame's
  ScreenCaptureKit configuration asks for the coded size with
  `destinationRect` at the origin and the content's size; a frame of its
  own size sets none.

All the mutations above were made here and taken back.

## Dwell A/B

`video_toolbox_padded` (`platform/macos/tests.rs`, ignored) paces a real
session as thread C paces it, on eight NV12 IOSurface pictures painted
before the run (scrolling block text, a still ramp and fresh grain of ±16
over the luma, which no reference predicts), with a Mac host's gate of
two units in flight (`VT_IN_FLIGHT`), for 1 s and then 5 s measured, at
Balanced's starting bits per frame scaled by the content's size. Each
case runs three arms: the content coded at its own size on pictures of
that size (unpadded); the same pictures in the top left of a padded frame,
black around them, whose SPS crops them (padded); and a session at the
content's own size on pictures of that size whose buffers say they extend
to the padded size at the right and bottom
(`kCVPixelBufferExtendedPixelsRightKey` and `...BottomKey`, the extension
filled from the picture's edges), which is what a host already had where
ScreenCaptureKit's own pictures extend so (extended). Five sizes a Mac
host pads, both codecs, paces of 60 and 120, five rounds, each starting
the arms one further along. The table gives each case's medians over its
rounds of units out a second, their dwell from submit to output at the
median (the `encode_us` a unit carries, which thread C reports as
`encoder_dwell_ms`), and the median VideoToolbox encode call on the
submit thread; every case's own line is in
[`video_toolbox_padded.txt`](video_toolbox_padded.txt), from
`cargo test --release -p pb-mapper-desktop --all-features --lib --
--ignored --nocapture --test-threads=1 video_toolbox_padded` (1809 s, load
averages from about 2 to 5).

| codec | size | coded | pace | units/s unpadded | padded | extended | dwell p50 unpadded (ms) | padded | extended | lower padded (ms) | VT call p50 unpadded (µs) | padded | extended |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| H.264 | 1920x1080 | 1920x1088 | 60 | 60.0 | 60.0 | 60.0 | 5.81 | 4.68 | 4.76 | 1.13 | 38 | 4752 | 4823 |
| H.264 | 1920x1080 | 1920x1088 | 120 | 120.0 | 120.0 | 120.0 | 5.77 | 4.78 | 4.75 | 0.99 | 30 | 4838 | 4810 |
| H.265 | 1920x1080 | 1920x1088 | 60 | 60.0 | 60.0 | 60.0 | 5.78 | 4.63 | 4.69 | 1.15 | 34 | 4692 | 4760 |
| H.265 | 1920x1080 | 1920x1088 | 120 | 120.0 | 120.0 | 120.0 | 5.68 | 4.64 | 4.65 | 1.04 | 30 | 4709 | 4716 |
| H.264 | 2216x1440 | 2224x1440 | 60 | 60.0 | 60.0 | 60.0 | 7.95 | 6.65 | 6.64 | 1.30 | 35 | 6719 | 6699 |
| H.264 | 2216x1440 | 2224x1440 | 120 | 120.0 | 120.0 | 120.0 | 7.41 | 6.46 | 6.52 | 0.95 | 21 | 6529 | 6578 |
| H.265 | 2216x1440 | 2224x1440 | 60 | 60.0 | 60.0 | 60.0 | 7.79 | 6.44 | 6.49 | 1.35 | 34 | 6500 | 6556 |
| H.265 | 2216x1440 | 2224x1440 | 120 | 120.0 | 120.0 | 120.0 | 7.67 | 6.37 | 6.39 | 1.30 | 30 | 6427 | 6455 |
| H.264 | 2940x1912 | 2944x1920 | 60 | 60.0 | 60.0 | 60.0 | 12.30 | 10.44 | 10.40 | 1.86 | 32 | 10505 | 10471 |
| H.264 | 2940x1912 | 2944x1920 | 120 | 96.4 | 94.8 | 95.2 | 16.59 | 10.28 | 10.30 | 6.31 | 29 | 10338 | 10357 |
| H.265 | 2940x1912 | 2944x1920 | 60 | 60.0 | 60.0 | 60.0 | 12.02 | 10.11 | 10.11 | 1.91 | 33 | 10170 | 10174 |
| H.265 | 2940x1912 | 2944x1920 | 120 | 99.8 | 97.0 | 96.2 | 15.79 | 10.10 | 10.09 | 5.69 | 21 | 10163 | 10154 |
| H.264 | 3024x1964 | 3024x1968 | 60 | 60.0 | 60.0 | 60.0 | 12.88 | 10.71 | 10.94 | 2.17 | 33 | 10775 | 11009 |
| H.264 | 3024x1964 | 3024x1968 | 120 | 91.4 | 91.8 | 92.4 | 17.65 | 10.76 | 10.25 | 6.89 | 28 | 10828 | 10272 |
| H.265 | 3024x1964 | 3024x1968 | 60 | 60.0 | 60.0 | 60.0 | 12.64 | 10.38 | 10.69 | 2.26 | 32 | 10414 | 10756 |
| H.265 | 3024x1964 | 3024x1968 | 120 | 93.0 | 92.2 | 91.8 | 17.30 | 10.62 | 10.66 | 6.68 | 29 | 10685 | 10722 |
| H.264 | 3340x2160 | 3344x2160 | 60 | 60.0 | 60.0 | 60.0 | 15.29 | 12.86 | 12.95 | 2.43 | 34 | 12936 | 13004 |
| H.264 | 3340x2160 | 3344x2160 | 120 | 75.0 | 74.4 | 74.4 | 22.35 | 12.98 | 12.89 | 9.37 | 31 | 13070 | 12962 |
| H.265 | 3340x2160 | 3344x2160 | 60 | 60.0 | 60.0 | 60.0 | 14.98 | 12.71 | 12.71 | 2.27 | 33 | 12772 | 12781 |
| H.265 | 3340x2160 | 3344x2160 | 120 | 77.6 | 76.8 | 76.6 | 21.50 | 12.62 | 12.62 | 8.88 | 31 | 12682 | 12680 |

VideoToolbox encoded the unpadded pictures in the background, its call
returning in 21 to 38 µs, and the padded and extended ones inside its
call, which took as long as the encode (4.7 to 13.1 ms by size). So the
fast path follows the picture's buffer, not the session's size: a buffer
that covers a multiple of 16 both ways, by its size or with the pixels it
says it extends by. The padded and extended arms are the same within the
rounds' spread: dwell within 0.51 ms of each other and units a second
within 0.8.

The pass the plan set, a dwell at least 1 ms lower at the 1440p and
4K-class sizes, holds in every case there but H.264 at 120 at 2216x1440,
0.95 ms lower: 1.30 to 2.43 ms lower at 60 FPS, and at 120, where
2940x1912 and above outrun the encoder, 5.69 to 9.37 ms. At 1920x1080 it
was 0.99 to 1.15 ms lower. Units a second were the same at 60 FPS, and at
120 at 1920x1080 and 2216x1440. At 120 at the three largest sizes the
encoder makes as many as it can, and padded made from 2.8 fewer to 0.4
more a second than unpadded, at the median, with the rounds' ranges
overlapping (H.264 at 2940x1912: 90.8–97.2 padded against 91.6–98.8
unpadded; H.265 at 2940x1912, the largest gap: 93.6–100.2 against
94.0–102.2), and extended from 3.6 fewer to 1.0 more: whatever encoding
inside the call costs there, a host whose ScreenCaptureKit pictures
already extended to 16 paid it too.

A first run of this test, with only its unpadded and padded arms
([`video_toolbox_padded_first_run.txt`](video_toolbox_padded_first_run.txt),
1206 s, load averages from about 2 to 10), gave the same dwells within
0.8 ms (at 3024x1964 and 60 FPS, 12.96 against 10.97 ms with H.264
and 12.73 against 10.75 ms with H.265), and at 120 at the three largest
sizes H.264 2 to 4 fewer units a second padded (95.0 against 99.2 at
2940x1912, 89.4 against 91.2 at 3024x1964, 75.0 against 77.0 at
3340x2160) and H.265 within 1. The gap at 120 moved between codecs and
sizes from run to run, and stayed within the rounds' ranges.

## Device runs still due

- The gate first, which decides whether a host gains anything: what
  ScreenCaptureKit's own pictures are at a size that is not a multiple of
  16. `sck_padded_capture` (`platform/macos/tests.rs`, ignored) streams
  the main display at 3024x1964, 2940x1912 and 1920x1080, unpadded and
  padded, prints each first picture's layout (its size, the pixels it
  says it extends by at the right and bottom, each plane's rows and bytes
  per row) and the median H.264 encode call on it beside one on a picture
  made at the same size: `cargo test --release -p pb-mapper-desktop --lib
  -- --ignored --nocapture --test-threads=1 sck_padded_capture`, with the
  display awake and unlocked. A MacBook Pro 14 at Native shows a host's
  own case; on a build without this change, its `macos_capture_layout`
  debug line says the same. Where an unpadded picture says it extends to
  16, or its call takes milliseconds, VideoToolbox already encoded it
  inside its call and padding gains nothing there. Here, Screen Recording
  was granted but the session was locked, and the test did not run.
- ScreenCaptureKit drawing into `destinationRect` as asked: the content
  in the top left, in pixels, filling it (macOS 14), black around it
  (`sck_padded_capture` fails otherwise), and no row or column of the
  display lost; before macOS 14, its shape kept.
- A 14-inch MacBook Pro host at Native: `encoder_dwell_ms` before and
  after.
- A viewer of a padded host: clicks landing at the right and bottom
  edges. Its parameter sets are those it got before (above), so the
  picture's size and crop are not in question.
