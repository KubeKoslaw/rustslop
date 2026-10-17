//! Shared GLES effects compositor. Captures are GPU-only; no framebuffer readback is used here.
//! Raw surface commits are cached separately from presentation geometry. An unchanged scene
//! returns the same render element ID, allowing the outer damage tracker to stop drawing.
use std::time::{Duration, Instant};
use smithay::wayland::seat::WaylandFocus;
use smithay::{
    backend::{allocator::Fourcc, renderer::{
        damage::OutputDamageTracker,
        element::{surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement}, texture::{TextureBuffer, TextureRenderElement}, Element, Kind},
        gles::{ffi, GlesError, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType},
        Bind, Color32F, Frame, Offscreen, Renderer, Texture,
    }},
    desktop::{layer_map_for_output, PopupManager, Window},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Physical, Point, Rectangle, Size, Transform},
    wayland::{compositor::with_states, shell::{wlr_layer::Layer, xdg::XdgToplevelSurfaceData}},
};
use crate::{config::Appearance, state::State};
type Rect = Rectangle<i32, Physical>;
type Surface = WaylandSurfaceRenderElement<GlesRenderer>;

#[derive(Clone, PartialEq, Eq, Debug)]
enum Key { Window(Window), Surface(WlSurface) }
struct Group {
    key: Key, rect: Rect, elements: Vec<Surface>, decorated: bool, blur: bool,
    fullscreen: bool, direct: bool, color: Color32F,
}
struct Cached {
    key: Key, signature: String, texture: GlesTexture, target: Rect, from: Rect,
    moved: Instant, opened: Instant, decorated: bool, blur: bool, fullscreen: bool,
    color: Color32F,
}
struct Closing { texture: GlesTexture, rect: Rect, started: Instant, color: Color32F }
struct OutputCache {
    output: Output, size: Size<i32, Physical>, scale: f64, ws: usize,
    groups: Vec<Cached>, closing: Vec<Closing>, canvas: GlesTexture,
    half_a: GlesTexture, half_b: GlesTexture,
    result: Option<TextureBuffer<GlesTexture>>, signature: String,
    transition: Option<(GlesTexture, Instant)>,
}
struct Programs { window: GlesTexProgram, blur: GlesTexProgram, decoration: GlesPixelProgram }
#[derive(Default)]
pub struct Effects { programs: Option<Programs>, outputs: Vec<OutputCache>, failed: bool }

pub fn contains_rounded(size: (i32, i32), point: (f64, f64), radius: i32) -> bool {
    let (w,h) = (size.0 as f64, size.1 as f64);
    let (x,y) = point;
    if x < 0.0 || y < 0.0 || x >= w || y >= h { return false; }
    let r = (radius as f64).min(w/2.0).min(h/2.0).max(0.0);
    let dx = (x-w/2.0).abs() - (w/2.0-r);
    let dy = (y-h/2.0).abs() - (h/2.0-r);
    dx.max(0.0).powi(2) + dy.max(0.0).powi(2) <= r*r
}
fn progress(since: Instant, now: Instant, ms: f64) -> f64 {
    if ms <= 0.0 { 1.0 } else { (now.duration_since(since).as_secs_f64()*1000.0/ms).clamp(0.0,1.0) }
}
fn interpolate(from: Rect, to: Rect, t: f64) -> Rect {
    let t = 1.0 - (1.0-t).powi(3);
    let mix = |a:i32,b:i32| (a as f64+(b-a) as f64*t).round() as i32;
    Rectangle::new((mix(from.loc.x,to.loc.x),mix(from.loc.y,to.loc.y)).into(),
        (mix(from.size.w,to.size.w).max(1),mix(from.size.h,to.size.h).max(1)).into())
}
fn alloc(renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Result<GlesTexture,GlesError> {
    renderer.create_buffer(Fourcc::Abgr8888, (size.w.max(1),size.h.max(1)).into())
}
fn texture_draw(renderer: &mut GlesRenderer, dst: &mut GlesTexture, src: &GlesTexture,
    rect: Rect, alpha: f32, shader: Option<&GlesTexProgram>, uniforms: &[Uniform<'_>], clear: bool) -> Result<(),GlesError> {
    let size = dst.size().to_logical(1,Transform::Normal).to_physical(1);
    let mut target = renderer.bind(dst)?;
    let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
    if clear { frame.clear(Color32F::TRANSPARENT, &[Rectangle::from_size(size)])?; }
    frame.render_texture_from_to(src, Rectangle::from_size(src.size().to_f64()), rect,
        &[Rectangle::from_size(rect.size)], &[], Transform::Normal, alpha, shader, uniforms)?;
    let _sync = frame.finish()?;
    Ok(())
}
fn snapshot(renderer: &mut GlesRenderer, src: &GlesTexture) -> Result<GlesTexture,GlesError> {
    let size = src.size().to_logical(1,Transform::Normal).to_physical(1);
    let mut tex = alloc(renderer,size)?;
    texture_draw(renderer,&mut tex,src,Rectangle::from_size(size),1.0,None,&[],true)?;
    Ok(tex)
}
impl Programs {
    fn new(r: &mut GlesRenderer) -> Result<Self,GlesError> {
        let names = |items: &[(&str,UniformType)]| items.iter().map(|(n,t)| UniformName::new(n.to_string(),*t)).collect::<Vec<_>>();
        use UniformType::*;
        Ok(Self {
            window: r.compile_custom_texture_shader(include_str!("shaders/window.frag"), &names(&[("backdrop",_1i),("extent",_2f),("output_size",_2f),("radius",_1f),("use_blur",_1f)]))?,
            blur: r.compile_custom_texture_shader(include_str!("shaders/blur.frag"), &names(&[("step_size",_2f)]))?,
            decoration: r.compile_custom_pixel_shader(include_str!("shaders/decoration.frag"), &names(&[("window_size",_2f),("padding",_1f),("radius",_1f),("border_width",_1f),("border_color",_4f),("shadow_size",_1f),("shadow_opacity",_1f),("shadow_offset",_1f)]))?,
        })
    }
}
impl Effects {
    /// Called immediately on lock and config reload, including if no frame is being presented.
    pub fn clear(&mut self) { self.outputs.clear(); self.failed = false; }
    pub fn render(&mut self, state: &State, r: &mut GlesRenderer, output: &Output) -> Option<TextureRenderElement<GlesTexture>> {
        if self.failed { return None; }
        match self.try_render(state,r,output) {
            Ok(e) => Some(e),
            Err(e) => { tracing::warn!("appearance effects disabled until reload: {e}"); self.outputs.clear(); self.failed=true; None }
        }
    }
    fn try_render(&mut self, state: &State, r: &mut GlesRenderer, output: &Output) -> Result<TextureRenderElement<GlesTexture>,GlesError> {
        if self.programs.is_none() { self.programs=Some(Programs::new(r)?); }
        self.outputs.retain(|c| state.outputs.iter().any(|m| m.output==c.output));
        let scale=output.current_scale().fractional_scale();
        let logical=state.space.output_geometry(output).unwrap().size;
        let size=logical.to_f64().to_physical(scale).to_i32_round();
        let ws=state.outputs.iter().find(|m| &m.output==output).unwrap().ws;
        self.outputs.retain(|c| c.output!=*output || (c.size==size && c.scale==scale));
        if !self.outputs.iter().any(|c| c.output==*output) {
            let half=(size.w.div_euclid(2).max(1),size.h.div_euclid(2).max(1)).into();
            self.outputs.push(OutputCache { output:output.clone(),size,scale,ws,groups:vec![],closing:vec![],canvas:alloc(r,size)?,half_a:alloc(r,half)?,half_b:alloc(r,half)?,result:None,signature:String::new(),transition:None });
        }
        let cache=self.outputs.iter_mut().find(|c| c.output==*output).unwrap();
        let p=self.programs.as_ref().unwrap();
        let a=&state.config.appearance;
        let animate=a.animations_enabled && a.animation_duration_ms>0;
        let now=Instant::now();
        let duration=if animate { a.animation_duration_ms as f64 } else { 0.0 };
        let workspace_changed=cache.ws!=ws;
        if workspace_changed {
            if animate && cache.result.is_some() { cache.transition=Some((snapshot(r,&cache.canvas)?,now)); }
            cache.ws=ws; cache.closing.clear(); cache.groups.clear();
        }
        if !animate { cache.transition=None; cache.closing.clear(); }
        let groups=groups(state,r,output);
        let mut signature=format!("{a:?}{:?}{:?}",state.config.background,state.config.general.border);
        let mut next=Vec::new();
        let mut moving=false;
        for group in groups {
            let sig=format!("{:?}",group.elements.iter().map(|e| (e.id(),e.current_commit(),e.geometry(scale.into()))).collect::<Vec<_>>());
            signature.push_str(&format!("{:?}{:?}{:?}{:?}{:?}{sig}",group.key,group.rect,group.color,group.blur,group.fullscreen));
            let previous=cache.groups.iter().position(|c| c.key==group.key).map(|i| cache.groups.remove(i));
            let mut c=if let Some(c)=previous {c} else {
                Cached { key:group.key.clone(), signature:String::new(),texture:alloc(r,group.rect.size)?,target:group.rect,from:group.rect,
                    moved:now-Duration::from_secs(2),opened:if workspace_changed || !group.decorated || group.fullscreen {now-Duration::from_secs(2)}else{now},decorated:group.decorated,blur:group.blur,fullscreen:group.fullscreen,color:group.color }
            };
            if c.signature!=sig || c.texture.size().to_logical(1,Transform::Normal).to_physical(1)!=group.rect.size {
                if c.texture.size().to_logical(1,Transform::Normal).to_physical(1)!=group.rect.size { c.texture=alloc(r,group.rect.size)?; }
                let mut fb=r.bind(&mut c.texture)?;
                OutputDamageTracker::new(group.rect.size,scale,Transform::Normal)
                    .render_output(r,&mut fb,0,&group.elements,Color32F::TRANSPARENT).map_err(|e| match e { smithay::backend::renderer::damage::Error::Rendering(e)=>e, _=>GlesError::UnknownSize })?;
                c.signature=sig;
            }
            if c.target!=group.rect {
                c.from=interpolate(c.from,c.target,progress(c.moved,now,duration)); c.target=group.rect; c.moved=now;
            }
            if group.direct || group.fullscreen || !group.decorated { c.from=c.target; c.moved=now-Duration::from_secs(2); }
            c.decorated=group.decorated;c.blur=group.blur;c.fullscreen=group.fullscreen;c.color=group.color;
            moving |= animate && group.decorated && !group.fullscreen && (progress(c.moved,now,duration)<1.0 || progress(c.opened,now,duration*160.0/180.0)<1.0);
            next.push(c);
        }
        for old in cache.groups.drain(..) {
            if animate && old.decorated && !workspace_changed {
                if let Key::Window(ref window)=old.key {
                    if !state.workspaces.iter().any(|ws| ws.entries.iter().any(|e| &e.window==window)) {
                        cache.closing.push(Closing {texture:old.texture,rect:old.target,started:now,color:old.color});
                    }
                }
            }
        }
        cache.groups=next;
        cache.closing.retain(|c| progress(c.started,now,duration*120.0/180.0)<1.0);
        if cache.transition.as_ref().is_some_and(|(_,t)|progress(*t,now,duration*160.0/180.0)>=1.0) { cache.transition=None; }
        let active=moving || !cache.closing.is_empty() || cache.transition.is_some();
        // The final settled frame must be drawn once after an animation ends.
        signature.push_str(if active {"moving"} else {"settled"});
        if cache.result.is_none() || signature!=cache.signature || active {
            let mut fb=r.bind(&mut cache.canvas)?;
            let mut frame=r.render(&mut fb,size,Transform::Normal)?;
            frame.clear(state.config.background,&[Rectangle::from_size(size)])?; let _sync = frame.finish()?;
            drop(fb);
            let mut closing_drawn=false;
            for c in &cache.groups {
                // Closing snapshots stay below panels and menus.
                if !c.decorated && !closing_drawn && matches!(c.key,Key::Surface(_)) && c.blur {
                    draw_closing(r,p,&mut cache.canvas,&cache.closing,a,scale,state.config.general.border,now,duration)?;closing_drawn=true;
                }
                let mut rect=interpolate(c.from,c.target,progress(c.moved,now,duration));
                let open=if c.decorated && !c.fullscreen {progress(c.opened,now,duration*160.0/180.0)}else{1.0};
                if open<1.0 {
                    let k=0.97+0.03*(1.0-(1.0-open).powi(3));
                    let new_size=(rect.size.w as f64*k,rect.size.h as f64*k);
                    rect.loc.x+=((rect.size.w as f64-new_size.0)/2.0).round() as i32;
                    rect.loc.y+=((rect.size.h as f64-new_size.1)/2.0).round() as i32;
                    rect.size=(new_size.0.round() as i32,new_size.1.round() as i32).into();
                }
                let alpha=open as f32;
                if c.decorated && !c.fullscreen { decoration(r,p,&mut cache.canvas,rect,a,scale,state.config.general.border,c.color,alpha)?; }
                let blur=a.blur_enabled && c.blur && !c.fullscreen;
                if blur {
                    let half=cache.half_a.size().to_logical(1,Transform::Normal).to_physical(1);
                    let radius=a.blur_radius as f32*scale as f32/3.230769;
                    texture_draw(r,&mut cache.half_a,&cache.canvas,Rectangle::from_size(half),1.0,Some(&p.blur),&[Uniform::new("step_size",[radius/size.w as f32,0.0])],true)?;
                    texture_draw(r,&mut cache.half_b,&cache.half_a,Rectangle::from_size(half),1.0,Some(&p.blur),&[Uniform::new("step_size",[0.0,radius/size.h as f32])],true)?;
                }
                draw_window(r,p,&mut cache.canvas,&c.texture,&cache.half_b,rect,
                    if c.decorated && !c.fullscreen {a.corner_radius as f32*scale as f32}else{0.0},blur,alpha)?;
            }
            if !closing_drawn { draw_closing(r,p,&mut cache.canvas,&cache.closing,a,scale,state.config.general.border,now,duration)?; }
            if let Some((old,since))=&cache.transition {
                texture_draw(r,&mut cache.canvas,old,Rectangle::from_size(size),(1.0-progress(*since,now,duration*160.0/180.0)) as f32,None,&[],false)?;
            }
            cache.result=Some(TextureBuffer::from_texture(r,cache.canvas.clone(),1,Transform::Normal,None));
            cache.signature=signature;
        }
        // The canvas is physical, so declare the full physical extent as the source (the texture
        // buffer has scalar scale 1) while the element itself stays `logical` sized. Without this,
        // fractional outputs sample only the top-left logical area and upscale it.
        let src=Rectangle::from_size((size.w as f64,size.h as f64).into());
        Ok(TextureRenderElement::from_texture_buffer((0.0,0.0),cache.result.as_ref().unwrap(),None,Some(src),Some(logical),Kind::Unspecified))
    }
}
fn draw_window(r:&mut GlesRenderer,p:&Programs,canvas:&mut GlesTexture,source:&GlesTexture,backdrop:&GlesTexture,rect:Rect,radius:f32,blur:bool,alpha:f32)->Result<(),GlesError> {
    let size=canvas.size().to_logical(1,Transform::Normal).to_physical(1);
    let mut fb=r.bind(canvas)?;
    let mut frame=r.render(&mut fb,size,Transform::Normal)?;
    // Smithay owns unit 0. Save and restore the extra texture unit even on draw failure.
    let saved=frame.with_context(|gl|unsafe {
        let mut active=0; let mut binding=0;
        gl.GetIntegerv(ffi::ACTIVE_TEXTURE,&mut active);gl.ActiveTexture(ffi::TEXTURE1);
        gl.GetIntegerv(ffi::TEXTURE_BINDING_2D,&mut binding);gl.BindTexture(ffi::TEXTURE_2D,backdrop.tex_id());
        gl.ActiveTexture(ffi::TEXTURE0);(active,binding)
    })?;
    let result=frame.render_texture_from_to(source,Rectangle::from_size(source.size().to_f64()),rect,&[Rectangle::from_size(rect.size)],&[],Transform::Normal,alpha,Some(&p.window),&[
        Uniform::new("backdrop",1i32),Uniform::new("extent",[rect.size.w as f32,rect.size.h as f32]),
        Uniform::new("output_size",[size.w as f32,size.h as f32]),Uniform::new("radius",radius),Uniform::new("use_blur",if blur {1.0f32}else{0.0})]);
    frame.with_context(|gl|unsafe {gl.ActiveTexture(ffi::TEXTURE1);gl.BindTexture(ffi::TEXTURE_2D,saved.1 as u32);gl.ActiveTexture(saved.0 as u32);})?;
    result?;let _sync = frame.finish()?;Ok(())
}
fn decoration(r:&mut GlesRenderer,p:&Programs,canvas:&mut GlesTexture,rect:Rect,a:&Appearance,scale:f64,border:i32,color:Color32F,alpha:f32)->Result<(),GlesError> {
    let shadow=if a.shadow_enabled {a.shadow_size as f32*scale as f32}else{0.0};
    let b=border as f32*scale as f32;let pad=(shadow+b+6.0*scale as f32).ceil() as i32;
    let dest:Rect=Rectangle::new((rect.loc.x-pad,rect.loc.y-pad).into(),(rect.size.w+2*pad,rect.size.h+2*pad).into());
    let size=canvas.size().to_logical(1,Transform::Normal).to_physical(1);let mut fb=r.bind(canvas)?;let mut frame=r.render(&mut fb,size,Transform::Normal)?;
    frame.render_pixel_shader_to(&p.decoration,Rectangle::from_size(Size::from((dest.size.w as f64,dest.size.h as f64))),dest,Size::from((dest.size.w,dest.size.h)),None,alpha,&[
        Uniform::new("window_size",[rect.size.w as f32,rect.size.h as f32]),Uniform::new("padding",pad as f32),Uniform::new("radius",a.corner_radius as f32*scale as f32),Uniform::new("border_width",b),Uniform::new("border_color",color.components()),Uniform::new("shadow_size",shadow),Uniform::new("shadow_opacity",if a.shadow_enabled {a.shadow_opacity as f32}else{0.0}),Uniform::new("shadow_offset",6.0*scale as f32)])?;
    let _sync = frame.finish()?;Ok(())
}
fn draw_closing(r:&mut GlesRenderer,p:&Programs,canvas:&mut GlesTexture,closing:&[Closing],a:&Appearance,scale:f64,border:i32,now:Instant,duration:f64)->Result<(),GlesError> {
    for c in closing {
        let alpha=(1.0-progress(c.started,now,duration*120.0/180.0)) as f32;
        decoration(r,p,canvas,c.rect,a,scale,border,c.color,alpha)?;
        // No backdrop is sampled when use_blur=0; source is safe to bind on both units.
        draw_window(r,p,canvas,&c.texture,&c.texture,c.rect,a.corner_radius as f32*scale as f32,false,alpha)?;
    }
    Ok(())
}
fn groups(state:&State,r:&mut GlesRenderer,output:&Output)->Vec<Group> {
    let mut groups=vec![];
    let scale=output.current_scale().fractional_scale();
    let origin=state.space.output_geometry(output).unwrap().loc;
    let layer_groups=|r:&mut GlesRenderer,which:&[Layer]| {
        let map=layer_map_for_output(output);
        map.layers().filter(|l|which.contains(&l.layer())).filter_map(|l| {
            let geo=map.layer_geometry(l)?;
            let rect=geo.to_physical_precise_round(scale);
            if rect.size.is_empty() {return None;}
            let elements=render_elements_from_surface_tree(r,l.wl_surface(),(0,0),scale,1.0,Kind::Unspecified);
            Some(Group{key:Key::Surface(l.wl_surface().clone()),rect,elements,decorated:false,blur:matches!(l.namespace(),"waybar"|"fuzzel"|"notifications"|"mako"),fullscreen:false,direct:true,color:state.config.unfocused})
        }).collect::<Vec<_>>()
    };
    groups.extend(layer_groups(r,&[Layer::Background,Layer::Bottom]));
    let focused=state.focused_window();
    let windows:Vec<_>=state.space.elements_for_output(output).cloned().collect();
    let fullscreen=windows.iter().any(|w|state.workspaces.iter().any(|ws|ws.entries.iter().any(|e|e.window==*w && e.fullscreen)));
    if fullscreen {groups.extend(layer_groups(r,&[Layer::Top]));}
    for window in windows {
        let Some(surface)=window.wl_surface() else {continue};
        let Some(location)=state.space.element_location(&window) else {continue};
        let geometry=window.geometry();
        let rect=Rectangle::new(location-origin,geometry.size).to_physical_precise_round(scale);
        if rect.size.is_empty() {continue;}
        let entry=state.workspaces.iter().flat_map(|w|&w.entries).find(|e|e.window==window);
        let fullscreen=entry.is_some_and(|e|e.fullscreen);
        let app=window.toplevel().map(|t|with_states(t.wl_surface(),|s|s.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap().app_id.clone().unwrap_or_default())).unwrap_or_default();
        let elements=render_elements_from_surface_tree(r,&surface,(Point::<i32,Logical>::from((0,0))-geometry.loc).to_physical_precise_round(scale),scale,1.0,Kind::Unspecified);
        groups.push(Group{key:Key::Window(window.clone()),rect,elements,decorated:entry.is_some(),blur:app=="foot"||app.starts_with("foot-"),fullscreen,direct:state.drag.as_ref().is_some_and(|d|d.window==window),color:if focused.as_ref()==Some(&window){state.config.focused}else{state.config.unfocused}});
        for (popup,offset) in PopupManager::popups_for_surface(&surface) {
            let geo=popup.geometry();
            let rect=Rectangle::new(location-origin+offset,geo.size).to_physical_precise_round(scale);
            if rect.size.is_empty(){continue;}
            groups.push(Group{key:Key::Surface(popup.wl_surface().clone()),rect,
                elements:render_elements_from_surface_tree(r,popup.wl_surface(),(Point::<i32,Logical>::from((0,0))-geo.loc).to_physical_precise_round(scale),scale,1.0,Kind::Unspecified),decorated:false,blur:false,fullscreen:false,direct:true,color:state.config.unfocused});
        }
    }
    if !fullscreen {groups.extend(layer_groups(r,&[Layer::Top]));}
    groups.extend(layer_groups(r,&[Layer::Overlay]));
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn rounded_hit_region() {
        assert!(!contains_rounded((100,80),(0.0,0.0),14));
        assert!(contains_rounded((100,80),(14.0,0.0),14));
        assert!(contains_rounded((100,80),(50.0,40.0),14));
        assert!(contains_rounded((100,80),(0.0,0.0),0));
        assert!(!contains_rounded((100,80),(100.0,40.0),0));
        assert!(contains_rounded((2,2),(1.0,1.0),64));
    }
    #[test] fn interpolation_endpoints_and_interruptions() {
        let a=Rect::new((0,0).into(),(100,80).into());let b=Rect::new((40,20).into(),(200,160).into());
        assert_eq!(interpolate(a,b,0.0),a);assert_eq!(interpolate(a,b,1.0),b);
        let mid=interpolate(a,b,0.5);assert!(mid.loc.x>0 && mid.loc.x<40);
        assert_eq!(interpolate(mid,a,0.0),mid);
    }
}
