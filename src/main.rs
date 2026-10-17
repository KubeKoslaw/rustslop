mod control;
mod input;
mod layout;
mod state;
mod wm;

use std::{sync::Arc, time::Duration};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::solid::SolidColorRenderElement,
            ExportMem, Bind, Offscreen,
            damage::OutputDamageTracker, gles::{GlesRenderer, GlesTexture},
            Color32F,
        },
        winit::{self, WinitEvent},
    },
    desktop::{space::render_output, Space, Window},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        wayland_server::{Display, ListeningSocket},
        winit::platform::pump_events::PumpStatus,
    },
    utils::{Rectangle, Transform},
    wayland::{
        compositor::CompositorState, output::OutputManagerState, selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState, shm::ShmState,
    },
};

use control::{Control, Reply};
use state::{ClientState, State};
use wm::{Workspace, WORKSPACE_COUNT};

const BACKGROUND: Color32F = Color32F::new(0.08, 0.08, 0.12, 1.0);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let mut display: Display<State> = Display::new()?;
    let dh = display.handle();

    let (mut backend, mut winit) = winit::init::<GlesRenderer>()?;
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
    let _global = output.create_global::<State>(&dh);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);

    let mut space = Space::default();
    space.map_output(&output, (0, 0));

    let mut seat_state = smithay::input::SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "seat0");
    let mut state = State {
        compositor_state: CompositorState::new::<State>(&dh),
        xdg_shell_state: XdgShellState::new::<State>(&dh),
        shm_state: ShmState::new::<State>(&dh, vec![]),
        output_manager_state: OutputManagerState::new_with_xdg_output::<State>(&dh),
        seat_state,
        data_device_state: DataDeviceState::new::<State>(&dh),
        seat,
        space,
        workspaces: (0..WORKSPACE_COUNT).map(|_| Workspace::default()).collect(),
        current_ws: 0,
        drag: None,
        output: output.clone(),
        running: true,
    };
    state.seat.add_keyboard(Default::default(), 200, 25)?;
    state.seat.add_pointer();

    let listener = ListeningSocket::bind_auto("rusted", 1..33)?;
    let socket_name = listener.socket_name().unwrap().to_string_lossy().into_owned();
    tracing::info!("listening on WAYLAND_DISPLAY={socket_name}");
    // Safety: still single-threaded here.
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };

    let control = Control::bind(&socket_name)?;
    tracing::info!("control socket: {}", control.path().display());
    let mut pending_shot: Option<(std::path::PathBuf, std::os::unix::net::UnixStream)> = None;

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    let start_time = std::time::Instant::now();
    let mut clients = Vec::new();

    while state.running {
        let status = winit.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(Some(Mode { size, refresh: 60_000 }), None, None, None);
                state.relayout();
            }
            WinitEvent::Input(event) => state.handle_input(event),
            WinitEvent::CloseRequested => state.running = false,
            _ => {}
        });
        if let PumpStatus::Exit(_) = status {
            break;
        }

        while let Some((line, stream)) = control.poll() {
            match state.run_command(&line) {
                Reply::Text(t) => control::answer(stream, &t),
                Reply::Screenshot(path) => pending_shot = Some((path, stream)),
            }
        }

        let age = backend.buffer_age().unwrap_or(0);
        let borders = state.border_elements();
        let damage = {
            let (renderer, mut framebuffer) = backend.bind()?;
            let result = render_output(
                &output,
                renderer,
                &mut framebuffer,
                1.0,
                age,
                [&state.space],
                &borders,
                &mut damage_tracker,
                BACKGROUND,
            )?;
            result.damage.cloned()
        };

        let time = start_time.elapsed();
        for window in state.space.elements() {
            window.send_frame(&output, time, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        state.space.refresh();

        while let Some(stream) = listener.accept()? {
            clients.push(display.handle().insert_client(stream, Arc::new(ClientState::default()))?);
        }
        display.dispatch_clients(&mut state)?;
        display.flush_clients()?;

        if let Some(damage) = damage {
            backend.submit(Some(&damage))?;
        }

        // Screenshots are rendered offscreen so the window's EGL surface is left alone.
        if let Some((path, stream)) = pending_shot.take() {
            let reply = match screenshot(backend.renderer(), &output, &state.space, &borders, &path) {
                Ok(()) => "ok".to_string(),
                Err(e) => format!("error: {e}"),
            };
            control::answer(stream, &reply);
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    Ok(())
}

/// Render the output into an offscreen texture, read it back and write a PNG.
fn screenshot(
    renderer: &mut GlesRenderer,
    output: &Output,
    space: &Space<Window>,
    borders: &[SolidColorRenderElement],
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mode = output.current_mode().ok_or("output has no mode")?;
    let (w, h) = (mode.size.w, mode.size.h);

    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (w, h).into())?;
    let mut framebuffer = renderer.bind(&mut texture)?;
    let mut tracker = OutputDamageTracker::from_output(output);
    render_output(output, renderer, &mut framebuffer, 1.0, 0, [space], borders, &mut tracker, BACKGROUND)?;

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
