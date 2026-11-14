// The remote desktop's native video on macOS and iOS. One source, compiled
// into both runners: ui/macos/Runner.xcodeproj and ui/ios/Runner.xcodeproj
// reference this file.
//
// It implements the `pb_mapper/desktop_video` method channel exactly as
// ui/lib/src/ffi/desktop_video.dart calls it, like the Windows runner
// (ui/windows/runner/desktop_video.cpp) and the Android one (DesktopVideo.kt,
// VideoFeeder.kt). While a source is attached, a decoder thread pulls H.264
// or H.265 access units straight from Rust (the pb_mapper_desktop_video_*_v1
// exports of ui/native/pb_mapper_ffi/src/desktop_video.rs), decodes them with
// VideoToolbox into IOSurface-backed BGRA pixel buffers, and keeps the newest
// for Flutter, whose compositor takes it in copyPixelBuffer: the presentation
// receipt Rust validates. Nothing of the video crosses Dart.

import CoreMedia
import CoreVideo
import Foundation
import ImageIO
import VideoToolbox

#if os(iOS)
  import Flutter
  import UIKit
#elseif os(macOS)
  import AppKit
  import FlutterMacOS
#endif

private let channelName = "pb_mapper/desktop_video"

// The video C ABI (desktop_video.rs): what `next` returns.
private let videoUnit: Int32 = 1
private let videoTimeout: Int32 = 0
private let videoNeedCapacity: Int32 = -2
private let videoPaused: Int32 = -3
// PbDesktopVideoMetaV1.flags. A unit with neither the H.265 bit nor the JPEG
// one (which never reaches a runner: Dart decodes JPEG) is H.264.
private let flagKeyframe: UInt32 = 1
private let flagDiscontinuity: UInt32 = 1 << 1
private let flagH265: UInt32 = 1 << 4
/// `keyframe_v1`'s reason for a decoder without references.
private let reasonGap: UInt32 = 1
/// PbDesktopVideoMetaV1 is 56 bytes; desktop_video.rs asserts the offsets
/// this file reads it at.
private let metaSize = 56

private let pullTimeoutMs: UInt32 = 50
private let maxUnitBytes = 4 * 1024 * 1024
// The largest picture a session allows (VideoLimits, 3840 x 2160): nothing
// larger reaches a decoder.
private let maxWidth: UInt32 = 3840
private let maxHeight: UInt32 = 2160
private let textureLimit = 2
private let snapshotWidth = 320
private let snapshotQuality = 0.8
private let snapshotDeadline = DispatchTimeInterval.milliseconds(300)
/// kVTInvalidSessionErr: the system took the session back, as it does from
/// an iOS app in the background.
private let invalidSessionErr: OSStatus = -12903
/// kVTVideoDecoderNotAvailableNowErr: no decoder for now (iOS, in the
/// background).
private let decoderNotAvailableNowErr: OSStatus = -12913

private let snapshotQueue = DispatchQueue(label: "pb-desktop-snapshot", qos: .userInitiated)

/// What the owner reads in Console.app (macOS) or the device log (iOS):
/// every line starts with "PbDesktopVideo:".
private func logVideo(_ message: String) {
  NSLog("%@", "PbDesktopVideo: \(message)")
}

/// A failure, in the words the Quality sheet and Rust's log show.
private struct VideoError: Error {
  let message: String
  init(_ message: String) { self.message = message }
}

/// The system took the decoder back (an app in the background): the next
/// key frame builds a new one. Not a decoding failure.
private struct DecoderLost: Error {}

private func check(_ status: OSStatus, _ operation: @autoclosure () -> String) throws {
  if status != 0 { throw VideoError("\(operation()) (\(status))") }
}

/// The video C ABI, from the copy of pb_mapper_ffi Dart loaded. On macOS that
/// is the dylib in the app's Frameworks, opened with RTLD_NOLOAD so that no
/// second copy can load (Rust state would split in two), as the Windows
/// runner's GetModuleHandleExW; on iOS it is the static library linked into
/// the app, found as Dart's DynamicLibrary.process() finds it.
private final class VideoLibrary {
  typealias Attach = @convention(c) (UInt64) -> Int64
  typealias Next = @convention(c) (
    UInt64, UInt32, UnsafeMutablePointer<UInt8>?, UInt32, UnsafeMutableRawPointer?
  ) -> Int32
  typealias Presented = @convention(c) (UInt64, UInt64, UInt64) -> Int32
  typealias Keyframe = @convention(c) (UInt64, UInt32) -> Int32
  typealias Failed = @convention(c) (UInt64, UnsafePointer<CChar>?) -> Int32
  typealias Report = @convention(c) (UInt64, UnsafePointer<CChar>?, UInt32) -> Int32
  typealias Detach = @convention(c) (UInt64) -> Void
  typealias Token = @convention(c) () -> UInt64

  let attach: Attach
  let next: Next
  let presented: Presented
  let keyframe: Keyframe
  let failed: Failed
  let report: Report
  let detach: Detach
  let instanceToken: Token

  private init(_ handle: UnsafeMutableRawPointer?) throws {
    func resolve<T>(_ name: String) throws -> T {
      guard let address = dlsym(handle, name) else {
        throw VideoError("pb_mapper_ffi does not match this app (no \(name))")
      }
      return unsafeBitCast(address, to: T.self)
    }
    attach = try resolve("pb_mapper_desktop_video_attach_v1")
    next = try resolve("pb_mapper_desktop_video_next_v1")
    presented = try resolve("pb_mapper_desktop_video_presented_v1")
    keyframe = try resolve("pb_mapper_desktop_video_keyframe_v1")
    failed = try resolve("pb_mapper_desktop_video_failed_v1")
    report = try resolve("pb_mapper_desktop_video_report_v1")
    detach = try resolve("pb_mapper_desktop_video_detach_v1")
    instanceToken = try resolve("pb_mapper_desktop_instance_token_v1")
  }

  static func load() throws -> VideoLibrary {
    #if os(macOS)
      // The path Dart opens (pb_mapper_ffi.dart). The handle stays open: the
      // runner links this library at launch, so it never unloads anyway.
      guard
        let handle = dlopen(
          "@executable_path/../Frameworks/libpb_mapper_ffi.dylib", RTLD_NOLOAD | RTLD_LAZY)
      else {
        throw VideoError("pb_mapper_ffi is not loaded")
      }
      return try VideoLibrary(handle)
    #else
      // RTLD_DEFAULT, which Swift cannot import: ((void *)-2).
      return try VideoLibrary(UnsafeMutableRawPointer(bitPattern: -2))
    #endif
  }
}

private enum Codec {
  case h264, h265

  private static let avcParameterSets: [UInt8] = [7, 8]
  private static let hevcParameterSets: [UInt8] = [32, 33, 34]

  var name: String { self == .h265 ? "H.265" : "H.264" }

  var type: CMVideoCodecType {
    self == .h265 ? CMVideoCodecType(kCMVideoCodecType_HEVC) : CMVideoCodecType(kCMVideoCodecType_H264)
  }

  /// The NAL unit types of its parameter sets, in the order a format
  /// description takes them: H.264's SPS (7) and PPS (8); H.265's VPS (32),
  /// SPS (33) and PPS (34).
  var parameterSets: [UInt8] { self == .h265 ? Codec.hevcParameterSets : Codec.avcParameterSets }

  /// A NAL unit's type: H.264's low five bits of its one-byte header, H.265's
  /// six above the forbidden bit of its two-byte one.
  func nalType(_ header: UInt8) -> UInt8 {
    self == .h265 ? (header >> 1) & 0x3F : header & 0x1F
  }

  /// Whether a NAL unit goes into the sample VideoToolbox decodes: not the
  /// parameter sets, which the format description carries, nor an access
  /// unit delimiter (H.264's 9, H.265's 35).
  func inSample(_ type: UInt8) -> Bool {
    self == .h265 ? !(32...35).contains(type) : !(7...9).contains(type)
  }
}

/// The Annex B handling: one access unit's NAL units, the parameter sets in
/// force, and the sample VideoToolbox decodes, where each NAL unit sits behind
/// its 4-byte big-endian length instead of a start code. Every buffer is
/// reused; a parameter set is copied only when it changes.
private final class Bitstream {
  private struct Nal {
    let start: Int
    let end: Int
    let type: UInt8
  }

  private struct Bytes {
    let pointer: UnsafeMutablePointer<UInt8>
    let count: Int
  }

  private var nals: [Nal] = []
  private(set) var sample: UnsafeMutablePointer<UInt8>
  private var capacity: Int
  private var setsCodec = Codec.h264
  private var sets: [Bytes?] = [nil, nil, nil]

  init() {
    capacity = 1 << 20
    sample = UnsafeMutablePointer<UInt8>.allocate(capacity: capacity)
    nals.reserveCapacity(64)
  }

  deinit {
    sample.deallocate()
    for set in sets { set?.pointer.deallocate() }
  }

  /// Finds the NAL units behind the start codes (00 00 01 or 00 00 00 01),
  /// jumping between 01 bytes with memchr. A unit's trailing zero bytes, the
  /// first byte of a 4-byte start code among them, are not part of it.
  func split(_ base: UnsafePointer<UInt8>, _ length: Int, _ codec: Codec) throws {
    nals.removeAll(keepingCapacity: true)
    var start = -1
    var from = 2
    while from < length {
      guard let hit = memchr(base + from, 1, length - from) else { break }
      let one = UnsafeRawPointer(base).distance(to: UnsafeRawPointer(hit))
      from = one + 1
      guard base[one - 1] == 0, base[one - 2] == 0 else { continue }
      if start >= 0 { try add(base, start, one - 2, codec) }
      start = one + 1
    }
    guard start >= 0 else { throw VideoError("The video unit is not Annex B") }
    try add(base, start, length, codec)
  }

  private func add(_ base: UnsafePointer<UInt8>, _ start: Int, _ end: Int, _ codec: Codec) throws {
    var end = end
    while end > start && base[end - 1] == 0 { end -= 1 }
    guard end > start else { return }
    guard nals.count < 4096 else { throw VideoError("Too many NAL units in a video unit") }
    nals.append(Nal(start: start, end: end, type: codec.nalType(base[start])))
  }

  /// A key frame's parameter sets replace those in force; true when one of
  /// them changed (or the codec did). A set it does not carry stays in force.
  func takeParameterSets(_ base: UnsafePointer<UInt8>, _ codec: Codec) -> Bool {
    var changed = false
    if codec != setsCodec {
      for index in sets.indices {
        sets[index]?.pointer.deallocate()
        sets[index] = nil
      }
      setsCodec = codec
      changed = true
    }
    for (index, type) in codec.parameterSets.enumerated() {
      guard let nal = nals.last(where: { $0.type == type }) else { continue }
      let count = nal.end - nal.start
      if let set = sets[index], set.count == count, memcmp(set.pointer, base + nal.start, count) == 0 {
        continue
      }
      let copy = UnsafeMutablePointer<UInt8>.allocate(capacity: count)
      copy.initialize(from: base + nal.start, count: count)
      sets[index]?.pointer.deallocate()
      sets[index] = Bytes(pointer: copy, count: count)
      changed = true
    }
    return changed
  }

  /// The format description of the parameter sets in force, which a key
  /// frame must have carried.
  func describe(_ codec: Codec) throws -> CMVideoFormatDescription {
    var pointers: [UnsafePointer<UInt8>] = []
    var sizes: [Int] = []
    for index in codec.parameterSets.indices {
      guard codec == setsCodec, let set = sets[index] else {
        throw VideoError(
          codec == .h265
            ? "Key frame must include H.265 VPS, SPS and PPS" : "Key frame must include H.264 SPS and PPS")
      }
      pointers.append(UnsafePointer(set.pointer))
      sizes.append(set.count)
    }
    var format: CMFormatDescription?
    let status: OSStatus
    switch codec {
    case .h264:
      status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: kCFAllocatorDefault, parameterSetCount: pointers.count,
        parameterSetPointers: pointers, parameterSetSizes: sizes, nalUnitHeaderLength: 4,
        formatDescriptionOut: &format)
    case .h265:
      status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: kCFAllocatorDefault, parameterSetCount: pointers.count,
        parameterSetPointers: pointers, parameterSetSizes: sizes, nalUnitHeaderLength: 4,
        extensions: nil, formatDescriptionOut: &format)
    }
    try check(status, "Read the \(codec.name) parameter sets")
    guard let described = format else { throw VideoError("Read the \(codec.name) parameter sets") }
    return try withBt709(described)
  }

  /// Writes the sample: every NAL unit but the parameter sets and access unit
  /// delimiters, each behind its length. Its length; 0 when nothing is left.
  func assemble(_ base: UnsafePointer<UInt8>, _ codec: Codec) -> Int {
    var length = 0
    for nal in nals where codec.inSample(nal.type) { length += 4 + nal.end - nal.start }
    if length > capacity {
      sample.deallocate()
      capacity = length
      sample = UnsafeMutablePointer<UInt8>.allocate(capacity: capacity)
    }
    var at = 0
    for nal in nals where codec.inSample(nal.type) {
      let count = nal.end - nal.start
      sample[at] = UInt8(truncatingIfNeeded: count >> 24)
      sample[at + 1] = UInt8(truncatingIfNeeded: count >> 16)
      sample[at + 2] = UInt8(truncatingIfNeeded: count >> 8)
      sample[at + 3] = UInt8(truncatingIfNeeded: count)
      (sample + at + 4).initialize(from: base + nal.start, count: count)
      at += 4 + count
    }
    return length
  }
}

/// A stream that names no colour matrix is read as BT.709, which every
/// pb-mapper host encodes (studio range, the default when nothing is said);
/// left untagged, the conversion to BGRA could take it for BT.601.
private func withBt709(_ format: CMVideoFormatDescription) throws -> CMVideoFormatDescription {
  if CMFormatDescriptionGetExtension(format, extensionKey: kCMFormatDescriptionExtension_YCbCrMatrix) != nil {
    return format
  }
  var extensions = (CMFormatDescriptionGetExtensions(format) as? [String: Any]) ?? [:]
  let tags: [(CFString, CFString)] = [
    (kCMFormatDescriptionExtension_YCbCrMatrix, kCMFormatDescriptionYCbCrMatrix_ITU_R_709_2),
    (kCMFormatDescriptionExtension_ColorPrimaries, kCMFormatDescriptionColorPrimaries_ITU_R_709_2),
    (kCMFormatDescriptionExtension_TransferFunction, kCMFormatDescriptionTransferFunction_ITU_R_709_2),
  ]
  for (key, value) in tags where extensions[key as String] == nil {
    extensions[key as String] = value
  }
  let dimensions = CMVideoFormatDescriptionGetDimensions(format)
  var tagged: CMFormatDescription?
  try check(
    CMVideoFormatDescriptionCreate(
      allocator: kCFAllocatorDefault, codecType: CMFormatDescriptionGetMediaSubType(format),
      width: dimensions.width, height: dimensions.height, extensions: extensions as CFDictionary,
      formatDescriptionOut: &tagged),
    "Tag the stream as BT.709")
  return tagged ?? format
}

/// VideoToolbox's output callback. It runs before
/// VTDecompressionSessionDecodeFrame returns: no frame asks for asynchronous
/// or temporal processing.
private func videoDecoded(
  _ refCon: UnsafeMutableRawPointer?, _ frameRefCon: UnsafeMutableRawPointer?, _ status: OSStatus,
  _ flags: VTDecodeInfoFlags, _ image: CVImageBuffer?, _ presentation: CMTime, _ duration: CMTime
) {
  guard let refCon = refCon else { return }
  let decoder = Unmanaged<VideoDecoder>.fromOpaque(refCon).takeUnretainedValue()
  decoder.outputStatus = status
  decoder.outputImage = image
}

/// A VideoToolbox session for one codec, size and set of parameter sets.
/// Decoding is synchronous: each access unit's picture comes out before the
/// call returns, so nothing is held back or reordered (the hosts send no
/// reordered frames). It runs in real time, into IOSurface-backed BGRA pixel
/// buffers of the session's size that VideoToolbox's own pool recycles.
private final class VideoDecoder {
  let codec: Codec
  let width: UInt32
  let height: UInt32
  private let format: CMVideoFormatDescription
  private var session: VTDecompressionSession?
  private(set) var hardware = false
  private(set) var realTime = false
  // Written by the output callback, during a decode call.
  fileprivate var outputStatus: OSStatus = 0
  fileprivate var outputImage: CVImageBuffer?

  var name: String { "VideoToolbox \(codec.name)" }

  init(codec: Codec, width: UInt32, height: UInt32, format: CMVideoFormatDescription) throws {
    self.codec = codec
    self.width = width
    self.height = height
    self.format = format
    var callback = VTDecompressionOutputCallbackRecord(
      decompressionOutputCallback: videoDecoded,
      decompressionOutputRefCon: Unmanaged.passUnretained(self).toOpaque())
    var created: VTDecompressionSession?
    let status = VTDecompressionSessionCreate(
      allocator: kCFAllocatorDefault, formatDescription: format,
      decoderSpecification: VideoDecoder.specification,
      imageBufferAttributes: VideoDecoder.attributes(width, height), outputCallback: &callback,
      decompressionSessionOut: &created)
    if status == invalidSessionErr || status == decoderNotAvailableNowErr { throw DecoderLost() }
    try check(status, "Create the \(codec.name) decoder")
    guard let session = created else { throw VideoError("Create the \(codec.name) decoder") }
    self.session = session
    realTime =
      VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue) == 0
    hardware = VideoDecoder.usesHardware(session, codec)
  }

  deinit { close() }

  /// A Mac decodes in hardware where it can, and in software where it cannot.
  /// iOS has no choice to make: it decodes in hardware wherever the device
  /// has the codec.
  private static var specification: CFDictionary? {
    #if os(macOS)
      let specification: [String: Any] = [
        kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder as String: true
      ]
      return specification as CFDictionary
    #else
      return nil
    #endif
  }

  private static func attributes(_ width: UInt32, _ height: UInt32) -> CFDictionary {
    let attributes: [String: Any] = [
      kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
      kCVPixelBufferWidthKey as String: Int(width),
      kCVPixelBufferHeightKey as String: Int(height),
      kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
      kCVPixelBufferMetalCompatibilityKey as String: true,
    ]
    return attributes as CFDictionary
  }

  private static func usesHardware(_ session: VTDecompressionSession, _ codec: Codec) -> Bool {
    #if os(macOS)
      var value: Unmanaged<CFTypeRef>?
      if VTSessionCopyProperty(
        session, key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
        allocator: kCFAllocatorDefault, valueOut: &value) == 0,
        let flag = value?.takeRetainedValue() as? Bool
      {
        return flag
      }
    #endif
    return VTIsHardwareDecodeSupported(codec.type)
  }

  /// One sample's picture; nil when the decoder dropped it.
  func decode(_ bytes: UnsafeMutablePointer<UInt8>, _ length: Int) throws -> CVPixelBuffer? {
    guard let session = session else { throw DecoderLost() }
    // The sample borrows the bitstream's buffer: decoding is synchronous,
    // so nothing reads it once this call returns.
    var block: CMBlockBuffer?
    try check(
      CMBlockBufferCreateWithMemoryBlock(
        allocator: kCFAllocatorDefault, memoryBlock: UnsafeMutableRawPointer(bytes),
        blockLength: length, blockAllocator: kCFAllocatorNull, customBlockSource: nil,
        offsetToData: 0, dataLength: length, flags: 0, blockBufferOut: &block),
      "Wrap the \(codec.name) unit")
    var sample: CMSampleBuffer?
    var size = length
    try check(
      CMSampleBufferCreateReady(
        allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format,
        sampleCount: 1, sampleTimingEntryCount: 0, sampleTimingArray: nil,
        sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample),
      "Wrap the \(codec.name) unit")
    guard let ready = sample else { throw VideoError("Wrap the \(codec.name) unit") }
    outputStatus = 0
    outputImage = nil
    var info = VTDecodeInfoFlags()
    let status = VTDecompressionSessionDecodeFrame(
      session, sampleBuffer: ready, flags: [], frameRefcon: nil, infoFlagsOut: &info)
    let result = status != 0 ? status : outputStatus
    let image = outputImage
    outputImage = nil
    if result == invalidSessionErr || result == decoderNotAvailableNowErr { throw DecoderLost() }
    try check(result, "Decode \(codec.name)")
    return image
  }

  func close() {
    if let session = session { VTDecompressionSessionInvalidate(session) }
    session = nil
  }
}

/// A decoded picture and the unit it came from.
private struct Picture {
  let buffer: CVPixelBuffer
  let generation: UInt64
  let frame: UInt64
  let revision: UInt64
  var readyNs: UInt64 = 0
}

/// One of the stats' timings: count, mean and maximum, in constant space.
/// It exists only while a stats request asks for timings.
private final class Timing {
  private var samples = 0
  private var totalNs: UInt64 = 0
  private var maxNs: UInt64 = 0

  func add(_ ns: UInt64) {
    samples += 1
    totalNs &+= ns
    maxNs = max(maxNs, ns)
  }

  var snapshot: [String: Any] {
    [
      "samples": samples,
      "mean_ms": samples == 0 ? 0.0 : Double(totalNs) / Double(samples) / 1_000_000,
      "max_ms": Double(maxNs) / 1_000_000,
    ]
  }
}

/// PbDesktopVideoMetaV1, read by offset.
private struct UnitMeta {
  let generation: UInt64
  let frame: UInt64
  let width: UInt32
  let height: UInt32
  let length: Int
  let flags: UInt32

  init(_ meta: UnsafeMutableRawPointer) {
    generation = meta.load(fromByteOffset: 0, as: UInt64.self)
    frame = meta.load(fromByteOffset: 8, as: UInt64.self)
    width = meta.load(fromByteOffset: 40, as: UInt32.self)
    height = meta.load(fromByteOffset: 44, as: UInt32.self)
    length = Int(meta.load(fromByteOffset: 48, as: UInt32.self))
    flags = meta.load(fromByteOffset: 52, as: UInt32.self)
  }

  var key: Bool { flags & flagKeyframe != 0 }
  var discontinuity: Bool { flags & flagDiscontinuity != 0 }
  var codec: Codec { flags & flagH265 != 0 ? .h265 : .h264 }
}

/// One texture. While a source is attached, its Feed thread decodes into the
/// newest picture, which waits for the compositor: a queue of one, so a
/// picture the compositor did not take in time is superseded, never queued.
private final class DesktopVideoViewer: NSObject, FlutterTexture {
  private weak var textures: FlutterTextureRegistry?
  private let library: VideoLibrary
  /// Set by the plugin once the texture is registered, before any attach.
  var textureId: Int64 = -1
  /// Platform thread only: signalled when the attached source's thread ends.
  private var worker: DispatchSemaphore?

  private let lock = NSLock()
  // Guarded by lock.
  private var consumer: UInt64 = 0
  private var stopping = false
  private var closed = false
  private var revision: UInt64 = 1
  private var latest: Picture?
  private var latestTaken = false
  private var shown: CVPixelBuffer?
  private var consumedRevision: UInt64 = 0
  private var consumedFrame: UInt64 = 0
  private var framePosted = false
  private var submitted = 0
  private var decoded = 0
  private var presented = 0
  private var dropped = 0
  private var configurations = 0
  private var recoveries = 0
  private var lastRecovery = ""
  private var decoderName = ""
  private var lowLatency = false
  private var surfaceConsume: Timing?

  init(textures: FlutterTextureRegistry, library: VideoLibrary) {
    self.textures = textures
    self.library = library
    super.init()
  }

  // MARK: Platform thread

  /// The texture and its last picture stay through a reconnect; the new
  /// session's first key frame replaces them.
  func attach(source: UInt64) throws {
    detach()
    let attached = library.attach(source)
    guard attached > 0 else { throw VideoError("Video source unavailable (\(attached))") }
    let consumer = UInt64(attached)
    lock.lock()
    self.consumer = consumer
    lock.unlock()
    let feed = Feed(viewer: self, library: library, consumer: consumer)
    let done = DispatchSemaphore(value: 0)
    let thread = Thread {
      feed.run()
      done.signal()
    }
    thread.name = "pb-desktop-video"
    thread.qualityOfService = .userInteractive
    worker = done
    thread.start()
  }

  /// Detaching wakes a blocked pull, so the wait is short. Then, with no Feed
  /// thread left to publish, a new revision: a picture of this source the
  /// compositor has not taken yet stays on screen until the next source's
  /// first one, but earns no receipt from any consumer (Android's
  /// FeedQueue.reset). Attach detaches first, so it starts one too.
  func detach() {
    lock.lock()
    stopping = true
    let consumer = self.consumer
    self.consumer = 0
    lock.unlock()
    if consumer != 0 { library.detach(consumer) }
    worker?.wait()
    worker = nil
    lock.lock()
    stopping = false
    revision += 1
    lock.unlock()
  }

  func close() {
    detach()
    lock.lock()
    closed = true
    lock.unlock()
    textures?.unregisterTexture(textureId)
  }

  /// [timings] starts the timing accumulator, or keeps it; false drops it.
  func stats(timings: Bool) -> [String: Any] {
    lock.lock()
    defer { lock.unlock() }
    if !timings {
      surfaceConsume = nil
    } else if surfaceConsume == nil {
      surfaceConsume = Timing()
    }
    var values: [String: Any] = [
      "submitted": submitted, "decoded": decoded, "presented": presented, "dropped": dropped,
      "configurations": configurations, "recoveries": recoveries, "lastRecovery": lastRecovery,
      "decoder": decoderName, "lowLatency": lowLatency,
    ]
    if let timing = surfaceConsume { values["timings"] = ["surface_consume": timing.snapshot] }
    return values
  }

  /// D23: the picture the compositor took last, which is the one on screen,
  /// as a JPEG at most maxWidth wide, written beside path and renamed over
  /// it, but not past the deadline: by then Dart has its answer and the old
  /// picture stays. Replies on the platform thread.
  func snapshot(path: String, maxWidth: Int, reply: @escaping (Bool) -> Void) {
    lock.lock()
    let picture = shown
    lock.unlock()
    guard let buffer = picture else {
      reply(false)
      return
    }
    let deadline = DispatchTime.now() + snapshotDeadline
    snapshotQueue.async {
      let written = writeSnapshot(buffer, path: path, maxWidth: maxWidth, deadline: deadline)
      DispatchQueue.main.async { reply(written) }
    }
  }

  private func frameAvailable() {
    lock.lock()
    framePosted = false
    let open = !closed
    lock.unlock()
    if open { textures?.textureFrameAvailable(textureId) }
  }

  // MARK: Raster thread

  /// The compositor takes the newest picture. One of an older revision stays
  /// on screen until a newer one replaces it, but only a current one earns a
  /// receipt, and only once.
  func copyPixelBuffer() -> Unmanaged<CVPixelBuffer>? {
    lock.lock()
    guard let picture = latest else {
      lock.unlock()
      return nil
    }
    latestTaken = true
    shown = picture.buffer
    var acknowledge = false
    if picture.revision == revision
      && (consumedRevision != picture.revision || consumedFrame != picture.frame)
    {
      consumedRevision = picture.revision
      consumedFrame = picture.frame
      acknowledge = true
      presented += 1
      if let timing = surfaceConsume, picture.readyNs != 0 {
        let now = DispatchTime.now().uptimeNanoseconds
        timing.add(now > picture.readyNs ? now - picture.readyNs : 0)
      }
    }
    let consumer = self.consumer
    lock.unlock()
    // Taken by the compositor: the receipt Rust validates and forwards for
    // input authorisation. Non-blocking.
    if acknowledge && consumer != 0 {
      _ = library.presented(consumer, picture.generation, picture.frame)
    }
    return Unmanaged.passRetained(picture.buffer)
  }

  // MARK: Feed thread

  var isStopping: Bool {
    lock.lock()
    defer { lock.unlock() }
    return stopping
  }

  /// A new generation, a discontinuity, or a decoder that failed: pictures
  /// decoded before earn no receipt.
  func newRevision() {
    lock.lock()
    revision += 1
    lock.unlock()
  }

  /// A unit goes to the decoder: the revision its picture belongs to.
  func submit() -> UInt64 {
    lock.lock()
    defer { lock.unlock() }
    submitted += 1
    return revision
  }

  func drop() {
    lock.lock()
    dropped += 1
    lock.unlock()
  }

  func configured(_ decoder: VideoDecoder) {
    lock.lock()
    configurations += 1
    decoderName = decoder.name
    lowLatency = decoder.realTime
    lock.unlock()
  }

  /// The next key frame starts over in a new revision.
  func recovered(_ reason: String) {
    lock.lock()
    revision += 1
    recoveries += 1
    lastRecovery = reason
    lock.unlock()
  }

  /// The newest picture, for the compositor. The platform thread tells
  /// Flutter at most once per frame it has not handled yet.
  func publish(_ decodedPicture: Picture) {
    var picture = decodedPicture
    lock.lock()
    decoded += 1
    guard picture.revision == revision else {
      lock.unlock()
      return
    }
    if let previous = latest, !latestTaken, previous.revision == revision { dropped += 1 }
    if surfaceConsume != nil { picture.readyNs = DispatchTime.now().uptimeNanoseconds }
    latest = picture
    latestTaken = false
    let post = !framePosted && !closed
    if post { framePosted = true }
    lock.unlock()
    if post { DispatchQueue.main.async { [weak self] in self?.frameAvailable() } }
  }
}

/// The decoder thread of one attached source, with the decoder state rules
/// of the Windows and Android runners: a new generation or a discontinuity
/// starts a revision; older generations, repeated frame ids and anything
/// before the next key frame are dropped; only a key frame configures a
/// decoder. `next` is its only blocking call.
private final class Feed {
  private let viewer: DesktopVideoViewer
  private let library: VideoLibrary
  private let consumer: UInt64
  private let bitstream = Bitstream()
  private var decoder: VideoDecoder?
  private var generation: UInt64?
  private var last: UInt64 = 0
  private var awaitingKey = true
  private var paused = false

  init(viewer: DesktopVideoViewer, library: VideoLibrary, consumer: UInt64) {
    self.viewer = viewer
    self.library = library
    self.consumer = consumer
  }

  func run() {
    var capacity = 1 << 20
    var buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: capacity)
    let meta = UnsafeMutableRawPointer.allocate(byteCount: metaSize, alignment: 8)
    defer {
      buffer.deallocate()
      meta.deallocate()
    }
    while !viewer.isStopping {
      let result = library.next(consumer, pullTimeoutMs, buffer, UInt32(capacity), meta)
      if result == videoUnit {
        paused = false
        autoreleasepool { accept(buffer, UnitMeta(meta)) }
      } else if result == videoNeedCapacity {
        // The unit stays queued in Rust and arrives on the next call.
        let needed = Int(meta.load(fromByteOffset: 48, as: UInt32.self))
        if needed > maxUnitBytes { break }
        buffer.deallocate()
        capacity = needed
        buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: capacity)
      } else if result == videoPaused {
        paused = true
      } else if result != videoTimeout {
        break  // Closed, detached or invalid: this consumer is done.
      }
    }
    library.detach(consumer)
    decoder?.close()
    decoder = nil
  }

  private func accept(_ base: UnsafeMutablePointer<UInt8>, _ unit: UnitMeta) {
    if let current = generation, unit.generation < current {
      viewer.drop()
      return
    }
    if unit.generation != generation || unit.discontinuity {
      viewer.newRevision()
      generation = unit.generation
      last = 0
      awaitingKey = true
    }
    if unit.frame <= last {
      viewer.drop()
      return
    }
    last = unit.frame
    if awaitingKey && !unit.key {
      viewer.drop()
      _ = library.keyframe(consumer, reasonGap)
      return
    }
    awaitingKey = false
    let revision = viewer.submit()
    do {
      try decode(base, unit, revision)
    } catch is DecoderLost {
      // Not a decoding failure: the system took the session back. The next
      // key frame builds a new one; ask for it, unless the source is paused,
      // whose resume starts with one.
      decoder?.close()
      decoder = nil
      awaitingKey = true
      viewer.recovered("Video decoder session was lost")
      logVideo("Video decoder session was lost; waiting for a key frame")
      if !paused { _ = library.keyframe(consumer, reasonGap) }
    } catch {
      decoder?.close()
      decoder = nil
      fail((error as? VideoError)?.message ?? "\(error)")
    }
  }

  /// Whether the running decoder takes this unit's codec and size.
  private func fits(_ unit: UnitMeta) -> Bool {
    guard let running = decoder else { return false }
    return running.codec == unit.codec && running.width == unit.width && running.height == unit.height
  }

  /// A decoder is configured only at a key frame: when there is none (after
  /// a failure too), when the codec or the size changed, which happen only
  /// at a key frame, or when its parameter sets changed.
  private func decode(_ base: UnsafeMutablePointer<UInt8>, _ unit: UnitMeta, _ revision: UInt64) throws {
    let codec = unit.codec
    try bitstream.split(base, unit.length, codec)
    let changedSets = unit.key && bitstream.takeParameterSets(base, codec)
    if !fits(unit) || changedSets {
      guard unit.key else { throw VideoError("A fresh key frame is required") }
      guard unit.width > 0, unit.height > 0, unit.width <= maxWidth, unit.height <= maxHeight else {
        throw VideoError("Video size outside the session limits")
      }
      try configure(unit)
    }
    let length = bitstream.assemble(base, codec)
    guard length > 0, let running = decoder else {
      viewer.drop()
      return
    }
    var image: CVPixelBuffer?
    do {
      image = try running.decode(bitstream.sample, length)
    } catch is DecoderLost where unit.key {
      // Lost while the app was away, and this key frame starts over: a new
      // session takes it at once.
      try configure(unit)
      image = try decoder?.decode(bitstream.sample, length)
    }
    guard let picture = image else {
      viewer.drop()
      return
    }
    guard CVPixelBufferGetWidth(picture) == Int(unit.width),
      CVPixelBufferGetHeight(picture) == Int(unit.height)
    else {
      throw VideoError("Decoded picture does not match the authorized frame")
    }
    viewer.publish(
      Picture(buffer: picture, generation: unit.generation, frame: unit.frame, revision: revision))
  }

  private func configure(_ unit: UnitMeta) throws {
    decoder?.close()
    decoder = nil
    let next = try VideoDecoder(
      codec: unit.codec, width: unit.width, height: unit.height,
      format: bitstream.describe(unit.codec))
    decoder = next
    viewer.configured(next)
    logVideo(
      "\(next.name) configured for \(unit.width)x\(unit.height) "
        + "(hardware: \(next.hardware), real time: \(next.realTime))")
    _ = next.name.withCString { library.report(consumer, $0, next.hardware ? 1 : 0) }
  }

  /// The next key frame recreates the decoder in a new revision. Rust logs
  /// the reason, counts it toward giving up and rate-limits the request; a
  /// paused source asks for nothing, since its resume starts with a key
  /// frame.
  private func fail(_ reason: String) {
    awaitingKey = true
    viewer.recovered(reason)
    logVideo("Native video decoding failed: \(reason)")
    if !paused { _ = reason.withCString { library.failed(consumer, $0) } }
  }
}

/// D23's JPEG: the picture box-filtered to at most maxWidth wide, quality 80.
private func writeSnapshot(
  _ buffer: CVPixelBuffer, path: String, maxWidth: Int, deadline: DispatchTime
) -> Bool {
  var image: CGImage?
  guard VTCreateCGImageFromCVPixelBuffer(buffer, options: nil, imageOut: &image) == 0,
    let picture = image, picture.width > 0
  else {
    logVideo("Desktop video snapshot failed: the picture cannot be read")
    return false
  }
  let width = min(maxWidth, picture.width)
  let height = max(1, (picture.height * width + picture.width / 2) / picture.width)
  let info = CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
  guard
    let context = CGContext(
      data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
      space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: info)
  else { return false }
  context.interpolationQuality = .high
  context.draw(picture, in: CGRect(x: 0, y: 0, width: width, height: height))
  guard let scaled = context.makeImage() else { return false }
  let jpeg = NSMutableData()
  guard
    let destination = CGImageDestinationCreateWithData(
      jpeg as CFMutableData, "public.jpeg" as CFString, 1, nil)
  else { return false }
  let options: [String: Any] = [kCGImageDestinationLossyCompressionQuality as String: snapshotQuality]
  CGImageDestinationAddImage(destination, scaled, options as CFDictionary)
  guard CGImageDestinationFinalize(destination) else { return false }
  return replaceFile(path, with: jpeg as Data, deadline: deadline)
}

/// Writes beside path, flushes, then renames over it, unless the deadline
/// passed meanwhile.
private func replaceFile(_ path: String, with data: Data, deadline: DispatchTime) -> Bool {
  try? FileManager.default.createDirectory(
    at: URL(fileURLWithPath: path).deletingLastPathComponent(), withIntermediateDirectories: true)
  let temporary = path + ".tmp"
  let file = Darwin.open(temporary, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
  guard file >= 0 else { return false }
  let written = data.withUnsafeBytes { (bytes: UnsafeRawBufferPointer) -> Bool in
    guard let base = bytes.baseAddress else { return false }
    var offset = 0
    while offset < bytes.count {
      let count = Darwin.write(file, base + offset, bytes.count - offset)
      if count <= 0 { return false }
      offset += count
    }
    return true
  }
  let flushed = written && fsync(file) == 0
  Darwin.close(file)
  guard flushed, DispatchTime.now() <= deadline, rename(temporary, path) == 0 else {
    unlink(temporary)
    return false
  }
  return true
}

/// The `decoders` answer. VideoToolbox has no size query, and nothing here
/// creates a decoder.
private enum DecoderProbe {
  private static let uhd: [String: Int] = ["max_width": 3840, "max_height": 2160]
  private static let fullHd: [String: Int] = ["max_width": 1920, "max_height": 1080]

  /// Off the platform thread. H.264 decodes on every device: a Mac whose
  /// hardware cannot take a size decodes it in software, so every bound
  /// decodes there, as on Windows. H.265 is offered only where VideoToolbox
  /// decodes it in hardware: Macs with a 6th-generation Intel GPU or later,
  /// every Apple silicon Mac, and the A9 (iPhone 6s) and later, all of
  /// which decode 3840x2160. On iOS those chips are also where H.264 reaches
  /// 3840x2160 (they record it); an older device (an A8 iPad) stays at 1080p.
  static func answer(displayMaxHz: Double) -> [String: Any] {
    let hevc = VTIsHardwareDecodeSupported(Codec.h265.type)
    #if os(iOS)
      let h264 = hevc ? uhd : fullHd
    #else
      let h264 = uhd
    #endif
    let h265: Any = hevc ? uhd : NSNull()
    return ["h264": h264, "h265": h265, "display_max_hz": displayMaxHz]
  }

  /// Platform thread: the fastest refresh among the displays, since the
  /// window may move to the fastest. Never below 60 Hz when nothing says.
  static func displayMaxHz() -> Double {
    var hz = 0.0
    #if os(iOS)
      hz = Double(UIScreen.main.maximumFramesPerSecond)
    #else
      if #available(macOS 12.0, *) {
        for screen in NSScreen.screens { hz = max(hz, Double(screen.maximumFramesPerSecond)) }
      } else {
        // A built-in panel reports 0 here: it is taken as 60.
        var displays = [CGDirectDisplayID](repeating: 0, count: 16)
        var count: UInt32 = 0
        if CGGetActiveDisplayList(UInt32(displays.count), &displays, &count) == .success {
          for display in displays.prefix(Int(count)) {
            if let mode = CGDisplayCopyDisplayMode(display) { hz = max(hz, mode.refreshRate) }
          }
        }
      }
    #endif
    return hz > 1 ? hz : 60
  }
}

/// The `pb_mapper/desktop_video` channel: probes the decoders, and creates,
/// attaches, snapshots, reads and disposes textures. Platform thread only;
/// what runs elsewhere replies through the main queue.
final class DesktopVideoPlugin: NSObject, FlutterPlugin {
  private weak var textures: FlutterTextureRegistry?
  /// Resolved by the first create: Dart has loaded the library by then.
  private var library: VideoLibrary?
  private var viewers: [Int64: DesktopVideoViewer] = [:]

  private init(textures: FlutterTextureRegistry) {
    self.textures = textures
    super.init()
  }

  static func register(with registrar: FlutterPluginRegistrar) {
    #if os(iOS)
      let messenger = registrar.messenger()
      let textures = registrar.textures()
    #else
      let messenger = registrar.messenger
      let textures = registrar.textures
    #endif
    let channel = FlutterMethodChannel(name: channelName, binaryMessenger: messenger)
    registrar.addMethodCallDelegate(DesktopVideoPlugin(textures: textures), channel: channel)
  }

  func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
    do {
      try dispatch(call, result)
    } catch {
      let message = (error as? VideoError)?.message ?? "\(error)"
      result(FlutterError(code: "desktop_video", message: message, details: nil))
    }
  }

  private func dispatch(_ call: FlutterMethodCall, _ result: @escaping FlutterResult) throws {
    if call.method == "decoders" {
      probe(result)
      return
    }
    guard let args = call.arguments as? [String: Any] else {
      throw VideoError("Missing desktop video arguments")
    }
    if call.method == "create" {
      try checkInstance(UInt64(bitPattern: number(args, "instance")))
      guard viewers.count < textureLimit else { throw VideoError("Desktop texture limit reached") }
      guard let textures = textures, let library = library else {
        throw VideoError("Desktop video is not set up")
      }
      let viewer = DesktopVideoViewer(textures: textures, library: library)
      let id = textures.register(viewer)
      viewer.textureId = id
      viewers[id] = viewer
      result(id)
      return
    }
    let id = try number(args, "textureId")
    guard let viewer = viewers[id] else { throw VideoError("Unknown desktop texture") }
    switch call.method {
    case "attach":
      try viewer.attach(source: UInt64(bitPattern: number(args, "source")))
      result(nil)
    case "snapshot":
      var width = snapshotWidth
      if args["maxWidth"] != nil { width = try Int(number(args, "maxWidth")) }
      guard width > 0 else { throw VideoError("Invalid snapshot width") }
      guard let path = args["path"] as? String, !path.isEmpty else {
        throw VideoError("Missing snapshot path")
      }
      viewer.snapshot(path: path, maxWidth: width) { result($0) }
    case "stats":
      var stats = viewer.stats(timings: args["timings"] as? Bool ?? true)
      stats["refreshHz"] = DecoderProbe.displayMaxHz()
      result(stats)
    case "dispose":
      viewers.removeValue(forKey: id)
      viewer.close()
      result(nil)
    default:
      result(FlutterMethodNotImplemented)
    }
  }

  private func number(_ args: [String: Any], _ key: String) throws -> Int64 {
    guard let value = args[key] as? NSNumber else { throw VideoError("Missing video field \(key)") }
    return value.int64Value
  }

  /// `decoders` takes no arguments: VideoToolbox is asked off the platform
  /// thread, the displays on it.
  private func probe(_ result: @escaping FlutterResult) {
    let hz = DecoderProbe.displayMaxHz()
    DispatchQueue.global(qos: .userInitiated).async {
      let answer = DecoderProbe.answer(displayMaxHz: hz)
      DispatchQueue.main.async { result(answer) }
    }
  }

  /// D9: Dart and this runner must reach one library instance, or the
  /// decoder would pull from a registry Dart never filled. Fails closed.
  private func checkInstance(_ dart: UInt64) throws {
    let library = try self.library ?? VideoLibrary.load()
    self.library = library
    let native = library.instanceToken()
    logVideo(
      "Video library instance: Dart 0x\(String(dart, radix: 16)), runner 0x\(String(native, radix: 16))")
    guard native == dart else { throw VideoError("native video library loaded twice") }
  }
}
