mod audio;
mod effects;
mod cli;
mod backend;
mod config;
mod control;
mod cursor;
mod input;
mod layout;
mod schema;
mod screencopy;
mod state;
mod wm;
mod xwm;

use std::{os::fd::{AsFd, BorrowedFd}, process::Stdio, sync::Arc, time::Duration};

use smithay::{
    desktop::{PopupManager, Space},
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{Clock, ClockSource, Monotonic},
    xwayland::{X11Wm, XWayland, XWaylandEvent},
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
        cursor_shape::CursorShapeManagerState,
        fractional_scale::FractionalScaleManagerState,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
        idle_inhibit::IdleInhibitManagerState,
        idle_notify::IdleNotifierState,
        presentation::PresentationState,
        session_lock::SessionLockManagerState,
        xwayland_shell::XWaylandShellState,
    },
};

use backend::{headless::Headless, tty::Tty, winit::Winit};
use config::Config;
use control::{Control, Reply};
use state::{ClientState, State};
use wm::{Mon, Workspace, WORKSPACE_COUNT};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // `rusted-wm --check-config [FILE]`: validate a config file and exit (0 = fine, 1 = problems).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("ctl") {
        std::process::exit(cli::ctl(&args[2..]));
    }
    if args.get(1).map(String::as_str) == Some("config") {
        std::process::exit(cli::run(&args[2..]));
    }
    if args.get(1).map(String::as_str) == Some("--dump-schema") {
        println!("{}", schema::to_json());
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--check-config") {
        let path = args.get(2).map(std::path::PathBuf::from).unwrap_or_else(Config::path);
        let (config, problems) = Config::load_file(&path, true);
        println!("checking {}", path.display());
        for p in &problems {
            println!("  problem: {p}");
        }
        if problems.is_empty() {
            println!("config OK: {} keybinds, {} autostart entries", config.bindings.len(), config.autostart.len());
        }
        std::process::exit(i32::from(!problems.is_empty()));
    }

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    let display: Display<State> = Display::new()?;
    let dh = display.handle();

    // Nested window when started inside another desktop, real display otherwise (override: --winit / --tty).
    let arg = std::env::args().nth(1);
    let headless = arg.as_deref() == Some("--headless");
    let use_tty = match arg.as_deref() {
        Some("--tty") => true,
        Some("--winit") | Some("--headless") => false,
        Some(other) => return Err(format!("unknown argument {other:?} (use --winit, --tty or --headless)").into()),
        None => std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none(),
    };
    tracing::info!(backend = if use_tty { "tty" } else if headless { "headless" } else { "winit" }, "starting");
    let (config, problems) = Config::load(use_tty);
    for p in &problems {
        tracing::warn!("config: {p}");
    }
    let (winit, tty, headless) = match (use_tty, headless) {
        (true, _) => (None, Some(Tty::new(&dh, &config.outputs)?), None),
        (false, true) => (None, None, Some(Headless::new(&dh)?)),
        (false, false) => (Some(Winit::new(&dh)?), None, None),
    };
    let outputs: Vec<Mon> = match (&winit, &tty, &headless) {
        (Some(w), _, _) => vec![Mon { output: w.output.clone(), slot: 0, ws: 0 }],
        (_, Some(t), _) => t
            .outputs()
            .into_iter()
            .enumerate()
            .map(|(i, output)| Mon { output, slot: i, ws: i * WORKSPACE_COUNT })
            .collect(),
        (_, _, Some(h)) => vec![Mon { output: h.output.clone(), slot: 0, ws: 0 }],
        _ => unreachable!(),
    };
    let session = tty.as_ref().map(|t| t.session.clone());
    let space = Space::default();

    let mut seat_state = smithay::input::SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "seat0");
    let primary_selection_state = PrimarySelectionState::new::<State>(&dh);
    let data_control_state = DataControlState::new::<State, _>(&dh, Some(&primary_selection_state), |_| true);
    let mut state = State {
        effects: effects::Effects::default(),
        display_handle: dh.clone(),
        compositor_state: CompositorState::new::<State>(&dh),
        xdg_shell_state: XdgShellState::new::<State>(&dh),
        xdg_decoration_state: XdgDecorationState::new::<State>(&dh),
        layer_shell_state: WlrLayerShellState::new::<State>(&dh),
        shm_state: ShmState::new::<State>(&dh, vec![]),
        output_manager_state: OutputManagerState::new_with_xdg_output::<State>(&dh),
        viewporter_state: ViewporterState::new::<State>(&dh),
        cursor_shape_state: CursorShapeManagerState::new::<State>(&dh),
        fractional_scale_state: FractionalScaleManagerState::new::<State>(&dh),
        xdg_activation_state: XdgActivationState::new::<State>(&dh),
        xwayland_shell_state: XWaylandShellState::new::<State>(&dh),
        idle_notifier_state: IdleNotifierState::new(&dh, handle.clone()),
        idle_inhibit_state: IdleInhibitManagerState::new::<State>(&dh),
        inhibitors: Vec::new(),
        session_lock_state: SessionLockManagerState::new::<State, _>(&dh, |_| true),
        locked: false,
        lock_surfaces: Vec::new(),
        presentation_state: PresentationState::new::<State>(&dh, <Monotonic as ClockSource>::ID as u32),
        clock: Clock::new(),
        screencopy: screencopy::ScreencopyState::new(&dh),
        xwm: None,
        loop_handle: handle.clone(),
        seat_state,
        data_device_state: DataDeviceState::new::<State>(&dh),
        primary_selection_state,
        data_control_state,
        seat,
        popups: PopupManager::default(),
        space,
        workspaces: (0..WORKSPACE_COUNT * outputs.len()).map(|_| Workspace::default()).collect(),
        slots: outputs.iter().map(|m| m.output.name()).collect(),
        outputs,
        focused: 0,
        drag: None,
        split_ratio: config.general.split_ratio,
        config,
        session,
        cursor_status: smithay::input::pointer::CursorImageStatus::default_named(),
        pending_shot: None,
        running: true,
    };
    let input = &state.config.input;
    let xkb = smithay::input::keyboard::XkbConfig {
        layout: &input.layout,
        variant: &input.variant,
        options: (!input.options.is_empty()).then(|| input.options.clone()),
        ..Default::default()
    };
    state.seat.add_keyboard(xkb, input.repeat_delay, input.repeat_rate)?;
    state.seat.add_pointer();
    state.apply_scale();

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

    // Xwayland for X11 apps. The X sockets exist as soon as it is spawned (it is started on
    // demand), so DISPLAY can be exported right away; the window manager starts when it is ready.
    if state.config.general.xwayland {
        match XWayland::spawn(&dh, None, std::iter::empty::<(String, String)>(), true, Stdio::null(), Stdio::null(), |_| {}) {
            Ok((xwayland, client)) => {
                let display_number = xwayland.display_number();
                // Safety: still single-threaded.
                unsafe { std::env::set_var("DISPLAY", format!(":{display_number}")) };
                tracing::info!("Xwayland on DISPLAY=:{display_number}");
                let wm_handle = handle.clone();
                handle.insert_source(xwayland, move |event, _, state| match event {
                    XWaylandEvent::Ready { x11_socket, .. } => {
                        match X11Wm::start_wm(wm_handle.clone(), x11_socket, client.clone()) {
                            Ok(wm) => {
                                state.xwm = Some(wm);
                                tracing::info!("X11 window manager running");
                            }
                            Err(e) => tracing::warn!("failed to start the X11 window manager: {e}"),
                        }
                    }
                    XWaylandEvent::Error => tracing::warn!("Xwayland failed to start"),
                })?;
            }
            Err(e) => tracing::warn!("Xwayland unavailable: {e}"),
        }
    }

    // Debug/automation control socket.
    let control = Control::bind(&socket_name)?;
    tracing::info!("control socket: {}", control.path().display());
    handle.insert_source(Generic::new(control, Interest::READ, Mode::Level), |_, control, state| {
        // Safety: the control socket is never dropped while the source lives.
        let control = unsafe { control.get_mut() };
        while let Some((line, stream)) = control.poll() {
            match state.run_command(&line) {
                Reply::Text(t) => control::answer(stream, &t),
                Reply::Screenshot(path, mon) => state.pending_shot = Some((path, mon, stream)),
            }
        }
        let _ = state.display_handle.flush_clients();
        Ok(PostAction::Continue)
    })?;

    if let Some(winit) = winit {
        winit.register(&handle)?;
    }
    if let Some(headless) = headless {
        headless.register(&handle)?;
    }
    if let Some(tty) = tty {
        tty.register(&handle)?;
        // Tell children what kind of session this is.
        // Safety: no other threads are running yet.
        unsafe {
            std::env::set_var("XDG_SESSION_TYPE", "wayland");
            std::env::set_var("XDG_CURRENT_DESKTOP", "rusted");
        }
        // Let D-Bus-activated programs (portals, notification daemons) find this compositor.
        state.spawn("dbus-update-activation-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE 2>/dev/null");
    }

    // pywal: pick up a new palette when `wal` rewrites its colors.json (checked every 2 s, only
    // while colors.pywal is on).
    {
        let mut seen: Option<Option<std::time::SystemTime>> = None; // None = not looked yet
        handle.insert_source(Timer::from_duration(Duration::from_secs(2)), move |_, _, state| {
            if let Some(path) = state.config.pywal_file.clone() {
                let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                match seen {
                    Some(previous) if previous != modified => {
                        tracing::info!("pywal colours changed, reloading");
                        state.reload_config();
                        seen = Some(modified);
                    }
                    None => {
                        // A palette may change between enabling pywal and this first tick.
                        state.reload_config();
                        seen = Some(modified);
                    },
                    _ => {}
                }
            } else {
                seen = None;
            }
            TimeoutAction::ToDuration(Duration::from_secs(2))
        })?;
    }

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
