#!/usr/bin/env python3
"""Install/restore the Rusted rice. Only explicitly listed files are ever touched.

Usage: python3 contrib/rice/install.py [--config-home DIR] [--wm PATH] [--dry-run]
       python3 contrib/rice/install.py --restore MANIFEST
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time

SOURCE = Path(__file__).resolve().parent

def digest(data): return hashlib.sha256(data).hexdigest()

def write_atomic(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
        stream.write(data); temporary = Path(stream.name)
    os.replace(temporary, path)

def restore(manifest):
    manifest = Path(manifest).resolve()
    records = json.loads(manifest.read_text())['files']
    conflicts = [r['path'] for r in records if Path(r['path']).exists() and digest(Path(r['path']).read_bytes()) != r['installed_sha256']]
    if conflicts:
        raise SystemExit('These files changed since installation; preserve them before restoring:\n' + '\n'.join(conflicts))
    for r in reversed(records):
        path = Path(r['path'])
        if r['backup']:
            if path.is_symlink(): path.unlink()
            shutil.copy2(manifest.parent / r['backup'], path, follow_symlinks=False)
        elif path.exists(): path.unlink()
    print('Restored', manifest)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config-home', type=Path, default=Path(os.environ.get('XDG_CONFIG_HOME', Path.home()/'.config')))
    parser.add_argument('--wm', type=Path, default=SOURCE.parents[1]/'target/debug/rusted-wm')
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--restore', type=Path)
    args = parser.parse_args()
    if args.restore: return restore(args.restore)
    required = ['waybar','fuzzel','mako','foot','swaybg','swaylock','playerctl']
    missing = [name for name in required if not shutil.which(name)]
    try: from PIL import Image, ImageEnhance
    except ImportError: missing.append('python Pillow')
    if not args.wm.is_file(): missing.append(str(args.wm))
    if missing: raise SystemExit('Missing dependencies: ' + ', '.join(missing))
    config_home=args.config_home.resolve();rice=config_home/'rusted-rice'
    payload = {}
    for f in SOURCE.rglob('*'):
        if f.is_file() and '__pycache__' not in f.parts and f.name not in ('install.py','README.md'):
            rel=f.relative_to(SOURCE)
            # App configs go to their native location; support files live in rusted-rice.
            app=rel.parts[0] in ('fuzzel','mako','foot','swaylock','gtk-3.0','gtk-4.0','qt5ct','qt6ct')
            dst=config_home/rel if app else rice/rel
            data=f.read_bytes()
            if f.suffix in ('.conf','.ini'): data=data.replace(b'@RICE@',str(rice).encode())
            payload[dst]=data
    with tempfile.TemporaryDirectory(prefix='rusted-rice-stage-') as tmp:
        tmp=Path(tmp)
        image=Image.open(SOURCE/'wallpapers/nixos.png').convert('RGB')
        ImageEnhance.Brightness(image).enhance(.42).save(tmp/'lock.png')
        payload[rice/'wallpapers/lock.png']=(tmp/'lock.png').read_bytes()
        cfg=config_home/'rusted-wm/config.toml'
        staged=tmp/'config.toml'
        staged.write_bytes(cfg.read_bytes() if cfg.exists() else b'autostart = []\n')
        env=os.environ|{'RUSTED_CONFIG':str(staged)}
        def wm(*cmd):
            result=subprocess.run([str(args.wm.resolve()),'config',*cmd,'--no-reload'],env=env,capture_output=True,text=True,check=True)
            return result.stdout
        values={
            'general.gap':'12','general.border':'2','apps.terminal':'foot','apps.launcher':'fuzzel',
            'apps.lock':'swaylock -f -i '+shlex.quote(str(rice/'wallpapers/lock.png')),
            'colors.focused':'#B4A1F5','colors.unfocused':'#383C50','colors.background':'#14151D','colors.pywal':'false',
            'appearance.corner_radius':'14','appearance.shadow_enabled':'true','appearance.shadow_size':'24',
            'appearance.shadow_opacity':'0.24','appearance.blur_enabled':'true','appearance.blur_radius':'8',
            'appearance.animations_enabled':'true','appearance.animation_duration_ms':'180',
            'wallpaper.path':str(rice/'wallpapers/nixos.png'),'wallpaper.mode':'fill',
        }
        for key,value in values.items(): wm('set',key,value)
        before=json.loads(wm('dump'))['autostart']
        for i,entry in reversed(list(enumerate(before))):
            try: words=shlex.split(entry)
            except ValueError: continue
            if words and (Path(words[0]).name in ('waybar','rusted-waybar','swaybg','mako') or 'waybar.py supervise' in entry):
                wm('autostart','remove',str(i+1))
        for entry in ['python3 '+shlex.quote(str(rice/'wallpaper.py')), 'python3 '+shlex.quote(str(rice/'waybar.py'))+' supervise','mako']:
            wm('autostart','add',entry)
        subprocess.run([str(args.wm.resolve()),'--check-config',str(staged)],env=env,check=True)
        payload[cfg]=staged.read_bytes()
    # Session-local environment consumed by rusted-session, never exported into another desktop.
    env_text='export XCURSOR_THEME=breeze_cursors\nexport XCURSOR_SIZE=24\nexport QT_STYLE_OVERRIDE=Fusion\n'
    if shutil.which('qt6ct') or shutil.which('qt5ct'):
        env_text+='export QT_QPA_PLATFORMTHEME=qt6ct\n' if shutil.which('qt6ct') else 'export QT_QPA_PLATFORMTHEME=qt5ct\n'
    else:
        print('Optional: qt6ct/qt5ct is absent; Qt palette files are installed for use after installing the platform theme.')
    payload[rice/'session.env']=env_text.encode()
    if args.dry_run:
        print('Would install:\n'+'\n'.join(map(str,payload)))
        return
    backup=config_home/'rusted-rice-backups'/time.strftime('%Y%m%d-%H%M%S')
    backup.mkdir(parents=True,exist_ok=False)
    records=[]
    for i,(path,data) in enumerate(payload.items()):
        saved=None
        if path.exists() or path.is_symlink():
            saved=str(i);shutil.copy2(path,backup/saved,follow_symlinks=False)
        records.append({'path':str(path),'backup':saved,'installed_sha256':digest(data)})
    manifest=backup/'manifest.json'
    manifest.write_text(json.dumps({'version':1,'files':records},indent=2))
    try:
        for path,data in payload.items(): write_atomic(path,data)
    except Exception:
        # Restore even if the transaction stopped halfway; keep the manifest for inspection.
        for record in reversed(records):
            path=Path(record['path'])
            if record['backup']:
                if path.is_symlink(): path.unlink()
                shutil.copy2(backup/record['backup'],path,follow_symlinks=False)
            elif path.exists(): path.unlink()
        raise
    print('Installed. Restart the Rusted session to activate all applications.')
    print('Restore: python3 '+shlex.quote(str(SOURCE/'install.py'))+' --restore '+shlex.quote(str(manifest)))

if __name__=='__main__': main()
