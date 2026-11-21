import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    self.contentViewController = flutterViewController

    // The app draws under a transparent title bar: the sidebar runs up behind
    // the window buttons (the Flutter side leaves 28 points free at the top).
    self.titlebarAppearsTransparent = true
    self.titleVisibility = .hidden
    self.styleMask.insert(.fullSizeContentView)
    self.minSize = NSSize(width: 960, height: 620)
    self.setContentSize(NSSize(width: 1280, height: 820))
    self.center()
    self.setFrameAutosaveName("YonderMain")

    RegisterGeneratedPlugins(registry: flutterViewController)

    super.awakeFromNib()
  }
}
