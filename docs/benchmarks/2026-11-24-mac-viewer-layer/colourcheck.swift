// Harness for the macOS viewer's layer presenter: colour, orientation, the
// grey still and the snapshot, all offscreen. `swiftc -O -target
// arm64-apple-macosx14.0 colourcheck.swift -o colourcheck && ./colourcheck`.
//
// 1. Builds a pb-mapper-like stream: 8 sRGB patches converted to NV12 with the
//    BT.709 matrix, video range, no transfer conversion, encoded by
//    VideoToolbox with the VUI tags pb-mapper's Mac encoder sets (709 x 3).
// 2. Decodes it three ways: BGRA (the texture path), 420v with the stream's
//    tags, 420v with the transfer function forced to sRGB (the layer path),
//    and prints the attachments each carries.
// 3. Renders the 420v surfaces with CARenderer and compares them with
//    VideoToolbox's BGRA values, and tries retagging a picture after decode.
// 4. Compares an IOSurface's orientation as layer contents with a CGImage's,
//    and the grey still as a CIColorMatrix filter and as Core Animation
//    compositing filters with GreyStill's matrix applied to the BGRA values.
// 5. Writes both pictures through DesktopVideo.swift's snapshot conversion
//    (VTCreateCGImageFromCVPixelBuffer into a device RGB context).
import AppKit
import CoreImage
import CoreMedia
import CoreVideo
import Foundation
import Metal
import QuartzCore
import VideoToolbox

let w = 256, h = 64
let patches: [(String, (Int, Int, Int))] = [
  ("red", (224, 48, 32)), ("green", (48, 192, 64)), ("blue", (32, 64, 224)), ("grey", (128, 128, 128)),
  ("orange", (240, 160, 16)), ("purple", (144, 48, 192)), ("white", (240, 240, 240)), ("dark", (32, 32, 32)),
]
/// Rows 0..<8 (the top) are white in every column, rows 56..<64 black:
/// tells an upright picture from a flipped one.
func pixel(_ x: Int, _ y: Int) -> (Int, Int, Int) {
  if y < 8 { return (255, 255, 255) }
  if y >= 56 { return (0, 0, 0) }
  return patches[x / 32].1
}

func clamp(_ v: Double) -> UInt8 { UInt8(max(0, min(255, v.rounded()))) }

/// NV12 from sRGB-encoded values with the BT.709 matrix, video range: what a
/// pb-mapper host encodes (no transfer conversion).
func nv12Source() -> CVPixelBuffer {
  var pb: CVPixelBuffer?
  CVPixelBufferCreate(
    nil, w, h, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    [kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any]()] as CFDictionary, &pb)
  let p = pb!
  CVPixelBufferLockBaseAddress(p, [])
  let yBase = CVPixelBufferGetBaseAddressOfPlane(p, 0)!.assumingMemoryBound(to: UInt8.self)
  let yStride = CVPixelBufferGetBytesPerRowOfPlane(p, 0)
  let cBase = CVPixelBufferGetBaseAddressOfPlane(p, 1)!.assumingMemoryBound(to: UInt8.self)
  let cStride = CVPixelBufferGetBytesPerRowOfPlane(p, 1)
  func ycc(_ c: (Int, Int, Int)) -> (Double, Double, Double) {
    let (r, g, b) = (Double(c.0) / 255, Double(c.1) / 255, Double(c.2) / 255)
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b
    let cb = (b - y) / 1.8556
    let cr = (r - y) / 1.5748
    return (16 + 219 * y, 128 + 224 * cb, 128 + 224 * cr)
  }
  for y in 0..<h {
    for x in 0..<w { yBase[y * yStride + x] = clamp(ycc(pixel(x, y)).0) }
  }
  for y in 0..<(h / 2) {
    for x in 0..<(w / 2) {
      let c = ycc(pixel(x * 2, y * 2))
      cBase[y * cStride + x * 2] = clamp(c.1)
      cBase[y * cStride + x * 2 + 1] = clamp(c.2)
    }
  }
  CVPixelBufferUnlockBaseAddress(p, [])
  // Tagged as the encoder's session, so VideoToolbox converts nothing.
  CVBufferSetAttachment(p, kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_709_2, .shouldPropagate)
  CVBufferSetAttachment(p, kCVImageBufferColorPrimariesKey, kCVImageBufferColorPrimaries_ITU_R_709_2, .shouldPropagate)
  CVBufferSetAttachment(p, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_ITU_R_709_2, .shouldPropagate)
  return p
}

final class Collector { var samples: [CMSampleBuffer] = [] }
func encode() -> CMSampleBuffer {
  var session: VTCompressionSession?
  let c = Collector()
  VTCompressionSessionCreate(
    allocator: nil, width: Int32(w), height: Int32(h), codecType: kCMVideoCodecType_H264,
    encoderSpecification: nil, imageBufferAttributes: nil, compressedDataAllocator: nil,
    outputCallback: { r, _, s, _, b in
      if s == 0, let b = b { Unmanaged<Collector>.fromOpaque(r!).takeUnretainedValue().samples.append(b) }
    },
    refcon: Unmanaged.passUnretained(c).toOpaque(), compressionSessionOut: &session)
  let s = session!
  VTSessionSetProperty(s, key: kVTCompressionPropertyKey_YCbCrMatrix, value: kCVImageBufferYCbCrMatrix_ITU_R_709_2)
  VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ColorPrimaries, value: kCVImageBufferColorPrimaries_ITU_R_709_2)
  VTSessionSetProperty(s, key: kVTCompressionPropertyKey_TransferFunction, value: kCVImageBufferTransferFunction_ITU_R_709_2)
  VTSessionSetProperty(s, key: kVTCompressionPropertyKey_Quality, value: NSNumber(value: 1.0))
  VTCompressionSessionEncodeFrame(
    s, imageBuffer: nv12Source(), presentationTimeStamp: .zero, duration: .invalid,
    frameProperties: nil, sourceFrameRefcon: nil, infoFlagsOut: nil)
  VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid)
  return c.samples[0]
}

/// The layer path's format description: transfer sRGB, primaries 709, the
/// stream's matrix (709 when absent), whatever the stream said.
func layerFormat(_ format: CMFormatDescription) -> CMFormatDescription {
  var ext = (CMFormatDescriptionGetExtensions(format) as? [String: Any]) ?? [:]
  ext.removeValue(forKey: kCMFormatDescriptionExtension_VerbatimSampleDescription as String)
  ext.removeValue(forKey: kCMFormatDescriptionExtension_VerbatimISOSampleEntry as String)
  ext[kCMFormatDescriptionExtension_TransferFunction as String] = kCMFormatDescriptionTransferFunction_sRGB
  ext[kCMFormatDescriptionExtension_ColorPrimaries as String] = kCMFormatDescriptionColorPrimaries_ITU_R_709_2
  if ext[kCMFormatDescriptionExtension_YCbCrMatrix as String] == nil {
    ext[kCMFormatDescriptionExtension_YCbCrMatrix as String] = kCMFormatDescriptionYCbCrMatrix_ITU_R_709_2
  }
  let d = CMVideoFormatDescriptionGetDimensions(format)
  var tagged: CMFormatDescription?
  CMVideoFormatDescriptionCreate(
    allocator: nil, codecType: CMFormatDescriptionGetMediaSubType(format), width: d.width, height: d.height,
    extensions: ext as CFDictionary, formatDescriptionOut: &tagged)
  return tagged!
}

final class Out { var image: CVImageBuffer? }
func decode(_ sample: CMSampleBuffer, _ pixelFormat: OSType, srgb: Bool) -> CVPixelBuffer {
  var fd = CMSampleBufferGetFormatDescription(sample)!
  if srgb { fd = layerFormat(fd) }
  let out = Out()
  var cb = VTDecompressionOutputCallbackRecord(
    decompressionOutputCallback: { r, _, _, _, img, _, _ in
      Unmanaged<Out>.fromOpaque(r!).takeUnretainedValue().image = img
    },
    decompressionOutputRefCon: Unmanaged.passUnretained(out).toOpaque())
  var session: VTDecompressionSession?
  let attrs: [String: Any] = [
    kCVPixelBufferPixelFormatTypeKey as String: pixelFormat,
    kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
    kCVPixelBufferMetalCompatibilityKey as String: true,
  ]
  VTDecompressionSessionCreate(
    allocator: nil, formatDescription: fd, decoderSpecification: nil,
    imageBufferAttributes: attrs as CFDictionary, outputCallback: &cb, decompressionSessionOut: &session)
  var size = CMSampleBufferGetTotalSampleSize(sample)
  var rebuilt: CMSampleBuffer?
  CMSampleBufferCreateReady(
    allocator: nil, dataBuffer: CMSampleBufferGetDataBuffer(sample), formatDescription: fd,
    sampleCount: 1, sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1,
    sampleSizeArray: &size, sampleBufferOut: &rebuilt)
  let st = VTDecompressionSessionDecodeFrame(
    session!, sampleBuffer: rebuilt!, flags: [], frameRefcon: nil, infoFlagsOut: nil)
  if st != 0 || out.image == nil { fatalError("decode \(st)") }
  return out.image!
}

func centre(_ i: Int) -> (Int, Int) { (i * 32 + 16, 32) }

func readBGRA(_ p: CVPixelBuffer) -> [(Int, Int, Int)] {
  CVPixelBufferLockBaseAddress(p, .readOnly)
  defer { CVPixelBufferUnlockBaseAddress(p, .readOnly) }
  let base = CVPixelBufferGetBaseAddress(p)!.assumingMemoryBound(to: UInt8.self)
  let bpr = CVPixelBufferGetBytesPerRow(p)
  return patches.indices.map { i in
    let (x, y) = centre(i)
    let o = y * bpr + x * 4
    return (Int(base[o + 2]), Int(base[o + 1]), Int(base[o]))
  }
}

func readLuma(_ p: CVPixelBuffer) -> [Int] {
  CVPixelBufferLockBaseAddress(p, .readOnly)
  defer { CVPixelBufferUnlockBaseAddress(p, .readOnly) }
  let base = CVPixelBufferGetBaseAddressOfPlane(p, 0)!.assumingMemoryBound(to: UInt8.self)
  let bpr = CVPixelBufferGetBytesPerRowOfPlane(p, 0)
  return patches.indices.map { i in let (x, y) = centre(i); return Int(base[y * bpr + x]) }
}

let device = MTLCreateSystemDefaultDevice()!
func renderOffscreen(_ surface: IOSurfaceRef) -> [(Int, Int, Int)] {
  let desc = MTLTextureDescriptor.texture2DDescriptor(
    pixelFormat: .bgra8Unorm, width: w, height: h, mipmapped: false)
  desc.usage = [.renderTarget, .shaderRead, .shaderWrite]
  desc.storageMode = .shared
  let tex = device.makeTexture(descriptor: desc)!
  let renderer = CARenderer(
    mtlTexture: tex, options: [kCARendererColorSpace: CGColorSpace(name: CGColorSpace.sRGB)!])
  let layer = CALayer()
  layer.frame = CGRect(x: 0, y: 0, width: w, height: h)
  layer.contents = surface
  layer.contentsGravity = .resize
  CATransaction.begin()
  renderer.layer = layer
  renderer.bounds = layer.frame
  CATransaction.commit()
  CATransaction.flush()
  renderer.beginFrame(atTime: CACurrentMediaTime(), timeStamp: nil)
  renderer.addUpdate(renderer.bounds)
  renderer.render()
  renderer.endFrame()
  Thread.sleep(forTimeInterval: 0.2)
  var bytes = [UInt8](repeating: 0, count: w * h * 4)
  tex.getBytes(&bytes, bytesPerRow: w * 4, from: MTLRegionMake2D(0, 0, w, h), mipmapLevel: 0)
  return patches.indices.map { i in
    let (x, y) = centre(i)
    let o = y * w * 4 + x * 4
    return (Int(bytes[o + 2]), Int(bytes[o + 1]), Int(bytes[o]))
  }
}

func attachments(_ p: CVPixelBuffer) -> String {
  let keys = [kCVImageBufferYCbCrMatrixKey, kCVImageBufferColorPrimariesKey, kCVImageBufferTransferFunctionKey]
  let cv = keys.map { "\(CVBufferCopyAttachment(p, $0, nil) ?? "-" as CFTypeRef)" }.joined(separator: ", ")
  let surface = CVPixelBufferGetIOSurface(p)!.takeUnretainedValue()
  let all = (IOSurfaceCopyAllValues(surface) as? [String: Any]) ?? [:]
  let io = all.keys.sorted().filter { $0.contains("Color") || $0.contains("Transfer") || $0.contains("YCbCr") || $0.contains("CVAttach") }
    .map { "\($0)=\(String(describing: all[$0]!).replacingOccurrences(of: "\n", with: " "))" }
  return "CV[\(cv)] IOSurface[\(io.joined(separator: "; "))]"
}

func fmt(_ v: [(Int, Int, Int)]) -> String { v.map { "\($0.0),\($0.1),\($0.2)" }.joined(separator: " | ") }
func maxDiff(_ a: [(Int, Int, Int)], _ b: [(Int, Int, Int)]) -> Int {
  zip(a, b).map { max(abs($0.0 - $1.0), abs($0.1 - $1.1), abs($0.2 - $1.2)) }.max() ?? 0
}

// MARK: Offscreen

let sample = encode()
let bgraBuffer = decode(sample, kCVPixelFormatType_32BGRA, srgb: false)
let bgra = readBGRA(bgraBuffer)
let yuvStream = decode(sample, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, srgb: false)
let yuvLayer = decode(sample, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, srgb: true)
let source = nv12Source()
print("source luma      ", readLuma(source))
print("decoded luma     ", readLuma(yuvLayer), "(no transfer conversion at encode when equal)")
print("attachments BGRA        :", attachments(bgraBuffer))
print("attachments 420v stream :", attachments(yuvStream))
print("attachments 420v layer  :", attachments(yuvLayer))
print("sRGB values      ", fmt(patches.map { $0.1 }))
print("VT-BGRA (today)  ", fmt(bgra))
let caStream = renderOffscreen(CVPixelBufferGetIOSurface(yuvStream)!.takeUnretainedValue())
let caLayer = renderOffscreen(CVPixelBufferGetIOSurface(yuvLayer)!.takeUnretainedValue())
print("CA 420v (709 tf) ", fmt(caStream), " max diff vs VT-BGRA", maxDiff(caStream, bgra))
print("CA 420v (sRGB tf)", fmt(caLayer), " max diff vs VT-BGRA", maxDiff(caLayer, bgra))
// The fallback: a 709-tagged picture retagged per picture.
let retag = decode(sample, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, srgb: false)
CVBufferSetAttachment(retag, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_sRGB, .shouldPropagate)
print("attachments retagged    :", attachments(retag))
let caRetag = renderOffscreen(CVPixelBufferGetIOSurface(retag)!.takeUnretainedValue())
print("CA 420v (retagged)", fmt(caRetag), " max diff vs VT-BGRA", maxDiff(caRetag, bgra))

// MARK: Fallback retagging, orientation and the grey still (offscreen)

/// Renders [build]'s layer tree offscreen; returns the 8 patch centres plus
/// the top band (row 2) and the bottom band (row 61), in layer-tree order of
/// rows as the texture holds them.
func renderTree(_ build: (CALayer) -> Void) -> [(Int, Int, Int)] {
  let desc = MTLTextureDescriptor.texture2DDescriptor(
    pixelFormat: .bgra8Unorm, width: w, height: h, mipmapped: false)
  desc.usage = [.renderTarget, .shaderRead, .shaderWrite]
  desc.storageMode = .shared
  let tex = device.makeTexture(descriptor: desc)!
  let renderer = CARenderer(
    mtlTexture: tex, options: [kCARendererColorSpace: CGColorSpace(name: CGColorSpace.sRGB)!])
  let root = CALayer()
  root.frame = CGRect(x: 0, y: 0, width: w, height: h)
  CATransaction.begin()
  build(root)
  renderer.layer = root
  renderer.bounds = root.frame
  CATransaction.commit()
  CATransaction.flush()
  renderer.beginFrame(atTime: CACurrentMediaTime(), timeStamp: nil)
  renderer.addUpdate(renderer.bounds)
  renderer.render()
  renderer.endFrame()
  Thread.sleep(forTimeInterval: 0.2)
  var bytes = [UInt8](repeating: 0, count: w * h * 4)
  tex.getBytes(&bytes, bytesPerRow: w * 4, from: MTLRegionMake2D(0, 0, w, h), mipmapLevel: 0)
  func at(_ x: Int, _ y: Int) -> (Int, Int, Int) {
    let o = y * w * 4 + x * 4
    return (Int(bytes[o + 2]), Int(bytes[o + 1]), Int(bytes[o]))
  }
  var values = patches.indices.map { at(centre($0).0, centre($0).1) }
  values.append(at(128, 2))
  values.append(at(128, 61))
  return values
}

func contentLayer(_ root: CALayer, _ contents: Any) -> CALayer {
  let content = CALayer()
  content.frame = root.bounds
  content.contentsGravity = .resize
  content.isOpaque = true
  content.contents = contents
  root.addSublayer(content)
  return content
}

func srgbImage(_ p: CVPixelBuffer) -> CGImage {
  CVPixelBufferLockBaseAddress(p, .readOnly)
  defer { CVPixelBufferUnlockBaseAddress(p, .readOnly) }
  let ctx = CGContext(
    data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
    space: CGColorSpace(name: CGColorSpace.sRGB)!,
    bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)!
  let dst = ctx.data!.assumingMemoryBound(to: UInt8.self)
  let src = CVPixelBufferGetBaseAddress(p)!.assumingMemoryBound(to: UInt8.self)
  let bpr = CVPixelBufferGetBytesPerRow(p)
  for y in 0..<h { memcpy(dst + y * w * 4, src + y * bpr, w * 4) }
  return ctx.makeImage()!
}

let image = srgbImage(bgraBuffer)
let surface = CVPixelBufferGetIOSurface(yuvLayer)!.takeUnretainedValue()
let viaImage = renderTree { root in _ = contentLayer(root, image) }
let viaSurface = renderTree { root in _ = contentLayer(root, surface) }
print("orientation: CGImage contents top band", viaImage[8], "bottom band", viaImage[9])
print("orientation: 420v surface top band    ", viaSurface[8], "bottom band", viaSurface[9])
print("CA CGImage(sRGB) of VT-BGRA", fmt(Array(viaImage.prefix(8))))
print("CA 420v layer path         ", fmt(Array(viaSurface.prefix(8))), " max diff", maxDiff(Array(viaSurface.prefix(8)), Array(viaImage.prefix(8))))

// Fallback retagging options for a picture VideoToolbox left 709-tagged.
func retagged(_ apply: (CVPixelBuffer) -> Void) -> [(Int, Int, Int)] {
  let p = decode(sample, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, srgb: false)
  apply(p)
  print("   ", attachments(p))
  return Array(renderTree { root in _ = contentLayer(root, CVPixelBufferGetIOSurface(p)!.takeUnretainedValue()) }.prefix(8))
}
let srgbSpace = CGColorSpace(name: CGColorSpace.sRGB)!
let r1 = retagged { p in
  CVBufferSetAttachment(p, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_sRGB, .shouldPropagate)
  CVBufferRemoveAttachment(p, kCVImageBufferCGColorSpaceKey)
}
print("retag transfer, drop CGColorSpace     ", fmt(r1), " max diff", maxDiff(r1, Array(viaImage.prefix(8))))
let r2 = retagged { p in
  CVBufferSetAttachment(p, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_sRGB, .shouldPropagate)
  CVBufferSetAttachment(p, kCVImageBufferCGColorSpaceKey, srgbSpace, .shouldPropagate)
}
print("retag transfer + CGColorSpace sRGB    ", fmt(r2), " max diff", maxDiff(r2, Array(viaImage.prefix(8))))
let r3 = retagged { p in
  CVBufferSetAttachment(p, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_sRGB, .shouldPropagate)
  IOSurfaceSetValue(CVPixelBufferGetIOSurface(p)!.takeUnretainedValue(), kIOSurfaceColorSpace, CGColorSpace.sRGB)
}
print("retag transfer + IOSurfaceColorSpace  ", fmt(r3), " max diff", maxDiff(r3, Array(viaImage.prefix(8))))
let r4 = retagged { p in
  CVBufferSetAttachment(p, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_sRGB, .shouldPropagate)
  if let attachments = CVBufferCopyAttachments(p, .shouldPropagate),
    let space = CVImageBufferCreateColorSpaceFromAttachments(attachments)
  {
    CVBufferSetAttachment(p, kCVImageBufferCGColorSpaceKey, space.takeRetainedValue(), .shouldPropagate)
  }
}
print("retag transfer + space from attachments", fmt(r4), " max diff", maxDiff(r4, Array(viaImage.prefix(8))))

// The grey still.
let expected = viaImage.prefix(8).map { c -> (Int, Int, Int) in
  let g = Int((0.1063 * Double(c.0) + 0.3576 * Double(c.1) + 0.0361 * Double(c.2)).rounded())
  return (g, g, g)
}
print("grey expected (GreyStill's matrix)", fmt(expected))
let matrix = CIFilter(name: "CIColorMatrix")!
matrix.setValue(CIVector(x: 0.1063, y: 0.3576, z: 0.0361, w: 0), forKey: "inputRVector")
matrix.setValue(CIVector(x: 0.1063, y: 0.3576, z: 0.0361, w: 0), forKey: "inputGVector")
matrix.setValue(CIVector(x: 0.1063, y: 0.3576, z: 0.0361, w: 0), forKey: "inputBVector")
matrix.setValue(CIVector(x: 0, y: 0, z: 0, w: 1), forKey: "inputAVector")
let ci = renderTree { root in contentLayer(root, surface).filters = [matrix] }
print("grey CIColorMatrix on content     ", fmt(Array(ci.prefix(8))), " max diff", maxDiff(Array(ci.prefix(8)), expected))
for mode in ["saturationBlendMode", "colorBlendMode"] {
  let blend = renderTree { root in
    _ = contentLayer(root, surface)
    let desaturate = CALayer()
    desaturate.frame = root.bounds
    desaturate.backgroundColor = CGColor(gray: 0.5, alpha: 1)
    desaturate.compositingFilter = mode
    let halve = CALayer()
    halve.frame = root.bounds
    halve.backgroundColor = CGColor(gray: 0, alpha: 0.5)
    root.addSublayer(desaturate)
    root.addSublayer(halve)
  }
  print("grey \(mode) + 50% black", fmt(Array(blend.prefix(8))), " max diff", maxDiff(Array(blend.prefix(8)), expected))
}
// MARK: Snapshot colour: writeSnapshot's path (VTCreateCGImageFromCVPixelBuffer,
// drawn into a device RGB context) for today's BGRA and the layer's 420v.

func snapshotValues(_ buffer: CVPixelBuffer) -> [(Int, Int, Int)]? {
  var image: CGImage?
  guard VTCreateCGImageFromCVPixelBuffer(buffer, options: nil, imageOut: &image) == 0, let picture = image
  else { return nil }
  let info = CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
  let context = CGContext(
    data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
    space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: info)!
  context.draw(picture, in: CGRect(x: 0, y: 0, width: w, height: h))
  let data = context.data!.assumingMemoryBound(to: UInt8.self)
  return patches.indices.map { i in
    let (x, y) = centre(i)
    let o = y * w * 4 + x * 4
    return (Int(data[o + 2]), Int(data[o + 1]), Int(data[o]))
  }
}

if let today = snapshotValues(bgraBuffer), let layer = snapshotValues(yuvLayer) {
  print("snapshot, BGRA (texture)  ", fmt(today))
  print("snapshot, 420v (layer)    ", fmt(layer), " max diff", maxDiff(layer, today))
  print("snapshot, 420v vs sRGB src max diff", maxDiff(layer, patches.map { $0.1 }),
    "; BGRA vs sRGB src", maxDiff(today, patches.map { $0.1 }))
} else {
  print("snapshot: VTCreateCGImageFromCVPixelBuffer failed")
}
