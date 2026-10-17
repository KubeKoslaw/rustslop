//! Compositor state and Wayland protocol handlers.

use std::os::unix::io::OwnedFd;

use smithay::wayland::seat::WaylandFocus as _;
use smithay::{
    backend::{renderer::utils::on_commit_buffer_handler, session::libseat::LibSeatSession},
    delegate_compositor, delegate_cursor_shape, delegate_data_control, delegate_fractional_scale, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_primary_selection, delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_activation, delegate_xwayland_shell, delegate_xdg_decoration,
    delegate_xdg_shell,
    desktop::{layer_map_for_output, LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType},
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    wayland::tablet_manager::TabletSeatHandler,
    output::Output,
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        calloop::LoopHandle,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_output::WlOutput, wl_seat, wl_surface::WlSurface},
            Client, DisplayHandle, Resource,
        },
    },
    utils::Serial,
    xwayland::{xwm::XwmId, X11Surface, X11Wm, XWaylandClientData},
    wayland::{
        buffer::BufferHandler,
        compositor::{get_parent, with_states, is_sync_subsurface, CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            SelectionSource, SelectionTarget,
            primary_selection::{set_primary_focus, PrimarySelectionHandler, PrimarySelectionState},
            wlr_data_control::{DataControlHandler, DataControlState},
            SelectionHandler,
        },
        shell::{
            wlr_layer::{
                Layer, KeyboardInteractivity, WlrLayerShellHandler, WlrLayerShellState,
            },
            xdg::{
                decoration::{XdgDecorationHandler, XdgDecorationState},
                PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            },
        },
        shm::{ShmHandler, ShmState},
        viewporter::ViewporterState,
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
        xdg_activation::{XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData},
        cursor_shape::CursorShapeManagerState,
        fractional_scale::{with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState},
    },
};

use crate::{
    config::Config,
    wm::{Drag, Mon, Workspace},
};

pub struct State {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    #[allow(dead_code)] // keeps the xdg-decoration global alive
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    #[allow(dead_code)] // keeps the xdg-output global alive
    pub output_manager_state: OutputManagerState,
    #[allow(dead_code)] // keeps the viewporter global alive
    pub viewporter_state: ViewporterState,
    #[allow(dead_code)] // keeps the cursor-shape global alive
    pub cursor_shape_state: CursorShapeManagerState,
    #[allow(dead_code)] // keeps the fractional-scale global alive
    pub fractional_scale_state: FractionalScaleManagerState,
    pub xdg_activation_state: XdgActivationState,
    pub xwayland_shell_state: XWaylandShellState,
    /// The X11 window manager, once Xwayland is up.
    pub xwm: Option<X11Wm>,
    pub loop_handle: LoopHandle<'static, State>,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub data_control_state: DataControlState,
    pub seat: Seat<Self>,
    pub popups: PopupManager,

    pub space: Space<Window>,
    pub workspaces: Vec<Workspace>,
    /// Monitors, each with the workspace it shows; `focused` indexes the current one.
    pub outputs: Vec<Mon>,
    pub focused: usize,
    pub drag: Option<Drag>,
    /// Share of each dwindle split taken by the first window.
    pub split_ratio: f32,
    pub config: Config,
    /// Present on a real (TTY) session: used for VT switching.
    pub session: Option<LibSeatSession>,
    /// Cursor requested by the focused client (drawn by the TTY backend).
    pub cursor_status: CursorImageStatus,
    /// Screenshot requested over the control socket; served by the backend after the next frame.
    pub pending_shot: Option<(std::path::PathBuf, usize, std::os::unix::net::UnixStream)>,
    pub running: bool,
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        tracing::info!("client connected");
    }

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        tracing::info!("client disconnected");
    }
}

impl State {
    /// Find the window (on any workspace) whose root surface is `surface`.
    /// Apply `general.scale` to every output, re-place the outputs and tell every toplevel/layer
    /// surface about the scale.
    pub fn apply_scale(&mut self) {
        let scale = self.config.general.scale;
        for m in &self.outputs {
            m.output.change_current_state(None, None, Some(smithay::output::Scale::Fractional(scale)), None);
        }
        self.layout_outputs();
        let mut surfaces: Vec<WlSurface> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.entries.iter())
            .filter_map(|e| e.window.wl_surface().map(|s| s.into_owned()))
            .collect();
        for m in &self.outputs {
            surfaces.extend(layer_map_for_output(&m.output).layers().map(|l| l.wl_surface().clone()));
        }
        for s in surfaces {
            with_states(&s, |states| with_fractional_scale(states, |fs| fs.set_preferred_scale(scale)));
        }
    }

    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.workspaces
            .iter()
            .flat_map(|ws| ws.entries.iter())
            .map(|e| &e.window)
            .find(|w| w.wl_surface().is_some_and(|s| &*s == surface))
            .cloned()
    }

    /// The window wrapping an X11 surface (managed or override-redirect).
    pub fn window_for_x11(&self, surface: &X11Surface) -> Option<Window> {
        self.space
            .elements()
            .chain(self.workspaces.iter().flat_map(|ws| ws.entries.iter()).map(|e| &e.window))
            .find(|w| w.x11_surface() == Some(surface))
            .cloned()
    }

    /// The layer surface owning `surface` and the output it lives on.
    fn layer_for_surface(&self, surface: &WlSurface) -> Option<(LayerSurface, Output)> {
        self.outputs.iter().find_map(|m| {
            let map = layer_map_for_output(&m.output);
            let layer = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).cloned()?;
            Some((layer, m.output.clone()))
        })
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(data) = client.get_data::<XWaylandClientData>() {
            return &data.compositor_state;
        }
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.popups.commit(surface);
        self.ensure_popup_configured(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for_surface(&root) {
                window.on_commit();
            }
            if let Some((layer, output)) = self.layer_for_surface(&root) {
                // Bars may change the usable area, so re-arrange and re-tile.
                layer_map_for_output(&output).arrange();
                // arrange() never sends the initial configure itself; this sends it (and any other
                // pending change) so the client can map.
                layer.layer_surface().send_pending_configure();
                self.relayout();
                if layer.can_receive_keyboard_focus()
                    && layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                    && self.seat.get_keyboard().unwrap().current_focus().as_ref() != Some(layer.wl_surface())
                {
                    self.focus_surface(layer.wl_surface().clone());
                }
            }
        }
    }
}

impl State {
    /// xdg popups must get an initial configure after their first commit.
    fn ensure_popup_configured(&mut self, surface: &WlSurface) {
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                if let Err(e) = popup.send_configure() {
                    tracing::warn!("popup initial configure failed: {e:?}");
                }
            }
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl OutputHandler for State {}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        tracing::info!("new toplevel");
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Activated);
        });
        self.add_window(Window::new_wayland_window(surface));
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // The window may live on a workspace that is not currently mapped.
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.remove_window(&window);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|s| s.geometry = positioner.get_geometry());
        if let Err(e) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!("failed to track popup: {e:?}");
        }
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        match self.window_for_surface(surface.wl_surface()) {
            Some(window) => self.set_fullscreen(&window, true),
            None => {
                surface.send_configure();
            }
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        match self.window_for_surface(surface.wl_surface()) {
            Some(window) => self.set_fullscreen(&window, false),
            None => {
                surface.send_configure();
            }
        }
    }

    // Tiling decides the size; just answer the request so the client does not wait.
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        surface.send_pending_configure();
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        surface.send_pending_configure();
    }

    // Popup grabs (click outside dismisses a menu) are not implemented yet; menus still work and
    // are closed by their client.
    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}
}

impl XdgDecorationHandler for State {
    // A tiling compositor draws no title bars: tell clients to skip theirs.
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        set_server_side(&toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        set_server_side(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        set_server_side(&toplevel);
    }
}

fn set_server_side(toplevel: &ToplevelSurface) {
    toplevel.with_pending_state(|s| s.decoration_mode = Some(DecorationMode::ServerSide));
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
}

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(&mut self, surface: smithay::wayland::shell::wlr_layer::LayerSurface, wl_output: Option<WlOutput>, _layer: Layer, namespace: String) {
        tracing::info!(%namespace, "new layer surface");
        // The requested output, or the focused one.
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .filter(|o| self.output_index(o).is_some())
            .unwrap_or_else(|| self.focused_output().clone());
        let layer = LayerSurface::new(surface, namespace);
        if let Err(e) = layer_map_for_output(&output).map_layer(&layer) {
            tracing::warn!("failed to map layer surface: {e}");
        }
    }

    fn layer_destroyed(&mut self, surface: smithay::wayland::shell::wlr_layer::LayerSurface) {
        let found = self.outputs.iter().find_map(|m| {
            let layer = layer_map_for_output(&m.output).layers().find(|l| l.layer_surface() == &surface).cloned()?;
            Some((layer, m.output.clone()))
        });
        if let Some((layer, output)) = found {
            let had_focus = self.seat.get_keyboard().unwrap().current_focus().as_ref() == Some(layer.wl_surface());
            layer_map_for_output(&output).unmap_layer(&layer);
            self.relayout();
            if had_focus {
                self.focus_top();
            }
        }
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    // Clipboard and primary selection follow keyboard focus.
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|s| self.display_handle.get_client(s.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }
}

/// A client asked to be focused (e.g. a launched app, or a notification click). Only tokens that
/// came from a user input event and are recent are honoured, so apps cannot steal focus on their own.
impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn request_activation(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData, surface: WlSurface) {
        if data.serial.is_none() || data.timestamp.elapsed().as_secs() >= 10 {
            tracing::debug!("ignoring activation request without a fresh input serial");
            return;
        }
        let Some(window) = self.window_for_surface(&surface) else { return };
        if let Some(ws) = self.workspaces.iter().position(|ws| ws.entries.iter().any(|e| e.window == window)) {
            self.switch_workspace(ws);
            self.focus_window(&window);
        }
    }
}

impl TabletSeatHandler for State {}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Walk up to the root so subsurfaces and popups get the same scale as their window.
        let mut root = surface;
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let scale = self.config.general.scale;
        with_states(&root, |states| with_fractional_scale(states, |fs| fs.set_preferred_scale(scale)));
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();

    // Wayland client set a selection: offer it to X11 clients.
    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, _seat: Seat<Self>) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(e) = xwm.new_selection(ty, source.map(|s| s.mime_types())) {
                tracing::warn!("failed to forward {ty:?} selection to X11: {e:?}");
            }
        }
    }

    // An X11 client wants to read the Wayland selection.
    fn send_selection(&mut self, ty: SelectionTarget, mime_type: String, fd: OwnedFd, _seat: Seat<Self>, _data: &()) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(e) = xwm.send_selection(ty, mime_type, fd, self.loop_handle.clone()) {
                tracing::warn!("failed to send {ty:?} selection to X11: {e:?}");
            }
        }
    }
}

impl XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    // The window now has a wl_surface: if it is the one that should have focus, focus it.
    fn surface_associated(&mut self, _xwm: XwmId, _wl_surface: WlSurface, surface: X11Surface) {
        if let Some(window) = self.window_for_x11(&surface) {
            if self.mon_of_window(&window) == Some(self.focused)
                && self.focused_stack_top().as_ref() == Some(&window)
            {
                self.focus_window(&window);
            }
        }
    }
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl DataControlHandler for State {
    fn data_control_state(&self) -> &DataControlState {
        &self.data_control_state
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

delegate_compositor!(State);
delegate_shm!(State);
delegate_output!(State);
delegate_viewporter!(State);
delegate_cursor_shape!(State);
delegate_fractional_scale!(State);
delegate_xdg_activation!(State);
delegate_xwayland_shell!(State);
delegate_xdg_shell!(State);
delegate_xdg_decoration!(State);
delegate_layer_shell!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_data_control!(State);
