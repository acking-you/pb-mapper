import ApplicationServices
import Cocoa
import CoreGraphics
import FlutterMacOS

// Floating drag-to-authorize card modeled on Isthmus (LinearMouse / PermissionFlow,
// MIT): when the user opens System Settings at Privacy & Security › Screen
// Recording or Accessibility, a non-activating panel sits directly below the
// Settings window holding pb-mapper's app icon. Dragging that icon into the
// list above registers the running app bundle immediately without hunting
// through "+" in Finder, and flipping the switch activates the permission live.

final class PermissionHelper: NSObject {
  static let shared = PermissionHelper()

  private var channel: FlutterMethodChannel?
  private var panel: PermissionCardPanel?
  private var timer: Timer?
  private var activePermission: String?
  private var missedFrames = 0
  private var sawSettings = false

  static func register(with messenger: FlutterBinaryMessenger) {
    let channel = FlutterMethodChannel(
      name: "pb_mapper/permission_helper", binaryMessenger: messenger)
    shared.channel = channel
    channel.setMethodCallHandler { call, result in
      switch call.method {
      case "show":
        let args = call.arguments as? [String: Any] ?? [:]
        shared.show(
          permission: args["permission"] as? String ?? "",
          title: args["title"] as? String ?? "",
          subtitle: args["subtitle"] as? String ?? "",
          appName: args["appName"] as? String ?? "pb-mapper")
        result(nil)
      case "hide":
        shared.hide()
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }

  func show(permission: String, title: String, subtitle: String, appName: String) {
    let card = panel ?? PermissionCardPanel()
    card.onClose = { [weak self] in self?.hide() }
    card.update(title: title, subtitle: subtitle, appName: appName)
    panel = card
    activePermission = permission
    missedFrames = 0
    sawSettings = false
    timer?.invalidate()
    let timer = Timer(timeInterval: 1.0 / 20.0, repeats: true) { [weak self] _ in self?.tick() }
    timer.tolerance = 0.01
    RunLoop.main.add(timer, forMode: .common)
    self.timer = timer
    tick()
  }

  func hide() {
    timer?.invalidate()
    timer = nil
    activePermission = nil
    panel?.orderOut(nil)
  }

  private func fastGranted(_ permission: String?) -> Bool {
    switch permission {
    case "input":
      return AXIsProcessTrusted() || CGPreflightPostEventAccess()
    case "screen_capture":
      return CGPreflightScreenCaptureAccess()
    default:
      return false
    }
  }

  private func tick() {
    guard let panel else {
      hide()
      return
    }
    if let perm = activePermission, fastGranted(perm), !panel.isDragging {
      hide()
      channel?.invokeMethod("granted", arguments: perm)
      return
    }
    guard let frame = SettingsWindow.frame() else {
      missedFrames += 1
      // Settings closed (~2 s after it was seen) or never appeared (~15 s).
      if missedFrames >= (sawSettings ? 40 : 300) {
        hide()
      } else if !panel.isDragging {
        panel.orderOut(nil)
      }
      return
    }
    missedFrames = 0
    sawSettings = true
    panel.follow(frame)
  }
}

/// System Settings' main window geometry in Cocoa coordinates (needs no TCC permission).
enum SettingsWindow {
  static func frame() -> CGRect? {
    let pids = Set(
      NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.systempreferences")
        .map(\.processIdentifier))
    guard !pids.isEmpty,
      let windows = CGWindowListCopyWindowInfo(
        [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]],
      let primary = NSScreen.screens.first
    else {
      return nil
    }
    let frames = windows.compactMap { info -> CGRect? in
      guard let pid = info[kCGWindowOwnerPID as String] as? pid_t, pids.contains(pid),
        info[kCGWindowLayer as String] as? Int == 0,
        let bounds = info[kCGWindowBounds as String] as? NSDictionary,
        let frame = CGRect(dictionaryRepresentation: bounds),
        frame.width > 300, frame.height > 200
      else {
        return nil
      }
      return CGRect(
        x: frame.minX, y: primary.frame.maxY - frame.maxY,
        width: frame.width, height: frame.height)
    }
    return frames.max { $0.width * $0.height < $1.width * $1.height }
  }

  static func placement(size: CGSize, below anchor: CGRect, within screen: CGRect) -> CGRect {
    let margin: CGFloat = 12
    var x = anchor.midX - size.width / 2
    x = min(max(x, screen.minX + margin), screen.maxX - size.width - margin)
    var y = anchor.minY - size.height - margin
    if y < screen.minY + margin {
      y = anchor.minY + margin
    }
    return CGRect(x: x, y: y, width: size.width, height: size.height)
  }
}

final class PermissionCardPanel: NSPanel {
  private let titleLabel = NSTextField(wrappingLabelWithString: "")
  private let subtitleLabel = NSTextField(wrappingLabelWithString: "")
  private let dragRow = AppDragView()
  var onClose: () -> Void = {}

  var isDragging: Bool { dragRow.isDragging }

  init() {
    super.init(
      contentRect: NSRect(x: 0, y: 0, width: 380, height: 160),
      styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
    level = .floating
    isReleasedWhenClosed = false
    isOpaque = false
    backgroundColor = .clear
    hasShadow = true
    hidesOnDeactivate = false
    collectionBehavior = [.fullScreenAuxiliary, .canJoinAllSpaces]
    buildContent()
  }

  override var canBecomeKey: Bool { false }
  override var canBecomeMain: Bool { false }

  func update(title: String, subtitle: String, appName: String) {
    titleLabel.stringValue = title
    subtitleLabel.stringValue = subtitle
    dragRow.appName = appName
    if let contentView {
      contentView.layoutSubtreeIfNeeded()
      setContentSize(contentView.fittingSize)
    }
  }

  func follow(_ settings: CGRect) {
    guard !isDragging else { return }
    let screen =
      NSScreen.screens.max {
        Self.area($0.frame.intersection(settings)) < Self.area($1.frame.intersection(settings))
      } ?? NSScreen.main
    guard let visible = screen?.visibleFrame else { return }
    let target = SettingsWindow.placement(size: frame.size, below: settings, within: visible)
    if frame != target {
      setFrame(target, display: true)
    }
    let front = NSWorkspace.shared.frontmostApplication?.bundleIdentifier
    if front == "com.apple.systempreferences" || front == Bundle.main.bundleIdentifier {
      if !isVisible { orderFrontRegardless() }
    } else {
      orderOut(nil)
    }
  }

  private func buildContent() {
    let background = NSVisualEffectView()
    background.material = .popover
    background.blendingMode = .behindWindow
    background.state = .active
    background.wantsLayer = true
    background.layer?.cornerRadius = 16
    background.layer?.masksToBounds = true
    contentView = background

    let arrow = NSImageView()
    if #available(macOS 11.0, *) {
      arrow.image = NSImage(systemSymbolName: "arrow.up", accessibilityDescription: nil)
      arrow.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 15, weight: .semibold)
    }
    arrow.contentTintColor = .controlAccentColor

    titleLabel.font = .systemFont(ofSize: 13, weight: .semibold)
    titleLabel.textColor = .labelColor
    subtitleLabel.font = .systemFont(ofSize: 12)
    subtitleLabel.textColor = .secondaryLabelColor

    let closeImage: NSImage = {
      if #available(macOS 11.0, *),
        let symbol = NSImage(systemSymbolName: "xmark.circle.fill", accessibilityDescription: "Close")
      {
        return symbol
      }
      return NSImage(named: NSImage.stopProgressFreestandingTemplateName) ?? NSImage()
    }()
    let close = NSButton(image: closeImage, target: self, action: #selector(closeTapped))
    close.isBordered = false
    close.contentTintColor = .tertiaryLabelColor

    for view in [arrow, titleLabel, subtitleLabel, dragRow, close] as [NSView] {
      view.translatesAutoresizingMaskIntoConstraints = false
      background.addSubview(view)
    }
    NSLayoutConstraint.activate([
      background.widthAnchor.constraint(equalToConstant: 380),

      arrow.leadingAnchor.constraint(equalTo: background.leadingAnchor, constant: 16),
      arrow.topAnchor.constraint(equalTo: background.topAnchor, constant: 17),
      arrow.widthAnchor.constraint(equalToConstant: 18),

      titleLabel.leadingAnchor.constraint(equalTo: arrow.trailingAnchor, constant: 8),
      titleLabel.topAnchor.constraint(equalTo: background.topAnchor, constant: 16),
      titleLabel.trailingAnchor.constraint(equalTo: close.leadingAnchor, constant: -8),

      subtitleLabel.leadingAnchor.constraint(equalTo: titleLabel.leadingAnchor),
      subtitleLabel.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 3),
      subtitleLabel.trailingAnchor.constraint(equalTo: background.trailingAnchor, constant: -16),

      close.topAnchor.constraint(equalTo: background.topAnchor, constant: 12),
      close.trailingAnchor.constraint(equalTo: background.trailingAnchor, constant: -12),
      close.widthAnchor.constraint(equalToConstant: 20),
      close.heightAnchor.constraint(equalToConstant: 20),

      dragRow.leadingAnchor.constraint(equalTo: background.leadingAnchor, constant: 16),
      dragRow.trailingAnchor.constraint(equalTo: background.trailingAnchor, constant: -16),
      dragRow.topAnchor.constraint(equalTo: subtitleLabel.bottomAnchor, constant: 12),
      dragRow.heightAnchor.constraint(equalToConstant: 52),
      dragRow.bottomAnchor.constraint(equalTo: background.bottomAnchor, constant: -16),
    ])
    background.layoutSubtreeIfNeeded()
    setContentSize(background.fittingSize)
  }

  @objc private func closeTapped() {
    onClose()
  }

  private static func area(_ rect: CGRect) -> CGFloat {
    rect.isNull ? 0 : rect.width * rect.height
  }
}

/// Draggable app badge: exports the running `.app` bundle URL on the dragging
/// pasteboard just as Finder does, so dropping it onto System Settings'
/// privacy tables registers this exact app bundle.
final class AppDragView: NSView, NSDraggingSource {
  private let appURL = Bundle.main.bundleURL
  private let icon = NSImageView(image: NSApplication.shared.applicationIconImage)
  private let label = NSTextField(labelWithString: "pb-mapper")
  private var mouseDownPoint: NSPoint?
  private(set) var isDragging = false

  var appName: String {
    get { label.stringValue }
    set {
      label.stringValue = newValue
      setAccessibilityLabel(newValue)
    }
  }

  init() {
    super.init(frame: .zero)
    wantsLayer = true
    layer?.cornerRadius = 10
    layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.05).cgColor
    layer?.borderColor = NSColor.labelColor.withAlphaComponent(0.12).cgColor
    layer?.borderWidth = 1
    icon.imageScaling = .scaleProportionallyUpOrDown
    label.font = .systemFont(ofSize: 14, weight: .medium)
    for view in [icon, label] {
      view.translatesAutoresizingMaskIntoConstraints = false
      addSubview(view)
    }
    NSLayoutConstraint.activate([
      icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
      icon.centerYAnchor.constraint(equalTo: centerYAnchor),
      icon.widthAnchor.constraint(equalToConstant: 32),
      icon.heightAnchor.constraint(equalToConstant: 32),
      label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 10),
      label.centerYAnchor.constraint(equalTo: centerYAnchor),
      label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -12),
    ])
    setAccessibilityElement(true)
    setAccessibilityRole(.image)
  }

  required init?(coder: NSCoder) { nil }

  override func hitTest(_ point: NSPoint) -> NSView? {
    super.hitTest(point) == nil ? nil : self
  }

  override func resetCursorRects() {
    addCursorRect(bounds, cursor: .openHand)
  }

  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

  override func mouseDown(with event: NSEvent) {
    mouseDownPoint = event.locationInWindow
  }

  override func mouseUp(with event: NSEvent) {
    mouseDownPoint = nil
  }

  override func mouseDragged(with event: NSEvent) {
    guard let origin = mouseDownPoint,
      hypot(event.locationInWindow.x - origin.x, event.locationInWindow.y - origin.y) >= 4,
      let bitmap = bitmapImageRepForCachingDisplay(in: bounds)
    else {
      return
    }
    mouseDownPoint = nil
    cacheDisplay(in: bounds, to: bitmap)
    let preview = NSImage(size: bounds.size)
    preview.addRepresentation(bitmap)

    let item = NSPasteboardItem()
    item.setString(appURL.absoluteString, forType: .fileURL)
    item.setPropertyList(
      [appURL.path], forType: NSPasteboard.PasteboardType("NSFilenamesPboardType"))
    let dragging = NSDraggingItem(pasteboardWriter: item)
    dragging.draggingFrame = bounds
    let frame = NSRect(origin: .zero, size: bounds.size)
    dragging.imageComponentsProvider = {
      let component = NSDraggingImageComponent(key: .init(rawValue: "pb_mapper.permissionRow"))
      component.contents = preview
      component.frame = frame
      return [component]
    }
    let session = beginDraggingSession(with: [dragging], event: event, source: self)
    session.draggingFormation = .none
    session.animatesToStartingPositionsOnCancelOrFail = true
  }

  func draggingSession(_ session: NSDraggingSession, willBeginAt screenPoint: NSPoint) {
    isDragging = true
    window?.alphaValue = 0.72
  }

  func draggingSession(
    _ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation
  ) {
    isDragging = false
    window?.alphaValue = 1
  }

  func draggingSession(
    _ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext
  ) -> NSDragOperation {
    .copy
  }

  func ignoreModifierKeys(for session: NSDraggingSession) -> Bool { true }
}
