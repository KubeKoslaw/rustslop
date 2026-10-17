//! Window management: workspaces, tiling/floating, focus, borders, mouse drags.

use smithay::wayland::seat::WaylandFocus as _;
use smithay::{
    backend::renderer::element::{
        solid::{SolidColorBuffer, SolidColorRenderElement},
        Kind,
    },
    desktop::{layer_map_for_output, space::RenderZindex, Window},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    utils::{Logical, Physical, Point, Rectangle, Transform, SERIAL_COUNTER},
};

use crate::{
    layout::{self, Rect},
    state::State,
};

pub const WORKSPACE_COUNT: usize = 9;
const MIN_FLOAT_SIZE: (i32, i32) = (120, 80);

pub(crate) const Z_TILED: u8 = RenderZindex::Shell as u8;
pub(crate) const Z_FLOATING: u8 = RenderZindex::Shell as u8 + 1;
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

/// Dialogs, splash screens etc. start floating.
fn window_is_dialog(window: &Window) -> bool {
    use smithay::xwayland::xwm::WmWindowType as T;
    if let Some(t) = window.toplevel() {
        return t.parent().is_some();
    }
    window.x11_surface().is_some_and(|x| {
        x.is_transient_for().is_some()
            || x.window_type().is_some_and(|t| matches!(t, T::Dialog | T::Splash | T::Utility | T::Toolbar))
    })
}

/// Ask a window to close itself (Wayland `xdg_toplevel.close` / X11 `WM_DELETE_WINDOW`).
pub fn close_window(window: &Window) {
    if let Some(t) = window.toplevel() {
        t.send_close();
    } else if let Some(x) = window.x11_surface() {
        let _ = x.close();
    }
}

/// One monitor and the workspace it currently shows (workspaces 1-9 are global; each is visible
/// on at most one monitor).
pub struct Mon {
    pub output: Output,
    pub ws: usize,
}

impl State {
    pub fn current_ws(&self) -> usize {
        self.outputs[self.focused].ws
    }

    /// The output that has keyboard/"current" focus.
    pub fn focused_output(&self) -> &Output {
        &self.outputs[self.focused].output
    }

    pub fn output_index(&self, output: &Output) -> Option<usize> {
        self.outputs.iter().position(|m| &m.output == output)
    }

    pub fn output_loc(&self, mon: usize) -> Point<i32, Logical> {
        self.space.output_geometry(&self.outputs[mon].output).unwrap_or_default().loc
    }

    /// Index of the monitor containing `pos`.
    pub fn mon_at(&self, pos: Point<f64, Logical>) -> Option<usize> {
        self.outputs.iter().position(|m| {
            self.space.output_geometry(&m.output).is_some_and(|g| g.to_f64().contains(pos))
        })
    }

    /// Place outputs left to right (logical widths) and map them into the space.
    pub fn layout_outputs(&mut self) {
        let mut x = 0;
        for m in &self.outputs {
            let size: smithay::utils::Size<i32, Logical> = m
                .output
                .current_mode()
                .map(|mode| mode.size.to_f64().to_logical(m.output.current_scale().fractional_scale()).to_i32_round())
                .unwrap_or_default();
            m.output.change_current_state(None, None, None, Some((x, 0).into()));
            self.space.map_output(&m.output, (x, 0));
            x += size.w;
        }
    }

    /// Area available for windows on monitor `mon`: the output minus bars' exclusive zones.
    fn area_of(&self, mon: usize) -> Rect {
        let output = &self.outputs[mon].output;
        let out = self.space.output_geometry(output).unwrap_or_default();
        let zone = layer_map_for_output(output).non_exclusive_zone();
        Rect::new(out.loc + zone.loc, zone.size)
    }

    pub fn ws(&self) -> &Workspace {
        &self.workspaces[self.current_ws()]
    }

    /// Monitor showing workspace `ws`, if any.
    fn mon_of_ws(&self, ws: usize) -> Option<usize> {
        self.outputs.iter().position(|m| m.ws == ws)
    }

    fn ws_of_window(&self, window: &Window) -> Option<usize> {
        self.workspaces.iter().position(|ws| ws.entries.iter().any(|e| &e.window == window))
    }

    /// Monitor on which `window` is currently shown.
    pub fn mon_of_window(&self, window: &Window) -> Option<usize> {
        self.mon_of_ws(self.ws_of_window(window)?)
    }

    fn entry_mut(&mut self, window: &Window) -> Option<&mut Entry> {
        self.workspaces.iter_mut().flat_map(|ws| ws.entries.iter_mut()).find(|e| &e.window == window)
    }

    /// Add a freshly created toplevel to the focused monitor's workspace.
    pub fn add_window(&mut self, window: Window) {
        let is_dialog = window_is_dialog(&window);
        let area = self.area_of(self.focused);
        let default = layout::default_float_rect(area);
        let float_rect = Rect::new(default.loc - self.output_loc(self.focused), default.size);
        window.override_z_index(if is_dialog { Z_FLOATING } else { Z_TILED });
        self.space.map_element(window.clone(), area.loc, true);
        let ws = self.current_ws();
        self.workspaces[ws].entries.push(Entry::new(window.clone(), is_dialog, float_rect));
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

    /// Make the space match what the monitors show: windows of visible workspaces are mapped,
    /// all others unmapped and deactivated.
    fn sync_space(&mut self) {
        let mut mapped: Vec<(Window, Point<i32, Logical>)> = Vec::new();
        for (i, m) in self.outputs.iter().enumerate() {
            let loc = self.output_loc(i);
            mapped.extend(self.workspaces[m.ws].entries.iter().map(|e| (e.window.clone(), loc)));
        }
        for (wi, ws) in self.workspaces.iter().enumerate() {
            if self.outputs.iter().any(|m| m.ws == wi) {
                continue;
            }
            for e in &ws.entries {
                if self.space.elements().any(|w| w == &e.window) {
                    self.space.unmap_elem(&e.window);
                    e.window.set_activated(false);
                    if let Some(t) = e.window.toplevel() {
                        t.send_pending_configure();
                    }
                }
            }
        }
        for (w, loc) in mapped {
            if !self.space.elements().any(|x| x == &w) {
                self.space.map_element(w, loc, false);
            }
        }
    }

    /// Apply the layout of every monitor's workspace: tile, place floating windows, configure clients.
    pub fn relayout(&mut self) {
        let gap = self.config.general.gap;
        let mut targets: Vec<(Window, Rect)> = Vec::new();
        for mon in 0..self.outputs.len() {
            let area = self.area_of(mon);
            let loc = self.output_loc(mon);
            let full = self.space.output_geometry(&self.outputs[mon].output).unwrap_or_default();
            let ws = self.outputs[mon].ws;
            let tiled_count = self.workspaces[ws].entries.iter().filter(|e| !e.floating && !e.fullscreen).count();
            let mut tiles = layout::dwindle(area, tiled_count, gap, self.split_ratio).into_iter();
            for entry in &self.workspaces[ws].entries {
                let rect = if entry.fullscreen {
                    full
                } else if entry.floating {
                    Rect::new(entry.float_rect.loc + loc, entry.float_rect.size)
                } else {
                    tiles.next().unwrap_or(area)
                };
                if let Some(x11) = entry.window.x11_surface() {
                    let _ = x11.set_fullscreen(entry.fullscreen);
                    let _ = x11.configure(Some(Rectangle::new(rect.loc, rect.size)));
                }
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
        if let Some(mon) = self.mon_of_window(window) {
            self.focused = mon;
        }
        self.space.raise_element(window, true);
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_pending_configure();
        }
        // X11 windows only have a wl_surface once Xwayland has associated one (see
        // `surface_associated`, which focuses them then).
        if let Some(surface) = window.wl_surface().map(|s| s.into_owned()) {
            let keyboard = self.seat.get_keyboard().unwrap();
            keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
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

    /// Windows of the focused monitor's workspace, bottom to top.
    fn focused_stack(&self) -> Vec<Window> {
        let ws = self.ws();
        self.space.elements().filter(|w| ws.entries.iter().any(|e| &e.window == *w)).cloned().collect()
    }

    pub fn focused_stack_top(&self) -> Option<Window> {
        self.focused_stack().pop()
    }

    /// Focus the topmost window of the focused monitor, or clear focus if it is empty.
    pub fn focus_top(&mut self) {
        let top = self.focused_stack().pop();
        if let Some(w) = top {
            self.focus_window(&w);
        } else {
            let keyboard = self.seat.get_keyboard().unwrap();
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
    }

    /// Cycle focus: the bottom window of the stack comes to the front.
    pub fn focus_next(&mut self) {
        let bottom = self.focused_stack().into_iter().next();
        if let Some(w) = bottom {
            self.focus_window(&w);
        }
    }

    /// Nearest window of the same workspace in direction (dx, dy), each in {-1, 0, 1}.
    fn neighbor(&self, current: &Window, dx: i32, dy: i32) -> Option<Window> {
        let cur = self.space.element_geometry(current)?;
        let c = Point::<i32, Logical>::from((cur.loc.x + cur.size.w / 2, cur.loc.y + cur.size.h / 2));
        let ws = &self.workspaces[self.ws_of_window(current)?];
        self.space
            .elements()
            .filter(|w| *w != current && ws.entries.iter().any(|e| &e.window == *w))
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

    /// Nearest monitor from the focused one in direction (dx, dy).
    fn output_in_dir(&self, dx: i32, dy: i32) -> Option<usize> {
        let center = |i: usize| {
            let g = self.space.output_geometry(&self.outputs[i].output).unwrap_or_default();
            (g.loc.x + g.size.w / 2, g.loc.y + g.size.h / 2)
        };
        let (cx, cy) = center(self.focused);
        (0..self.outputs.len())
            .filter(|&i| i != self.focused)
            .filter_map(|i| {
                let (x, y) = center(i);
                let (vx, vy) = (x - cx, y - cy);
                let along = vx * dx + vy * dy;
                (along > 0).then(|| (along + 2 * ((vx * dy).abs() + (vy * dx).abs()), i))
            })
            .min_by_key(|(score, _)| *score)
            .map(|(_, i)| i)
    }

    /// Make monitor `mon` the focused one: focus its top window and warp the pointer there.
    pub fn focus_output(&mut self, mon: usize) {
        if mon >= self.outputs.len() {
            return;
        }
        self.focused = mon;
        let geo = self.space.output_geometry(&self.outputs[mon].output).unwrap_or_default();
        let center = Point::<f64, Logical>::from((
            (geo.loc.x + geo.size.w / 2) as f64,
            (geo.loc.y + geo.size.h / 2) as f64,
        ));
        self.focus_top();
        self.move_pointer(center, 0);
    }

    /// Focus the nearest window in direction (dx, dy); at the edge of a monitor, go to the
    /// neighbouring monitor in that direction.
    pub fn focus_dir(&mut self, dx: i32, dy: i32) {
        if let Some(current) = self.focused_window() {
            if let Some(w) = self.neighbor(&current, dx, dy) {
                self.focus_window(&w);
                return;
            }
        }
        if let Some(mon) = self.output_in_dir(dx, dy) {
            self.focus_output(mon);
        }
    }

    /// Swap the focused tiled window with its neighbour in a direction; focus stays on it.
    pub fn swap_dir(&mut self, dx: i32, dy: i32) {
        let Some(current) = self.focused_window() else { return };
        let Some(other) = self.neighbor(&current, dx, dy) else { return };
        let ws = self.current_ws();
        let entries = &mut self.workspaces[ws].entries;
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
        let Some(mon) = self.mon_of_window(&window) else { return };
        let area = self.area_of(mon);
        let loc = self.output_loc(mon);
        let geo = self.space.element_geometry(&window);
        let Some(entry) = self.entry_mut(&window) else { return };
        if entry.fullscreen {
            return;
        }
        entry.floating = !entry.floating;
        if entry.floating {
            // Keep the window where it is but shrink it a bit so the change is visible.
            let rect = match geo {
                Some(g) if g.size.w < area.size.w * 8 / 10 => g,
                _ => layout::default_float_rect(area),
            };
            entry.float_rect = Rect::new(rect.loc - loc, rect.size);
        }
        window.override_z_index(if entry.floating { Z_FLOATING } else { Z_TILED });
        self.relayout();
        self.focus_window(&window);
    }

    /// Show workspace `n` on the focused monitor; if another monitor already shows it, go there.
    pub fn switch_workspace(&mut self, n: usize) {
        if n >= WORKSPACE_COUNT || n == self.current_ws() {
            return;
        }
        self.drag = None;
        if let Some(mon) = self.mon_of_ws(n) {
            self.focus_output(mon);
            return;
        }
        let focused = self.focused;
        self.outputs[focused].ws = n;
        self.sync_space();
        self.relayout();
        self.focus_top();
        tracing::info!(workspace = n + 1, "switched workspace");
    }

    pub fn move_to_workspace(&mut self, n: usize) {
        let cur = self.current_ws();
        if n >= WORKSPACE_COUNT || n == cur {
            return;
        }
        let Some(window) = self.focused_window() else { return };
        let Some(pos) = self.workspaces[cur].entries.iter().position(|e| e.window == window) else { return };
        let entry = self.workspaces[cur].entries.remove(pos);
        self.workspaces[n].entries.push(entry);
        self.sync_space();
        self.relayout();
        self.focus_top();
    }

    /// Send the focused window to the workspace shown on the neighbouring monitor.
    pub fn move_to_output(&mut self, dx: i32, dy: i32) {
        let Some(mon) = self.output_in_dir(dx, dy) else { return };
        let ws = self.outputs[mon].ws;
        self.move_to_workspace(ws);
    }

    // ---- virtual outputs (control socket / tests) ----

    /// Add an output without a backend (nothing is drawn on it, but it takes part in layout,
    /// focus and screenshots). Used to exercise multi-monitor logic without hardware.
    pub fn add_virtual_output(&mut self, w: i32, h: i32) {
        let name = format!("virtual-{}", self.outputs.len() + 1);
        let output = Output::new(
            name.clone(),
            PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "rusted".into(), model: name },
        );
        let _global = output.create_global::<State>(&self.display_handle);
        let mode = OutputMode { size: (w, h).into(), refresh: 60_000 };
        output.change_current_state(Some(mode), Some(Transform::Normal), None, None);
        output.set_preferred(mode);
        let ws = (0..WORKSPACE_COUNT).find(|&i| self.mon_of_ws(i).is_none()).unwrap_or(0);
        self.outputs.push(Mon { output, ws });
        self.apply_scale();
        self.sync_space();
        self.relayout();
    }

    pub fn remove_last_output(&mut self) {
        if self.outputs.len() < 2 {
            return;
        }
        let mon = self.outputs.pop().unwrap();
        self.space.unmap_output(&mon.output);
        if self.focused >= self.outputs.len() {
            self.focused = 0;
        }
        self.sync_space();
        self.relayout();
        self.focus_top();
    }

    // ---- mouse drags (floating windows) ----

    /// Start moving/resizing the window under the pointer. Tiled windows become floating first.
    pub fn begin_drag(&mut self, window: &Window, kind: DragKind, pointer: Point<f64, Logical>) {
        let Some(geo) = self.space.element_geometry(window) else { return };
        let loc = self.mon_of_window(window).map(|m| self.output_loc(m)).unwrap_or_default();
        let Some(entry) = self.entry_mut(window) else { return };
        if !entry.floating {
            entry.floating = true;
            entry.float_rect = Rect::new(geo.loc - loc, geo.size);
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

    /// Border strips drawn just outside every window of `output`'s workspace, in output-local
    /// physical coordinates.
    pub fn border_elements(&mut self, output: &Output) -> Vec<SolidColorRenderElement> {
        let Some(mon) = self.output_index(output) else { return Vec::new() };
        let ws = self.outputs[mon].ws;
        // Borders are drawn above all windows, so hide them while something is fullscreen.
        if self.workspaces[ws].entries.iter().any(|e| e.fullscreen) {
            return Vec::new();
        }
        let origin = self.output_loc(mon);
        let scale = self.config.general.scale;
        let focused = self.focused_window();
        let mut out = Vec::new();
        let b = self.config.general.border;
        let (focused_color, unfocused_color) = (self.config.focused, self.config.unfocused);
        for i in 0..self.workspaces[ws].entries.len() {
            let window = self.workspaces[ws].entries[i].window.clone();
            let Some(g) = self.space.element_geometry(&window) else { continue };
            let g = Rect::new(g.loc - origin, g.size);
            let color = if focused.as_ref() == Some(&window) { focused_color } else { unfocused_color };
            // (x, y, w, h) of top, bottom, left, right
            let strips = [
                (g.loc.x - b, g.loc.y - b, g.size.w + 2 * b, b),
                (g.loc.x - b, g.loc.y + g.size.h, g.size.w + 2 * b, b),
                (g.loc.x - b, g.loc.y, b, g.size.h),
                (g.loc.x + g.size.w, g.loc.y, b, g.size.h),
            ];
            let entry = &mut self.workspaces[ws].entries[i];
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
            if self.space.outputs_for_element(window).contains(output) {
                window.send_frame(output, time, Some(std::time::Duration::ZERO), |_, _| Some(output.clone()));
            }
        }
        for layer in layer_map_for_output(output).layers() {
            layer.send_frame(output, time, Some(std::time::Duration::ZERO), |_, _| Some(output.clone()));
        }
        self.space.refresh();
        self.popups.cleanup();
        layer_map_for_output(output).cleanup();
    }
}
