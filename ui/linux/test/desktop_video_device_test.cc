// The decoder module's GPU placement (ui/linux/runner/desktop_video_device.cc)
// as pure functions: the order decoders are tried in for a set of GPUs and
// the one that presents the window, and sysfs read from a tree of its own.
// Needs no GPU, GStreamer or EGL; scripts/test/linux-desktop-video.sh runs it
// before the clips.
#include <ftw.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <string>
#include <utility>
#include <vector>

#include "desktop_video_device.h"

namespace {
using pb_video::Decoder;
using pb_video::Gpu;
using pb_video::Topology;

// GStreamer's ranks.
constexpr uint32_t kMarginal = 64, kSecondary = 128, kPrimary = 256;

int g_failed = 0;
void Check(bool ok, const char* what) {
  printf("  %s %s\n", ok ? "ok  " : "FAIL", what);
  if (!ok) ++g_failed;
}

Gpu MakeGpu(const char* node, const char* driver) {
  Gpu gpu;
  gpu.node = node;
  gpu.driver = driver;
  gpu.openable = true;
  return gpu;
}

Decoder Hardware(const char* factory, const char* plugin, uint32_t rank) {
  Decoder decoder;
  decoder.factory = factory;
  decoder.plugin = plugin;
  decoder.hardware = true;
  decoder.rank = rank;
  return decoder;
}

Decoder Software(const char* factory, const char* plugin, uint32_t rank) {
  Decoder decoder = Hardware(factory, plugin, rank);
  decoder.hardware = false;
  return decoder;
}

// The factories in the order they are tried, each with what its name adds.
std::string Ordered(const std::vector<Decoder>& decoders, const Topology& topology,
                    const std::vector<Decoder>* all = nullptr) {
  std::vector<pb_video::Where> where;
  const std::vector<size_t> order = pb_video::Order(decoders, all ? *all : decoders, topology, &where);
  std::string text;
  for (const size_t i : order) {
    text += (text.empty() ? "" : " | ") + decoders[i].factory + pb_video::Describe(where[i]);
  }
  printf("    %s\n", text.c_str());
  return text;
}

Topology Machine(std::vector<Gpu> gpus, const char* presenting) {
  Topology topology;
  topology.gpus = std::move(gpus);
  topology.presenting = presenting;
  return topology;
}

void Write(const std::string& path, const char* text) {
  FILE* file = fopen(path.c_str(), "w");
  if (!file) return;
  fputs(text, file);
  fclose(file);
}

// A GPU in a sysfs tree as Linux lays it out: /sys/class/drm/<node> for
// its render and primary nodes, each with `device`, the PCI device, whose
// `driver` names the kernel driver and whose `drm` lists both nodes.
void FakeGpu(const std::string& root, const char* bus, const char* driver, const char* card, const char* render) {
  const std::string device = root + "/sys/devices/pci0000:00/" + bus;
  const std::string drivers = root + "/sys/bus/pci/drivers/" + driver;
  mkdir(drivers.c_str(), 0755);
  mkdir(device.c_str(), 0755);
  mkdir((device + "/drm").c_str(), 0755);
  if (symlink(drivers.c_str(), (device + "/driver").c_str()) != 0) perror("symlink");
  for (const char* node : {card, render}) {
    const std::string own = device + "/drm/" + node;
    mkdir(own.c_str(), 0755);
    if (symlink(device.c_str(), (own + "/device").c_str()) != 0) perror("symlink");
    if (symlink(own.c_str(), (root + "/sys/class/drm/" + node).c_str()) != 0) perror("symlink");
  }
}

int Remove(const char* path, const struct stat*, int, struct FTW*) { return remove(path); }

void Orders() {
  printf("a hybrid laptop: an Intel GPU presents, an NVIDIA one sleeps\n");
  // NVDEC ranked as high as VA, and listed first: before, it won.
  const std::vector<Decoder> h264 = {Hardware("nvh264dec", "nvcodec", kPrimary + 1),
                                     Hardware("vah264dec", "va", kPrimary + 1),
                                     Software("avdec_h264", "libav", kPrimary),
                                     Software("openh264dec", "openh264", kMarginal)};
  const std::vector<Gpu> laptop = {MakeGpu("renderD128", "i915"), MakeGpu("renderD129", "nvidia")};
  Check(Ordered(h264, Machine(laptop, "")) == "nvh264dec on renderD129 | vah264dec on renderD128 | avdec_h264 | "
                                              "openh264dec",
        "with no presenting GPU known, hardware first, then rank, then the registry's order, as before");
  Check(Ordered(h264, Machine(laptop, "renderD128")) ==
            "vah264dec on renderD128 | nvh264dec on renderD129, another GPU | avdec_h264 | openh264dec",
        "VA on the Intel GPU goes first, NVDEC on the NVIDIA one stays a fallback before software");
  Check(Ordered(h264, Machine(laptop, "renderD129")) ==
            "nvh264dec on renderD129 | vah264dec on renderD128, another GPU | avdec_h264 | openh264dec",
        "rendered on the NVIDIA GPU (PRIME offload), NVDEC goes first");

  printf("Ubuntu 22.04's GStreamer 1.20: VA through gstreamer-vaapi, whose GPU is the display server's\n");
  const std::vector<Decoder> jammy = {Hardware("nvh264dec", "nvcodec", kPrimary),
                                      Hardware("vaapih264dec", "vaapi", kPrimary),
                                      Software("avdec_h264", "libav", kPrimary)};
  Check(Ordered(jammy, Machine(laptop, "renderD128")) ==
            "vaapih264dec | nvh264dec on renderD129, another GPU | avdec_h264",
        "a decoder whose GPU is unknown goes before one known to be on another GPU");
  printf("an NVIDIA driver with no DRM device (nvidia-drm not loaded)\n");
  Check(Ordered(jammy, Machine({MakeGpu("renderD128", "i915")}, "renderD128")) ==
            "vaapih264dec | nvh264dec on another GPU | avdec_h264",
        "NVDEC is still another GPU's when an Intel GPU presents");

  printf("two AMD GPUs, each with its own VA factory\n");
  const std::vector<Decoder> amd = {Hardware("vah264dec", "va", kPrimary + 1),
                                    Hardware("varenderD129h264dec", "va", kPrimary)};
  const std::vector<Gpu> desktop = {MakeGpu("renderD128", "amdgpu"), MakeGpu("renderD129", "amdgpu")};
  Check(Ordered(amd, Machine(desktop, "renderD129")) ==
            "varenderD129h264dec on renderD129 | vah264dec on renderD128, another GPU",
        "the presenting GPU's own factory goes first, though it ranks lower");
  Check(Ordered(amd, Machine(desktop, "renderD128")) ==
            "vah264dec on renderD128 | varenderD129h264dec on renderD129, another GPU",
        "the first GPU's default factory is placed on it");
  // The named factory ranked out (GST_PLUGIN_FEATURE_RANK=
  // varenderD129h264dec:NONE) is no candidate, yet still says the default
  // one is not on renderD129.
  const std::vector<Decoder> first = {amd[0]};
  Check(Ordered(first, Machine(desktop, "renderD129"), &amd) == "vah264dec on renderD128, another GPU",
        "a factory that is no candidate still places the default one");
  Check(Ordered(first, Machine(desktop, "renderD129")) == "vah264dec",
        "without it, the default factory could be on either GPU");

  printf("H.265 on one of two VA GPUs: the default factory could be on either\n");
  const std::vector<Decoder> h265 = {Hardware("vah265dec", "va", kPrimary + 1),
                                     Hardware("nvh265dec", "nvcodec", kPrimary + 1)};
  const std::vector<Gpu> three = {MakeGpu("renderD128", "i915"), MakeGpu("renderD129", "amdgpu"),
                                  MakeGpu("renderD130", "nvidia")};
  Check(Ordered(h265, Machine(three, "renderD129")) == "vah265dec | nvh265dec on renderD130, another GPU",
        "an ambiguous default factory is unknown, not another GPU's");
  Check(Ordered(h265, Machine(three, "renderD130")) == "nvh265dec on renderD130 | vah265dec on another GPU",
        "it is another GPU's when the presenting one is NVIDIA's, which the va plugin does not open");
  std::vector<Gpu> closed = three;
  closed[0].openable = false;
  Check(Ordered(h265, Machine(closed, "renderD129")) ==
            "vah265dec on renderD129 | nvh265dec on renderD130, another GPU",
        "a render node this process cannot open is not the va plugin's");

  printf("NVIDIA's VA driver under GST_VA_ALL_DRIVERS\n");
  const std::vector<Decoder> all_drivers = {Hardware("vah264dec", "va", kPrimary + 1),
                                            Hardware("varenderD129h264dec", "va", kPrimary)};
  Topology nvidia_first = Machine({MakeGpu("renderD128", "nvidia"), MakeGpu("renderD129", "i915")}, "renderD129");
  nvidia_first.va_all_drivers = true;
  Check(Ordered(all_drivers, nvidia_first) ==
            "varenderD129h264dec on renderD129 | vah264dec on renderD128, another GPU",
        "the NVIDIA GPU, first by name, has the default factory");

  printf("two NVIDIA GPUs\n");
  const std::vector<Decoder> two = {Hardware("nvh264dec", "nvcodec", kPrimary),
                                    Hardware("nvh264device1dec", "nvcodec", kPrimary - 1),
                                    Hardware("v4l2slh264dec", "v4l2codecs", kSecondary)};
  Check(Ordered(two, Machine({MakeGpu("renderD128", "nvidia"), MakeGpu("renderD129", "nvidia")}, "renderD128")) ==
            "nvh264dec | nvh264device1dec | v4l2slh264dec",
        "CUDA's numbering is unknown without waking a GPU: rank decides, as before");

  printf("a presenting node sysfs does not list\n");
  Check(Ordered(h264, Machine(laptop, "renderD200")) ==
            "nvh264dec on renderD129 | vah264dec on renderD128 | avdec_h264 | openh264dec",
        "placement is unknown, and the order is the one before");
}

void Sysfs() {
  printf("sysfs read from a tree of its own\n");
  const char* temporary = getenv("TMPDIR");
  std::string pattern = std::string(temporary && *temporary ? temporary : "/tmp") + "/pb-video-device-XXXXXX";
  std::vector<char> folder(pattern.begin(), pattern.end());
  folder.push_back('\0');
  if (!mkdtemp(folder.data())) {
    Check(false, "a temporary folder");
    return;
  }
  const std::string root = folder.data();
  for (const char* path : {"/sys", "/sys/class", "/sys/class/drm", "/sys/devices", "/sys/devices/pci0000:00",
                           "/sys/bus", "/sys/bus/pci", "/sys/bus/pci/drivers", "/dev", "/dev/dri"}) {
    mkdir((root + path).c_str(), 0755);
  }
  // Probed in the other order: renderD129 belongs to card0.
  FakeGpu(root, "0000:01:00.0", "nvidia", "card0", "renderD129");
  FakeGpu(root, "0000:00:02.0", "i915", "card1", "renderD128");
  Write(root + "/dev/dri/renderD128", "");
  // A card node with no render node beside it, as simpledrm's.
  mkdir((root + "/sys/class/drm/card2").c_str(), 0755);

  const std::vector<Gpu> gpus = pb_video::ReadGpus(root + "/sys", root + "/dev");
  Check(gpus.size() == 2 && gpus[0].node == "renderD128" && gpus[1].node == "renderD129",
        "every render node, sorted by name");
  Check(gpus.size() == 2 && gpus[0].driver == "i915" && gpus[1].driver == "nvidia", "each with its driver");
  Check(gpus.size() == 2 && gpus[0].openable && !gpus[1].openable, "and whether its device file opens");
  const std::string sys = root + "/sys";
  Check(pb_video::RenderNode("/dev/dri/card1", sys) == "renderD128", "a primary node maps to its GPU's render node");
  Check(pb_video::RenderNode("/dev/dri/card0", sys) == "renderD129", "whatever the numbering");
  Check(pb_video::RenderNode("/dev/dri/renderD129", sys) == "renderD129", "a render node is itself");
  Check(pb_video::RenderNode("/dev/dri/card2", sys).empty() && pb_video::RenderNode("/dev/dri/card7", sys).empty() &&
            pb_video::RenderNode("", sys).empty() && pb_video::RenderNode("/dev/dri/..", sys).empty(),
        "anything else has none");
  if (nftw(root.c_str(), Remove, 16, FTW_DEPTH | FTW_PHYS) != 0) perror("nftw");
}
}  // namespace

int main() {
  Orders();
  Sysfs();
  printf("%s: %d check(s) failed\n", g_failed ? "FAILED" : "PASSED", g_failed);
  return g_failed ? 1 : 0;
}
