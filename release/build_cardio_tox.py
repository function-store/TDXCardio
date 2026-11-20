"""Build the Cardio companion COMP from source and save it as release/Cardio.tox.

The tox is made from the files in this repo (Cardio/CardioExt.py and the
network below, which mirrors Cardio.tdxn), so it never carries Embody, file
links or dev values. Run it in the OLDEST TouchDesigner build the release
must load in (newer builds open older toxes, not the other way round), in any
project, from the textport:

	CARDIO_REPO = 'C:/path/to/TDXCardio'
	exec(open(CARDIO_REPO + '/release/build_cardio_tox.py', encoding='utf-8').read())

Optional, set before the exec line:
	CARDIO_SAVE = False      build and check only, write no file
	CARDIO_OUT = 'D:/x.tox'  write somewhere else than release/Cardio.tox
	CARDIO_KEEP = True       leave the built COMP in /_cardio_build to look at

The build happens inside a temporary /_cardio_build COMP, which is deleted
afterwards unless CARDIO_KEEP. Nothing else in the open project changes.

Keep this file ASCII and on old API: it must run in TD 2023.
"""
import os

REPO = globals().get('CARDIO_REPO') or project.folder
SAVE = globals().get('CARDIO_SAVE', True)
OUT = globals().get('CARDIO_OUT') or (REPO + '/release/Cardio.tox')
KEEP = globals().get('CARDIO_KEEP', False)
EXT_SOURCE = REPO + '/Cardio/CardioExt.py'

PAREXEC = '''# One dispatcher: handlers are _on<Par>ValueChange / _on<Par>Pulse on the extension.

def onValueChange(par, prev):
	parent.Cardio.ext.Cardio.dispatchPar(par, prev)

def onPulse(par):
	parent.Cardio.ext.Cardio.dispatchPar(par, pulse=True)
'''

EXEC_FRAME = '''# Main-thread pulse: a hung main thread stops heartbeats, which is the point.

def onFrameEnd(frame):
	parent.Cardio.ext.Cardio.onFrame()

def onExit():
	parent.Cardio.ext.Cardio.onProjectExit()
'''

TCPIP_CALLBACKS = '''# TCP/IP DAT callbacks: lines from TDXCardio go to the Cardio extension.

def onConnect(dat, peer):
	parent.Cardio.ext.Cardio.onConnected()

def onReceive(dat, rowIndex, message, byteData, peer):
	parent.Cardio.ext.Cardio.onLine(message)

def onClose(dat, peer):
	parent.Cardio.ext.Cardio.onDisconnected()
'''


def _log(msg):
	print('[Cardio build] ' + msg)


def _set(o, name, value, expr=False):
	"""Set a parameter if this build has it; report it otherwise."""
	par = getattr(o.par, name, None)
	if par is None:
		_log('WARNING %s has no parameter %s in this build' % (o.path, name))
		return False
	if expr:
		par.expr = value
	else:
		par.val = value
	return True


def _set_extension(comp, obj, name):
	"""Extension 0 (sequence pars in newer builds, extension1/extname1 in older ones)."""
	if hasattr(comp.par, 'ext0object'):
		comp.par.ext0object = obj
		comp.par.ext0name = name
		comp.par.ext0promote = True
	else:
		comp.par.extension1 = obj
		comp.par.extname1 = name
		comp.par.promoteextension1 = True


def build(parent_comp):
	with open(EXT_SOURCE, encoding='utf-8') as f:
		ext_text = f.read()

	c = parent_comp.create(baseCOMP, 'Cardio')
	c.color = (0.75, 0.2, 0.25)
	c.par.parentshortcut = 'Cardio'

	ext = c.create(textDAT, 'CardioExt')
	ext.text = ext_text
	ext.nodeX, ext.nodeY = 0, 0

	tcp = c.create(tcpipDAT, 'tcpip_cardio')
	tcp.nodeX, tcp.nodeY = 400, 0
	for d in list(tcp.docked):	# the default callbacks DAT; ours replaces it
		d.destroy()
	_set(tcp, 'address', '127.0.0.1')
	_set(tcp, 'port', 'parent.Cardio.par.Port', expr=True)
	_set(tcp, 'maxlines', 20)

	cb = c.create(textDAT, 'tcpip_callbacks')
	cb.text = TCPIP_CALLBACKS
	cb.nodeX, cb.nodeY = 400, -120
	_set(tcp, 'callbacks', 'tcpip_callbacks')
	cb.dock = tcp

	ex = c.create(executeDAT, 'exec_frame')
	ex.text = EXEC_FRAME
	ex.nodeX, ex.nodeY = 800, 0
	_set(ex, 'frameend', True)
	_set(ex, 'exit', True)

	pe = c.create(parameterexecuteDAT, 'parexec')
	pe.text = PAREXEC
	pe.nodeX, pe.nodeY = 1200, 0
	_set(pe, 'op', '..')
	_set(pe, 'pars', '*')
	_set(pe, 'builtin', False)

	ps = c.create(performCHOP, 'perform_stats')
	ps.nodeX, ps.nodeY = 1600, 0
	for name in ('msec', 'droppedframes', 'gpumemused'):
		_set(ps, name, True)

	_set_extension(c, "op('./CardioExt').module.CardioExt(me)", 'Cardio')
	# __init__ creates the custom parameters (get-or-create), so they are in the tox
	c.initializeExtensions()
	return c


def check(c):
	problems = []
	if c.findChildren(name='Embody') or c.findChildren(name='Envoy*'):
		problems.append('contains Embody/Envoy')
	for d in c.findChildren(type=DAT):
		if hasattr(d.par, 'file') and str(d.par.file.eval()):
			problems.append('%s links a file (%s)' % (d.path, d.par.file.eval()))
	names = sorted(ch.name for ch in c.children)
	want = sorted(['CardioExt', 'tcpip_cardio', 'tcpip_callbacks', 'exec_frame', 'parexec', 'perform_stats'])
	if names != want:
		problems.append('children %s, expected %s' % (names, want))
	pars = [p.name for p in c.customPars]
	for name in ('Active', 'Port', 'Interval', 'Watchid', 'Holdseconds', 'Hold', 'Release',
	             'Connected', 'Phase', 'Restarts', 'Status', 'Version'):
		if name not in pars:
			problems.append('custom par %s missing' % name)
	if not hasattr(c, 'Hold'):
		problems.append('extension not promoted (no Hold method)')
	return problems


def main():
	_log('TD %s, source %s' % (app.build, EXT_SOURCE))
	scratch = op('/_cardio_build')
	if scratch is not None:
		scratch.destroy()
	scratch = op('/').create(baseCOMP, '_cardio_build')
	scratch.nodeX, scratch.nodeY = -2000, -2000
	try:
		c = build(scratch)
		problems = check(c)
		if problems:
			_log('STOP, not saved: ' + '; '.join(problems))
			return
		if SAVE:
			os.makedirs(os.path.dirname(OUT), exist_ok=True)
			c.save(OUT)
			_log('saved %s (%d bytes)' % (OUT, os.path.getsize(OUT)))
		else:
			_log('built and checked, CARDIO_SAVE is False: nothing written')
	finally:
		if not KEEP:
			scratch.destroy()
	_log('done')


main()
