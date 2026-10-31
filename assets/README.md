# Task Manager icon package — Glossy #5

Two matching variants: **light** (Windows-style gray) and **dark** (near-black), with an ice-blue Times-style serif T. Includes SVG sources and PNGs at 16, 24, 32, 48, 64, 128, 256, 512, 1024 px.

## Rust integration (egui/eframe)

Add `image = { version = "0.25", default-features = false, features = ["png"] }` to Cargo.toml.

```rust
use eframe::egui;

fn icon_data(dark: bool) -> egui::IconData {
    let bytes: &[u8] = if dark {
        include_bytes!("../assets/icons/dark/icon-256.png")
    } else {
        include_bytes!("../assets/icons/light/icon-256.png")
    };
    let image = image::load_from_memory(bytes).unwrap().to_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData { rgba: image.into_raw(), width, height }
}

// When creating native options, select the initial system theme:
let dark = dark_light::detect() == dark_light::Mode::Dark;
let options = eframe::NativeOptions {
    viewport: egui::ViewportBuilder::default().with_icon(icon_data(dark)),
    ..Default::default()
};
```

For automatic system theme detection, add `dark-light = "2"` to Cargo.toml. The icon above is chosen at startup. If your application supports live theme switching, update the viewport icon when the theme changes (API varies by eframe version).

## Linux desktop launcher

Install your preferred PNG to `~/.local/share/icons/hicolor/256x256/apps/task-manager.png`, then use `Icon=task-manager` in your `.desktop` file. The desktop icon generally **does not auto-switch** with the GTK/KDE color scheme; to support that, install/update the selected asset when the system theme changes, or implement a theme-aware icon lookup.

## Notes

SVGs are editable vector approximations. PNGs are rendered using a Times New Roman-compatible serif font available in the build environment. The font is not bundled.
