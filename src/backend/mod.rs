//! Output backends. `headless` renders nowhere (tests); `winit` runs nested inside another compositor (development); `tty` drives
//! real displays through DRM/KMS with libinput and a libseat session.

pub mod headless;
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
                AsRenderElements,
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind, RenderElementStates,
            },
            gles::{GlesRenderer, GlesTexture},
            Bind, Color32F, ExportMem, Offscreen,
        },
    },
    desktop::{
        layer_map_for_output, Window,
        utils::{surface_presentation_feedback_flags_from_states, OutputPresentationFeedback},
    },
    output::Output,
    wayland::{presentation::Refresh, shell::wlr_layer::Layer},
    utils::{Rectangle, Scale, Transform},
};

smithay::backend::renderer::element::render_elements! {
    /// One drawable piece of the scene.
    pub SceneElement<=GlesRenderer>;
    Solid = SolidColorRenderElement,
    Texture = smithay::backend::renderer::element::texture::TextureRenderElement<GlesTexture>,
    Surface = WaylandSurfaceRenderElement<GlesRenderer>,
}

/// The whole scene of `output`, top to bottom: overlay/top layers, then every window followed by
/// its own border (so a border never paints over a window above it), then bottom/background
/// layers. While the session is locked: only the lock surface of `output`.
pub fn scene_elements(state: &mut State, renderer: &mut GlesRenderer, output: &Output) -> Vec<SceneElement> {
    let scale = state.config.general.scale;
    let mut out: Vec<SceneElement> = Vec::new();

    if state.locked {
        for (o, surface) in &state.lock_surfaces {
            if o == output {
                out.extend(
                    render_elements_from_surface_tree(renderer, surface.wl_surface(), (0, 0), scale, 1.0, Kind::Unspecified)
                        .into_iter()
                        .map(SceneElement::Surface),
                );
            }
        }
        return out;
    }

    if state.config.appearance.enabled() {
        let mut effects = std::mem::take(&mut state.effects);
        let image = effects.render(state, renderer, output);
        state.effects = effects;
        if let Some(image) = image { return vec![SceneElement::Texture(image)]; }
    }

    let layer_elements = |renderer: &mut GlesRenderer, upper: bool| -> Vec<SceneElement> {
        let map = layer_map_for_output(output);
        map.layers()
            .rev()
            .filter(|l| matches!(l.layer(), Layer::Background | Layer::Bottom) != upper)
            .filter_map(|l| map.layer_geometry(l).map(|geo| (geo.loc, l)))
            .flat_map(|(loc, l)| {
                AsRenderElements::<GlesRenderer>::render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    l,
                    renderer,
                    loc.to_physical_precise_round(scale),
                    Scale::from(scale),
                    1.0,
                )
                .into_iter()
                .map(SceneElement::Surface)
                .collect::<Vec<_>>()
            })
            .collect()
    };

    out.extend(layer_elements(renderer, true));

    let mut borders = state.border_strips(output);
    let origin = state.space.output_geometry(output).map(|g| g.loc).unwrap_or_default();
    let windows: Vec<Window> = state.space.elements_for_output(output).rev().cloned().collect();
    for window in windows {
        let Some(location) = state.space.element_location(&window) else { continue };
        let render_location = location - window.geometry().loc - origin;
        out.extend(
            AsRenderElements::<GlesRenderer>::render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                &window,
                renderer,
                render_location.to_physical_precise_round(scale),
                Scale::from(scale),
                1.0,
            )
            .into_iter()
            .map(SceneElement::Surface),
        );
        if let Some(pos) = borders.iter().position(|(w, _)| *w == window) {
            out.extend(borders.swap_remove(pos).1.into_iter().map(SceneElement::Solid));
        }
    }

    out.extend(layer_elements(renderer, false));
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

/// Render `output` offscreen and return its size and upright RGBA bytes (R, G, B, A in memory).
/// Refused while the session is locked (it would leak what the lock screen hides). The cursor is
/// not included.
pub fn render_to_rgba(
    renderer: &mut GlesRenderer,
    state: &mut State,
    output: &Output,
) -> Result<(i32, i32, Vec<u8>), Box<dyn std::error::Error>> {
    if state.locked {
        return Err("session is locked".into());
    }
    let elements = scene_elements(state, renderer, output);
    let background = state.config.background;
    let mode = output.current_mode().ok_or("output has no mode")?;
    let (w, h) = (mode.size.w, mode.size.h);

    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (w, h).into())?;
    let mut framebuffer = renderer.bind(&mut texture)?;
    let mut tracker = OutputDamageTracker::from_output(output);
    tracker.render_output(renderer, &mut framebuffer, 0, &elements, background)?;

    let mapping = renderer.copy_framebuffer(&framebuffer, Rectangle::from_size((w, h).into()), Fourcc::Abgr8888)?;
    let bytes = renderer.map_texture(&mapping)?;
    // The winit output is Flipped180 and its readback needs its rows flipped to come out upright;
    // outputs with a Normal transform (TTY, virtual) are already upright.
    let stride = w as usize * 4;
    let upright: Vec<u8> = if output.current_transform() == Transform::Flipped180 {
        bytes.chunks_exact(stride).rev().flatten().copied().collect()
    } else {
        bytes.to_vec()
    };
    Ok((w, h, upright))
}

/// Render the output offscreen and write a PNG.
pub fn screenshot(
    renderer: &mut GlesRenderer,
    state: &mut State,
    output: &Output,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h, upright) = render_to_rgba(renderer, state, output)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&upright)?;
    Ok(())
}
