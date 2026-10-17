//! Nested backend: the compositor runs as a window inside another Wayland/X11 session.

use std::time::Duration;

use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend},
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::{
        timer::{TimeoutAction, Timer},
        LoopHandle,
    },
    reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
    utils::{Monotonic, Transform},
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
                    state.layout_outputs();
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
    let background = super::clear_color(state);
    let (damage, mut feedback) = {
        let (renderer, mut framebuffer) = backend.bind()?;
        let elements = super::scene_elements(state, renderer, output);
        let result = damage_tracker.render_output(renderer, &mut framebuffer, age, &elements, background)?;
        let feedback = super::collect_feedback(state, output, &result.states);
        (result.damage.cloned(), feedback)
    };

    state.send_frames(output, time);

    // Everything the clients need must be flushed before the (possibly blocking) swap.
    let _ = state.display_handle.flush_clients();
    if let Some(damage) = damage {
        backend.submit(Some(&damage))?;
        // Nested, there is no real scan-out: report presentation as soon as the swap returns.
        feedback.presented::<_, Monotonic>(
            state.clock.now(),
            super::refresh_of(output),
            0,
            wp_presentation_feedback::Kind::empty(),
        );
    }

    if state.has_pending_screencopy() {
        state.serve_screencopy(|state, output| super::render_to_rgba(backend.renderer(), state, output));
    }

    // Screenshots are rendered offscreen so the window's EGL surface is left alone.
    if let Some((path, mon, stream)) = state.pending_shot.take() {
        let target = state.outputs[mon].output.clone();
        let reply = match super::screenshot(backend.renderer(), state, &target, &path) {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("error: {e}"),
        };
        control::answer(stream, &reply);
    }
    Ok(())
}
