#include "desktop_video.h"

#include <d3d11.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <wmcodecdsp.h>
#include <codecapi.h>
#include <wrl/client.h>
#include <flutter/method_channel.h>
#include <flutter/plugin_registrar_windows.h>
#include <flutter/standard_method_codec.h>
#include <flutter/texture_registrar.h>

#include <atomic>
#include <condition_variable>
#include <deque>
#include <map>
#include <mutex>
#include <stdexcept>
#include <thread>
#include <vector>

namespace {
using Microsoft::WRL::ComPtr;
using Value = flutter::EncodableValue;
using Map = flutter::EncodableMap;
using SubmitReply = std::unique_ptr<flutter::MethodResult<Value>>;
constexpr UINT kWake = WM_APP + 0x413;
constexpr size_t kMaxBytes = 4 * 1024 * 1024;

void Check(HRESULT hr, const char* operation) {
  if (FAILED(hr)) throw std::runtime_error(std::string(operation) + " (" + std::to_string(static_cast<unsigned long>(hr)) + ")");
}
int64_t Number(const Map& map, const char* key) {
  const auto it = map.find(Value(key));
  if (it == map.end()) throw std::runtime_error("Missing video field");
  if (const auto* value = std::get_if<int64_t>(&it->second)) return *value;
  if (const auto* value = std::get_if<int32_t>(&it->second)) return *value;
  throw std::runtime_error("Invalid video field");
}
bool Boolean(const Map& map, const char* key) {
  const auto it = map.find(Value(key));
  return it != map.end() && std::holds_alternative<bool>(it->second) && std::get<bool>(it->second);
}
struct Packet {
  std::vector<uint8_t> bytes;
  int64_t generation = 0, frame = 0;
  uint32_t width = 0, height = 0;
  uint64_t revision = 0;
  bool key = false;
};
struct Event {
  int64_t texture;
  Packet packet;
  bool presented = false, key = false;
  std::string error;
};
struct Submission {
  Packet packet;
  SubmitReply reply;
};
// Coalesce texture acknowledgements before posting to the platform thread.
struct Events {
  explicit Events(HWND value) : window(value) {}
  std::mutex mutex;
  HWND window;
  bool active = true, posted = false;
  std::map<std::pair<int64_t, bool>, Event> pending;
  std::deque<SubmitReply> replies;
  void Send(Event event) {
    event.packet.bytes.clear();
    std::lock_guard<std::mutex> lock(mutex);
    if (!active) return;
    pending.insert_or_assign({event.texture, event.key}, std::move(event));
    if (!posted) { posted = true; PostMessage(window, kWake, 0, 0); }
  }
  // The decoder owns each reply until it has consumed or retired that input.
  // Delivery is on the platform thread, independently of presentation events.
  void Complete(SubmitReply reply) {
    if (!reply) return;
    std::lock_guard<std::mutex> lock(mutex);
    replies.push_back(std::move(reply));
    if (!posted) { posted = true; PostMessage(window, kWake, 0, 0); }
  }
};

struct Surface {
  ComPtr<ID3D11Texture2D> texture;
  FlutterDesktopGpuSurfaceDescriptor descriptor{};
  Packet packet;
  Surface(ComPtr<ID3D11Texture2D> value, const Packet& frame) : texture(std::move(value)), packet(frame) {
    packet.bytes.clear();
    descriptor.struct_size = sizeof(descriptor);
    ComPtr<IDXGIResource> resource;
    Check(texture.As(&resource), "Get shared Flutter texture");
    HANDLE shared = nullptr;
    Check(resource->GetSharedHandle(&shared), "Share Flutter texture");
    descriptor.handle = shared;
    descriptor.width = descriptor.visible_width = frame.width;
    descriptor.height = descriptor.visible_height = frame.height;
    descriptor.format = kFlutterDesktopPixelFormatBGRA8888;
  }
};
struct SurfaceLease {
  std::shared_ptr<Surface> surface;
  FlutterDesktopGpuSurfaceDescriptor descriptor;
};

class Viewer : public std::enable_shared_from_this<Viewer> {
 public:
  Viewer(flutter::TextureRegistrar* registrar, std::shared_ptr<Events> events, ComPtr<IDXGIAdapter> adapter)
      : registrar_(registrar), events_(std::move(events)), adapter_(std::move(adapter)) {}
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
    worker_ = std::thread([this] { Run(); });
  }
  ~Viewer() { StopWorker(); }
  int64_t id() const { return id_; }
  void Close() {
    StopWorker();
    auto keep_alive = shared_from_this();
    registrar_->UnregisterTexture(id_, [keep_alive] {});
  }
  void Reset() {
    std::lock_guard<std::mutex> lock(mutex_);
    ++revision_;
    ClearQueue();
    latest_.reset();
    generation_ = -1;
    last_ = 0;
    awaiting_key_ = true;
    wake_.notify_one();
  }
  void Submit(Packet packet, SubmitReply reply) {
    bool request_key = false;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (stopped_ || packet.generation < generation_) { reply->Success(); return; }
      if (generation_ != packet.generation) {
        ++revision_;
        ClearQueue();
        latest_.reset();
        generation_ = packet.generation;
        last_ = 0;
        awaiting_key_ = true;
      }
      if (packet.frame <= last_) { reply->Success(); return; }
      last_ = packet.frame;
      if (queue_.size() >= 4) {
        dropped_ += queue_.size();
        ClearQueue();
        latest_.reset();
        ++revision_;
        awaiting_key_ = true;
        request_key = true;
      }
      packet.revision = revision_;
      if (awaiting_key_ && !packet.key) {
        ++dropped_;
        request_key = true;
      } else {
        awaiting_key_ = false;
        queue_.push_back({std::move(packet), std::move(reply)});
        ++submitted_;
        wake_.notify_one();
      }
    }
    if (reply) reply->Success();
    if (request_key) events_->Send({id_, std::move(packet), false, true, ""});
  }
  Map Stats() const {
    return {{Value("submitted"), Value(static_cast<int64_t>(submitted_.load()))},
            {Value("decoded"), Value(static_cast<int64_t>(decoded_.load()))},
            {Value("presented"), Value(static_cast<int64_t>(presented_.load()))},
            {Value("dropped"), Value(static_cast<int64_t>(dropped_.load()))},
            {Value("decoder"), Value("Media Foundation / D3D11")}};
  }
  bool Current(uint64_t revision) const { return !stopped_ && revision == revision_.load(); }

 private:
  // Caller holds mutex_. Retiring an input always returns its producer credit.
  void ClearQueue() {
    for (auto& input : queue_) events_->Complete(std::move(input.reply));
    queue_.clear();
  }
  const FlutterDesktopGpuSurfaceDescriptor* AcquireSurface() {
    std::shared_ptr<Surface> surface;
    bool acknowledge = false;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (!latest_ || !Current(latest_->packet.revision)) return nullptr;
      surface = latest_;
      if (consumed_revision_ != surface->packet.revision || consumed_frame_ != surface->packet.frame) {
        consumed_revision_ = surface->packet.revision;
        consumed_frame_ = surface->packet.frame;
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
    if (acknowledge) { ++presented_; events_->Send({id_, surface->packet, true, false, ""}); }
    return &lease->descriptor;
  }
  void StopWorker() {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      stopped_ = true;
      ++revision_;
      ClearQueue();
      latest_.reset();
    }
    wake_.notify_one();
    if (worker_.joinable()) worker_.join();
  }
  void Fail(const Packet& packet, const std::string& reason) {
    Packet failed = packet;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (!Current(packet.revision)) return;
      ++revision_;
      failed.revision = revision_;
      ClearQueue();
      latest_.reset();
      awaiting_key_ = true;
    }
    events_->Send({id_, std::move(failed), false, true, reason});
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
    std::map<LONGLONG, Packet> pending;
    uint64_t revision;
    uint32_t width, height;
    UINT32 coded_width = 0, coded_height = 0;
    RECT source_rect{};
    LONGLONG next_pts = 0;
    Decoder(const Packet& packet, IDXGIAdapter* adapter) : revision(packet.revision), width(packet.width), height(packet.height) {
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
    void Feed(const Packet& packet) {
      if (pending.size() >= 16) throw std::runtime_error("Native decoder backlog");
      ComPtr<IMFMediaBuffer> buffer;
      Check(MFCreateMemoryBuffer(static_cast<DWORD>(packet.bytes.size()), &buffer), "Allocate compressed video");
      BYTE* data = nullptr;
      Check(buffer->Lock(&data, nullptr, nullptr), "Lock video buffer");
      memcpy(data, packet.bytes.data(), packet.bytes.size());
      buffer->Unlock();
      Check(buffer->SetCurrentLength(static_cast<DWORD>(packet.bytes.size())), "Set compressed video size");
      ComPtr<IMFSample> sample;
      Check(MFCreateSample(&sample), "Create compressed sample");
      Check(sample->AddBuffer(buffer.Get()), "Assign video buffer");
      next_pts += 166667;
      sample->SetSampleTime(next_pts);
      sample->SetSampleDuration(166667);
      if (packet.key) sample->SetUINT32(MFSampleExtension_CleanPoint, TRUE);
      Check(transform->ProcessInput(0, sample.Get(), 0), "Decode H.264 input");
      Packet metadata = packet;
      metadata.bytes.clear();
      pending.emplace(next_pts, std::move(metadata));
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
      Packet packet = std::move(found->second);
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
      return std::make_shared<Surface>(std::move(target), packet);
    }
  };

  void Run() {
    const HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    const HRESULT mf = SUCCEEDED(com) ? MFStartup(MF_VERSION, MFSTARTUP_LITE) : com;
    std::unique_ptr<Decoder> decoder;
    while (!stopped_) {
      Packet packet;
      SubmitReply reply;
      {
        std::unique_lock<std::mutex> lock(mutex_);
        wake_.wait(lock, [&] { return stopped_ || !queue_.empty() || (decoder && decoder->revision != revision_); });
        if (stopped_) break;
        if (decoder && decoder->revision != revision_) {
          try { decoder->Flush(revision_); } catch (...) { decoder.reset(); }
        }
        if (queue_.empty()) continue;
        packet = std::move(queue_.front().packet);
        reply = std::move(queue_.front().reply);
        queue_.pop_front();
      }
      try {
        Check(mf, "Initialize native media decoding");
        if (!Current(packet.revision)) { events_->Complete(std::move(reply)); continue; }
        if (!decoder || decoder->width != packet.width || decoder->height != packet.height) {
          if (!packet.key) throw std::runtime_error("A fresh key frame is required");
          decoder = std::make_unique<Decoder>(packet, adapter_.Get());
        }
        if (!Current(packet.revision)) { events_->Complete(std::move(reply)); continue; }
        decoder->Feed(packet);
        events_->Complete(std::move(reply));
        for (int i = 0; i < 16; ++i) {
          auto surface = decoder->Take();
          if (!surface) break;
          ++decoded_;
          {
            std::lock_guard<std::mutex> lock(mutex_);
            if (!Current(surface->packet.revision)) continue;
            latest_ = std::move(surface);
          }
          registrar_->MarkTextureFrameAvailable(id_);
        }
      } catch (const std::exception& e) {
        events_->Complete(std::move(reply));
        decoder.reset();
        Fail(packet, e.what());
      }
    }
    decoder.reset();
    if (SUCCEEDED(mf)) MFShutdown();
    if (SUCCEEDED(com)) CoUninitialize();
  }

  flutter::TextureRegistrar* registrar_;
  std::shared_ptr<Events> events_;
  ComPtr<IDXGIAdapter> adapter_;
  std::unique_ptr<flutter::TextureVariant> texture_;
  int64_t id_ = -1;
  std::atomic<bool> stopped_{false};
  std::atomic<uint64_t> revision_{1};
  std::atomic<uint64_t> submitted_{0}, decoded_{0}, presented_{0}, dropped_{0};
  std::mutex mutex_;
  std::condition_variable wake_;
  std::deque<Submission> queue_;
  std::shared_ptr<Surface> latest_;
  std::shared_ptr<SurfaceLease> borrowed_;
  int64_t generation_ = -1, last_ = 0, consumed_frame_ = 0;
  uint64_t consumed_revision_ = 0;
  bool awaiting_key_ = true;
  std::thread worker_;
};
}  // namespace

struct DesktopVideo::Impl {
  flutter::FlutterEngine* engine;
  flutter::PluginRegistrarWindows* registrar;
  std::unique_ptr<flutter::MethodChannel<Value>> channel;
  std::shared_ptr<Events> events;
  std::map<int64_t, std::shared_ptr<Viewer>> viewers;
  Impl(flutter::FlutterEngine* value, HWND window) : engine(value), events(std::make_shared<Events>(window)) {
    registrar = flutter::PluginRegistrarManager::GetInstance()->GetRegistrar<flutter::PluginRegistrarWindows>(engine->GetRegistrarForPlugin("PbMapperDesktopVideo"));
    channel = std::make_unique<flutter::MethodChannel<Value>>(engine->messenger(), "pb_mapper/desktop_video", &flutter::StandardMethodCodec::GetInstance());
    channel->SetMethodCallHandler([this](const auto& call, auto result) {
      try {
        if (call.method_name() == "create") {
          if (viewers.size() >= 2) throw std::runtime_error("Desktop texture limit reached");
          ComPtr<IDXGIAdapter> adapter;
          engine->GetGraphicsAdapter(&adapter);
          auto viewer = std::make_shared<Viewer>(registrar->texture_registrar(), events, std::move(adapter));
          viewer->Start();
          viewers.emplace(viewer->id(), viewer);
          result->Success(Value(viewer->id()));
          return;
        }
        const auto* args = call.arguments() ? std::get_if<Map>(call.arguments()) : nullptr;
        if (!args) throw std::runtime_error("Missing desktop video arguments");
        const auto id = Number(*args, "textureId");
        const auto found = viewers.find(id);
        if (found == viewers.end()) throw std::runtime_error("Unknown desktop texture");
        auto viewer = found->second;
        if (call.method_name() == "submit") {
          const auto bytes = args->find(Value("bytes"));
          if (bytes == args->end()) throw std::runtime_error("Missing compressed frame");
          const auto* data = std::get_if<std::vector<uint8_t>>(&bytes->second);
          const auto width = Number(*args, "width"), height = Number(*args, "height");
          if (!data || data->empty() || data->size() > kMaxBytes || width < 2 || width > 7680 || height < 2 || height > 4320 || width * height > 16777216) throw std::runtime_error("Invalid video bounds");
          Packet packet;
          packet.bytes = *data;
          packet.generation = Number(*args, "generation");
          packet.frame = Number(*args, "frameId");
          packet.width = static_cast<uint32_t>(width);
          packet.height = static_cast<uint32_t>(height);
          packet.key = Boolean(*args, "keyframe");
          viewer->Submit(std::move(packet), std::move(result));
        } else if (call.method_name() == "reset") { viewer->Reset(); result->Success(); }
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
    // All decoder threads have stopped. Resolve every outstanding submit while
    // the Flutter engine and its binary messenger still exist.
    Drain();
    { std::lock_guard<std::mutex> lock(events->mutex); events->active = false; events->pending.clear(); }
  }
  void Drain() {
    std::map<std::pair<int64_t, bool>, Event> ready;
    std::deque<SubmitReply> replies;
    { std::lock_guard<std::mutex> lock(events->mutex); ready.swap(events->pending); replies.swap(events->replies); events->posted = false; }
    for (auto& reply : replies) reply->Success();
    for (const auto& entry : ready) {
      const auto& event = entry.second;
      const auto found = viewers.find(event.texture);
      if (found == viewers.end() || !found->second->Current(event.packet.revision)) continue;
      channel->InvokeMethod("event", std::make_unique<Value>(Map{
        {Value("textureId"), Value(event.texture)}, {Value("generation"), Value(event.packet.generation)},
        {Value("frameId"), Value(event.packet.frame)}, {Value("width"), Value(static_cast<int64_t>(event.packet.width))},
        {Value("height"), Value(static_cast<int64_t>(event.packet.height))}, {Value("presented"), Value(event.presented)},
        {Value("needKeyframe"), Value(event.key)}, {Value("message"), Value(event.error)}}));
    }
  }
};

DesktopVideo::DesktopVideo(flutter::FlutterEngine* engine, HWND window) : impl_(std::make_unique<Impl>(engine, window)) {}
DesktopVideo::~DesktopVideo() = default;
bool DesktopVideo::HandleMessage(UINT message) {
  if (message != kWake) return false;
  impl_->Drain();
  return true;
}
