import Cocoa
import FlutterMacOS

@main
class AppDelegate: FlutterAppDelegate {
  /// Closing hides to the tray, so the app has to outlive its window.
  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    return false
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }

  /// A quit sent by logout, restart or shutdown proceeds immediately without
  /// FlutterAppDelegate's two-step `.terminateCancel` reply, which loginwindow
  /// treats as refusing to quit.
  override func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
    if Self.systemIsEndingSession() {
      return .terminateNow
    }
    return super.applicationShouldTerminate(sender)
  }

  private static func systemIsEndingSession() -> Bool {
    guard let event = NSAppleEventManager.shared().currentAppleEvent,
      event.eventClass == AEEventClass(kCoreEventClass),
      event.eventID == AEEventID(kAEQuitApplication)
    else {
      return false
    }
    let keyword = AEKeyword(kAEQuitReason)
    guard
      let reason = event.attributeDescriptor(forKeyword: keyword)
        ?? event.paramDescriptor(forKeyword: keyword)
    else {
      return false
    }
    return reason.typeCodeValue != 0
  }

  /// Bring the window back when the app is reopened while hidden.
  ///
  /// Hiding to the tray orders the window out, and AppKit only offers to reopen
  /// windows it still knows about. Without this the Dock icon does nothing once
  /// the window has been closed, leaving the tray as the only way back in.
  override func applicationShouldHandleReopen(
    _ sender: NSApplication,
    hasVisibleWindows flag: Bool
  ) -> Bool {
    if !flag {
      for window in sender.windows {
        window.setIsVisible(true)
        window.makeKeyAndOrderFront(self)
      }
      sender.activate(ignoringOtherApps: true)
    }
    return true
  }
}
