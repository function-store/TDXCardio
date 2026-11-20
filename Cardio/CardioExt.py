"""
Cardio: TouchDesigner companion for the TDXCardio watchdog app.

Connects out to TDXCardio on 127.0.0.1:<Port> (11998) and sends `hello`
(every 2 s) and `heartbeat` (every Interval s) lines with fps, frame time and
dropped frames. TDXCardio's commands (ping, save, quit, perform) and its
`watch` state come back on the SAME connection. Contract: docs/PROTOCOL.md.

Standalone: no Embody, no TDX Launcher Ultra needed. Heartbeats run from an
Execute DAT on the main thread, so a hung main thread stops them, which is
exactly what the watchdog must see.

TDXCardio: https://github.com/function-store/TDXCardio
MIT License, Copyright (c) 2026 Function Store (full text: LICENSE in the repo)
"""

from __future__ import annotations

import json
import os
import time
from typing import Any, Optional

CARDIO_VERSION = '0.1.1'
_REPO_URL = 'https://github.com/function-store/TDXCardio'
_LICENSE = 'MIT License, Copyright (c) 2026 Function Store'
_HELLO_SECS = 2.0
_CAPS = ['ping', 'save', 'quit', 'perform']


def _ensurePage(comp: COMP, name: str) -> Page:
	for page in comp.customPages:
		if page.name == name:
			return page
	return comp.appendCustomPage(name)


def _ensurePar(comp: COMP, page: Page, name: str, style: str, label: str, help: str,
		default: Any = None, **attrs: Any) -> Par:
	"""Get-or-create: code owns the schema, the user owns the value."""
	existing = [p for p in comp.customPars if p.tupletName == name]
	created = not existing
	if created:
		getattr(page, 'append' + style)(name, label=label)
		existing = [p for p in comp.customPars if p.tupletName == name]
	elif existing[0].style != style:
		raise ValueError(f'Cardio: par {name} is {existing[0].style}, declared {style}; not replacing it')
	p = existing[0]
	p.label = label
	p.help = help
	for key, value in attrs.items():
		setattr(p, key, value)
	if default is not None:
		p.default = default
		if created:
			p.val = default
	return p


class CardioExt:
	"""Heartbeat + command channel to the TDXCardio watchdog."""

	def __init__(self, ownerComp: COMP) -> None:
		self.ownerComp = ownerComp
		self.Outputs = {
			'Connected': tdu.Dependency(0),
			'Phase': tdu.Dependency('not watched'),
			'Restarts': tdu.Dependency(0),
			'Status': tdu.Dependency('starting'),
		}
		self._lastBeat = 0.0
		self._lastHello = 0.0
		self._beatFrame = None
		self._beatTime = None
		self._ensurePars()

	def onInitTD(self) -> None:
		run('args[0].postInit()', self, delayFrames=5)

	def postInit(self) -> None:
		"""Idempotent: bind outputs once the network is cooked."""
		for name in self.Outputs:
			par = getattr(self.ownerComp.par, name, None)
			if par is None:
				continue
			par.bindExpr = f"me.ext.Cardio.Outputs['{name}']"
			par.mode = ParMode.BIND
		self._setConnected(0)
		# The DAT cannot report whether it is connected, and a reinit lost our
		# record of it: cycle the socket so onConnect fires for this instance.
		tcp = self.ownerComp.op('tcpip_cardio')
		if tcp is not None:
			tcp.par.active = False
			run('args[0].par.active = True', tcp, delayFrames=2)

	def onDestroyTD(self) -> None:
		return

	# ---- parameters -------------------------------------------------------

	def _ensurePars(self) -> None:
		c = self.ownerComp
		pg = _ensurePage(c, 'Cardio')
		_ensurePar(c, pg, 'Active', 'Toggle', 'Active',
			'Send heartbeats to TDXCardio. Off stops them, and a watching TDXCardio will then '
			'treat this project as stalled; use Hold to pause watching instead.', default=1)
		_ensurePar(c, pg, 'Port', 'Int', 'TDXCardio Port',
			'TCP port of the TDXCardio app on 127.0.0.1 (its cardio_port, 11998 by default).',
			default=11998, min=1, max=65535, clampMin=True, clampMax=True, normMin=11990, normMax=12010)
		_ensurePar(c, pg, 'Interval', 'Float', 'Heartbeat Interval',
			'Seconds between heartbeats. Keep it well under the timeout_secs set in cardio.json.',
			default=1.0, min=0.25, max=10.0, clampMin=True, clampMax=True, normMin=0.25, normMax=5.0)
		_ensurePar(c, pg, 'Watchid', 'Str', 'Watch ID Override',
			'Leave empty: the id is this project\'s .toe path, the same id TDX Launcher Ultra uses. '
			'Set it only to report as a different project.')
		hold = _ensurePar(c, pg, 'Holdseconds', 'Float', 'Hold Seconds',
			'How long Hold suspends stall detection (max 600). Use before a known long stall: a big load, a save, an offline render.',
			default=120.0, min=1.0, max=600.0, clampMin=True, clampMax=True, normMin=10, normMax=600)
		hold.startSection = True
		_ensurePar(c, pg, 'Hold', 'Pulse', 'Hold',
			'Tell TDXCardio not to treat missing heartbeats as a stall for Hold Seconds.')
		_ensurePar(c, pg, 'Release', 'Pulse', 'Release Hold',
			'End a Hold early.')

		st = _ensurePage(c, 'Status')
		conn = _ensurePar(c, st, 'Connected', 'Toggle', 'Connected',
			'On while the connection to TDXCardio is open. Driven by the extension.')
		conn.startSection = True
		_ensurePar(c, st, 'Phase', 'Str', 'Watch Phase',
			'Phase TDXCardio reports for this project: watching, grace, held, unhealthy... '
			'"not watched" when TDXCardio is not running or does not watch this project.')
		_ensurePar(c, st, 'Restarts', 'Int', 'Restarts',
			'How many times TDXCardio has restarted this project since it started watching.')
		_ensurePar(c, st, 'Status', 'Str', 'Status',
			'Last heartbeat result or error.')
		_ensurePar(c, st, 'Version', 'Str', 'Version',
			'Cardio companion version.', default=CARDIO_VERSION)
		c.par.Version.val = CARDIO_VERSION

		ab = _ensurePage(c, 'About')
		repo = _ensurePar(c, ab, 'Repository', 'Str', 'Repository',
			'Source, documentation and releases of TDXCardio and this companion.')
		repo.startSection = True
		_ensurePar(c, ab, 'Openrepository', 'Pulse', 'Open Repository',
			'Open the TDXCardio repository (README, setup, protocol) in the web browser.')
		lic = _ensurePar(c, ab, 'License', 'Str', 'License',
			'Cardio and TDXCardio are released under the MIT License; the full text is LICENSE in the repository.')
		# code-owned facts, not user values: rewritten on every init, read-only
		for p, value in ((repo, _REPO_URL), (lic, _LICENSE)):
			p.val = value
			p.default = value
			p.readOnly = True

	def _onOpenrepositoryPulse(self, par: Par) -> None:
		ui.viewFile(_REPO_URL)

	def _onHoldPulse(self, par: Par) -> None:
		self.Hold(float(self.ownerComp.par.Holdseconds.eval()), 'Hold pulse')

	def _onReleasePulse(self, par: Par) -> None:
		self.Release()

	def _onActiveValueChange(self, par: Par, prev: Any) -> None:
		self.Outputs['Status'].val = 'active' if par.eval() else 'inactive'

	# ---- public API -------------------------------------------------------

	def ProjectWatchId(self) -> Optional[str]:
		"""Normalized absolute .toe path; identical to TDX Launcher Ultra's id."""
		override = str(self.ownerComp.par.Watchid.eval() or '').strip()
		path = override
		if not path:
			if not project.folder or not project.name:
				return None
			name = str(project.name)
			path = os.path.join(project.folder, name if name.lower().endswith('.toe') else name + '.toe')
		p = os.path.normpath(os.path.abspath(path))
		if p.startswith('\\\\?\\'):
			p = p[4:]
		p = p.replace('\\', '/')
		return p.lower() if os.name == 'nt' else p

	def Hold(self, seconds: float = 120.0, reason: str = '') -> bool:
		"""Suspend stall detection in TDXCardio for `seconds` (max 600)."""
		return self._send({'type': 'hold', 'seconds': max(1.0, min(600.0, float(seconds))), 'reason': reason})

	def Release(self) -> bool:
		"""End a Hold early."""
		return self._send({'type': 'release'})

	# ---- wiring (Execute DAT, TCP/IP callbacks, parexec) ------------------

	def onFrame(self) -> None:
		if not self.ownerComp.par.Active.eval():
			return
		now = time.monotonic()
		if now - self._lastHello >= _HELLO_SECS:
			self._lastHello = now
			self._sendHello()
		interval = max(0.25, float(self.ownerComp.par.Interval.eval()))
		if now - self._lastBeat >= interval:
			self._lastBeat = now
			self._sendHeartbeat(now)

	def onConnected(self) -> None:
		self._setConnected(1)
		self._lastHello = 0.0  # hello on the next frame

	def onDisconnected(self) -> None:
		self._setConnected(0)
		self.Outputs['Phase'].val = 'not watched'

	def onLine(self, line: str) -> None:
		line = line.strip()
		if not line:
			return
		try:
			msg = json.loads(line)
		except Exception:
			return
		kind = msg.get('type')
		if kind == 'watch':
			self.Outputs['Phase'].val = str(msg.get('phase', ''))
			self.Outputs['Restarts'].val = int(msg.get('restarts', 0))
		elif kind == 'cmd':
			self._runCommand(msg)

	def onProjectExit(self) -> None:
		"""Clean quit: TDXCardio will not relaunch unless relaunch_on_clean_exit."""
		self._send({'type': 'bye', 'reason': 'quit'})

	def dispatchPar(self, par: Par, prev: Any = None, pulse: bool = False) -> None:
		name = f"_on{par.name}{'Pulse' if pulse else 'ValueChange'}"
		handler = getattr(self, name, None)
		if handler is None:
			return
		if pulse:
			handler(par)
		else:
			handler(par, prev)

	# ---- private ----------------------------------------------------------

	def _runCommand(self, msg: dict) -> None:
		action = msg.get('action')
		reply: dict = {'type': 'result', 'req': msg.get('req'), 'ok': True}
		try:
			if action == 'ping':
				reply['version'] = CARDIO_VERSION
			elif action == 'save':
				project.save()
			elif action == 'perform':
				ui.performMode = bool(msg.get('on', True))
				if 'topmost' in msg:
					project.windowOnTop = bool(msg['topmost'])
			elif action == 'quit':
				if msg.get('save'):
					project.save()
				self._send(reply)
				self._send({'type': 'bye', 'reason': 'quit_requested'})
				# Later frames, so the reply and bye leave the socket first.
				run('project.quit(force=True)', delayFrames=10)
				return
			else:
				reply = {'type': 'result', 'req': msg.get('req'), 'ok': False, 'error': f'unknown action {action}'}
		except Exception as e:
			reply = {'type': 'result', 'req': msg.get('req'), 'ok': False, 'error': str(e)}
		self._send(reply)

	def _sendHello(self) -> None:
		self._send({
			'type': 'hello', 'td_pid': os.getpid(), 'cardio': CARDIO_VERSION,
			'td_build': str(app.build), 'caps': _CAPS,
		})

	def _sendHeartbeat(self, now: float) -> None:
		frame = int(absTime.frame)
		fps = None
		if self._beatFrame is not None and now > self._beatTime:
			fps = (frame - self._beatFrame) / (now - self._beatTime)
		self._beatFrame, self._beatTime = frame, now
		beat = {'type': 'heartbeat', 'frame': frame, 'perform': bool(ui.performMode)}
		if fps is not None:
			beat['fps'] = round(fps, 2)
		stats = self.ownerComp.op('perform_stats')
		if stats is not None:
			for key, chan in (('cook_ms', 'msec'), ('dropped', 'dropped_frames'), ('gpu_mb', 'gpu_mem_used')):
				ch = stats.chan(chan)
				if ch is not None:
					beat[key] = round(float(ch.eval()), 3) if key != 'dropped' else int(ch.eval())
		if self._send(beat):
			self.Outputs['Status'].val = f"ok {time.strftime('%H:%M:%S')}" + (f' {fps:.1f} fps' if fps else '')

	def _send(self, payload: dict) -> bool:
		out = self.ownerComp.op('tcpip_cardio')
		wid = self.ProjectWatchId()
		if out is None or not wid or not self._isConnected():
			return False
		payload = {'v': 1, 'id': wid, **payload}
		try:
			out.send(json.dumps(payload), terminator='\n')
			return True
		except Exception as e:
			self.Outputs['Status'].val = f'send failed: {e}'
			return False

	def _isConnected(self) -> bool:
		return bool(self.Outputs['Connected'].val)

	def _setConnected(self, on: int) -> None:
		self.Outputs['Connected'].val = int(bool(on))
		if not on:
			self.Outputs['Status'].val = 'waiting for TDXCardio'
