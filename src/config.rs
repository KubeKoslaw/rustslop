//! TOML configuration: appearance, terminal, autostart and keybinds.
//!
//! Loaded from `$RUSTED_CONFIG`, else `$XDG_CONFIG_HOME/rusted-wm/config.toml`
//! (`~/.config/rusted-wm/config.toml`). A missing file means defaults; a broken file is reported
//! and defaults are used. See `config.example.toml` for every option.

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use smithay::{
    backend::renderer::Color32F,
    input::keyboard::{xkb, ModifiersState},
};

use crate::input::Action;

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// Output scale, may be fractional (e.g. 1.5). Clamped to 0.5-4.
    pub scale: f64,
    /// Start Xwayland (if installed) so X11 apps work.
    pub xwayland: bool,
}

impl Default for General {
    fn default() -> Self {
        Self { gap: 12, border: 2, terminal: "konsole".into(), split_ratio: 0.5, modifier: "auto".into(), scale: 1.0, xwayland: true }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Input {
    pub layout: String,
    pub variant: String,
    pub options: String,
    pub repeat_delay: i32,
    pub repeat_rate: i32,
    pub tap_to_click: bool,
    pub natural_scroll: bool,
    /// Natural scrolling for mice (wheel direction); touchpads use `natural_scroll`.
    pub mouse_natural_scroll: bool,
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
            mouse_natural_scroll: false,
        }
    }
}

/// Default programs, referenced from keybinds as `spawn:@launcher`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Apps {
    /// Empty = use `general.terminal`.
    pub terminal: String,
    pub launcher: String,
    pub browser: String,
    pub file_manager: String,
    pub lock: String,
}

impl Default for Apps {
    fn default() -> Self {
        Self {
            terminal: String::new(),
            launcher: "fuzzel".into(),
            browser: "xdg-open https://".into(),
            file_manager: "xdg-open .".into(),
            lock: "swaylock -f -c 000000".into(),
        }
    }
}

impl Apps {
    /// Command for `@name`, with the terminal falling back to `general.terminal`.
    fn resolve(&self, name: &str, general_terminal: &str) -> Option<String> {
        Some(match name {
            "terminal" if self.terminal.is_empty() => general_terminal.to_string(),
            "terminal" => self.terminal.clone(),
            "launcher" => self.launcher.clone(),
            "browser" => self.browser.clone(),
            "file_manager" => self.file_manager.clone(),
            "lock" => self.lock.clone(),
            _ => return None,
        })
    }
}

/// Volume / mute actions (`volume-up`, `volume-down`, `volume-mute`, `mic-mute`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Audio {
    /// Percent per key press.
    pub step: u32,
    /// Limit in percent; above 100 is software boost.
    pub max_volume: u32,
    /// Show a notification with the new level.
    pub notify: bool,
}

impl Default for Audio {
    fn default() -> Self {
        Self { step: 5, max_volume: 100, notify: true }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    audio: Audio,
    apps: Apps,
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
    /// As written in the config ("mod+shift+1"), for messages.
    pub key: String,
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
    pub audio: crate::audio::AudioSettings,
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

pub(crate) const DEFAULT_KEYBINDS: &[(&str, &str)] = &[
    ("mod+return", "spawn"),
    ("mod+q", "close"),
    ("mod+tab", "focus-next"),
    ("mod+f", "toggle-floating"),
    ("mod+d", "spawn:@launcher"),
    ("mod+b", "spawn:@browser"),
    ("mod+e", "spawn:@file_manager"),
    ("mod+shift+x", "spawn:@lock"),
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
    // Media keys and keyboard knobs work without any config.
    ("XF86AudioRaiseVolume", "volume-up"),
    ("XF86AudioLowerVolume", "volume-down"),
    ("XF86AudioMute", "volume-mute"),
    ("XF86AudioMicMute", "mic-mute"),
    ("XF86AudioPlay", "spawn:playerctl play-pause"),
    ("XF86AudioPause", "spawn:playerctl pause"),
    ("XF86AudioNext", "spawn:playerctl next"),
    ("XF86AudioPrev", "spawn:playerctl previous"),
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
        // Unknown keys are reported (typos, keys in the wrong section) but never fatal.
        let mut problems = match text.parse::<toml::Table>() {
            Ok(table) => unknown_key_problems(&table),
            Err(e) => return (Self::default(), vec![format!("config parse error: {e}")]),
        };
        match toml::from_str::<RawConfig>(text) {
            Ok(raw) => {
                let (config, more) = Self::from_raw(raw, real_session);
                problems.extend(more);
                (config, problems)
            }
            Err(e) => (Self::default(), vec![format!("config parse error: {e}")]),
        }
    }

    /// Load a specific file (used by `--check-config`).
    pub fn load_file(path: &std::path::Path, real_session: bool) -> (Self, Vec<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, real_session),
            Err(e) => (Self::default(), vec![format!("cannot read {}: {e}", path.display())]),
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
            match (parse_binding(key, super_mod), parse_action(action, &raw.apps, &raw.general.terminal)) {
                (Ok(mut b), Ok(a)) => {
                    b.action = a;
                    bindings.push(b);
                }
                (Err(e), _) | (_, Err(e)) => problems.push(format!("keybind {key:?} = {action:?}: {e}")),
            }
        }

        // Report values that get clamped instead of silently changing them.
        let g = &raw.general;
        if g.gap < 0 || g.border < 0 {
            problems.push(format!("general.gap/border cannot be negative (gap = {}, border = {}), using 0", g.gap, g.border));
        } else if g.border > g.gap {
            problems.push(format!("general.border ({}) is larger than general.gap ({}): borders will overlap neighbouring windows", g.border, g.gap));
        }
        if !(crate::layout::MIN_RATIO..=crate::layout::MAX_RATIO).contains(&g.split_ratio) {
            problems.push(format!(
                "general.split_ratio {} is outside {}-{}, clamped",
                g.split_ratio,
                crate::layout::MIN_RATIO,
                crate::layout::MAX_RATIO
            ));
        }
        if !(0.5..=4.0).contains(&g.scale) {
            problems.push(format!("general.scale {} is outside 0.5-4, clamped", g.scale));
        }
        if raw.input.repeat_delay <= 0 || raw.input.repeat_rate <= 0 {
            problems.push(format!(
                "input.repeat_delay/repeat_rate must be positive (got {}/{})",
                raw.input.repeat_delay, raw.input.repeat_rate
            ));
        }
        for (i, a) in raw.autostart.iter().enumerate() {
            if a.trim().is_empty() {
                problems.push(format!("autostart entry #{} is empty", i + 1));
            }
        }
        for (i, b) in bindings.iter().enumerate() {
            if let Some(first) = bindings[..i].iter().find(|o| (o.logo, o.alt, o.ctrl, o.shift, o.sym) == (b.logo, b.alt, b.ctrl, b.shift, b.sym)) {
                problems.push(format!(
                    "keybinds {:?} ({:?}) and {:?} ({:?}) are the same keys; the first one wins",
                    first.key, first.action, b.key, b.action
                ));
            }
        }

        let general = General {
            gap: raw.general.gap.max(0),
            border: raw.general.border.max(0),
            split_ratio: raw.general.split_ratio.clamp(crate::layout::MIN_RATIO, crate::layout::MAX_RATIO),
            // The fractional-scale protocol speaks in 1/120ths.
            scale: (raw.general.scale.clamp(0.5, 4.0) * 120.0).round() / 120.0,
            ..raw.general
        };
        if raw.audio.step == 0 || raw.audio.step > 50 {
            problems.push(format!("audio.step {} is outside 1-50, using 5", raw.audio.step));
        }
        if !(50..=150).contains(&raw.audio.max_volume) {
            problems.push(format!("audio.max_volume {} is outside 50-150, using 100", raw.audio.max_volume));
        }
        let audio = crate::audio::AudioSettings {
            step: if (1..=50).contains(&raw.audio.step) { raw.audio.step } else { 5 },
            max_volume: if (50..=150).contains(&raw.audio.max_volume) { raw.audio.max_volume } else { 100 },
            notify: raw.audio.notify,
        };
        (Self { general, audio, input: raw.input, focused, unfocused, background, autostart: raw.autostart, bindings }, problems)
    }
}

/// Which keys exist comes from `schema::SETTINGS` (a unit test keeps it in sync with the structs).


/// Closest candidate to `name` (a likely typo), if any is close enough.
fn suggest<'a>(name: &str, candidates: &[&'a str]) -> Option<&'a str> {
    fn distance(a: &str, b: &str) -> usize {
        let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        let mut prev: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.iter().enumerate() {
            let mut cur = vec![i + 1];
            for (j, cb) in b.iter().enumerate() {
                cur.push((prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
            }
            prev = cur;
        }
        prev[b.len()]
    }
    candidates
        .iter()
        .map(|c| (distance(&name.to_ascii_lowercase(), c), *c))
        .filter(|(d, c)| *d < c.len() && *d <= 2.max(c.len() / 2))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Report every key the parser would silently ignore.
fn unknown_key_problems(table: &toml::Table) -> Vec<String> {
    use crate::schema::{keys_of, SECTIONS, SETTINGS};
    let mut problems = Vec::new();
    let top = keys_of("");
    // Where a known key really lives, to say so when it is in the wrong place.
    let home = |key: &str| -> Option<String> {
        if top.contains(&key) {
            return Some("a top-level key (put it above the first [section])".into());
        }
        SETTINGS.iter().find(|x| x.key == key && !x.section.is_empty()).map(|x| format!("a [{}] key", x.section))
    };

    for (key, value) in table {
        if !top.contains(&key.as_str()) {
            let hint = match (home(key), suggest(key, &top)) {
                (Some(h), _) => format!(" — it is {h}"),
                (None, Some(s)) => format!(" — did you mean `{s}`?"),
                (None, None) => String::new(),
            };
            problems.push(format!("unknown top-level key `{key}`{hint}; it is ignored"));
            continue;
        }
        if !SECTIONS.contains(&key.as_str()) {
            continue;
        }
        let keys = keys_of(key);
        let Some(inner) = value.as_table() else {
            problems.push(format!("`{key}` must be a [{key}] table"));
            continue;
        };
        for k in inner.keys().filter(|k| !keys.contains(&k.as_str())) {
            let hint = match (home(k), suggest(k, &keys)) {
                (Some(h), _) => format!(" — it is {h}, not part of [{key}]"),
                (None, Some(s)) => format!(" — did you mean `{s}`?"),
                (None, None) => String::new(),
            };
            problems.push(format!("unknown key `{k}` in [{key}]{hint}; it is ignored"));
        }
    }
    problems
}

pub(crate) fn parse_color(s: &str) -> Option<Color32F> {
    let h = s.strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|v| v as f32 / 255.0);
    Some(Color32F::new(byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { 1.0 }))
}

pub(crate) fn parse_binding(key: &str, super_mod: bool) -> Result<Binding, String> {
    let mut b = Binding { key: key.to_string(), logo: false, alt: false, ctrl: false, shift: false, sym: 0, action: Action::Quit };
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

pub(crate) fn parse_action(s: &str, apps: &Apps, terminal: &str) -> Result<Action, String> {
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
        "spawn" => Action::Exec(match arg {
            None => apps.resolve("terminal", terminal).unwrap_or_default(),
            // `@launcher`, or `@terminal -e htop`: a default program, optionally with arguments.
            Some(a) if a.starts_with('@') => {
                let (name, rest) = a[1..].split_once(' ').unwrap_or((&a[1..], ""));
                let base = apps.resolve(name, terminal).ok_or_else(|| {
                    format!("unknown app @{name} (use @terminal, @launcher, @browser, @file_manager or @lock)")
                })?;
                if rest.is_empty() { base } else { format!("{base} {rest}") }
            }
            Some(a) => a.to_string(),
        }),
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
        "move-to-output-left" => Action::MoveToOutput(-1, 0),
        "move-to-output-right" => Action::MoveToOutput(1, 0),
        "move-to-output-up" => Action::MoveToOutput(0, -1),
        "move-to-output-down" => Action::MoveToOutput(0, 1),
        "volume-up" => Action::Audio(crate::audio::AudioCmd::VolumeUp),
        "volume-down" => Action::Audio(crate::audio::AudioCmd::VolumeDown),
        "volume-mute" => Action::Audio(crate::audio::AudioCmd::VolumeMute),
        "mic-mute" => Action::Audio(crate::audio::AudioCmd::MicMute),
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
    fn unknown_keys_are_reported_with_hints() {
        let (_, p) = Config::parse("[general]\ngapp = 3\nautostart = [\"x\"]\n[keybinding]\n\"mod+q\" = \"close\"", false);
        let all = p.join("\n");
        assert!(all.contains("unknown key `gapp` in [general] — did you mean `gap`?"), "{all}");
        assert!(all.contains("unknown key `autostart` in [general] — it is a top-level key"), "{all}");
        assert!(all.contains("unknown top-level key `keybinding` — did you mean `keybinds`?"), "{all}");
        // The valid parts of a config with unknown keys still apply.
        let (c, _) = Config::parse("[general]\ngap = 7\nbogus = 1", false);
        assert_eq!(c.general.gap, 7);
    }

    #[test]
    fn out_of_range_values_and_conflicts_are_reported() {
        let (_, p) = Config::parse(
            "[general]\ngap = -1\nscale = 9\nsplit_ratio = 0.99\n[keybinds]\n\"mod+x\" = \"close\"\n\"mod+X\" = \"quit\"",
            false,
        );
        let all = p.join("\n");
        assert!(all.contains("cannot be negative"), "{all}");
        assert!(all.contains("general.scale 9"), "{all}");
        assert!(all.contains("split_ratio"), "{all}");
        assert!(all.contains("are the same keys"), "{all}");
    }

    #[test]
    fn key_lists_match_the_example_config() {
        let example = include_str!("../config.example.toml");
        for x in crate::schema::SETTINGS {
            let documented = example.contains(&format!("{} =", x.key)) || example.contains(&format!("[{}]", x.key));
            assert!(documented, "config.example.toml does not document `{}`", x.key);
        }
        // And the shipped examples produce no unknown-key complaints.
        for text in [example, include_str!("../contrib/config.rice.toml")] {
            let table: toml::Table = text.parse().unwrap();
            assert!(unknown_key_problems(&table).is_empty());
        }
    }

    #[test]
    fn schema_matches_config_structs() {
        use crate::schema::{keys_of, SETTINGS};
        let sections: [(&str, toml::Value); 5] = [
            ("general", toml::Value::try_from(General::default()).unwrap()),
            ("apps", toml::Value::try_from(Apps::default()).unwrap()),
            ("input", toml::Value::try_from(Input::default()).unwrap()),
            ("colors", toml::Value::try_from(Colors::default()).unwrap()),
            ("audio", toml::Value::try_from(Audio::default()).unwrap()),
        ];
        for (name, value) in &sections {
            let table = value.as_table().unwrap();
            let mut have: Vec<&str> = table.keys().map(String::as_str).collect();
            let mut schema = keys_of(name);
            have.sort();
            schema.sort();
            assert_eq!(have, schema, "schema and struct disagree about [{name}]");
            for x in SETTINGS.iter().filter(|x| x.section == *name) {
                let expected = format!("v = {}", x.default).parse::<toml::Table>().unwrap().remove("v").unwrap();
                assert_eq!(table[x.key], expected, "default of {name}.{} differs from the schema", x.key);
                // And the schema default is accepted without complaints.
                let (_, problems) = Config::parse(&format!("[{name}]\n{} = {}", x.key, x.default), false);
                assert!(problems.is_empty(), "{name}.{}: {problems:?}", x.key);
            }
        }
        let json = crate::schema::to_json();
        assert!(json.starts_with("{\"version\":1") && json.contains("\"key\":\"scale\""));
    }

    #[test]
    fn every_listed_action_parses() {
        for (name, arg) in crate::schema::ACTIONS {
            let text = match *arg {
                "N" => format!("{name}:1"),
                "CMD" => format!("{name}:echo hi"),
                _ => name.to_string(),
            };
            parse_action(&text, &Apps::default(), "foot").unwrap_or_else(|e| panic!("{text}: {e}"));
        }
    }

    #[test]
    fn apps_are_referenced_with_at_names() {
        let text = "[general]\nterminal = \"foot\"\n[apps]\nlauncher = \"wofi --show drun\"\n[keybinds]\n\"mod+x\" = \"spawn:@launcher\"\n\"mod+y\" = \"spawn:@terminal -e htop\"\n\"mod+z\" = \"spawn:@nope\"";
        let (c, p) = Config::parse(text, false);
        let exec = |key: &str| {
            let b = c.bindings.iter().find(|b| b.key == key).unwrap_or_else(|| panic!("no bind {key}"));
            match &b.action {
                Action::Exec(cmd) => cmd.clone(),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(exec("mod+x"), "wofi --show drun");
        assert_eq!(exec("mod+y"), "foot -e htop");
        assert!(p.iter().any(|m| m.contains("unknown app @nope")), "{p:?}");
        // The default launcher bind follows the [apps] choice, too.
        let (c2, _) = Config::parse("[apps]\nlauncher = \"rofi -show drun\"", false);
        let d = c2.bindings.iter().find(|b| b.key == "mod+d").unwrap();
        assert!(matches!(&d.action, Action::Exec(cmd) if cmd == "rofi -show drun"));
    }

    #[test]
    fn shipped_rice_config_is_valid() {
        let (c, problems) = Config::parse(include_str!("../contrib/config.rice.toml"), true);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(c.autostart.iter().any(|a| a == "waybar"));
        // Media keys have no modifier and must be matched on their own.
        assert!(c.bindings.iter().any(|b| !b.logo && !b.alt && !b.ctrl && !b.shift));
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
            let b = c.bindings.iter().find(|b| matches!(&b.action, Action::Exec(_)) && (b.logo || b.alt)).unwrap();
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
