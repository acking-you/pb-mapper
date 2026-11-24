// Whether FlutterMacOS (the framework binary at argv[1]) has what the layer
// view's cursorUpdate(with:) reads in DesktopVideo.swift: FlutterView
// answering didUpdateMouseCursor: and its _lastCursor ivar of an object
// type, and that object_getIvar reads back what the hook stored.
// `swiftc -O cursorcheck.swift -o cursorcheck`, then
// `cursorcheck .../FlutterMacOS.xcframework/macos-arm64_x86_64/FlutterMacOS.framework/FlutterMacOS`.
import AppKit
import Foundation

guard CommandLine.arguments.count > 1, dlopen(CommandLine.arguments[1], RTLD_NOW) != nil else {
  print("cannot load", String(cString: dlerror()))
  exit(1)
}
guard let flutterView = NSClassFromString("FlutterView") as? NSView.Type else {
  print("no FlutterView")
  exit(1)
}
let hook = NSSelectorFromString("didUpdateMouseCursor:")
print("responds to didUpdateMouseCursor:", flutterView.instancesRespond(to: hook))
guard let ivar = class_getInstanceVariable(flutterView, "_lastCursor"),
  let encoding = ivar_getTypeEncoding(ivar)
else {
  print("no _lastCursor")
  exit(1)
}
print("_lastCursor encoding", String(cString: encoding))
// An instance made without the engine's initializer: only the ivar and the
// hook are touched, both plain.
let view = flutterView.init(frame: .zero)
print("before:", object_getIvar(view, ivar) as? NSCursor as Any)
_ = view.perform(hook, with: NSCursor.crosshair)
let read = object_getIvar(view, ivar) as? NSCursor
print("after hook, reads crosshair:", read === NSCursor.crosshair)
