//! Real-session backend: DRM/KMS output, libinput input, libseat session (VT switching).
//!
//! Scope for now: the first connected connector on the primary GPU, one output, software cursor.
//! Multi-monitor and hotplug come later. Start with `rusted-wm --tty` on a free VT (Ctrl+Alt+F3),
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
        wayland_server::DisplayHandle,
    },
    utils::{DeviceFd, Logical, Physical, Point, Transform},
    wayland::compositor::with_states,
};

use crate::{control, cursor::CursorImage, state::State};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
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

struct Gpu {
    drm: DrmDevice,
    compositor: Compositor,
    renderer: GlesRenderer,
    cursor: CursorImage,
    frame_pending: bool,
    active: bool,
}

pub struct Tty {
    pub session: LibSeatSession,
    session_notifier: LibSeatSessionNotifier,
    seat_name: String,
    drm_notifier: DrmDeviceNotifier,
    gpu: Gpu,
    pub output: Output,
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
        // Safety: the gbm device outlives the EGL display (both are owned by `Gpu`/the compositor).
        let egl = unsafe { EGLDisplay::new(gbm.clone())? };
        let context = EGLContext::new(&egl)?;
        let renderer = unsafe { GlesRenderer::new(context)? };

        // First connected connector with a mode.
        let resources = drm.resource_handles()?;
        let (conn, mode) = resources
            .connectors()
            .iter()
            .filter_map(|&h| drm.get_connector(h, true).ok())
            .filter(|c| c.state() == connector::State::Connected)
            .find_map(|c| {
                let mode = c
                    .modes()
                    .iter()
                    .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                    .or(c.modes().first())
                    .copied()?;
                Some((c, mode))
            })
            .ok_or("no connected display found")?;
        let crtc: crtc::Handle = conn
            .current_encoder()
            .and_then(|e| drm.get_encoder(e).ok())
            .and_then(|e| e.crtc())
            .or_else(|| {
                conn.encoders()
                    .iter()
                    .filter_map(|&e| drm.get_encoder(e).ok())
                    .find_map(|e| resources.filter_crtcs(e.possible_crtcs()).first().copied())
            })
            .ok_or("no usable CRTC for the connector")?;

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
        let _global = output.create_global::<State>(dh);
        let output_mode = OutputMode::from(mode);
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
            Some(gbm),
        )?;

        Ok(Self {
            session,
            session_notifier,
            seat_name,
            drm_notifier,
            gpu: Gpu { drm, compositor, renderer, cursor: CursorImage::load(), frame_pending: false, active: true },
            output,
        })
    }

    pub fn register(self, handle: &LoopHandle<'static, State>) -> Result<(), BoxError> {
        let Tty { session, session_notifier, seat_name, drm_notifier, gpu, output } = self;
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
                        gpu.compositor.reset_buffers();
                        gpu.frame_pending = false;
                        gpu.active = true;
                    }
                }
            }
        })?;

        // Page flips completed.
        handle.insert_source(drm_notifier, {
            let gpu = gpu.clone();
            let output = output.clone();
            move |event, _, state| match event {
                DrmEvent::VBlank(_) => {
                    let mut gpu = gpu.borrow_mut();
                    if let Err(e) = gpu.compositor.frame_submitted() {
                        tracing::warn!("frame_submitted: {e}");
                    }
                    gpu.frame_pending = false;
                    render(&mut gpu, state, &output);
                }
                DrmEvent::Error(e) => tracing::error!("drm error: {e}"),
            }
        })?;

        // Fallback tick: starts the first frame and retries when nothing was queued.
        handle.insert_source(Timer::from_duration(FRAME_INTERVAL), {
            let gpu = gpu.clone();
            let start = std::time::Instant::now();
            move |_, _, state| {
                let mut gpu = gpu.borrow_mut();
                render(&mut gpu, state, &output);
                let _ = start;
                TimeoutAction::ToDuration(FRAME_INTERVAL)
            }
        })?;
        Ok(())
    }
}

/// Draw and queue one frame if the GPU is ours and idle.
fn render(gpu: &mut Gpu, state: &mut State, output: &Output) {
    // Screenshots are rendered offscreen, so they work even while another VT is in front.
    if let Some((path, stream)) = state.pending_shot.take() {
        let borders = state.border_elements();
        let reply = match super::screenshot(&mut gpu.renderer, output, &state.space, &borders, state.config.background, &path) {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("error: {e}"),
        };
        control::answer(stream, &reply);
    }
    if !gpu.active || gpu.frame_pending {
        return;
    }
    let borders = state.border_elements();
    let background = state.config.background;
    let pointer: Point<f64, Logical> = state.seat.get_pointer().unwrap().current_location();

    let mut elements: Vec<TtyElement> = Vec::new();
    match &state.cursor_status {
        CursorImageStatus::Hidden => {}
        CursorImageStatus::Surface(surface) => {
            let hotspot = with_states(surface, |s| {
                s.data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|d| d.lock().unwrap().hotspot)
                    .unwrap_or_default()
            });
            let at: Point<i32, Physical> = (pointer - hotspot.to_f64()).to_physical_precise_round(1.0);
            elements.extend(
                render_elements_from_surface_tree(&mut gpu.renderer, surface, at, 1.0, 1.0, Kind::Cursor)
                    .into_iter()
                    .map(TtyElement::Surface),
            );
        }
        CursorImageStatus::Named(_) => {
            let at = Point::<f64, Physical>::from((
                pointer.x - gpu.cursor.hotspot.x as f64,
                pointer.y - gpu.cursor.hotspot.y as f64,
            ));
            match MemoryRenderBufferRenderElement::from_buffer(&mut gpu.renderer, at, &gpu.cursor.buffer, None, None, None, Kind::Cursor) {
                Ok(e) => elements.push(TtyElement::Memory(e)),
                Err(e) => tracing::warn!("cursor render: {e}"),
            }
        }
    }
    elements.extend(borders.iter().cloned().map(TtyElement::Solid));
    match space_render_elements(&mut gpu.renderer, [&state.space], output, 1.0) {
        Ok(e) => elements.extend(e.into_iter().map(TtyElement::Space)),
        Err(e) => tracing::warn!("space render: {e}"),
    }

    match gpu.compositor.render_frame(&mut gpu.renderer, &elements, background, FrameFlags::DEFAULT) {
        Ok(result) => {
            if !result.is_empty {
                match gpu.compositor.queue_frame(()) {
                    Ok(()) => gpu.frame_pending = true,
                    Err(e) => tracing::warn!("queue_frame: {e}"),
                }
            }
        }
        Err(e) => tracing::warn!("render_frame: {e}"),
    }

    state.send_frames(output, std::time::UNIX_EPOCH.elapsed().unwrap_or_default());
    let _ = state.display_handle.flush_clients();
}
