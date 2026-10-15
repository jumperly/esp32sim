#!/usr/bin/env python3
"""Remove one gate at a time; each must fail a named call-counter test."""
import pathlib, subprocess, os, json, sys
root=pathlib.Path(sys.argv[1]).resolve(); out=pathlib.Path(os.environ['EVIDENCE_OUT']).resolve()
env=dict(os.environ,CARGO_TARGET_DIR=str(out/'target'))
cases=[
('ledc-call','periph.rs','if self.ledc.clock() == Some(ClockDomain::Apb) { self.ledc.tick(ticks); }','self.ledc.tick(ticks);','classic_idle_gates_skip_adapter_calls'),
('rmt-call','periph.rs','if self.rmt.clock() == Some(ClockDomain::Apb) { self.rmt.tick(ticks); }','self.rmt.tick(ticks);','classic_idle_gates_skip_adapter_calls'),
('wifi-call','bus.rs','if self.periph.wifi.clock_enabled { self.wifi_step(); }','self.wifi_step();','classic_wifi_clock_and_reset_skip_step'),
('wifi-clock','periph.rs','self.dport.ram.read(0xcc) & 0x406 == 0x406','true','classic_wifi_clock_and_reset_skip_step'),
('wifi-reset','periph.rs','&& self.dport.ram.read(0xd0) & (1 << 2) == 0','&& true','classic_wifi_clock_and_reset_skip_step'),
('i2s-clock','periph.rs','self.i2s[port].clock_enabled = self.dport.ram.read(0xc0) & (1 << bit) != 0','self.i2s[port].clock_enabled = true','classic_i2s_clock_reset_and_pending_gates'),
('i2s-reset','periph.rs','&& self.dport.ram.read(0xc4) & (1 << bit) == 0','&& true','classic_i2s_clock_reset_and_pending_gates'),
('i2s-pending','i2s.rs',' || !self.periph.i2s[port].dma.running',' || false','classic_i2s_clock_reset_and_pending_gates'),
]
results=[]
for name,file,old,new,test in cases:
 p=root/'esp32/src'/file; original=p.read_text(); assert original.count(old)==1,(name,original.count(old))
 try:
  p.write_text(original.replace(old,new))
  with (out/('mutation-'+name+'.log')).open('w') as f: r=subprocess.run(['nice','-n','10','cargo','+1.99.0','test','--release','-p','esp32',test,'--','--nocapture'],cwd=root,env=env,stdout=f,stderr=subprocess.STDOUT)
  log=(out/('mutation-'+name+'.log')).read_text(); killed=r.returncode!=0 and 'test result: FAILED' in log and 'assertion' in log
  results.append({'mutation':name,'test':test,'exit':r.returncode,'killed':killed}); (out/'mutations.json').write_text(json.dumps(results,indent=2)+'\n'); print(name,killed,flush=True)
  if not killed: sys.exit(1)
 finally: p.write_text(original)
