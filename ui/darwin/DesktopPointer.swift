// The remote desktop viewer's pointer on macOS: the `pb_mapper/desktop_pointer`
// method channel, exactly as ui/lib/src/ffi/desktop_pointer.dart calls it,
// like the Windows runner (ui/windows/runner/desktop_pointer.cpp) and the
// Linux one (ui/linux/runner/desktop_pointer.cc). It makes a remote
// computer's pointer bitmap this Mac's own cursor, which then moves with the
// hardware and changes the moment Dart sets it, with no Flutter frame.
//
// Only the macOS runner (ui/macos/Runner.xcodeproj) compiles this file; an
// iOS viewer paints the remote pointer.

#if os(macOS)
  import AppKit
  import FlutterMacOS

  private let pointerChannel = "pb_mapper/desktop_pointer"
  /// No side Dart sends is longer: 128 logical pixels at the display's scale.
  private let maxSide = 1024
  /// FlutterView's own: it keeps the cursor last activated and sets it again
  /// on every cursorUpdate: (FlutterView.mm), which would undo ours.
  private let didUpdateMouseCursor = NSSelectorFromString("didUpdateMouseCursor:")

  private struct PointerError: Error {
    let message: String
    init(_ message: String) { self.message = message }
  }

  /// `cursor_create {id, rgba, width, height, hot_x, hot_y}` makes a cursor
  /// from straight RGBA in physical pixels, `cursor_set {id}` shows it, and
  /// `cursor_delete {id}` frees it. A shown cursor stays until Dart activates
  /// a system one through the engine, which tells the view in turn.
  final class DesktopPointerPlugin: NSObject, FlutterPlugin {
    private weak var registrar: FlutterPluginRegistrar?
    private var cursors: [Int64: NSCursor] = [:]
    /// The cursor last set, re-set by `monitor` where the view cannot keep it.
    private var shown: NSCursor?
    private var monitor: Any?

    private init(registrar: FlutterPluginRegistrar) {
      self.registrar = registrar
      super.init()
    }

    deinit {
      if let monitor = monitor { NSEvent.removeMonitor(monitor) }
    }

    static func register(with registrar: FlutterPluginRegistrar) {
      let channel = FlutterMethodChannel(
        name: pointerChannel, binaryMessenger: registrar.messenger)
      registrar.addMethodCallDelegate(DesktopPointerPlugin(registrar: registrar), channel: channel)
    }

    func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
      do {
        try dispatch(call, result)
      } catch {
        let message = (error as? PointerError)?.message ?? "\(error)"
        result(FlutterError(code: "desktop_pointer", message: message, details: nil))
      }
    }

    private func dispatch(_ call: FlutterMethodCall, _ result: @escaping FlutterResult) throws {
      guard ["cursor_create", "cursor_set", "cursor_delete"].contains(call.method) else {
        result(FlutterMethodNotImplemented)
        return
      }
      guard let args = call.arguments as? [String: Any] else {
        throw PointerError("Missing desktop pointer arguments")
      }
      let id = try number(args, "id")
      switch call.method {
      case "cursor_create":
        let width = Int(try number(args, "width"))
        let height = Int(try number(args, "height"))
        let hotX = Int(try number(args, "hot_x"))
        let hotY = Int(try number(args, "hot_y"))
        guard (1...maxSide).contains(width), (1...maxSide).contains(height),
          (0..<width).contains(hotX), (0..<height).contains(hotY)
        else { throw PointerError("Invalid cursor size or hotspot") }
        guard let rgba = (args["rgba"] as? FlutterStandardTypedData)?.data,
          rgba.count == width * height * 4
        else { throw PointerError("Invalid cursor pixels") }
        guard cursors[id] == nil else { throw PointerError("Cursor id in use") }
        guard let cursor = makeCursor(rgba, width, height, hotX, hotY) else {
          throw PointerError("Cannot create the cursor")
        }
        cursors[id] = cursor
      case "cursor_set":
        guard let cursor = cursors[id] else { throw PointerError("Unknown cursor") }
        show(cursor)
      default:
        if let cursor = cursors.removeValue(forKey: id), cursor === shown { shown = nil }
      }
      result(nil)
    }

    private func show(_ cursor: NSCursor) {
      cursor.set()
      shown = cursor
      // The view's cursorUpdate: restores the cursor it was told last: ours,
      // from now on. An engine without that hook gets a monitor instead,
      // which takes cursorUpdate events while ours is the current cursor.
      if let view = registrar?.view, view.responds(to: didUpdateMouseCursor) {
        _ = view.perform(didUpdateMouseCursor, with: cursor)
      } else if monitor == nil {
        monitor = NSEvent.addLocalMonitorForEvents(matching: .cursorUpdate) {
          [weak self] event in
          guard let shown = self?.shown, NSCursor.current === shown else { return event }
          shown.set()
          return nil
        }
      }
    }

    /// The bitmap at the window's scale: an image of `width / scale` by
    /// `height / scale` points holding every pixel, its hotspot in points from
    /// the top left.
    private func makeCursor(_ rgba: Data, _ width: Int, _ height: Int, _ hotX: Int, _ hotY: Int)
      -> NSCursor?
    {
      guard let provider = CGDataProvider(data: rgba as CFData),
        let space = CGColorSpace(name: CGColorSpace.sRGB),
        let image = CGImage(
          width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
          bytesPerRow: width * 4, space: space,
          bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
          provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
      else { return nil }
      let scale = max(
        1, registrar?.view?.window?.backingScaleFactor ?? NSScreen.main?.backingScaleFactor ?? 1)
      let size = NSSize(width: CGFloat(width) / scale, height: CGFloat(height) / scale)
      let rep = NSBitmapImageRep(cgImage: image)
      rep.size = size
      let picture = NSImage(size: size)
      picture.addRepresentation(rep)
      return NSCursor(
        image: picture, hotSpot: NSPoint(x: CGFloat(hotX) / scale, y: CGFloat(hotY) / scale))
    }

    private func number(_ args: [String: Any], _ key: String) throws -> Int64 {
      guard let value = args[key] as? NSNumber else {
        throw PointerError("Missing desktop pointer field \(key)")
      }
      return value.int64Value
    }
  }
#endif
