#!/usr/bin/env python3
"""Rusted's per-output Waybar adapter. No compositor-specific third-party IPC."""
import html
import json
import os
from pathlib import Path
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent

def ctl(*args):
    runtime = Path(os.environ.get('XDG_RUNTIME_DIR', '/tmp'))
    display = os.environ.get('WAYLAND_DISPLAY', '')
    path = runtime / (display + '-ctl')
    # Never attach a panel to an unrelated running session.
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(2)
        conn.connect(str(path))
        conn.sendall((' '.join(map(str, args)) + '\n').encode())
        chunks = []
        while data := conn.recv(65536):
            chunks.append(data)
    return b''.join(chunks).decode().strip()

def command(*args):
    return ' '.join(shlex.quote(str(x)) for x in [sys.executable, ROOT / 'waybar.py', *args])

def config(output):
    name = output['name']
    compact = output['width'] < 1500
    cfg = {
        'name': 'rusted', 'output': name, 'layer': 'top', 'position': 'top',
        'height': 36, 'margin-top': 10, 'margin-bottom': 0, 'margin-left': 10, 'margin-right': 10,
        'spacing': 8, 'fixed-center': True,
        'modules-left': ['custom/launcher', 'group/desktops'],
        'modules-center': ['clock'],
        'modules-right': ['custom/media', 'group/status', 'tray'],
        'group/desktops': {'orientation': 'horizontal', 'modules': [f'custom/ws{i}' for i in range(1, 10)]},
        'group/status': {'orientation': 'horizontal', 'modules': ['pulseaudio', 'network', 'battery', 'custom/system']},
        'custom/launcher': {'format': '', 'tooltip': False, 'on-click': 'fuzzel'},
        'clock': {'format': '{:%H:%M}', 'tooltip-format': '<big>{:%A, %d %B %Y}</big>\n<tt><small>{calendar}</small></tt>', 'calendar': {'mode': 'month', 'weeks-pos': 'right'}},
        'custom/media': {'exec': command('media', 16 if compact else 28), 'return-type': 'json', 'interval': 2,
            'on-click': 'playerctl play-pause', 'on-scroll-up': 'playerctl next', 'on-scroll-down': 'playerctl previous', 'escape': False},
        'pulseaudio': {'format': ' {volume}%', 'format-muted': '', 'tooltip-format': '{desc}', 'on-click': 'pavucontrol', 'on-click-right': 'wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle', 'scroll-step': 5},
        'network': {'format-wifi': '', 'format-ethernet': '', 'format-disconnected': '', 'tooltip-format-wifi': '{essid} · {signalStrength}%\n{ipaddr}', 'tooltip-format-ethernet': '{ifname}\n{ipaddr}'},
        'battery': {'format': '{icon} {capacity}%', 'format-charging': ' {capacity}%', 'format-icons': ['', '', '', '', ''], 'states': {'warning': 30, 'critical': 15}},
        'custom/system': {'format': '{}', 'exec': command('system'), 'return-type': 'json', 'interval': 5},
        'tray': {'icon-size': 16, 'spacing': 10},
    }
    for i in range(1, 10):
        cfg[f'custom/ws{i}'] = {'exec': command('workspace', name, i), 'return-type': 'json', 'interval': 1,
            'on-click': command('switch', name, i), 'tooltip': True}
    if not any(Path('/sys/class/power_supply').glob('*/capacity')):
        cfg['group/status']['modules'].remove('battery')
    return cfg

def get_media(limit):
    try:
        result = subprocess.run(['playerctl', 'metadata', '--format', '{{status}}\n{{artist}}\n{{title}}'], capture_output=True, text=True, timeout=2)
        if result.returncode: return {'text': ''}
        status, artist, title = (result.stdout.strip().split('\n') + ['', '', ''])[:3]
        if not title: return {'text': ''}
        title = title if len(title) <= limit else title[:limit-1] + '…'
        return {'text': (' ' if status == 'Playing' else ' ') + html.escape(title), 'tooltip': html.escape(f'{artist}\n{result.stdout.strip().splitlines()[-1]}'), 'class': status.lower()}
    except (OSError, subprocess.TimeoutExpired):
        return {'text': ''}

def supervise():
    children = {}
    running = True
    def stop(*_):
        nonlocal running
        running = False
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    with tempfile.TemporaryDirectory(prefix='rusted-bar-', dir=os.environ.get('XDG_RUNTIME_DIR')) as tmp:
        try:
            while running:
                try: outputs = json.loads(ctl('outputs-json'))
                except (OSError, ValueError): outputs = []
                desired = {o['name']: o for o in outputs}
                for name, (proc, size) in list(children.items()):
                    o = desired.get(name)
                    if not o or size != (o['width'], o['scale']) or proc.poll() is not None:
                        if proc.poll() is None: proc.terminate(); proc.wait(timeout=4)
                        del children[name]
                for name, o in desired.items():
                    if name in children: continue
                    path = Path(tmp) / (str(len(children)) + '.json')
                    path.write_text(json.dumps(config(o)))
                    proc = subprocess.Popen(['waybar', '-c', str(path), '-s', str(ROOT / 'style.css')], env=os.environ | {'GDK_BACKEND': 'wayland'})
                    children[name] = (proc, (o['width'], o['scale']))
                time.sleep(1)
        finally:
            for proc, _ in children.values():
                if proc.poll() is None: proc.terminate()
            for proc, _ in children.values():
                try: proc.wait(timeout=4)
                except subprocess.TimeoutExpired: proc.kill(); proc.wait()

def main():
    mode, *args = sys.argv[1:]
    if mode == 'supervise': return supervise()
    if mode == 'switch': return ctl('ws-on', *args)
    if mode == 'media': result = get_media(int(args[0]))
    elif mode == 'workspace':
        try:
            out = next(o for o in json.loads(ctl('outputs-json')) if o['name'] == args[0])
            ws = out['workspaces'][int(args[1])-1]
            state = 'active' if ws['active'] else 'occupied' if ws['windows'] else 'empty'
            result = {'text': str(ws['number']), 'class': state, 'tooltip': f"Desktop {ws['number']} · {ws['windows']} windows"}
        except (OSError, ValueError, StopIteration): result = {'text': '', 'class': 'offline'}
    elif mode == 'system':
        mem = {line.split(':')[0]: int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines()}
        used = 100 * (1 - mem['MemAvailable'] / mem['MemTotal'])
        result = {'text': '', 'tooltip': f'CPU load (1 min): {os.getloadavg()[0]:.2f}\nMemory: {used:.0f}%'}
    else: raise SystemExit('unknown command')
    print(json.dumps(result))

if __name__ == '__main__': main()
