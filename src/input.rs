//! Input handling: keybinds, pointer focus, click-to-focus.

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, Device, DeviceCapability, PointerMotionEvent, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState,
            KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
        },
    },
    desktop::{layer_map_for_output, PopupKind, PopupManager, WindowSurfaceType},
    input::{
        keyboard::{keysyms, FilterResult},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::shell::wlr_layer::Layer as WlrLayer,
};

use smithay::backend::session::Session as _;
use crate::{config::normalize_sym, state::State, wm::DragKind};

/// Compositor actions triggered by keybinds (and by the debug control socket).
#[derive(Debug, Clone)]
pub enum Action {
    Exec(String),
    Reload,
    Close,
    FocusNext,
    FocusDir(i32, i32),
    MoveToOutput(i32, i32),
    ToggleFloating,
    ToggleFullscreen,
    SwapDir(i32, i32),
    Resize(f32),
    Workspace(usize),
    MoveToWorkspace(usize),
    SwitchVt(i32),
    Audio(crate::audio::AudioCmd),
    Quit,
}

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

impl State {
    pub fn handle_input<B: InputBackend>(&mut self, event: InputEvent<B>)
    where
        B::Device: 'static,
    {
        // Any input counts as activity for idle notifiers (swayidle).
        if matches!(
            event,
            InputEvent::Keyboard { .. }
                | InputEvent::PointerMotion { .. }
                | InputEvent::PointerMotionAbsolute { .. }
                | InputEvent::PointerButton { .. }
                | InputEvent::PointerAxis { .. }
        ) {
            let seat = self.seat.clone();
            self.idle_notifier_state.notify_activity(&seat);
        }
        match event {
            InputEvent::Keyboard { event } => self.on_keyboard(event.key_code(), event.state(), event.time_msec()),
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(geo) = self.space.output_geometry(&self.outputs[0].output) else { return };
                let pos = Point::<f64, Logical>::from((
                    event.x_transformed(geo.size.w) + geo.loc.x as f64,
                    event.y_transformed(geo.size.h) + geo.loc.y as f64,
                ));
                self.move_pointer(pos, event.time_msec());
            }
            InputEvent::PointerMotion { event } => {
                let cur = self.seat.get_pointer().unwrap().current_location();
                let pos = self.clamp_to_outputs(Point::<f64, Logical>::from((cur.x + event.delta_x(), cur.y + event.delta_y())));
                self.move_pointer(pos, event.time_msec());
            }
            InputEvent::PointerButton { event } => self.on_button(event.button_code(), event.state(), event.time_msec()),
            InputEvent::PointerAxis { event } => self.on_axis::<B>(event),
            InputEvent::DeviceAdded { device } => self.configure_device(&device),
            _ => {}
        }
    }

    fn on_keyboard(&mut self, code: smithay::input::keyboard::Keycode, key_state: KeyState, time: u32) {
        let keyboard = self.seat.get_keyboard().unwrap();
        let action = keyboard.input::<Action, _>(self, code, key_state, SERIAL_COUNTER.next_serial(), time, |state, mods, handle| {
            if key_state == KeyState::Pressed {
                // Ctrl+Alt+F1..F12 arrive as XF86Switch_VT_n.
                let modified = handle.modified_sym().raw();
                if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&modified) {
                    return FilterResult::Intercept(Action::SwitchVt((modified - keysyms::KEY_XF86Switch_VT_1 + 1) as i32));
                }
                // Raw (unshifted) symbol, so e.g. Shift+1 is still "1".
                let sym = handle.raw_latin_sym_or_raw_current_sym().unwrap_or(handle.modified_sym()).raw();
                let sym = normalize_sym(sym);
                let key_name = smithay::input::keyboard::xkb::keysym_get_name(smithay::input::keyboard::Keysym::new(sym));
                tracing::debug!(key = %key_name, ?mods, "key pressed");
                // Keybinds are off while the session is locked (only VT switching above works).
                if state.locked {
                    return FilterResult::Forward;
                }
                if let Some(b) = state.config.bindings.iter().find(|b| b.matches(mods, sym)) {
                    return FilterResult::Intercept(b.action.clone());
                }
                // Media keys that nothing is bound to: say so, it is the first thing to check when
                // a knob or key "does nothing".
                if key_name.starts_with("XF86") {
                    tracing::info!(key = %key_name, "unbound media key (bind it in [keybinds])");
                }
            }
            FilterResult::Forward
        });
        if let Some(action) = action {
            self.run_action(action);
        }
    }

    pub fn run_action(&mut self, action: Action) {
        tracing::debug!(?action, "keybind");
        match action {
            Action::Exec(cmd) => self.spawn(&cmd),
            Action::Reload => {
                self.reload_config();
            }
            Action::Close => {
                if let Some(window) = self.focused_window() {
                    crate::wm::close_window(&window);
                }
            }
            Action::FocusNext => self.focus_next(),
            Action::FocusDir(dx, dy) => self.focus_dir(dx, dy),
            Action::MoveToOutput(dx, dy) => self.move_to_output(dx, dy),
            Action::ToggleFloating => self.toggle_floating(),
            Action::ToggleFullscreen => self.toggle_fullscreen(),
            Action::SwapDir(dx, dy) => self.swap_dir(dx, dy),
            Action::Resize(delta) => self.adjust_ratio(delta),
            Action::Workspace(n) => self.switch_workspace(n),
            Action::MoveToWorkspace(n) => self.move_to_workspace(n),
            Action::Audio(cmd) => crate::audio::run(cmd, self.config.audio.clone()),
            Action::SwitchVt(vt) => {
                if let Some(session) = &mut self.session {
                    if let Err(e) = session.change_vt(vt) {
                        tracing::warn!("VT switch to {vt} failed: {e}");
                    }
                }
            }
            Action::Quit => self.running = false,
        }
    }

    /// Match the rendered rounded shape, while keeping popup input outside its parent.
    fn window_under_rounded(&self, pos: Point<f64, Logical>) -> Option<(smithay::desktop::Window, Point<i32, Logical>)> {
        self.space.elements().rev().find_map(|window| {
            let location = self.space.element_location(window)?;
            let geometry = window.geometry();
            let origin = location - geometry.loc;
            let local = pos - origin.to_f64();
            if window.surface_under(local, WindowSurfaceType::POPUP | WindowSurfaceType::SUBSURFACE).is_some() {
                return Some((window.clone(), origin));
            }
            let fullscreen = self.workspaces.iter().flat_map(|ws| &ws.entries).any(|e| e.window == *window && e.fullscreen);
            let radius = if fullscreen { 0 } else { self.config.appearance.corner_radius };
            let point = pos - location.to_f64();
            if crate::effects::contains_rounded((geometry.size.w, geometry.size.h), (point.x, point.y), radius)
                && window.surface_under(local, WindowSurfaceType::ALL).is_some() {
                Some((window.clone(), origin))
            } else { None }
        })
    }

    /// Surface under `pos` with its origin in global coordinates. Overlay/top layer surfaces
    /// (bars, launchers) are above windows, bottom/background ones below.
    fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let mon = self.mon_at(pos)?;
        let output = &self.outputs[mon].output;
        if self.locked {
            let (_, lock) = self.lock_surfaces.iter().find(|(o, s)| o == output && s.alive())?;
            let origin = self.space.output_geometry(output)?.loc;
            return Some((lock.wl_surface().clone(), origin.to_f64()));
        }
        let output_loc = self.space.output_geometry(output)?.loc;
        let map = layer_map_for_output(output);
        let layer_hit = |layers: &[WlrLayer]| {
            layers.iter().find_map(|&l| {
                let layer = map.layer_under(l, pos - output_loc.to_f64())?;
                let geo = map.layer_geometry(layer)?;
                let (surface, offset) = layer.surface_under(pos - output_loc.to_f64() - geo.loc.to_f64(), WindowSurfaceType::ALL)?;
                Some((surface, (output_loc + geo.loc + offset).to_f64()))
            })
        };
        if let Some(hit) = layer_hit(&[WlrLayer::Overlay, WlrLayer::Top]) {
            return Some(hit);
        }
        if let Some((window, loc)) = self.window_under_rounded(pos) {
            if let Some((surface, offset)) = window.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL) {
                return Some((surface, (loc + offset).to_f64()));
            }
        }
        layer_hit(&[WlrLayer::Bottom, WlrLayer::Background])
    }

    /// Keep `pos` inside the union of outputs (nearest point of the nearest output).
    fn clamp_to_outputs(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        self.outputs
            .iter()
            .filter_map(|m| self.space.output_geometry(&m.output))
            .map(|g| {
                let c = Point::<f64, Logical>::from((
                    pos.x.clamp(g.loc.x as f64, (g.loc.x + g.size.w - 1) as f64),
                    pos.y.clamp(g.loc.y as f64, (g.loc.y + g.size.h - 1) as f64),
                ));
                ((c.x - pos.x).powi(2) + (c.y - pos.y).powi(2), c)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, c)| c)
            .unwrap_or(pos)
    }

    pub fn move_pointer(&mut self, pos: Point<f64, Logical>, time: u32) {
        let pointer = self.seat.get_pointer().unwrap();
        // While dragging a window the clients don't get pointer focus.
        let focus = if self.drag.is_some() {
            self.update_drag(pos);
            None
        } else {
            self.surface_under(pos)
        };
        // Crossing onto another monitor moves keyboard focus there too.
        if self.drag.is_none() {
            if let Some(mon) = self.mon_at(pos).filter(|&m| m != self.focused) {
                self.focused = mon;
                self.focus_top(); // the lock surface of that output while locked
            }
        }
        pointer.motion(self, focus, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    /// Poor man's popup grab: a click outside every popup of the focused window closes its menus.
    fn dismiss_popups_outside(&mut self, pos: Point<f64, Logical>) {
        let Some(root) = self.seat.get_keyboard().unwrap().current_focus() else { return };
        let popups: Vec<PopupKind> = PopupManager::popups_for_surface(&root).map(|(p, _)| p).collect();
        if popups.is_empty() {
            return;
        }
        let hit = self.surface_under(pos).map(|(s, _)| s);
        if !popups.iter().any(|p| Some(p.wl_surface()) == hit.as_ref()) {
            for popup in &popups {
                let _ = PopupManager::dismiss_popup(&root, popup);
            }
        }
    }

    pub fn on_button(&mut self, button: u32, state: ButtonState, time: u32) {
        let pointer = self.seat.get_pointer().unwrap();
        let alt = self.seat.get_keyboard().unwrap().modifier_state().alt;
        let pos = pointer.current_location();

        if self.locked {
            pointer.button(self, &ButtonEvent { button, state, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if state == ButtonState::Pressed && alt && (button == BTN_LEFT || button == BTN_RIGHT) {
            if let Some(window) = self.window_under_rounded(pos).map(|(w, _)| w) {
                let kind = if button == BTN_LEFT { DragKind::Move } else { DragKind::Resize };
                self.begin_drag(&window, kind, pos);
                return;
            }
        }
        if state == ButtonState::Released && self.drag.take().is_some() {
            return;
        }

        if state == ButtonState::Pressed {
            self.dismiss_popups_outside(pos);
            let layer = {
                let mon = self.mon_at(pos);
                let output = mon.map(|m| self.outputs[m].output.clone());
                let hit = output.and_then(|o| {
                    let local = pos - self.space.output_geometry(&o).unwrap_or_default().loc.to_f64();
                    let map = layer_map_for_output(&o);
                    [WlrLayer::Overlay, WlrLayer::Top, WlrLayer::Bottom, WlrLayer::Background]
                        .iter()
                        .find_map(|&l| map.layer_under(l, local).cloned())
                });
                hit.filter(|l| l.can_receive_keyboard_focus())
            };
            if let Some(layer) = layer {
                self.focus_surface(layer.wl_surface().clone());
            } else if let Some(window) = self.window_under_rounded(pos).map(|(w, _)| w) {
                self.focus_window(&window);
            }
        }
        pointer.button(self, &ButtonEvent { button, state, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    fn on_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        let amount = |axis| {
            event.amount(axis).unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0)
        };
        let (h, v) = (amount(Axis::Horizontal), amount(Axis::Vertical));

        let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
        for (axis, value) in [(Axis::Horizontal, h), (Axis::Vertical, v)] {
            if value != 0.0 {
                frame = frame.relative_direction(axis, event.relative_direction(axis)).value(axis, value);
                if let Some(discrete) = event.amount_v120(axis) {
                    frame = frame.v120(axis, discrete as i32);
                }
            } else if event.source() == AxisSource::Finger {
                frame = frame.stop(axis);
            }
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}

impl State {
    /// Apply touchpad settings from the config to a newly added libinput device. Other backends
    /// (winit) hand us devices that are not libinput ones, which is fine: nothing happens.
    fn configure_device<D: Device + 'static>(&self, device: &D) {
        use smithay::reexports::input::Device as LibinputDevice;
        if !device.has_capability(DeviceCapability::Pointer) {
            return;
        }
        let any: &dyn std::any::Any = device;
        if let Some(dev) = any.downcast_ref::<LibinputDevice>() {
            let mut dev = dev.clone();
            // Touchpads report tap fingers; mice do not. Natural scrolling is a separate choice for
            // each: it used to apply to mice too, which made the wheel scroll backwards.
            let touchpad = dev.config_tap_finger_count() > 0;
            if touchpad {
                let _ = dev.config_tap_set_enabled(self.config.input.tap_to_click);
            }
            if dev.config_scroll_has_natural_scroll() {
                let natural = if touchpad { self.config.input.natural_scroll } else { self.config.input.mouse_natural_scroll };
                let _ = dev.config_scroll_set_natural_scroll_enabled(natural);
            }
        }
    }
}
