// This record's harness (see README.md): decode a clipgen H.264 clip paced
// at 60 fps with the
// session DesktopVideo.swift builds (hardware, real time, synchronous,
// IOSurface-backed Metal-compatible BGRA of the clip's size), holding two
// pictures as the texture path does, and time VTDecompressionSessionDecodeFrame.
// Usage: paced CLIP.h264 WIDTH HEIGHT LOOPS
import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

let args = CommandLine.arguments
let clip = try! Data(contentsOf: URL(fileURLWithPath: args[1]))
let (w, h, loops) = (Int(args[2])!, Int(args[3])!, Int(args[4])!)
let bytes = [UInt8](clip)

// NAL units behind 4-byte start codes (clipgen writes only those).
var nals: [ArraySlice<UInt8>] = []
var starts: [Int] = []
var i = 0
while i + 4 <= bytes.count {
  if bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 0 && bytes[i + 3] == 1 { starts.append(i + 4); i += 4 } else { i += 1 }
}
for (k, s) in starts.enumerated() { nals.append(bytes[s..<(k + 1 < starts.count ? starts[k + 1] - 4 : bytes.count)]) }
var sps: [UInt8] = [], pps: [UInt8] = []
var units: [[UInt8]] = []
for nal in nals {
  let type = nal.first! & 0x1F
  if type == 9 { units.append([]) } else if type == 7 { sps = Array(nal) } else if type == 8 { pps = Array(nal) } else {
    let n = nal.count
    units[units.count - 1] += [UInt8(n >> 24 & 0xFF), UInt8(n >> 16 & 0xFF), UInt8(n >> 8 & 0xFF), UInt8(n & 0xFF)] + nal
  }
}
var format: CMFormatDescription?
sps.withUnsafeBufferPointer { s in
  pps.withUnsafeBufferPointer { p in
    let pointers = [s.baseAddress!, p.baseAddress!]
    let sizes = [sps.count, pps.count]
    _ = CMVideoFormatDescriptionCreateFromH264ParameterSets(
      allocator: kCFAllocatorDefault, parameterSetCount: 2, parameterSetPointers: pointers,
      parameterSetSizes: sizes, nalUnitHeaderLength: 4, formatDescriptionOut: &format)
  }
}
final class Out { var image: CVImageBuffer?; var status: OSStatus = 0 }
let out = Out()
var callback = VTDecompressionOutputCallbackRecord(
  decompressionOutputCallback: { refCon, _, status, _, image, _, _ in
    let o = Unmanaged<Out>.fromOpaque(refCon!).takeUnretainedValue()
    o.status = status
    o.image = image
  }, decompressionOutputRefCon: Unmanaged.passUnretained(out).toOpaque())
let attributes: [String: Any] = [
  kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
  kCVPixelBufferWidthKey as String: w, kCVPixelBufferHeightKey as String: h,
  kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
  kCVPixelBufferMetalCompatibilityKey as String: true,
]
let specification: [String: Any] = [
  kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder as String: true
]
var session: VTDecompressionSession?
VTDecompressionSessionCreate(
  allocator: nil, formatDescription: format!, decoderSpecification: specification as CFDictionary,
  imageBufferAttributes: attributes as CFDictionary, outputCallback: &callback, decompressionSessionOut: &session)
VTSessionSetProperty(session!, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)

var times: [Double] = []
var held: [CVImageBuffer] = []
for _ in 0..<loops {
  for var unit in units {
    let length = unit.count
    unit.withUnsafeMutableBytes { raw in
      var block: CMBlockBuffer?
      CMBlockBufferCreateWithMemoryBlock(
        allocator: kCFAllocatorDefault, memoryBlock: raw.baseAddress, blockLength: length,
        blockAllocator: kCFAllocatorNull, customBlockSource: nil, offsetToData: 0, dataLength: length,
        flags: 0, blockBufferOut: &block)
      var sample: CMSampleBuffer?
      var size = length
      CMSampleBufferCreateReady(
        allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format, sampleCount: 1,
        sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1, sampleSizeArray: &size,
        sampleBufferOut: &sample)
      out.image = nil
      let t0 = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
      let status = VTDecompressionSessionDecodeFrame(session!, sampleBuffer: sample!, flags: [], frameRefcon: nil, infoFlagsOut: nil)
      let t1 = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
      guard status == 0, out.status == 0, let image = out.image else { print("decode failed \(status) \(out.status)"); exit(1) }
      held.append(image)
      if held.count > 2 { held.removeFirst() }
      times.append(Double(t1 - t0) / 1e6)
    }
    Thread.sleep(forTimeInterval: 1.0 / 60)
  }
}
let sorted = times.sorted()
func f(_ x: Double) -> String { String(format: "%.3f", x) }
print("clip=\(args[1].split(separator: "/").last!) \(w)x\(h) decodes=\(times.count) "
  + "mean=\(f(times.reduce(0, +) / Double(times.count))) median=\(f(sorted[sorted.count / 2])) "
  + "p95=\(f(sorted[Int(Double(sorted.count) * 0.95)])) max=\(f(sorted.last!)) (ms)")
