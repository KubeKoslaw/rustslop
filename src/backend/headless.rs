//! Headless backend: one virtual output, no window and no real display. Frames are never
//! presented anywhere; clients get frame callbacks on a timer and screenshots / screencopy are
//! rendered offscreen on the GPU's render node. Meant for automated tests (it works while no
//! host compositor is on screen) and as a CI backend: `rusted-wm --headless`.

use std::{fs::OpenOptions, time::Duration};

use smithay::{
    backend::{
        allocator::gbm::GbmDevice,
        egl::{EGLContext, EGLDisplay},
        renderer::gles::GlesRenderer,
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            LoopHandle,
        },
        wayland_server::DisplayHandle,
    },
    utils::Transform,
};

use crate::{control, state::State};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const SIZE: (i32, i32) = (1280, 800);

pub struct Headless {
    pub output: Output,
    renderer: GlesRenderer,
}

impl Headless {
    pub fn new(dh: &DisplayHandle) -> Result<Self, Box<dyn std::error::Error>> {
        // Any GPU render node will do; no session or DRM master is needed for offscreen work.
        let node = std::fs::read_dir("/dev/dri")?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("renderD")))
            .min()
            .ok_or("no /dev/dri/renderD* node: headless mode needs a GPU render node")?;
        let file = OpenOptions::new().read(true).write(true).open(&node)?;
        let gbm = GbmDevice::new(file)?;
        // Safety: the display keeps the gbm device alive for as long as the context exists.
        let egl = unsafe { EGLDisplay::new(gbm)? };
        let context = EGLContext::new(&egl)?;
        let renderer = unsafe { GlesRenderer::new(context)? };

        let output = Output::new(
            "HEADLESS-1".into(),
            PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "rusted".into(), model: "headless".into() },
        );
        let _global = output.create_global::<State>(dh);
        let mode = Mode { size: SIZE.into(), refresh: 60_000 };
        output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
        output.set_preferred(mode);
        tracing::info!(render_node = %node.display(), "headless output ready");
        Ok(Self { output, renderer })
    }

    pub fn register(self, handle: &LoopHandle<'static, State>) -> Result<(), Box<dyn std::error::Error>> {
        let Headless { output: _, mut renderer } = self;
        let start = std::time::Instant::now();
        handle.insert_source(Timer::from_duration(FRAME_INTERVAL), move |_, _, state| {
            if state.config.appearance.enabled() && !state.locked {
                let outputs: Vec<_> = state.outputs.iter().map(|m| m.output.clone()).collect();
                for output in outputs { let _ = super::scene_elements(state, &mut renderer, &output); }
            }
            if let Some((path, mon, stream)) = state.pending_shot.take() {
                let target = state.outputs[mon].output.clone();
                let reply = match super::screenshot(&mut renderer, state, &target, &path) {
                    Ok(()) => "ok".to_string(),
                    Err(e) => format!("error: {e}"),
                };
                control::answer(stream, &reply);
            }
            if state.has_pending_screencopy() {
                state.serve_screencopy(|state, output| super::render_to_rgba(&mut renderer, state, output));
            }
            let outputs: Vec<Output> = state.outputs.iter().map(|m| m.output.clone()).collect();
            for output in outputs {
                state.send_frames(&output, start.elapsed());
            }
            let _ = state.display_handle.flush_clients();
            TimeoutAction::ToDuration(FRAME_INTERVAL)
        })?;
        Ok(())
    }
}
