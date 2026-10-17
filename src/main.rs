mod backend;
mod config;
mod control;
mod input;
mod layout;
mod state;
mod wm;

use std::{os::fd::{AsFd, BorrowedFd}, sync::Arc, time::Duration};

use smithay::{
    desktop::{PopupManager, Space},
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, Mode, PostAction},
        wayland_server::{Display, ListeningSocket},
    },
    wayland::{
        compositor::CompositorState,
        output::OutputManagerState,
        selection::{
            data_device::DataDeviceState, primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::ShmState,
        viewporter::ViewporterState,
    },
};

use backend::winit::Winit;
use config::Config;
use control::{Control, Reply};
use state::{ClientState, State};
use wm::{Workspace, WORKSPACE_COUNT};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    let display: Display<State> = Display::new()?;
    let dh = display.handle();

    let (config, problems) = Config::load();
    for p in &problems {
        tracing::warn!("config: {p}");
    }

    let winit = Winit::new(&dh)?;
    let output = winit.output.clone();
    let mut space = Space::default();
    space.map_output(&output, (0, 0));

    let mut seat_state = smithay::input::SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "seat0");
    let primary_selection_state = PrimarySelectionState::new::<State>(&dh);
    let data_control_state = DataControlState::new::<State, _>(&dh, Some(&primary_selection_state), |_| true);
    let mut state = State {
        display_handle: dh.clone(),
        compositor_state: CompositorState::new::<State>(&dh),
        xdg_shell_state: XdgShellState::new::<State>(&dh),
        xdg_decoration_state: XdgDecorationState::new::<State>(&dh),
        layer_shell_state: WlrLayerShellState::new::<State>(&dh),
        shm_state: ShmState::new::<State>(&dh, vec![]),
        output_manager_state: OutputManagerState::new_with_xdg_output::<State>(&dh),
        viewporter_state: ViewporterState::new::<State>(&dh),
        seat_state,
        data_device_state: DataDeviceState::new::<State>(&dh),
        primary_selection_state,
        data_control_state,
        seat,
        popups: PopupManager::default(),
        space,
        workspaces: (0..WORKSPACE_COUNT).map(|_| Workspace::default()).collect(),
        current_ws: 0,
        drag: None,
        output,
        config,
        pending_shot: None,
        running: true,
    };
    state.seat.add_keyboard(Default::default(), 200, 25)?;
    state.seat.add_pointer();

    // Wayland clients: protocol requests and new connections.
    handle.insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, state| {
        // Safety: the display is never dropped while the source lives.
        unsafe { display.get_mut().dispatch_clients(state)? };
        let _ = state.display_handle.flush_clients();
        Ok(PostAction::Continue)
    })?;

    let listener = ListeningSocket::bind_auto("rusted", 1..33)?;
    let socket_name = listener.socket_name().unwrap().to_string_lossy().into_owned();
    tracing::info!("listening on WAYLAND_DISPLAY={socket_name}");
    handle.insert_source(Generic::new(listener, Interest::READ, Mode::Level), |_, listener, state| {
        while let Some(stream) = listener.accept()? {
            if let Err(e) = state.display_handle.insert_client(stream, Arc::new(ClientState::default())) {
                tracing::warn!("failed to add client: {e}");
            }
        }
        Ok(PostAction::Continue)
    })?;
    // Safety: still single-threaded (no other threads exist yet).
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };

    // Debug/automation control socket.
    let control = Control::bind(&socket_name)?;
    tracing::info!("control socket: {}", control.path().display());
    handle.insert_source(Generic::new(control, Interest::READ, Mode::Level), |_, control, state| {
        // Safety: the control socket is never dropped while the source lives.
        let control = unsafe { control.get_mut() };
        while let Some((line, stream)) = control.poll() {
            match state.run_command(&line) {
                Reply::Text(t) => control::answer(stream, &t),
                Reply::Screenshot(path) => state.pending_shot = Some((path, stream)),
            }
        }
        let _ = state.display_handle.flush_clients();
        Ok(PostAction::Continue)
    })?;

    winit.register(&handle)?;

    for cmd in state.config.autostart.clone() {
        state.spawn(&cmd);
    }

    let signal = event_loop.get_signal();
    event_loop.run(Duration::from_millis(100), &mut state, |state| {
        if !state.running {
            signal.stop();
        }
    })?;
    Ok(())
}

impl AsFd for Control {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd()
    }
}
