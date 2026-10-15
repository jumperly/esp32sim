#!/usr/bin/env python3
"""Temporary counter-only instrumentation; restores every source in finally."""
import pathlib, subprocess, os, json, sys, re, hashlib
root=pathlib.Path(sys.argv[1]).resolve(); out=pathlib.Path(os.environ['EVIDENCE_OUT']).resolve()
label=sys.argv[2]; originals={}
def edit(p,old,new):
 p=root/p; text=p.read_text(); originals.setdefault(p,text)
 assert text.count(old)==1,(p,old,text.count(old)); p.write_text(text.replace(old,new))
def count(i): return f'esp32::periph::ATTR[{i}].fetch_add(1, std::sync::atomic::Ordering::Relaxed);'
def local(i): return count(i).replace('esp32::','crate::')
try:
 if label == 'base':
  for name in ['periph','bus','wifi','i2s','ledc','rmt']:
   p=root/'esp32/src'/f'{name}.rs'; originals[p]=p.read_text()
   p.write_text(subprocess.check_output(['git','show',f'ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea:esp32/src/{name}.rs'],cwd=root).decode())
 edit('esp32/src/periph.rs','    pub fn tick(&mut self, cycles: u64) {','    pub fn tick(&mut self, cycles: u64) {\n'+local(0)+'\n'+ '\n'.join(f'if {expr} {{ {local(i)} }}' for i,expr in [(1,'self.dport.ram.read(0xcc) & 0x406 == 0x406 && self.dport.ram.read(0xd0) & 4 == 0'),(2,'self.ledc.clock_enabled'),(3,'self.rmt.clock_enabled'),(4,'self.dport.ram.read(0xc0) & 16 != 0'),(5,'self.dport.ram.read(0xc0) & (1 << 21) != 0'),(6,'self.dport.ram.read(0xc4) & 16 != 0'),(7,'self.dport.ram.read(0xc4) & (1 << 21) != 0')]))
 p=root/'esp32/src/periph.rs'; p.write_text(p.read_text()+'\npub static ATTR: [std::sync::atomic::AtomicU64; 20] = [const { std::sync::atomic::AtomicU64::new(0) }; 20];\n')
 edit('esp32/src/ledc.rs','    fn tick(&mut self, ticks: u64) {','    fn tick(&mut self, ticks: u64) { '+local(8))
 edit('esp32/src/ledc.rs','        for (g, group) in self.groups.iter_mut().enumerate() {','        '+local(9)+'\n        for (g, group) in self.groups.iter_mut().enumerate() {')
 edit('esp32/src/rmt.rs','    fn tick(&mut self, ticks: u64) {','    fn tick(&mut self, ticks: u64) { '+local(10))
 edit('esp32/src/rmt.rs','            for n in 0..CHANNELS {','            '+local(11)+'\n            for n in 0..CHANNELS {')
 edit('esp32/src/i2s.rs','    pub(crate) fn i2s_step(&mut self, cycles: u64) {','    pub(crate) fn i2s_step(&mut self, cycles: u64) { '+local(12))
 edit('esp32/src/i2s.rs','            // Pin-routed sources live on controller 0, as on the other chips.','            '+local(13)+'\n            // Pin-routed sources live on controller 0, as on the other chips.')
 edit('esp32/src/wifi.rs','    pub(crate) fn wifi_step(&mut self) {','    pub(crate) fn wifi_step(&mut self) { '+local(14))
 edit('esp32/src/wifi.rs','        self.periph.wifi.now_cycles = self.cycles;','        '+local(15)+'\n        self.periph.wifi.now_cycles = self.cycles;')
 edit('esp32/src/ble.rs','pub fn intercept(cpu: &mut xtensa_lx7::Cpu, bus: &mut SocBus) -> bool {','pub fn intercept(cpu: &mut xtensa_lx7::Cpu, bus: &mut SocBus) -> bool { '+local(16))
 edit('esp32/src/adc.rs','    fn convert(&mut self, unit: usize, value: u32, old_data: u32) -> u32 {','    fn convert(&mut self, unit: usize, value: u32, old_data: u32) -> u32 { '+local(17))
 edit('esp32/src/adc.rs','    pub fn dac_outputs(&self) -> Vec<(u8, u32)> {','    pub fn dac_outputs(&self) -> Vec<(u8, u32)> { '+local(18))
 edit('esp32/src/adc.rs','    fn measure_touch(&mut self) {','    fn measure_touch(&mut self) { '+local(19))
 edit('cli/src/lib.rs','    let total = m.insns();','    eprintln!("ATTR {:?}", esp32::periph::ATTR.iter().map(|c| c.load(std::sync::atomic::Ordering::Relaxed)).collect::<Vec<_>>());\n    let total = m.insns();')
 env=dict(os.environ,CARGO_TARGET_DIR=str(out/'target-attribution'))
 with (out/(label+'-attribute-build.log')).open('w') as f: subprocess.run(['nice','-n','10','cargo','+1.99.0','build','--release','-p','esp32sim','--bin','esp32sim'],cwd=root,env=env,stdout=f,stderr=subprocess.STDOUT,check=True)
 cmd=[str(out/'target-attribution/release/esp32sim'),'--chip','esp32','--rom','web/wasm/fw/esp32_rev300_rom.elf','--board','esp32dev','--boot','rom','--no-dump','--console','uart0','--bootloader','web/wasm/fw/public/classic-hello-bootloader.bin','--ptable','web/wasm/fw/public/classic-hello-ptable.bin','--app','web/wasm/fw/public/classic-hello_world.bin','--max-seconds','30']
 r=subprocess.run(['nice','-n','10']+cmd,cwd=root,capture_output=True)
 (out/(label+'-attribute.console')).write_bytes(r.stdout); (out/(label+'-attribute.stderr')).write_bytes(r.stderr)
 a=json.loads(re.search(r'ATTR (\[[^\n]+\])',r.stderr.decode())[1])
 names=['bus_ticks','wifi_gate_on','ledc_gate_on','rmt_gate_on','i2s0_clock_on','i2s1_clock_on','i2s0_reset_on','i2s1_reset_on','ledc_calls','ledc_work','rmt_calls','rmt_work','i2s_calls','i2s_work','wifi_calls','wifi_work','vhci_intercept_calls','adc_convert_calls','dac_query_calls','touch_measure_calls']
 result={'label':label,'counters':dict(zip(names,a)),'console_sha256':hashlib.sha256(r.stdout).hexdigest(),'stop':re.sub(r' in .*?; emulated', '; emulated', next(l for l in r.stderr.decode().splitlines() if '[emu] stop:' in l)),'command':cmd[1:]}
 (out/(label+'-attribution.json')).write_text(json.dumps(result,indent=2)+'\n'); print(json.dumps(result,indent=2))
finally:
 for p,text in originals.items(): p.write_text(text)
