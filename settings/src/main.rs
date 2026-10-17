//! Settings app for rusted-wm.
//!
//! A thin GTK4 client of the compositor binary: the list of settings comes from
//! `rusted-wm --dump-schema`, the current state from `rusted-wm config dump`, and every change is
//! one `rusted-wm config ...` call (which validates, keeps comments, writes a backup and tells the
//! running compositor to reload). The app itself never touches the config file.

use std::{cell::{Cell, RefCell}, path::PathBuf, process::Command, rc::Rc, time::Duration};

use gtk4 as gtk;
use gtk::{gdk, gio, glib, prelude::*};
use serde_json::Value;

const SECTIONS: &[(&str, &str)] = &[
    ("general", "General"),
    ("apps", "Default apps"),
    ("input", "Input"),
    ("colors", "Colors"),
    ("theme", "Theme"),
    ("wallpaper", "Wallpaper"),
    ("appearance", "Appearance"),
    ("audio", "Audio"),
];

// ---- talking to rusted-wm ----

fn wm_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("RUSTED_WM") {
        return p.into();
    }
    if let Some(sibling) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("rusted-wm"))) {
        if sibling.exists() {
            return sibling;
        }
    }
    "rusted-wm".into()
}

/// Run `rusted-wm ARGS`; Ok(stdout) or Err(stderr / reason).
fn run_wm(args: &[String]) -> Result<String, String> {
    let out = Command::new(wm_binary()).args(args).output().map_err(|e| format!("cannot run {}: {e}", wm_binary().display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.trim().trim_start_matches("error: ").to_string())
    }
}

fn run_json(args: &[&str]) -> Result<Value, String> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let text = run_wm(&args)?;
    serde_json::from_str(&text).map_err(|e| format!("unexpected output from rusted-wm: {e}"))
}

// ---- shared state ----

struct Ctx {
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    status: gtk::Label,
    schema: Value,
    dump: RefCell<Value>,
}

type Shared = Rc<Ctx>;

fn set_status(ctx: &Ctx, text: &str, error: bool) {
    ctx.status.set_text(text);
    if error {
        ctx.status.add_css_class("error");
    } else {
        ctx.status.remove_css_class("error");
    }
}

/// Run `rusted-wm config <args>` for each command (the last one reloads the compositor), update
/// the status line and call `done` on success.
fn apply(ctx: &Shared, commands: Vec<Vec<String>>, done: impl Fn(&Shared) + 'static) {
    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        let last = commands.len().saturating_sub(1);
        let result = gio::spawn_blocking(move || {
            let mut summary = String::new();
            for (i, cmd) in commands.into_iter().enumerate() {
                let mut args = vec!["config".to_string()];
                args.extend(cmd);
                if i != last {
                    args.push("--no-reload".into());
                }
                summary = run_wm(&args)?;
            }
            Ok::<String, String>(summary)
        })
        .await;
        match result {
            Ok(Ok(text)) => {
                // "updated FILE / warning: ... / compositor reloaded"
                let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
                let warnings: Vec<&&str> = lines.iter().filter(|l| l.starts_with("warning")).collect();
                if warnings.is_empty() {
                    let tail = lines.iter().rev().find(|l| l.contains("reload") || l.contains("no running")).copied().unwrap_or("saved");
                    set_status(&ctx, tail, false);
                } else {
                    set_status(&ctx, warnings[0], true);
                }
                reload_dump(&ctx);
                done(&ctx);
            }
            Ok(Err(e)) => set_status(&ctx, &e, true),
            Err(_) => set_status(&ctx, "internal error", true),
        }
    });
}

fn reload_dump(ctx: &Ctx) {
    if let Ok(dump) = run_json(&["config", "dump"]) {
        *ctx.dump.borrow_mut() = dump;
    }
}

fn setting_state(ctx: &Ctx, key: &str) -> Option<Value> {
    ctx.dump.borrow()["settings"].as_array()?.iter().find(|s| s["key"] == key).cloned()
}

// ---- helpers ----

fn pretty(key: &str) -> String {
    let mut s = key.replace('_', " ");
    if let Some(first) = s.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    s
}

fn hex_to_rgba(hex: &str) -> gdk::RGBA {
    let h = hex.trim_start_matches('#');
    let byte = |i: usize| h.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()).unwrap_or(0) as f32 / 255.0;
    let alpha = if h.len() == 8 { byte(6) } else { 1.0 };
    gdk::RGBA::new(byte(0), byte(2), byte(4), alpha)
}

fn rgba_to_hex(c: &gdk::RGBA) -> String {
    let b = |v: f32| (v * 255.0).round() as u8;
    if (c.alpha() - 1.0).abs() < 0.004 {
        format!("#{:02x}{:02x}{:02x}", b(c.red()), b(c.green()), b(c.blue()))
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", b(c.red()), b(c.green()), b(c.blue()), b(c.alpha()))
    }
}

/// Calls `f` once the user stops changing a value for a moment (spin buttons fire per click).
fn debounced(f: impl Fn() + 'static) -> impl Fn() {
    let generation = Rc::new(Cell::new(0u32));
    let f = Rc::new(f);
    move || {
        let id = generation.get().wrapping_add(1);
        generation.set(id);
        let (generation, f) = (generation.clone(), f.clone());
        glib::timeout_add_local_once(Duration::from_millis(350), move || {
            if generation.get() == id {
                f();
            }
        });
    }
}

fn label(text: &str, css: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).wrap(true).build();
    for c in css {
        l.add_css_class(c);
    }
    l
}

fn page(title: &str) -> (gtk::Box, gtk::Box) {
    let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(24).margin_end(24).build();
    outer.append(&label(title, &["title-2"]));
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
    outer.append(&body);
    (outer, body)
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder().child(child).hexpand(true).vexpand(true).build()
}

// ---- settings pages (generated from the schema) ----

fn setting_row(ctx: &Shared, schema: &Value) -> gtk::Box {
    let section = schema["section"].as_str().unwrap_or("");
    let name = schema["key"].as_str().unwrap_or("");
    let key = format!("{section}.{name}");
    let state = setting_state(ctx, &key).unwrap_or(Value::Null);
    let value = state["value"].clone();
    let is_set = state["set"].as_bool().unwrap_or(false);

    let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(16).margin_top(8).margin_bottom(8).build();
    let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).build();
    let mut title = pretty(name);
    if schema["restart"].as_bool().unwrap_or(false) {
        title.push_str("  (applies after restarting the session)");
    }
    text.append(&label(&title, &["heading"]));
    text.append(&label(schema["description"].as_str().unwrap_or(""), &["dim-label", "caption"]));
    row.append(&text);

    let reset = gtk::Button::with_label("Reset");
    reset.add_css_class("flat");
    reset.set_tooltip_text(Some("Back to the default"));
    reset.set_valign(gtk::Align::Center);
    reset.set_sensitive(is_set);
    {
        let (ctx, key) = (ctx.clone(), key.clone());
        reset.connect_clicked(move |_| apply(&ctx, vec![vec!["unset".into(), key.clone()]], rebuild_all));
    }

    // Applies `text` as the new value of this setting.
    let set_value = {
        let (ctx, key, reset) = (ctx.clone(), key.clone(), reset.clone());
        Rc::new(move |text: String| {
            reset.set_sensitive(true);
            apply(&ctx, vec![vec!["set".into(), key.clone(), text]], |_| {});
        })
    };

    let control: gtk::Widget = match schema["type"].as_str().unwrap_or("text") {
        "int" | "float" => {
            let is_float = schema["type"] == "float";
            let (min, max) = (schema["min"].as_f64().unwrap_or(0.0), schema["max"].as_f64().unwrap_or(100.0));
            let spin = gtk::SpinButton::with_range(min, max, if is_float { 0.05 } else { 1.0 });
            spin.set_digits(if is_float { 2 } else { 0 });
            spin.set_value(value.as_f64().unwrap_or(min));
            spin.set_valign(gtk::Align::Center);
            let on_change = debounced({
                let (spin, set_value) = (spin.clone(), set_value.clone());
                move || set_value(if is_float { format!("{:.2}", spin.value()) } else { format!("{}", spin.value() as i64) })
            });
            spin.connect_value_changed(move |_| on_change());
            spin.upcast()
        }
        "bool" => {
            let switch = gtk::Switch::builder().active(value.as_bool().unwrap_or(false)).valign(gtk::Align::Center).build();
            let set_value = set_value.clone();
            switch.connect_active_notify(move |s| set_value(s.is_active().to_string()));
            switch.upcast()
        }
        "choice" => {
            let choices: Vec<String> = schema["choices"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default();
            let refs: Vec<&str> = choices.iter().map(String::as_str).collect();
            let dropdown = gtk::DropDown::from_strings(&refs);
            dropdown.set_valign(gtk::Align::Center);
            dropdown.set_selected(choices.iter().position(|c| Some(c.as_str()) == value.as_str()).unwrap_or(0) as u32);
            let set_value = set_value.clone();
            dropdown.connect_selected_notify(move |d| {
                if let Some(c) = choices.get(d.selected() as usize) {
                    set_value(c.clone());
                }
            });
            dropdown.upcast()
        }
        "color" => {
            let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
            button.set_rgba(&hex_to_rgba(value.as_str().unwrap_or("#000000")));
            button.set_valign(gtk::Align::Center);
            let set_value = set_value.clone();
            let on_change = debounced({
                let button = button.clone();
                move || set_value(rgba_to_hex(&button.rgba()))
            });
            button.connect_rgba_notify(move |_| on_change());
            button.upcast()
        }
        "file" => {
            let entry = gtk::Entry::builder()
                .text(value.as_str().unwrap_or(""))
                .placeholder_text("No wallpaper — the background colour is used")
                .width_chars(40)
                .hexpand(true)
                .valign(gtk::Align::Center)
                .build();
            let initial = Rc::new(RefCell::new(entry.text().to_string()));
            let commit = {
                let (entry, set_value, initial) = (entry.clone(), set_value.clone(), initial.clone());
                Rc::new(move || {
                    let now = entry.text().to_string();
                    if now != *initial.borrow() {
                        *initial.borrow_mut() = now.clone();
                        set_value(now);
                    }
                })
            };
            let c = commit.clone();
            entry.connect_activate(move |_| c());
            let focus = gtk::EventControllerFocus::new();
            focus.connect_leave(move |_| commit());
            entry.add_controller(focus);
            let browse = gtk::Button::with_label("Browse…");
            browse.set_valign(gtk::Align::Center);
            {
                let (ctx, entry) = (ctx.clone(), entry.clone());
                browse.connect_clicked(move |_| {
                    let dialog = gtk::FileDialog::builder().title("Choose a wallpaper").modal(true).build();
                    let filter = gtk::FileFilter::new();
                    filter.set_name(Some("Images"));
                    for mime in ["image/png", "image/jpeg", "image/webp"] {
                        filter.add_mime_type(mime);
                    }
                    let filters = gio::ListStore::new::<gtk::FileFilter>();
                    filters.append(&filter);
                    dialog.set_filters(Some(&filters));
                    dialog.set_default_filter(Some(&filter));
                    let (set_value, entry) = (set_value.clone(), entry.clone());
                    dialog.open(Some(&ctx.window), None::<&gio::Cancellable>, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let text = path.display().to_string();
                                entry.set_text(&text);
                                set_value(text);
                            }
                        }
                    });
                });
            }
            let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
            row.append(&entry);
            row.append(&browse);
            row.upcast()
        }
        _ => {
            let entry = gtk::Entry::builder().text(value.as_str().unwrap_or("")).width_chars(28).valign(gtk::Align::Center).build();
            let initial = Rc::new(RefCell::new(entry.text().to_string()));
            let commit = {
                let (entry, set_value, initial) = (entry.clone(), set_value.clone(), initial.clone());
                Rc::new(move || {
                    let now = entry.text().to_string();
                    if now != *initial.borrow() {
                        *initial.borrow_mut() = now.clone();
                        set_value(now);
                    }
                })
            };
            let c = commit.clone();
            entry.connect_activate(move |_| c());
            let focus = gtk::EventControllerFocus::new();
            focus.connect_leave(move |_| commit());
            entry.add_controller(focus);
            entry.upcast()
        }
    };
    row.append(&control);
    row.append(&reset);
    row
}

fn settings_page(ctx: &Shared, section: &str, title: &str) -> gtk::ScrolledWindow {
    let (outer, body) = page(title);
    let settings: Vec<Value> = ctx.schema["settings"].as_array().cloned().unwrap_or_default();
    for s in settings.iter().filter(|s| s["section"] == section) {
        if section == "colors" && s["key"].as_str().is_some_and(|key| key.starts_with("pywal")) {
            continue;
        }
        body.append(&setting_row(ctx, s));
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }
    scrolled(&outer)
}

/// pywal's palette file, used by the compositor, Waybar and the Theme page.
fn wal_path() -> PathBuf {
    if let Some(path) = std::env::var_os("RUSTED_WAL_FILE") {
        return path.into();
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".cache/wal/colors.json")
}

/// The rice's palette generator (falls back to none when the rice is not installed).
fn palette_script() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RUSTED_PALETTE") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let path = base.join("rusted-rice/palette.py");
    path.is_file().then_some(path)
}

fn current_palette() -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(wal_path()).ok()?).ok()
}

fn hex_rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() < 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|v| v as f64 / 255.0);
    Some((channel(0)?, channel(2)?, channel(4)?))
}

fn swatch(hex: &str, name: &str) -> gtk::Box {
    let area = gtk::DrawingArea::new();
    area.set_content_width(34);
    area.set_content_height(34);
    let color = hex_rgb(hex).unwrap_or((0.5, 0.5, 0.5));
    area.set_draw_func(move |_, context, width, height| {
        context.set_source_rgb(color.0, color.1, color.2);
        context.rectangle(0.0, 0.0, width as f64, height as f64);
        let _ = context.fill();
    });
    let cell = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
    cell.append(&area);
    let text = label(hex, &["monospace", "caption"]);
    text.set_tooltip_text(Some(&format!("{name} {hex}")));
    cell.append(&text);
    cell.set_tooltip_text(Some(&format!("{name} {hex}")));
    cell
}

fn theme_page(ctx: &Shared) -> gtk::ScrolledWindow {
    let (outer, body) = page("Theme");
    body.append(&label(
        "The compositor, Waybar and this app follow the palette in ~/.cache/wal/colors.json. With pywal on, \
         the palette is derived from the current wallpaper (the rice's wallpaper helper regenerates it whenever \
         the wallpaper changes).",
        &["dim-label"],
    ));
    let settings: Vec<Value> = ctx.schema["settings"].as_array().cloned().unwrap_or_default();
    for entry in settings.iter().filter(|s| s["section"] == "colors") {
        if !entry["key"].as_str().is_some_and(|key| key.starts_with("pywal")) {
            continue;
        }
        body.append(&setting_row(ctx, entry));
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }

    let generate = gtk::Button::with_label("Regenerate palette from the current wallpaper");
    generate.add_css_class("suggested-action");
    generate.set_halign(gtk::Align::Start);
    match palette_script() {
        Some(script) => {
            let ctx = ctx.clone();
            generate.connect_clicked(move |_| {
                let (ctx, script) = (ctx.clone(), script.clone());
                glib::spawn_future_local(async move {
                    let result = gio::spawn_blocking(move || {
                        Command::new("python3").arg(&script).arg("generate").output()
                    })
                    .await;
                    match result {
                        Ok(Ok(out)) if out.status.success() => {
                            reload_dump(&ctx);
                            rebuild_all(&ctx);
                            set_status(&ctx, "Palette regenerated from the current wallpaper", false);
                        }
                        Ok(Ok(out)) => {
                            let error = String::from_utf8_lossy(&out.stderr).trim().to_string();
                            set_status(&ctx, &format!("palette: {}", if error.is_empty() { "failed".into() } else { error }), true);
                        }
                        Ok(Err(error)) => set_status(&ctx, &format!("cannot run palette.py: {error}"), true),
                        Err(error) => set_status(&ctx, &format!("palette task failed: {error:?}"), true),
                    }
                });
            });
        }
        None => {
            generate.set_sensitive(false);
            generate.set_tooltip_text(Some("palette.py not found — set RUSTED_PALETTE or install the rice helpers"));
        }
    }
    body.append(&generate);

    body.append(&label("Current palette", &["heading"]));
    match current_palette() {
        Some(palette) => {
            let flow = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .max_children_per_line(9)
                .min_children_per_line(2)
                .column_spacing(12)
                .row_spacing(8)
                .build();
            let mut entries: Vec<(String, String)> = Vec::new();
            for (name, value) in palette["special"].as_object().into_iter().flatten() {
                if let Some(hex) = value.as_str() {
                    entries.push((name.clone(), hex.to_string()));
                }
            }
            for index in 0..16 {
                let key = format!("color{index}");
                if let Some(hex) = palette["colors"][&key].as_str() {
                    entries.push((key, hex.to_string()));
                }
            }
            for (name, hex) in entries {
                flow.append(&swatch(&hex, &name));
            }
            body.append(&flow);
        }
        None => body.append(&label(
            "No palette yet — enable pywal and press the button, or pick a wallpaper first.",
            &["dim-label"],
        )),
    }
    scrolled(&outer)
}

/// Directories searched for the "choose a wallpaper" gallery.
fn wallpaper_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let mut dirs = Vec::new();
    if let Some(config) = config {
        dirs.push(config.join("rusted-rice/wallpapers"));
    }
    if let Some(home) = &home {
        dirs.push(home.join("Pictures/Wallpapers"));
    }
    dirs.push(PathBuf::from("/usr/share/backgrounds"));
    dirs.into_iter().filter(|d| d.is_dir()).collect()
}

fn wallpaper_images() -> Vec<PathBuf> {
    let image = |p: &std::path::Path| {
        p.is_file()
            && matches!(
                p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
                Some("png" | "jpg" | "jpeg" | "webp")
            )
    };
    let mut images = Vec::new();
    for dir in wallpaper_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut here: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| image(p)).collect();
        here.sort();
        images.extend(here);
    }
    images.truncate(60);
    images
}

fn wallpaper_page(ctx: &Shared) -> gtk::ScrolledWindow {
    let (outer, body) = page("Wallpaper");
    let settings: Vec<Value> = ctx.schema["settings"].as_array().cloned().unwrap_or_default();
    for entry in settings.iter().filter(|s| s["section"] == "wallpaper") {
        body.append(&setting_row(ctx, entry));
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }
    let current = setting_state(ctx, "wallpaper.path")
        .and_then(|state| state["value"].as_str().map(String::from))
        .unwrap_or_default();
    if !current.is_empty() && std::path::Path::new(&current).is_file() {
        let picture = gtk::Picture::for_filename(&current);
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_size_request(520, 240);
        picture.set_halign(gtk::Align::Start);
        body.append(&label("Current wallpaper", &["heading"]));
        body.append(&picture);
    }
    body.append(&label("Choose an image", &["heading"]));
    let images = wallpaper_images();
    if images.is_empty() {
        body.append(&label(
            "No wallpapers found in ~/.config/rusted-rice/wallpapers, ~/Pictures/Wallpapers or /usr/share/backgrounds.",
            &["dim-label"],
        ));
    } else {
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(6)
            .min_children_per_line(2)
            .column_spacing(10)
            .row_spacing(10)
            .build();
        for path in images {
            let picture = gtk::Picture::for_filename(&path);
            picture.set_content_fit(gtk::ContentFit::Cover);
            picture.set_size_request(150, 84);
            picture.set_can_shrink(true);
            let button = gtk::Button::builder().child(&picture).tooltip_text(path.display().to_string()).build();
            button.add_css_class("flat");
            if path.display().to_string() == current {
                button.add_css_class("suggested-action");
            }
            let (ctx, text) = (ctx.clone(), path.display().to_string());
            button.connect_clicked(move |_| {
                apply(&ctx, vec![vec!["set".into(), "wallpaper.path".into(), text.clone()]], rebuild_all)
            });
            flow.append(&button);
        }
        body.append(&flow);
    }
    scrolled(&outer)
}

// ---- keybinds ----

/// "mod+shift+s" from a key press; None for a bare modifier.
fn describe_key(keyval: gdk::Key, keycode: u32, state: gdk::ModifierType) -> Option<String> {
    let name = keyval.name()?;
    if matches!(name.as_str(), "Shift_L" | "Shift_R" | "Control_L" | "Control_R" | "Alt_L" | "Alt_R" | "Super_L" | "Super_R" | "Meta_L" | "Meta_R" | "ISO_Level3_Shift" | "Caps_Lock" | "Num_Lock") {
        return None;
    }
    // With Shift held GTK reports the shifted symbol ("exclam"); binds use the unshifted key.
    let base = if state.contains(gdk::ModifierType::SHIFT_MASK) {
        gdk::Display::default()
            .and_then(|d| d.map_keycode(keycode))
            .and_then(|keys| keys.into_iter().find(|(k, _)| k.level() == 0 && k.group() == 0).map(|(_, key)| key))
            .unwrap_or(keyval)
    } else {
        keyval
    };
    let mut parts: Vec<String> = Vec::new();
    if state.contains(gdk::ModifierType::SUPER_MASK) {
        parts.push("mod".into());
    }
    if state.contains(gdk::ModifierType::CONTROL_MASK) {
        parts.push("ctrl".into());
    }
    if state.contains(gdk::ModifierType::ALT_MASK) {
        parts.push("alt".into());
    }
    if state.contains(gdk::ModifierType::SHIFT_MASK) {
        parts.push("shift".into());
    }
    let key = base.name()?.to_string();
    parts.push(if key.len() == 1 { key.to_ascii_lowercase() } else { key });
    Some(parts.join("+"))
}

/// Dialog to add or edit a bind; `on_save(keys, action)` runs when the user confirms.
fn bind_dialog(ctx: &Shared, existing: Option<(String, String)>, on_save: impl Fn(String, String) + 'static) {
    let dialog = gtk::Window::builder().transient_for(&ctx.window).modal(true).title(if existing.is_some() { "Edit keybind" } else { "New keybind" }).default_width(520).build();
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();

    // Keys: free text plus a "record" button that captures the next combination.
    body.append(&label("Keys", &["heading"]));
    let keys_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
    let keys = gtk::Entry::builder().hexpand(true).placeholder_text("mod+shift+s").build();
    let record = gtk::ToggleButton::with_label("Record…");
    keys_row.append(&keys);
    keys_row.append(&record);
    body.append(&keys_row);
    body.append(&label("\"mod\" is Super on a real session. Press Record, then the combination.", &["dim-label", "caption"]));

    // Action: a list of known actions plus an argument for those that take one.
    body.append(&label("Action", &["heading"]));
    let actions: Vec<(String, String)> = ctx.schema["actions"]
        .as_array()
        .map(|a| a.iter().map(|x| (x["name"].as_str().unwrap_or("").to_string(), x["argument"].as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default();
    let names: Vec<&str> = actions.iter().map(|(n, _)| n.as_str()).collect();
    let action_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
    let dropdown = gtk::DropDown::from_strings(&names);
    let arg = gtk::Entry::builder().hexpand(true).build();
    action_row.append(&dropdown);
    action_row.append(&arg);
    body.append(&action_row);
    let hint = label("", &["dim-label", "caption"]);
    body.append(&hint);

    let update_arg = {
        let (arg, hint, actions) = (arg.clone(), hint.clone(), actions.clone());
        move |idx: usize| {
            let kind = actions.get(idx).map(|(_, a)| a.as_str()).unwrap_or("");
            arg.set_visible(!kind.is_empty());
            arg.set_placeholder_text(Some(if kind == "N" { "workspace 1-9" } else { "command, e.g. foot or @launcher" }));
            hint.set_text(match kind {
                "CMD" => "Command to run. @terminal, @launcher, @browser, @file_manager and @lock use your default apps.",
                "N" => "Workspace number, 1 to 9.",
                _ => "",
            });
        }
    };
    {
        let update_arg = update_arg.clone();
        dropdown.connect_selected_notify(move |d| update_arg(d.selected() as usize));
    }

    if let Some((k, a)) = &existing {
        keys.set_text(k);
        let (name, argument) = a.split_once(':').unwrap_or((a.as_str(), ""));
        if let Some(i) = actions.iter().position(|(n, _)| n == name) {
            dropdown.set_selected(i as u32);
        }
        arg.set_text(argument);
    }
    update_arg(dropdown.selected() as usize);

    // Recording: while the toggle is on, the next real key press fills the entry.
    let recorder = gtk::EventControllerKey::new();
    recorder.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let (keys, record) = (keys.clone(), record.clone());
        recorder.connect_key_pressed(move |_, keyval, keycode, state| {
            if !record.is_active() {
                return glib::Propagation::Proceed;
            }
            if let Some(combo) = describe_key(keyval, keycode, state) {
                keys.set_text(&combo);
                record.set_active(false);
            }
            glib::Propagation::Stop
        });
    }
    dialog.add_controller(recorder);

    let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).margin_top(8).build();
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&save);
    body.append(&buttons);
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let dialog = dialog.clone();
        let (keys, dropdown, arg, actions) = (keys.clone(), dropdown.clone(), arg.clone(), actions.clone());
        save.connect_clicked(move |_| {
            let name = actions.get(dropdown.selected() as usize).map(|(n, _)| n.clone()).unwrap_or_default();
            let argument = arg.text().to_string();
            let action = if argument.trim().is_empty() { name } else { format!("{name}:{}", argument.trim()) };
            let keys = keys.text().trim().to_string();
            if !keys.is_empty() {
                on_save(keys, action);
            }
            dialog.close();
        });
    }
    dialog.set_child(Some(&body));
    dialog.present();
}

fn keybinds_page(ctx: &Shared) -> gtk::Box {
    let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(24).margin_end(24).build();
    let head = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(12).build();
    head.append(&label("Keybinds", &["title-2"]));
    let add = gtk::Button::with_label("Add keybind");
    add.set_hexpand(true);
    add.set_halign(gtk::Align::End);
    head.append(&add);
    outer.append(&head);
    {
        let ctx = ctx.clone();
        add.connect_clicked(move |_| {
            let ctx2 = ctx.clone();
            bind_dialog(&ctx, None, move |keys, action| apply(&ctx2, vec![vec!["bind".into(), keys, action]], |_| {}));
        });
    }

    let search = gtk::SearchEntry::builder().placeholder_text("Filter by keys or action").build();
    outer.append(&search);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let mut binds: Vec<Value> = ctx.dump.borrow()["binds"].as_array().cloned().unwrap_or_default();
    // Your own changes first, then the disabled defaults, then the rest.
    let rank = |b: &Value| match b["tag"].as_str() {
        Some("custom") => 0,
        Some("disabled") => 1,
        _ => 2,
    };
    binds.sort_by_key(|b| (rank(b), b["keys"].as_str().unwrap_or("").to_lowercase()));
    // Row text used by the filter, by position.
    let haystacks: Vec<String> = binds.iter().map(|b| format!("{} {}", b["keys"].as_str().unwrap_or(""), b["action"].as_str().unwrap_or("")).to_lowercase()).collect();
    for b in &binds {
        let (keys, action, tag) = (b["keys"].as_str().unwrap_or("").to_string(), b["action"].as_str().unwrap_or("").to_string(), b["tag"].as_str().unwrap_or("").to_string());
        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(12).margin_top(6).margin_bottom(6).margin_start(8).margin_end(8).build();
        let key_label = label(&keys, &["monospace"]);
        key_label.set_width_chars(26);
        let action_label = label(&action, &[]);
        action_label.set_hexpand(true);
        action_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        if tag == "disabled" {
            action_label.add_css_class("dim-label");
        }
        row.append(&key_label);
        row.append(&action_label);
        row.append(&label(&tag, &["dim-label", "caption"]));

        let edit = gtk::Button::with_label("Edit");
        edit.set_tooltip_text(Some("Edit"));
        {
            let (ctx, keys, action, tag) = (ctx.clone(), keys.clone(), action.clone(), tag.clone());
            edit.connect_clicked(move |_| {
                let (ctx2, old_keys, old_tag) = (ctx.clone(), keys.clone(), tag.clone());
                bind_dialog(&ctx, Some((keys.clone(), action.clone())), move |new_keys, new_action| {
                    let mut commands = Vec::new();
                    if new_keys != old_keys {
                        // Moving a bind: drop the old keys (default binds are switched off).
                        let mut c = vec!["unbind".to_string(), old_keys.clone()];
                        if old_tag == "default" {
                            c.push("--disable".into());
                        }
                        commands.push(c);
                    }
                    commands.push(vec!["bind".into(), new_keys, new_action]);
                    apply(&ctx2, commands, rebuild_all);
                });
            });
        }
        row.append(&edit);

        // custom -> remove (a default bind on the same keys comes back); default -> disable;
        // disabled -> enable.
        let (text, tip, args): (&str, &str, Vec<String>) = match tag.as_str() {
            "default" => ("Disable", "Turn this default bind off", vec!["unbind".into(), keys.clone(), "--disable".into()]),
            "disabled" => ("Enable", "Restore the default", vec!["unbind".into(), keys.clone()]),
            _ => ("Remove", "Remove this bind", vec!["unbind".into(), keys.clone()]),
        };
        let remove = gtk::Button::with_label(text);
        remove.set_tooltip_text(Some(tip));
        {
            let ctx = ctx.clone();
            remove.connect_clicked(move |_| apply(&ctx, vec![args.clone()], rebuild_all));
        }
        row.append(&remove);
        list.append(&row);
    }
    {
        let (filter_search, list2) = (search.clone(), list.clone());
        list.set_filter_func(move |row| {
            let needle = filter_search.text().to_lowercase();
            needle.is_empty() || haystacks.get(row.index() as usize).is_some_and(|h| h.contains(&needle))
        });
        search.connect_search_changed(move |_| list2.invalidate_filter());
    }
    outer.append(&scrolled(&list));
    outer
}

// ---- autostart ----

fn autostart_page(ctx: &Shared) -> gtk::Box {
    let (outer, body) = page("Autostart");
    body.append(&label("Programs started once when the compositor starts. Changes apply at the next start.", &["dim-label"]));
    let entries: Vec<String> = ctx.dump.borrow()["autostart"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    for (i, entry) in entries.iter().enumerate() {
        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(12).margin_top(6).margin_bottom(6).margin_start(8).margin_end(8).build();
        let text = label(entry, &["monospace"]);
        text.set_hexpand(true);
        row.append(&text);
        let remove = gtk::Button::with_label("Remove");
        let ctx = ctx.clone();
        remove.connect_clicked(move |_| apply(&ctx, vec![vec!["autostart".into(), "remove".into(), (i + 1).to_string()]], rebuild_all));
        row.append(&remove);
        list.append(&row);
    }
    if entries.is_empty() {
        body.append(&label("(nothing yet)", &["dim-label"]));
    }
    body.append(&list);

    let add_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).margin_top(12).build();
    let entry = gtk::Entry::builder().hexpand(true).placeholder_text("command, e.g. waybar").build();
    let add = gtk::Button::with_label("Add");
    add_row.append(&entry);
    add_row.append(&add);
    body.append(&add_row);
    let submit = {
        let (ctx, entry) = (ctx.clone(), entry.clone());
        Rc::new(move || {
            let cmd = entry.text().trim().to_string();
            if !cmd.is_empty() {
                apply(&ctx, vec![vec!["autostart".into(), "add".into(), cmd]], rebuild_all);
            }
        })
    };
    let s = submit.clone();
    add.connect_clicked(move |_| s());
    entry.connect_activate(move |_| submit());
    outer
}

// ---- assembling the window ----

fn rebuild_all(ctx: &Shared) {
    let current = ctx.stack.visible_child_name();
    while let Some(child) = ctx.stack.first_child() {
        ctx.stack.remove(&child);
    }
    for (section, title) in SECTIONS {
        let page = match *section {
            "wallpaper" => wallpaper_page(ctx),
            "theme" => theme_page(ctx),
            _ => settings_page(ctx, section, title),
        };
        ctx.stack.add_titled(&page, Some(section), title);
    }
    ctx.stack.add_titled(&scrolled(&keybinds_page(ctx)), Some("keybinds"), "Keybinds");
    ctx.stack.add_titled(&scrolled(&autostart_page(ctx)), Some("autostart"), "Autostart");
    if let Some(name) = current {
        if ctx.stack.child_by_name(&name).is_some() {
            ctx.stack.set_visible_child_name(&name);
        }
    }
}

fn build_ui(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder().application(app).title("rusted-wm settings").default_width(980).default_height(680).build();
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    window.set_child(Some(&root));

    let schema = run_json(&["--dump-schema"]);
    let dump = run_json(&["config", "dump"]);
    let (schema, dump) = match (schema, dump) {
        (Ok(s), Ok(d)) => (s, d),
        (Err(e), _) | (_, Err(e)) => {
            let msg = label(&format!("Cannot read the settings from rusted-wm:\n\n{e}\n\nSet RUSTED_WM to the rusted-wm binary, or install it next to this app."), &["title-4"]);
            msg.set_margin_top(24);
            msg.set_margin_start(24);
            root.append(&msg);
            window.present();
            return;
        }
    };

    let stack = gtk::Stack::builder().hexpand(true).vexpand(true).transition_type(gtk::StackTransitionType::Crossfade).build();
    let sidebar = gtk::StackSidebar::new();
    sidebar.set_stack(&stack);
    sidebar.set_width_request(190);
    let main = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).vexpand(true).build();
    main.append(&sidebar);
    main.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    main.append(&stack);
    root.append(&main);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let status = gtk::Label::builder().xalign(0.0).margin_top(6).margin_bottom(6).margin_start(12).margin_end(12).ellipsize(gtk::pango::EllipsizeMode::End).build();
    root.append(&status);

    let ctx: Shared = Rc::new(Ctx { window: window.clone(), stack, status, schema, dump: RefCell::new(dump) });
    let problems: Vec<String> = ctx.dump.borrow()["problems"].as_array().map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()).unwrap_or_default();
    let path = ctx.dump.borrow()["path"].as_str().unwrap_or("").to_string();
    if problems.is_empty() {
        set_status(&ctx, &format!("Config file: {path} — changes are saved and applied immediately"), false);
    } else {
        set_status(&ctx, &format!("{} problem(s) in {path}: {}", problems.len(), problems[0]), true);
    }
    rebuild_all(&ctx);
    if let Some(page) = std::env::args().skip(1).find_map(|a| a.strip_prefix("--page=").map(String::from)) {
        if ctx.stack.child_by_name(&page).is_some() {
            ctx.stack.set_visible_child_name(&page);
        }
    }
    window.present();
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id("org.rusted.Settings").build();
    app.connect_activate(build_ui);
    // `--page=NAME` is ours; keep it out of GTK's own argument parsing.
    let args: Vec<String> = std::env::args().filter(|arg| !arg.starts_with("--page=")).collect();
    app.run_with_args(&args)
}
