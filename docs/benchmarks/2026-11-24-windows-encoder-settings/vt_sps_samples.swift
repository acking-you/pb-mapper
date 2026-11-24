// Prints the SPS and PPS (hex, emulation prevention included) that
// VideoToolbox's H.264 encoder writes for a few configurations, to commit as
// test samples for pb-mapper's SPS rewriter.
import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

func hex(_ bytes: [UInt8]) -> String { bytes.map { String(format: "%02x", $0) }.joined() }

func sample(name: String, width: Int32, height: Int32, profile: CFString, reorder: Bool, lowLatency: Bool, colour: Bool = false) {
  var spec: [CFString: Any] = [:]
  if lowLatency { spec["EnableLowLatencyRateControl" as CFString] = true }
  var session: VTCompressionSession?
  let status = VTCompressionSessionCreate(
    allocator: nil, width: width, height: height, codecType: kCMVideoCodecType_H264,
    encoderSpecification: spec as CFDictionary, imageBufferAttributes: nil, compressedDataAllocator: nil,
    outputCallback: nil, refcon: nil, compressionSessionOut: &session)
  guard status == noErr, let session else { print("\(name) create failed \(status)"); return }
  VTSessionSetProperty(session, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
  VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AllowFrameReordering, value: reorder ? kCFBooleanTrue : kCFBooleanFalse)
  let set = VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ProfileLevel, value: profile)
  if colour {
    VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ColorPrimaries, value: kCVImageBufferColorPrimaries_ITU_R_709_2)
    VTSessionSetProperty(session, key: kVTCompressionPropertyKey_TransferFunction, value: kCVImageBufferTransferFunction_ITU_R_709_2)
    VTSessionSetProperty(session, key: kVTCompressionPropertyKey_YCbCrMatrix, value: kCVImageBufferYCbCrMatrix_ITU_R_709_2)
  }
  VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: NSNumber(value: 60))
  VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: 4_000_000))
  var pixel: CVPixelBuffer?
  CVPixelBufferCreate(nil, Int(width), Int(height), kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                      [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &pixel)
  var printed = false
  for i in 0..<3 {
    let done = DispatchSemaphore(value: 0)
    let pts = CMTime(value: CMTimeValue(i), timescale: 60)
    VTCompressionSessionEncodeFrame(session, imageBuffer: pixel!, presentationTimeStamp: pts, duration: .invalid,
                                    frameProperties: nil, infoFlagsOut: nil) { st, _, sample in
      defer { done.signal() }
      guard !printed, st == noErr, let sample, let format = CMSampleBufferGetFormatDescription(sample) else { return }
      var count = 0
      CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: 0, parameterSetPointerOut: nil,
                                                         parameterSetSizeOut: nil, parameterSetCountOut: &count,
                                                         nalUnitHeaderLengthOut: nil)
      var sets: [String] = []
      for index in 0..<count {
        var ptr: UnsafePointer<UInt8>?
        var size = 0
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: index, parameterSetPointerOut: &ptr,
                                                           parameterSetSizeOut: &size, parameterSetCountOut: nil,
                                                           nalUnitHeaderLengthOut: nil)
        if let ptr { sets.append(hex(Array(UnsafeBufferPointer(start: ptr, count: size)))) }
      }
      print("\(name) profile_set=\(set) \(width)x\(height) reorder=\(reorder) low_latency=\(lowLatency) sets=\(sets.joined(separator: ","))")
      printed = true
    }
    if reorder { _ = done.wait(timeout: .now() + 0.01) } else { done.wait() }
  }
  VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
  VTCompressionSessionInvalidate(session)
}

let constrainedHigh = "kVTProfileLevel_H264_ConstrainedHigh_AutoLevel" as CFString
sample(name: "vt_constrained_high_1080p", width: 1920, height: 1080, profile: constrainedHigh, reorder: false, lowLatency: true)
sample(name: "vt_high_720p", width: 1280, height: 720, profile: kVTProfileLevel_H264_High_AutoLevel, reorder: false, lowLatency: false)
sample(name: "vt_high_reorder_720p", width: 1280, height: 720, profile: kVTProfileLevel_H264_High_AutoLevel, reorder: true, lowLatency: false)
sample(name: "vt_main_reorder_640x360", width: 640, height: 360, profile: kVTProfileLevel_H264_Main_AutoLevel, reorder: true, lowLatency: false)
sample(name: "vt_baseline_640x360", width: 640, height: 360, profile: kVTProfileLevel_H264_Baseline_AutoLevel, reorder: false, lowLatency: false)
sample(name: "vt_constrained_high_709_1080p", width: 1920, height: 1080, profile: constrainedHigh, reorder: false, lowLatency: true, colour: true)
sample(name: "vt_constrained_high_2160p", width: 3840, height: 2160, profile: constrainedHigh, reorder: false, lowLatency: true)
sample(name: "vt_high_709_reorder_1080p", width: 1920, height: 1080, profile: kVTProfileLevel_H264_High_AutoLevel, reorder: true, lowLatency: false, colour: true)
