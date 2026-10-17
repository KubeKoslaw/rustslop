//! X11 window management for Xwayland clients.

use std::os::unix::io::OwnedFd;

use smithay::{
    desktop::Window,
    utils::{Logical, Rectangle},
    wayland::selection::{
        data_device::{clear_data_device_selection, request_data_device_client_selection, set_data_device_selection},
        primary_selection::{clear_primary_selection, request_primary_client_selection, set_primary_selection},
        SelectionTarget,
    },
    xwayland::{
        xwm::{ResizeEdge, XwmId},
        X11Surface, X11Wm, XwmHandler,
    },
};

use crate::{state::State, wm::Z_FLOATING};

impl State {
    /// Stop managing an X11 window that was unmapped or destroyed.
    fn drop_x11(&mut self, surface: &X11Surface) {
        let Some(window) = self.window_for_x11(surface) else { return };
        if self.workspaces.iter().any(|ws| ws.entries.iter().any(|e| e.window == window)) {
            self.remove_window(&window);
        } else {
            self.space.unmap_elem(&window);
        }
    }

    fn is_managed(&self, window: &Window) -> bool {
        self.workspaces.iter().any(|ws| ws.entries.iter().any(|e| &e.window == window))
    }
}

impl XwmHandler for State {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("xwm callback without a running X11 window manager")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Err(e) = window.set_mapped(true) {
            tracing::warn!("failed to map X11 window: {e:?}");
            return;
        }
        tracing::info!(title = %window.title(), class = %window.class(), "new X11 window");
        self.add_window(Window::new_x11_window(window));
    }

    // Menus, tooltips, drop-downs: placed exactly where the client wants them, above everything.
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let loc = window.geometry().loc;
        let window = Window::new_x11_window(window);
        window.override_z_index(Z_FLOATING + 1);
        self.space.map_element(window, loc, true);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.drop_x11(&window);
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.drop_x11(&window);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<smithay::xwayland::xwm::Reorder>,
    ) {
        // Managed windows are placed by the layout; re-assert it. Others (not yet mapped) get what
        // they ask for.
        if self.window_for_x11(&window).is_some_and(|w| self.is_managed(&w)) {
            self.relayout();
            return;
        }
        let mut geo = window.geometry();
        if let Some(x) = x {
            geo.loc.x = x;
        }
        if let Some(y) = y {
            geo.loc.y = y;
        }
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        let _ = window.configure(geo);
    }

    fn configure_notify(&mut self, _xwm: XwmId, window: X11Surface, geometry: Rectangle<i32, Logical>, _above: Option<u32>) {
        if !window.is_override_redirect() {
            return;
        }
        if let Some(w) = self.window_for_x11(&window) {
            self.space.map_element(w, geometry.loc, false);
        }
    }

    fn resize_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32, _edge: ResizeEdge) {}
    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.window_for_x11(&window) {
            self.set_fullscreen(&w, true);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.window_for_x11(&window) {
            self.set_fullscreen(&w, false);
        }
    }

    // ---- clipboard bridge (X11 -> Wayland) ----

    fn allow_selection_access(&mut self, _xwm: XwmId, _selection: SelectionTarget) -> bool {
        true
    }

    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        let result = match selection {
            SelectionTarget::Clipboard => request_data_device_client_selection(&self.seat, mime_type, fd).map_err(|e| format!("{e:?}")),
            SelectionTarget::Primary => request_primary_client_selection(&self.seat, mime_type, fd).map_err(|e| format!("{e:?}")),
        };
        if let Err(e) = result {
            tracing::warn!("failed to read the Wayland {selection:?} selection for X11: {e}");
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        match selection {
            SelectionTarget::Clipboard => set_data_device_selection(&self.display_handle, &self.seat, mime_types, ()),
            SelectionTarget::Primary => set_primary_selection(&self.display_handle, &self.seat, mime_types, ()),
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => clear_data_device_selection(&self.display_handle, &self.seat),
            SelectionTarget::Primary => clear_primary_selection(&self.display_handle, &self.seat),
        }
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        tracing::warn!("lost the connection to Xwayland");
        self.xwm = None;
    }
}
