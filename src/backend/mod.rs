//! Output backends. `winit` runs nested inside another compositor (development); `tty` drives
//! real displays through DRM/KMS with libinput and a libseat session.

pub mod tty;
pub mod winit;

use std::{path::Path, time::Duration};

use crate::state::State;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                solid::SolidColorRenderElement,
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind, RenderElementStates,
            },
            gles::{GlesRenderer, GlesTexture},
            Bind, Color32F, ExportMem, Offscreen,
        },
    },
    desktop::{
        layer_map_for_output,
        space::render_output,
        utils::{surface_presentation_feedback_flags_from_states, OutputPresentationFeedback},
    },
    output::Output,
    wayland::presentation::Refresh,
    utils::{Rectangle, Transform},
};

smithay::backend::renderer::element::render_elements! {
    /// Everything drawn on top of (or instead of) the space: borders, or the lock screen.
    pub OverlayElement<=GlesRenderer>;
    Solid = SolidColorRenderElement,
    Surface = WaylandSurfaceRenderElement<GlesRenderer>,
}

/// Borders normally; while the session is locked, only the lock surface of `output`.
pub fn overlay_elements(state: &mut State, renderer: &mut GlesRenderer, output: &Output) -> Vec<OverlayElement> {
    if !state.locked {
        return state.border_elements(output).into_iter().map(OverlayElement::Solid).collect();
    }
    let scale = state.config.general.scale;
    let mut out = Vec::new();
    for (o, surface) in &state.lock_surfaces {
        if o == output {
            out.extend(
                render_elements_from_surface_tree(renderer, surface.wl_surface(), (0, 0), scale, 1.0, Kind::Unspecified)
                    .into_iter()
                    .map(OverlayElement::Surface),
            );
        }
    }
    out
}

/// Clear colour: the configured background, or black behind the lock screen.
pub fn clear_color(state: &State) -> Color32F {
    if state.locked { Color32F::new(0.0, 0.0, 0.0, 1.0) } else { state.config.background }
}

/// Presentation-time feedback for everything shown on `output` in the frame just rendered.
pub fn collect_feedback(state: &State, output: &Output, states: &RenderElementStates) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    let on_output = |_: &_, _: &_| Some(output.clone());
    let flags = |surface: &_, _: &_| surface_presentation_feedback_flags_from_states(surface, states);
    for window in state.space.elements() {
        if state.space.outputs_for_element(window).contains(output) {
            window.take_presentation_feedback(&mut feedback, on_output, flags);
        }
    }
    for layer in layer_map_for_output(output).layers() {
        layer.take_presentation_feedback(&mut feedback, on_output, flags);
    }
    feedback
}

pub fn refresh_of(output: &Output) -> Refresh {
    match output.current_mode() {
        Some(mode) if mode.refresh > 0 => Refresh::fixed(Duration::from_nanos(1_000_000_000_000 / mode.refresh as u64)),
        _ => Refresh::Unknown,
    }
}

/// Render the output into an offscreen texture, read it back and write a PNG. Refused while the
/// session is locked (it would leak what the lock screen hides).
pub fn screenshot(
    renderer: &mut GlesRenderer,
    state: &mut State,
    output: &Output,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if state.locked {
        return Err("session is locked".into());
    }
    let borders = overlay_elements(state, renderer, output);
    let space = &state.space;
    let background = state.config.background;
    let mode = output.current_mode().ok_or("output has no mode")?;
    let (w, h) = (mode.size.w, mode.size.h);

    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (w, h).into())?;
    let mut framebuffer = renderer.bind(&mut texture)?;
    let mut tracker = OutputDamageTracker::from_output(output);
    render_output(output, renderer, &mut framebuffer, 1.0, 0, [space], &borders, &mut tracker, background)?;

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

