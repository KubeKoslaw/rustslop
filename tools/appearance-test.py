#!/usr/bin/env python3
"""GPU pixel tests. Uses an isolated headless session and only its own child processes."""
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time
from PIL import Image

ROOT=Path(__file__).resolve().parents[1]
OUT=Path(os.environ.get('RUSTED_TEST_OUT','/tmp/rusted-appearance-tests'));OUT.mkdir(exist_ok=True)
children=[]
with tempfile.TemporaryDirectory(prefix='rusted-appearance-runtime-') as runtime:
    os.chmod(runtime,0o700)
    cfg=OUT/'config.toml'
    config='''autostart=[]
[general]
gap=40
border=2
xwayland=false
[colors]
background="#14151d"
focused="#b4a1f5"
[appearance]
corner_radius=14
shadow_enabled=false
blur_enabled=true
animations_enabled=false
'''
    cfg.write_text(config)
    env=os.environ|{'XDG_RUNTIME_DIR':runtime,'RUSTED_CONFIG':str(cfg),'GDK_BACKEND':'wayland'}
    log=open(OUT/'wm.log','w')
    wm=subprocess.Popen([str(ROOT/'target/debug/rusted-wm'),'--headless'],env=env,stdout=log,stderr=log)
    children.append(wm)
    try:
        for _ in range(100):
            paths=list(Path(runtime).glob('rusted-*-ctl'))
            if paths:break
            if wm.poll() is not None:raise RuntimeError((OUT/'wm.log').read_text())
            time.sleep(.05)
        control=paths[0];env['WAYLAND_DISPLAY']=control.name.removesuffix('-ctl')
        def ctl(*args):
            with socket.socket(socket.AF_UNIX) as s:
                s.settimeout(5);s.connect(str(control));s.sendall((' '.join(map(str,args))+'\n').encode());result=b''
                while part:=s.recv(65536):result+=part
            return result.decode().strip()
        def spawn(mode):
            p=subprocess.Popen([str(ROOT/'target/debug/examples/appearance_client'),mode],env=env,stdout=log,stderr=log);children.append(p);time.sleep(.25);return p
        def shot(name):
            path=OUT/(name+'.png');assert ctl('shot',path)=='ok';return Image.open(path).convert('RGB')
        def check(name,condition):
            assert condition,name
            print('PASS',name,flush=True)
        def change(old,new):
            cfg.write_text(cfg.read_text().replace(old,new));assert ctl('reload')=='ok';time.sleep(.3)
        bg=spawn('backdrop');base=shot('background')
        win=spawn('window');im=shot('rounded')
        check('rounded corner reveals backdrop',im.getpixel((40,40))==base.getpixel((40,40)))
        check('window interior preserved',im.getpixel((80,80))==(204,34,68))
        check('rounded border follows top edge',max(abs(a-b) for a,b in zip(im.getpixel((60,38)),(180,161,245)))<4)
        ctl('float');ctl('drag','move',100,100,240,220);time.sleep(.25)
        floating=shot('floating')
        change('shadow_enabled=false','shadow_enabled=true')
        shadow=shot('shadow')
        check('shadow affects pixels outside window',shadow.tobytes()!=floating.tobytes())
        # Back to the reference look: later checks compare pixels against `base`.
        change('shadow_enabled=true','shadow_enabled=false')
        ctl('fullscreen');time.sleep(.25);full=shot('fullscreen')
        check('fullscreen has square corners and no border',full.getpixel((0,0))==(204,34,68))
        ctl('fullscreen');win.terminate();win.wait();time.sleep(.2)
        glass=spawn('glass');blur=shot('blur')
        check('opaque detail stays sharp with blur',blur.getpixel((105,105))==(0,255,0))
        change('blur_enabled=true','blur_enabled=false');plain=shot('no-blur')
        diff=lambda image:abs(image.getpixel((100,200))[1]-image.getpixel((104,200))[1])
        check('backdrop blur reduces stripe contrast',diff(blur)<diff(plain)/2)
        glass.terminate();glass.wait();time.sleep(.2)
        change('blur_enabled=false','blur_enabled=true')
        islands=spawn('islands');panel=shot('islands')
        check('transparent gap between panels has no blur',panel.getpixel((500,30))==base.getpixel((500,30)))
        check('translucent island blurs its own backdrop',abs(panel.getpixel((100,30))[1]-panel.getpixel((104,30))[1])<20)
        islands.terminate();islands.wait();time.sleep(.2)
        win=spawn('window')
        for scale in (1.25,2):
            text=cfg.read_text();text=text.replace('[general]','[general]\nscale='+str(scale),1) if 'scale=' not in text else re.sub(r'scale=[0-9.]+','scale='+str(scale),text)
            cfg.write_text(text);assert ctl('reload')=='ok';time.sleep(.3);scaled=shot('scale-'+str(scale))
            # The backdrop is a greyscale stripe pattern, so a clipped corner must be grey while
            # window content (magenta) and the border (lavender) are strongly coloured.
            xy=round(40*scale);corner=scaled.getpixel((xy,xy))
            check('rounded clip at scale '+str(scale),max(corner)-min(corner)<6)
            check('surface content scales with output '+str(scale),scaled.getpixel((round(80*scale),round(80*scale)))==(204,34,68))
        change('scale=2','scale=1')
        ctl('output','add',800,600);time.sleep(.2)
        import json
        outputs=json.loads(ctl('outputs-json'));other=outputs[-1]['name']
        ctl('ws-on',other,3)
        outputs=json.loads(ctl('outputs-json'))
        check('workspace command targets its own monitor',outputs[0]['workspaces'][0]['active'] and outputs[-1]['workspaces'][2]['active'])
        ctl('ws-on',outputs[0]['name'],1);time.sleep(.1)
        first=shot('screencopy-reference')
        subprocess.run([str(ROOT/'target/debug/examples/screencopy_client'),str(OUT/'screencopy.png')],env=env,check=True,stdout=log)
        check('screencopy equals composed screenshot',Image.open(OUT/'screencopy.png').convert('RGB').tobytes()==first.tobytes())
        change('animations_enabled=false','animations_enabled=true')
        ctl('ws',2);time.sleep(.3);settled=shot('workspace-settled')
        check('workspace animation settles without old window pixels',settled.getpixel((80,200))==base.getpixel((80,200)))
        ctl('ws',1);time.sleep(.3);win.terminate();win.wait();time.sleep(.3)
        closed=shot('close-settled');check('close snapshot expires',closed.getpixel((80,200))==base.getpixel((80,200)))
        locker=subprocess.Popen([str(ROOT/'target/debug/examples/lock_client')],env=env,stdout=log,stderr=log);children.append(locker);time.sleep(.3)
        check('screenshots refused during lock',ctl('shot',OUT/'forbidden.png').startswith('error'))
        locker.terminate();locker.wait();time.sleep(.2)
        check('locker death does not reveal previous scene','locked=true' in ctl('info'))
        check('no effect fallback occurred','appearance effects disabled' not in (OUT/'wm.log').read_text())
    finally:
        for p in reversed(children):
            if p.poll() is None:p.terminate()
        for p in children:
            try:p.wait(timeout=3)
            except subprocess.TimeoutExpired:p.kill();p.wait()
print('All appearance pixel tests passed. Artifacts:',OUT)
