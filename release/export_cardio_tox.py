"""Export release/Cardio.tox from the /Cardio COMP in TDXCardio.toe.

Run in TDXCardio.toe, from the textport (or through Envoy's execute_python):

	exec(open(project.folder + '/release/export_cardio_tox.py', encoding='utf-8').read())

Why a script: Embody's ExportPortableTox() exports the EMBODY COMP when it is
called without target=, and that is how release/Cardio.tox once shipped Embody
instead of Cardio. This script always passes target=/Cardio, then checks the
file it wrote and refuses to keep it unless it is the Cardio COMP.

Steps:
 1. op.Embody.ExportPortableTox(target=op('/Cardio'), ...): file links and
    Embody tags stripped in the saved copy, the live /Cardio restored.
 2. Loads the saved tox into a temporary COMP, resets the user parameters to
    their defaults (the dev toe carries test values, e.g. Hold Seconds), checks
    it (named Cardio, its six operators, no Embody, no file links, no tags,
    every custom parameter) and saves it again over release/Cardio.tox.
 3. On any failed check the file is deleted and the reason printed.

Nothing in the toe changes; do not save the toe for this.
"""
import os

OUT = project.folder + '/release/Cardio.tox'
OPS = ['CardioExt', 'exec_frame', 'parexec', 'perform_stats', 'tcpip_callbacks', 'tcpip_cardio']
USER_PARS = ['Active', 'Port', 'Interval', 'Watchid', 'Holdseconds']
ALL_PARS = USER_PARS + ['Hold', 'Release', 'Connected', 'Phase', 'Restarts', 'Status', 'Version']


def _log(msg):
	print('[Cardio export] ' + msg)


def _problems(c):
	found = []
	if c.name != 'Cardio':
		found.append('top COMP is %s, not Cardio' % c.name)
	if c.findChildren(name='Embody*') or c.findChildren(name='Envoy*'):
		found.append('contains Embody/Envoy')
	names = sorted(x.name for x in c.children)
	if names != OPS:
		found.append('operators %s, expected %s' % (names, OPS))
	for d in c.findChildren(type=DAT):
		if hasattr(d.par, 'file') and str(d.par.file.eval()):
			found.append('%s links file %s' % (d.path, d.par.file.eval()))
	if c.tags:
		found.append('tags %s' % list(c.tags))
	pars = [p.name for p in c.customPars]
	missing = [n for n in ALL_PARS if n not in pars]
	if missing:
		found.append('custom pars missing: %s' % missing)
	return found


def export():
	src = op('/Cardio')
	if src is None:
		_log('STOP: no /Cardio in this project')
		return False
	if not op.Embody.ExportPortableTox(target=src, save_path=OUT):
		_log('STOP: ExportPortableTox reported a failure')
		return False
	holder = op('/').create(baseCOMP, '_cardio_release_check')
	holder.nodeX, holder.nodeY = -3000, -3000
	try:
		c = holder.loadTox(OUT)
		for name in USER_PARS:
			p = getattr(c.par, name)
			if p.val != p.default:
				_log('%s %s -> default %s' % (name, p.val, p.default))
				p.val = p.default
		found = _problems(c)
		if found:
			os.remove(OUT)
			_log('STOP, %s deleted: %s' % (OUT, '; '.join(found)))
			return False
		c.save(OUT)
	finally:
		holder.destroy()
	_log('saved %s (%d bytes) from /Cardio, TD %s' % (OUT, os.path.getsize(OUT), app.build))
	return True


export()
