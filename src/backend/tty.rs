//! Real-session backend: DRM/KMS output, libinput input, libseat session (VT switching).
//!
//! Scope for now: every connected connector on the primary GPU (placed left to right), software
//! cursor. Hotplug and multi-GPU come later. Start with `rusted-wm --tty` on a free VT (Ctrl+Alt+F3),
//! or just run it with no `$WAYLAND_DISPLAY`/`$DISPLAY` set.

use std::{cell::RefCell, rc::Rc, time::Duration};

use smithay::{
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
                memory::MemoryRenderBufferRenderElement, solid::SolidColorRenderElement,
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
    desktop::space::{space_render_elements, SpaceRenderElements},
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            LoopHandle,
        },
        drm::control::{connector, crtc, Device as ControlDevice, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::{backend::GlobalId, DisplayHandle},
    },
    utils::{DeviceFd, Logical, Physical, Point, Transform},
    wayland::compositor::with_states,
};

use crate::{control, cursor::CursorTheme, state::State};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);
const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Xrgb8888, Fourcc::Argb8888];

type Compositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

smithay::backend::renderer::element::render_elements! {
    pub TtyElement<=GlesRenderer>;
    Space = SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Surface = WaylandSurfaceRenderElement<GlesRenderer>,
    Solid = SolidColorRenderElement,
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
    pub fn new(dh: &DisplayHandle) -> Result<Self, BoxError> {
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
            match make_head(&mut drm, &gbm, &renderer, dh, &conn, &heads) {
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
                    if let Err(e) = gpu.heads[idx].compositor.frame_submitted() {
                        tracing::warn!("frame_submitted: {e}");
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
) -> Result<Option<Head>, BoxError> {
    let Some(mode) = conn
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or(conn.modes().first())
        .copied()
    else {
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

    let name = format!("{}-{}", conn.interface().as_str(), conn.interface_id());
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
        match make_head(&mut gpu.drm, &gpu.gbm, &gpu.renderer, &gpu.dh, &conn, &gpu.heads) {
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
    // Screenshots are rendered offscreen, so they work even while another VT is in front.
    if let Some((path, mon, stream)) = state.pending_shot.take() {
        let target = state.outputs[mon].output.clone();
        let borders = state.border_elements(&target);
        let reply = match super::screenshot(&mut gpu.renderer, &target, &state.space, &borders, state.config.background, &path) {
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
    let borders = state.border_elements(&output);
    let background = state.config.background;
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
    elements.extend(borders.iter().cloned().map(TtyElement::Solid));
    match space_render_elements(&mut gpu.renderer, [&state.space], &output, 1.0) {
        Ok(e) => elements.extend(e.into_iter().map(TtyElement::Space)),
        Err(e) => tracing::warn!("space render: {e}"),
    }

    let head = &mut gpu.heads[idx];
    match head.compositor.render_frame(&mut gpu.renderer, &elements, background, FrameFlags::DEFAULT) {
        Ok(result) => {
            if !result.is_empty {
                match head.compositor.queue_frame(()) {
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
