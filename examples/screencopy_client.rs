//! Minimal wlr-screencopy test client (a stand-in for `grim`): captures an output, or a region,
//! into a PNG.
//!
//!   WAYLAND_DISPLAY=rusted-1 cargo run --example screencopy_client -- out.png [x,y,w,h]

use std::{io::Write, os::fd::AsFd};

use wayland_client::{
    delegate_noop,
    protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool},
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

#[derive(Default)]
struct App {
    shm: Option<wl_shm::WlShm>,
    manager: Option<ZwlrScreencopyManagerV1>,
    output: Option<wl_output::WlOutput>,
    /// (width, height, stride) from the `buffer` event
    buffer: Option<(u32, u32, u32)>,
    buffer_done: bool,
    ready: bool,
    failed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_output" if state.output.is_none() => state.output = Some(registry.bind(name, 1, qh, ())),
                "zwlr_screencopy_manager_v1" => state.manager = Some(registry.bind(name, 3, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for App {
    fn event(state: &mut Self, _: &ZwlrScreencopyFrameV1, event: zwlr_screencopy_frame_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer { format, width, height, stride } => {
                assert_eq!(format, WEnum::Value(wl_shm::Format::Argb8888), "unexpected format");
                state.buffer = Some((width, height, stride));
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => state.buffer_done = true,
            zwlr_screencopy_frame_v1::Event::Ready { .. } => state.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => state.failed = true,
            _ => {}
        }
    }
}

delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: ignore wl_output::WlOutput);
delegate_noop!(App: ignore ZwlrScreencopyManagerV1);

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: screencopy_client OUT.png [x,y,w,h]");
    let region: Option<Vec<i32>> = args.next().map(|r| r.split(',').map(|n| n.parse().unwrap()).collect());

    let conn = Connection::connect_to_env().expect("cannot connect to WAYLAND_DISPLAY");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut app = App::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut app).unwrap();
    let (shm, manager, output) = (
        app.shm.clone().expect("no wl_shm"),
        app.manager.clone().expect("compositor has no zwlr_screencopy_manager_v1"),
        app.output.clone().expect("no wl_output"),
    );

    let frame = match region.as_deref() {
        Some(&[x, y, w, h]) => manager.capture_output_region(0, &output, x, y, w, h, &qh, ()),
        _ => manager.capture_output(0, &output, &qh, ()),
    };
    while !app.buffer_done && !app.failed {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    let (w, h, stride) = app.buffer.expect("no buffer event");
    println!("buffer {w}x{h}");

    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&vec![0u8; (stride * h) as usize]).unwrap();
    let pool = shm.create_pool(file.as_fd(), (stride * h) as i32, &qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, wl_shm::Format::Argb8888, &qh, ());
    frame.copy(&buffer);
    while !app.ready && !app.failed {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    if app.failed {
        println!("failed");
        std::process::exit(1);
    }

    // Read the pixels back from the shared file and write an RGBA PNG.
    let mut map = std::fs::read(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&file))).unwrap();
    for px in map.chunks_exact_mut(4) {
        px.swap(0, 2); // BGRA -> RGBA
    }
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path).unwrap()), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().unwrap().write_image_data(&map).unwrap();
    println!("ready {path}");
}
