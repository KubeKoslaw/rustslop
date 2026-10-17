//! Minimal layer-shell test client: a solid-colour bar anchored to the top (or bottom) edge.
//!
//!   WAYLAND_DISPLAY=rusted-1 cargo run --example layer_client -- [top|bottom] [height] [exclusive]
//!
//! Used to verify the compositor's wlr-layer-shell support (reserved area, input, stacking).

use std::{io::Write, os::fd::AsFd};

use wayland_client::{
    delegate_noop,
    protocol::{wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};

#[derive(Default)]
struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    configured: Option<(u32, u32, u32)>, // serial, w, h
    closed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => state.layer_shell = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(state: &mut Self, surface: &ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                surface.ack_configure(serial);
                state.configured = Some((serial, width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: ignore ZwlrLayerShellV1);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bottom = args.get(1).is_some_and(|a| a == "bottom");
    let height: u32 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(30);
    let exclusive = args.get(3).is_none_or(|a| a != "0");

    let conn = Connection::connect_to_env().expect("cannot connect to WAYLAND_DISPLAY");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut app = App::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut app).unwrap();

    let (compositor, shm, shell) = (
        app.compositor.clone().expect("no wl_compositor"),
        app.shm.clone().expect("no wl_shm"),
        app.layer_shell.clone().expect("compositor has no zwlr_layer_shell_v1"),
    );
    let surface = compositor.create_surface(&qh, ());
    let layer = shell.get_layer_surface(&surface, None, Layer::Top, "test-bar".into(), &qh, ());
    let edge = if bottom { Anchor::Bottom } else { Anchor::Top };
    layer.set_anchor(edge | Anchor::Left | Anchor::Right);
    layer.set_size(0, height);
    if exclusive {
        layer.set_exclusive_zone(height as i32);
    }
    surface.commit();

    while app.configured.is_none() && !app.closed {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    let (_, w, h) = app.configured.expect("closed before configure");
    let (w, h) = (w.max(1), h.max(1));
    println!("configured: {w}x{h}");

    // Solid orange ARGB8888 buffer.
    let stride = w * 4;
    let mut file = tempfile::tempfile().unwrap();
    let pixel = 0xFFFF9900u32.to_le_bytes();
    file.write_all(&pixel.repeat((w * h) as usize)).unwrap();
    let pool = shm.create_pool(file.as_fd(), (stride * h) as i32, &qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, wl_shm::Format::Argb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    println!("bar mapped");

    while !app.closed {
        queue.blocking_dispatch(&mut app).unwrap();
    }
}
