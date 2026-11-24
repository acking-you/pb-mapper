// Which GPU a GStreamer decoder decodes on, and the order the decoder
// module (desktop_video_gst.cc) tries them in: hardware first, those on the
// GPU that presents the window first among them. Read from the registry's
// factory names and from sysfs only, so placing them loads no plugin and
// opens no device, either of which can wake a GPU that sleeps (a hybrid
// laptop's NVIDIA one). Plain C++ and POSIX, built into the module and into
// the unit test ui/linux/test/desktop_video_device_test.cc.
#ifndef RUNNER_DESKTOP_VIDEO_DEVICE_H_
#define RUNNER_DESKTOP_VIDEO_DEVICE_H_

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

namespace pb_video {

// A GPU's DRM render node, as sysfs describes it.
struct Gpu {
  // "renderD128".
  std::string node;
  // Its kernel driver: "i915", "amdgpu", "nvidia".
  std::string driver;
  // Whether this process may open the node, as a VA display must.
  bool openable = false;
};

// Every render node, sorted by name as GStreamer's va plugin sorts them.
// `sysfs` and `dev` are where /sys and /dev are mounted (a test's own trees).
std::vector<Gpu> ReadGpus(const std::string& sysfs = "/sys", const std::string& dev = "/dev");

// The render node ("renderD128") of the GPU whose DRM device file is
// `device`: a render node ("/dev/dri/renderD128") or a primary one
// ("/dev/dri/card0"), as EGL names it. Empty when there is none.
std::string RenderNode(const std::string& device, const std::string& sysfs = "/sys");

// A decoder factory the registry has for a codec.
struct Decoder {
  // "vah264dec".
  std::string factory;
  // The plugin that registered it: "va", "nvcodec", "vaapi", "libav".
  std::string plugin;
  bool hardware = false;
  uint32_t rank = 0;
};

// Where a decoder decodes, as seen from the GPU that presents.
enum class Placement { kPresenting = 0, kUnknown = 1, kOther = 2 };

struct Where {
  Placement placement = Placement::kUnknown;
  // The render node it decodes on, when the registry and sysfs say.
  std::string node;
};

// The machine's GPUs and the one that presents the window: its render
// node, or empty when that is unknown (a software renderer, an EGL that
// names no device).
struct Topology {
  std::vector<Gpu> gpus;
  std::string presenting;
  // GST_VA_ALL_DRIVERS: the va plugin opens every VA driver, NVIDIA's
  // included, instead of Intel's and Mesa's only.
  bool va_all_drivers = false;
};

// Where `decoder` decodes. `all` is every decoder factory the registry has
// for the same codec, whatever its rank: the va plugin names a GPU's factory
// after its render node, except the first GPU's.
Where Locate(const Decoder& decoder, const std::vector<Decoder>& all, const Topology& topology);

// The order to try `decoders` in, as indices into it: hardware first, and
// among hardware decoders those on the presenting GPU, then those whose GPU
// is unknown, then those on another GPU; then by rank, highest first; the
// registry's order otherwise. `where`, when given, gets each decoder's
// Where, by its index in `decoders`. With no presenting GPU known, this is
// hardware first, then rank.
std::vector<size_t> Order(const std::vector<Decoder>& decoders, const std::vector<Decoder>& all,
                          const Topology& topology, std::vector<Where>* where);

// What a decoder's name adds about its GPU: " on renderD128",
// " on renderD129, another GPU", " on another GPU", or nothing.
std::string Describe(const Where& where);

}  // namespace pb_video

#endif  // RUNNER_DESKTOP_VIDEO_DEVICE_H_
