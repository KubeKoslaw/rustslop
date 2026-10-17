//! Machine-readable description of every setting: the single source of truth for which keys exist,
//! their types, ranges, defaults and whether they apply live. The parser's unknown-key check uses
//! it, `rusted-wm --dump-schema` prints it as JSON (for the settings app / editors), and unit
//! tests compare it with the real config structs so it cannot drift.

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Int { min: i64, max: i64 },
    Float { min: f64, max: f64 },
    Bool,
    Text,
    Choice(&'static [&'static str]),
    /// "#rrggbb" or "#rrggbbaa"
    Color,
    /// Array of strings.
    List,
    /// Table of string -> string.
    Map,
}

#[derive(Debug, Clone, Copy)]
pub struct Setting {
    /// Table name, or "" for top-level keys.
    pub section: &'static str,
    pub key: &'static str,
    pub kind: Kind,
    /// The default as a TOML literal.
    pub default: &'static str,
    pub description: &'static str,
    /// True when a change only takes effect after restarting the compositor.
    pub restart: bool,
}

const fn s(section: &'static str, key: &'static str, kind: Kind, default: &'static str, description: &'static str, restart: bool) -> Setting {
    Setting { section, key, kind, default, description, restart }
}

pub const SETTINGS: &[Setting] = &[
    s("", "autostart", Kind::List, "[]", "Programs started once when the compositor starts (run through `sh -c`).", true),
    s("", "keybinds", Kind::Map, "{}", "Key combination -> action, merged over the defaults; \"none\" removes a default.", false),
    s("general", "gap", Kind::Int { min: 0, max: 200 }, "12", "Pixels between windows and screen edges.", false),
    s("general", "border", Kind::Int { min: 0, max: 50 }, "2", "Focus border width; keep it at most the gap.", false),
    s("general", "terminal", Kind::Text, "\"konsole\"", "Terminal for the plain `spawn` action (`apps.terminal` overrides it).", false),
    s("general", "split_ratio", Kind::Float { min: 0.1, max: 0.9 }, "0.5", "Share of each split taken by the first window.", false),
    s("general", "modifier", Kind::Choice(&["auto", "alt", "super"]), "\"auto\"", "The `mod` key: auto = Super on a real session, Alt when nested.", false),
    s("general", "scale", Kind::Float { min: 0.5, max: 4.0 }, "1.0", "Output scale; fractional values are allowed.", false),
    s("general", "xwayland", Kind::Bool, "true", "Run Xwayland so X11 apps work.", true),
    s("apps", "terminal", Kind::Text, "\"\"", "Terminal emulator (empty = use general.terminal). Referenced as @terminal.", false),
    s("apps", "launcher", Kind::Text, "\"fuzzel\"", "Application launcher. Referenced as @launcher.", false),
    s("apps", "browser", Kind::Text, "\"xdg-open https://\"", "Web browser. Referenced as @browser.", false),
    s("apps", "file_manager", Kind::Text, "\"xdg-open .\"", "File manager. Referenced as @file_manager.", false),
    s("apps", "lock", Kind::Text, "\"swaylock -f -c 000000\"", "Screen locker. Referenced as @lock.", false),
    s("apps", "settings", Kind::Text, "\"rusted-settings\"", "This settings app. Referenced as @settings.", false),
    s("input", "layout", Kind::Text, "\"\"", "xkb keyboard layout, e.g. \"pl\" (needed on a real TTY).", true),
    s("input", "variant", Kind::Text, "\"\"", "xkb layout variant.", true),
    s("input", "options", Kind::Text, "\"\"", "xkb options, e.g. \"caps:escape\".", true),
    s("input", "repeat_delay", Kind::Int { min: 1, max: 2000 }, "200", "Key repeat delay in milliseconds.", true),
    s("input", "repeat_rate", Kind::Int { min: 1, max: 200 }, "25", "Key repeats per second.", true),
    s("input", "tap_to_click", Kind::Bool, "true", "Touchpad tap-to-click.", true),
    s("input", "natural_scroll", Kind::Bool, "false", "Touchpad natural scrolling.", true),
    s("input", "mouse_natural_scroll", Kind::Bool, "false", "Natural scrolling for mice (reverses the wheel).", true),
    s("colors", "focused", Kind::Color, "\"#73a6ff\"", "Border colour of the focused window.", false),
    s("colors", "unfocused", Kind::Color, "\"#40455a\"", "Border colour of other windows.", false),
    s("colors", "background", Kind::Color, "\"#14141f\"", "Desktop background colour.", false),
    s("audio", "step", Kind::Int { min: 1, max: 50 }, "5", "Percent per volume key press.", false),
    s("audio", "max_volume", Kind::Int { min: 50, max: 150 }, "100", "Volume limit in percent (above 100 = software boost).", false),
    s("audio", "notify", Kind::Bool, "true", "Show the new level with notify-send (needs mako/dunst).", false),
];

/// Names of the tables that exist (everything in SETTINGS with a non-empty section).
pub const SECTIONS: &[&str] = &["general", "apps", "input", "colors", "audio"];

/// Keys accepted in `section` ("" = top level, where the sections themselves are accepted too).
pub fn keys_of(section: &str) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = SETTINGS.iter().filter(|x| x.section == section).map(|x| x.key).collect();
    if section.is_empty() {
        keys.extend(SECTIONS);
    }
    keys
}

/// Actions usable in `[keybinds]`: (name, argument) where the argument is "" (none), "N"
/// (workspace number 1-9) or "CMD" (a command, may start with @app).
pub const ACTIONS: &[(&str, &str)] = &[
    ("spawn", "CMD"),
    ("close", ""),
    ("focus-next", ""),
    ("focus-left", ""),
    ("focus-right", ""),
    ("focus-up", ""),
    ("focus-down", ""),
    ("toggle-floating", ""),
    ("toggle-fullscreen", ""),
    ("swap-left", ""),
    ("swap-right", ""),
    ("swap-up", ""),
    ("swap-down", ""),
    ("move-to-output-left", ""),
    ("move-to-output-right", ""),
    ("move-to-output-up", ""),
    ("move-to-output-down", ""),
    ("grow", ""),
    ("shrink", ""),
    ("workspace", "N"),
    ("move-to-workspace", "N"),
    ("volume-up", ""),
    ("volume-down", ""),
    ("volume-mute", ""),
    ("mic-mute", ""),
    ("reload", ""),
    ("quit", ""),
];

pub fn json_str(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A TOML default literal as JSON.
pub fn default_json(literal: &str) -> String {
    let value: toml::Value = format!("v = {literal}").parse::<toml::Table>().ok().and_then(|mut t| t.remove("v")).unwrap_or(toml::Value::String(String::new()));
    fn conv(v: &toml::Value) -> String {
        match v {
            toml::Value::String(s) => json_str(s),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => format!("{f}"),
            toml::Value::Boolean(b) => b.to_string(),
            toml::Value::Array(a) => format!("[{}]", a.iter().map(conv).collect::<Vec<_>>().join(",")),
            toml::Value::Table(t) => format!("{{{}}}", t.iter().map(|(k, v)| format!("{}:{}", json_str(k), conv(v))).collect::<Vec<_>>().join(",")),
            other => json_str(&other.to_string()),
        }
    }
    conv(&value)
}

/// The whole schema as JSON (`rusted-wm --dump-schema`).
pub fn to_json() -> String {
    let settings: Vec<String> = SETTINGS
        .iter()
        .map(|x| {
            let kind = match x.kind {
                Kind::Int { min, max } => format!("\"type\":\"int\",\"min\":{min},\"max\":{max}"),
                Kind::Float { min, max } => format!("\"type\":\"float\",\"min\":{min},\"max\":{max}"),
                Kind::Bool => "\"type\":\"bool\"".into(),
                Kind::Text => "\"type\":\"text\"".into(),
                Kind::Choice(c) => format!("\"type\":\"choice\",\"choices\":[{}]", c.iter().map(|c| json_str(c)).collect::<Vec<_>>().join(",")),
                Kind::Color => "\"type\":\"color\"".into(),
                Kind::List => "\"type\":\"list\"".into(),
                Kind::Map => "\"type\":\"map\"".into(),
            };
            format!(
                "{{\"section\":{},\"key\":{},{kind},\"default\":{},\"description\":{},\"restart\":{}}}",
                json_str(x.section),
                json_str(x.key),
                default_json(x.default),
                json_str(x.description),
                x.restart
            )
        })
        .collect();
    let actions: Vec<String> = ACTIONS.iter().map(|(n, a)| format!("{{\"name\":{},\"argument\":{}}}", json_str(n), json_str(a))).collect();
    format!("{{\"version\":1,\"settings\":[{}],\"actions\":[{}]}}", settings.join(","), actions.join(","))
}
