//! Minimal ext-session-lock test client (a stand-in for swaylock): locks the session, covers every
//! output with a solid red surface and unlocks again after a while.
//!
//!   WAYLAND_DISPLAY=rusted-1 cargo run --example lock_client -- [seconds to stay locked, default 5]

use std::{io::Write, os::fd::AsFd, time::{Duration, Instant}};

use wayland_client::{
    delegate_noop,
    protocol::{wl_buffer, wl_compositor, wl_output, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};

#[derive(Default)]
struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    manager: Option<ExtSessionLockManagerV1>,
    outputs: Vec<wl_output::WlOutput>,
    locked: bool,
    finished: bool,
    /// Lock surfaces that got their configure: (surface, serial, w, h).
    configured: Vec<(ExtSessionLockSurfaceV1, u32, u32, u32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_output" => state.outputs.push(registry.bind(name, 1, qh, ())),
                "ext_session_lock_manager_v1" => state.manager = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for App {
    fn event(state: &mut Self, _: &ExtSessionLockV1, event: ext_session_lock_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            ext_session_lock_v1::Event::Locked => state.locked = true,
            ext_session_lock_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for App {
    fn event(state: &mut Self, surface: &ExtSessionLockSurfaceV1, event: ext_session_lock_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let ext_session_lock_surface_v1::Event::Configure { serial, width, height } = event {
            surface.ack_configure(serial);
            state.configured.push((surface.clone(), serial, width, height));
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: ignore wl_output::WlOutput);
delegate_noop!(App: ignore ExtSessionLockManagerV1);

fn main() {
    let hold: u64 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(5);
    let conn = Connection::connect_to_env().expect("cannot connect to WAYLAND_DISPLAY");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut app = App::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut app).unwrap();

    let (compositor, shm, manager) = (
        app.compositor.clone().expect("no wl_compositor"),
        app.shm.clone().expect("no wl_shm"),
        app.manager.clone().expect("compositor has no ext_session_lock_manager_v1"),
    );
    let lock = manager.lock(&qh, ());
    let surfaces: Vec<_> = app
        .outputs
        .iter()
        .map(|o| {
            let surface = compositor.create_surface(&qh, ());
            (lock.get_lock_surface(&surface, o, &qh, ()), surface)
        })
        .collect();
    while !app.locked && !app.finished {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    if app.finished {
        println!("lock refused");
        return;
    }
    println!("locked");

    while app.configured.len() < surfaces.len() {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    for (lock_surface, surface) in &surfaces {
        let &(_, _, w, h) = app.configured.iter().find(|(s, ..)| s == lock_surface).unwrap();
        let stride = w * 4;
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&0xFFCC0000u32.to_le_bytes().repeat((w * h) as usize)).unwrap();
        let pool = shm.create_pool(file.as_fd(), (stride * h) as i32, &qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, wl_shm::Format::Argb8888, &qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.commit();
    }
    println!("lock surfaces mapped: {}", surfaces.len());

    let until = Instant::now() + Duration::from_secs(hold);
    while Instant::now() < until {
        queue.dispatch_pending(&mut app).unwrap();
        conn.flush().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    lock.unlock_and_destroy();
    conn.flush().unwrap();
    queue.roundtrip(&mut app).unwrap();
    println!("unlocked");
}
