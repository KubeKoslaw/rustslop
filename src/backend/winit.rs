//! Nested backend: the compositor runs as a window inside another Wayland/X11 session.

use std::{path::Path, time::Duration};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker, element::solid::SolidColorRenderElement, gles::{GlesRenderer, GlesTexture},
            Bind, Color32F, ExportMem, Offscreen,
        },
        winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend},
    },
    desktop::{layer_map_for_output, space::render_output, Space, Window},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::{
        timer::{TimeoutAction, Timer},
        LoopHandle,
    },
    utils::{Rectangle, Transform},
    wayland::output::OutputManagerState,
};

use crate::{control, state::State};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

pub struct Winit {
    backend: WinitGraphicsBackend<GlesRenderer>,
    event_loop: WinitEventLoop,
    pub output: Output,
}

impl Winit {
    /// Open the nested window and create the matching `Output` (global is created by the caller's
    /// `DisplayHandle`).
    pub fn new(dh: &smithay::reexports::wayland_server::DisplayHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let (backend, event_loop) = winit::init::<GlesRenderer>()?;
        let mode = Mode { size: backend.window_size(), refresh: 60_000 };
        let output = Output::new(
            "winit".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "rusted".into(),
                model: "nested".into(),
            },
        );
        let _global = output.create_global::<State>(dh);
        output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
        output.set_preferred(mode);
        Ok(Self { backend, event_loop, output })
    }

    /// Hook input and rendering into the event loop.
    pub fn register(self, handle: &LoopHandle<'static, State>) -> Result<(), Box<dyn std::error::Error>> {
        let Winit { mut backend, event_loop, output } = self;

        handle.insert_source(event_loop, {
            let output = output.clone();
            move |event, _, state| match event {
                WinitEvent::Resized { size, .. } => {
                    output.change_current_state(Some(Mode { size, refresh: 60_000 }), None, None, None);
                    layer_map_for_output(&output).arrange();
                    state.relayout();
                }
                WinitEvent::Input(event) => state.handle_input(event),
                WinitEvent::CloseRequested => state.running = false,
                _ => {}
            }
        })?;

        let mut damage_tracker = OutputDamageTracker::from_output(&output);
        let start_time = std::time::Instant::now();
        handle.insert_source(Timer::from_duration(FRAME_INTERVAL), move |_, _, state| {
            if let Err(e) = frame(state, &mut backend, &output, &mut damage_tracker, start_time.elapsed()) {
                tracing::error!("frame failed: {e}");
            }
            TimeoutAction::ToDuration(FRAME_INTERVAL)
        })?;
        Ok(())
    }
}

fn frame(
    state: &mut State,
    backend: &mut WinitGraphicsBackend<GlesRenderer>,
    output: &Output,
    damage_tracker: &mut OutputDamageTracker,
    time: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let age = backend.buffer_age().unwrap_or(0);
    let borders = state.border_elements();
    let background = state.config.background;
    let damage = {
        let (renderer, mut framebuffer) = backend.bind()?;
        let result = render_output(
            output,
            renderer,
            &mut framebuffer,
            1.0,
            age,
            [&state.space],
            &borders,
            damage_tracker,
            background,
        )?;
        result.damage.cloned()
    };

    state.send_frames(output, time);

    // Everything the clients need must be flushed before the (possibly blocking) swap.
    let _ = state.display_handle.flush_clients();
    if let Some(damage) = damage {
        backend.submit(Some(&damage))?;
    }

    // Screenshots are rendered offscreen so the window's EGL surface is left alone.
    if let Some((path, stream)) = state.pending_shot.take() {
        let reply = match screenshot(backend.renderer(), output, &state.space, &borders, background, &path) {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("error: {e}"),
        };
        control::answer(stream, &reply);
    }
    Ok(())
}

/// Render the output into an offscreen texture, read it back and write a PNG.
fn screenshot(
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
    // GL reads back bottom-up and the output is Flipped180: flip rows so the PNG is upright.
    let stride = w as usize * 4;
    let upright: Vec<u8> = bytes.chunks_exact(stride).rev().flatten().copied().collect();
    encoder.write_header()?.write_image_data(&upright)?;
    Ok(())
}

// Keep the xdg-output manager type referenced so a future multi-output refactor can find it here.
#[allow(dead_code)]
type _OutputManager = OutputManagerState;
