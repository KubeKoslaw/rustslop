//! Deterministic pixels for renderer regression tests: window, glass, backdrop, islands.
use std::{io::Write, os::fd::AsFd};
use wayland_client::{delegate_noop, protocol::{wl_buffer,wl_compositor,wl_registry,wl_shm,wl_shm_pool,wl_surface},Connection,Dispatch,QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_wm_base,xdg_surface,xdg_toplevel};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1::{Layer,ZwlrLayerShellV1},zwlr_layer_surface_v1::{self,Anchor,ZwlrLayerSurfaceV1}};
#[derive(Default)]
struct App {
    compositor:Option<wl_compositor::WlCompositor>,shm:Option<wl_shm::WlShm>,shell:Option<xdg_wm_base::XdgWmBase>,layers:Option<ZwlrLayerShellV1>,
    size:(u32,u32),ready:bool,closed:bool,
}
impl Dispatch<wl_registry::WlRegistry,()> for App {
    fn event(s:&mut Self,r:&wl_registry::WlRegistry,e:wl_registry::Event,_:&(),_:&Connection,q:&QueueHandle<Self>) {
        if let wl_registry::Event::Global{name,interface,..}=e {match interface.as_str(){
            "wl_compositor"=>s.compositor=Some(r.bind(name,4,q,())),"wl_shm"=>s.shm=Some(r.bind(name,1,q,())),
            "xdg_wm_base"=>s.shell=Some(r.bind(name,1,q,())),"zwlr_layer_shell_v1"=>s.layers=Some(r.bind(name,1,q,())),_=>{}
        }}
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase,()> for App {fn event(_:&mut Self,p:&xdg_wm_base::XdgWmBase,e:xdg_wm_base::Event,_:&(),_:&Connection,_:&QueueHandle<Self>){if let xdg_wm_base::Event::Ping{serial}=e{p.pong(serial);}}}
impl Dispatch<xdg_surface::XdgSurface,()> for App {fn event(s:&mut Self,p:&xdg_surface::XdgSurface,e:xdg_surface::Event,_:&(),_:&Connection,_:&QueueHandle<Self>){if let xdg_surface::Event::Configure{serial}=e{p.ack_configure(serial);s.ready=true;}}}
impl Dispatch<xdg_toplevel::XdgToplevel,()> for App {fn event(s:&mut Self,_:&xdg_toplevel::XdgToplevel,e:xdg_toplevel::Event,_:&(),_:&Connection,_:&QueueHandle<Self>){match e{xdg_toplevel::Event::Configure{width,height,..}=>s.size=(width.max(1) as u32,height.max(1) as u32),xdg_toplevel::Event::Close=>s.closed=true,_=>{}}}}
impl Dispatch<ZwlrLayerSurfaceV1,()> for App {fn event(s:&mut Self,p:&ZwlrLayerSurfaceV1,e:zwlr_layer_surface_v1::Event,_:&(),_:&Connection,_:&QueueHandle<Self>){match e{zwlr_layer_surface_v1::Event::Configure{serial,width,height}=>{p.ack_configure(serial);s.size=(width,height);s.ready=true;},zwlr_layer_surface_v1::Event::Closed=>s.closed=true,_=>{}}}}
delegate_noop!(App:ignore wl_compositor::WlCompositor);
delegate_noop!(App:ignore wl_surface::WlSurface);
delegate_noop!(App:ignore wl_shm::WlShm);
delegate_noop!(App:ignore wl_shm_pool::WlShmPool);
delegate_noop!(App:ignore wl_buffer::WlBuffer);
delegate_noop!(App:ignore ZwlrLayerShellV1);
fn main(){
    let mode=std::env::args().nth(1).unwrap_or("window".into());
    let conn=Connection::connect_to_env().unwrap();let mut queue=conn.new_event_queue();let q=queue.handle();let mut app=App::default();conn.display().get_registry(&q,());queue.roundtrip(&mut app).unwrap();
    let surface=app.compositor.as_ref().unwrap().create_surface(&q,());
    let mut _xdg=None;let mut _top=None;let mut _layer=None;
    if mode=="backdrop"||mode=="islands" {
        let layer=app.layers.as_ref().unwrap().get_layer_surface(&surface,None,if mode=="backdrop"{Layer::Background}else{Layer::Top},if mode=="backdrop"{"test-background"}else{"waybar"}.into(),&q,());
        layer.set_anchor(Anchor::Top|Anchor::Left|Anchor::Right|if mode=="backdrop"{Anchor::Bottom}else{Anchor::empty()});
        layer.set_size(0,if mode=="backdrop"{0}else{80});layer.set_exclusive_zone(-1);_layer=Some(layer);
    }else{
        let xdg=app.shell.as_ref().unwrap().get_xdg_surface(&surface,&q,());let top=xdg.get_toplevel(&q,());top.set_app_id("foot".into());top.set_title(format!("appearance-{mode}"));_xdg=Some(xdg);_top=Some(top);
    }
    surface.commit();
    while !app.closed {
        queue.blocking_dispatch(&mut app).unwrap();
        if !app.ready{continue;}app.ready=false;
        let (w,h)=(app.size.0.max(1),app.size.1.max(1));let mut bytes=Vec::with_capacity((w*h*4)as usize);
        for y in 0..h{for x in 0..w{
            let pixel:u32=match mode.as_str(){
                "backdrop"=>if (x/4)%2==0{0xffeeeeee}else{0xff222222},
                "islands"=>if (20..220).contains(&x)||(w-220..w-20).contains(&x){0xa020222e}else{0},
                "glass"=>if (60..80).contains(&x)&&(60..80).contains(&y){0xff00ff00}else{0x80600000},
                _=>0xffcc2244,
            };bytes.extend_from_slice(&pixel.to_le_bytes());
        }}
        let mut file=tempfile::tempfile().unwrap();file.write_all(&bytes).unwrap();let pool=app.shm.as_ref().unwrap().create_pool(file.as_fd(),bytes.len()as i32,&q,());let buffer=pool.create_buffer(0,w as i32,h as i32,w as i32*4,wl_shm::Format::Argb8888,&q,());
        surface.attach(Some(&buffer),0,0);surface.damage_buffer(0,0,w as i32,h as i32);surface.commit();conn.flush().unwrap();
        // The server holds the fd mapping until release. Destroy proxies to bound client resources.
        buffer.destroy();pool.destroy();
    }
}
