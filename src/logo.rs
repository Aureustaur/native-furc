//! Helper for loading the Bear Built logo as an egui texture.

use eframe::egui::{ColorImage, Context, TextureHandle, TextureOptions};

/// Load the embedded Bear Built logo and return an egui texture handle.
pub fn load_logo(ctx: &Context) -> TextureHandle {
    let bytes = include_bytes!("../bear-built-logo.png");
    let img = image::load_from_memory(bytes).expect("embedded bear-built-logo.png must be valid PNG");
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    ctx.load_texture("bear_built_logo", color_image, TextureOptions::LINEAR)
}
