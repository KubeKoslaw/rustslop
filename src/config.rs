//! TOML configuration: appearance, terminal, autostart and keybinds.
//!
//! Loaded from `$RUSTED_CONFIG`, else `$XDG_CONFIG_HOME/rusted-wm/config.toml`
//! (`~/.config/rusted-wm/config.toml`). A missing file means defaults; a broken file is reported
//! and defaults are used. See `config.example.toml` for every option.

use std::{collections::BTreeMap, path::PathBuf};

use serde::Deserialize;
use smithay::{
    backend::renderer::Color32F,
    input::keyboard::{xkb, ModifiersState},
};

use crate::input::Action;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct General {
    pub gap: i32,
    pub border: i32,
    pub terminal: String,
    /// Share of a split taken by the first window (0.1 - 0.9).
    pub split_ratio: f32,
    /// "alt", "super", or "auto" (default): Super on a real session, Alt when nested in another
    /// desktop (which usually keeps Super for itself).
    pub modifier: String,
}

impl Default for General {
    fn default() -> Self {
        Self { gap: 12, border: 2, terminal: "konsole".into(), split_ratio: 0.5, modifier: "auto".into() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Colors {
    pub focused: String,
    pub unfocused: String,
    pub background: String,
}

impl Default for Colors {
    fn default() -> Self {
        Self { focused: "#73a6ff".into(), unfocused: "#40455a".into(), background: "#14141f".into() }
    }
}

/// Keyboard layout and repeat. Needed on a real TTY, where there is no host compositor to ask.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Input {
    pub layout: String,
    pub variant: String,
    pub options: String,
    pub repeat_delay: i32,
    pub repeat_rate: i32,
    pub tap_to_click: bool,
    pub natural_scroll: bool,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            layout: String::new(),
            variant: String::new(),
            options: String::new(),
            repeat_delay: 200,
            repeat_rate: 25,
            tap_to_click: true,
            natural_scroll: false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    general: General,
    input: Input,
    colors: Colors,
    autostart: Vec<String>,
    /// "mod+shift+1" = "move-to-workspace:1"; merged over the defaults, "none" removes a bind.
    keybinds: BTreeMap<String, String>,
}

/// One parsed keybind.
#[derive(Debug, Clone)]
pub struct Binding {
    pub logo: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    /// Lower-cased raw keysym.
    pub sym: u32,
    pub action: Action,
}

impl Binding {
    pub fn matches(&self, mods: &ModifiersState, sym: u32) -> bool {
        self.sym == sym && self.logo == mods.logo && self.alt == mods.alt && self.ctrl == mods.ctrl && self.shift == mods.shift
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub general: General,
    pub input: Input,
    pub focused: Color32F,
    pub unfocused: Color32F,
    pub background: Color32F,
    pub autostart: Vec<String>,
    pub bindings: Vec<Binding>,
}

impl Default for Config {
    fn default() -> Self {
        Self::from_raw(RawConfig::default(), false).0
    }
}

const DEFAULT_KEYBINDS: &[(&str, &str)] = &[
    ("mod+return", "spawn"),
    ("mod+q", "close"),
    ("mod+tab", "focus-next"),
    ("mod+f", "toggle-floating"),
    ("mod+h", "focus-left"),
    ("mod+l", "focus-right"),
    ("mod+k", "focus-up"),
    ("mod+j", "focus-down"),
    ("mod+shift+h", "swap-left"),
    ("mod+shift+l", "swap-right"),
    ("mod+shift+k", "swap-up"),
    ("mod+shift+j", "swap-down"),
    ("mod+ctrl+h", "shrink"),
    ("mod+ctrl+l", "grow"),
    ("mod+shift+f", "toggle-fullscreen"),
    ("mod+shift+e", "quit"),
    ("mod+shift+r", "reload"),
    ("mod+1", "workspace:1"),
    ("mod+2", "workspace:2"),
    ("mod+3", "workspace:3"),
    ("mod+4", "workspace:4"),
    ("mod+5", "workspace:5"),
    ("mod+6", "workspace:6"),
    ("mod+7", "workspace:7"),
    ("mod+8", "workspace:8"),
    ("mod+9", "workspace:9"),
    ("mod+shift+1", "move-to-workspace:1"),
    ("mod+shift+2", "move-to-workspace:2"),
    ("mod+shift+3", "move-to-workspace:3"),
    ("mod+shift+4", "move-to-workspace:4"),
    ("mod+shift+5", "move-to-workspace:5"),
    ("mod+shift+6", "move-to-workspace:6"),
    ("mod+shift+7", "move-to-workspace:7"),
    ("mod+shift+8", "move-to-workspace:8"),
    ("mod+shift+9", "move-to-workspace:9"),
];

impl Config {
    pub fn path() -> PathBuf {
        if let Ok(p) = std::env::var("RUSTED_CONFIG") {
            return p.into();
        }
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default();
        base.join("rusted-wm").join("config.toml")
    }

    /// Load the config; returns it with a list of human-readable problems (empty if all fine).
    pub fn load(real_session: bool) -> (Self, Vec<String>) {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text, real_session),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), vec![]),
            Err(e) => (Self::default(), vec![format!("cannot read {}: {e}", path.display())]),
        }
    }

    pub fn parse(text: &str, real_session: bool) -> (Self, Vec<String>) {
        match toml::from_str::<RawConfig>(text) {
            Ok(raw) => Self::from_raw(raw, real_session),
            Err(e) => (Self::default(), vec![format!("config parse error: {e}")]),
        }
    }

    fn from_raw(raw: RawConfig, real_session: bool) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        let mut color = |name: &str, s: &str, fallback: Color32F| {
            parse_color(s).unwrap_or_else(|| {
                problems.push(format!("invalid colour for {name}: {s:?} (use #rrggbb or #rrggbbaa)"));
                fallback
            })
        };
        let focused = color("colors.focused", &raw.colors.focused, Color32F::new(0.45, 0.65, 1.0, 1.0));
        let unfocused = color("colors.unfocused", &raw.colors.unfocused, Color32F::new(0.25, 0.27, 0.35, 1.0));
        let background = color("colors.background", &raw.colors.background, Color32F::new(0.08, 0.08, 0.12, 1.0));

        let super_mod = match raw.general.modifier.to_ascii_lowercase().as_str() {
            "super" | "logo" | "win" => true,
            "alt" => false,
            "auto" => real_session,
            other => {
                problems.push(format!("invalid general.modifier {other:?} (use auto, alt or super)"));
                real_session
            }
        };
        let mut keybinds: BTreeMap<String, String> =
            DEFAULT_KEYBINDS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        keybinds.extend(raw.keybinds);

        let mut bindings = Vec::new();
        for (key, action) in &keybinds {
            if action == "none" {
                continue;
            }
            match (parse_binding(key, super_mod), parse_action(action, &raw.general.terminal)) {
                (Ok(mut b), Ok(a)) => {
                    b.action = a;
                    bindings.push(b);
                }
                (Err(e), _) | (_, Err(e)) => problems.push(format!("keybind {key:?} = {action:?}: {e}")),
            }
        }

        let general = General {
            gap: raw.general.gap.max(0),
            border: raw.general.border.max(0),
            split_ratio: raw.general.split_ratio.clamp(crate::layout::MIN_RATIO, crate::layout::MAX_RATIO),
            ..raw.general
        };
        (Self { general, input: raw.input, focused, unfocused, background, autostart: raw.autostart, bindings }, problems)
    }
}

fn parse_color(s: &str) -> Option<Color32F> {
    let h = s.strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|v| v as f32 / 255.0);
    Some(Color32F::new(byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { 1.0 }))
}

fn parse_binding(key: &str, super_mod: bool) -> Result<Binding, String> {
    let mut b = Binding { logo: false, alt: false, ctrl: false, shift: false, sym: 0, action: Action::Quit };
    let mut sym = None;
    for part in key.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "mod" => {
                if super_mod {
                    b.logo = true
                } else {
                    b.alt = true
                }
            }
            "super" | "logo" | "win" => b.logo = true,
            "alt" => b.alt = true,
            "ctrl" | "control" => b.ctrl = true,
            "shift" => b.shift = true,
            _ => {
                let s = xkb::keysym_from_name(part, xkb::KEYSYM_CASE_INSENSITIVE);
                if s.raw() == 0 {
                    return Err(format!("unknown key {part:?}"));
                }
                sym = Some(normalize_sym(s.raw()));
            }
        }
    }
    b.sym = sym.ok_or("no key given")?;
    Ok(b)
}

/// Letters are compared lower-case so `Shift` does not change the symbol.
pub fn normalize_sym(raw: u32) -> u32 {
    if (0x41..=0x5a).contains(&raw) {
        raw + 0x20
    } else {
        raw
    }
}

fn parse_action(s: &str, terminal: &str) -> Result<Action, String> {
    let (name, arg) = match s.split_once(':') {
        Some((n, a)) => (n.trim(), Some(a.trim())),
        None => (s.trim(), None),
    };
    let ws = |arg: Option<&str>| -> Result<usize, String> {
        match arg.and_then(|a| a.parse::<usize>().ok()) {
            Some(n) if (1..=crate::wm::WORKSPACE_COUNT).contains(&n) => Ok(n - 1),
            _ => Err(format!("{name} needs a workspace number 1-{}", crate::wm::WORKSPACE_COUNT)),
        }
    };
    Ok(match name {
        "spawn" => Action::Exec(arg.unwrap_or(terminal).to_string()),
        "close" => Action::Close,
        "focus-next" => Action::FocusNext,
        "focus-left" => Action::FocusDir(-1, 0),
        "focus-right" => Action::FocusDir(1, 0),
        "focus-up" => Action::FocusDir(0, -1),
        "focus-down" => Action::FocusDir(0, 1),
        "toggle-floating" => Action::ToggleFloating,
        "toggle-fullscreen" => Action::ToggleFullscreen,
        "swap-left" => Action::SwapDir(-1, 0),
        "swap-right" => Action::SwapDir(1, 0),
        "swap-up" => Action::SwapDir(0, -1),
        "swap-down" => Action::SwapDir(0, 1),
        "grow" => Action::Resize(0.05),
        "shrink" => Action::Resize(-0.05),
        "workspace" => Action::Workspace(ws(arg)?),
        "move-to-workspace" => Action::MoveToWorkspace(ws(arg)?),
        "reload" => Action::Reload,
        "quit" => Action::Quit,
        other => return Err(format!("unknown action {other:?}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_all_bindings() {
        let (c, problems) = Config::parse("", false);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(c.bindings.len(), DEFAULT_KEYBINDS.len());
        assert_eq!(c.general.gap, 12);
    }

    #[test]
    fn overrides_and_removal() {
        let (c, p) = Config::parse(
            "[general]\ngap = 4\nmodifier = \"super\"\n[keybinds]\n\"mod+return\" = \"spawn:foot\"\n\"mod+q\" = \"none\"",
            false,
        );
        assert!(p.is_empty(), "{p:?}");
        assert_eq!(c.general.gap, 4);
        assert_eq!(c.bindings.len(), DEFAULT_KEYBINDS.len() - 1);
        let ret = c.bindings.iter().find(|b| matches!(&b.action, Action::Exec(s) if s == "foot")).unwrap();
        assert!(ret.logo && !ret.alt);
    }

    #[test]
    fn bad_input_is_reported_not_fatal() {
        let (c, p) = Config::parse("[colors]\nfocused = \"red\"\n[keybinds]\n\"mod+x\" = \"fly\"\n\"mod+nokey\" = \"close\"", false);
        assert_eq!(p.len(), 3, "{p:?}");
        assert_eq!(c.general.border, 2);
        let (_, p) = Config::parse("[general\n", false);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn modifier_auto_follows_session_kind() {
        let ret = |real| {
            let (c, _) = Config::parse("", real);
            let b = c.bindings.iter().find(|b| matches!(&b.action, Action::Exec(_))).unwrap();
            (b.logo, b.alt)
        };
        assert_eq!(ret(false), (false, true)); // nested: Alt
        assert_eq!(ret(true), (true, false)); // real session: Super
        let (c, _) = Config::parse("[general]\nmodifier = \"alt\"", true);
        assert!(c.bindings.iter().all(|b| !b.logo));
    }

    #[test]
    fn colors() {
        let c = parse_color("#ff0000").unwrap();
        assert_eq!((c.r(), c.g(), c.b(), c.a()), (1.0, 0.0, 0.0, 1.0));
        assert!(parse_color("ff0000").is_none());
        assert!(parse_color("#ff00").is_none());
    }
}
