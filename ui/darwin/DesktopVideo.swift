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
// - texture (iOS, and macOS unless Dart asks for a layer): IOSurface-backed
//   BGRA pixel buffers, the newest kept for Flutter, whose compositor takes
//   it in copyPixelBuffer: the presentation receipt Rust validates.
// - layer (macOS, when Dart asks for it): IOSurface-backed 420v pixel
//   buffers, which the decoder thread commits to the viewer's own Core
//   Animation layer as each is decoded, with no BGRA pass and no Flutter
//   frame. The commit is the receipt. Flutter shows the layer through the
//   platform view `pb_mapper/desktop_video_view`. While Dart asks a layer
//   presenter to `enhance` a picture shown enlarged, a presenter thread
//   upscales and sharpens each picture on the GPU into a CAMetalLayer
//   beside that layer instead; its present is the receipt.
// Nothing of the video crosses Dart.

import CoreMedia
import CoreVideo
import Foundation
import ImageIO
import QuartzCore
import VideoToolbox
import os

#if os(iOS)
  import Flutter
  import UIKit
#elseif os(macOS)
  import AppKit
  import FlutterMacOS
  import Metal
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

/// Instruments' points of interest, by picture: `Decoded` as a picture
/// leaves the decoder, `Commit` around a layer presenter's commit (or its
/// enhancement's present), `Enhance` around the enhancement encoding its
/// GPU work until the GPU scheduled it, and `Consumed` when Flutter's
/// compositor takes a texture's picture, each with
/// the picture's generation and frame. Animation Hitches with the Points of
/// Interest instrument added records them in one trace with the display's
/// frames. Points of interest go to the system log whether or not a trace
/// records them, so they are marked only while the stats ask for timings,
/// as the acceptance tool and the quality panel do.
private enum Signpost {
  static let log = OSLog(subsystem: "pb-mapper.desktop-video", category: .pointsOfInterest)

  static func event(_ name: StaticString, _ generation: UInt64, _ frame: UInt64) {
    guard log.signpostsEnabled else { return }
    os_signpost(.event, log: log, name: name, "generation %llu frame %llu", generation, frame)
  }

  /// An interval's start, nil when nothing records it.
  static func begin(_ name: StaticString, _ generation: UInt64, _ frame: UInt64) -> OSSignpostID? {
    guard log.signpostsEnabled else { return nil }
    let id = OSSignpostID(log: log)
    os_signpost(
      .begin, log: log, name: name, signpostID: id, "generation %llu frame %llu", generation, frame)
    return id
  }

  static func end(_ name: StaticString, _ id: OSSignpostID?) {
    guard let id = id else { return }
    os_signpost(.end, log: log, name: name, signpostID: id)
  }
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
  /// Before macOS 12, which has no CVBufferCopyAttachment, the call it
  /// replaced reads it.
  private func carriesSrgb(_ buffer: CVPixelBuffer) -> Bool {
    let transfer: CFTypeRef?
    if #available(macOS 12.0, *) {
      transfer = CVBufferCopyAttachment(buffer, kCVImageBufferTransferFunctionKey, nil)
    } else {
      transfer = CVBufferGetAttachment(buffer, kCVImageBufferTransferFunctionKey, nil)?
        .takeUnretainedValue()
    }
    guard let tag = transfer else { return false }
    return CFEqual(tag, kCVImageBufferTransferFunction_sRGB)
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

  /// Whether [other] is this picture: the same unit of the same revision.
  /// Not the buffer, which VideoToolbox's pool hands out again.
  func same(as other: Picture?) -> Bool {
    guard let other = other else { return false }
    return generation == other.generation && frame == other.frame && revision == other.revision
  }
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

/// `enhance`'s `level`: how much a layer presenter sharpens a picture it
/// shows enlarged (the viewer's "Sharpen enlarged picture"). Off, it does
/// nothing at all.
private enum EnhanceLevel: String {
  case off, low, medium, high

  /// AMD FidelityFX CAS's peak, as airhand weighs it: how far a pixel moves
  /// from its four neighbours' mean where they hardly differ. CAS's own
  /// range, -0.125 to -0.2, is too strong for compressed video.
  var peak: Float {
    switch self {
    case .off: return 0
    case .low: return -0.06
    case .medium: return -0.1
    case .high: return -0.14
    }
  }
}

/// A size in pixels.
private struct PixelSize: Equatable {
  let width: Int
  let height: Int
}

#if os(macOS)
  /// The largest drawable the enhancement asks for, a Metal texture's
  /// limit on every Mac GPU that runs this app.
  private let maxDrawable = 16384

  /// What the enhancement draws: the picture's size on screen, in pixels,
  /// and the level.
  private struct EnhanceTarget: Equatable {
    let size: PixelSize
    let level: EnhanceLevel
  }

  /// Where the enhancement's layer goes over [layer], the view's own layer,
  /// which [container] fills as its sublayer: [layer]'s rect in the window's
  /// root layer [root], each edge moved to the nearest edge between device
  /// pixels at [scale], as a frame in [container], and that rect's size in
  /// pixels, which the drawable takes. Flutter places the view at fractional
  /// pixel offsets and sizes; a drawable on whole pixels at exactly its size
  /// reaches the screen one texel a pixel, so the WindowServer does not
  /// filter it again, which would blur what the enhancement sharpened. The
  /// window's backing, and so its root layer's bounds, lies on whole pixels.
  /// Nil while [container] is not [layer]'s, or for less than a pixel.
  private func pixelPlacement(of layer: CALayer, in container: CALayer, root: CALayer, scale: CGFloat)
    -> (size: PixelSize, frame: CGRect)?
  {
    guard container.superlayer === layer, scale.isFinite, scale > 0 else { return nil }
    let rect = layer.convert(layer.bounds, to: root)
    let origin = root.bounds.origin
    // Rounded alike at both edges, so a whole-pixel extent stays whole.
    func edge(_ points: CGFloat, _ from: CGFloat) -> CGFloat? {
      let pixels = ((points - from) * scale + 0.5).rounded(.down)
      return pixels.isFinite && abs(pixels) <= 1_000_000 ? pixels : nil
    }
    guard let left = edge(rect.minX, origin.x), let right = edge(rect.maxX, origin.x),
      let low = edge(rect.minY, origin.y), let high = edge(rect.maxY, origin.y), right - left >= 1,
      high - low >= 1
    else { return nil }
    let aligned = CGRect(
      x: origin.x + left / scale, y: origin.y + low / scale, width: (right - left) / scale,
      height: (high - low) / scale)
    let size = PixelSize(width: Int(right - left), height: Int(high - low))
    return (size, container.convert(aligned, from: root))
  }

  /// The shaders' constants (`Params` in [enhanceShaders]): four float4s,
  /// laid out alike in Swift and in the Metal Shading Language.
  private struct EnhanceParams {
    /// xy: the picture's size; zw: picture pixels per output pixel.
    var size: SIMD4<Float>
    /// x: video-range luma's scale (255/219), y: its offset (16/255),
    /// z: chroma's zero (128/255), w: the sharpening peak.
    var range: SIMD4<Float>
    /// Red from Cr, green from Cb, green from Cr, blue from Cb, video range.
    var chroma: SIMD4<Float>
    /// xy: the output's size in pixels.
    var extent: SIMD4<Float>
  }

  /// Video-range chroma's weights for the matrix with luma coefficients
  /// [kr] and [kb].
  private func chromaWeights(kr: Float, kb: Float) -> SIMD4<Float> {
    let kg = 1 - kr - kb
    let scale: Float = 255.0 / 224.0
    return SIMD4(2 * (1 - kr), 2 * kb * (1 - kb) / kg, 2 * kr * (1 - kr) / kg, 2 * (1 - kb)) * scale
  }

  private let bt709Weights = chromaWeights(kr: 0.2126, kb: 0.0722)
  private let bt601Weights = chromaWeights(kr: 0.299, kb: 0.114)

  /// The weights of the matrix [buffer] is tagged with: BT.601 where it says
  /// so, BT.709 otherwise, as the layer presenter tags its pictures.
  private func matrixWeights(_ buffer: CVPixelBuffer) -> SIMD4<Float> {
    let matrix: CFTypeRef?
    if #available(macOS 12.0, *) {
      matrix = CVBufferCopyAttachment(buffer, kCVImageBufferYCbCrMatrixKey, nil)
    } else {
      matrix = CVBufferGetAttachment(buffer, kCVImageBufferYCbCrMatrixKey, nil)?.takeUnretainedValue()
    }
    if let tag = matrix, CFEqual(tag, kCVImageBufferYCbCrMatrix_ITU_R_601_4) { return bt601Weights }
    return bt709Weights
  }

  /// The enhancement's shaders, after airhand's (client/src/lib/enhance.ts),
  /// compiled at run time so that neither runner project carries a .metal
  /// resource. Texture and render target coordinates alike run y down from
  /// the picture's top left.
  ///
  /// Pass 1, while the picture is enlarged, reads the decoder's 420v planes
  /// and writes the picture at its size on screen: luma by Catmull-Rom, from
  /// nine bilinear taps, kept within its four nearest texels so that it
  /// never rings beside text; chroma bilinear; then RGB through the stream's
  /// matrix with no transfer conversion, which the sRGB layer shows as the
  /// plain layer's colours. The luma goes in alpha for pass 2.
  ///
  /// Pass 2 sharpens luma alone, as AMD FidelityFX CAS does: a pixel moves
  /// toward or away from its four neighbours' mean by the peak, less where
  /// its 3x3 already has contrast, which limits overshoot beside an edge but
  /// does not prevent it. Chroma, which 4:2:0 carries at half the
  /// resolution, is left alone: sharpening it would only widen colour
  /// fringes. At 1:1 it runs alone, on the planes.
  private let enhanceShaders = """
    #include <metal_stdlib>
    using namespace metal;

    struct Params {
      float4 size;
      float4 range;
      float4 chroma;
      float4 extent;
    };

    struct Cover {
      float4 position [[position]];
    };

    constexpr sampler bilinear(coord::normalized, address::clamp_to_edge, filter::linear);

    // One triangle over the whole target.
    vertex Cover cover(uint id [[vertex_id]]) {
      float2 p = float2(float((id << 1) & 2), float(id & 2));
      Cover out;
      out.position = float4(p * 2.0 - 1.0, 0.0, 1.0);
      return out;
    }

    static float lumaOf(float y, constant Params &p) {
      return saturate((y - p.range.y) * p.range.x);
    }

    static float3 rgbOf(float y, float2 c, constant Params &p) {
      float l = (y - p.range.y) * p.range.x;
      float cb = c.x - p.range.z;
      float cr = c.y - p.range.z;
      return saturate(float3(l + p.chroma.x * cr, l - p.chroma.y * cb - p.chroma.z * cr,
                             l + p.chroma.w * cb));
    }

    fragment float4 upscale(Cover in [[stage_in]], texture2d<float> luma [[texture(0)]],
                            texture2d<float> chroma [[texture(1)]],
                            constant Params &p [[buffer(0)]]) {
      float2 size = p.size.xy;
      float2 pos = in.position.xy * p.size.zw;
      float2 t1 = floor(pos - 0.5) + 0.5;
      float2 f = pos - t1;
      float2 w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
      float2 w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
      float2 w2 = f * (0.5 + f * (2.0 - 1.5 * f));
      float2 w3 = f * f * (-0.5 + 0.5 * f);
      float2 w12 = w1 + w2;
      float2 t0 = (t1 - 1.0) / size;
      float2 t3 = (t1 + 2.0) / size;
      float2 t12 = (t1 + w2 / w12) / size;
      float y =
          (luma.sample(bilinear, float2(t0.x, t0.y)).r * w0.x
           + luma.sample(bilinear, float2(t12.x, t0.y)).r * w12.x
           + luma.sample(bilinear, float2(t3.x, t0.y)).r * w3.x) * w0.y
          + (luma.sample(bilinear, float2(t0.x, t12.y)).r * w0.x
             + luma.sample(bilinear, float2(t12.x, t12.y)).r * w12.x
             + luma.sample(bilinear, float2(t3.x, t12.y)).r * w3.x) * w12.y
          + (luma.sample(bilinear, float2(t0.x, t3.y)).r * w0.x
             + luma.sample(bilinear, float2(t12.x, t3.y)).r * w12.x
             + luma.sample(bilinear, float2(t3.x, t3.y)).r * w3.x) * w3.y;
      // The four texels nearest, around t1 + 0.5: no ringing.
      float4 near = luma.gather(bilinear, (t1 + 0.5) / size);
      y = clamp(y, min(min(near.x, near.y), min(near.z, near.w)),
                max(max(near.x, near.y), max(near.z, near.w)));
      float2 c = chroma.sample(bilinear, pos / size).rg;
      return float4(rgbOf(y, c, p), lumaOf(y, p));
    }

    static float4 cas(float3 rgb, float e, float a, float b, float c, float d, float f, float g,
                      float h, float i, float peak) {
      float mn = min(min(min(d, e), min(f, b)), h);
      mn += min(mn, min(min(a, c), min(g, i)));
      float mx = max(max(max(d, e), max(f, b)), h);
      mx += max(mx, max(max(a, c), max(g, i)));
      float amp = sqrt(saturate(min(mn, 2.0 - mx) / max(mx, 1e-5)));
      float w = amp * peak;
      float l = (e + w * (b + d + f + h)) / (1.0 + 4.0 * w);
      return float4(saturate(rgb + (l - e)), 1.0);
    }

    static float alphaAt(texture2d<float> image, int2 q, int2 m) {
      return image.read(uint2(clamp(q, int2(0), m))).a;
    }

    fragment float4 sharpenImage(Cover in [[stage_in]], texture2d<float> image [[texture(0)]],
                                 constant Params &p [[buffer(0)]]) {
      int2 m = int2(p.extent.xy) - 1;
      int2 q = int2(in.position.xy);
      float4 center = image.read(uint2(clamp(q, int2(0), m)));
      return cas(center.rgb, center.a, alphaAt(image, q + int2(-1, -1), m),
                 alphaAt(image, q + int2(0, -1), m), alphaAt(image, q + int2(1, -1), m),
                 alphaAt(image, q + int2(-1, 0), m), alphaAt(image, q + int2(1, 0), m),
                 alphaAt(image, q + int2(-1, 1), m), alphaAt(image, q + int2(0, 1), m),
                 alphaAt(image, q + int2(1, 1), m), p.range.w);
    }

    static float lumaAt(texture2d<float> luma, int2 q, int2 m, constant Params &p) {
      return lumaOf(luma.read(uint2(clamp(q, int2(0), m))).r, p);
    }

    fragment float4 sharpenPlanes(Cover in [[stage_in]], texture2d<float> luma [[texture(0)]],
                                  texture2d<float> chroma [[texture(1)]],
                                  constant Params &p [[buffer(0)]]) {
      int2 m = int2(p.size.xy) - 1;
      int2 q = int2(in.position.xy);
      float y = luma.read(uint2(clamp(q, int2(0), m))).r;
      float2 c = chroma.sample(bilinear, in.position.xy / p.size.xy).rg;
      return cas(rgbOf(y, c, p), lumaOf(y, p), lumaAt(luma, q + int2(-1, -1), m, p),
                 lumaAt(luma, q + int2(0, -1), m, p), lumaAt(luma, q + int2(1, -1), m, p),
                 lumaAt(luma, q + int2(-1, 0), m, p), lumaAt(luma, q + int2(1, 0), m, p),
                 lumaAt(luma, q + int2(-1, 1), m, p), lumaAt(luma, q + int2(0, 1), m, p),
                 lumaAt(luma, q + int2(1, 1), m, p), p.range.w);
    }
    """

  /// The enhancement's GPU work, on its presenter thread only: the shaders'
  /// pipelines, the texture cache that wraps a decoded picture's planes
  /// without a copy, and pass 1's picture.
  private final class EnhanceGpu {
    enum Drawn {
      case presented
      case failed(String)
    }

    private let device: MTLDevice
    private let queue: MTLCommandQueue
    private let upscale: MTLRenderPipelineState
    private let sharpenImage: MTLRenderPipelineState
    private let sharpenPlanes: MTLRenderPipelineState
    private let cache: CVMetalTextureCache
    /// Pass 1's picture, at least the size on screen. It only grows while
    /// the enhancement runs, so that a resize or a pinch does not allocate
    /// at every step, and goes when it idles.
    private var image: MTLTexture?

    /// Compiles the shaders: tens of milliseconds, never on the platform
    /// thread.
    init(device: MTLDevice) throws {
      self.device = device
      guard let queue = device.makeCommandQueue() else { throw VideoError("No Metal command queue") }
      self.queue = queue
      let library: MTLLibrary
      do {
        library = try device.makeLibrary(source: enhanceShaders, options: nil)
      } catch {
        throw VideoError("Compile the enhancement shaders: \(error.localizedDescription)")
      }
      func pipeline(_ fragment: String, _ format: MTLPixelFormat) throws -> MTLRenderPipelineState {
        guard let vertex = library.makeFunction(name: "cover"),
          let shader = library.makeFunction(name: fragment)
        else { throw VideoError("The enhancement shaders have no \(fragment)") }
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.vertexFunction = vertex
        descriptor.fragmentFunction = shader
        descriptor.colorAttachments[0].pixelFormat = format
        do {
          return try device.makeRenderPipelineState(descriptor: descriptor)
        } catch {
          throw VideoError("Build the \(fragment) pipeline: \(error.localizedDescription)")
        }
      }
      upscale = try pipeline("upscale", .rgba8Unorm)
      sharpenImage = try pipeline("sharpenImage", .bgra8Unorm)
      sharpenPlanes = try pipeline("sharpenPlanes", .bgra8Unorm)
      var made: CVMetalTextureCache?
      guard CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, device, nil, &made) == kCVReturnSuccess,
        let cache = made
      else { throw VideoError("Create the Metal texture cache") }
      self.cache = cache
    }

    /// Nothing to draw for now: pass 1's picture goes.
    func idle() { image = nil }

    /// [layer]'s next drawable at [size], nil if none came within
    /// nextDrawable's second. It waits while every drawable is still on
    /// screen or queued: this thread's wait, never the decoder's, and before
    /// the picture is chosen, so that the one drawn is the newest.
    func drawable(_ size: PixelSize, in layer: CAMetalLayer) -> CAMetalDrawable? {
      let pixels = CGSize(width: size.width, height: size.height)
      if layer.drawableSize != pixels {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        layer.drawableSize = pixels
        CATransaction.commit()
      }
      return layer.nextDrawable()
    }

    /// [picture] at [target] in [drawable], one of [layer]'s at the
    /// target's size, presented in an explicit transaction ([layer]
    /// presents with transactions) once the GPU has scheduled the work.
    /// [completed] hears, on Metal's thread, whether the GPU finished it.
    /// While [timed], `Enhance` marks the GPU work's encoding and scheduling
    /// and `Commit` the present.
    func draw(
      _ picture: Picture, _ target: EnhanceTarget, into drawable: CAMetalDrawable, timed: Bool,
      completed: @escaping (Bool) -> Void
    ) -> Drawn {
      let buffer = picture.buffer
      guard CVPixelBufferGetPixelFormatType(buffer) == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        let lumaPlane = wrap(buffer, plane: 0, .r8Unorm),
        let chromaPlane = wrap(buffer, plane: 1, .rg8Unorm),
        let luma = CVMetalTextureGetTexture(lumaPlane),
        let chroma = CVMetalTextureGetTexture(chromaPlane)
      else { return .failed("Metal cannot read the decoded picture") }
      let width = luma.width
      let height = luma.height
      let out = target.size
      let span = timed ? Signpost.begin("Enhance", picture.generation, picture.frame) : nil
      guard let commands = queue.makeCommandBuffer() else {
        Signpost.end("Enhance", span)
        return .failed("No Metal command buffer")
      }
      var params = EnhanceParams(
        size: SIMD4(Float(width), Float(height), Float(width) / Float(out.width), Float(height) / Float(out.height)),
        range: SIMD4(255.0 / 219.0, 16.0 / 255.0, 128.0 / 255.0, target.level.peak),
        chroma: matrixWeights(buffer),
        extent: SIMD4(Float(out.width), Float(out.height), 0, 0))
      let encoded: Bool
      if out.width == width && out.height == height {
        encoded = pass(commands, sharpenPlanes, into: drawable.texture, out, luma, chroma, &params)
      } else if let image = image(for: out) {
        encoded =
          pass(commands, upscale, into: image, out, luma, chroma, &params)
          && pass(commands, sharpenImage, into: drawable.texture, out, image, nil, &params)
      } else {
        encoded = false
      }
      // The planes stay wrapped until the GPU is done with them.
      commands.addCompletedHandler { done in
        withExtendedLifetime((lumaPlane, chromaPlane)) {}
        completed(done.status == .completed)
      }
      // A pass not encoded leaves the drawable as it was: committed all
      // the same, so that it goes back to the layer, but not presented.
      commands.commit()
      guard encoded else {
        Signpost.end("Enhance", span)
        return .failed("Encode the enhancement")
      }
      commands.waitUntilScheduled()
      Signpost.end("Enhance", span)
      let commit = timed ? Signpost.begin("Commit", picture.generation, picture.frame) : nil
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      drawable.present()
      CATransaction.commit()
      Signpost.end("Commit", commit)
      CVMetalTextureCacheFlush(cache, 0)
      return .presented
    }

    private func wrap(_ buffer: CVPixelBuffer, plane: Int, _ format: MTLPixelFormat) -> CVMetalTexture? {
      var made: CVMetalTexture?
      let status = CVMetalTextureCacheCreateTextureFromImage(
        kCFAllocatorDefault, cache, buffer, nil, format, CVPixelBufferGetWidthOfPlane(buffer, plane),
        CVPixelBufferGetHeightOfPlane(buffer, plane), plane, &made)
      return status == kCVReturnSuccess ? made : nil
    }

    /// Pass 1's picture for [size], grown as needed, in 256-pixel steps.
    private func image(for size: PixelSize) -> MTLTexture? {
      if let image = image, image.width >= size.width, image.height >= size.height { return image }
      let width = min(maxDrawable, max(image?.width ?? 0, (size.width + 255) & ~255))
      let height = min(maxDrawable, max(image?.height ?? 0, (size.height + 255) & ~255))
      let descriptor = MTLTextureDescriptor.texture2DDescriptor(
        pixelFormat: .rgba8Unorm, width: width, height: height, mipmapped: false)
      descriptor.usage = [.renderTarget, .shaderRead]
      descriptor.storageMode = .private
      image = device.makeTexture(descriptor: descriptor)
      return image
    }

    /// One full-target triangle of [pipeline] into [size] of [target].
    private func pass(
      _ commands: MTLCommandBuffer, _ pipeline: MTLRenderPipelineState, into target: MTLTexture,
      _ size: PixelSize, _ first: MTLTexture, _ second: MTLTexture?, _ params: inout EnhanceParams
    ) -> Bool {
      let descriptor = MTLRenderPassDescriptor()
      descriptor.colorAttachments[0].texture = target
      descriptor.colorAttachments[0].loadAction = .dontCare
      descriptor.colorAttachments[0].storeAction = .store
      guard let encoder = commands.makeRenderCommandEncoder(descriptor: descriptor) else { return false }
      encoder.setRenderPipelineState(pipeline)
      encoder.setViewport(
        MTLViewport(
          originX: 0, originY: 0, width: Double(size.width), height: Double(size.height), znear: 0, zfar: 1))
      encoder.setFragmentTexture(first, index: 0)
      if let second = second { encoder.setFragmentTexture(second, index: 1) }
      encoder.setFragmentBytes(&params, length: MemoryLayout<EnhanceParams>.stride, index: 0)
      encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
      encoder.endEncoding()
      return true
    }
  }

  /// A layer presenter's enhancement: a CAMetalLayer beside the content
  /// layer, and a presenter thread that draws on it. The decoder thread puts
  /// each picture in a slot of one, the newest winning, and returns at once:
  /// it never waits for the GPU or for a drawable. The presenter thread
  /// draws the newest picture into the layer's next drawable and presents
  /// it, which is the picture's receipt. It keeps that picture, so that a
  /// new size or level redraws it while the desktop is still and sends
  /// nothing. Made on the platform thread when a viewer first enhances, and
  /// ended there.
  private final class EnhancedPresenter {
    let layer = CAMetalLayer()
    private let device: MTLDevice
    // Presenter thread: a picture presented in the current activation, and
    // the first of an activation, each with this presenter, so that a word
    // from one already replaced changes nothing; or the enhancement failing,
    // which ends the thread.
    private let presented: (Picture) -> Void
    private let revealed: (EnhancedPresenter, UInt64) -> Void
    private let failed: (EnhancedPresenter, String) -> Void
    private let condition = NSCondition()
    // Guarded by condition.
    private var slot: Picture?
    private var last: Picture?
    private var target: EnhanceTarget?
    private var redraw = false
    /// Each activation's number, and the last whose first picture was
    /// presented.
    private var epoch: UInt64 = 0
    private var revealedEpoch: UInt64 = 0
    /// Pictures the GPU failed, in a row.
    private var gpuErrors = 0
    private var quit = false

    init(
      device: MTLDevice, presented: @escaping (Picture) -> Void,
      revealed: @escaping (EnhancedPresenter, UInt64) -> Void,
      failed: @escaping (EnhancedPresenter, String) -> Void
    ) {
      self.device = device
      self.presented = presented
      self.revealed = revealed
      self.failed = failed
      layer.delegate = StillLayers.shared
      // The platform thread places it on whole pixels (pixelPlacement): a
      // resize of the container must not stretch it off them.
      layer.autoresizingMask = []
      layer.device = device
      layer.pixelFormat = .bgra8Unorm
      layer.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
      layer.framebufferOnly = true
      layer.isOpaque = true
      layer.maximumDrawableCount = 3
      // Each present joins the transaction that carries it, so the layer
      // shows its first picture before the platform thread reveals it.
      layer.presentsWithTransaction = true
      layer.isHidden = true
    }

    // MARK: Platform thread

    func start() {
      let thread = Thread { [self] in run() }
      thread.name = "pb-desktop-enhance"
      thread.qualityOfService = .userInteractive
      thread.start()
    }

    /// Pictures come here from now on, starting from [picture], the one on
    /// the content layer. A new epoch, whose first present the platform
    /// thread hears of.
    func activate(_ target: EnhanceTarget, from picture: Picture?) {
      condition.lock()
      epoch += 1
      self.target = target
      slot = nil
      last = picture
      redraw = picture != nil
      condition.signal()
      condition.unlock()
    }

    /// A new size or level redraws the picture shown.
    func retarget(_ target: EnhanceTarget) {
      condition.lock()
      if self.target != target {
        self.target = target
        redraw = slot != nil || last != nil
        condition.signal()
      }
      condition.unlock()
    }

    /// No more pictures come here: the newest it was given, for the content
    /// layer.
    func deactivate() -> Picture? {
      condition.lock()
      epoch += 1
      target = nil
      let newest = slot ?? last
      slot = nil
      last = nil
      redraw = false
      condition.signal()
      condition.unlock()
      return newest
    }

    /// Whether [epoch] is the current activation.
    func isCurrent(_ epoch: UInt64) -> Bool {
      condition.lock()
      defer { condition.unlock() }
      return target != nil && self.epoch == epoch
    }

    /// The thread ends once a draw in progress, if any, is done.
    func stop() {
      condition.lock()
      quit = true
      condition.signal()
      condition.unlock()
    }

    // MARK: Decoder thread

    /// True when [picture] replaced one not drawn yet.
    func put(_ picture: Picture) -> Bool {
      condition.lock()
      let superseded = slot != nil
      slot = picture
      condition.signal()
      condition.unlock()
      return superseded
    }

    // MARK: Presenter thread

    private func run() {
      let gpu: EnhanceGpu
      do {
        gpu = try EnhanceGpu(device: device)
      } catch {
        fail((error as? VideoError)?.message ?? "\(error)")
        return
      }
      var failures = 0
      // Drawables that did not come, in a row, while an activation ran.
      var starved = 0
      while true {
        condition.lock()
        while !quit && gpuErrors < 3 && (target == nil || (slot == nil && !redraw)) {
          if target == nil { gpu.idle() }
          condition.wait()
        }
        if quit || gpuErrors >= 3 {
          condition.unlock()
          fail("The GPU failed three enhanced pictures in a row")
          return
        }
        guard let size = target?.size else {
          condition.unlock()
          continue
        }
        let waiting = self.epoch
        condition.unlock()
        // Enclose drawable acquisition and every exit path in the iteration's
        // autorelease pool so autoreleased CAMetalDrawable references drain
        // promptly instead of exhausting the three-drawable pool.
        let keepRunning: Bool = autoreleasepool {
          // The drawable first, which may wait; the picture after, so that it
          // is the newest the decoder thread gave.
          guard let drawable = gpu.drawable(size, in: layer) else {
            // A second went by without one: the picture is tried again. Two
            // in a row for a window that counts as visible, and the content
            // layer takes the picture back rather than hold it and its
            // receipts any longer.
            condition.lock()
            let current = waiting == self.epoch && target != nil
            condition.unlock()
            starved = current ? starved + 1 : 0
            if starved >= 2 {
              fail("No drawable came for two seconds")
              return false
            }
            return true
          }
          starved = 0
          condition.lock()
          if let next = slot {
            last = next
            slot = nil
          }
          redraw = false
          let picture = last
          let goal = target
          let drawing = self.epoch
          // A new size while it waited: drawn again, at that size.
          if let goal = goal, goal.size != size, picture != nil { redraw = true }
          condition.unlock()
          // Ended or resized while it waited: the drawable goes back unshown.
          guard let shown = picture, let goal = goal, goal.size == size else { return true }
          let drawn = gpu.draw(shown, goal, into: drawable, timed: shown.readyNs != 0) {
            [weak self] done in
            self?.completed(done)
          }
          switch drawn {
          case .presented:
            failures = 0
            condition.lock()
            let current = drawing == self.epoch
            let first = current && revealedEpoch != drawing
            if first { revealedEpoch = drawing }
            condition.unlock()
            // Presented after its activation ended, into a layer hidden
            // since, it reached no screen and earns no receipt.
            if current { presented(shown) }
            if first { revealed(self, drawing) }
          case .failed(let reason):
            failures += 1
            if failures >= 3 {
              fail(reason)
              return false
            }
          }
          return true
        }
        if !keepRunning { return }
      }
    }

    /// The enhancement failed, unless it was stopped meanwhile, which
    /// leaves the platform thread nothing to hear.
    private func fail(_ reason: String) {
      condition.lock()
      let stopped = quit
      condition.unlock()
      if !stopped { failed(self, reason) }
    }

    /// Metal's thread: whether the GPU finished a picture.
    private func completed(_ done: Bool) {
      condition.lock()
      gpuErrors = done ? 0 : gpuErrors + 1
      if !done { condition.signal() }
      condition.unlock()
    }
  }
#endif

#if os(macOS)
  /// The delegate of every layer here: no property change animates, on any
  /// thread or in any transaction, so a picture, a resize or the grey still
  /// takes effect at the commit that carries it.
  private final class StillLayers: NSObject, CALayerDelegate {
    static let shared = StillLayers()

    func action(for layer: CALayer, forKey event: String) -> CAAction? { NSNull() }
  }

  /// A layer presenter's layers: a container that a hosting view adds as its
  /// sublayer, holding the picture's content layer, while the viewer
  /// enhances its picture the enhancement's metal layer above it, and above
  /// both the grey still's two layers. Made at create, so the decoder thread
  /// commits pictures before Flutter makes the view, and kept through a
  /// reconnect, so the last picture stays.
  ///
  /// The decoder thread sets only the content layer's contents, each time in
  /// its own explicit transaction, which commits at once: that thread has no
  /// run loop to commit an implicit one. While the enhancement runs it also
  /// hands the enhancement each picture, and once the enhancement's layer
  /// shows, only hands it over. Everything else (hosting, resizing, placing
  /// the metal layer, the grey still, which layer shows) is the platform
  /// thread's.
  private final class LayerPresenter {
    /// How the decoder thread showed a picture.
    enum Shown {
      case committed
      /// Handed to the enhancement alone, replacing one it had not drawn
      /// yet or not.
      case queued(superseded: Bool)
      case noSurface
    }

    let container = CALayer()
    private let content = CALayer()
    /// The grey still (GreyStill in viewer_overlays.dart): Core Animation's
    /// own compositing filters take the picture's luminance, then halve it.
    /// A CIColorMatrix with GreyStill's coefficients would need the view's
    /// layerUsesCoreImageFilters and, applied in linear light, came out up
    /// to twice as bright as GreyStill.
    private let desaturate = CALayer()
    private let halve = CALayer()
    /// Held while the content layer's contents change, from either thread,
    /// and while the decoder thread hands a picture to the enhancement, so
    /// that the platform thread's switch back to the content layer shows
    /// the newest picture.
    private let routeLock = NSLock()
    // Guarded by routeLock.
    /// The pictures last committed, oldest first, kept from VideoToolbox's
    /// pool while the WindowServer may still composite them.
    private var held: [CVPixelBuffer] = []
    /// The picture last committed to the content layer.
    private var newest: Picture?
    /// The enhancement the decoder thread hands pictures to; nil while they
    /// go to the content layer alone.
    private var routed: EnhancedPresenter?
    /// Whether they go to the enhancement alone: once its layer shows in
    /// place of the content layer. Until then the content layer goes on
    /// showing each picture too, while the presenter thread compiles its
    /// shaders and draws its first.
    private var handedOff = false
    // Platform thread only, which is also routed's and handedOff's only
    // writer: the enhancement, while the viewer asks for one, the target it
    // last took while pictures go to it, whether its layer shows in place
    // of the content layer, and that layer's frame on whole pixels.
    private var enhanced: EnhancedPresenter?
    private var enhancedTarget: EnhanceTarget?
    private var revealed = false
    private var placement: CGRect?

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

    /// Shows [picture] at once, or, once the enhancement's layer shows,
    /// hands it to the enhancement alone. While [timed], `Commit` marks the
    /// content layer's commit.
    func show(_ picture: Picture, timed: Bool) -> Shown {
      routeLock.lock()
      defer { routeLock.unlock() }
      if let enhancement = routed {
        if handedOff { return .queued(superseded: enhancement.put(picture)) }
        // Its layer not showing yet, the content layer shows the picture
        // too, whether or not the enhancement draws it.
        _ = enhancement.put(picture)
      }
      let span = timed ? Signpost.begin("Commit", picture.generation, picture.frame) : nil
      let committed = commitContent(picture)
      Signpost.end("Commit", span)
      return committed ? .committed : .noSurface
    }

    /// Holding routeLock: [picture] on the content layer, in a transaction
    /// of its own or the one around it. False when it has no IOSurface.
    private func commitContent(_ picture: Picture) -> Bool {
      guard let surface = CVPixelBufferGetIOSurface(picture.buffer)?.takeUnretainedValue() else {
        return false
      }
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      content.contents = surface
      CATransaction.commit()
      if held.count == heldPictures { held.removeFirst() }
      held.append(picture.buffer)
      newest = picture
      return true
    }

    // MARK: Platform thread

    /// Pictures go to the enhancement, at [target], whose layer takes
    /// [frame] ([place]), and which [make] makes the first time; false when
    /// it cannot. A new activation keeps the content layer on screen, and
    /// showing each picture, until the enhancement's first present, which
    /// [reveal] then shows in its place in one transaction: the picture
    /// never blanks, nor stops while the shaders compile.
    func enhance(_ target: EnhanceTarget, at frame: CGRect, make: () -> EnhancedPresenter?) -> Bool {
      let enhancement: EnhancedPresenter
      if let current = enhanced {
        enhancement = current
      } else {
        guard let made = make() else { return false }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        container.insertSublayer(made.layer, above: content)
        CATransaction.commit()
        made.start()
        enhanced = made
        enhancement = made
        placement = nil
      }
      place(frame)
      // The platform thread is routed's only writer, so it reads it
      // without the lock, which the decoder thread holds across commits.
      if routed != nil {
        enhancement.retarget(target)
      } else {
        routeLock.lock()
        enhancement.activate(target, from: newest)
        routed = enhancement
        handedOff = false
        routeLock.unlock()
        revealed = false
      }
      enhancedTarget = target
      return true
    }

    /// The enhancement's layer at [frame] in the container, on whole pixels
    /// (pixelPlacement), in the transaction around this call if any: in
    /// Flutter's, as Flutter moves or resizes the view, the layer moves with
    /// it in the same commit.
    func place(_ frame: CGRect) {
      guard let enhancement = enhanced, frame != placement else { return }
      placement = frame
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      enhancement.layer.frame = frame
      CATransaction.commit()
    }

    /// Whether [enhancement] is the one this presenter has: a word from
    /// one released since is for nobody.
    func owns(_ enhancement: EnhancedPresenter) -> Bool { enhanced === enhancement }

    /// [sender] presented the first picture of [epoch]: if that is still
    /// this presenter's enhancement and the activation running, its layer
    /// replaces the content layer, and pictures go to it alone.
    func reveal(_ sender: EnhancedPresenter, _ epoch: UInt64) {
      guard let enhancement = enhanced, enhancement === sender, !revealed, enhancement.isCurrent(epoch)
      else { return }
      revealed = true
      routeLock.lock()
      handedOff = true
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      content.isHidden = true
      enhancement.layer.isHidden = false
      CATransaction.commit()
      routeLock.unlock()
    }

    /// Pictures go to the content layer alone again: in the transaction
    /// that hides the enhancement's layer, it takes the newest picture the
    /// enhancement was given, which is returned when that is a new one: it
    /// reached the WindowServer, so it earns its receipt. [release] also
    /// ends the enhancement and its thread.
    func plain(release: Bool) -> Picture? {
      guard let enhancement = enhanced else { return nil }
      var shown: Picture?
      if routed != nil {
        routeLock.lock()
        routed = nil
        handedOff = false
        let latest = enhancement.deactivate()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        if let picture = latest, !picture.same(as: newest), commitContent(picture) {
          shown = picture
        }
        enhancement.layer.isHidden = true
        content.isHidden = false
        CATransaction.commit()
        routeLock.unlock()
        revealed = false
        enhancedTarget = nil
      }
      if release {
        enhancement.stop()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        enhancement.layer.removeFromSuperlayer()
        CATransaction.commit()
        enhanced = nil
        placement = nil
      }
      return shown
    }

    /// Where the enhancement draws now, if it is the one showing.
    var enhancing: EnhanceTarget? { revealed ? enhancedTarget : nil }

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

    /// Once no decoder thread runs: the enhancement, the picture and those
    /// held go.
    func clear() {
      _ = plain(release: true)
      routeLock.lock()
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      content.contents = nil
      CATransaction.commit()
      held.removeAll()
      newest = nil
      routeLock.unlock()
    }
  }

  /// FlutterView's hook (FlutterView.mm), which only a FlutterView answers:
  /// the engine's cursor plugin and the pointer plugin (DesktopPointer.swift)
  /// tell it each cursor they set.
  private let flutterViewCursorHook = NSSelectorFromString("didUpdateMouseCursor:")

  /// The platform view that shows a layer presenter: a layer-backed view
  /// whose own layer hosts the presenter's container. Pointer and keyboard
  /// stay Flutter's: every mouse, scroll and gesture event goes up the
  /// responder chain to the FlutterView and its controller, as it would from
  /// any view without handlers, and the view never takes the keyboard.
  private final class DesktopVideoView: NSView {
    private weak var viewer: DesktopVideoViewer?
    private var screenObserver: NSObjectProtocol?
    /// While the viewer enhances its picture, which it does only while the
    /// picture is enlarged on screen: the frame Flutter sets on this view
    /// each frame it composites the view in, after it placed the view's
    /// ancestors, its zoom, which reaches this view as its container's
    /// sublayer transform (FlutterMutatorView.mm), and the window's
    /// occlusion, each with a new backing scale or window, tell the viewer
    /// to measure where the picture is on screen again. Nothing watches
    /// otherwise.
    var watchesGeometry = false {
      didSet {
        if watchesGeometry != oldValue { watch() }
      }
    }
    private var transformWatch: NSKeyValueObservation?
    private var occlusionObserver: NSObjectProtocol?

    init(viewer: DesktopVideoViewer?) {
      self.viewer = viewer
      super.init(frame: .zero)
      wantsLayer = true
      layerContentsRedrawPolicy = .never
      // The cursor is whatever Flutter or the pointer plugin set last: no
      // cursor rects, and a cursor update over the picture, as the pointer
      // enters it or the app comes back to the front, sets that cursor
      // again (cursorUpdate(with:)). AppKit takes cursor updates only in an
      // active app (NSTrackingArea.h: not supported with activeAlways).
      addTrackingArea(
        NSTrackingArea(
          rect: .zero, options: [.cursorUpdate, .inVisibleRect, .activeInActiveApp], owner: self,
          userInfo: nil))
    }

    required init?(coder: NSCoder) { nil }

    deinit {
      if let observer = screenObserver { NotificationCenter.default.removeObserver(observer) }
      if let observer = occlusionObserver { NotificationCenter.default.removeObserver(observer) }
    }

    private func watch() {
      transformWatch = nil
      if let observer = occlusionObserver { NotificationCenter.default.removeObserver(observer) }
      occlusionObserver = nil
      guard watchesGeometry else { return }
      if let container = superview?.layer {
        transformWatch = container.observe(\.sublayerTransform) { [weak self] _, _ in self?.moved() }
      }
      if let window = window {
        occlusionObserver = NotificationCenter.default.addObserver(
          forName: NSWindow.didChangeOcclusionStateNotification, object: window, queue: .main
        ) { [weak self] _ in self?.moved() }
      }
    }

    /// Flutter sets the transform as it composites, on the platform thread;
    /// any other thread's word goes there.
    private func moved() {
      guard Thread.isMainThread else {
        DispatchQueue.main.async { [weak self] in self?.moved() }
        return
      }
      viewer?.geometryChanged(self)
    }

    /// Flutter sets this view's frame each frame it composites the view in,
    /// once it placed the view's ancestors (FlutterMutatorView.mm), and
    /// NSView's setFrame: calls this even with the same size: a picture
    /// that moved is measured again here.
    override func setFrameSize(_ newSize: NSSize) {
      super.setFrameSize(newSize)
      if watchesGeometry { moved() }
    }

    override func layout() {
      super.layout()
      if watchesGeometry { moved() }
    }

    override func viewDidChangeBackingProperties() {
      super.viewDidChangeBackingProperties()
      if watchesGeometry { moved() }
    }

    override func viewDidMoveToSuperview() {
      super.viewDidMoveToSuperview()
      if watchesGeometry {
        watch()
        moved()
      }
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

    /// AppKit sends a cursor update to the view under the pointer, which
    /// over the picture is this one, after it set a cursor of its own (an
    /// arrow, as a menu closes or the app comes back to the front).
    /// FlutterView sets its last cursor again on a cursor update only where
    /// it is that view itself, so over the picture this view does: Flutter's
    /// cursor, hidden while the pointer is the remote's, or the host's shape
    /// the pointer plugin set. No super call, which would leave AppKit's.
    override func cursorUpdate(with event: NSEvent) {
      flutterCursor()?.set()
    }

    /// The cursor the FlutterView this view sits in was told last: its
    /// `_lastCursor` (FlutterView.mm), the one its own cursorUpdate: sets.
    /// Nil if the engine keeps none, which leaves the cursor as it is.
    private func flutterCursor() -> NSCursor? {
      var ancestor = superview
      while let view = ancestor {
        if view.responds(to: flutterViewCursorHook) {
          guard let ivar = class_getInstanceVariable(object_getClass(view), "_lastCursor"),
            let encoding = ivar_getTypeEncoding(ivar), String(cString: encoding).hasPrefix("@")
          else { return nil }
          return object_getIvar(view, ivar) as? NSCursor
        }
        ancestor = view.superview
      }
      return nil
    }

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
      if watchesGeometry { watch() }
      viewer?.viewChanged(self)
    }
  }

  /// The display link of a layer presenter's view: while the source streams
  /// and the view is on screen, it asks a variable-refresh display (a
  /// ProMotion panel) for its fastest rate, which the video commits no
  /// longer get from Flutter's own frames, and, while the stats ask for
  /// timings, has the viewer count the pictures committed beyond one a
  /// refresh, which no refresh showed. Before macOS 14, which has no NSView
  /// display link, a CVDisplayLink counts and nothing asks for a rate. Each
  /// callback gives the time of its refresh and the refresh period, since
  /// the NSView link's callbacks run on the main thread, which Flutter's
  /// UI thread shares: a busy one misses refreshes, which arrive as one
  /// callback. Platform thread.
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
      CVDisplayLinkSetOutputHandler(cvLink) { [weak viewer] _, _, output, _, _ in
        let stamp = output.pointee
        let scale = Double(stamp.videoTimeScale)
        let valid = stamp.flags & CVTimeStampFlags.videoTimeValid.rawValue != 0 && scale > 0
        viewer?.refreshed(
          at: valid ? Double(stamp.videoTime) / scale : 0,
          period: valid ? Double(stamp.videoRefreshPeriod) / scale : 0)
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
      guard #available(macOS 14.0, *), let link = link as? CADisplayLink else { return }
      viewer?.refreshed(at: link.timestamp, period: link.duration)
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
    // Platform thread only: the enhancement asked for, whether it failed at
    // that level, the decoded picture's size, the picture's size on screen
    // in pixels on whole pixels (nil while the window is not visible), the
    // metal layer's frame there, its screen's size, and whether an update
    // of the enhancement waits for the platform thread's next turn.
    private var enhanceLevel = EnhanceLevel.off
    private var enhanceFailed = false
    private var pictureSize: PixelSize?
    private var onScreen: PixelSize?
    private var placement: CGRect?
    private var screenSize: PixelSize?
    private var updateQueued = false
    /// Feed threads, one at a time: the size of the last picture presented,
    /// which the platform thread hears of when it changes.
    private var feedSize: PixelSize?
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
  /// committed since it last fired (`CACurrentMediaTime`), and the time in
  /// seconds of the refresh it last fired for.
  private var countingRefreshes = false
  private var commitTimes: [Double] = []
  private var lastRefresh: Double?
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
        view?.watchesGeometry = false
        enhanceLevel = .off
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
      commitTimes.removeAll()
      lastRefresh = nil
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
      // While the enhancement's layer shows the picture: its level and the
      // size it draws at.
      if let target = presenter?.enhancing {
        values["enhanced"] = [
          "level": target.level.rawValue, "width": target.size.width, "height": target.size.height,
        ]
      }
    #endif
    return values
  }

  #if os(macOS)
    /// The view Flutter made for this viewer now shows the presenter's
    /// layers, taken from any view that showed them before.
    func host(in view: DesktopVideoView) {
      guard let presenter = presenter, let layer = view.layer else { return }
      self.view?.watchesGeometry = false
      self.view = view
      presenter.host(in: layer)
      updateRefreshLink()
      view.watchesGeometry = enhanceLevel != .off
      geometryChanged(view)
    }

    /// The hosting view entered or left a window, or its window another
    /// display.
    func viewChanged(_ view: DesktopVideoView) {
      guard view === self.view else { return }
      updateRefreshLink()
      geometryChanged(view)
    }

    /// `enhance`: how much the picture is sharpened while it shows enlarged.
    /// Off, which every viewer starts at, no thread, GPU state or watch of
    /// the view exists. A new level tries again after a failure.
    func setEnhance(_ level: EnhanceLevel) {
      guard presenter != nil, level != enhanceLevel else { return }
      enhanceLevel = level
      enhanceFailed = false
      view?.watchesGeometry = level != .off
      measure()
      updateEnhancement()
    }

    /// The hosting view's frame, transform, window or occlusion may have
    /// changed: measured again while the viewer enhances, and the metal
    /// layer placed on whole pixels at once, so that it moves in the
    /// transaction Flutter moves the view in. What else that changes waits
    /// for the platform thread's next turn: in a frame that zooms, Flutter
    /// sets the view's frame before the zoom, and a measure between the two
    /// must not reach the presenter thread.
    func geometryChanged(_ view: DesktopVideoView) {
      guard view === self.view, enhanceLevel != .off else { return }
      measure()
      if let placement = placement { presenter?.place(placement) }
      guard !updateQueued else { return }
      updateQueued = true
      DispatchQueue.main.async { [weak self] in
        guard let self = self else { return }
        self.updateQueued = false
        guard self.enhanceLevel != .off else { return }
        self.measure()
        self.updateEnhancement()
      }
    }

    /// Where the picture is on screen: the view's layer in the window's
    /// root layer, which takes in Flutter's transform on the view's
    /// container, on whole pixels at the backing scale (pixelPlacement), as
    /// a size in pixels and the metal layer's frame. Nothing while the
    /// window is not visible (occluded, minimised, or its display asleep),
    /// where waiting on drawables would hold receipts back, nor before
    /// AppKit joined a view new in the window to its layer tree, which it
    /// does in its next layout pass, where layout() measures again.
    private func measure() {
      onScreen = nil
      placement = nil
      screenSize = nil
      guard let presenter = presenter, let view = view, let layer = view.layer, let window = view.window,
        let screen = window.screen, window.occlusionState.contains(.visible)
      else { return }
      // The window's frame view, the top of its views: its layer is among
      // the view's layer's ancestors once AppKit joined them.
      var top: NSView = view
      while let up = top.superview { top = up }
      var root = layer
      var joined = top.layer == nil
      while let up = root.superlayer {
        if up === top.layer { joined = true }
        root = up
      }
      guard joined, root !== layer else { return }
      let scale = window.backingScaleFactor
      func pixels(_ points: CGFloat) -> Int? {
        let value = (points * scale).rounded()
        return value.isFinite && value >= 1 && value <= 1_000_000 ? Int(value) : nil
      }
      guard let placed = pixelPlacement(of: layer, in: presenter.container, root: root, scale: scale),
        let screenWidth = pixels(screen.frame.width), let screenHeight = pixels(screen.frame.height)
      else { return }
      onScreen = placed.size
      placement = placed.frame
      screenSize = PixelSize(width: min(maxDrawable, screenWidth), height: min(maxDrawable, screenHeight))
    }

    /// A Feed thread's picture of a new size reached the platform thread.
    func pictureSized(_ size: PixelSize) {
      guard size != pictureSize else { return }
      pictureSize = size
      if enhanceLevel != .off { updateEnhancement() }
    }

    /// The enhancement runs while it is asked for and the picture shows at
    /// least its own size on both axes, as airhand's renderPlan: a picture
    /// shown smaller is left to the content layer's linear filter. Nor does
    /// it run for a picture larger on screen than its screen, zoomed past
    /// it, whose drawables would grow without bound: the whole picture is
    /// drawn, not the part a zoom shows, so a picture filling the screen
    /// goes back to the content layer as soon as it is zoomed in.
    private func enhanceTarget() -> EnhanceTarget? {
      guard enhanceLevel != .off, !enhanceFailed, let picture = pictureSize, let pixels = onScreen,
        let screen = screenSize, pixels.width >= picture.width, pixels.height >= picture.height,
        pixels.width <= screen.width, pixels.height <= screen.height
      else { return nil }
      return EnhanceTarget(size: pixels, level: enhanceLevel)
    }

    private func updateEnhancement() {
      guard let presenter = presenter else { return }
      if let target = enhanceTarget(), let frame = placement {
        if presenter.enhance(target, at: frame, make: makeEnhancement) { return }
        enhanceFailed = true
        logVideo("Picture enhancement is unavailable: no Metal device")
      }
      if let shown = presenter.plain(release: enhanceLevel == .off || enhanceFailed) {
        acknowledge(shown)
      }
    }

    /// On the GPU that drives the window's display, so that a Mac with two
    /// GPUs neither switches nor copies between them.
    private func makeEnhancement() -> EnhancedPresenter? {
      let number = view?.window?.screen?.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")]
      var device: MTLDevice?
      if let display = (number as? NSNumber)?.uint32Value {
        device = CGDirectDisplayCopyCurrentMetalDevice(display)
      }
      guard let gpu = device ?? MTLCreateSystemDefaultDevice() else { return nil }
      return EnhancedPresenter(
        device: gpu,
        presented: { [weak self] picture in self?.acknowledge(picture) },
        revealed: { [weak self] sender, epoch in
          DispatchQueue.main.async { self?.presenter?.reveal(sender, epoch) }
        },
        failed: { [weak self] sender, reason in
          DispatchQueue.main.async { self?.enhancementFailed(sender, reason) }
        })
    }

    /// [sender] failed: if it is still the viewer's enhancement, the content
    /// layer shows the picture again, until the level changes.
    private func enhancementFailed(_ sender: EnhancedPresenter, _ reason: String) {
      guard presenter?.owns(sender) == true, enhanceLevel != .off, !enhanceFailed else { return }
      enhanceFailed = true
      logVideo("Picture enhancement stopped: \(reason)")
      updateEnhancement()
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
    /// a display whose refresh varies (which it asks for its fastest rate,
    /// from macOS 14, the first with a link that can ask) or while timings
    /// count the pictures no refresh showed. Otherwise it runs none.
    private func updateRefreshLink() {
      guard presenter != nil else { return }
      lock.lock()
      let timed = decodeTime != nil
      lock.unlock()
      var askHz: Double?
      var run = false
      if attached, streaming, let view = view, let screen = view.window?.screen {
        if #available(macOS 14.0, *), screen.minimumRefreshInterval < screen.maximumRefreshInterval {
          askHz = Double(screen.maximumFramesPerSecond)
        }
        run = timed || askHz != nil
        if run { refreshLink.start(view, askHz: askHz) }
      }
      if !run { refreshLink.stop() }
      lock.lock()
      countingRefreshes = run && refreshLink.running && decodeTime != nil
      commitTimes.removeAll(keepingCapacity: countingRefreshes)
      lastRefresh = nil
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
    let timed = shownTiming != nil
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
    if acknowledge {
      if timed { Signpost.event("Consumed", picture.generation, picture.frame) }
      if consumer != 0 { _ = library.presented(consumer, picture.generation, picture.frame) }
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
    /// lock, or hands it to the enhancement, whose thread presents it. A
    /// picture of an older revision is not shown. One the enhancement alone
    /// had and had not drawn when this one replaced it counts as dropped.
    private func present(_ picture: Picture, _ presenter: LayerPresenter) throws {
      lock.lock()
      decoded += 1
      let current = picture.revision == revision
      lock.unlock()
      guard current else { return }
      let size = PixelSize(
        width: CVPixelBufferGetWidth(picture.buffer), height: CVPixelBufferGetHeight(picture.buffer))
      if size != feedSize {
        feedSize = size
        DispatchQueue.main.async { [weak self] in self?.pictureSized(size) }
      }
      // Stamped at its decode's end only while timed.
      switch presenter.show(picture, timed: picture.readyNs != 0) {
      case .committed: acknowledge(picture)
      case .queued(let superseded): if superseded { drop() }
      case .noSurface: throw VideoError("The decoded picture has no IOSurface")
      }
    }

    /// [picture] reached the WindowServer: committed to the content layer
    /// (decoder or platform thread), or presented by the enhancement
    /// (its thread). That is the presentation receipt, once per picture of
    /// the current revision, as the compositor taking it is a texture's.
    /// A picture no newer than the last one acknowledged changes nothing,
    /// as Rust's sink takes only newer frames: one the enhancement presented
    /// after the content layer took a newer picture back is not on screen.
    /// While timed, `present` runs from the end of its decode
    /// ([Picture.readyNs]) to the end of its commit.
    private func acknowledge(_ picture: Picture) {
      lock.lock()
      let newer =
        picture.revision > consumedRevision
        || (picture.revision == consumedRevision && picture.frame > consumedFrame)
      if newer { shown = picture.buffer }
      var acknowledge = false
      if newer && picture.revision == revision {
        consumedRevision = picture.revision
        consumedFrame = picture.frame
        acknowledge = true
        presented += 1
        if countingRefreshes {
          if commitTimes.count < 512 {
            commitTimes.append(CACurrentMediaTime())
          } else {
            dropped += 1
          }
        }
        if let timing = shownTiming, picture.readyNs != 0 {
          let now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
          timing.add(now > picture.readyNs ? now - picture.readyNs : 0)
        }
      }
      let consumer = self.consumer
      lock.unlock()
      // The receipt Rust validates and forwards for input authorisation.
      // Non-blocking.
      if acknowledge && consumer != 0 {
        _ = library.presented(consumer, picture.generation, picture.frame)
      }
    }

    /// Thread of the display link, for the refresh at [time] seconds, every
    /// [period] seconds: within each refresh interval since its last callback,
    /// commits beyond the first were superseded before any composite showed
    /// them. Idle intervals after a burst never offset earlier superseded
    /// commits; without a period (0) all commits fall in one interval. Counted
    /// only while timed.
    func refreshed(at time: Double, period: Double) {
      lock.lock()
      if countingRefreshes {
        var refreshes = 1
        if let last = lastRefresh, period > 0, time > last {
          // Bounded before the conversion: a gap of hours stays an Int.
          refreshes = max(1, Int(min((time - last) / period, 1_000_000).rounded()))
        }
        if refreshes == 1 || period <= 0 {
          if commitTimes.count > 1 { dropped += commitTimes.count - 1 }
        } else if !commitTimes.isEmpty {
          let end = CACurrentMediaTime()
          var currentSlot = -1
          var inSlot = 0
          for committed in commitTimes {
            let age = max(0, end - committed)
            let slot = min(refreshes - 1, Int((age / period).rounded(.down)))
            if slot == currentSlot {
              inSlot += 1
            } else {
              if inSlot > 1 { dropped += inSlot - 1 }
              currentSlot = slot
              inSlot = 1
            }
          }
          if inSlot > 1 { dropped += inSlot - 1 }
        }
        lastRefresh = period > 0 ? time : nil
      }
      commitTimes.removeAll(keepingCapacity: true)
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
    if timed { Signpost.event("Decoded", unit.generation, unit.frame) }
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
    case "enhance":
      guard let name = args["level"] as? String, let level = EnhanceLevel(rawValue: name) else {
        throw VideoError("Unknown enhancement level")
      }
      // Only a layer presenter enhances; a texture takes none.
      #if os(macOS)
        viewer.setEnhance(level)
      #else
        _ = level
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
