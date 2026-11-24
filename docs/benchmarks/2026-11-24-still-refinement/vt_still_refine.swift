// Still-picture refinement on VideoToolbox, set up as pb-mapper's Mac host
// encoder is (platform/macos/encoder.rs): hardware, low-latency rate
// control, real time, no reordering, H.264 Constrained High or H.265 Main,
// an unbounded GOP, the nominal 60 FPS, 4 Mbit/s with a cap of 1.5 times
// the mean over 1 s, BT.709, and wall-clock sample times in microseconds.
//
// A page of text scrolls 4 rows a frame at 60 FPS for 1 s (at most four
// pictures in the encoder), then stops, and the last picture is submitted
// again for 2 s after the last new one, as pb-mapper schedules it: the
// liveness repeat every 250 ms from 250 ms (arm `liveness`); or a
// refinement burst of `repeats` a period of the pace apart from 200 ms,
// then liveness 250 ms after its last (arm `burst`; the full cap, with no
// stop rule applied). Each unit's time after the stop, size and PSNR, from
// the luma mean squared error VideoToolbox computes against its input, are
// printed, with the PSNR the viewer shows at 250, 500 and 1000 ms (the last
// unit submitted by then) and how long after the stop the picture first
// came within 1 and 0.5 dB of its best.
//
// swiftc -O vt_still_refine.swift -o vt_still_refine && ./vt_still_refine [trials]
import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

setvbuf(stdout, nil, _IOLBF, 0)

let width = 1920, height = 1080, fps = 60, bitrate = 4_000_000
let repeats = 15

func now() -> UInt64 { DispatchTime.now().uptimeNanoseconds }

final class Outputs {
    let lock = NSLock()
    var sent: [Int: UInt64] = [:]
    /// Per unit, by its token: bytes and luma MSE (-1 without one).
    var units: [Int: (Int, Double)] = [:]
    var dropped = 0
    var inFlight: Int { lock.lock(); defer { lock.unlock() }; return sent.count }
}

let callback: VTCompressionOutputCallback = { refcon, frameRefcon, status, flags, sample in
    let outputs = Unmanaged<Outputs>.fromOpaque(refcon!).takeUnretainedValue()
    let token = Int(bitPattern: frameRefcon)
    outputs.lock.lock()
    defer { outputs.lock.unlock() }
    outputs.sent.removeValue(forKey: token)
    guard status == 0, let sample = sample, !flags.contains(.frameDropped) else {
        outputs.dropped += 1
        return
    }
    var mse = -1.0
    if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false)
        as? [[CFString: Any]], let first = attachments.first,
        let metrics = first[kVTSampleAttachmentKey_QualityMetrics] as? [CFString: Any],
        let luma = metrics[kVTSampleAttachmentQualityMetricsKey_LumaMeanSquaredError] as? Double
    {
        mse = luma
    }
    outputs.units[token] = (CMSampleBufferGetTotalSampleSize(sample), mse)
}

enum Codec {
    case h264, hevc
    var type: CMVideoCodecType { self == .h264 ? kCMVideoCodecType_H264 : kCMVideoCodecType_HEVC }
    var name: String { self == .h264 ? "h264" : "h265" }
}

func session(_ codec: Codec, _ outputs: Outputs) -> VTCompressionSession? {
    let spec: [CFString: Any] = [
        kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: true,
        kVTVideoEncoderSpecification_EnableLowLatencyRateControl: true,
    ]
    var made: VTCompressionSession?
    let status = VTCompressionSessionCreate(
        allocator: nil, width: Int32(width), height: Int32(height), codecType: codec.type,
        encoderSpecification: spec as CFDictionary, imageBufferAttributes: nil,
        compressedDataAllocator: nil, outputCallback: callback,
        refcon: Unmanaged.passUnretained(outputs).toOpaque(), compressionSessionOut: &made)
    guard status == 0, let session = made else {
        print("create \(codec.name) failed: \(status)")
        return nil
    }
    func set(_ key: CFString, _ value: Any) {
        let status = VTSessionSetProperty(session, key: key, value: value as CFTypeRef)
        if status != 0 { print("set \(key) failed: \(status)") }
    }
    set(kVTCompressionPropertyKey_RealTime, true)
    set(kVTCompressionPropertyKey_AllowFrameReordering, false)
    set(kVTCompressionPropertyKey_ProfileLevel,
        codec == .h264 ? kVTProfileLevel_H264_ConstrainedHigh_AutoLevel : kVTProfileLevel_HEVC_Main_AutoLevel)
    set(kVTCompressionPropertyKey_MaxKeyFrameInterval, Int(Int32.max))
    set(kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, 0.0)
    set(kVTCompressionPropertyKey_ExpectedFrameRate, fps)
    set(kVTCompressionPropertyKey_AverageBitRate, bitrate)
    set(kVTCompressionPropertyKey_DataRateLimits, [bitrate / 8 * 3 / 2, 1.0] as CFArray)
    set(kVTCompressionPropertyKey_ColorPrimaries, kCVImageBufferColorPrimaries_ITU_R_709_2)
    set(kVTCompressionPropertyKey_TransferFunction, kCVImageBufferTransferFunction_ITU_R_709_2)
    set(kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_709_2)
    set(kVTCompressionPropertyKey_CalculateMeanSquaredError, true)
    VTCompressionSessionPrepareToEncodeFrames(session)
    return session
}

/// A page of glyph-like dark marks on white, four screens tall.
struct Page {
    let rows = height * 4
    var luma: [UInt8]
    init() {
        luma = [UInt8](repeating: 235, count: width * height * 4)
        var x: UInt64 = 0x9E37_79B9_7F4A_7C15
        func next() -> UInt64 { x ^= x << 13; x ^= x >> 7; x ^= x << 17; return x }
        let line = 18, glyph = 9
        var y = 0
        while y + line <= rows {
            let length = Int(next() % UInt64(width - 40)) + 40
            var gx = 20
            while gx + glyph < min(length, width - 20) {
                let bits = next()
                if bits & 7 != 0 {
                    for gy in 2..<(line - 4) {
                        for px in 1..<(glyph - 1) where (bits >> UInt64((gy * 3 + px) % 60)) & 1 == 1 {
                            luma[(y + gy) * width + gx + px] = 16
                        }
                    }
                }
                gx += glyph
            }
            y += line
        }
    }
    func paint(_ buffer: CVPixelBuffer, scrolled: Int) {
        CVPixelBufferLockBaseAddress(buffer, [])
        let y = CVPixelBufferGetBaseAddressOfPlane(buffer, 0)!.assumingMemoryBound(to: UInt8.self)
        let yStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        let uv = CVPixelBufferGetBaseAddressOfPlane(buffer, 1)!.assumingMemoryBound(to: UInt8.self)
        let uvStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
        let top = scrolled % (rows - height)
        luma.withUnsafeBufferPointer { page in
            for row in 0..<height { memcpy(y + row * yStride, page.baseAddress! + (top + row) * width, width) }
        }
        for row in 0..<(height / 2) { memset(uv + row * uvStride, 128, width) }
        CVPixelBufferUnlockBaseAddress(buffer, [])
    }
}

func pixelBuffer() -> CVPixelBuffer {
    var buffer: CVPixelBuffer?
    let attributes: [CFString: Any] = [kCVPixelBufferIOSurfacePropertiesKey: [:] as [CFString: Any]]
    CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                        attributes as CFDictionary, &buffer)
    return buffer!
}

func psnr(_ mse: Double) -> Double { mse > 0 ? 10 * log10(255 * 255 / mse) : .infinity }

/// When the last picture is submitted again, in ms after the last new one:
/// the liveness repeat every 250 ms; or a refinement burst of `repeats` a
/// period of the pace apart from 200 ms, then liveness after its last.
enum Arm {
    case liveness, burst
    var name: String { self == .liveness ? "liveness" : "burst" }
    func schedule() -> [Double] {
        var times: [Double] = []
        var at = 250.0
        if self == .burst {
            times = (0..<repeats).map { 200 + Double($0) * 1000 / Double(fps) }
            at = times.last! + 250
        }
        while at <= span { times.append(at); at += 250 }
        return times
    }
}
/// How long after the last new picture the run goes on.
let span = 2000.0

/// One trial: scroll for a second, stop, then repeat as `arm` does.
func trial(_ codec: Codec, _ page: Page, _ arm: Arm) -> String {
    let outputs = Outputs()
    guard let session = session(codec, outputs) else { return "no session" }
    defer { VTCompressionSessionInvalidate(session) }
    let ring = (0..<6).map { _ in pixelBuffer() }
    let start = now()
    var token = 0
    var last = ring[0]
    var pts: Int64 = -1
    func submit(_ buffer: CVPixelBuffer, keyframe: Bool) -> UInt64 {
        token += 1
        let at = now()
        // Wall-clock microseconds since the session, strictly rising.
        pts = max(pts + 1, Int64((at - start) / 1000))
        let options: CFDictionary? = keyframe
            ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
        outputs.lock.lock(); outputs.sent[token] = at; outputs.lock.unlock()
        VTCompressionSessionEncodeFrame(
            session, imageBuffer: buffer, presentationTimeStamp: CMTime(value: pts, timescale: 1_000_000),
            duration: .invalid, frameProperties: options,
            sourceFrameRefcon: UnsafeMutableRawPointer(bitPattern: token), infoFlagsOut: nil)
        return at
    }
    let period = UInt64(1e9 / Double(fps))
    var next = now()
    var stopped = now()
    for frame in 0..<fps {
        while outputs.inFlight >= 4 { usleep(500) }
        let buffer = ring[frame % ring.count]
        page.paint(buffer, scrolled: frame * 4)
        stopped = submit(buffer, keyframe: frame == 0)
        last = buffer
        next += period
        let wait = Int64(next) - Int64(now())
        if wait > 0 { usleep(UInt32(wait / 1000)) }
    }
    let scrolled = token
    var times: [Double] = []
    for due in arm.schedule() {
        let wait = Int64(stopped + UInt64(due * 1e6)) - Int64(now())
        if wait > 0 { usleep(UInt32(wait / 1000)) }
        times.append(Double(submit(last, keyframe: false) - stopped) / 1e6)
    }
    VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
    outputs.lock.lock()
    defer { outputs.lock.unlock() }
    var units: [String] = []
    var measured: [(Double, Double)] = []
    var bytes = 0
    for (n, at) in times.enumerated() {
        guard let (size, mse) = outputs.units[scrolled + 1 + n] else {
            units.append(String(format: "%.0f:dropped", at))
            continue
        }
        bytes += size
        units.append(String(format: "%.0f:%dB/%.1fdB", at, size, psnr(mse)))
        if mse > 0 { measured.append((at, psnr(mse))) }
    }
    let best = measured.map(\.1).max() ?? .nan
    func within(_ db: Double) -> String {
        measured.first { $0.1 >= best - db }.map { String(format: "%.0f", $0.0) } ?? "none"
    }
    /// The PSNR the viewer shows at `ms`: the last unit submitted by then.
    func shown(_ ms: Double) -> String {
        measured.last { $0.0 <= ms }.map { String(format: "%.1f", $0.1) } ?? "none"
    }
    let scroll = outputs.units[scrolled].map { String(format: "%dB/%.1fdB", $0.0, psnr($0.1)) } ?? "dropped"
    return "last_scroll=\(scroll) repeat_bytes=\(bytes) db_at_250ms=\(shown(250)) db_at_500ms=\(shown(500)) db_at_1000ms=\(shown(1000)) best_db=\(String(format: "%.1f", best)) within_1db_ms=\(within(1)) within_0.5db_ms=\(within(0.5)) dropped=\(outputs.dropped) units(ms:bytes/psnr): \(units.joined(separator: " "))"
}

let trials = CommandLine.arguments.count > 1 ? Int(CommandLine.arguments[1]) ?? 3 : 3
let page = Page()
print("vt_still_refine \(width)x\(height) nominal \(fps) bitrate \(bitrate) low_latency wall_pts burst_repeats \(repeats) span_ms \(Int(span))")
for codec in [Codec.h264, .hevc] {
    for arm in [Arm.liveness, .burst] {
        for n in 1...trials {
            print("\(codec.name) arm=\(arm.name) trial=\(n) \(trial(codec, page, arm))")
        }
    }
}
