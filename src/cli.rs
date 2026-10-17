//! `rusted-wm config ...`: read and change the config file from the command line (and, later, from
//! a settings app). Edits keep the file's comments and layout (toml_edit), are checked against the
//! schema before anything is written, keep a `.bak` of the previous file, and finish by asking a
//! running compositor to reload.

use std::{io::{Read, Write}, os::unix::net::UnixStream, path::Path, time::Duration};

use toml_edit::{Array, DocumentMut, Item, Table, Value};

use crate::{
    config::{parse_action, parse_binding, parse_color, Apps, Config, DEFAULT_KEYBINDS},
    schema::{Kind, Setting, SETTINGS},
};

/// `println!` that ignores a closed pipe (`rusted-wm config list | head`).
macro_rules! outln {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

const USAGE: &str = "usage: rusted-wm config <command>
  path                         print the config file path
  list                         every setting with its current value and default
  get KEY                      current value of section.key (e.g. general.gap)
  set KEY VALUE                change a setting (checked against its type and range)
  unset KEY                    drop a setting from the file (back to the default)
  binds                        all keybinds (defaults and your changes)
  bind KEYS ACTION             e.g. bind mod+shift+s 'spawn:grim -g \"$(slurp)\" - | wl-copy'
  unbind KEYS [--disable]      remove your bind (back to the default); --disable turns a default off
  autostart list|add CMD|remove N
  dump                         everything above as one JSON document (used by rusted-settings)
(also: rusted-wm ctl COMMAND...   send a control command, e.g. `ctl ws-json DP-1`, `ctl info`)
options: --no-reload   do not tell a running compositor to reload";

/// `rusted-wm ctl COMMAND...`: talk to the running compositor (what tools/ctl.py does, without Python).
pub fn ctl(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("usage: rusted-wm ctl COMMAND [ARGS...]   (e.g. info, ws 2, ws-json DP-1, volume up)");
        return 2;
    }
    match send_command(&args.join(" ")) {
        Some(reply) => {
            outln!("{}", reply.trim_end());
            0
        }
        None => {
            eprintln!("error: no running rusted-wm control socket found");
            1
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let no_reload = args.iter().any(|a| a == "--no-reload");
    let args: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--no-reload").collect();
    let path = Config::path();
    let result = match args.as_slice() {
        ["path"] => {
            outln!("{}", path.display());
            Ok(())
        }
        ["list"] => list(&path),
        ["get", key] => get(&path, key),
        ["binds"] => binds(&path),
        ["dump"] => dump(&path),
        ["autostart", "list"] => autostart_list(&path),
        ["set", key, value] => modify(&path, no_reload, |t| set_value(t, key, value)),
        ["unset", key] => modify(&path, no_reload, |t| unset(t, key)),
        ["bind", keys, action] => modify(&path, no_reload, |t| bind(t, keys, action)),
        ["unbind", keys] => modify(&path, no_reload, |t| unbind(t, keys, false)),
        ["unbind", keys, "--disable"] | ["unbind", "--disable", keys] => modify(&path, no_reload, |t| unbind(t, keys, true)),
        ["autostart", "add", cmd] => modify(&path, no_reload, |t| autostart_add(t, cmd)),
        ["autostart", "remove", n] => modify(&path, no_reload, |t| autostart_remove(t, n)),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

// ---- pure text edits (unit tested) ----

fn find_setting(key: &str) -> Result<&'static Setting, String> {
    let (section, name) = key.split_once('.').map(|(s, n)| (s, n)).unwrap_or(("", key));
    SETTINGS.iter().find(|s| s.section == section && s.key == name).ok_or_else(|| {
        let all: Vec<String> = SETTINGS
            .iter()
            .filter(|s| !s.section.is_empty())
            .map(|s| format!("{}.{}", s.section, s.key))
            .collect();
        format!("unknown setting `{key}`; settings: {}", all.join(", "))
    })
}

fn parse_doc(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>().map_err(|e| format!("the config file is not valid TOML: {e}"))
}

/// Replace/insert `key`, keeping the old value's trailing comment and the key's own comments.
fn put(table: &mut Table, key: &str, mut new: Value) {
    match table.get_mut(key) {
        // Replace in place: `Table::insert` would also replace the key and lose the comments above it.
        Some(item) => {
            if let Some(old) = item.as_value() {
                *new.decor_mut() = old.decor().clone();
            }
            *item = Item::Value(new);
        }
        None => {
            table.insert(key, Item::Value(new));
        }
    }
}

/// The table for `section` ("" = top level), created if missing.
fn section_table<'a>(doc: &'a mut DocumentMut, section: &str) -> Result<&'a mut Table, String> {
    if section.is_empty() {
        return Ok(doc.as_table_mut());
    }
    doc.entry(section)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| format!("`{section}` in the config file is not a [{section}] table"))
}

fn typed_value(setting: &Setting, raw: &str) -> Result<Value, String> {
    let name = format!("{}.{}", setting.section, setting.key);
    Ok(match setting.kind {
        Kind::Int { min, max } => {
            let n: i64 = raw.parse().map_err(|_| format!("{name} needs a whole number, got `{raw}`"))?;
            if !(min..=max).contains(&n) {
                return Err(format!("{name} must be between {min} and {max}, got {n}"));
            }
            Value::from(n)
        }
        Kind::Float { min, max } => {
            let f: f64 = raw.parse().map_err(|_| format!("{name} needs a number, got `{raw}`"))?;
            if !(min..=max).contains(&f) {
                return Err(format!("{name} must be between {min} and {max}, got {f}"));
            }
            Value::from(f)
        }
        Kind::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Value::from(true),
            "false" | "no" | "off" | "0" => Value::from(false),
            _ => return Err(format!("{name} is true or false, got `{raw}`")),
        },
        Kind::Text => Value::from(raw),
        Kind::Choice(choices) => {
            if !choices.contains(&raw) {
                return Err(format!("{name} must be one of {}, got `{raw}`", choices.join(", ")));
            }
            Value::from(raw)
        }
        Kind::Color => {
            if parse_color(raw).is_none() {
                return Err(format!("{name} needs a colour like #rrggbb or #rrggbbaa, got `{raw}`"));
            }
            Value::from(raw)
        }
        Kind::List => return Err(format!("{name} is a list: use `rusted-wm config autostart ...`")),
        Kind::Map => return Err(format!("{name} is a table: use `rusted-wm config bind/unbind`")),
    })
}

pub fn set_value(text: &str, key: &str, raw: &str) -> Result<String, String> {
    let setting = find_setting(key)?;
    let value = typed_value(setting, raw)?;
    let mut doc = parse_doc(text)?;
    put(section_table(&mut doc, setting.section)?, setting.key, value);
    Ok(doc.to_string())
}

pub fn unset(text: &str, key: &str) -> Result<String, String> {
    let setting = find_setting(key)?;
    let mut doc = parse_doc(text)?;
    let table = if setting.section.is_empty() {
        Some(doc.as_table_mut())
    } else {
        doc.get_mut(setting.section).and_then(Item::as_table_mut)
    };
    if let Some(table) = table {
        table.remove(setting.key);
    }
    Ok(doc.to_string())
}

pub fn bind(text: &str, keys: &str, action: &str) -> Result<String, String> {
    parse_binding(keys, true).map_err(|e| format!("bad key combination `{keys}`: {e}"))?;
    parse_action(action, &Apps::default(), "terminal").map_err(|e| format!("bad action `{action}`: {e}"))?;
    let mut doc = parse_doc(text)?;
    put(section_table(&mut doc, "keybinds")?, keys, Value::from(action));
    Ok(doc.to_string())
}

pub fn unbind(text: &str, keys: &str, disable: bool) -> Result<String, String> {
    let mut doc = parse_doc(text)?;
    if disable {
        put(section_table(&mut doc, "keybinds")?, keys, Value::from("none"));
    } else if let Some(table) = doc.get_mut("keybinds").and_then(Item::as_table_mut) {
        table.remove(keys);
    }
    Ok(doc.to_string())
}

pub fn autostart_add(text: &str, cmd: &str) -> Result<String, String> {
    let mut doc = parse_doc(text)?;
    let item = doc.entry("autostart").or_insert(Item::Value(Value::Array(Array::new())));
    let array = item.as_array_mut().ok_or("`autostart` in the config file is not a list")?;
    let mut value = Value::from(cmd);
    // Follow the file's style: one entry per line when the existing list is written that way.
    if array.iter().any(|v| v.decor().prefix().and_then(|p| p.as_str()).is_some_and(|p| p.contains('\n'))) {
        value.decor_mut().set_prefix("\n  ");
        array.set_trailing_comma(true);
    }
    array.push_formatted(value);
    Ok(doc.to_string())
}

pub fn autostart_remove(text: &str, n: &str) -> Result<String, String> {
    let n: usize = n.parse().map_err(|_| format!("`{n}` is not a number (see `autostart list`)"))?;
    let mut doc = parse_doc(text)?;
    let array = doc
        .get_mut("autostart")
        .and_then(Item::as_array_mut)
        .ok_or("there is no autostart list in the config file")?;
    if n == 0 || n > array.len() {
        return Err(format!("autostart has {} entries, no #{n}", array.len()));
    }
    array.remove(n - 1);
    Ok(doc.to_string())
}

// ---- file handling and output ----

fn read(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Apply `edit` to the file: validate, back up, write atomically, then reload the compositor.
fn modify(path: &Path, no_reload: bool, edit: impl FnOnce(&str) -> Result<String, String>) -> Result<(), String> {
    let old = read(path)?;
    let new = edit(&old)?;
    if new == old {
        outln!("nothing to change");
        return Ok(());
    }
    // Report problems the edit introduced (problems that were already there are not its fault).
    let (_, before) = Config::parse(&old, true);
    let (_, after) = Config::parse(&new, true);
    let introduced: Vec<&String> = after.iter().filter(|p| !before.contains(p)).collect();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    if !old.is_empty() {
        let _ = std::fs::write(path.with_extension("toml.bak"), &old);
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, &new).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    outln!("updated {}", path.display());
    for p in &introduced {
        outln!("  warning: {p}");
    }
    if !no_reload {
        match send_command("reload") {
            Some(reply) if reply.trim() == "ok" => outln!("compositor reloaded"),
            Some(reply) => outln!("compositor reloaded: {}", reply.trim()),
            None => outln!("(no running compositor found; the change applies at the next start)"),
        }
    }
    Ok(())
}

/// Send one control command to the running compositor and return its reply (None if there is no
/// compositor): its own `$WAYLAND_DISPLAY` socket first, else the newest `rusted-*-ctl`.
pub fn send_command(command: &str) -> Option<String> {
    let dir = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").ok()?);
    let own = std::env::var("WAYLAND_DISPLAY").ok().map(|d| dir.join(format!("{d}-ctl"))).filter(|p| p.exists());
    let socket = own.or_else(|| {
        std::fs::read_dir(&dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with("rusted-") && n.ends_with("-ctl")
            })
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
            .map(|e| e.path())
    })?;
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream.write_all(command.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply).ok()?;
    Some(reply)
}

fn current(path: &Path, setting: &Setting) -> Result<(String, bool), String> {
    let doc = parse_doc(&read(path)?)?;
    let item = if setting.section.is_empty() { doc.get(setting.key) } else { doc.get(setting.section).and_then(|s| s.get(setting.key)) };
    Ok(match item.and_then(Item::as_value) {
        Some(v) => (display_value(v), true),
        None => (display_literal(setting.default), false),
    })
}

fn display_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.value().clone(),
        other => {
            // Without the comment that may trail it in the file.
            let mut plain = other.clone();
            plain.decor_mut().clear();
            plain.to_string().trim().to_string()
        }
    }
}

fn display_literal(literal: &str) -> String {
    format!("v = {literal}")
        .parse::<DocumentMut>()
        .ok()
        .and_then(|d| d.get("v").and_then(Item::as_value).map(display_value))
        .unwrap_or_else(|| literal.to_string())
}

fn get(path: &Path, key: &str) -> Result<(), String> {
    let setting = find_setting(key)?;
    outln!("{}", current(path, setting)?.0);
    Ok(())
}

fn list(path: &Path) -> Result<(), String> {
    for s in SETTINGS.iter().filter(|s| !matches!(s.kind, Kind::List | Kind::Map)) {
        let (value, set) = current(path, s)?;
        let default = display_literal(s.default);
        let mark = if set && value != default { " *" } else { "" };
        outln!("{}.{} = {value}{mark}    (default {default}) {}", s.section, s.key, s.description);
    }
    outln!("\n* = changed from the default; see `autostart list` and `binds` for the lists");
    Ok(())
}

fn binds(path: &Path) -> Result<(), String> {
    let doc = parse_doc(&read(path)?)?;
    let user: std::collections::BTreeMap<String, String> = doc
        .get("keybinds")
        .and_then(Item::as_table)
        .map(|t| t.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.to_string(), s.to_string()))).collect())
        .unwrap_or_default();
    let mut all: std::collections::BTreeMap<String, (String, &str)> =
        DEFAULT_KEYBINDS.iter().map(|(k, v)| (k.to_string(), (v.to_string(), "default"))).collect();
    for (k, v) in user {
        let tag = if v == "none" { "disabled" } else { "custom" };
        all.insert(k, (v, tag));
    }
    for (k, (action, tag)) in &all {
        outln!("{k:<26} {action:<40} [{tag}]");
    }
    Ok(())
}

/// Current state as JSON: settings (value, default, whether the file sets it), binds, autostart.
fn dump(path: &Path) -> Result<(), String> {
    use crate::schema::json_str;
    fn conv(v: &Value) -> String {
        match v {
            Value::String(s) => json_str(s.value()),
            Value::Integer(i) => i.value().to_string(),
            Value::Float(f) => f.value().to_string(),
            Value::Boolean(b) => b.value().to_string(),
            other => json_str(other.to_string().trim()),
        }
    }
    let text = read(path)?;
    let doc = parse_doc(&text)?;
    let mut settings = Vec::new();
    for s in SETTINGS.iter().filter(|s| !matches!(s.kind, Kind::List | Kind::Map)) {
        let item = doc.get(s.section).and_then(|t| t.get(s.key));
        let (value, set) = match item.and_then(Item::as_value) {
            Some(v) => (conv(v), true),
            None => (crate::schema::default_json(s.default), false),
        };
        settings.push(format!(
            "{{\"key\":{},\"value\":{value},\"default\":{},\"set\":{set}}}",
            json_str(&format!("{}.{}", s.section, s.key)),
            crate::schema::default_json(s.default)
        ));
    }
    let user: std::collections::BTreeMap<String, String> = doc
        .get("keybinds")
        .and_then(Item::as_table)
        .map(|t| t.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.to_string(), s.to_string()))).collect())
        .unwrap_or_default();
    let mut all: std::collections::BTreeMap<String, (String, &str)> =
        DEFAULT_KEYBINDS.iter().map(|(k, v)| (k.to_string(), (v.to_string(), "default"))).collect();
    for (k, v) in user {
        let tag = if v == "none" { "disabled" } else { "custom" };
        all.insert(k, (v, tag));
    }
    let binds: Vec<String> = all
        .iter()
        .map(|(k, (a, t))| format!("{{\"keys\":{},\"action\":{},\"tag\":{}}}", json_str(k), json_str(a), json_str(t)))
        .collect();
    let autostart: Vec<String> = doc
        .get("autostart")
        .and_then(Item::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(json_str)).collect())
        .unwrap_or_default();
    let (_, problems) = Config::parse(&text, true);
    let problems: Vec<String> = problems.iter().map(|p| json_str(p)).collect();
    outln!(
        "{{\"path\":{},\"settings\":[{}],\"binds\":[{}],\"autostart\":[{}],\"problems\":[{}]}}",
        json_str(&path.display().to_string()),
        settings.join(","),
        binds.join(","),
        autostart.join(","),
        problems.join(",")
    );
    Ok(())
}

fn autostart_list(path: &Path) -> Result<(), String> {
    let doc = parse_doc(&read(path)?)?;
    let entries: Vec<String> = doc
        .get("autostart")
        .and_then(Item::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if entries.is_empty() {
        outln!("(no autostart entries)");
    }
    for (i, e) in entries.iter().enumerate() {
        outln!("{}: {e}", i + 1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# my config\nautostart = [\n  \"waybar\",\n  \"mako\",\n]\n\n[general]\n# gaps between windows\ngap = 12   # was 8\nscale = 1.0\n\n[keybinds]\n\"mod+d\" = \"spawn:fuzzel\"\n";

    #[test]
    fn set_keeps_comments_and_layout() {
        let out = set_value(SAMPLE, "general.gap", "20").unwrap();
        assert!(out.contains("gap = 20   # was 8"), "{out}");
        assert!(out.contains("# my config") && out.contains("# gaps between windows"));
        assert!(out.contains("\"waybar\",\n  \"mako\","), "the list must be untouched: {out}");
    }

    #[test]
    fn set_creates_missing_sections_and_files() {
        let out = set_value("", "audio.step", "10").unwrap();
        assert_eq!(out.trim(), "[audio]\nstep = 10");
        let out = set_value(SAMPLE, "colors.focused", "#ff0000").unwrap();
        assert!(out.contains("[colors]\nfocused = \"#ff0000\""), "{out}");
        // The result must parse cleanly.
        assert!(Config::parse(&out, true).1.is_empty());
    }

    #[test]
    fn set_rejects_bad_input() {
        assert!(set_value(SAMPLE, "general.gap", "-3").unwrap_err().contains("between 0 and 200"));
        assert!(set_value(SAMPLE, "general.gap", "abc").unwrap_err().contains("whole number"));
        assert!(set_value(SAMPLE, "general.modifier", "hyper").unwrap_err().contains("one of auto, alt, super"));
        assert!(set_value(SAMPLE, "colors.focused", "red").unwrap_err().contains("colour"));
        assert!(set_value(SAMPLE, "general.gapp", "1").unwrap_err().contains("unknown setting"));
        assert!(set_value(SAMPLE, "autostart", "x").is_err());
        assert!(set_value("[general", "general.gap", "1").unwrap_err().contains("not valid TOML"));
    }

    #[test]
    fn bool_and_float_values() {
        let out = set_value(SAMPLE, "general.xwayland", "off").unwrap();
        assert!(out.contains("xwayland = false"));
        let out = set_value(SAMPLE, "general.scale", "1.5").unwrap();
        assert!(out.contains("scale = 1.5"));
    }

    #[test]
    fn unset_removes_only_that_key() {
        let out = unset(SAMPLE, "general.gap").unwrap();
        assert!(!out.contains("gap ="));
        assert!(out.contains("scale = 1.0"));
    }

    #[test]
    fn bind_validates_and_edits() {
        let out = bind(SAMPLE, "mod+shift+s", "spawn:grim - | wl-copy").unwrap();
        assert!(out.contains("\"mod+shift+s\" = \"spawn:grim - | wl-copy\""), "{out}");
        assert!(bind(SAMPLE, "mod+nonsense", "close").unwrap_err().contains("bad key combination"));
        assert!(bind(SAMPLE, "mod+x", "explode").unwrap_err().contains("bad action"));
        let out = bind(SAMPLE, "mod+d", "spawn:wofi").unwrap();
        assert!(out.contains("\"mod+d\" = \"spawn:wofi\"") && !out.contains("fuzzel"));
        let out = unbind(SAMPLE, "mod+d", false).unwrap();
        assert!(!out.contains("mod+d"));
        let out = unbind(SAMPLE, "mod+q", true).unwrap();
        assert!(out.contains("\"mod+q\" = \"none\""));
    }

    #[test]
    fn autostart_edit_follows_file_style() {
        let out = autostart_add(SAMPLE, "swaybg -c '#000000'").unwrap();
        assert!(out.contains("  \"mako\",\n  \"swaybg -c '#000000'\","), "{out}");
        assert!(Config::parse(&out, true).0.autostart.len() == 3);
        let out = autostart_remove(SAMPLE, "1").unwrap();
        assert!(!out.contains("waybar") && out.contains("mako"));
        assert!(autostart_remove(SAMPLE, "9").is_err());
        let out = autostart_add("", "mako").unwrap();
        assert_eq!(out.trim(), "autostart = [\"mako\"]");
    }

    #[test]
    fn schema_defaults_display_as_plain_values() {
        assert_eq!(display_literal("\"konsole\""), "konsole");
        assert_eq!(display_literal("12"), "12");
        assert_eq!(display_literal("true"), "true");
    }
}
