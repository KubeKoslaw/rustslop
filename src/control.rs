//! Debug/automation control socket.
//!
//! Lets scripts (and AI agents) drive the compositor and inspect it without real input devices.
//! Socket: `$XDG_RUNTIME_DIR/<wayland-socket-name>-ctl`. One command per connection, text reply.
//! Use `tools/ctl.py`. Commands are documented in `PROGRESS.md`.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    time::Duration,
};

use smithay::{
    backend::input::ButtonState,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{Logical, Point},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{input::Action, state::State, wm::DragKind};

pub struct Control {
    listener: UnixListener,
    path: PathBuf,
}

/// What the main loop must do after a command ran.
pub enum Reply {
    Text(String),
    /// Take a screenshot of output N into this path, then answer "ok".
    Screenshot(PathBuf, usize),
}

impl Control {
    pub fn bind(socket_name: &str) -> std::io::Result<Self> {
        let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        let path = PathBuf::from(dir).join(format!("{socket_name}-ctl"));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, path })
    }

    pub fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.listener)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Next pending command, if any, with the stream to answer on.
    pub fn poll(&self) -> Option<(String, UnixStream)> {
        let (stream, _) = self.listener.accept().ok()?;
        stream.set_nonblocking(false).ok()?;
        stream.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).ok()?;
        Some((line.trim().to_string(), stream))
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn answer(mut stream: UnixStream, text: &str) {
    let _ = stream.write_all(text.as_bytes());
    let _ = stream.write_all(b"\n");
}

fn now_ms() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(0)
}

impl State {
    pub fn run_command(&mut self, line: &str) -> Reply {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| parts.get(i).and_then(|s| s.parse::<f64>().ok());
        let err = |m: &str| Reply::Text(format!("error: {m}"));
        let ok = || Reply::Text("ok".into());

        match parts.first().copied() {
            Some("spawn") => {
                let term = self.config.general.terminal.clone();
                self.run_action(Action::Exec(term))
            }
            Some("reload") => {
                let problems = self.reload_config();
                if !problems.is_empty() {
                    return Reply::Text(format!("reloaded with problems:\n{}", problems.join("\n")));
                }
            }
            Some("exec") if parts.len() > 1 => self.spawn(&parts[1..].join(" ")),
            Some("volume") => {
                use crate::audio::AudioCmd::*;
                match parts.get(1).copied() {
                    Some("up") => self.run_action(Action::Audio(VolumeUp)),
                    Some("down") => self.run_action(Action::Audio(VolumeDown)),
                    Some("mute") => self.run_action(Action::Audio(VolumeMute)),
                    Some("mic-mute") => self.run_action(Action::Audio(MicMute)),
                    _ => return err("volume up|down|mute|mic-mute"),
                }
            }
            Some("close") => self.run_action(Action::Close),
            Some("next") => self.run_action(Action::FocusNext),
            Some("float") => self.run_action(Action::ToggleFloating),
            Some("fullscreen") => self.run_action(Action::ToggleFullscreen),
            Some("grow") => self.run_action(Action::Resize(0.05)),
            Some("shrink") => self.run_action(Action::Resize(-0.05)),
            Some("swap") => match parts.get(1).copied() {
                Some("h") => self.run_action(Action::SwapDir(-1, 0)),
                Some("l") => self.run_action(Action::SwapDir(1, 0)),
                Some("k") => self.run_action(Action::SwapDir(0, -1)),
                Some("j") => self.run_action(Action::SwapDir(0, 1)),
                _ => return err("swap h|j|k|l"),
            },
            Some("quit") => self.run_action(Action::Quit),
            Some("focus") => match parts.get(1).copied() {
                Some("h") => self.run_action(Action::FocusDir(-1, 0)),
                Some("l") => self.run_action(Action::FocusDir(1, 0)),
                Some("k") => self.run_action(Action::FocusDir(0, -1)),
                Some("j") => self.run_action(Action::FocusDir(0, 1)),
                _ => return err("focus h|j|k|l"),
            },
            Some("ws") if matches!(parts.get(1).copied(), Some("next" | "prev")) => {
                let n = crate::wm::WORKSPACE_COUNT;
                let cur = self.current_ws();
                let target = if parts[1] == "next" { (cur + 1) % n } else { (cur + n - 1) % n };
                self.run_action(Action::Workspace(target));
            }
            Some("ws-json") => return Reply::Text(self.workspaces_json(parts.get(1).copied())),
            Some("ws") => match num(1) {
                Some(n) if n >= 1.0 => self.run_action(Action::Workspace(n as usize - 1)),
                _ => return err("ws N (1-9)"),
            },
            Some("mv") => match num(1) {
                Some(n) if n >= 1.0 => self.run_action(Action::MoveToWorkspace(n as usize - 1)),
                _ => return err("mv N (1-9)"),
            },
            Some("motion") => match (num(1), num(2)) {
                (Some(x), Some(y)) => self.move_pointer(Point::<f64, Logical>::from((x, y)), now_ms()),
                _ => return err("motion X Y"),
            },
            Some("click") => match (num(1), num(2)) {
                (Some(x), Some(y)) => {
                    let button = if parts.get(3) == Some(&"right") { 0x111 } else { 0x110 };
                    self.move_pointer(Point::<f64, Logical>::from((x, y)), now_ms());
                    self.on_button(button, ButtonState::Pressed, now_ms());
                    self.on_button(button, ButtonState::Released, now_ms());
                }
                _ => return err("click X Y [left|right]"),
            },
            Some("drag") => match (parts.get(1).copied(), num(2), num(3), num(4), num(5)) {
                (Some(kind @ ("move" | "resize")), Some(x1), Some(y1), Some(x2), Some(y2)) => {
                    let (p1, p2) = (Point::<f64, Logical>::from((x1, y1)), Point::<f64, Logical>::from((x2, y2)));
                    self.move_pointer(p1, now_ms());
                    let Some(window) = self.space.element_under(p1).map(|(w, _)| w.clone()) else {
                        return err("no window under start point");
                    };
                    let kind = if kind == "move" { DragKind::Move } else { DragKind::Resize };
                    self.begin_drag(&window, kind, p1);
                    self.move_pointer(p2, now_ms());
                    self.drag = None;
                }
                _ => return err("drag move|resize X1 Y1 X2 Y2"),
            },
            Some("shot") if parts.len() == 2 || parts.len() == 3 => {
                let mon = parts.get(2).and_then(|n| n.parse::<usize>().ok()).unwrap_or(1).max(1) - 1;
                return Reply::Screenshot(PathBuf::from(parts[1]), mon.min(self.outputs.len() - 1));
            }
            Some("output") => match (parts.get(1).copied(), num(2), num(3)) {
                (Some("add"), Some(w), Some(h)) if w >= 1.0 && h >= 1.0 => self.add_virtual_output(w as i32, h as i32),
                (Some("rm"), _, _) => self.remove_last_output(),
                (Some("focus"), Some(n), _) if n >= 1.0 => self.focus_output(n as usize - 1),
                _ => return err("output add W H | output rm | output focus N"),
            },
            Some("mvout") => match parts.get(1).copied() {
                Some("h") => self.run_action(Action::MoveToOutput(-1, 0)),
                Some("l") => self.run_action(Action::MoveToOutput(1, 0)),
                Some("k") => self.run_action(Action::MoveToOutput(0, -1)),
                Some("j") => self.run_action(Action::MoveToOutput(0, 1)),
                _ => return err("mvout h|j|k|l"),
            },
            Some("info") => return Reply::Text(self.describe()),
            _ => return err("unknown command (spawn, exec CMD.., reload, close, next, float, fullscreen, grow, shrink, swap h|j|k|l, quit, focus hjkl, volume up|down|mute|mic-mute, ws N|next|prev, ws-json [OUTPUT], mv N, motion X Y, click X Y [right], drag move|resize X1 Y1 X2 Y2, shot PATH [N], output add W H|rm|focus N, mvout h|j|k|l, info)"),
        }
        ok()
    }

    /// Waybar `custom` module payload (`return-type: json`): occupied and visible workspaces with
    /// Pango markup. With an output name, "current" means the workspace shown on that output.
    fn workspaces_json(&self, output: Option<&str>) -> String {
        let mon = output
            .and_then(|name| self.outputs.iter().position(|m| m.output.name() == name))
            .unwrap_or(self.focused);
        let shown = self.outputs[mon].ws;
        let mut text = Vec::new();
        for (i, ws) in self.workspaces.iter().enumerate() {
            let visible_elsewhere = self.outputs.iter().enumerate().any(|(m, o)| m != mon && o.ws == i);
            let label = if i == shown {
                format!("<span foreground='#8aadf4'><b> {} </b></span>", i + 1)
            } else if visible_elsewhere {
                format!("<u> {} </u>", i + 1)
            } else if !ws.entries.is_empty() {
                format!(" {} ", i + 1)
            } else {
                continue;
            };
            text.push(label);
        }
        let windows: usize = self.workspaces[shown].entries.len();
        format!(
            "{{\"text\": \"{}\", \"tooltip\": \"workspace {} ({} window(s))\", \"class\": \"ws-{}\"}}",
            text.join("").replace('\'', "\\u0027"),
            shown + 1,
            windows,
            shown + 1
        )
    }

    /// Human/AI readable dump of the whole window state.
    fn describe(&self) -> String {
        let focused = self.focused_window();
        let mut out = String::new();
        out += &format!("locked={}\n", self.locked);
        for (n, m) in self.outputs.iter().enumerate() {
            let o = self.space.output_geometry(&m.output).unwrap_or_default();
            out += &format!(
                "output {}x{} at {},{} name={} current_ws={} focused={}\n",
                o.size.w, o.size.h, o.loc.x, o.loc.y, m.output.name(), m.ws + 1, n == self.focused
            );
        }
        for (i, ws) in self.workspaces.iter().enumerate() {
            let visible = self.outputs.iter().any(|m| m.ws == i);
            if ws.entries.is_empty() && !visible {
                continue;
            }
            out += &format!("ws {}{}: {} window(s)\n", i + 1, if visible { " (visible)" } else { "" }, ws.entries.len());
            for e in &ws.entries {
                let (title, app_id) = if let Some(x) = e.window.x11_surface() {
                    (x.title(), x.class())
                } else {
                    e.window
                        .toplevel()
                        .map(|t| {
                            with_states(t.wl_surface(), |s| {
                                let d = s.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap();
                                (d.title.clone().unwrap_or_default(), d.app_id.clone().unwrap_or_default())
                            })
                        })
                        .unwrap_or_default()
                };
                let activated = match e.window.x11_surface() {
                    Some(x) => x.is_activated(),
                    None => e.window.toplevel().is_some_and(|t| {
                        t.current_state().states.contains(xdg_toplevel::State::Activated)
                    }),
                };
                let geo = if visible { self.space.element_geometry(&e.window) } else { None };
                out += &format!(
                    "  - app_id={app_id:?} title={title:?} floating={} fullscreen={} focused={} activated={} geo={}\n",
                    e.floating,
                    e.fullscreen,
                    focused.as_ref() == Some(&e.window),
                    activated,
                    geo.map(|g| format!("{},{} {}x{}", g.loc.x, g.loc.y, g.size.w, g.size.h)).unwrap_or_else(|| "unmapped".into()),
                );
            }
        }
        out.trim_end().to_string()
    }
}
