#!/usr/bin/env python3
"""Mutation-test classic IRQ event propagation in a caller-owned worktree.
Usage: mutate.py WORKTREE TARGET_DIR OUTPUT_DIR
Compilation failures are not kills. No timing is collected.
"""
from pathlib import Path
import json, os, subprocess, sys
root,target,out=map(Path,sys.argv[1:]); out.mkdir(parents=True,exist_ok=True)
cases=[
 ('idle-gate','bus.rs','u32::from(self.irq_dirty)','1'),
 ('bus-propagation','bus.rs','self.irq_dirty |= irq_changed;','let _ = irq_changed;'),
 ('dispatch-propagation','periph.rs','let mut irq_changed = Dispatch::tick(self, cycles);','Dispatch::tick(self, cycles); let mut irq_changed = false;'),
 ('manual-timer','periph.rs','irq_changed |= before != timer.irq_sources();','let _ = before;'),
 ('manual-ledc','periph.rs','irq_changed |= ledc_before != self.ledc.irq_sources();','let _ = ledc_before;'),
 ('manual-rmt','periph.rs','irq_changed |= rmt_before != self.rmt.irq_sources();','let _ = rmt_before;'),
 ('routed-gpio','periph.rs','irq_changed | (gpio_before != self.gpio.raw_status())','let _ = gpio_before; irq_changed'),
 ('core-release','soc.rs','        bus.irq_dirty = true;','        let _ = bus;'),
 ('unmodeled-reset-order','../esp-soc/src/machine.rs','S::reset_core_with_bus(&mut self.cores[i], &mut self.bus, i);','S::reset_core(&mut self.cores[i], i);'),
 ('modeled-reset-order','../esp-soc/src/machine/modeled.rs','S::reset_core_with_bus(&mut self.cores[i], &mut self.bus, i);','S::reset_core(&mut self.cores[i], i);'),
 ('board-reset','bus.rs','    pub fn attach_board_devices(&mut self) {\n        self.irq_dirty = true;','    pub fn attach_board_devices(&mut self) {'),
]
env=dict(os.environ,CARGO_TARGET_DIR=str(target))
rows=[]
for name,file,old,new in cases:
 p=root/file[3:] if file.startswith('../') else root/'esp32/src'/file
 text=p.read_text(); assert text.count(old)==1,(name,text.count(old))
 try:
  p.write_text(text.replace(old,new))
  command=['nice','-n','10','cargo','+1.99.0','test','-p','esp32','irq_events','--','--nocapture']
  r=subprocess.run(command,cwd=root,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
  (out/(name+'.log')).write_text(r.stdout)
  killed=r.returncode!=0 and 'panicked at' in r.stdout and 'test result: FAILED' in r.stdout and 'could not compile' not in r.stdout
  failed=[line.split()[1] for line in r.stdout.splitlines() if line.startswith('test ') and line.endswith(' ... FAILED')]
  rows.append(dict(mutation=name,assertion_killed=killed,exit=r.returncode,tests=failed))
  (out/'mutations.json').write_text(json.dumps(rows,indent=2)+'\n')
  print(name,killed,failed,flush=True)
 finally: p.write_text(text)
assert all(r['assertion_killed'] for r in rows), 'surviving or invalid mutation'
