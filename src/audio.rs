//! Volume and mute actions (keyboard knobs / media keys) with an on-screen notification.
//!
//! Tries PipeWire's `wpctl` first and falls back to PulseAudio's `pactl` (also what
//! pipewire-pulse answers). Everything runs on a short-lived thread so a slow audio server can
//! never stall the compositor; failures are logged with the reason.

use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCmd {
    VolumeUp,
    VolumeDown,
    VolumeMute,
    MicMute,
}

#[derive(Debug, Clone)]
pub struct AudioSettings {
    /// Percent per key press / knob click.
    pub step: u32,
    /// Upper limit in percent (above 100 = software boost).
    pub max_volume: u32,
    /// Show a notification (needs a daemon such as mako) with the new level.
    pub notify: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { step: 5, max_volume: 100, notify: true }
    }
}

/// What the audio server reports after a change.
#[derive(Debug, PartialEq)]
struct Level {
    percent: u32,
    muted: bool,
}

pub fn run(cmd: AudioCmd, settings: AudioSettings) {
    std::thread::spawn(move || match apply(cmd, &settings) {
        Ok(level) => {
            tracing::debug!(?cmd, ?level, "audio");
            if settings.notify {
                notify(cmd, &level);
            }
        }
        Err(e) => tracing::warn!(?cmd, "audio control failed: {e}"),
    });
}

fn apply(cmd: AudioCmd, s: &AudioSettings) -> Result<Level, String> {
    let wpctl = apply_wpctl(cmd, s);
    match wpctl {
        Ok(level) => Ok(level),
        Err(w) => apply_pactl(cmd, s).map_err(|p| format!("wpctl: {w}; pactl: {p} (is PipeWire/PulseAudio running in this session?)")),
    }
}

fn apply_wpctl(cmd: AudioCmd, s: &AudioSettings) -> Result<Level, String> {
    let sink = "@DEFAULT_AUDIO_SINK@";
    let source = "@DEFAULT_AUDIO_SOURCE@";
    let limit = format!("{:.2}", s.max_volume as f64 / 100.0);
    match cmd {
        AudioCmd::VolumeUp => {
            run_cmd("wpctl", &["set-mute", sink, "0"])?;
            run_cmd("wpctl", &["set-volume", "-l", &limit, sink, &format!("{}%+", s.step)])?;
        }
        AudioCmd::VolumeDown => {
            run_cmd("wpctl", &["set-mute", sink, "0"])?;
            run_cmd("wpctl", &["set-volume", sink, &format!("{}%-", s.step)])?;
        }
        AudioCmd::VolumeMute => {
            run_cmd("wpctl", &["set-mute", sink, "toggle"])?;
        }
        AudioCmd::MicMute => {
            run_cmd("wpctl", &["set-mute", source, "toggle"])?;
        }
    }
    let target = if cmd == AudioCmd::MicMute { source } else { sink };
    parse_wpctl(&run_cmd("wpctl", &["get-volume", target])?)
}

/// "Volume: 0.45" or "Volume: 0.45 [MUTED]".
fn parse_wpctl(out: &str) -> Result<Level, String> {
    let value = out
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<f64>().ok())
        .ok_or_else(|| format!("unexpected wpctl output {out:?}"))?;
    Ok(Level { percent: (value * 100.0).round() as u32, muted: out.contains("[MUTED]") })
}

fn apply_pactl(cmd: AudioCmd, s: &AudioSettings) -> Result<Level, String> {
    let (volume_kind, sink) = match cmd {
        AudioCmd::MicMute => ("source", "@DEFAULT_SOURCE@"),
        _ => ("sink", "@DEFAULT_SINK@"),
    };
    match cmd {
        AudioCmd::VolumeUp | AudioCmd::VolumeDown => {
            let current = parse_pactl_volume(&run_cmd("pactl", &["get-sink-volume", sink])?)?;
            let new = if cmd == AudioCmd::VolumeUp {
                (current + s.step).min(s.max_volume)
            } else {
                current.saturating_sub(s.step)
            };
            run_cmd("pactl", &["set-sink-mute", sink, "0"])?;
            run_cmd("pactl", &["set-sink-volume", sink, &format!("{new}%")])?;
        }
        AudioCmd::VolumeMute | AudioCmd::MicMute => {
            run_cmd("pactl", &[&format!("set-{volume_kind}-mute"), sink, "toggle"])?;
        }
    }
    let percent = parse_pactl_volume(&run_cmd("pactl", &[&format!("get-{volume_kind}-volume"), sink])?)?;
    let muted = run_cmd("pactl", &[&format!("get-{volume_kind}-mute"), sink])?.contains("yes");
    Ok(Level { percent, muted })
}

/// "Volume: front-left: 29491 /  45% / -20.00 dB, ..." -> 45
fn parse_pactl_volume(out: &str) -> Result<u32, String> {
    out.split('/')
        .filter_map(|part| part.trim().strip_suffix('%'))
        .find_map(|n| n.trim().parse().ok())
        .ok_or_else(|| format!("unexpected pactl output {out:?}"))
}

fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program).args(args).output().map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{program} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Replace-in-place notification with a progress bar (mako / dunst understand these hints).
fn notify(cmd: AudioCmd, level: &Level) {
    let (title, body, value) = match (cmd, level.muted) {
        (AudioCmd::MicMute, true) => ("Microphone", "muted".to_string(), 0),
        (AudioCmd::MicMute, false) => ("Microphone", "on".to_string(), 100),
        (_, true) => ("Volume", "muted".to_string(), 0),
        (_, false) => ("Volume", format!("{}%", level.percent), level.percent.min(100)),
    };
    let _ = Command::new("notify-send")
        .args(["-a", "rusted-wm", "-t", "1200", "-h", "string:x-canonical-private-synchronous:rusted-audio"])
        .args(["-h", &format!("int:value:{value}")])
        .args([title, &body])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wpctl_output() {
        assert_eq!(parse_wpctl("Volume: 0.45\n").unwrap(), Level { percent: 45, muted: false });
        assert_eq!(parse_wpctl("Volume: 1.00 [MUTED]\n").unwrap(), Level { percent: 100, muted: true });
        assert!(parse_wpctl("nonsense").is_err());
    }

    #[test]
    fn parses_pactl_output() {
        let out = "Volume: front-left: 29491 /  45% / -20.00 dB,   front-right: 29491 /  45% / -20.00 dB\n        balance 0.00\n";
        assert_eq!(parse_pactl_volume(out).unwrap(), 45);
        assert!(parse_pactl_volume("nothing").is_err());
    }
}
