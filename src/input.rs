//! Input handling: keybinds, pointer focus, click-to-focus.

use std::process::Command;

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState,
            KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
        },
        winit::WinitInput,
    },
    desktop::WindowSurfaceType,
    input::{
        keyboard::{keysyms, FilterResult},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
};

use crate::{state::State, wm::DragKind};

/// Compositor actions triggered by keybinds (and by the debug control socket).
#[derive(Debug, Clone, Copy)]
pub enum Action {
    Spawn,
    Close,
    FocusNext,
    FocusDir(i32, i32),
    ToggleFloating,
    Workspace(usize),
    MoveToWorkspace(usize),
    Quit,
}

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

fn workspace_index(sym: u32) -> Option<usize> {
    (keysyms::KEY_1..=keysyms::KEY_9).contains(&sym).then(|| (sym - keysyms::KEY_1) as usize)
}

impl State {
    pub fn handle_input(&mut self, event: InputEvent<WinitInput>) {
        match event {
            InputEvent::Keyboard { event } => self.on_keyboard(event.key_code(), event.state(), event.time_msec()),
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(geo) = self.space.output_geometry(&self.output) else { return };
                let pos = Point::<f64, Logical>::from((
                    event.x_transformed(geo.size.w) + geo.loc.x as f64,
                    event.y_transformed(geo.size.h) + geo.loc.y as f64,
                ));
                self.move_pointer(pos, event.time_msec());
            }
            InputEvent::PointerButton { event } => self.on_button(event.button_code(), event.state(), event.time_msec()),
            InputEvent::PointerAxis { event } => self.on_axis::<WinitInput>(event),
            _ => {}
        }
    }

    fn on_keyboard(&mut self, code: smithay::input::keyboard::Keycode, key_state: KeyState, time: u32) {
        let keyboard = self.seat.get_keyboard().unwrap();
        let action = keyboard.input::<Action, _>(self, code, key_state, SERIAL_COUNTER.next_serial(), time, |_, mods, handle| {
            // Nested in another compositor Super is usually taken, so Alt is the modifier for now.
            if key_state == KeyState::Pressed && mods.alt {
                // Raw (unshifted) symbol, so Alt+Shift+1 is still "1".
                let sym = handle.raw_latin_sym_or_raw_current_sym().unwrap_or(handle.modified_sym()).raw();
                let action = match sym {
                    keysyms::KEY_Return => Some(Action::Spawn),
                    keysyms::KEY_q => Some(Action::Close),
                    keysyms::KEY_Tab => Some(Action::FocusNext),
                    keysyms::KEY_e if mods.shift => Some(Action::Quit),
                    keysyms::KEY_f => Some(Action::ToggleFloating),
                    keysyms::KEY_h => Some(Action::FocusDir(-1, 0)),
                    keysyms::KEY_l => Some(Action::FocusDir(1, 0)),
                    keysyms::KEY_k => Some(Action::FocusDir(0, -1)),
                    keysyms::KEY_j => Some(Action::FocusDir(0, 1)),
                    s => workspace_index(s)
                        .map(|n| if mods.shift { Action::MoveToWorkspace(n) } else { Action::Workspace(n) }),
                };
                if let Some(a) = action {
                    return FilterResult::Intercept(a);
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
            Action::Spawn => {
                let term = std::env::var("RUSTED_TERMINAL").unwrap_or_else(|_| "konsole".into());
                if let Err(e) = Command::new(&term).env("QT_QPA_PLATFORM", "wayland").spawn() {
                    tracing::error!("failed to spawn {term}: {e}");
                }
            }
            Action::Close => {
                if let Some(toplevel) = self.focused_window().and_then(|w| w.toplevel().cloned()) {
                    toplevel.send_close();
                }
            }
            Action::FocusNext => self.focus_next(),
            Action::FocusDir(dx, dy) => self.focus_dir(dx, dy),
            Action::ToggleFloating => self.toggle_floating(),
            Action::Workspace(n) => self.switch_workspace(n),
            Action::MoveToWorkspace(n) => self.move_to_workspace(n),
            Action::Quit => self.running = false,
        }
    }

    /// Surface under `pos` with its origin in global coordinates.
    fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, loc) = self.space.element_under(pos)?;
        let (surface, offset) = window.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)?;
        Some((surface, (loc + offset).to_f64()))
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
        pointer.motion(self, focus, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    pub fn on_button(&mut self, button: u32, state: ButtonState, time: u32) {
        let pointer = self.seat.get_pointer().unwrap();
        let alt = self.seat.get_keyboard().unwrap().modifier_state().alt;
        let pos = pointer.current_location();

        if state == ButtonState::Pressed && alt && (button == BTN_LEFT || button == BTN_RIGHT) {
            if let Some(window) = self.space.element_under(pos).map(|(w, _)| w.clone()) {
                let kind = if button == BTN_LEFT { DragKind::Move } else { DragKind::Resize };
                self.begin_drag(&window, kind, pos);
                return;
            }
        }
        if state == ButtonState::Released && self.drag.take().is_some() {
            return;
        }

        if state == ButtonState::Pressed {
            if let Some(window) = self.space.element_under(pos).map(|(w, _)| w.clone()) {
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
