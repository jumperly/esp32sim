#!/usr/bin/env python3
"""Run rule-removal mutations serially in a disposable worktree; restore each source."""
import argparse, json, os, pathlib, subprocess
p = argparse.ArgumentParser()
p.add_argument('--out', type=pathlib.Path, required=True)
a = p.parse_args()
root = pathlib.Path(__file__).resolve().parents[3]
a.out.mkdir(parents=True, exist_ok=True)
env = os.environ.copy()
env.setdefault('CARGO_BUILD_JOBS', '4')
mutations = []
for chip, kind in [('esp32', 'classic'), ('esp32s3', 's3'), ('esp32c3', 'c3'), ('esp32c6', 'c6')]:
 test_name = kind+'_routed_waveform' + ('_and_alias' if kind == 'classic' else '')
 path = root / chip / 'src/bus.rs'
 text = path.read_text()
 lines = text.splitlines(keepends=True)
 callbacks = [i for i,l in enumerate(lines) if 'self.board.gpio_waveform_at(' in l]
 assert len(callbacks) == 2
 # The attachment block precedes periph_write only on C6/S3, so classify by enclosing method.
 for i in callbacks:
  before = ''.join(lines[:i])
  attachment = before.rfind('pub fn attach_board_devices(') > before.rfind('fn periph_write(')
  label = 'reset' if attachment else 'write'
  # Keep signatures and all existing paths; omit only the new notification.
  modified = lines.copy()
  modified[i] = ''
  mutations.append((kind+'-'+label, path, text, ''.join(modified), 'esp32sim', test_name))
 # Remove opt-in from the register-write notification alone.
 i = next(i for i in callbacks if ''.join(lines[:i]).rfind('fn periph_write(') > ''.join(lines[:i]).rfind('pub fn attach_board_devices('))
 modified = lines.copy()
 modified[i-1] = modified[i-1].replace('&& self.board.uses_gpio_waveform()', '&& true')
 mutations.append((kind+'-optout',path,text,''.join(modified),'esp32sim',test_name))
path = root/'esp-soc/src/pins.rs'
text = path.read_text()
for name, old, new in [
 ('mux', 'if !self.function(pin, 1)', 'if !self.valid_pin(pin)'),
 ('route', 'if route & (signal * 2 - 1) != signal', 'if false'),
 ('enable', 'enabled.then(||', 'true.then(||'),
 ('output-inversion', ' ^ (route & (signal * 2) != 0)', ''),
 ('enable-inversion', ' ^ (route & (signal * 8) != 0)', ''),
]:
 assert old in text
 mutations.append((name,path,text,text.replace(old,new,1),'esp-soc','software_drive_respects_mux_route_enable_and_inversion'))
results=[]
for name,path,original,modified,package,test in mutations:
 try:
  path.write_text(modified)
  command=['nice','-n','10','cargo','+1.99.0','test','-p',package,'--test','routed_waveform',test,'--','--exact']
  with (a.out/(name+'.log')).open('w') as log:
   rc=subprocess.run(command,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT).returncode
  output=(a.out/(name+'.log')).read_text()
  killed=rc!=0 and 'test result: FAILED.' in output and 'error[E' not in output
  results.append(dict(mutation=name,test=test,killed=killed,exit_code=rc))
  (a.out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
  print(name,'KILLED' if killed else 'SURVIVED/INVALID',flush=True)
  if not killed: raise SystemExit(1)
 finally: path.write_text(original)
print(len(results),'semantic mutations killed',flush=True)
