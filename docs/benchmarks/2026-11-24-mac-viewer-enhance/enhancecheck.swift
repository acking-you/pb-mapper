// The macOS viewer's picture enhancement, offscreen and in a window, with
// the runner's own code: run.sh appends this file to a copy of
// ui/darwin/DesktopVideo.swift as main.swift, so the file-private shaders,
// EnhanceGpu, EnhancedPresenter and LayerPresenter are the ones the app
// ships. It links FlutterMacOS only because that file's plugin does.
//
// 1. Compiles the shaders as the runner does (makeLibrary(source:)).
// 2. Draws desktop-like test pictures (text from 9 to 14 points at one
//    point a pixel, thin rules, coloured bars), converted to 420v as a
//    pb-mapper host does: sRGB values through the BT.709 matrix, video
//    range, 2x2 chroma, no transfer conversion.
// 3. Times each pass on the GPU (gpuStartTime to gpuEndTime of a command
//    buffer per pass) and both passes in one command buffer as the runner
//    encodes them, with the CPU's encode-commit-schedule time, for each
//    size and level.
// 4. Renders the plain layer (the 420v surface as a CALayer's contents,
//    linear filter) with CARenderer at the same size, and compares: colour
//    at 1:1 with no sharpening, no ringing at an edge, a flat field left
//    flat, and a sharpness figure (mean luma gradient over the text), with
//    PNG crops for the eye.
// 5. Places the metal layer on whole pixels (pixelPlacement) for a view
//    under a hierarchy like Flutter's, at fractional points, at rest,
//    moved and zoomed, and checks that setting a view's frame unchanged,
//    as Flutter does each frame, calls its setFrameSize:.
// 6. Runs LayerPresenter with its enhancement in a window: plain commits,
//    the switch, the pictures before the reveal, a feeder thread handing
//    pictures at 120 a second, a retarget, the switch back and again, a
//    reveal from a released enhancement, the end; how long the feeder's
//    hand-off takes, how many pictures the presenter thread presented,
//    and when it revealed.
// 7. Checks when KVO on a layer's sublayerTransform fires, and that
//    CALayer.convert(_:to:) takes the superlayer's sublayerTransform in.
//
// Usage: run.sh [OUT_DIR]; the crops go to OUT_DIR (default: ./crops).
// ENHANCECHECK_ONLY=presenter runs 1, 5, 6 and 7 only.

private func say(_ line: String) {
  print(line)
  fflush(stdout)
}

private let outputDirectory = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "crops"

// MARK: Test pictures

/// BGRA, sRGB, premultiplied first, little-endian: the bytes and the size.
private struct Bgra {
  let width: Int
  let height: Int
  var bytes: [UInt8]
}

/// A desktop-like picture: window chrome, text at several sizes, rules.
private func desktopPicture(_ width: Int, _ height: Int) -> Bgra {
  let space = CGColorSpace(name: CGColorSpace.sRGB)!
  let info = CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
  let context = CGContext(
    data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4, space: space,
    bitmapInfo: info)!
  context.setFillColor(CGColor(red: 0.93, green: 0.94, blue: 0.95, alpha: 1))
  context.fill(CGRect(x: 0, y: 0, width: width, height: height))
  let graphics = NSGraphicsContext(cgContext: context, flipped: true)
  NSGraphicsContext.saveGraphicsState()
  NSGraphicsContext.current = graphics
  // Flipped: y down from the top, as the picture.
  context.translateBy(x: 0, y: CGFloat(height))
  context.scaleBy(x: 1, y: -1)
  // A window: title bar, white page, text.
  let window = CGRect(x: 40, y: 40, width: CGFloat(width) - 80, height: CGFloat(height) - 80)
  NSColor(srgbRed: 0.16, green: 0.36, blue: 0.70, alpha: 1).setFill()
  NSRect(x: window.minX, y: window.minY, width: window.width, height: 30).fill()
  NSColor.white.setFill()
  NSRect(x: window.minX, y: window.minY + 30, width: window.width, height: window.height - 30).fill()
  let title: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: 13, weight: .semibold), .foregroundColor: NSColor.white,
  ]
  ("Quarterly report.docx - Editor" as NSString).draw(
    at: NSPoint(x: window.minX + 12, y: window.minY + 7), withAttributes: title)
  let lines = [
    "The quick brown fox jumps over the lazy dog. 0123456789 (){}[]<>/\\|;:'\",.?!@#$%^&*",
    "Remote desktop text at one point a pixel: Ilm1| rn m vv w cl d oO0 S5 B8 Z2 ;:,. il!1",
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt.",
    "fn main() { let total: u64 = items.iter().map(|x| x.len() as u64).sum(); println!(\"{total}\"); }",
  ]
  var y = window.minY + 48
  for size in [9.0, 10.0, 11.0, 12.0, 13.0, 14.0] as [CGFloat] {
    for (index, line) in lines.enumerated() {
      let attributes: [NSAttributedString.Key: Any] = [
        .font: index == 3
          ? NSFont.monospacedSystemFont(ofSize: size, weight: .regular) : NSFont.systemFont(ofSize: size),
        .foregroundColor: index == 2 ? NSColor(srgbRed: 0.75, green: 0.1, blue: 0.1, alpha: 1) : NSColor.black,
      ]
      (line as NSString).draw(at: NSPoint(x: window.minX + 16, y: y), withAttributes: attributes)
      y += size * 1.5
      if y > window.maxY - 40 { break }
    }
  }
  // Rules one and two pixels wide, and a grid.
  NSColor(white: 0.55, alpha: 1).setFill()
  for i in 0..<12 {
    NSRect(x: window.minX + 16 + CGFloat(i) * 24, y: window.maxY - 30, width: 1, height: 20).fill()
    NSRect(x: window.minX + 320 + CGFloat(i) * 24, y: window.maxY - 30, width: 2, height: 20).fill()
  }
  NSGraphicsContext.restoreGraphicsState()
  let data = context.data!.assumingMemoryBound(to: UInt8.self)
  return Bgra(width: width, height: height, bytes: Array(UnsafeBufferPointer(start: data, count: width * height * 4)))
}

/// Flat patches, a step edge, or one grey: [pixel] gives each pixel's sRGB.
private func patternPicture(_ width: Int, _ height: Int, _ pixel: (Int, Int) -> (UInt8, UInt8, UInt8)) -> Bgra {
  var bytes = [UInt8](repeating: 255, count: width * height * 4)
  for y in 0..<height {
    for x in 0..<width {
      let (r, g, b) = pixel(x, y)
      let o = (y * width + x) * 4
      bytes[o] = b
      bytes[o + 1] = g
      bytes[o + 2] = r
    }
  }
  return Bgra(width: width, height: height, bytes: bytes)
}

private func clampByte(_ v: Double) -> UInt8 { UInt8(max(0, min(255, v.rounded()))) }

/// 420v as a pb-mapper host encodes it, tagged as the layer presenter tags
/// its pictures (BT.709 matrix and primaries, sRGB transfer).
private func to420v(_ image: Bgra) -> CVPixelBuffer {
  let attributes: [String: Any] = [
    kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
    kCVPixelBufferMetalCompatibilityKey as String: true,
  ]
  var made: CVPixelBuffer?
  CVPixelBufferCreate(
    kCFAllocatorDefault, image.width, image.height, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    attributes as CFDictionary, &made)
  let buffer = made!
  CVPixelBufferLockBaseAddress(buffer, [])
  let lumaBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 0)!.assumingMemoryBound(to: UInt8.self)
  let lumaRow = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
  let chromaBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 1)!.assumingMemoryBound(to: UInt8.self)
  let chromaRow = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
  func rgb(_ x: Int, _ y: Int) -> (Double, Double, Double) {
    let o = (y * image.width + x) * 4
    return (Double(image.bytes[o + 2]) / 255, Double(image.bytes[o + 1]) / 255, Double(image.bytes[o]) / 255)
  }
  for y in 0..<image.height {
    for x in 0..<image.width {
      let (r, g, b) = rgb(x, y)
      lumaBase[y * lumaRow + x] = clampByte(16 + 219 * (0.2126 * r + 0.7152 * g + 0.0722 * b))
    }
  }
  for cy in 0..<(image.height / 2) {
    for cx in 0..<(image.width / 2) {
      var (r, g, b) = (0.0, 0.0, 0.0)
      for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let p = rgb(cx * 2 + dx, cy * 2 + dy)
        r += p.0 / 4
        g += p.1 / 4
        b += p.2 / 4
      }
      let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b
      chromaBase[cy * chromaRow + cx * 2] = clampByte(128 + 224 * (b - luma) / 1.8556)
      chromaBase[cy * chromaRow + cx * 2 + 1] = clampByte(128 + 224 * (r - luma) / 1.5748)
    }
  }
  CVPixelBufferUnlockBaseAddress(buffer, [])
  CVBufferSetAttachment(buffer, kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_709_2, .shouldPropagate)
  CVBufferSetAttachment(
    buffer, kCVImageBufferColorPrimariesKey, kCVImageBufferColorPrimaries_ITU_R_709_2, .shouldPropagate)
  CVBufferSetAttachment(
    buffer, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_sRGB, .shouldPropagate)
  return buffer
}

// MARK: Offscreen passes, encoded as EnhanceGpu encodes them

private let device = MTLCreateSystemDefaultDevice()!
say("device: \(device.name)")
private let compileStart = Date()
private let library = try! device.makeLibrary(source: enhanceShaders, options: nil)
say(String(format: "shaders compiled at run time in %.1f ms", Date().timeIntervalSince(compileStart) * 1000))

private func pipeline(_ fragment: String, _ format: MTLPixelFormat) -> MTLRenderPipelineState {
  let descriptor = MTLRenderPipelineDescriptor()
  descriptor.vertexFunction = library.makeFunction(name: "cover")!
  descriptor.fragmentFunction = library.makeFunction(name: fragment)!
  descriptor.colorAttachments[0].pixelFormat = format
  return try! device.makeRenderPipelineState(descriptor: descriptor)
}
private let upscalePipeline = pipeline("upscale", .rgba8Unorm)
private let sharpenImagePipeline = pipeline("sharpenImage", .bgra8Unorm)
private let sharpenPlanesPipeline = pipeline("sharpenPlanes", .bgra8Unorm)
private let queue = device.makeCommandQueue()!
private var textureCache: CVMetalTextureCache?
CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, device, nil, &textureCache)

private func wrapPlane(_ buffer: CVPixelBuffer, _ plane: Int, _ format: MTLPixelFormat) -> (CVMetalTexture, MTLTexture) {
  var made: CVMetalTexture?
  CVMetalTextureCacheCreateTextureFromImage(
    kCFAllocatorDefault, textureCache!, buffer, nil, format, CVPixelBufferGetWidthOfPlane(buffer, plane),
    CVPixelBufferGetHeightOfPlane(buffer, plane), plane, &made)
  return (made!, CVMetalTextureGetTexture(made!)!)
}

private func texture(_ size: PixelSize, _ format: MTLPixelFormat, shared: Bool = false) -> MTLTexture {
  let descriptor = MTLTextureDescriptor.texture2DDescriptor(
    pixelFormat: format, width: size.width, height: size.height, mipmapped: false)
  descriptor.usage = [.renderTarget, .shaderRead]
  descriptor.storageMode = shared ? .shared : .private
  return device.makeTexture(descriptor: descriptor)!
}

private func encodePass(
  _ commands: MTLCommandBuffer, _ pipeline: MTLRenderPipelineState, into target: MTLTexture, _ size: PixelSize,
  _ first: MTLTexture, _ second: MTLTexture?, _ params: inout EnhanceParams
) {
  let descriptor = MTLRenderPassDescriptor()
  descriptor.colorAttachments[0].texture = target
  descriptor.colorAttachments[0].loadAction = .dontCare
  descriptor.colorAttachments[0].storeAction = .store
  let encoder = commands.makeRenderCommandEncoder(descriptor: descriptor)!
  encoder.setRenderPipelineState(pipeline)
  encoder.setViewport(
    MTLViewport(originX: 0, originY: 0, width: Double(size.width), height: Double(size.height), znear: 0, zfar: 1))
  encoder.setFragmentTexture(first, index: 0)
  if let second = second { encoder.setFragmentTexture(second, index: 1) }
  encoder.setFragmentBytes(&params, length: MemoryLayout<EnhanceParams>.stride, index: 0)
  encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
  encoder.endEncoding()
}

private func params(_ buffer: CVPixelBuffer, _ out: PixelSize, peak: Float) -> EnhanceParams {
  let width = Float(CVPixelBufferGetWidth(buffer))
  let height = Float(CVPixelBufferGetHeight(buffer))
  return EnhanceParams(
    size: SIMD4(width, height, width / Float(out.width), height / Float(out.height)),
    range: SIMD4(255.0 / 219.0, 16.0 / 255.0, 128.0 / 255.0, peak),
    chroma: matrixWeights(buffer),
    extent: SIMD4(Float(out.width), Float(out.height), 0, 0))
}

/// The runner's work for one picture into [output]: both passes in one
/// command buffer (one pass at 1:1). The GPU's time, and the CPU's from
/// encoding to the GPU scheduling it.
@discardableResult
private func enhance(_ buffer: CVPixelBuffer, into output: MTLTexture, _ out: PixelSize, peak: Float, image: MTLTexture)
  -> (gpuMs: Double, cpuMs: Double)
{
  let (lumaRef, luma) = wrapPlane(buffer, 0, .r8Unorm)
  let (chromaRef, chroma) = wrapPlane(buffer, 1, .rg8Unorm)
  let started = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
  let commands = queue.makeCommandBuffer()!
  var p = params(buffer, out, peak: peak)
  if out.width == luma.width && out.height == luma.height {
    encodePass(commands, sharpenPlanesPipeline, into: output, out, luma, chroma, &p)
  } else {
    encodePass(commands, upscalePipeline, into: image, out, luma, chroma, &p)
    encodePass(commands, sharpenImagePipeline, into: output, out, image, nil, &p)
  }
  commands.commit()
  commands.waitUntilScheduled()
  let cpu = Double(clock_gettime_nsec_np(CLOCK_UPTIME_RAW) - started) / 1e6
  commands.waitUntilCompleted()
  CVMetalTextureCacheFlush(textureCache!, 0)
  withExtendedLifetime((lumaRef, chromaRef)) {}
  return ((commands.gpuEndTime - commands.gpuStartTime) * 1000, cpu)
}

/// Each pass in a command buffer of its own: the GPU time of each.
private func passTimes(_ buffer: CVPixelBuffer, into output: MTLTexture, _ out: PixelSize, peak: Float, image: MTLTexture)
  -> (upscale: Double?, sharpen: Double)
{
  let (lumaRef, luma) = wrapPlane(buffer, 0, .r8Unorm)
  let (chromaRef, chroma) = wrapPlane(buffer, 1, .rg8Unorm)
  var p = params(buffer, out, peak: peak)
  var first: MTLCommandBuffer?
  let second = queue.makeCommandBuffer()!
  if out.width == luma.width && out.height == luma.height {
    encodePass(second, sharpenPlanesPipeline, into: output, out, luma, chroma, &p)
  } else {
    let one = queue.makeCommandBuffer()!
    encodePass(one, upscalePipeline, into: image, out, luma, chroma, &p)
    one.commit()
    // Done before pass 2 starts, so that neither's time holds the other's.
    one.waitUntilCompleted()
    first = one
    encodePass(second, sharpenImagePipeline, into: output, out, image, nil, &p)
  }
  second.commit()
  second.waitUntilCompleted()
  CVMetalTextureCacheFlush(textureCache!, 0)
  withExtendedLifetime((lumaRef, chromaRef)) {}
  let upscale = first.map { ($0.gpuEndTime - $0.gpuStartTime) * 1000 }
  return (upscale, (second.gpuEndTime - second.gpuStartTime) * 1000)
}

private func percentile(_ values: [Double], _ p: Double) -> Double {
  let sorted = values.sorted()
  return sorted[min(sorted.count - 1, Int((Double(sorted.count - 1) * p).rounded()))]
}

private func summary(_ values: [Double]) -> String {
  String(format: "median %.3f p95 %.3f max %.3f", percentile(values, 0.5), percentile(values, 0.95), values.max()!)
}

// MARK: GPU time

private let levels: [EnhanceLevel] = [.low, .medium, .high]
private let cases: [(String, PixelSize, PixelSize)] = [
  ("1080p on a 4096x2304 display, full screen", PixelSize(width: 1920, height: 1080), PixelSize(width: 4096, height: 2304)),
  ("1440p on a 4096x2304 display, full screen", PixelSize(width: 2560, height: 1440), PixelSize(width: 4096, height: 2304)),
  ("1080p on a 3024x1964 panel, full screen", PixelSize(width: 1920, height: 1080), PixelSize(width: 3024, height: 1701)),
  ("1440p on a 3456x2234 panel, full screen", PixelSize(width: 2560, height: 1440), PixelSize(width: 3456, height: 1944)),
  ("4K on a 4096x2304 display, full screen", PixelSize(width: 3840, height: 2160), PixelSize(width: 4096, height: 2304)),
  ("1080p at 1:1 (sharpening only)", PixelSize(width: 1920, height: 1080), PixelSize(width: 1920, height: 1080)),
]
private let frames = 300
private var pictures: [String: CVPixelBuffer] = [:]
private func key(_ size: PixelSize) -> String { "\(size.width)x\(size.height)" }
// ENHANCECHECK_ONLY=presenter skips the offscreen sections (GPU time,
// colour, ringing, sharpness), whose code the presenter's does not touch.
private let offscreen = ProcessInfo.processInfo.environment["ENHANCECHECK_ONLY"] != "presenter"
if offscreen {
  for (_, source, _) in cases where pictures[key(source)] == nil {
    pictures[key(source)] = to420v(desktopPicture(source.width, source.height))
  }
  say("")
  say("GPU time a picture, ms (\(frames) pictures after 30 to warm up, one at a time)")
}
for (name, source, out) in cases where offscreen {
  let buffer = pictures[key(source)]!
  let output = texture(out, .bgra8Unorm)
  let image = texture(out, .rgba8Unorm)
  say("\(name): \(source.width)x\(source.height) to \(out.width)x\(out.height)")
  for level in levels {
    for _ in 0..<30 { enhance(buffer, into: output, out, peak: level.peak, image: image) }
    var gpu: [Double] = []
    var cpu: [Double] = []
    var upscale: [Double] = []
    var sharpen: [Double] = []
    for _ in 0..<frames {
      let timed = enhance(buffer, into: output, out, peak: level.peak, image: image)
      gpu.append(timed.gpuMs)
      cpu.append(timed.cpuMs)
    }
    for _ in 0..<frames {
      let timed = passTimes(buffer, into: output, out, peak: level.peak, image: image)
      if let u = timed.upscale { upscale.append(u) }
      sharpen.append(timed.sharpen)
    }
    say("  \(level.rawValue): both passes \(summary(gpu)); CPU encode to scheduled \(summary(cpu))")
    if !upscale.isEmpty { say("    pass 1 (upscale) \(summary(upscale))") }
    say("    pass 2 (sharpen) \(summary(sharpen))")
  }
}

// MARK: The plain layer, rendered by Core Animation

/// The 420v surface as a layer's contents at [out], linear filter, as the
/// content layer shows it, rendered into an sRGB target.
private func plainLayer(_ buffer: CVPixelBuffer, _ out: PixelSize) -> [UInt8] {
  let target = texture(out, .bgra8Unorm, shared: true)
  let renderer = CARenderer(
    mtlTexture: target, options: [kCARendererColorSpace: CGColorSpace(name: CGColorSpace.sRGB)!])
  let layer = CALayer()
  layer.frame = CGRect(x: 0, y: 0, width: out.width, height: out.height)
  layer.contents = CVPixelBufferGetIOSurface(buffer)!.takeUnretainedValue()
  layer.contentsGravity = .resize
  layer.magnificationFilter = .linear
  layer.minificationFilter = .linear
  CATransaction.begin()
  renderer.layer = layer
  renderer.bounds = layer.frame
  CATransaction.commit()
  CATransaction.flush()
  renderer.beginFrame(atTime: CACurrentMediaTime(), timeStamp: nil)
  renderer.addUpdate(renderer.bounds)
  renderer.render()
  renderer.endFrame()
  Thread.sleep(forTimeInterval: 0.3)
  // CARenderer's target holds its rows bottom up: the picture's top row
  // is its last.
  let bottomUp = read(target, out)
  var rows = [UInt8](repeating: 0, count: bottomUp.count)
  let stride = out.width * 4
  for y in 0..<out.height {
    rows[(y * stride)..<((y + 1) * stride)] = bottomUp[((out.height - 1 - y) * stride)..<((out.height - y) * stride)]
  }
  return rows
}

private func read(_ source: MTLTexture, _ size: PixelSize) -> [UInt8] {
  var copy = source
  if source.storageMode == .private {
    copy = texture(size, source.pixelFormat, shared: true)
    let commands = queue.makeCommandBuffer()!
    let blit = commands.makeBlitCommandEncoder()!
    blit.copy(
      from: source, sourceSlice: 0, sourceLevel: 0, sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
      sourceSize: MTLSize(width: size.width, height: size.height, depth: 1), to: copy, destinationSlice: 0,
      destinationLevel: 0, destinationOrigin: MTLOrigin(x: 0, y: 0, z: 0))
    blit.endEncoding()
    commands.commit()
    commands.waitUntilCompleted()
  }
  var bytes = [UInt8](repeating: 0, count: size.width * size.height * 4)
  copy.getBytes(
    &bytes, bytesPerRow: size.width * 4, from: MTLRegionMake2D(0, 0, size.width, size.height), mipmapLevel: 0)
  return bytes
}

private func enhanced(_ buffer: CVPixelBuffer, _ out: PixelSize, peak: Float) -> [UInt8] {
  let output = texture(out, .bgra8Unorm)
  enhance(buffer, into: output, out, peak: peak, image: texture(out, .rgba8Unorm))
  return read(output, out)
}

/// BGRA bytes' BT.709 luma, 0 to 255.
private func luma(_ bytes: [UInt8], _ i: Int) -> Double {
  0.2126 * Double(bytes[i * 4 + 2]) + 0.7152 * Double(bytes[i * 4 + 1]) + 0.0722 * Double(bytes[i * 4])
}

/// Mean Sobel gradient of luma over [region].
private func sharpness(_ bytes: [UInt8], _ size: PixelSize, _ region: CGRect) -> Double {
  var total = 0.0
  var count = 0
  for y in Int(region.minY)..<Int(region.maxY) {
    for x in Int(region.minX)..<Int(region.maxX) {
      func l(_ dx: Int, _ dy: Int) -> Double { luma(bytes, (y + dy) * size.width + x + dx) }
      let gx = l(1, -1) + 2 * l(1, 0) + l(1, 1) - l(-1, -1) - 2 * l(-1, 0) - l(-1, 1)
      let gy = l(-1, 1) + 2 * l(0, 1) + l(1, 1) - l(-1, -1) - 2 * l(0, -1) - l(1, -1)
      total += (gx * gx + gy * gy).squareRoot()
      count += 1
    }
  }
  return total / Double(count)
}

private func writePng(_ bytes: [UInt8], _ size: PixelSize, crop: CGRect, name: String) {
  let width = Int(crop.width)
  let height = Int(crop.height)
  var cropped = [UInt8](repeating: 0, count: width * height * 4)
  for y in 0..<height {
    for x in 0..<width {
      let s = ((Int(crop.minY) + y) * size.width + Int(crop.minX) + x) * 4
      let d = (y * width + x) * 4
      cropped[d] = bytes[s]
      cropped[d + 1] = bytes[s + 1]
      cropped[d + 2] = bytes[s + 2]
      cropped[d + 3] = 255
    }
  }
  let info = CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
  let context = CGContext(
    data: &cropped, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
    space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: info)!
  let image = context.makeImage()!
  try? FileManager.default.createDirectory(atPath: outputDirectory, withIntermediateDirectories: true)
  let url = URL(fileURLWithPath: "\(outputDirectory)/\(name).png") as CFURL
  let destination = CGImageDestinationCreateWithURL(url, "public.png" as CFString, 1, nil)!
  CGImageDestinationAddImage(destination, image, nil)
  CGImageDestinationFinalize(destination)
}

/// Colour, a flat field, ringing and sharpness against the plain layer.
private func qualityChecks() {
  say("")
  say("Colour at 1:1, sharpening off (peak 0), against the plain layer, eight flat patches")
  let patches: [(UInt8, UInt8, UInt8)] = [
    (224, 48, 32), (48, 192, 64), (32, 64, 224), (128, 128, 128), (240, 160, 16), (144, 48, 192), (240, 240, 240),
    (32, 32, 32),
  ]
  let patchSize = PixelSize(width: 256, height: 64)
  let patchPicture = to420v(
    patternPicture(256, 64) { x, _ in patches[x / 32] })
  let plainPatches = plainLayer(patchPicture, patchSize)
  let enhancedPatches = enhanced(patchPicture, patchSize, peak: 0)
  var worst = 0
  for (index, source) in patches.enumerated() {
    let i = (32 * patchSize.width + index * 32 + 16) * 4
    let plain = (Int(plainPatches[i + 2]), Int(plainPatches[i + 1]), Int(plainPatches[i]))
    let mine = (Int(enhancedPatches[i + 2]), Int(enhancedPatches[i + 1]), Int(enhancedPatches[i]))
    let difference = max(abs(plain.0 - mine.0), abs(plain.1 - mine.1), abs(plain.2 - mine.2))
    worst = max(worst, difference)
    say("  source \(source) plain layer \(plain) enhancement \(mine) differ by \(difference)")
  }
  say("  largest difference: \(worst) levels")

  say("")
  say("A flat grey field stays flat at every level (1080p to 4096x2304 and at 1:1)")
  let flat = to420v(patternPicture(1920, 1080) { _, _ in (100, 110, 120) })
  for out in [PixelSize(width: 4096, height: 2304), PixelSize(width: 1920, height: 1080)] {
    let reference = enhanced(flat, out, peak: 0)
    for level in levels {
      let bytes = enhanced(flat, out, peak: level.peak)
      var most = 0
      for i in stride(from: 0, to: bytes.count, by: 4) {
        most = max(most, abs(Int(bytes[i]) - Int(reference[i])), abs(Int(bytes[i + 1]) - Int(reference[i + 1])))
      }
      say("  \(out.width)x\(out.height) \(level.rawValue): largest change from no sharpening \(most) levels")
    }
  }

  say("")
  say("A black-white edge, enlarged 1.6x: the output's luma stays within the source's")
  let edge = to420v(patternPicture(1920, 1080) { x, _ in x < 960 || (x / 3) % 2 == 0 && x > 1200 ? (16, 16, 16) : (235, 235, 235) })
  let edgeOut = PixelSize(width: 3072, height: 1728)
  for peak in [Float(0)] + levels.map(\.peak) {
    let bytes = enhanced(edge, edgeOut, peak: peak)
    var low = 255.0
    var high = 0.0
    for x in 1400..<2400 {
      let l = luma(bytes, 800 * edgeOut.width + x)
      low = min(low, l)
      high = max(high, l)
    }
    let bilinear = peak == 0 ? " (Catmull-Rom only)" : ""
    say(String(format: "  peak %.2f%@: luma %.1f to %.1f around the edge and the 3-pixel bars; source 16 to 235", peak, bilinear, low, high))
  }

  say("")
  say("Sharpness: mean luma gradient over the text, the plain layer against each level")
  for (name, source, out) in cases where out != source {
    let buffer = pictures[key(source)]!
    let plain = plainLayer(buffer, out)
    let region = CGRect(
      x: 80 * out.width / source.width, y: 80 * out.height / source.height, width: out.width / 2,
      height: out.height / 3)
    let crop = CGRect(x: region.minX, y: region.minY, width: 320, height: 140)
    let base = sharpness(plain, out, region)
    let tag = "\(source.width)x\(source.height)-to-\(out.width)x\(out.height)"
    writePng(plain, out, crop: crop, name: "\(tag)-plain")
    var line = String(format: "  %@: plain %.2f", name, base)
    for (label, peak) in [("catmull-rom", Float(0))] + levels.map({ ($0.rawValue, $0.peak) }) {
      let bytes = enhanced(buffer, out, peak: peak)
      line += String(format: ", %@ %.2f (%+.0f%%)", label, sharpness(bytes, out, region), (sharpness(bytes, out, region) / base - 1) * 100)
      writePng(bytes, out, crop: crop, name: "\(tag)-\(label)")
    }
    say(line)
  }
}
if offscreen { qualityChecks() }

// MARK: The presenter, in a window

private func picturesFor(_ size: PixelSize, count: Int) -> [CVPixelBuffer] {
  let base = desktopPicture(size.width, size.height)
  return (0..<count).map { index in
    var frame = base
    // A changing corner, so each buffer differs.
    for y in 0..<8 {
      for x in 0..<(8 * (index + 1)) where x < size.width {
        frame.bytes[(y * size.width + x) * 4 + 1] = 0
      }
    }
    return to420v(frame)
  }
}

private let app = NSApplication.shared
app.setActivationPolicy(.accessory)
private let window = NSWindow(
  contentRect: NSRect(x: 100, y: 100, width: 960, height: 540), styleMask: [.titled], backing: .buffered,
  defer: false)
private let host = NSView(frame: NSRect(x: 0, y: 0, width: 960, height: 540))
host.wantsLayer = true
window.contentView = host
window.orderFrontRegardless()

// The picture's view as Flutter hosts a platform view (FlutterMutatorView.mm):
// a flipped mutator view placed at the picture's place, a flipped container
// whose sublayer transform carries the zoom, and the platform view at the
// picture's size, here all three at fractional points.
private final class FlippedView: NSView {
  override var isFlipped: Bool { true }
}
/// Counts setFrameSize: and layout(), which DesktopVideoView watches.
private final class PictureView: NSView {
  var sizeCalls = 0
  var layouts = 0
  override func setFrameSize(_ newSize: NSSize) {
    sizeCalls += 1
    super.setFrameSize(newSize)
  }
  override func layout() {
    layouts += 1
    super.layout()
  }
}
private let mutator = FlippedView(frame: NSRect(x: 100.25, y: 37.75, width: 640.3, height: 360.17))
mutator.wantsLayer = true
host.addSubview(mutator)
private let platformContainer = FlippedView(frame: mutator.bounds)
platformContainer.wantsLayer = true
mutator.addSubview(platformContainer)
private let pictureView = PictureView(frame: NSRect(x: 0, y: 0, width: 640.3, height: 360.17))
pictureView.wantsLayer = true
platformContainer.addSubview(pictureView)

private let presenter = LayerPresenter()
presenter.host(in: pictureView.layer!)

private func rootLayer(_ layer: CALayer) -> CALayer {
  var root = layer
  while let up = root.superlayer { root = up }
  return root
}
private func spin(_ seconds: Double) { RunLoop.main.run(until: Date().addingTimeInterval(seconds)) }
/// Whether [layer] joined the window's layers: the frame view's is among
/// its ancestors, as the viewer's measure requires.
private func joined(_ layer: CALayer) -> Bool {
  var top: NSView = pictureView
  while let up = top.superview { top = up }
  var at = layer
  while let up = at.superlayer {
    if up === top.layer { return true }
    at = up
  }
  return false
}

/// pixelPlacement for the picture's view at [scale]: where the view is in
/// the root layer, in pixels, where a layer given the placement's frame in
/// the presenter's container lands, how far its edges are from whole pixels
/// and from the view's, and whether its size is the placement's.
@discardableResult
private func checkPlacement(_ label: String, scale: CGFloat) -> CGRect? {
  let layer = pictureView.layer!
  let root = rootLayer(layer)
  guard let placed = pixelPlacement(of: layer, in: presenter.container, root: root, scale: scale) else {
    say("  \(label), \(scale)x: no placement")
    return nil
  }
  let exact = layer.convert(layer.bounds, to: root)
  let probe = CALayer()
  presenter.container.addSublayer(probe)
  probe.frame = placed.frame
  let landed = probe.convert(probe.bounds, to: root)
  probe.removeFromSuperlayer()
  let edges = [landed.minX, landed.minY, landed.maxX, landed.maxY].map { $0 * scale }
  let fromPixel = edges.map { abs($0 - $0.rounded()) }.max() ?? 0
  let moved = zip(edges, [exact.minX, exact.minY, exact.maxX, exact.maxY].map { $0 * scale }).map { abs($0 - $1) }
    .max() ?? 0
  let sized =
    Int((landed.width * scale).rounded()) == placed.size.width
    && Int((landed.height * scale).rounded()) == placed.size.height
  say(
    String(
      format: "  %@, %.0fx: view at %.3f,%.3f px, %.3fx%.3f px; placed %dx%d, %.1e px from whole pixels, "
        + "edges moved at most %.3f px, size %@",
      label, scale, exact.minX * scale, exact.minY * scale, exact.width * scale, exact.height * scale,
      placed.size.width, placed.size.height, fromPixel, moved, sized ? "the drawable's" : "NOT the drawable's"))
  return placed.frame
}

say("")
say("Placement on whole pixels (pixelPlacement), the view under a Flutter-like hierarchy at fractional points")
say("  views new in the window: joined its layers at once \(joined(pictureView.layer!)), layout() \(pictureView.layouts) time(s)")
spin(0.1)
say("  after the run loop's next pass: joined \(joined(pictureView.layer!)), layout() \(pictureView.layouts) time(s)")
for scale in [1.0, 2.0] as [CGFloat] { checkPlacement("at rest", scale: scale) }
// Flutter sets the platform view's frame, unchanged, each frame it
// composites it; a move of the mutator alone reaches the view only so.
private let callsBefore = pictureView.sizeCalls
mutator.setFrameOrigin(NSPoint(x: 100.55, y: 38.2))
pictureView.frame = pictureView.frame
say("  the mutator moved, the view's frame set unchanged: setFrameSize: called \(pictureView.sizeCalls - callsBefore) time(s)")
checkPlacement("moved", scale: 2)
platformContainer.layer!.sublayerTransform = CATransform3DMakeScale(1.37, 1.37, 1)
checkPlacement("zoomed 1.37x", scale: 2)
platformContainer.layer!.sublayerTransform = CATransform3DIdentity
private let restingFrame = checkPlacement("back at 1x", scale: window.backingScaleFactor)!

private final class Counts {
  let lock = NSLock()
  var presented = 0
  var reveals: [UInt64] = []
  var handOff: [Double] = []
  var toPresent: [Double] = []
  var superseded = 0
  var failures: [String] = []
}
private let counts = Counts()
private let presentSize = PixelSize(width: 1920, height: 1080)
private let feed = picturesFor(presentSize, count: 6)
private var frameNumber: UInt64 = 0
private func nextPicture() -> Picture {
  frameNumber += 1
  return Picture(
    buffer: feed[Int(frameNumber) % feed.count], generation: 1, frame: frameNumber, revision: 1,
    readyNs: clock_gettime_nsec_np(CLOCK_UPTIME_RAW))
}
private var made: [EnhancedPresenter] = []
private func makeEnhancement() -> EnhancedPresenter? {
  let enhancement = EnhancedPresenter(
    device: device,
    presented: { picture in
      let now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
      counts.lock.lock()
      counts.presented += 1
      counts.toPresent.append(Double(now - picture.readyNs) / 1e6)
      counts.lock.unlock()
    },
    revealed: { sender, epoch in
      DispatchQueue.main.async {
        counts.lock.lock()
        counts.reveals.append(epoch)
        counts.lock.unlock()
        presenter.reveal(sender, epoch)
      }
    },
    failed: { _, reason in
      counts.lock.lock()
      counts.failures.append(reason)
      counts.lock.unlock()
    })
  made.append(enhancement)
  return enhancement
}
private func showing() -> String {
  presenter.enhancing.map { "\($0.size.width)x\($0.size.height) \($0.level.rawValue)" } ?? "the content layer"
}

say("")
say(
  "The presenter, in a window on \(window.screen?.localizedName ?? "no screen"), backing scale "
    + "\(window.backingScaleFactor), occlusion visible: \(window.occlusionState.contains(.visible))")
private let displays = (0..<8).compactMap { index -> String? in
  var ids = [CGDirectDisplayID](repeating: 0, count: 8)
  var count: UInt32 = 0
  CGGetOnlineDisplayList(8, &ids, &count)
  return index < Int(count) ? "display \(ids[index]) asleep \(CGDisplayIsAsleep(ids[index]) != 0)" : nil
}
say("  \(displays.joined(separator: "; "))")

// Plain commits first, then the switch. Until the metal layer shows, the
// content layer keeps showing each picture.
for _ in 0..<3 { _ = presenter.show(nextPicture(), timed: false) }
private let target = EnhanceTarget(size: PixelSize(width: 3840, height: 2160), level: .medium)
private let started = presenter.enhance(target, at: restingFrame, make: makeEnhancement)
private func describe(_ shown: LayerPresenter.Shown) -> String {
  switch shown {
  case .committed: return "committed"
  case .queued(let superseded): return superseded ? "queued, superseding" : "queued"
  case .noSurface: return "no surface"
  }
}
private let beforeReveal = (0..<3).map { _ in describe(presenter.show(nextPicture(), timed: false)) }
say("  enhancement started: \(started); the next 3 pictures before the reveal: \(beforeReveal)")
spin(1)
say("  after 1 s: \(counts.presented) presented, reveals \(counts.reveals), showing \(showing())")
private let metal = made[0].layer
private let metalInRoot = metal.convert(metal.bounds, to: rootLayer(metal))
say(
  String(
    format: "  the metal layer at %.3f,%.3f px, %.3fx%.3f px in the root layer",
    metalInRoot.minX * window.backingScaleFactor, metalInRoot.minY * window.backingScaleFactor,
    metalInRoot.width * window.backingScaleFactor, metalInRoot.height * window.backingScaleFactor))

// A feeder at 120 pictures a second for 5 s, as a decoder thread would.
private let feederDone = DispatchSemaphore(value: 0)
private var feederPictures: [Picture] = (0..<600).map { _ in nextPicture() }
private let feeder = Thread {
  let period: UInt64 = 1_000_000_000 / 120
  var due = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
  for var picture in feederPictures {
    let now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
    if due > now { usleep(UInt32((due - now) / 1000)) }
    due += period
    picture.readyNs = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
    let begun = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
    let shown = presenter.show(picture, timed: false)
    let took = Double(clock_gettime_nsec_np(CLOCK_UPTIME_RAW) - begun) / 1e6
    counts.lock.lock()
    counts.handOff.append(took)
    if case .queued(true) = shown { counts.superseded += 1 }
    counts.lock.unlock()
  }
  feederDone.signal()
}
feeder.qualityOfService = .userInteractive
private let before = counts.presented
private let timedFrom = counts.toPresent.count
feeder.start()
while feederDone.wait(timeout: .now()) == .timedOut { spin(0.05) }
spin(0.5)
counts.lock.lock()
say("  feeder: \(feederPictures.count) pictures at 120 a second; hand-off ms \(summary(counts.handOff))")
say(
  "  presenter thread: \(counts.presented - before) presented, \(counts.superseded) superseded in the slot; "
    + "hand-off to present ms \(summary(Array(counts.toPresent[timedFrom...])))")
say("  failures: \(counts.failures)")
counts.lock.unlock()

// A new size and level redraw the picture shown, with no new picture.
private let presentedBeforeRetarget = counts.presented
_ = presenter.enhance(EnhanceTarget(size: PixelSize(width: 2880, height: 1620), level: .high), at: restingFrame) {
  nil
}
spin(0.5)
say("  a new size and level with the desktop still: \(counts.presented - presentedBeforeRetarget) redrawn")

// Back to the content layer, then again.
private let back = presenter.plain(release: false)
say("  back to the content layer: newest handed back is frame \(back.map { String($0.frame) } ?? "none") of \(frameNumber)")
private let routedAfter = presenter.show(nextPicture(), timed: false)
say("  the next picture: \(describe(routedAfter))")
_ = presenter.enhance(target, at: restingFrame) { nil }
spin(0.5)
say("  enhancing again: reveals \(counts.reveals), showing \(showing())")

// Released, then a new enhancement: a reveal from the released one, of an
// epoch the new one also reaches, must not show the new one's layer.
_ = presenter.plain(release: true)
_ = presenter.enhance(target, at: restingFrame, make: makeEnhancement)
presenter.reveal(made[0], 1)
say("  a new enhancement, told of the released one's epoch 1 before its own present: showing \(showing())")
spin(0.5)
say("  after its own present: reveals \(counts.reveals), showing \(showing())")
_ = presenter.plain(release: true)
presenter.clear()
spin(0.3)
say("  released; failures: \(counts.failures)")

// MARK: KVO and conversion

say("")
private let root = CALayer()
root.frame = CGRect(x: 0, y: 0, width: 2000, height: 2000)
private let container = CALayer()
container.frame = CGRect(x: 10, y: 20, width: 1000, height: 1000)
private let leaf = CALayer()
leaf.frame = CGRect(x: 0, y: 0, width: 960, height: 540)
root.addSublayer(container)
container.addSublayer(leaf)
private var fired = 0
private let watch = container.observe(\.sublayerTransform) { _, _ in fired += 1 }
private let unscaled = leaf.convert(leaf.bounds, to: root)
container.sublayerTransform = CATransform3DMakeScale(2, 2, 1)
private let scaled = leaf.convert(leaf.bounds, to: root)
say("KVO on sublayerTransform fired \(fired) time(s) for one change")
container.sublayerTransform = CATransform3DMakeScale(2, 2, 1)
say("and \(fired) in all after the same transform was set again")
say("leaf in root: \(unscaled.size) unscaled, \(scaled.size) under the container's 2x sublayerTransform")
private let viewWatch = host.layer!.observe(\.sublayerTransform) { _, _ in fired += 1 }
host.layer!.sublayerTransform = CATransform3DMakeScale(1.5, 1.5, 1)
say("and on a layer-backed view's own layer: \(fired) in all")
withExtendedLifetime((watch, viewWatch)) {}
