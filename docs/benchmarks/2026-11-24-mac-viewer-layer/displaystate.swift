// Whether each display is asleep, and whether loginwindow's shield (the
// lock screen) is on screen. `swiftc -O displaystate.swift -o displaystate`.
import CoreGraphics
import Foundation

var displays = [CGDirectDisplayID](repeating: 0, count: 8)
var count: UInt32 = 0
CGGetOnlineDisplayList(8, &displays, &count)
for d in displays.prefix(Int(count)) {
  print(
    "display", d, "asleep", CGDisplayIsAsleep(d) != 0, "active", CGDisplayIsActive(d) != 0,
    "bounds", CGDisplayBounds(d))
}
let windows =
  (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]]) ?? []
let shield = windows.contains { window in
  (window[kCGWindowOwnerName as String] as? String) == "loginwindow"
    && ((window[kCGWindowLayer as String] as? Int) ?? 0) >= 2000
}
print("loginwindow shield on screen:", shield)
