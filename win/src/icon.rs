//! The app icon, embedded from assets/icon-256.png and decoded at runtime for
//! the window icon, the tray icon, and the in-window logo.

/// Committed 256x256 PNG rendered from assets/icon.svg (see repo build).
const ICON_PNG: &[u8] = include_bytes!("../../assets/icon-256.png");

/// Decode to (rgba8, width, height).
fn decode() -> (Vec<u8>, u32, u32) {
    let img = image::load_from_memory(ICON_PNG)
        .expect("embedded icon-256.png must be a valid PNG")
        .to_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

/// Icon for the eframe window (title bar / taskbar).
pub fn egui_icon() -> egui::IconData {
    let (rgba, width, height) = decode();
    egui::IconData { rgba, width, height }
}

/// Icon for the system tray.
pub fn tray_icon() -> Option<tray_icon::Icon> {
    let (rgba, w, h) = decode();
    tray_icon::Icon::from_rgba(rgba, w, h).ok()
}

/// RGBA + size for building an egui texture (the in-window logo).
pub fn logo_rgba() -> (Vec<u8>, [usize; 2]) {
    let (rgba, w, h) = decode();
    (rgba, [w as usize, h as usize])
}
