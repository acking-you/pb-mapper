import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    // Forward the command line to Dart's `main(List<String> args)`, the way the
    // Windows and Linux runners already do. Without this, `pb_mapper_ui status`
    // on macOS reaches Dart with no arguments at all and silently opens a
    // window instead of running the command.
    //
    // Finder passes things like `-psn_0_123456`; those are dropped by the CLI's
    // own check, which only treats a known verb as a command.
    let project = FlutterDartProject()
    project.dartEntrypointArguments = Array(CommandLine.arguments.dropFirst())

    let flutterViewController = FlutterViewController(project: project)
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)

    super.awakeFromNib()
  }
}
