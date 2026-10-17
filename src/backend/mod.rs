//! Output backends. `winit` runs nested inside another compositor (development); a real
//! TTY/DRM backend lives in `tty` (Phase 5b).

pub mod winit;
