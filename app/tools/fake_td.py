"""Stand-in for a TouchDesigner session, for testing TDXCardio without TD.

Run as the "project": TDXCardio launches `python fake.toe` when td_executable is
python and the project path is a copy of this file named *.toe. Behaviour is
read every loop from `<file>.mode` beside it (one word):

  ok        heartbeat at 60 fps (default)
  lowfps    heartbeat at 12 fps
  hang      stay alive, stop sending (a frozen main thread)
  exit      exit with code 1 (a crash)
  hold      send one `hold` for 8 s, then go silent (Cardio must wait 8 s, then call it a stall)
  tdxlu     speak TDXLauncherUtility: hello on the compat port with a cmd_port

Ports: CARDIO_PORT / COMPAT_PORT env vars (defaults 11998 / 11999).
"""

import json
import os
import socket
import sys
import threading
import time

ME = os.path.abspath(sys.argv[0])
MODE_FILE = ME + '.mode'
ID = ME.replace('\\', '/').lower() if os.name == 'nt' else ME
CARDIO = int(os.environ.get('CARDIO_PORT', '11998'))
COMPAT = int(os.environ.get('COMPAT_PORT', '11999'))
CAPS = ['ping', 'save', 'quit', 'perform']


def mode() -> str:
    try:
        return open(MODE_FILE).read().strip() or 'ok'
    except OSError:
        return 'ok'


def send(sock: socket.socket, obj: dict) -> None:
    sock.sendall((json.dumps(obj) + '\n').encode())


def reader(sock: socket.socket) -> None:
    buf = b''
    while True:
        data = sock.recv(4096)
        if not data:
            return
        buf += data
        while b'\n' in buf:
            line, buf = buf.split(b'\n', 1)
            msg = json.loads(line)
            print('<-', msg, flush=True)
            if msg.get('type') == 'cmd':
                send(sock, {'type': 'result', 'v': 1, 'req': msg['req'], 'ok': True, 'id': ID})
                if msg.get('action') == 'quit':
                    send(sock, {'type': 'bye', 'v': 1, 'id': ID, 'reason': 'quit_requested'})
                    time.sleep(0.2)
                    os._exit(0)


def tdxlu_cmd_server() -> int:
    srv = socket.socket()
    srv.bind(('127.0.0.1', 0))
    srv.listen()

    def loop() -> None:
        while True:
            c, _ = srv.accept()
            line = c.makefile().readline()
            msg = json.loads(line)
            print('<- (cmd_port)', msg, flush=True)
            c.sendall((json.dumps({'type': 'result', 'v': 1, 'req': msg.get('req'), 'ok': True}) + '\n').encode())
            c.close()

    threading.Thread(target=loop, daemon=True).start()
    return srv.getsockname()[1]


def main() -> None:
    tdxlu = mode() == 'tdxlu'
    port = COMPAT if tdxlu else CARDIO
    cmd_port = tdxlu_cmd_server() if tdxlu else None
    sock = None
    frame = 0
    held = False
    last_hello = 0.0
    while True:
        m = mode()
        if m == 'exit':
            sys.exit(1)
        if sock is None:
            try:
                sock = socket.create_connection(('127.0.0.1', port), timeout=2)
                sock.settimeout(None)
                if not tdxlu:
                    threading.Thread(target=reader, args=(sock,), daemon=True).start()
            except OSError:
                time.sleep(1)
                continue
        try:
            if time.time() - last_hello > 2:
                last_hello = time.time()
                hello = {'type': 'hello', 'v': 1, 'id': ID, 'td_pid': os.getpid()}
                hello.update({'cmd_port': cmd_port, 'utility': True, 'utility_version': 'fake'} if tdxlu
                             else {'cardio': 'fake-0.1', 'caps': CAPS, 'td_build': 'fake'})
                send(sock, hello)
            if m == 'hold' and not held:
                held = True
                send(sock, {'type': 'hold', 'v': 1, 'id': ID, 'seconds': 8, 'reason': 'loading media'})
            if m not in ('hang', 'hold'):  # hold: announce, then go silent like a long load
                frame += 60
                fps = 12.0 if m == 'lowfps' else 60.0
                send(sock, {'type': 'heartbeat', 'v': 1, 'id': ID, 'fps': fps, 'frame': frame,
                            'cook_ms': 1000.0 / fps, 'dropped': 0, 'perform': False})
        except OSError:
            sock = None
        time.sleep(1)


if __name__ == '__main__':
    main()
