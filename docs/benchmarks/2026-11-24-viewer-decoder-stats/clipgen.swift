// The clips of this record (see README.md): an H.264 Annex B clip for
// desktop_ui_host, with an access
// unit delimiter before every unit and SPS/PPS before the one IDR (the
// first), from VideoToolbox's real-time encoder, no reordering. Content: a
// desktop-like picture (flat background, text-like rows, a moving window).
// Usage: clipgen WIDTH HEIGHT FRAMES OUT.h264
import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

func makeFrame(_ w: Int, _ h: Int, _ i: Int) -> CVPixelBuffer {
  var pb: CVPixelBuffer?
  let attrs: [String: Any] = [kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any]()]
  CVPixelBufferCreate(nil, w, h, kCVPixelFormatType_32BGRA, attrs as CFDictionary, &pb)
  let p = pb!
  CVPixelBufferLockBaseAddress(p, [])
  let base = CVPixelBufferGetBaseAddress(p)!.assumingMemoryBound(to: UInt32.self)
  let stride = CVPixelBufferGetBytesPerRow(p) / 4
  let wx = (i * 24) % max(1, w - w / 3)
  let wy = (i * 12) % max(1, h - h / 3)
  for y in 0..<h {
    let row = base + y * stride
    let textRow = (y / 6) % 3 == 0
    for x in 0..<w {
      var v: UInt32 = 0xFFF0_F0F0
      if textRow && ((x &* 2654435761) >> 29) & 1 == 1 { v = 0xFF20_2020 }
      if x >= wx && x < wx + w / 3 && y >= wy && y < wy + h / 3 {
        v = 0xFF00_0000 | UInt32((x + i * 3) & 0xFF) << 16 | UInt32((y + i) & 0xFF) << 8 | 0x80
      }
      row[x] = v
    }
  }
  CVPixelBufferUnlockBaseAddress(p, [])
  return p
}

final class Collector { var samples: [CMSampleBuffer] = [] }

let args = CommandLine.arguments
guard args.count == 5, let w = Int(args[1]), let h = Int(args[2]), let n = Int(args[3]) else {
  print("usage: clipgen WIDTH HEIGHT FRAMES OUT.h264")
  exit(2)
}
var session: VTCompressionSession?
let collector = Collector()
let ref = Unmanaged.passUnretained(collector).toOpaque()
VTCompressionSessionCreate(
  allocator: nil, width: Int32(w), height: Int32(h), codecType: kCMVideoCodecType_H264,
  encoderSpecification: nil, imageBufferAttributes: nil, compressedDataAllocator: nil,
  outputCallback: { refCon, _, status, _, sample in
    guard status == 0, let s = sample else { return }
    Unmanaged<Collector>.fromOpaque(refCon!).takeUnretainedValue().samples.append(s)
  }, refcon: ref, compressionSessionOut: &session)
let s = session!
VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel, value: kVTProfileLevel_H264_Main_AutoLevel)
VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: w * h * 4))
VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: NSNumber(value: n + 1))
for i in 0..<n {
  let t = CMTime(value: CMTimeValue(i), timescale: 60)
  VTCompressionSessionEncodeFrame(
    s, imageBuffer: makeFrame(w, h, i), presentationTimeStamp: t, duration: .invalid,
    frameProperties: nil, sourceFrameRefcon: nil, infoFlagsOut: nil)
}
VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid)
VTCompressionSessionInvalidate(s)

let startCode: [UInt8] = [0, 0, 0, 1]
var out = Data()
var keys = 0
for sample in collector.samples {
  out.append(contentsOf: startCode + [0x09, 0xF0])
  let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]]
  let key = !(attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool ?? false)
  if key {
    keys += 1
    let format = CMSampleBufferGetFormatDescription(sample)!
    for index in 0..<2 {
      var pointer: UnsafePointer<UInt8>?
      var size = 0
      CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        format, parameterSetIndex: index, parameterSetPointerOut: &pointer, parameterSetSizeOut: &size,
        parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil)
      out.append(contentsOf: startCode)
      out.append(pointer!, count: size)
    }
  }
  let block = CMSampleBufferGetDataBuffer(sample)!
  var length = 0
  var data: UnsafeMutablePointer<CChar>?
  CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &data)
  let bytes = UnsafeRawPointer(data!).assumingMemoryBound(to: UInt8.self)
  var at = 0
  while at + 4 <= length {
    let nal = Int(bytes[at]) << 24 | Int(bytes[at + 1]) << 16 | Int(bytes[at + 2]) << 8 | Int(bytes[at + 3])
    out.append(contentsOf: startCode)
    out.append(bytes + at + 4, count: nal)
    at += 4 + nal
  }
}
try! out.write(to: URL(fileURLWithPath: args[4]))
print("frames=\(collector.samples.count) keyframes=\(keys) bytes=\(out.count) mean_unit_bytes=\(out.count / max(1, collector.samples.count))")
