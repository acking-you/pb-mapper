#include "desktop_video_device.h"

#include <dirent.h>
#include <unistd.h>

#include <algorithm>
#include <cstdlib>
#include <cstring>

namespace pb_video {
namespace {

constexpr char kRenderPrefix[] = "renderD";
constexpr char kCardPrefix[] = "card";
// NVIDIA's own kernel driver, the only one CUDA (and so NVDEC) runs on.
constexpr char kNvidiaDriver[] = "nvidia";
constexpr char kVaPlugin[] = "va";
constexpr char kNvcodecPlugin[] = "nvcodec";

bool StartsWith(const std::string& text, const char* prefix) { return text.compare(0, strlen(prefix), prefix) == 0; }

std::string Basename(const std::string& path) {
  const size_t slash = path.find_last_of('/');
  return slash == std::string::npos ? path : path.substr(slash + 1);
}

std::string ReadLink(const std::string& path) {
  char target[4096];
  const ssize_t length = readlink(path.c_str(), target, sizeof(target));
  if (length <= 0 || static_cast<size_t>(length) >= sizeof(target)) return "";
  return std::string(target, static_cast<size_t>(length));
}

// The names in `folder` that start with `prefix`.
std::vector<std::string> List(const std::string& folder, const char* prefix) {
  std::vector<std::string> names;
  DIR* dir = opendir(folder.c_str());
  if (!dir) return names;
  while (const dirent* entry = readdir(dir)) {
    const std::string name = entry->d_name;
    if (StartsWith(name, prefix) && name.size() > strlen(prefix)) names.push_back(name);
  }
  closedir(dir);
  std::sort(names.begin(), names.end());
  return names;
}

const Gpu* Find(const std::vector<Gpu>& gpus, const std::string& node) {
  if (node.empty()) return nullptr;
  for (const Gpu& gpu : gpus) {
    if (gpu.node == node) return &gpu;
  }
  return nullptr;
}

// A va factory's name: "va" + the render node for every GPU but the first
// ("varenderD129h264dec"), then the codec ("h264dec"). False for any other.
bool ParseVa(const std::string& factory, std::string* node, std::string* codec) {
  if (!StartsWith(factory, kVaPlugin)) return false;
  size_t at = strlen(kVaPlugin);
  node->clear();
  if (factory.compare(at, strlen(kRenderPrefix), kRenderPrefix) == 0) {
    size_t end = at + strlen(kRenderPrefix);
    while (end < factory.size() && factory[end] >= '0' && factory[end] <= '9') ++end;
    if (end == at + strlen(kRenderPrefix)) return false;
    *node = factory.substr(at, end - at);
    at = end;
  }
  *codec = factory.substr(at);
  return !codec->empty();
}

// Whether the va plugin may open `gpu` as a VA display: this process may
// open its node, and GStreamer takes its driver. From 1.24 that is Intel's
// or Mesa's, NVIDIA's only under GST_VA_ALL_DRIVERS; before, any driver.
bool VaOpens(const Gpu& gpu, const Topology& topology) {
  const bool filters = topology.gst_major == 1 && topology.gst_minor >= 24 && !topology.va_all_drivers;
  return gpu.openable && (!filters || gpu.driver != kNvidiaDriver);
}

// Whether the va plugin's default factory for a codec is on a GPU that sorts
// before every GPU with a factory named after it. The plugin names a
// codec's factory "va<codec>dec" on one GPU and "va<node><codec>dec" on
// every other, and which one has the default name changed:
// - 1.20 goes through the GPUs it opens in order of their device files and
//   gives it to the first that decodes the codec;
// - 1.22 gives it to the first GPU it opened in udev's order, which follows
//   the PCI devices, not the device files;
// - 1.24 on gives it to the first it opens in order of their device files.
// So 1.22, and a version not known here, say no more than that it is on a
// GPU with no factory of its own.
bool VaDefaultSortsFirst(const Topology& topology) {
  return topology.gst_major == 1 && (topology.gst_minor == 20 || topology.gst_minor >= 24);
}

// A named va factory is on the render node its name carries. The default
// one is on a GPU the va plugin may open, with no factory of its own for
// the codec and, where VaDefaultSortsFirst, sorted before every named one.
// When more than one GPU fits, which one is unknown (whether a VA driver is
// installed is not in sysfs), though it is still another GPU's when the
// presenting one does not fit.
Where LocateVa(const Decoder& decoder, const std::vector<Decoder>& all, const Topology& topology,
               const Gpu* presenting) {
  Where where;
  std::string node, codec;
  if (!ParseVa(decoder.factory, &node, &codec)) return where;
  if (!node.empty()) {
    where.node = node;
    if (presenting) where.placement = node == presenting->node ? Placement::kPresenting : Placement::kOther;
    return where;
  }
  std::vector<std::string> named;
  for (const Decoder& other : all) {
    std::string other_node, other_codec;
    if (other.plugin == kVaPlugin && ParseVa(other.factory, &other_node, &other_codec) && !other_node.empty() &&
        other_codec == codec) {
      named.push_back(other_node);
    }
  }
  std::sort(named.begin(), named.end());
  const bool sorts_first = VaDefaultSortsFirst(topology);
  std::vector<const Gpu*> possible;
  for (const Gpu& gpu : topology.gpus) {
    if (!VaOpens(gpu, topology) || std::find(named.begin(), named.end(), gpu.node) != named.end()) continue;
    if (sorts_first && !named.empty() && gpu.node >= named.front()) continue;
    possible.push_back(&gpu);
  }
  if (possible.size() == 1) where.node = possible.front()->node;
  // No GPU fits: the registry was scanned on other GPUs, and says nothing.
  if (!presenting || possible.empty()) return where;
  if (!where.node.empty()) {
    where.placement = where.node == presenting->node ? Placement::kPresenting : Placement::kOther;
  } else if (std::find(possible.begin(), possible.end(), presenting) == possible.end()) {
    where.placement = Placement::kOther;
  }
  return where;
}

// NVDEC runs on NVIDIA's driver alone. Which NVIDIA GPU a factory is on
// takes CUDA's device numbering, and asking CUDA wakes the GPU: with one
// NVIDIA GPU it is that one, with more it is unknown, and it is never a GPU
// another driver runs.
Where LocateNvcodec(const Topology& topology, const Gpu* presenting) {
  Where where;
  const Gpu* nvidia = nullptr;
  size_t count = 0;
  for (const Gpu& gpu : topology.gpus) {
    if (gpu.driver != kNvidiaDriver) continue;
    nvidia = &gpu;
    ++count;
  }
  if (count == 1) where.node = nvidia->node;
  if (!presenting) return where;
  if (presenting->driver != kNvidiaDriver) {
    where.placement = Placement::kOther;
  } else if (count == 1) {
    where.placement = Placement::kPresenting;
  }
  return where;
}

}  // namespace

std::vector<Gpu> ReadGpus(const std::string& sysfs, const std::string& dev) {
  std::vector<Gpu> gpus;
  const std::string drm = sysfs + "/class/drm/";
  for (const std::string& node : List(drm, kRenderPrefix)) {
    Gpu gpu;
    gpu.node = node;
    gpu.driver = Basename(ReadLink(drm + node + "/device/driver"));
    gpu.openable = access((dev + "/dri/" + node).c_str(), R_OK | W_OK) == 0;
    gpus.push_back(gpu);
  }
  return gpus;
}

std::string RenderNode(const std::string& device, const std::string& sysfs) {
  std::string path = device;
  // A link such as /dev/dri/by-path/pci-0000:01:00.0-render names its node.
  if (char* resolved = realpath(device.c_str(), nullptr)) {
    path = resolved;
    free(resolved);
  }
  const std::string name = Basename(path);
  if (StartsWith(name, kRenderPrefix) && name.size() > strlen(kRenderPrefix)) return name;
  if (!StartsWith(name, kCardPrefix) || name.size() <= strlen(kCardPrefix)) return "";
  // A primary node's GPU lists its render node beside it.
  const std::vector<std::string> nodes = List(sysfs + "/class/drm/" + name + "/device/drm", kRenderPrefix);
  return nodes.empty() ? "" : nodes.front();
}

Where Locate(const Decoder& decoder, const std::vector<Decoder>& all, const Topology& topology) {
  // A software decoder runs on the CPU, and any other plugin's factory names
  // no GPU here.
  if (!decoder.hardware) return Where{};
  const Gpu* presenting = Find(topology.gpus, topology.presenting);
  if (decoder.plugin == kVaPlugin) return LocateVa(decoder, all, topology, presenting);
  if (decoder.plugin == kNvcodecPlugin) return LocateNvcodec(topology, presenting);
  return Where{};
}

std::vector<size_t> Order(const std::vector<Decoder>& decoders, const std::vector<Decoder>& all,
                          const Topology& topology, std::vector<Where>* where) {
  std::vector<Where> found;
  found.reserve(decoders.size());
  for (const Decoder& decoder : decoders) found.push_back(Locate(decoder, all, topology));
  std::vector<size_t> order(decoders.size());
  for (size_t i = 0; i < order.size(); ++i) order[i] = i;
  std::stable_sort(order.begin(), order.end(), [&](size_t a, size_t b) {
    const Decoder& x = decoders[a];
    const Decoder& y = decoders[b];
    if (x.hardware != y.hardware) return x.hardware;
    if (x.hardware && found[a].placement != found[b].placement) return found[a].placement < found[b].placement;
    return x.rank > y.rank;
  });
  if (where) where->swap(found);
  return order;
}

std::string Describe(const Where& where) {
  const bool other = where.placement == Placement::kOther;
  if (where.node.empty()) return other ? " on another GPU" : "";
  return " on " + where.node + (other ? ", another GPU" : "");
}

}  // namespace pb_video
