// Checks the layer presenter's view structure as DesktopVideo.swift builds
// it: a layer-backed view whose own layer hosts a container (autoresizing),
// a content layer, and commits of IOSurfaces from a thread without a run
// loop; moving the container to a second view; the view's display link.
import AppKit
import CoreVideo
import QuartzCore

final class StillLayers: NSObject, CALayerDelegate {
  static let shared = StillLayers()
  func action(for layer: CALayer, forKey event: String) -> CAAction? { NSNull() }
}

final class View: NSView {
  init() {
    super.init(frame: .zero)
    wantsLayer = true
    layerContentsRedrawPolicy = .never
  }
  required init?(coder: NSCoder) { nil }
  override var wantsUpdateLayer: Bool { true }
  override func updateLayer() {}
}

final class Ticks: NSObject {
  var count = 0
  @objc func tick(_ link: CADisplayLink) { count += 1 }
}

let container = CALayer()
let content = CALayer()
for layer in [container, content] {
  layer.delegate = StillLayers.shared
  layer.autoresizingMask = [.layerWidthSizable, .layerHeightSizable]
}
content.contentsGravity = .resize
container.addSublayer(content)

func host(_ view: NSView) {
  CATransaction.begin()
  CATransaction.setDisableActions(true)
  container.frame = view.layer!.bounds
  view.layer!.addSublayer(container)
  CATransaction.commit()
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(
  contentRect: NSRect(x: 100, y: 100, width: 600, height: 400), styleMask: [.titled, .resizable],
  backing: .buffered, defer: false)
let root = NSView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
root.wantsLayer = true
window.contentView = root
let first = View()
host(first)  // before it has a frame or a window, as the factory does
root.addSubview(first)
first.frame = NSRect(x: 10, y: 20, width: 320, height: 180)
window.makeKeyAndOrderFront(nil)

func pump(_ seconds: Double) {
  let until = Date().addingTimeInterval(seconds)
  while Date() < until { RunLoop.main.run(mode: .default, before: until) }
}
pump(0.3)
print("view", first.frame, "backing layer", first.layer!.bounds)
print("container", container.frame, "content", content.frame)
first.frame = NSRect(x: 10, y: 20, width: 640, height: 360)
pump(0.3)
print("after resize: container", container.frame, "content", content.frame)

// Commits from a thread with no run loop.
var pool: [CVPixelBuffer] = []
for _ in 0..<4 {
  var pb: CVPixelBuffer?
  CVPixelBufferCreate(
    nil, 64, 36, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    [kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any]()] as CFDictionary, &pb)
  pool.append(pb!)
}
let done = DispatchSemaphore(value: 0)
let thread = Thread {
  for i in 0..<120 {
    autoreleasepool {
      CATransaction.begin()
      CATransaction.setDisableActions(true)
      content.contents = CVPixelBufferGetIOSurface(pool[i % 4])!.takeUnretainedValue()
      CATransaction.commit()
    }
    usleep(8000)
  }
  done.signal()
}
thread.start()
while done.wait(timeout: .now()) == .timedOut { pump(0.05) }
print("commits done; contents is an IOSurface:", CFGetTypeID(content.contents as CFTypeRef) == IOSurfaceGetTypeID())

// A second view takes the container; removing the first leaves it in place.
let second = View()
host(second)
root.addSubview(second)
second.frame = NSRect(x: 0, y: 0, width: 300, height: 200)
pump(0.2)
first.removeFromSuperview()
pump(0.2)
print("moved: container superlayer is second's layer:", container.superlayer === second.layer, container.frame)

if #available(macOS 14.0, *) {
  let ticks = Ticks()
  let link = second.displayLink(target: ticks, selector: #selector(Ticks.tick(_:)))
  link.add(to: .main, forMode: .common)
  pump(1)
  link.invalidate()
  print("display link ticks in 1 s:", ticks.count, "screen", second.window?.screen?.localizedName ?? "none")
}
