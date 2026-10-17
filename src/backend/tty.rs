//! Real-session backend: DRM/KMS output, libinput input, libseat session (VT switching).
//!
//! Scope for now: every connected connector on the primary GPU (placed left to right), software
//! cursor. Hotplug and multi-GPU come later. Start with `rusted-wm --tty` on a free VT (Ctrl+Alt+F3),
//! or just run it with no `$WAYLAND_DISPLAY`/`$DISPLAY` set.

use std::{cell::RefCell, rc::Rc, time::Duration};

use smithay::{
    desktop::utils::OutputPresentationFeedback,
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent,
        },
        egl::{EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::{
                memory::MemoryRenderBufferRenderElement,
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind,
            },
            gles::GlesRenderer,
            ImportDma,
        },
        session::{
            libseat::{LibSeatSession, LibSeatSessionNotifier},
            Event as SessionEvent, Session,
        },
        udev::{all_gpus, primary_gpu},
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            LoopHandle,
        },
        drm::control::{connector, crtc, Device as ControlDevice, ModeTypeFlags},
        input::Libinput,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        rustix::fs::OFlags,
        wayland_server::{backend::GlobalId, DisplayHandle},
    },
    utils::{DeviceFd, Logical, Monotonic, Physical, Point, Transform},
    wayland::compositor::with_states,
};

use std::collections::BTreeMap;

use crate::{config::{parse_output_mode, OutputSettings}, control, cursor::CursorTheme, state::State};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);
const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Xrgb8888, Fourcc::Argb8888];

/// Per-frame user data: presentation-time feedback to send when the frame hits the screen.
type Compositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,
    DrmDeviceFd,
>;

smithay::backend::renderer::element::render_elements! {
    pub TtyElement<=GlesRenderer>;
    Surface = WaylandSurfaceRenderElement<GlesRenderer>,
    Scene = super::SceneElement,
    Memory = MemoryRenderBufferRenderElement<GlesRenderer>,
}

/// One connected display: its output, CRTC and scan-out queue.
struct Head {
    output: Output,
    global: GlobalId,
    connector: connector::Handle,
    crtc: crtc::Handle,
    compositor: Compositor,
    frame_pending: bool,
}

struct Gpu {
    drm: DrmDevice,
    gbm: GbmDevice<DrmDeviceFd>,
    dh: DisplayHandle,
    renderer: GlesRenderer,
    cursor: CursorTheme,
    heads: Vec<Head>,
    active: bool,
}

pub struct Tty {
    pub session: LibSeatSession,
    session_notifier: LibSeatSessionNotifier,
    seat_name: String,
    drm_notifier: DrmDeviceNotifier,
    gpu: Gpu,
}

type BoxError = Box<dyn std::error::Error>;

impl Tty {
    pub fn new(dh: &DisplayHandle, outputs: &BTreeMap<String, OutputSettings>) -> Result<Self, BoxError> {
        let (mut session, session_notifier) =
            LibSeatSession::new().map_err(|e| format!("cannot open a libseat session (is seatd/elogind running, and are you on a real VT?): {e}"))?;
        let seat_name = session.seat();
        tracing::info!(seat = %seat_name, "session opened");

        let path = primary_gpu(&seat_name)?
            .or_else(|| all_gpus(&seat_name).ok().and_then(|g| g.into_iter().next()))
            .ok_or("no GPU found")?;
        tracing::info!(gpu = %path.display(), "using GPU");
        let fd = session
            .open(&path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)
            .map_err(|e| format!("cannot open {}: {e:?}", path.display()))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));

        let (mut drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
        let gbm = GbmDevice::new(fd.clone())?;
        // Safety: the gbm device outlives the EGL display (both are owned by `Gpu`/the compositors).
        let egl = unsafe { EGLDisplay::new(gbm.clone())? };
        let context = EGLContext::new(&egl)?;
        let renderer = unsafe { GlesRenderer::new(context)? };

        // Every connected connector with a mode, each on its own CRTC.
        let mut heads: Vec<Head> = Vec::new();
        for conn in connected_connectors(&drm) {
            match make_head(&mut drm, &gbm, &renderer, dh, &conn, &heads, outputs) {
                Ok(Some(head)) => heads.push(head),
                Ok(None) => {}
                Err(e) => tracing::warn!(connector = ?conn.interface(), "cannot use connector: {e}"),
            }
        }
        if heads.is_empty() {
            return Err("no connected display found".into());
        }

        Ok(Self {
            session,
            session_notifier,
            seat_name,
            drm_notifier,
            gpu: Gpu { drm, gbm, dh: dh.clone(), renderer, cursor: CursorTheme::load(), heads, active: true },
        })
    }

    /// The outputs, in the order they are laid out (left to right).
    pub fn outputs(&self) -> Vec<Output> {
        self.gpu.heads.iter().map(|h| h.output.clone()).collect()
    }

    pub fn register(self, handle: &LoopHandle<'static, State>) -> Result<(), BoxError> {
        let Tty { session, session_notifier, seat_name, drm_notifier, gpu } = self;
        let gpu = Rc::new(RefCell::new(gpu));

        // Input devices.
        let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session));
        libinput.udev_assign_seat(&seat_name).map_err(|_| "libinput: cannot assign seat")?;
        handle.insert_source(LibinputInputBackend::new(libinput.clone()), |event, _, state| {
            state.handle_input(event);
        })?;

        // VT switches: stop using the GPU and input while another session owns them.
        handle.insert_source(session_notifier, {
            let gpu = gpu.clone();
            let mut libinput = libinput.clone();
            move |event, _, _state| {
                let mut gpu = gpu.borrow_mut();
                match event {
                    SessionEvent::PauseSession => {
                        tracing::info!("session paused");
                        libinput.suspend();
                        gpu.drm.pause();
                        gpu.active = false;
                    }
                    SessionEvent::ActivateSession => {
                        tracing::info!("session resumed");
                        if libinput.resume().is_err() {
                            tracing::error!("libinput failed to resume");
                        }
                        if let Err(e) = gpu.drm.activate(true) {
                            tracing::error!("failed to re-activate the DRM device: {e}");
                        }
                        for head in &mut gpu.heads {
                            head.compositor.reset_buffers();
                            head.frame_pending = false;
                        }
                        gpu.active = true;
                    }
                }
            }
        })?;

        // Page flips completed (one event per CRTC).
        handle.insert_source(drm_notifier, {
            let gpu = gpu.clone();
            move |event, _, state| match event {
                DrmEvent::VBlank(crtc) => {
                    let mut gpu = gpu.borrow_mut();
                    let Some(idx) = gpu.heads.iter().position(|h| h.crtc == crtc) else { return };
                    match gpu.heads[idx].compositor.frame_submitted() {
                        Ok(Some(Some(mut feedback))) => {
                            let output = gpu.heads[idx].output.clone();
                            feedback.presented::<_, Monotonic>(
                                state.clock.now(),
                                super::refresh_of(&output),
                                0,
                                wp_presentation_feedback::Kind::Vsync | wp_presentation_feedback::Kind::HwCompletion,
                            );
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!("frame_submitted: {e}"),
                    }
                    gpu.heads[idx].frame_pending = false;
                    render(&mut gpu, state, idx);
                }
                DrmEvent::Error(e) => tracing::error!("drm error: {e}"),
            }
        })?;

        // Fallback tick: starts the first frame and retries when nothing was queued.
        handle.insert_source(Timer::from_duration(FRAME_INTERVAL), {
            let gpu = gpu.clone();
            let mut last_scan = std::time::Instant::now();
            move |_, _, state| {
                let mut gpu = gpu.borrow_mut();
                // Hotplug: look for (un)plugged monitors a couple of times a second.
                if gpu.active && last_scan.elapsed() >= RESCAN_INTERVAL {
                    last_scan = std::time::Instant::now();
                    rescan(&mut gpu, state);
                }
                for idx in 0..gpu.heads.len() {
                    render(&mut gpu, state, idx);
                }
                TimeoutAction::ToDuration(FRAME_INTERVAL)
            }
        })?;
        Ok(())
    }
}

fn connected_connectors(drm: &DrmDevice) -> Vec<connector::Info> {
    let Ok(resources) = drm.resource_handles() else { return Vec::new() };
    resources
        .connectors()
        .iter()
        // Not forcing a probe: the kernel updates connector state itself on hotplug events.
        .filter_map(|&h| drm.get_connector(h, false).ok())
        .filter(|c| c.state() == connector::State::Connected)
        .collect()
}

/// Bring up a connector: pick a free CRTC, create its output and scan-out queue. `None` if the
/// connector has no mode or no CRTC is free.
fn make_head(
    drm: &mut DrmDevice,
    gbm: &GbmDevice<DrmDeviceFd>,
    renderer: &GlesRenderer,
    dh: &DisplayHandle,
    conn: &connector::Info,
    existing: &[Head],
    outputs: &BTreeMap<String, OutputSettings>,
) -> Result<Option<Head>, BoxError> {
    let name = format!("{}-{}", conn.interface().as_str(), conn.interface_id());
    let wanted = outputs.get(&name).and_then(|settings| {
        if settings.mode.trim().is_empty() { return None; }
        match parse_output_mode(&settings.mode) {
            Ok(mode) => Some(mode),
            Err(e) => {
                tracing::warn!(%name, mode = %settings.mode, "config: {e}");
                None
            }
        }
    });
    let available: Vec<(i32, i32, u32)> = conn
        .modes()
        .iter()
        .map(|m| {
            let (w, h) = m.size();
            (w as i32, h as i32, m.vrefresh())
        })
        .collect();
    let configured = wanted.and_then(|(w, h, hz)| {
        choose_mode(&available, w, h, hz).map(|index| {
            tracing::info!(%name, width = w, height = h, refresh = ?hz, "mode from [outputs]");
            conn.modes()[index]
        })
    });
    if wanted.is_some() && configured.is_none() {
        tracing::warn!(%name, "requested output mode is not offered by the monitor; using the preferred mode");
    }
    let Some(mode) = configured.or_else(|| {
        conn.modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or(conn.modes().first())
            .copied()
    }) else {
        return Ok(None);
    };
    let resources = drm.resource_handles()?;
    let used = |c: &crtc::Handle| existing.iter().any(|h| &h.crtc == c);
    let current = conn.current_encoder().and_then(|e| drm.get_encoder(e).ok()).and_then(|e| e.crtc());
    let crtc: Option<crtc::Handle> = current.filter(|c| !used(c)).or_else(|| {
        conn.encoders()
            .iter()
            .filter_map(|&e| drm.get_encoder(e).ok())
            .flat_map(|e| resources.filter_crtcs(e.possible_crtcs()))
            .find(|c| !used(c))
    });
    let Some(crtc) = crtc else {
        tracing::warn!(connector = ?conn.interface(), "no free CRTC, skipping");
        return Ok(None);
    };

    let (w_mm, h_mm) = conn.size().unwrap_or((0, 0));
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (w_mm as i32, h_mm as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "rusted".into(),
            model: name.clone(),
        },
    );
    let global = output.create_global::<State>(dh);
    let output_mode = OutputMode::from(mode);
    // Positions are assigned by `State::layout_outputs` once the config (scale) is known.
    output.change_current_state(Some(output_mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(output_mode);
    tracing::info!(%name, mode = ?output_mode, "output ready");

    let surface = drm.create_surface(crtc, mode, &[conn.handle()])?;
    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), None);
    let compositor: Compositor = DrmCompositor::new(
        &output,
        surface,
        None,
        allocator,
        exporter,
        COLOR_FORMATS,
        renderer.dmabuf_formats(),
        drm.cursor_size(),
        Some(gbm.clone()),
    )?;
    Ok(Some(Head { output, global, connector: conn.handle(), crtc, compositor, frame_pending: false }))
}

/// Pick a mode matching `w`x`h` (and the closest refresh within 1.5 Hz when one is requested).
/// Without a refresh rate, the fastest mode of that size wins. Modes are `(width, height, Hz)`.
fn choose_mode(available: &[(i32, i32, u32)], w: i32, h: i32, refresh: Option<f64>) -> Option<usize> {
    let matching: Vec<usize> = available
        .iter()
        .enumerate()
        .filter(|(_, m)| m.0 == w && m.1 == h)
        .map(|(i, _)| i)
        .collect();
    match refresh {
        Some(hz) => matching
            .into_iter()
            .filter(|&i| (available[i].2 as f64 - hz).abs() <= 1.5)
            .min_by(|&a, &b| {
                (available[a].2 as f64 - hz).abs().total_cmp(&(available[b].2 as f64 - hz).abs())
            }),
        None => matching.into_iter().max_by_key(|&i| available[i].2),
    }
}

/// React to monitors being plugged in or out. The last remaining head is never dropped.
fn rescan(gpu: &mut Gpu, state: &mut State) {
    let connected = connected_connectors(&gpu.drm);

    let mut i = 0;
    while i < gpu.heads.len() {
        let still_there = connected.iter().any(|c| c.handle() == gpu.heads[i].connector);
        if still_there || gpu.heads.len() == 1 {
            i += 1;
            continue;
        }
        let head = gpu.heads.remove(i);
        tracing::info!(output = %head.output.name(), "monitor unplugged");
        state.remove_output(&head.output);
        gpu.dh.remove_global::<State>(head.global);
    }

    for conn in connected {
        if gpu.heads.iter().any(|h| h.connector == conn.handle()) {
            continue;
        }
        match make_head(&mut gpu.drm, &gpu.gbm, &gpu.renderer, &gpu.dh, &conn, &gpu.heads, &state.config.outputs) {
            Ok(Some(head)) => {
                tracing::info!(output = %head.output.name(), "monitor plugged in");
                state.add_output(head.output.clone());
                gpu.heads.push(head);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(connector = ?conn.interface(), "cannot use new connector: {e}"),
        }
    }
}

/// Draw and queue one frame on head `idx` if the GPU is ours and the head is idle.
fn render(gpu: &mut Gpu, state: &mut State, idx: usize) {
    if state.has_pending_screencopy() && gpu.active {
        state.serve_screencopy(|state, output| super::render_to_rgba(&mut gpu.renderer, state, output));
    }
    // Screenshots are rendered offscreen, so they work even while another VT is in front.
    if let Some((path, mon, stream)) = state.pending_shot.take() {
        let target = state.outputs[mon].output.clone();
        let reply = match super::screenshot(&mut gpu.renderer, state, &target, &path) {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("error: {e}"),
        };
        control::answer(stream, &reply);
    }
    if !gpu.active || gpu.heads[idx].frame_pending {
        return;
    }
    let output = gpu.heads[idx].output.clone();
    let geo = state.space.output_geometry(&output).unwrap_or_default();
    let scene = super::scene_elements(state, &mut gpu.renderer, &output);
    let background = super::clear_color(state);
    let scale = state.config.general.scale;
    let pointer: Point<f64, Logical> = state.seat.get_pointer().unwrap().current_location();
    // Pointer relative to this output; the cursor is only drawn on the output it is on.
    let local = pointer - geo.loc.to_f64();
    let on_this_output = geo.to_f64().contains(pointer);

    let mut elements: Vec<TtyElement> = Vec::new();
    if on_this_output {
        match &state.cursor_status {
            CursorImageStatus::Hidden => {}
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |s| {
                    s.data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|d| d.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let at: Point<i32, Physical> = (local - hotspot.to_f64()).to_physical_precise_round(scale);
                elements.extend(
                    render_elements_from_surface_tree(&mut gpu.renderer, surface, at, scale, 1.0, Kind::Cursor)
                        .into_iter()
                        .map(TtyElement::Surface),
                );
            }
            CursorImageStatus::Named(icon) => {
                let image = gpu.cursor.get(*icon);
                let at = Point::<f64, Physical>::from((
                    local.x * scale - image.hotspot.x as f64,
                    local.y * scale - image.hotspot.y as f64,
                ));
                match MemoryRenderBufferRenderElement::from_buffer(&mut gpu.renderer, at, &image.buffer, None, None, None, Kind::Cursor) {
                    Ok(e) => elements.push(TtyElement::Memory(e)),
                    Err(e) => tracing::warn!("cursor render: {e}"),
                }
            }
        }
    }
    elements.extend(scene.into_iter().map(TtyElement::Scene));

    let head = &mut gpu.heads[idx];
    match head.compositor.render_frame(&mut gpu.renderer, &elements, background, FrameFlags::DEFAULT) {
        Ok(result) => {
            if !result.is_empty {
                let feedback = super::collect_feedback(state, &output, &result.states);
                match head.compositor.queue_frame(Some(feedback)) {
                    Ok(()) => head.frame_pending = true,
                    Err(e) => tracing::warn!("queue_frame: {e}"),
                }
            }
        }
        Err(e) => tracing::warn!("render_frame: {e}"),
    }

    state.send_frames(&output, std::time::UNIX_EPOCH.elapsed().unwrap_or_default());
    let _ = state.display_handle.flush_clients();
}

#[cfg(test)]
mod tests {
    use super::choose_mode;

    #[test]
    fn mode_choice_matches_size_and_refresh() {
        let modes = [(1920, 1080, 60), (1920, 1080, 75), (1280, 720, 60)];
        assert_eq!(choose_mode(&modes, 1920, 1080, Some(75.0)), Some(1));
        assert_eq!(choose_mode(&modes, 1920, 1080, Some(60.0)), Some(0));
        assert_eq!(choose_mode(&modes, 1920, 1080, Some(59.94)), Some(0));
        assert_eq!(choose_mode(&modes, 1920, 1080, None), Some(1), "without a rate, take the fastest");
        assert_eq!(choose_mode(&modes, 1920, 1080, Some(120.0)), None);
        assert_eq!(choose_mode(&modes, 800, 600, None), None);
    }
}
