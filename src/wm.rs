//! Window management: workspaces, tiling/floating, focus, borders, mouse drags.

use smithay::{
    backend::renderer::element::{
        solid::{SolidColorBuffer, SolidColorRenderElement},
        Kind,
    },
    desktop::{layer_map_for_output, space::RenderZindex, Window},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{Logical, Physical, Point, SERIAL_COUNTER},
};

use crate::{
    layout::{self, Rect},
    state::State,
};

pub const WORKSPACE_COUNT: usize = 9;
const MIN_FLOAT_SIZE: (i32, i32) = (120, 80);

const Z_TILED: u8 = RenderZindex::Shell as u8;
const Z_FLOATING: u8 = RenderZindex::Shell as u8 + 1;
/// Above the Top layer (bars) but below Overlay.
const Z_FULLSCREEN: u8 = RenderZindex::Top as u8 + 10;

pub struct Entry {
    pub window: Window,
    pub floating: bool,
    pub fullscreen: bool,
    pub float_rect: Rect,
    /// top, bottom, left, right
    borders: [SolidColorBuffer; 4],
}

impl Entry {
    fn new(window: Window, floating: bool, float_rect: Rect) -> Self {
        Self { window, floating, fullscreen: false, float_rect, borders: Default::default() }
    }
}

#[derive(Default)]
pub struct Workspace {
    pub entries: Vec<Entry>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DragKind {
    Move,
    Resize,
}

/// An in-progress Alt+mouse drag of a floating window.
pub struct Drag {
    pub window: Window,
    pub kind: DragKind,
    pub start_pointer: Point<f64, Logical>,
    pub start_rect: Rect,
}

impl State {
    /// Area available for windows: the output minus bars' exclusive zones (layer-shell).
    fn area(&self) -> Rect {
        let out = self.space.output_geometry(&self.output).unwrap_or_default();
        let zone = layer_map_for_output(&self.output).non_exclusive_zone();
        Rect::new(out.loc + zone.loc, zone.size)
    }

    pub fn ws(&self) -> &Workspace {
        &self.workspaces[self.current_ws]
    }

    fn entry_mut(&mut self, window: &Window) -> Option<&mut Entry> {
        self.workspaces[self.current_ws].entries.iter_mut().find(|e| &e.window == window)
    }

    /// Add a freshly created toplevel to the current workspace.
    pub fn add_window(&mut self, window: Window) {
        let is_dialog = window.toplevel().is_some_and(|t| t.parent().is_some());
        let float_rect = layout::default_float_rect(self.area());
        window.override_z_index(if is_dialog { Z_FLOATING } else { Z_TILED });
        self.space.map_element(window.clone(), self.area().loc, true);
        self.workspaces[self.current_ws].entries.push(Entry::new(window.clone(), is_dialog, float_rect));
        self.relayout();
        self.focus_window(&window);
    }

    /// Forget a window (destroyed by its client) wherever it lives.
    pub fn remove_window(&mut self, window: &Window) {
        for ws in &mut self.workspaces {
            ws.entries.retain(|e| &e.window != window);
        }
        self.space.unmap_elem(window);
        if self.drag.as_ref().is_some_and(|d| &d.window == window) {
            self.drag = None;
        }
        self.relayout();
        self.focus_top();
    }

    /// Apply the layout of the current workspace: tile, place floating windows, configure clients.
    pub fn relayout(&mut self) {
        let area = self.area();
        let gap = self.config.general.gap;
        let tiled_count = self.ws().entries.iter().filter(|e| !e.floating && !e.fullscreen).count();
        let mut tiles = layout::dwindle(area, tiled_count, gap, self.split_ratio).into_iter();
        let full = self.space.output_geometry(&self.output).unwrap_or_default();

        let mut targets: Vec<(Window, Rect)> = Vec::new();
        for entry in &self.workspaces[self.current_ws].entries {
            let rect = if entry.fullscreen {
                full
            } else if entry.floating {
                entry.float_rect
            } else {
                tiles.next().unwrap_or(area)
            };
            if let Some(toplevel) = entry.window.toplevel() {
                let tiled = !entry.floating && !entry.fullscreen;
                let fullscreen = entry.fullscreen;
                toplevel.with_pending_state(|s| {
                    s.size = Some(rect.size);
                    if fullscreen {
                        s.states.set(xdg_toplevel::State::Fullscreen);
                    } else {
                        s.states.unset(xdg_toplevel::State::Fullscreen);
                    }
                    for st in [
                        xdg_toplevel::State::TiledLeft,
                        xdg_toplevel::State::TiledRight,
                        xdg_toplevel::State::TiledTop,
                        xdg_toplevel::State::TiledBottom,
                    ] {
                        if tiled {
                            s.states.set(st);
                        } else {
                            s.states.unset(st);
                        }
                    }
                });
                toplevel.send_pending_configure();
            }
            targets.push((entry.window.clone(), rect));
        }

        // Re-map in current z-order: `map_element` raises, so walking back-to-front keeps the stack.
        let order: Vec<Window> = self.space.elements().cloned().collect();
        for window in order {
            if let Some((_, rect)) = targets.iter().find(|(w, _)| *w == window) {
                self.space.map_element(window, rect.loc, false);
            }
        }
    }

    pub fn focus_window(&mut self, window: &Window) {
        self.space.raise_element(window, true);
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_pending_configure();
            let keyboard = self.seat.get_keyboard().unwrap();
            keyboard.set_focus(self, Some(toplevel.wl_surface().clone()), SERIAL_COUNTER.next_serial());
        }
    }

    /// Give keyboard focus to an arbitrary surface (e.g. a layer-shell launcher).
    pub fn focus_surface(&mut self, surface: WlSurface) {
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
    }

    pub fn focused_window(&self) -> Option<Window> {
        let surface = self.seat.get_keyboard()?.current_focus()?;
        self.window_for_surface(&surface)
    }

    /// Focus the topmost window of the current workspace, or clear focus if it is empty.
    pub fn focus_top(&mut self) {
        let top = self.space.elements().last().cloned();
        if let Some(w) = top {
            self.focus_window(&w);
        } else {
            let keyboard = self.seat.get_keyboard().unwrap();
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
    }

    /// Cycle focus: the bottom window of the stack comes to the front.
    pub fn focus_next(&mut self) {
        let bottom = self.space.elements().next().cloned();
        if let Some(w) = bottom {
            self.focus_window(&w);
        }
    }

    /// Nearest window of the current workspace in direction (dx, dy), each in {-1, 0, 1}.
    fn neighbor(&self, current: &Window, dx: i32, dy: i32) -> Option<Window> {
        let cur = self.space.element_geometry(current)?;
        let c = Point::<i32, Logical>::from((cur.loc.x + cur.size.w / 2, cur.loc.y + cur.size.h / 2));
        self.space
            .elements()
            .filter(|w| *w != current)
            .filter_map(|w| {
                let g = self.space.element_geometry(w)?;
                let p = Point::<i32, Logical>::from((g.loc.x + g.size.w / 2, g.loc.y + g.size.h / 2));
                let (vx, vy) = (p.x - c.x, p.y - c.y);
                let along = vx * dx + vy * dy;
                if along <= 0 {
                    return None;
                }
                let across = (vx * dy).abs() + (vy * dx).abs();
                Some((along + 2 * across, w.clone()))
            })
            .min_by_key(|(score, _)| *score)
            .map(|(_, w)| w)
    }

    /// Focus the nearest window in direction (dx, dy).
    pub fn focus_dir(&mut self, dx: i32, dy: i32) {
        let Some(current) = self.focused_window() else { return };
        if let Some(w) = self.neighbor(&current, dx, dy) {
            self.focus_window(&w);
        }
    }

    /// Swap the focused tiled window with its neighbour in a direction; focus stays on it.
    pub fn swap_dir(&mut self, dx: i32, dy: i32) {
        let Some(current) = self.focused_window() else { return };
        let Some(other) = self.neighbor(&current, dx, dy) else { return };
        let entries = &mut self.workspaces[self.current_ws].entries;
        let (Some(a), Some(b)) = (
            entries.iter().position(|e| e.window == current),
            entries.iter().position(|e| e.window == other),
        ) else {
            return;
        };
        if entries[a].floating || entries[b].floating || entries[a].fullscreen || entries[b].fullscreen {
            return;
        }
        entries.swap(a, b);
        self.relayout();
    }

    /// Grow/shrink the first window's share of each split.
    pub fn adjust_ratio(&mut self, delta: f32) {
        self.split_ratio = (self.split_ratio + delta).clamp(layout::MIN_RATIO, layout::MAX_RATIO);
        self.relayout();
    }

    pub fn toggle_fullscreen(&mut self) {
        let Some(window) = self.focused_window() else { return };
        let now = self.ws().entries.iter().find(|e| e.window == window).is_some_and(|e| e.fullscreen);
        self.set_fullscreen(&window, !now);
    }

    /// Fullscreen on/off for a window on any workspace (also used for client requests).
    pub fn set_fullscreen(&mut self, window: &Window, on: bool) {
        let Some(entry) = self.workspaces.iter_mut().flat_map(|ws| ws.entries.iter_mut()).find(|e| &e.window == window)
        else {
            return;
        };
        entry.fullscreen = on;
        window.override_z_index(if on {
            Z_FULLSCREEN
        } else if entry.floating {
            Z_FLOATING
        } else {
            Z_TILED
        });
        self.relayout();
        if on {
            self.focus_window(window);
        }
    }

    pub fn toggle_floating(&mut self) {
        let Some(window) = self.focused_window() else { return };
        let area = self.area();
        let geo = self.space.element_geometry(&window);
        let Some(entry) = self.entry_mut(&window) else { return };
        if entry.fullscreen {
            return;
        }
        entry.floating = !entry.floating;
        if entry.floating {
            // Keep the window where it is but shrink it a bit so the change is visible.
            entry.float_rect = match geo {
                Some(g) if g.size.w < area.size.w * 8 / 10 => g,
                _ => layout::default_float_rect(area),
            };
        }
        window.override_z_index(if entry.floating { Z_FLOATING } else { Z_TILED });
        self.relayout();
        self.focus_window(&window);
    }

    pub fn switch_workspace(&mut self, n: usize) {
        if n >= WORKSPACE_COUNT || n == self.current_ws {
            return;
        }
        if self.drag.is_some() {
            self.drag = None;
        }
        for e in &self.workspaces[self.current_ws].entries {
            self.space.unmap_elem(&e.window);
            e.window.set_activated(false);
            if let Some(t) = e.window.toplevel() {
                t.send_pending_configure();
            }
        }
        self.current_ws = n;
        let loc = self.area().loc;
        let windows: Vec<Window> = self.ws().entries.iter().map(|e| e.window.clone()).collect();
        for w in windows {
            self.space.map_element(w, loc, false);
        }
        self.relayout();
        self.focus_top();
        tracing::info!(workspace = n + 1, "switched workspace");
    }

    pub fn move_to_workspace(&mut self, n: usize) {
        if n >= WORKSPACE_COUNT || n == self.current_ws {
            return;
        }
        let Some(window) = self.focused_window() else { return };
        let cur = self.current_ws;
        let Some(pos) = self.workspaces[cur].entries.iter().position(|e| e.window == window) else { return };
        let entry = self.workspaces[cur].entries.remove(pos);
        self.workspaces[n].entries.push(entry);
        self.space.unmap_elem(&window);
        window.set_activated(false);
        if let Some(t) = window.toplevel() {
            t.send_pending_configure();
        }
        self.relayout();
        self.focus_top();
    }

    // ---- mouse drags (floating windows) ----

    /// Start moving/resizing the window under the pointer. Tiled windows become floating first.
    pub fn begin_drag(&mut self, window: &Window, kind: DragKind, pointer: Point<f64, Logical>) {
        let Some(geo) = self.space.element_geometry(window) else { return };
        let Some(entry) = self.entry_mut(window) else { return };
        if !entry.floating {
            entry.floating = true;
            entry.float_rect = geo;
            window.override_z_index(Z_FLOATING);
        }
        let start_rect = entry.float_rect;
        self.focus_window(window);
        self.relayout();
        self.drag = Some(Drag { window: window.clone(), kind, start_pointer: pointer, start_rect });
    }

    pub fn update_drag(&mut self, pointer: Point<f64, Logical>) {
        let Some(drag) = &self.drag else { return };
        let (window, kind, start_rect) = (drag.window.clone(), drag.kind, drag.start_rect);
        let d = pointer - drag.start_pointer;
        let (dx, dy) = (d.x.round() as i32, d.y.round() as i32);
        let new = match kind {
            DragKind::Move => Rect::new((start_rect.loc.x + dx, start_rect.loc.y + dy).into(), start_rect.size),
            DragKind::Resize => Rect::new(
                start_rect.loc,
                ((start_rect.size.w + dx).max(MIN_FLOAT_SIZE.0), (start_rect.size.h + dy).max(MIN_FLOAT_SIZE.1)).into(),
            ),
        };
        if let Some(entry) = self.entry_mut(&window) {
            entry.float_rect = new;
        }
        self.relayout();
    }

    // ---- borders ----

    /// Border strips drawn just outside every window of the current workspace.
    pub fn border_elements(&mut self) -> Vec<SolidColorRenderElement> {
        // Borders are drawn above all windows, so hide them while something is fullscreen.
        if self.ws().entries.iter().any(|e| e.fullscreen) {
            return Vec::new();
        }
        let scale = self.config.general.scale;
        let focused = self.focused_window();
        let mut out = Vec::new();
        let b = self.config.general.border;
        let (focused_color, unfocused_color) = (self.config.focused, self.config.unfocused);
        for i in 0..self.workspaces[self.current_ws].entries.len() {
            if self.workspaces[self.current_ws].entries[i].fullscreen {
                continue;
            }
            let window = self.workspaces[self.current_ws].entries[i].window.clone();
            let Some(g) = self.space.element_geometry(&window) else { continue };
            let color = if focused.as_ref() == Some(&window) { focused_color } else { unfocused_color };
            // (x, y, w, h) of top, bottom, left, right
            let strips = [
                (g.loc.x - b, g.loc.y - b, g.size.w + 2 * b, b),
                (g.loc.x - b, g.loc.y + g.size.h, g.size.w + 2 * b, b),
                (g.loc.x - b, g.loc.y, b, g.size.h),
                (g.loc.x + g.size.w, g.loc.y, b, g.size.h),
            ];
            let entry = &mut self.workspaces[self.current_ws].entries[i];
            for (buf, (x, y, w, h)) in entry.borders.iter_mut().zip(strips) {
                buf.update((w, h), color);
                let at: Point<i32, Physical> = Point::<f64, Physical>::from((x as f64 * scale, y as f64 * scale)).to_i32_round();
                out.push(SolidColorRenderElement::from_buffer(buf, at, scale, 1.0, Kind::Unspecified));
            }
        }
        out
    }
}

// ---- config / processes ----

impl State {
    /// Re-read the config file and apply it to the running compositor.
    pub fn reload_config(&mut self) -> Vec<String> {
        let (config, problems) = crate::config::Config::load(self.session.is_some());
        for p in &problems {
            tracing::warn!("config: {p}");
        }
        self.split_ratio = config.general.split_ratio;
        self.config = config;
        self.apply_scale();
        self.relayout();
        tracing::info!("config reloaded from {}", crate::config::Config::path().display());
        problems
    }

    /// Run a shell command detached from the compositor (reaped in the background).
    pub fn spawn(&self, command: &str) {
        match std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("exec {command}"))
            .env("QT_QPA_PLATFORM", "wayland")
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => tracing::error!("failed to spawn {command:?}: {e}"),
        }
    }
}

// ---- frame callbacks ----

impl State {
    /// Tell clients a frame was presented (drives their next redraw).
    pub fn send_frames(&mut self, output: &smithay::output::Output, time: std::time::Duration) {
        for window in self.space.elements() {
            window.send_frame(output, time, Some(std::time::Duration::ZERO), |_, _| Some(output.clone()));
        }
        for layer in layer_map_for_output(output).layers() {
            layer.send_frame(output, time, Some(std::time::Duration::ZERO), |_, _| Some(output.clone()));
        }
        self.space.refresh();
        self.popups.cleanup();
        layer_map_for_output(output).cleanup();
    }
}
