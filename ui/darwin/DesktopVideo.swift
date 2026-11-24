// The remote desktop's native video on macOS and iOS. One source, compiled
// into both runners: ui/macos/Runner.xcodeproj and ui/ios/Runner.xcodeproj
// reference this file.
//
// It implements the `pb_mapper/desktop_video` method channel exactly as
// ui/lib/src/ffi/desktop_video.dart calls it, like the Windows runner
// (ui/windows/runner/desktop_video.cpp) and the Android one (DesktopVideo.kt,
// VideoFeeder.kt). While a source is attached, a decoder thread pulls H.264
// or H.265 access units straight from Rust (the pb_mapper_desktop_video_*_v1
// exports of ui/native/pb_mapper_ffi/src/desktop_video.rs) and decodes them
// with VideoToolbox. How a picture reaches the screen is the viewer's
// presenter, chosen at create:
// - texture (iOS, and macOS when Dart asks for it): IOSurface-backed BGRA
//   pixel buffers, the newest kept for Flutter, whose compositor takes it in
//   copyPixelBuffer: the presentation receipt Rust validates.
// - layer (macOS): IOSurface-backed 420v pixel buffers, which the decoder
//   thread commits to the viewer's own Core Animation layer as each is
//   decoded, with no BGRA pass and no Flutter frame. The commit is the
//   receipt. Flutter shows the layer through the platform view
//   `pb_mapper/desktop_video_view`.
// Nothing of the video crosses Dart.

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
  import QuartzCore
#endif

private let channelName = "pb_mapper/desktop_video"
#if os(macOS)
  /// The platform view that shows a layer presenter's pictures.
  private let viewType = "pb_mapper/desktop_video_view"
  /// The pictures a layer presenter keeps referenced: the one on screen and
  /// those before it, which the WindowServer may still composite, stay out
  /// of VideoToolbox's pool.
  private let heldPictures = 3
#endif

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
  private var scratch: UnsafeMutablePointer<UInt8>?
  private var capacity = 0
  private var setsCodec = Codec.h264
  private var sets: [Bytes?] = [nil, nil, nil]

  init() {
    nals.reserveCapacity(64)
  }

  deinit {
    scratch?.deallocate()
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

  /// Replace four-byte start codes in the feed buffer with AVCC lengths.
  /// Mixed prefixes or gaps between slices need a separate packed sample.
  /// The decoder borrows the result until its synchronous call returns.
  func assemble(_ base: UnsafeMutablePointer<UInt8>, _ codec: Codec)
    -> (bytes: UnsafeMutablePointer<UInt8>, length: Int)?
  {
    var start = 0
    var length = 0
    var contiguous = true
    for nal in nals where codec.inSample(nal.type) {
      if length == 0 { start = nal.start - 4 }
      contiguous = contiguous && start >= 0 && nal.start == start + length + 4
      length += 4 + nal.end - nal.start
    }
    guard length > 0 else { return nil }
    let sample: UnsafeMutablePointer<UInt8>
    if contiguous {
      sample = base + start
    } else if let scratch = scratch, length <= capacity {
      sample = scratch
    } else {
      scratch?.deallocate()
      sample = .allocate(capacity: length)
      scratch = sample
      capacity = length
    }
    var at = 0
    for nal in nals where codec.inSample(nal.type) {
      let count = nal.end - nal.start
      sample[at] = UInt8(truncatingIfNeeded: count >> 24)
      sample[at + 1] = UInt8(truncatingIfNeeded: count >> 16)
      sample[at + 2] = UInt8(truncatingIfNeeded: count >> 8)
      sample[at + 3] = UInt8(truncatingIfNeeded: count)
      if !contiguous {
        (sample + at + 4).initialize(from: base + nal.start, count: count)
      }
      at += 4 + count
    }
    return (sample, length)
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
  // CMFormatDescription.h: a modified clone must drop the verbatim sample
  // description and ISO sample entry, or the modification can be lost to
  // the original they carry.
  extensions.removeValue(forKey: kCMFormatDescriptionExtension_VerbatimSampleDescription as String)
  extensions.removeValue(forKey: kCMFormatDescriptionExtension_VerbatimISOSampleEntry as String)
  let tags: [(CFString, CFString)] = [
    (kCMFormatDescriptionExtension_YCbCrMatrix, kCMFormatDescriptionYCbCrMatrix_ITU_R_709_2),
    (kCMFormatDescriptionExtension_ColorPrimaries, kCMFormatDescriptionColorPrimaries_ITU_R_709_2),
    (kCMFormatDescriptionExtension_TransferFunction, kCMFormatDescriptionTransferFunction_ITU_R_709_2),
  ]
  for (key, value) in tags where extensions[key as String] == nil {
    extensions[key as String] = value
  }
  return try retagged(format, extensions, "Tag the stream as BT.709")
}

/// [format] with [extensions] in place of its own.
private func retagged(
  _ format: CMVideoFormatDescription, _ extensions: [String: Any], _ operation: String
) throws -> CMVideoFormatDescription {
  let dimensions = CMVideoFormatDescriptionGetDimensions(format)
  var tagged: CMFormatDescription?
  try check(
    CMVideoFormatDescriptionCreate(
      allocator: kCFAllocatorDefault, codecType: CMFormatDescriptionGetMediaSubType(format),
      width: dimensions.width, height: dimensions.height, extensions: extensions as CFDictionary,
      formatDescriptionOut: &tagged),
    operation)
  return tagged ?? format
}

#if os(macOS)
  /// The layer presenter's tags, whatever the stream says: sRGB transfer,
  /// BT.709 primaries and the stream's matrix (BT.709 when it names none).
  /// Every pb-mapper host puts sRGB values through the BT.709 matrix with
  /// no transfer conversion and tags its VUI BT.709 (the Mac encoder's
  /// session properties, the Windows host's NV12 conversion). Flutter shows
  /// the BGRA conversion's values as sRGB, so a BT.709 transfer would have
  /// Core Animation lift mid-greys (128 to 139); sRGB keeps the texture
  /// path's look within 2 levels. VideoToolbox carries the tags onto each
  /// picture and its IOSurface, which Core Animation reads.
  private func forLayer(_ format: CMVideoFormatDescription) throws -> CMVideoFormatDescription {
    var extensions = (CMFormatDescriptionGetExtensions(format) as? [String: Any]) ?? [:]
    extensions.removeValue(forKey: kCMFormatDescriptionExtension_VerbatimSampleDescription as String)
    extensions.removeValue(forKey: kCMFormatDescriptionExtension_VerbatimISOSampleEntry as String)
    extensions[kCMFormatDescriptionExtension_TransferFunction as String] =
      kCMFormatDescriptionTransferFunction_sRGB
    extensions[kCMFormatDescriptionExtension_ColorPrimaries as String] =
      kCMFormatDescriptionColorPrimaries_ITU_R_709_2
    if extensions[kCMFormatDescriptionExtension_YCbCrMatrix as String] == nil {
      extensions[kCMFormatDescriptionExtension_YCbCrMatrix as String] =
        kCMFormatDescriptionYCbCrMatrix_ITU_R_709_2
    }
    return try retagged(format, extensions, "Tag the stream as sRGB")
  }

  /// Whether a decoded picture carries the layer presenter's sRGB transfer.
  /// A system without the query (before macOS 12) is taken to carry it, as
  /// VideoToolbox does with the format description's tags.
  private func carriesSrgb(_ buffer: CVPixelBuffer) -> Bool {
    if #available(macOS 12.0, *) {
      guard let transfer = CVBufferCopyAttachment(buffer, kCVImageBufferTransferFunctionKey, nil)
      else { return false }
      return CFEqual(transfer, kCVImageBufferTransferFunction_sRGB)
    }
    return true
  }

  /// Tags a picture VideoToolbox left without the layer's tags. Its colour
  /// space goes: Core Animation reads an IOSurface's colour space before its
  /// transfer function, so a stale BT.709 one would stay in force.
  private func tagSrgb(_ buffer: CVPixelBuffer, matrix: CFTypeRef) {
    CVBufferSetAttachment(buffer, kCVImageBufferYCbCrMatrixKey, matrix, .shouldPropagate)
    CVBufferSetAttachment(
      buffer, kCVImageBufferColorPrimariesKey, kCVImageBufferColorPrimaries_ITU_R_709_2, .shouldPropagate)
    CVBufferSetAttachment(
      buffer, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_sRGB, .shouldPropagate)
    CVBufferRemoveAttachment(buffer, kCVImageBufferCGColorSpaceKey)
  }
#endif

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
/// reordered frames). It runs in real time, into IOSurface-backed pixel
/// buffers of the session's size that VideoToolbox's own pool recycles:
/// BGRA for a texture, which Flutter samples as RGB, and for a layer
/// presenter the decoder's own 420v (NV12, video range), which Core
/// Animation composites, so VideoToolbox runs no conversion pass.
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
  #if os(macOS)
    /// A layer presenter's decoder: the matrix its pictures are tagged
    /// with, whether its first picture was checked for the sRGB tags, and
    /// whether every picture needs them set because VideoToolbox did not
    /// carry them.
    private let layerMatrix: CFTypeRef?
    private var tagsChecked = false
    private var retag = false
  #endif

  var name: String { "VideoToolbox \(codec.name)" }

  /// [layered]: for a layer presenter, which takes 420v tagged sRGB.
  init(codec: Codec, width: UInt32, height: UInt32, format: CMVideoFormatDescription, layered: Bool)
    throws
  {
    self.codec = codec
    self.width = width
    self.height = height
    #if os(macOS)
      let format = layered ? try forLayer(format) : format
      layerMatrix =
        layered
        ? CMFormatDescriptionGetExtension(format, extensionKey: kCMFormatDescriptionExtension_YCbCrMatrix)
          ?? kCVImageBufferYCbCrMatrix_ITU_R_709_2 : nil
    #endif
    self.format = format
    var callback = VTDecompressionOutputCallbackRecord(
      decompressionOutputCallback: videoDecoded,
      decompressionOutputRefCon: Unmanaged.passUnretained(self).toOpaque())
    var created: VTDecompressionSession?
    let status = VTDecompressionSessionCreate(
      allocator: kCFAllocatorDefault, formatDescription: format,
      decoderSpecification: VideoDecoder.specification,
      imageBufferAttributes: VideoDecoder.attributes(width, height, layered: layered),
      outputCallback: &callback, decompressionSessionOut: &created)
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

  /// Metal compatible in either format: a later GPU pass may sample them.
  private static func attributes(_ width: UInt32, _ height: UInt32, layered: Bool) -> CFDictionary {
    let format = layered ? kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange : kCVPixelFormatType_32BGRA
    let attributes: [String: Any] = [
      kCVPixelBufferPixelFormatTypeKey as String: format,
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

  /// One sample's picture, nil when the decoder dropped it, and, when
  /// [timed], how long VTDecompressionSessionDecodeFrame took in
  /// nanoseconds. Untimed, it reads no clock.
  func decode(_ bytes: UnsafeMutablePointer<UInt8>, _ length: Int, timed: Bool) throws
    -> (image: CVPixelBuffer?, ns: UInt64?)
  {
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
    let started = timed ? clock_gettime_nsec_np(CLOCK_UPTIME_RAW) : 0
    let status = VTDecompressionSessionDecodeFrame(
      session, sampleBuffer: ready, flags: [], frameRefcon: nil, infoFlagsOut: &info)
    let ns = timed ? clock_gettime_nsec_np(CLOCK_UPTIME_RAW) &- started : nil
    let result = status != 0 ? status : outputStatus
    let image = outputImage
    outputImage = nil
    if result == invalidSessionErr || result == decoderNotAvailableNowErr { throw DecoderLost() }
    try check(result, "Decode \(codec.name)")
    #if os(macOS)
      if let matrix = layerMatrix, let picture = image { tagForLayer(picture, matrix) }
    #endif
    return (image, ns)
  }

  #if os(macOS)
    /// The first picture says whether VideoToolbox carried the sRGB tags
    /// onto its pictures; if not, each picture is tagged before its commit.
    private func tagForLayer(_ picture: CVPixelBuffer, _ matrix: CFTypeRef) {
      if !tagsChecked {
        tagsChecked = true
        retag = !carriesSrgb(picture)
        if retag { logVideo("VideoToolbox did not tag \(codec.name) pictures sRGB; tagging each") }
      }
      if retag { tagSrgb(picture, matrix: matrix) }
    }
  #endif

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

#if os(macOS)
  /// The delegate of every layer here: no property change animates, on any
  /// thread or in any transaction, so a picture, a resize or the grey still
  /// takes effect at the commit that carries it.
  private final class StillLayers: NSObject, CALayerDelegate {
    static let shared = StillLayers()

    func action(for layer: CALayer, forKey event: String) -> CAAction? { NSNull() }
  }

  /// A layer presenter's layers: a container that a hosting view adds as its
  /// sublayer, holding the picture's content layer and, above it, the grey
  /// still's two layers. Made at create, so the decoder thread commits
  /// pictures before Flutter makes the view, and kept through a reconnect,
  /// so the last picture stays.
  ///
  /// The decoder thread sets only the content layer's contents, each time in
  /// its own explicit transaction, which commits at once: that thread has no
  /// run loop to commit an implicit one. Everything else (hosting, resizing,
  /// the grey still) is the platform thread's.
  private final class LayerPresenter {
    let container = CALayer()
    private let content = CALayer()
    /// The grey still (GreyStill in viewer_overlays.dart): Core Animation's
    /// own compositing filters take the picture's luminance, then halve it.
    /// A CIColorMatrix with GreyStill's coefficients would need the view's
    /// layerUsesCoreImageFilters and, applied in linear light, came out up
    /// to twice as bright as GreyStill.
    private let desaturate = CALayer()
    private let halve = CALayer()
    /// Decoder thread, or the platform thread once none runs: the pictures
    /// last committed, oldest first, kept from VideoToolbox's pool while
    /// the WindowServer may still composite them.
    private var held: [CVPixelBuffer] = []

    init() {
      let fill: CAAutoresizingMask = [.layerWidthSizable, .layerHeightSizable]
      for layer in [container, content, desaturate, halve] {
        layer.delegate = StillLayers.shared
        layer.autoresizingMask = fill
      }
      content.isOpaque = true
      content.contentsGravity = .resize
      content.minificationFilter = .linear
      content.magnificationFilter = .linear
      desaturate.backgroundColor = CGColor(gray: 0.5, alpha: 1)
      desaturate.compositingFilter = "colorBlendMode"
      halve.backgroundColor = CGColor(gray: 0, alpha: 0.5)
      for layer in [desaturate, halve] { layer.isHidden = true }
      for layer in [content, desaturate, halve] { container.addSublayer(layer) }
      held.reserveCapacity(heldPictures)
    }

    // MARK: Decoder thread

    /// Shows [buffer] at once. False when it has no IOSurface to show.
    func commit(_ buffer: CVPixelBuffer) -> Bool {
      guard let surface = CVPixelBufferGetIOSurface(buffer)?.takeUnretainedValue() else { return false }
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      content.contents = surface
      CATransaction.commit()
      if held.count == heldPictures { held.removeFirst() }
      held.append(buffer)
      return true
    }

    // MARK: Platform thread

    /// Moves the layers into [host], a view's own layer, from any view that
    /// hosted them before: Flutter may make a new view for the same viewer.
    func host(in host: CALayer) {
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      container.frame = host.bounds
      host.addSublayer(container)
      CATransaction.commit()
    }

    func grey(_ on: Bool) {
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      desaturate.isHidden = !on
      halve.isHidden = !on
      CATransaction.commit()
    }

    /// Once no decoder thread runs: the picture and those held go.
    func clear() {
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      content.contents = nil
      CATransaction.commit()
      held.removeAll()
    }
  }

  /// The platform view that shows a layer presenter: a layer-backed view
  /// whose own layer hosts the presenter's container. Pointer and keyboard
  /// stay Flutter's: every mouse, scroll and gesture event goes up the
  /// responder chain to the FlutterView and its controller, as it would from
  /// any view without handlers, and the view never takes the keyboard.
  private final class DesktopVideoView: NSView {
    private weak var viewer: DesktopVideoViewer?
    private var screenObserver: NSObjectProtocol?

    init(viewer: DesktopVideoViewer?) {
      self.viewer = viewer
      super.init(frame: .zero)
      wantsLayer = true
      layerContentsRedrawPolicy = .never
      // The cursor is whatever Flutter or the pointer plugin set last: no
      // cursor rects, and cursor updates over the picture are taken here
      // so AppKit restores no arrow. FlutterView restores its own cursor
      // only where it is the hit view itself.
      addTrackingArea(
        NSTrackingArea(
          rect: .zero, options: [.cursorUpdate, .inVisibleRect, .activeAlways], owner: self,
          userInfo: nil))
    }

    required init?(coder: NSCoder) { nil }

    deinit {
      if let observer = screenObserver { NotificationCenter.default.removeObserver(observer) }
    }

    /// The backing layer only hosts: nothing is drawn into it.
    override var wantsUpdateLayer: Bool { true }
    override func updateLayer() {}

    /// FlutterView's: the first click into an inactive window reaches the
    /// remote computer, here as over the rest of the viewer.
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override var acceptsFirstResponder: Bool { false }
    override var mouseDownCanMoveWindow: Bool { false }
    override func resetCursorRects() {}
    override func cursorUpdate(with event: NSEvent) {}

    override func viewDidMoveToWindow() {
      super.viewDidMoveToWindow()
      if let observer = screenObserver { NotificationCenter.default.removeObserver(observer) }
      screenObserver = nil
      if let window = window {
        // A window on another display may refresh differently.
        screenObserver = NotificationCenter.default.addObserver(
          forName: NSWindow.didChangeScreenNotification, object: window, queue: .main
        ) { [weak self] _ in
          guard let self = self else { return }
          self.viewer?.viewChanged(self)
        }
      }
      viewer?.viewChanged(self)
    }
  }

  /// The display link of a layer presenter's view: while the source streams
  /// and the view is on screen, it asks a variable-refresh display (a
  /// ProMotion panel) for its fastest rate, which the video commits no
  /// longer get from Flutter's own frames, and, while the stats ask for
  /// timings, has the viewer count the pictures committed within one
  /// refresh beyond the first, which no refresh showed. Before macOS 14,
  /// which has no NSView display link, a CVDisplayLink counts and nothing
  /// asks for a rate. Platform thread.
  private final class RefreshLink: NSObject {
    private weak var viewer: DesktopVideoViewer?
    /// The CADisplayLink (macOS 14), which holds this object as its target
    /// until stop invalidates it.
    private var link: AnyObject?
    private var cvLink: CVDisplayLink?

    init(viewer: DesktopVideoViewer) {
      self.viewer = viewer
      super.init()
    }

    /// A CVDisplayLink does not hold this object: it stops with it.
    deinit { stop() }

    var running: Bool { link != nil || cvLink != nil }

    /// Runs on [view]'s display, asking for [hz] if not nil.
    func start(_ view: NSView, askHz hz: Double?) {
      stop()
      if #available(macOS 14.0, *) {
        let link = view.displayLink(target: self, selector: #selector(refreshed(_:)))
        if let hz = hz {
          let rate = Float(hz)
          link.preferredFrameRateRange = CAFrameRateRange(
            minimum: min(60, rate), maximum: rate, preferred: rate)
        }
        link.add(to: .main, forMode: .common)
        self.link = link
        return
      }
      var created: CVDisplayLink?
      guard CVDisplayLinkCreateWithActiveCGDisplays(&created) == kCVReturnSuccess, let cvLink = created
      else { return }
      let number = view.window?.screen?.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")]
      if let display = (number as? NSNumber)?.uint32Value {
        CVDisplayLinkSetCurrentCGDisplay(cvLink, display)
      }
      // The callback runs on CoreVideo's thread; the count takes the
      // viewer's lock, nothing else.
      CVDisplayLinkSetOutputHandler(cvLink) { [weak viewer] _, _, _, _, _ in
        viewer?.refreshed()
        return kCVReturnSuccess
      }
      CVDisplayLinkStart(cvLink)
      self.cvLink = cvLink
    }

    func stop() {
      if #available(macOS 14.0, *), let link = link as? CADisplayLink { link.invalidate() }
      link = nil
      if let cvLink = cvLink { CVDisplayLinkStop(cvLink) }
      cvLink = nil
    }

    @objc private func refreshed(_ link: AnyObject) {
      viewer?.refreshed()
    }
  }
#endif

/// One viewer's picture. While a source is attached, its Feed thread decodes
/// each unit. With a texture the newest picture waits for the compositor: a
/// queue of one, so a picture the compositor did not take in time is
/// superseded, never queued. With a layer presenter (macOS) the Feed thread
/// commits each picture to the layer as it is decoded.
private final class DesktopVideoViewer: NSObject, FlutterTexture {
  private weak var textures: FlutterTextureRegistry?
  private let library: VideoLibrary
  /// Set by the plugin once the texture is registered, or, for a layer
  /// presenter, given out, before any attach.
  var textureId: Int64 = -1
  /// Platform thread only: signalled when the attached source's thread ends.
  private var worker: DispatchSemaphore?
  #if os(macOS)
    /// The layer presenter, when the viewer has one rather than a texture.
    let presenter: LayerPresenter?
    // Platform thread only: the view showing the presenter, the display
    // link on it, whether a source is attached and streams (not paused),
    // and whether the grey still is on.
    private weak var view: DesktopVideoView?
    private lazy var refreshLink = RefreshLink(viewer: self)
    private var attached = false
    private var streaming = false
    private var grey = false
  #endif
  private var layered: Bool {
    #if os(macOS)
      return presenter != nil
    #else
      return false
    #endif
  }

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
  private var hardware: Bool?
  /// Layer presenter, while timed and the display link runs: pictures
  /// committed since it last fired.
  private var countingRefreshes = false
  private var commitsSinceRefresh = 0
  // While a stats read asks for timings: from a picture's publication to
  // the compositor taking it (texture: `surface_consume`) or from its
  // decode to the end of its commit (layer: `present`), and
  // VTDecompressionSessionDecodeFrame's own time (key frames included).
  private var shownTiming: Timing?
  private var decodeTime: Timing?
  /// While timed: the CPU time of this texture's Feed threads, the running
  /// one's as it read it at the end of its last unit. Each attach starts a
  /// thread whose time adds to the ended ones', so the figure only grows
  /// and two reads across a reconnect still give the CPU spent between
  /// them. Gone when the timings stop.
  private var decoderCpuNs: UInt64?
  /// While timed: the CPU time of the Feed threads that ended.
  private var endedCpuNs: UInt64 = 0

  /// [layered]: a layer presenter (macOS) instead of a texture.
  init(textures: FlutterTextureRegistry?, library: VideoLibrary, layered: Bool) {
    self.textures = textures
    self.library = library
    #if os(macOS)
      presenter = layered ? LayerPresenter() : nil
    #endif
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
    let feed = Feed(viewer: self, library: library, consumer: consumer, layered: layered)
    let done = DispatchSemaphore(value: 0)
    let thread = Thread {
      feed.run()
      done.signal()
    }
    thread.name = "pb-desktop-video"
    thread.qualityOfService = .userInteractive
    worker = done
    thread.start()
    #if os(macOS)
      self.attached = true
      streaming = true
      updateRefreshLink()
    #endif
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
    #if os(macOS)
      attached = false
      streaming = false
      updateRefreshLink()
    #endif
  }

  func close() {
    detach()
    lock.lock()
    closed = true
    lock.unlock()
    #if os(macOS)
      if let presenter = presenter {
        refreshLink.stop()
        presenter.clear()
        return
      }
    #endif
    textures?.unregisterTexture(textureId)
  }

  /// [timings] starts the timing accumulators, or keeps them; false drops
  /// them, and the decoder threads' CPU time with them.
  func stats(timings: Bool) -> [String: Any] {
    lock.lock()
    let wasTimed = decodeTime != nil
    if !timings {
      shownTiming = nil
      decodeTime = nil
      decoderCpuNs = nil
      endedCpuNs = 0
      commitsSinceRefresh = 0
    } else if decodeTime == nil {
      shownTiming = Timing()
      decodeTime = Timing()
    }
    var values: [String: Any] = [
      "submitted": submitted, "decoded": decoded, "presented": presented, "dropped": dropped,
      "configurations": configurations, "recoveries": recoveries, "lastRecovery": lastRecovery,
      "decoder": decoderName, "lowLatency": lowLatency,
    ]
    if let hardware = hardware { values["hardware"] = hardware }
    if let shown = shownTiming, let decode = decodeTime {
      values["timings"] = [layered ? "present" : "surface_consume": shown.snapshot, "decode": decode.snapshot]
    }
    if let cpu = decoderCpuNs { values["decoderCpuMs"] = Double(cpu) / 1_000_000 }
    lock.unlock()
    #if os(macOS)
      if timings != wasTimed { updateRefreshLink() }
    #endif
    return values
  }

  #if os(macOS)
    /// The view Flutter made for this viewer now shows the presenter's
    /// layers, taken from any view that showed them before.
    func host(in view: DesktopVideoView) {
      guard let presenter = presenter, let layer = view.layer else { return }
      self.view = view
      presenter.host(in: layer)
      updateRefreshLink()
    }

    /// The hosting view entered or left a window, or its window another
    /// display.
    func viewChanged(_ view: DesktopVideoView) {
      if view === self.view { updateRefreshLink() }
    }

    /// The Feed thread of [consumer] saw its source pause or resume; a word
    /// from a source detached since changes nothing.
    func setStreaming(_ on: Bool, consumer: UInt64) {
      lock.lock()
      let current = consumer == self.consumer
      lock.unlock()
      guard current, attached, streaming != on else { return }
      streaming = on
      updateRefreshLink()
    }

    /// The grey still on the picture, which no Flutter filter reaches.
    func setGrey(_ on: Bool) {
      guard let presenter = presenter, grey != on else { return }
      grey = on
      presenter.grey(on)
    }

    /// The display link runs while a source streams to a view on screen, on
    /// a display whose refresh varies (which it asks for its fastest rate)
    /// or while timings count the pictures no refresh showed. A
    /// fixed-refresh display without timings runs none.
    private func updateRefreshLink() {
      guard presenter != nil else { return }
      lock.lock()
      let timed = decodeTime != nil
      lock.unlock()
      var askHz: Double?
      var run = false
      if attached, streaming, let view = view, let screen = view.window?.screen {
        if #available(macOS 12.0, *), screen.minimumRefreshInterval < screen.maximumRefreshInterval {
          askHz = Double(screen.maximumFramesPerSecond)
        }
        run = timed || askHz != nil
        if run { refreshLink.start(view, askHz: askHz) }
      }
      if !run { refreshLink.stop() }
      lock.lock()
      countingRefreshes = run && refreshLink.running && decodeTime != nil
      commitsSinceRefresh = 0
      lock.unlock()
    }
  #endif

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
      if let timing = shownTiming, picture.readyNs != 0 {
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

  /// A unit goes to the decoder: the revision its picture belongs to, and
  /// whether a stats read asked for timings.
  func submit() -> (revision: UInt64, timed: Bool) {
    lock.lock()
    defer { lock.unlock() }
    submitted += 1
    return (revision, decodeTime != nil)
  }

  /// A timed unit is done: its decode time, when it reached the decoder,
  /// and the Feed thread's CPU time so far. Timings stopped meanwhile keep
  /// neither.
  func measured(decodeNs: UInt64?, threadCpuNs: UInt64) {
    lock.lock()
    if let timing = decodeTime {
      if let ns = decodeNs { timing.add(ns) }
      decoderCpuNs = endedCpuNs &+ threadCpuNs
    }
    lock.unlock()
  }

  /// The Feed thread, as it ends: while timed, its whole CPU time joins the
  /// ended threads', read here because only the thread itself can read it.
  /// Detach waits for this, so the next thread adds to it.
  func feedEnded() {
    lock.lock()
    if decodeTime != nil {
      endedCpuNs &+= clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID)
      decoderCpuNs = endedCpuNs
    }
    lock.unlock()
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
    hardware = decoder.hardware
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
  /// Flutter at most once per frame it has not handled yet. A layer
  /// presenter shows it at once instead.
  func publish(_ decodedPicture: Picture) throws {
    #if os(macOS)
      if let presenter = presenter {
        try present(decodedPicture, presenter)
        return
      }
    #endif
    var picture = decodedPicture
    lock.lock()
    decoded += 1
    guard picture.revision == revision else {
      lock.unlock()
      return
    }
    if let previous = latest, !latestTaken, previous.revision == revision { dropped += 1 }
    if shownTiming != nil { picture.readyNs = DispatchTime.now().uptimeNanoseconds }
    latest = picture
    latestTaken = false
    let post = !framePosted && !closed
    if post { framePosted = true }
    lock.unlock()
    if post { DispatchQueue.main.async { [weak self] in self?.frameAvailable() } }
  }

  #if os(macOS)
    /// Commits a picture of the current revision to the layer, outside the
    /// lock: the commit is the presentation receipt, once per picture, as
    /// the compositor taking it is a texture's. A picture of an older
    /// revision is not shown. While timed, `present` runs from the end of
    /// its decode ([Picture.readyNs]) to the end of its commit.
    private func present(_ picture: Picture, _ presenter: LayerPresenter) throws {
      lock.lock()
      decoded += 1
      let current = picture.revision == revision
      lock.unlock()
      guard current else { return }
      guard presenter.commit(picture.buffer) else {
        throw VideoError("The decoded picture has no IOSurface")
      }
      lock.lock()
      shown = picture.buffer
      var acknowledge = false
      if picture.revision == revision
        && (consumedRevision != picture.revision || consumedFrame != picture.frame)
      {
        consumedRevision = picture.revision
        consumedFrame = picture.frame
        acknowledge = true
        presented += 1
        if countingRefreshes { commitsSinceRefresh += 1 }
        if let timing = shownTiming, picture.readyNs != 0 {
          let now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
          timing.add(now > picture.readyNs ? now - picture.readyNs : 0)
        }
      }
      let consumer = self.consumer
      lock.unlock()
      // Committed to the WindowServer: the receipt Rust validates and
      // forwards for input authorisation. Non-blocking.
      if acknowledge && consumer != 0 {
        _ = library.presented(consumer, picture.generation, picture.frame)
      }
    }

    /// Thread of the display link: each refresh, the pictures committed
    /// since the one before beyond the first were superseded before any
    /// composite showed them. Counted only while timed.
    func refreshed() {
      lock.lock()
      if countingRefreshes, commitsSinceRefresh > 1 { dropped += commitsSinceRefresh - 1 }
      commitsSinceRefresh = 0
      lock.unlock()
    }
  #endif
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
  /// Decodes for a layer presenter: 420v tagged sRGB, each picture stamped
  /// at its decode's end while timed.
  private let layered: Bool

  init(viewer: DesktopVideoViewer, library: VideoLibrary, consumer: UInt64, layered: Bool) {
    self.viewer = viewer
    self.library = library
    self.consumer = consumer
    self.layered = layered
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
        setPaused(false)
        autoreleasepool { accept(buffer, UnitMeta(meta)) }
      } else if result == videoNeedCapacity {
        // The unit stays queued in Rust and arrives on the next call.
        let needed = Int(meta.load(fromByteOffset: 48, as: UInt32.self))
        if needed > maxUnitBytes { break }
        buffer.deallocate()
        capacity = needed
        buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: capacity)
      } else if result == videoPaused {
        setPaused(true)
      } else if result != videoTimeout {
        break  // Closed, detached or invalid: this consumer is done.
      }
    }
    library.detach(consumer)
    decoder?.close()
    decoder = nil
    viewer.feedEnded()
  }

  /// A layer presenter's display link runs only while the source streams:
  /// the platform thread hears of each pause and resume.
  private func setPaused(_ now: Bool) {
    guard paused != now else { return }
    paused = now
    #if os(macOS)
      if layered {
        let consumer = self.consumer
        DispatchQueue.main.async { [weak viewer] in viewer?.setStreaming(!now, consumer: consumer) }
      }
    #endif
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
    let (revision, timed) = viewer.submit()
    var decodeNs: UInt64?
    // While timed, once the unit is done, failed or not: what its decode
    // took and this thread's CPU time so far.
    defer {
      if timed {
        viewer.measured(decodeNs: decodeNs, threadCpuNs: clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID))
      }
    }
    do {
      decodeNs = try decode(base, unit, revision, timed: timed)
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
  /// at a key frame, or when its parameter sets changed. When [timed], what
  /// the decode took, in nanoseconds.
  private func decode(_ base: UnsafeMutablePointer<UInt8>, _ unit: UnitMeta, _ revision: UInt64, timed: Bool)
    throws -> UInt64?
  {
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
    guard let sample = bitstream.assemble(base, codec), let running = decoder else {
      viewer.drop()
      return nil
    }
    var decoded: (image: CVPixelBuffer?, ns: UInt64?)
    do {
      decoded = try running.decode(sample.bytes, sample.length, timed: timed)
    } catch is DecoderLost where unit.key {
      // Lost while the app was away, and this key frame starts over: a new
      // session takes it at once.
      try configure(unit)
      decoded = try decoder?.decode(sample.bytes, sample.length, timed: timed) ?? (nil, nil)
    }
    // A layer presenter's `present` starts here, at the decode's end.
    let decodedNs = timed && layered ? clock_gettime_nsec_np(CLOCK_UPTIME_RAW) : 0
    guard let picture = decoded.image else {
      viewer.drop()
      return decoded.ns
    }
    guard CVPixelBufferGetWidth(picture) == Int(unit.width),
      CVPixelBufferGetHeight(picture) == Int(unit.height)
    else {
      throw VideoError("Decoded picture does not match the authorized frame")
    }
    try viewer.publish(
      Picture(
        buffer: picture, generation: unit.generation, frame: unit.frame, revision: revision,
        readyNs: decodedNs))
    return decoded.ns
  }

  private func configure(_ unit: UnitMeta) throws {
    decoder?.close()
    decoder = nil
    let next = try VideoDecoder(
      codec: unit.codec, width: unit.width, height: unit.height,
      format: bitstream.describe(unit.codec), layered: layered)
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

  #if os(macOS)
    /// Platform thread: the fastest refresh of the display [window] is on,
    /// where the viewer presents; the fastest display's while the window is
    /// on none (hidden, or no window yet).
    static func windowHz(_ window: NSWindow?) -> Double {
      guard let screen = window?.screen else { return displayMaxHz() }
      if #available(macOS 12.0, *) {
        let hz = Double(screen.maximumFramesPerSecond)
        return hz > 1 ? hz : displayMaxHz()
      }
      let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber
      guard let display = number?.uint32Value, let mode = CGDisplayCopyDisplayMode(display) else {
        return displayMaxHz()
      }
      // A built-in panel reports 0 here: it is taken as 60, as above.
      return mode.refreshRate > 1 ? mode.refreshRate : 60
    }
  #endif
}

/// The `pb_mapper/desktop_video` channel: probes the decoders, and creates,
/// attaches, snapshots, reads, greys and disposes textures; on macOS also the
/// factory of the platform view that shows a layer presenter. Platform
/// thread only; what runs elsewhere replies through the main queue.
final class DesktopVideoPlugin: NSObject, FlutterPlugin {
  private weak var textures: FlutterTextureRegistry?
  /// Resolved by the first create: Dart has loaded the library by then.
  private var library: VideoLibrary?
  private var viewers: [Int64: DesktopVideoViewer] = [:]
  #if os(macOS)
    /// Its `view` is the Flutter view, whose window's display each stats
    /// read takes the refresh from: the window can move. It holds the
    /// engine only weakly, so holding it here makes no cycle.
    private var registrar: FlutterPluginRegistrar?
    /// The next id a layer presenter gets. Texture ids are the addresses
    /// of Flutter's texture wrappers, never this small.
    private var nextLayerId: Int64 = 1
  #endif

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
    let plugin = DesktopVideoPlugin(textures: textures)
    #if os(macOS)
      plugin.registrar = registrar
      registrar.register(plugin, withId: viewType)
    #endif
    let channel = FlutterMethodChannel(name: channelName, binaryMessenger: messenger)
    registrar.addMethodCallDelegate(plugin, channel: channel)
  }

  /// The stats' refresh: on macOS the viewer window's display's, on iOS the
  /// device's.
  private var refreshHz: Double {
    #if os(macOS)
      return DecoderProbe.windowHz(registrar?.view?.window)
    #else
      return DecoderProbe.displayMaxHz()
    #endif
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
      let layered = try presenter(args)
      try checkInstance(UInt64(bitPattern: number(args, "instance")))
      guard viewers.count < textureLimit else { throw VideoError("Desktop texture limit reached") }
      guard let textures = textures, let library = library else {
        throw VideoError("Desktop video is not set up")
      }
      let viewer = DesktopVideoViewer(textures: textures, library: library, layered: layered)
      let id: Int64
      #if os(macOS)
        if layered {
          while viewers[nextLayerId] != nil { nextLayerId += 1 }
          id = nextLayerId
          nextLayerId += 1
        } else {
          id = textures.register(viewer)
        }
      #else
        id = textures.register(viewer)
      #endif
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
      stats["refreshHz"] = refreshHz
      result(stats)
    case "grey":
      guard let on = args["on"] as? Bool else { throw VideoError("Missing video field on") }
      #if os(macOS)
        viewer.setGrey(on)
      #endif
      result(nil)
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

  /// `create`'s `presenter`: `layer` (macOS only) or `texture`, the default.
  private func presenter(_ args: [String: Any]) throws -> Bool {
    switch args["presenter"] {
    case nil, is NSNull: return false
    case let name as String where name == "texture": return false
    #if os(macOS)
      case let name as String where name == "layer": return true
    #endif
    default: throw VideoError("Unknown desktop video presenter")
    }
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

#if os(macOS)
  /// `pb_mapper/desktop_video_view`, made with `{textureId}`: the view that
  /// shows that viewer's layer presenter. A view for a viewer gone, or one
  /// with a texture, shows nothing.
  extension DesktopVideoPlugin: FlutterPlatformViewFactory {
    func create(withViewIdentifier viewId: Int64, arguments args: Any?) -> NSView {
      let id = ((args as? [String: Any])?["textureId"] as? NSNumber)?.int64Value
      let viewer = id.flatMap { viewers[$0] }
      let view = DesktopVideoView(viewer: viewer)
      viewer?.host(in: view)
      let hosting = viewer?.presenter != nil
      logVideo("Video view \(viewId) for \(id.map(String.init) ?? "no id"): \(hosting ? "its layer" : "empty")")
      return view
    }

    func createArgsCodec() -> (NSObjectProtocol & FlutterMessageCodec)? {
      FlutterStandardMessageCodec.sharedInstance()
    }
  }
#endif
