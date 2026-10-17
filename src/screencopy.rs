//! wlr-screencopy-unstable-v1 (what `grim` uses): copy an output, or a region of it, into a
//! client's wl_shm buffer. Copies are queued here and served by the backends right after a frame
//! (they own the renderer), through the same offscreen path as the control-socket screenshots.
//! Not supported: dmabuf buffers, the cursor overlay, damage tracking (the whole buffer is
//! always reported as damaged).

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, Flags, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            backend::GlobalId,
            protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_shm},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Physical, Rectangle},
    wayland::shm::{with_buffer_contents_mut, BufferData},
};

use crate::state::State;

pub struct ScreencopyState {
    #[allow(dead_code)] // keeps the global alive
    global: GlobalId,
    pending: Vec<PendingCopy>,
}

struct PendingCopy {
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    output: Output,
    /// Region in the output's physical pixels.
    region: Rectangle<i32, Physical>,
    with_damage: bool,
}

/// What a frame object was created for.
pub struct FrameData {
    output: Output,
    region: Rectangle<i32, Physical>,
}

impl ScreencopyState {
    pub fn new(dh: &DisplayHandle) -> Self {
        Self { global: dh.create_global::<State, ZwlrScreencopyManagerV1, _>(3, ()), pending: Vec::new() }
    }
}

impl State {
    /// Serve queued screencopy requests with `render`, which returns the output's size and RGBA
    /// bytes (see `backend::render_to_rgba`).
    pub fn serve_screencopy(
        &mut self,
        mut render: impl FnMut(&mut State, &Output) -> Result<(i32, i32, Vec<u8>), Box<dyn std::error::Error>>,
    ) {
        let pending = std::mem::take(&mut self.screencopy.pending);
        for copy in pending {
            if !copy.frame.is_alive() {
                continue;
            }
            match render(self, &copy.output).map(|(w, h, rgba)| write_region(&copy, w, h, &rgba)) {
                Ok(true) => {
                    if copy.with_damage {
                        copy.frame.damage(0, 0, copy.region.size.w as u32, copy.region.size.h as u32);
                    }
                    copy.frame.flags(Flags::empty());
                    let now: std::time::Duration = self.clock.now().into();
                    let secs = now.as_secs();
                    copy.frame.ready((secs >> 32) as u32, secs as u32, now.subsec_nanos());
                }
                Ok(false) => copy.frame.failed(),
                Err(e) => {
                    tracing::debug!("screencopy failed: {e}");
                    copy.frame.failed();
                }
            }
        }
    }

    pub fn has_pending_screencopy(&self) -> bool {
        !self.screencopy.pending.is_empty()
    }
}

/// Convert the region of `rgba` (full output, w x h) into the client's ARGB8888 shm buffer.
fn write_region(copy: &PendingCopy, w: i32, h: i32, rgba: &[u8]) -> bool {
    let r = copy.region;
    if r.loc.x < 0 || r.loc.y < 0 || r.loc.x + r.size.w > w || r.loc.y + r.size.h > h {
        return false;
    }
    with_buffer_contents_mut(&copy.buffer, |ptr, len, data: BufferData| {
        let (stride, offset) = (data.stride as usize, data.offset as usize);
        if data.width != r.size.w || data.height != r.size.h || offset + stride * data.height as usize > len {
            return false;
        }
        for row in 0..r.size.h as usize {
            let src_start = ((r.loc.y as usize + row) * w as usize + r.loc.x as usize) * 4;
            let src = &rgba[src_start..src_start + r.size.w as usize * 4];
            // Safety: the bounds were checked against the pool length above.
            let dst = unsafe { std::slice::from_raw_parts_mut(ptr.add(offset + row * stride), r.size.w as usize * 4) };
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                // RGBA in memory -> ARGB8888 (little endian: B, G, R, A).
                d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            }
        }
        true
    })
    .unwrap_or(false)
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, manager: New<ZwlrScreencopyManagerV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        data_init.init(manager, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let (frame, wl_output, region) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput { frame, output, .. } => (frame, output, None),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion { frame, output, x, y, width, height, .. } => {
                (frame, output, Some((x, y, width, height)))
            }
            _ => return,
        };
        let Some(output) = Output::from_resource(&wl_output as &WlOutput).filter(|o| state.output_index(o).is_some()) else {
            data_init.init(frame, FrameData { output: state.focused_output().clone(), region: Rectangle::default() }).failed();
            return;
        };
        let Some(mode) = output.current_mode() else {
            data_init.init(frame, FrameData { output, region: Rectangle::default() }).failed();
            return;
        };
        let scale = output.current_scale().fractional_scale();
        let region = match region {
            // Logical coordinates relative to the output -> physical pixels.
            Some((x, y, w, h)) if w > 0 && h > 0 => Rectangle::<i32, Physical>::new(
                ((x as f64 * scale).round() as i32, (y as f64 * scale).round() as i32).into(),
                (((w as f64 * scale).round() as i32).max(1), ((h as f64 * scale).round() as i32).max(1)).into(),
            ),
            Some(_) => {
                data_init.init(frame, FrameData { output, region: Rectangle::default() }).failed();
                return;
            }
            None => Rectangle::from_size(mode.size),
        };
        let frame = data_init.init(frame, FrameData { output, region });
        frame.buffer(wl_shm::Format::Argb8888, region.size.w as u32, region.size.h as u32, region.size.w as u32 * 4);
        if frame.version() >= 3 {
            frame.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            _ => return,
        };
        let valid = with_buffer_contents_mut(&buffer, |_, _, d: BufferData| {
            d.width == data.region.size.w
                && d.height == data.region.size.h
                && d.stride >= d.width * 4
                && d.format == wl_shm::Format::Argb8888
        });
        if !valid.unwrap_or(false) {
            frame.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer, "buffer does not match the advertised wl_shm format and size");
            return;
        }
        state.screencopy.pending.push(PendingCopy {
            frame: frame.clone(),
            buffer,
            output: data.output.clone(),
            region: data.region,
            with_damage,
        });
    }
}
