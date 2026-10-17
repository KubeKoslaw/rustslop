//! Output backends. `winit` runs nested inside another compositor (development); `tty` drives
//! real displays through DRM/KMS with libinput and a libseat session.

pub mod tty;
pub mod winit;

use std::path::Path;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::solid::SolidColorRenderElement,
            gles::{GlesRenderer, GlesTexture},
            Bind, Color32F, ExportMem, Offscreen,
        },
    },
    desktop::{space::render_output, Space, Window},
    output::Output,
    utils::{Rectangle, Transform},
};

/// Render the output into an offscreen texture, read it back and write a PNG.
pub fn screenshot(
    renderer: &mut GlesRenderer,
    output: &Output,
    space: &Space<Window>,
    borders: &[SolidColorRenderElement],
    background: Color32F,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mode = output.current_mode().ok_or("output has no mode")?;
    let (w, h) = (mode.size.w, mode.size.h);

    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (w, h).into())?;
    let mut framebuffer = renderer.bind(&mut texture)?;
    let mut tracker = OutputDamageTracker::from_output(output);
    render_output(output, renderer, &mut framebuffer, 1.0, 0, [space], borders, &mut tracker, background)?;

    let mapping = renderer.copy_framebuffer(&framebuffer, Rectangle::from_size((w, h).into()), Fourcc::Abgr8888)?;
    let bytes = renderer.map_texture(&mapping)?;

    let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // The winit output is Flipped180 and its readback needs its rows flipped to come out upright;
    // outputs with a Normal transform (TTY, virtual) are already upright.
    let stride = w as usize * 4;
    let upright: Vec<u8> = if output.current_transform() == Transform::Flipped180 {
        bytes.chunks_exact(stride).rev().flatten().copied().collect()
    } else {
        bytes.to_vec()
    };
    encoder.write_header()?.write_image_data(&upright)?;
    Ok(())
}

