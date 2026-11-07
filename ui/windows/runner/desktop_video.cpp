#include "desktop_video.h"

#include <d3d11.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <wmcodecdsp.h>
#include <codecapi.h>
#include <wincodec.h>
#include <wrl/client.h>
#include <flutter/method_channel.h>
#include <flutter/plugin_registrar_windows.h>
#include <flutter/standard_method_codec.h>
#include <flutter/texture_registrar.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <map>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <vector>

namespace {
using Microsoft::WRL::ComPtr;
using Value = flutter::EncodableValue;
using Map = flutter::EncodableMap;
using Reply = std::unique_ptr<flutter::MethodResult<Value>>;
using Clock = std::chrono::steady_clock;
constexpr UINT kWake = WM_APP + 0x413;
constexpr uint32_t kMaxBytes = 4 * 1024 * 1024;
constexpr int64_t kSnapshotWidth = 320;
constexpr float kSnapshotQuality = 0.8f;
constexpr auto kSnapshotDeadline = std::chrono::milliseconds(300);

// The video C ABI of pb_mapper_ffi.dll (desktop_video.rs). The runner does
// not link the DLL: it resolves these from the copy Dart already loaded.
struct PbDesktopVideoMetaV1 {
  uint64_t generation;
  uint64_t frame_id;
  uint64_t seq;
  uint64_t pts_us;
  uint64_t age_us;
  uint32_t width;
  uint32_t height;
  uint32_t len;
  uint32_t flags;
};
static_assert(sizeof(PbDesktopVideoMetaV1) == 56, "PbDesktopVideoMetaV1 is 56 bytes");
static_assert(offsetof(PbDesktopVideoMetaV1, generation) == 0 && offsetof(PbDesktopVideoMetaV1, frame_id) == 8 &&
                  offsetof(PbDesktopVideoMetaV1, seq) == 16 && offsetof(PbDesktopVideoMetaV1, pts_us) == 24 &&
                  offsetof(PbDesktopVideoMetaV1, age_us) == 32 && offsetof(PbDesktopVideoMetaV1, width) == 40 &&
                  offsetof(PbDesktopVideoMetaV1, height) == 44 && offsetof(PbDesktopVideoMetaV1, len) == 48 &&
                  offsetof(PbDesktopVideoMetaV1, flags) == 52,
              "PbDesktopVideoMetaV1 matches desktop_video.rs");
constexpr int32_t kVideoUnit = 1;
constexpr int32_t kVideoTimeout = 0;
constexpr int32_t kVideoNeedCapacity = -2;
constexpr int32_t kVideoPaused = -3;
constexpr uint32_t kFlagKeyframe = 1;
constexpr uint32_t kFlagDiscontinuity = 2;
constexpr uint32_t kReasonGap = 1;
constexpr uint32_t kPullTimeoutMs = 50;

struct VideoLibrary {
  HMODULE module = nullptr;
  int64_t (*attach)(uint64_t source) = nullptr;
  int32_t (*next)(uint64_t consumer, uint32_t timeout_ms, uint8_t* dst, uint32_t capacity, PbDesktopVideoMetaV1* meta) = nullptr;
  int32_t (*presented)(uint64_t consumer, uint64_t generation, uint64_t frame_id) = nullptr;
  int32_t (*keyframe)(uint64_t consumer, uint32_t reason) = nullptr;
  int32_t (*failed)(uint64_t consumer, const char* message_utf8) = nullptr;
  int32_t (*report)(uint64_t consumer, const char* decoder_utf8, uint32_t flags) = nullptr;
  void (*detach)(uint64_t consumer) = nullptr;
  uint64_t (*instance_token)() = nullptr;
  VideoLibrary() = default;
  VideoLibrary(const VideoLibrary&) = delete;
  VideoLibrary& operator=(const VideoLibrary&) = delete;
  // Released by the last viewer, so a late raster callback never calls into
  // an unpinned module.
  ~VideoLibrary() { if (module) FreeLibrary(module); }
};

template <typename Fn>
bool Resolve(HMODULE module, Fn& fn, const char* name) {
  fn = reinterpret_cast<Fn>(GetProcAddress(module, name));
  return fn != nullptr;
}

// GetModuleHandleExW finds the DLL Dart loaded and pins it. It can never load
// a second copy, which LoadLibrary could: Rust state would then split in two.
std::shared_ptr<const VideoLibrary> LoadVideoLibrary() {
  auto library = std::make_shared<VideoLibrary>();
  if (!GetModuleHandleExW(0, L"pb_mapper_ffi.dll", &library->module)) {
    throw std::runtime_error("pb_mapper_ffi.dll is not loaded");
  }
  const HMODULE module = library->module;
  if (!Resolve(module, library->attach, "pb_mapper_desktop_video_attach_v1") ||
      !Resolve(module, library->next, "pb_mapper_desktop_video_next_v1") ||
      !Resolve(module, library->presented, "pb_mapper_desktop_video_presented_v1") ||
      !Resolve(module, library->keyframe, "pb_mapper_desktop_video_keyframe_v1") ||
      !Resolve(module, library->failed, "pb_mapper_desktop_video_failed_v1") ||
      !Resolve(module, library->report, "pb_mapper_desktop_video_report_v1") ||
      !Resolve(module, library->detach, "pb_mapper_desktop_video_detach_v1") ||
      !Resolve(module, library->instance_token, "pb_mapper_desktop_instance_token_v1")) {
    throw std::runtime_error("pb_mapper_ffi.dll does not match this app");
  }
  return library;
}

// mfplat.dll is delay-loaded (CMakeLists.txt): Windows N/KN without the Media
// Feature Pack, and Windows Server without the feature, lack it, and there the
// first Media Foundation call would raise. Loaded from System32 and kept, the
// delay-load binds to this copy.
bool MediaFoundationInstalled() {
  static const bool installed =
      LoadLibraryExW(L"mfplat.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32) != nullptr;
  return installed;
}

// "Create H.264 decoder (0x80040154)": the HRESULT as it is searched for.
void Check(HRESULT hr, const char* operation) {
  if (SUCCEEDED(hr)) return;
  char code[16];
  snprintf(code, sizeof(code), "0x%08lX", static_cast<unsigned long>(hr));
  throw std::runtime_error(std::string(operation) + " (" + code + ")");
}
int64_t Number(const Map& map, const char* key) {
  const auto it = map.find(Value(key));
  if (it == map.end()) throw std::runtime_error("Missing video field");
  if (const auto* value = std::get_if<int64_t>(&it->second)) return *value;
  if (const auto* value = std::get_if<int32_t>(&it->second)) return *value;
  throw std::runtime_error("Invalid video field");
}
std::wstring Path(const Map& map, const char* key) {
  const auto it = map.find(Value(key));
  const auto* utf8 = it == map.end() ? nullptr : std::get_if<std::string>(&it->second);
  if (!utf8 || utf8->empty()) throw std::runtime_error("Missing snapshot path");
  const int size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, utf8->data(), static_cast<int>(utf8->size()), nullptr, 0);
  if (size <= 0) throw std::runtime_error("Invalid snapshot path");
  std::wstring path(static_cast<size_t>(size), L'\0');
  MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, utf8->data(), static_cast<int>(utf8->size()), path.data(), size);
  return path;
}

// Snapshot replies, completed on the platform thread: the decoder thread
// queues them and posts kWake, and DesktopVideo::HandleMessage drains them.
struct Replies {
  explicit Replies(HWND value) : window(value) {}
  std::mutex mutex;
  HWND window;
  bool posted = false;
  std::vector<std::pair<Reply, bool>> ready;
  void Send(Reply reply, bool ok) {
    std::lock_guard<std::mutex> lock(mutex);
    ready.emplace_back(std::move(reply), ok);
    if (!posted) { posted = true; PostMessage(window, kWake, 0, 0); }
  }
  void Drain() {
    std::vector<std::pair<Reply, bool>> done;
    { std::lock_guard<std::mutex> lock(mutex); done.swap(ready); posted = false; }
    for (auto& entry : done) entry.first->Success(Value(entry.second));
  }
};

struct SnapshotRequest {
  std::wstring path;
  uint32_t max_width = 0;
  Clock::time_point deadline;
  Reply reply;
};

// "Media Foundation DXVA (<adapter>)", cut to the 64 bytes the report takes
// without splitting a UTF-8 sequence.
std::string DecoderName(IDXGIAdapter* adapter) {
  std::string name = "Media Foundation DXVA";
  DXGI_ADAPTER_DESC desc{};
  if (adapter && SUCCEEDED(adapter->GetDesc(&desc))) {
    char utf8[256] = {};
    if (WideCharToMultiByte(CP_UTF8, 0, desc.Description, -1, utf8, static_cast<int>(sizeof(utf8)), nullptr, nullptr) > 1) {
      name += std::string(" (") + utf8 + ")";
    }
  }
  if (name.size() > 64) {
    size_t end = 63;
    while (end > 0 && (static_cast<unsigned char>(name[end]) & 0xC0) == 0x80) --end;
    name = name.substr(0, end) + ")";
  }
  return name;
}

// D23: `texture` box-filtered (WIC Fant) to at most max_width and encoded as
// JPEG q80. It is written beside the path, flushed, then moved over it, but
// not past the deadline: by then Dart has its answer and the old picture stays.
bool WriteSnapshot(ID3D11Texture2D* texture, const SnapshotRequest& request) {
  ComPtr<ID3D11Device> device;
  texture->GetDevice(&device);
  ComPtr<ID3D11DeviceContext> context;
  device->GetImmediateContext(&context);
  D3D11_TEXTURE2D_DESC desc{};
  texture->GetDesc(&desc);
  desc.Usage = D3D11_USAGE_STAGING;
  desc.BindFlags = 0;
  desc.MiscFlags = 0;
  desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  ComPtr<ID3D11Texture2D> staging;
  Check(device->CreateTexture2D(&desc, nullptr, &staging), "Allocate snapshot");
  context->CopyResource(staging.Get(), texture);
  ComPtr<IWICImagingFactory> factory;
  Check(CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&factory)), "Create imaging factory");
  D3D11_MAPPED_SUBRESOURCE mapped{};
  Check(context->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped), "Read snapshot");
  // The bitmap copies the pixels, so the staging texture unmaps at once.
  ComPtr<IWICBitmap> picture;
  const HRESULT copied = factory->CreateBitmapFromMemory(desc.Width, desc.Height, GUID_WICPixelFormat32bppBGR,
      mapped.RowPitch, mapped.RowPitch * desc.Height, static_cast<BYTE*>(mapped.pData), &picture);
  context->Unmap(staging.Get(), 0);
  Check(copied, "Copy snapshot");
  const UINT width = std::min<UINT>(request.max_width, desc.Width);
  const UINT height = std::max<UINT>(1, static_cast<UINT>((static_cast<uint64_t>(desc.Height) * width + desc.Width / 2) / desc.Width));
  ComPtr<IWICBitmapScaler> scaler;
  Check(factory->CreateBitmapScaler(&scaler), "Create snapshot scaler");
  Check(scaler->Initialize(picture.Get(), width, height, WICBitmapInterpolationModeFant), "Scale snapshot");
  ComPtr<IWICFormatConverter> converter;
  Check(factory->CreateFormatConverter(&converter), "Create snapshot converter");
  Check(converter->Initialize(scaler.Get(), GUID_WICPixelFormat24bppBGR, WICBitmapDitherTypeNone, nullptr, 0.0,
      WICBitmapPaletteTypeCustom), "Convert snapshot");
  ComPtr<IStream> stream;
  Check(CreateStreamOnHGlobal(nullptr, TRUE, &stream), "Allocate JPEG");
  ComPtr<IWICBitmapEncoder> encoder;
  Check(factory->CreateEncoder(GUID_ContainerFormatJpeg, nullptr, &encoder), "Create JPEG encoder");
  Check(encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache), "Start JPEG");
  ComPtr<IWICBitmapFrameEncode> frame;
  ComPtr<IPropertyBag2> options;
  Check(encoder->CreateNewFrame(&frame, &options), "Create JPEG frame");
  PROPBAG2 option{};
  option.pstrName = const_cast<LPOLESTR>(L"ImageQuality");
  VARIANT quality;
  VariantInit(&quality);
  quality.vt = VT_R4;
  quality.fltVal = kSnapshotQuality;
  Check(options->Write(1, &option, &quality), "Set JPEG quality");
  Check(frame->Initialize(options.Get()), "Start JPEG frame");
  Check(frame->SetSize(width, height), "Size JPEG");
  WICPixelFormatGUID format = GUID_WICPixelFormat24bppBGR;
  Check(frame->SetPixelFormat(&format), "Set JPEG format");
  Check(frame->WriteSource(converter.Get(), nullptr), "Encode JPEG");
  Check(frame->Commit(), "Finish JPEG frame");
  Check(encoder->Commit(), "Finish JPEG");
  HGLOBAL memory = nullptr;
  Check(GetHGlobalFromStream(stream.Get(), &memory), "Get JPEG");
  STATSTG stat{};
  Check(stream->Stat(&stat, STATFLAG_NONAME), "Measure JPEG");
  const DWORD size = static_cast<DWORD>(stat.cbSize.QuadPart);

  std::error_code ignored;
  std::filesystem::create_directories(std::filesystem::path(request.path).parent_path(), ignored);
  const std::wstring temporary = request.path + L".tmp";
  const HANDLE file = CreateFileW(temporary.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
  if (file == INVALID_HANDLE_VALUE) throw std::runtime_error("Create snapshot file");
  const void* bytes = GlobalLock(memory);
  DWORD written = 0;
  const bool flushed = bytes && WriteFile(file, bytes, size, &written, nullptr) && written == size && FlushFileBuffers(file);
  if (bytes) GlobalUnlock(memory);
  CloseHandle(file);
  if (!flushed || Clock::now() > request.deadline ||
      !MoveFileExW(temporary.c_str(), request.path.c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
    DeleteFileW(temporary.c_str());
    return false;
  }
  return true;
}

struct Frame {
  int64_t generation = 0, frame = 0;
  uint32_t width = 0, height = 0;
  uint64_t revision = 0;
  bool key = false;
};

struct Surface {
  ComPtr<ID3D11Texture2D> texture;
  FlutterDesktopGpuSurfaceDescriptor descriptor{};
  Frame frame;
  Surface(ComPtr<ID3D11Texture2D> value, const Frame& decoded) : texture(std::move(value)), frame(decoded) {
    descriptor.struct_size = sizeof(descriptor);
    ComPtr<IDXGIResource> resource;
    Check(texture.As(&resource), "Get shared Flutter texture");
    HANDLE shared = nullptr;
    Check(resource->GetSharedHandle(&shared), "Share Flutter texture");
    descriptor.handle = shared;
    descriptor.width = descriptor.visible_width = decoded.width;
    descriptor.height = descriptor.visible_height = decoded.height;
    descriptor.format = kFlutterDesktopPixelFormatBGRA8888;
  }
};
struct SurfaceLease {
  std::shared_ptr<Surface> surface;
  FlutterDesktopGpuSurfaceDescriptor descriptor;
};

// One texture. While a source is attached, a decoder thread pulls its access
// units straight from Rust; nothing of the video crosses Dart.
class Viewer : public std::enable_shared_from_this<Viewer> {
 public:
  Viewer(flutter::TextureRegistrar* registrar, std::shared_ptr<const VideoLibrary> video, std::shared_ptr<Replies> replies,
         ComPtr<IDXGIAdapter> adapter)
      : registrar_(registrar), video_(std::move(video)), replies_(std::move(replies)), adapter_(std::move(adapter)),
        decoder_name_(DecoderName(adapter_.Get())) {}
  void Start() {
    std::weak_ptr<Viewer> weak = shared_from_this();
    texture_ = std::make_unique<flutter::TextureVariant>(flutter::GpuSurfaceTexture(
        kFlutterDesktopGpuSurfaceTypeDxgiSharedHandle,
        [weak](size_t, size_t) -> const FlutterDesktopGpuSurfaceDescriptor* {
          if (auto self = weak.lock()) return self->AcquireSurface();
          return nullptr;
        }));
    id_ = registrar_->RegisterTexture(texture_.get());
    if (id_ < 0) throw std::runtime_error("Cannot register video texture");
  }
  ~Viewer() { Detach(); }
  int64_t id() const { return id_; }
  // Platform thread. The texture and its last picture stay through a
  // reconnect; the new session's first IDR replaces them.
  void Attach(uint64_t source) {
    Detach();
    generation_ = -1;
    last_ = 0;
    awaiting_key_ = true;
    paused_ = false;
    const int64_t consumer = video_->attach(source);
    if (consumer <= 0) throw std::runtime_error("Video source unavailable (" + std::to_string(consumer) + ")");
    consumer_ = static_cast<uint64_t>(consumer);
    { std::lock_guard<std::mutex> lock(snapshot_mutex_); running_ = true; }
    worker_ = std::thread([this, consumer] { Run(static_cast<uint64_t>(consumer)); });
  }
  // Platform thread. Detaching wakes a blocked pull, so the join is prompt.
  void Detach() {
    stopping_ = true;
    if (const uint64_t consumer = consumer_.exchange(0)) video_->detach(consumer);
    if (worker_.joinable()) worker_.join();
    stopping_ = false;
  }
  void Close() {
    Detach();
    auto keep_alive = shared_from_this();
    registrar_->UnregisterTexture(id_, [keep_alive] {});
  }
  // Platform thread. A running decoder thread takes the request between two
  // pulls, and steady video never makes it wait for a timeout. With no
  // thread, this one writes the picture. One request at a time.
  void Snapshot(std::wstring path, uint32_t max_width, Reply reply) {
    SnapshotRequest request{std::move(path), max_width, Clock::now() + kSnapshotDeadline, std::move(reply)};
    bool busy = false;
    {
      std::lock_guard<std::mutex> lock(snapshot_mutex_);
      if (running_) {
        busy = snapshot_.has_value();
        if (!busy) {
          snapshot_ = std::move(request);
          snapshot_requested_ = true;
          return;
        }
      }
    }
    request.reply->Success(Value(!busy && Capture(request)));
  }
  Map Stats() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return {{Value("submitted"), Value(static_cast<int64_t>(submitted_.load()))},
            {Value("decoded"), Value(static_cast<int64_t>(decoded_.load()))},
            {Value("presented"), Value(static_cast<int64_t>(presented_.load()))},
            {Value("dropped"), Value(static_cast<int64_t>(dropped_.load()))},
            {Value("configurations"), Value(static_cast<int64_t>(configurations_.load()))},
            {Value("recoveries"), Value(static_cast<int64_t>(recoveries_.load()))},
            {Value("lastRecovery"), Value(last_recovery_)},
            {Value("decoder"), Value(decoder_name_)}};
  }

 private:
  bool Current(uint64_t revision) const { return revision == revision_.load(); }
  // Raster thread. A picture of an older revision stays on screen until a
  // newer one replaces it, but only a current one earns a receipt.
  const FlutterDesktopGpuSurfaceDescriptor* AcquireSurface() {
    std::shared_ptr<Surface> surface;
    bool acknowledge = false;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (!latest_) return nullptr;
      surface = latest_;
      if (Current(surface->frame.revision) &&
          (consumed_revision_ != surface->frame.revision || consumed_frame_ != surface->frame.frame)) {
        consumed_revision_ = surface->frame.revision;
        consumed_frame_ = surface->frame.frame;
        acknowledge = true;
      }
    }
    // The engine opens the D3D resource asynchronously. Retain the descriptor and
    // its COM texture until its explicit release callback, independent of reset.
    auto lease = std::make_shared<SurfaceLease>(SurfaceLease{surface, surface->descriptor});
    auto* retained = new std::shared_ptr<SurfaceLease>(lease);
    lease->descriptor.release_context = retained;
    lease->descriptor.release_callback = [](void* context) {
      delete static_cast<std::shared_ptr<SurfaceLease>*>(context);
    };
    // Flutter also reads descriptor dimensions immediately after invoking the
    // release callback. Keep the last descriptor until its next acquisition.
    { std::lock_guard<std::mutex> lock(mutex_); borrowed_ = lease; }
    if (acknowledge) {
      ++presented_;
      // Acquired by the compositor: the receipt Rust validates and forwards
      // for input authorisation. Non-blocking.
      if (const uint64_t consumer = consumer_.load()) {
        video_->presented(consumer, static_cast<uint64_t>(surface->frame.generation), static_cast<uint64_t>(surface->frame.frame));
      }
    }
    return &lease->descriptor;
  }
  // The picture the compositor acquired last, which is the one on screen.
  bool Capture(const SnapshotRequest& request) {
    std::shared_ptr<Surface> surface;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (borrowed_) surface = borrowed_->surface;
    }
    if (!surface) return false;
    try {
      return WriteSnapshot(surface->texture.Get(), request);
    } catch (const std::exception& e) {
      OutputDebugStringA((std::string("Desktop video snapshot failed: ") + e.what() + "\n").c_str());
      return false;
    }
  }
  // Decoder thread, after a pull returned.
  void ServiceSnapshot() {
    std::optional<SnapshotRequest> request;
    {
      std::lock_guard<std::mutex> lock(snapshot_mutex_);
      request.swap(snapshot_);
    }
    if (!request) return;
    const bool ok = Capture(*request);
    replies_->Send(std::move(request->reply), ok);
  }
  // Decoder thread. The next keyframe recreates the decoder in a new revision.
  void Fail(uint64_t consumer, const std::string& reason) {
    ++revision_;
    awaiting_key_ = true;
    ++recoveries_;
    { std::lock_guard<std::mutex> lock(mutex_); last_recovery_ = reason; }
    OutputDebugStringA(("Native H.264 decoding failed: " + reason + "\n").c_str());
    // Rust logs the reason, counts it toward giving up and rate-limits the
    // request. A paused source asks for nothing: its resume already starts
    // with an IDR.
    if (!paused_) video_->failed(consumer, reason.c_str());
  }

  struct Decoder {
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    ComPtr<ID3D11VideoDevice> video;
    ComPtr<ID3D11VideoContext> video_context;
    ComPtr<ID3D11VideoProcessorEnumerator> enumerator;
    ComPtr<ID3D11VideoProcessor> processor;
    ComPtr<IMFDXGIDeviceManager> manager;
    ComPtr<IMFTransform> transform;
    std::map<LONGLONG, Frame> pending;
    uint64_t revision;
    uint32_t width, height;
    UINT32 coded_width = 0, coded_height = 0;
    RECT source_rect{};
    LONGLONG next_pts = 0;
    Decoder(const Frame& frame, IDXGIAdapter* adapter) : revision(frame.revision), width(frame.width), height(frame.height) {
      UINT flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
      const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0};
      Check(D3D11CreateDevice(adapter, adapter ? D3D_DRIVER_TYPE_UNKNOWN : D3D_DRIVER_TYPE_HARDWARE,
            nullptr, flags, levels, 2, D3D11_SDK_VERSION, &device, nullptr, &context), "Create video device");
      ComPtr<ID3D10Multithread> multithread;
      if (SUCCEEDED(context.As(&multithread))) multithread->SetMultithreadProtected(TRUE);
      Check(device.As(&video), "Get video device");
      Check(context.As(&video_context), "Get video context");
      UINT token = 0;
      Check(MFCreateDXGIDeviceManager(&token, &manager), "Create decoder device manager");
      Check(manager->ResetDevice(device.Get(), token), "Assign decoder device");
      Check(CoCreateInstance(CLSID_CMSH264DecoderMFT, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&transform)), "Create H.264 decoder");
      ComPtr<IMFAttributes> attrs;
      if (SUCCEEDED(transform->GetAttributes(&attrs))) {
        attrs->SetUINT32(MF_LOW_LATENCY, TRUE);
        attrs->SetUINT32(CODECAPI_AVLowLatencyMode, TRUE);
        attrs->SetUINT32(CODECAPI_AVDecVideoMaxCodedWidth, (width + 15) & ~15u);
        attrs->SetUINT32(CODECAPI_AVDecVideoMaxCodedHeight, (height + 15) & ~15u);
      }
      Check(transform->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, reinterpret_cast<ULONG_PTR>(manager.Get())), "Set decoder GPU manager");
      ComPtr<IMFMediaType> input;
      Check(MFCreateMediaType(&input), "Create H.264 type");
      Check(input->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video), "Set video type");
      Check(input->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264), "Set H.264 subtype");
      Check(MFSetAttributeSize(input.Get(), MF_MT_FRAME_SIZE, width, height), "Set video size");
      Check(input->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive), "Set progressive type");
      Check(transform->SetInputType(0, input.Get(), 0), "Configure H.264 decoder");
      SetOutput();
      Check(transform->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "Begin video decoding");
      Check(transform->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "Start video decoding");
    }
    void ConfigureConverter(UINT32 input_width, UINT32 input_height, const RECT& aperture) {
      processor.Reset();
      enumerator.Reset();
      D3D11_VIDEO_PROCESSOR_CONTENT_DESC desc{};
      desc.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
      desc.InputWidth = input_width;
      desc.InputHeight = input_height;
      desc.OutputWidth = width;
      desc.OutputHeight = height;
      desc.InputFrameRate = desc.OutputFrameRate = {60, 1};
      desc.Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;
      Check(video->CreateVideoProcessorEnumerator(&desc, &enumerator), "Create GPU converter");
      Check(video->CreateVideoProcessor(enumerator.Get(), 0, &processor), "Create GPU processor");
      const RECT rect{0, 0, static_cast<LONG>(width), static_cast<LONG>(height)};
      video_context->VideoProcessorSetStreamSourceRect(processor.Get(), 0, TRUE, &aperture);
      video_context->VideoProcessorSetStreamDestRect(processor.Get(), 0, TRUE, &rect);
      video_context->VideoProcessorSetOutputTargetRect(processor.Get(), TRUE, &rect);
      video_context->VideoProcessorSetStreamFrameFormat(processor.Get(), 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
      // The picture is the host's pixels: no driver denoise, sharpening or colour correction.
      video_context->VideoProcessorSetStreamAutoProcessingMode(processor.Get(), 0, FALSE);
      D3D11_VIDEO_PROCESSOR_COLOR_SPACE color{};
      color.YCbCr_Matrix = 1;  // BT.709, matching the desktop encoder.
      color.Nominal_Range = D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235;
      video_context->VideoProcessorSetStreamColorSpace(processor.Get(), 0, &color);
      color.RGB_Range = 0;
      video_context->VideoProcessorSetOutputColorSpace(processor.Get(), &color);
      coded_width = input_width;
      coded_height = input_height;
      source_rect = aperture;
    }
    ~Decoder() {
      if (transform) {
        transform->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        transform->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
      }
    }
    // A discontinuity keeps the decoder: drop what it holds of the old
    // revision and let the IDR that follows reset its references.
    void Flush(uint64_t next_revision) {
      Check(transform->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0), "Reset video decoder");
      Check(transform->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "Restart video decoder");
      pending.clear();
      revision = next_revision;
    }
    void SetOutput() {
      for (DWORD i = 0; ; ++i) {
        ComPtr<IMFMediaType> type;
        Check(transform->GetOutputAvailableType(0, i, &type), "Enumerate decoded format");
        GUID subtype{};
        if (SUCCEEDED(type->GetGUID(MF_MT_SUBTYPE, &subtype)) && subtype == MFVideoFormat_NV12) {
          UINT32 actual_width = 0, actual_height = 0;
          Check(MFGetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, &actual_width, &actual_height), "Get decoded dimensions");
          // H.264 surfaces are macroblock aligned (1080p commonly uses 1088
          // rows). Only an explicit, integral display aperture may crop those
          // rows; the visible rectangle must still equal the authorized frame.
          MFVideoArea area{};
          area.Area = {static_cast<LONG>(actual_width), static_cast<LONG>(actual_height)};
          UINT32 blob_size = 0;
          HRESULT aperture_hr = type->GetBlob(MF_MT_MINIMUM_DISPLAY_APERTURE,
              reinterpret_cast<UINT8*>(&area), sizeof(area), &blob_size);
          if (aperture_hr == MF_E_ATTRIBUTENOTFOUND) {
            aperture_hr = type->GetBlob(MF_MT_GEOMETRIC_APERTURE,
                reinterpret_cast<UINT8*>(&area), sizeof(area), &blob_size);
          }
          if (aperture_hr != MF_E_ATTRIBUTENOTFOUND) {
            Check(aperture_hr, "Read decoded display aperture");
            if (blob_size != sizeof(area)) throw std::runtime_error("Invalid decoded display aperture");
          }
          const int64_t right = static_cast<int64_t>(area.OffsetX.value) + area.Area.cx;
          const int64_t bottom = static_cast<int64_t>(area.OffsetY.value) + area.Area.cy;
          if (actual_width > ((width + 15) & ~15u) || actual_height > ((height + 15) & ~15u) ||
              area.OffsetX.fract != 0 || area.OffsetY.fract != 0 ||
              area.OffsetX.value < 0 || area.OffsetY.value < 0 ||
              area.Area.cx != static_cast<LONG>(width) || area.Area.cy != static_cast<LONG>(height) ||
              right > actual_width || bottom > actual_height) {
            throw std::runtime_error("Decoded display aperture does not match the authorized frame");
          }
          const RECT aperture{area.OffsetX.value, area.OffsetY.value,
              static_cast<LONG>(right), static_cast<LONG>(bottom)};
          Check(transform->SetOutputType(0, type.Get(), 0), "Set decoded NV12 output");
          if (!processor || coded_width != actual_width || coded_height != actual_height ||
              !EqualRect(&source_rect, &aperture)) {
            ConfigureConverter(actual_width, actual_height, aperture);
          }
          return;
        }
      }
    }
    void Feed(const Frame& frame, const uint8_t* data, uint32_t len) {
      if (pending.size() >= 16) throw std::runtime_error("Native decoder backlog");
      ComPtr<IMFMediaBuffer> buffer;
      Check(MFCreateMemoryBuffer(len, &buffer), "Allocate compressed video");
      BYTE* target = nullptr;
      Check(buffer->Lock(&target, nullptr, nullptr), "Lock video buffer");
      memcpy(target, data, len);
      buffer->Unlock();
      Check(buffer->SetCurrentLength(len), "Set compressed video size");
      ComPtr<IMFSample> sample;
      Check(MFCreateSample(&sample), "Create compressed sample");
      Check(sample->AddBuffer(buffer.Get()), "Assign video buffer");
      next_pts += 166667;
      sample->SetSampleTime(next_pts);
      sample->SetSampleDuration(166667);
      if (frame.key) sample->SetUINT32(MFSampleExtension_CleanPoint, TRUE);
      Check(transform->ProcessInput(0, sample.Get(), 0), "Decode H.264 input");
      pending.emplace(next_pts, frame);
    }
    std::shared_ptr<Surface> Take() {
      MFT_OUTPUT_DATA_BUFFER output{};
      DWORD flags = 0;
      HRESULT hr = transform->ProcessOutput(0, 1, &output, &flags);
      ComPtr<IMFSample> sample;
      sample.Attach(output.pSample);
      if (output.pEvents) output.pEvents->Release();
      if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) return nullptr;
      if (hr == MF_E_TRANSFORM_STREAM_CHANGE) { SetOutput(); return Take(); }
      Check(hr, "Decode H.264 output");
      if (!sample) throw std::runtime_error("Decoder returned an empty sample");
      LONGLONG timestamp = 0;
      Check(sample->GetSampleTime(&timestamp), "Get decoded timestamp");
      auto found = pending.find(timestamp);
      if (found == pending.end()) throw std::runtime_error("Unknown decoded frame");
      const Frame frame = found->second;
      pending.erase(pending.begin(), std::next(found));
      ComPtr<IMFMediaBuffer> buffer;
      Check(sample->GetBufferByIndex(0, &buffer), "Get decoded surface");
      ComPtr<IMFDXGIBuffer> dxgi;
      Check(buffer.As(&dxgi), "Get GPU decoded frame");
      ComPtr<ID3D11Texture2D> source;
      Check(dxgi->GetResource(IID_PPV_ARGS(&source)), "Get decoded texture");
      UINT subresource = 0;
      Check(dxgi->GetSubresourceIndex(&subresource), "Get decoded plane");
      D3D11_TEXTURE2D_DESC source_desc{};
      source->GetDesc(&source_desc);
      D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC in_desc{};
      in_desc.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
      in_desc.Texture2D.MipSlice = subresource % source_desc.MipLevels;
      in_desc.Texture2D.ArraySlice = subresource / source_desc.MipLevels;
      ComPtr<ID3D11VideoProcessorInputView> in_view;
      Check(video->CreateVideoProcessorInputView(source.Get(), enumerator.Get(), &in_desc, &in_view), "Open decoded GPU frame");
      D3D11_TEXTURE2D_DESC desc{};
      desc.Width = width; desc.Height = height; desc.MipLevels = 1; desc.ArraySize = 1;
      desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
      desc.SampleDesc.Count = 1;
      desc.Usage = D3D11_USAGE_DEFAULT;
      desc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
      desc.MiscFlags = D3D11_RESOURCE_MISC_SHARED;
      ComPtr<ID3D11Texture2D> target;
      Check(device->CreateTexture2D(&desc, nullptr, &target), "Allocate Flutter GPU frame");
      D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC out_desc{};
      out_desc.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
      ComPtr<ID3D11VideoProcessorOutputView> out_view;
      Check(video->CreateVideoProcessorOutputView(target.Get(), enumerator.Get(), &out_desc, &out_view), "Open Flutter GPU frame");
      D3D11_VIDEO_PROCESSOR_STREAM stream{};
      stream.Enable = TRUE;
      stream.pInputSurface = in_view.Get();
      Check(video_context->VideoProcessorBlt(processor.Get(), out_view.Get(), 0, 1, &stream), "Convert decoded GPU frame");
      context->Flush();
      return std::make_shared<Surface>(std::move(target), frame);
    }
  };

  // Decoder thread: what Submit decided on the platform thread before. A new
  // generation or a discontinuity starts a revision; older generations,
  // duplicates and anything before the next keyframe are dropped.
  void Accept(uint64_t consumer, std::unique_ptr<Decoder>& decoder, const uint8_t* data, const PbDesktopVideoMetaV1& meta) {
    Frame frame;
    frame.generation = static_cast<int64_t>(meta.generation);
    frame.frame = static_cast<int64_t>(meta.frame_id);
    frame.width = meta.width;
    frame.height = meta.height;
    frame.key = (meta.flags & kFlagKeyframe) != 0;
    if (frame.generation < generation_) { ++dropped_; return; }
    if (frame.generation != generation_ || (meta.flags & kFlagDiscontinuity) != 0) {
      ++revision_;
      generation_ = frame.generation;
      last_ = 0;
      awaiting_key_ = true;
    }
    if (frame.frame <= last_) { ++dropped_; return; }
    last_ = frame.frame;
    if (awaiting_key_ && !frame.key) {
      ++dropped_;
      video_->keyframe(consumer, kReasonGap);
      return;
    }
    awaiting_key_ = false;
    frame.revision = revision_;
    ++submitted_;
    try {
      Check(mf_, "Initialize native media decoding");
      // Only a size change or a failure (which released it) rebuilds the decoder.
      if (!decoder || decoder->width != frame.width || decoder->height != frame.height) {
        if (!frame.key) throw std::runtime_error("A fresh key frame is required");
        decoder.reset();
        decoder = std::make_unique<Decoder>(frame, adapter_.Get());
        ++configurations_;
        video_->report(consumer, decoder_name_.c_str(), 1);
      } else if (decoder->revision != frame.revision) {
        decoder->Flush(frame.revision);
      }
      decoder->Feed(frame, data, meta.len);
      for (int i = 0; i < 16; ++i) {
        auto surface = decoder->Take();
        if (!surface) break;
        ++decoded_;
        {
          std::lock_guard<std::mutex> lock(mutex_);
          if (!Current(surface->frame.revision)) continue;
          latest_ = std::move(surface);
        }
        registrar_->MarkTextureFrameAvailable(id_);
      }
    } catch (const std::exception& e) {
      decoder.reset();
      Fail(consumer, e.what());
    }
  }

  // Decoder thread, one per attached source. `next` is the only blocking call.
  void Run(uint64_t consumer) {
    const HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    mf_ = FAILED(com) ? com
          : MediaFoundationInstalled() ? MFStartup(MF_VERSION, MFSTARTUP_LITE)
                                       : HRESULT_FROM_WIN32(ERROR_MOD_NOT_FOUND);
    std::unique_ptr<Decoder> decoder;
    std::vector<uint8_t> buf(1 << 20);
    PbDesktopVideoMetaV1 meta{};
    while (!stopping_) {
      const int32_t r = video_->next(consumer, kPullTimeoutMs, buf.data(), static_cast<uint32_t>(buf.size()), &meta);
      if (r == kVideoUnit) {
        paused_ = false;
        Accept(consumer, decoder, buf.data(), meta);
      } else if (r == kVideoNeedCapacity) {
        // The unit stays queued in Rust and arrives on the next call.
        if (meta.len > kMaxBytes) break;
        buf.resize(meta.len);
      } else if (r == kVideoPaused) {
        paused_ = true;
      } else if (r != kVideoTimeout) {
        break;  // Closed, detached or invalid: this consumer is done.
      }
      // After every return, a unit included: steady video never times out.
      if (snapshot_requested_.exchange(false)) ServiceSnapshot();
    }
    // A request that raced the exit is served here; later ones by the
    // platform thread.
    {
      std::lock_guard<std::mutex> lock(snapshot_mutex_);
      running_ = false;
    }
    snapshot_requested_ = false;
    ServiceSnapshot();
    video_->detach(consumer);
    decoder.reset();
    if (SUCCEEDED(mf_)) MFShutdown();
    if (SUCCEEDED(com)) CoUninitialize();
  }

  flutter::TextureRegistrar* registrar_;
  std::shared_ptr<const VideoLibrary> video_;
  std::shared_ptr<Replies> replies_;
  ComPtr<IDXGIAdapter> adapter_;
  const std::string decoder_name_;
  std::unique_ptr<flutter::TextureVariant> texture_;
  int64_t id_ = -1;
  std::atomic<uint64_t> consumer_{0};
  std::atomic<bool> stopping_{false};
  std::atomic<uint64_t> revision_{1};
  std::atomic<uint64_t> submitted_{0}, decoded_{0}, presented_{0}, dropped_{0}, configurations_{0}, recoveries_{0};
  mutable std::mutex mutex_;
  // Guarded by mutex_.
  std::shared_ptr<Surface> latest_;
  std::shared_ptr<SurfaceLease> borrowed_;
  int64_t consumed_frame_ = 0;
  uint64_t consumed_revision_ = 0;
  std::string last_recovery_;
  // Set with snapshot_ for the decoder thread, which checks it after each pull.
  std::atomic<bool> snapshot_requested_{false};
  std::mutex snapshot_mutex_;
  // Guarded by snapshot_mutex_: the pending request, and whether a decoder
  // thread will take it.
  std::optional<SnapshotRequest> snapshot_;
  bool running_ = false;
  // The decoder thread's own; reset by Attach while no thread runs.
  int64_t generation_ = -1, last_ = 0;
  bool awaiting_key_ = true, paused_ = false;
  HRESULT mf_ = E_FAIL;
  std::thread worker_;
};
}  // namespace

struct DesktopVideo::Impl {
  flutter::FlutterEngine* engine;
  flutter::PluginRegistrarWindows* registrar;
  std::unique_ptr<flutter::MethodChannel<Value>> channel;
  // Resolved by the first create: Dart has loaded the DLL by then.
  std::shared_ptr<const VideoLibrary> video;
  std::shared_ptr<Replies> replies;
  std::map<int64_t, std::shared_ptr<Viewer>> viewers;
  Impl(flutter::FlutterEngine* value, HWND window) : engine(value), replies(std::make_shared<Replies>(window)) {
    registrar = flutter::PluginRegistrarManager::GetInstance()->GetRegistrar<flutter::PluginRegistrarWindows>(engine->GetRegistrarForPlugin("PbMapperDesktopVideo"));
    channel = std::make_unique<flutter::MethodChannel<Value>>(engine->messenger(), "pb_mapper/desktop_video", &flutter::StandardMethodCodec::GetInstance());
    channel->SetMethodCallHandler([this](const auto& call, auto result) {
      try {
        const auto* args = call.arguments() ? std::get_if<Map>(call.arguments()) : nullptr;
        if (!args) throw std::runtime_error("Missing desktop video arguments");
        if (call.method_name() == "create") {
          CheckInstance(static_cast<uint64_t>(Number(*args, "instance")));
          if (viewers.size() >= 2) throw std::runtime_error("Desktop texture limit reached");
          ComPtr<IDXGIAdapter> adapter;
          engine->GetGraphicsAdapter(&adapter);
          auto viewer = std::make_shared<Viewer>(registrar->texture_registrar(), video, replies, std::move(adapter));
          viewer->Start();
          viewers.emplace(viewer->id(), viewer);
          result->Success(Value(viewer->id()));
          return;
        }
        const auto id = Number(*args, "textureId");
        const auto found = viewers.find(id);
        if (found == viewers.end()) throw std::runtime_error("Unknown desktop texture");
        auto viewer = found->second;
        if (call.method_name() == "attach") {
          viewer->Attach(static_cast<uint64_t>(Number(*args, "source")));
          result->Success();
        } else if (call.method_name() == "detach") { viewer->Detach(); result->Success(); }
        else if (call.method_name() == "snapshot") {
          const int64_t width = args->find(Value("maxWidth")) == args->end() ? kSnapshotWidth : Number(*args, "maxWidth");
          if (width <= 0) throw std::runtime_error("Invalid snapshot width");
          auto path = Path(*args, "path");
          viewer->Snapshot(std::move(path), static_cast<uint32_t>(std::min<int64_t>(width, UINT32_MAX)), std::move(result));
        }
        else if (call.method_name() == "dispose") { viewers.erase(found); viewer->Close(); result->Success(); }
        else if (call.method_name() == "stats") result->Success(Value(viewer->Stats()));
        else result->NotImplemented();
      } catch (const std::exception& e) { result->Error("desktop_video", e.what()); }
    });
  }
  ~Impl() {
    channel->SetMethodCallHandler(nullptr);
    for (auto& entry : viewers) entry.second->Close();
    viewers.clear();
    // Every decoder thread has stopped. Answer the snapshots they served
    // while the engine and its messenger still exist.
    replies->Drain();
  }
  // D9: Dart and this runner must reach one library instance, or the decoder
  // would pull from a registry Dart never filled. Fails closed.
  void CheckInstance(uint64_t dart) {
    if (!video) video = LoadVideoLibrary();
    const uint64_t native = video->instance_token();
    char line[96];
    snprintf(line, sizeof(line), "Video library instance: Dart 0x%llx, runner 0x%llx\n",
             static_cast<unsigned long long>(dart), static_cast<unsigned long long>(native));
    OutputDebugStringA(line);
    if (native != dart) throw std::runtime_error("native video library loaded twice");
  }
};

DesktopVideo::DesktopVideo(flutter::FlutterEngine* engine, HWND window) : impl_(std::make_unique<Impl>(engine, window)) {}
DesktopVideo::~DesktopVideo() = default;
bool DesktopVideo::HandleMessage(UINT message) {
  if (message != kWake) return false;
  impl_->replies->Drain();
  return true;
}
