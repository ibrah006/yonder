# Yonder's logo

A dot for the computer you're at, a ring for the one far away, one arc between
them, in mint (`#8FE3BE`) on a deep-green tile (`#4C8272` → `#1A362F`).

| File | Use |
|---|---|
| `yonder-mark.svg` | The mark alone, in `currentColor`: the docs site, a favicon, anything on its own background. |
| `yonder-icon.svg` | The full-bleed icon: the Windows icon, the app's own mark (`app/lib/ui/logo.dart` draws the same geometry). |
| `yonder-icon-small.svg` | The same at 16–32 px: a bigger, bolder mark that stays legible. |
| `yonder-icon-macos.svg` | The macOS app icon on Apple's 1024 grid (an 824-point tile). |
| `yonder-icon-macos-small.svg` | The macOS icon's 16 and 32 px sizes. |

The wordmark is "yonder" in lowercase, Sora SemiBold, letter-spacing −0.035em,
beside the mark.

## Remaking the icon files

After changing an SVG, regenerate the PNGs and the `.ico` from it with any
SVG renderer (on a Mac, AppKit's `NSImage` reads SVG):

- `app/macos/Runner/Assets.xcassets/AppIcon.appiconset/app_icon_<n>.png`: from
  `yonder-icon-macos.svg` at 64–1024 px and `yonder-icon-macos-small.svg` at
  16 and 32, each with macOS's icon shadow (black, 30%, 20 px blur, 10 px
  down, at 1024 scale).
- `crates/agent/yonder.ico`: PNGs of `yonder-icon-small.svg` at 16, 24 and 32
  and `yonder-icon.svg` at 48, 64, 128 and 256, in one icon file. The agent's
  `build.rs` embeds it in `yonder-agent.exe`.
