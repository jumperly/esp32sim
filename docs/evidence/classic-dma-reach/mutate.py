"""Run sequentially on an idle worktree; restore each source in finally."""
from pathlib import Path
import os, subprocess, json, sys
root=Path(sys.argv[1]).resolve(); out=Path(sys.argv[2]).resolve()
env=os.environ.copy(); env['CARGO_TARGET_DIR']=str(out/'target')
files={n:(root/'esp32/src'/n).read_text() for n in ['bus.rs','wifi.rs']}
spi='bus::tests::classic_spi_dma_reach_descriptors_and_payloads'
wifi='wifi::tests::classic_wifi_dma_reach_descriptors_and_payloads'
i2s='i2s::tests::classic_i2s_dma_reach_reads_and_writes'
helper='''
fn legacy_payload_range(addr: u32, len: usize) -> Option<std::ops::Range<usize>> {
 let off = addr.checked_sub(crate::bus::ROM_DRAM_LOW)? as usize;
 let end = off.checked_add(len)?;
 (end <= (crate::bus::DRAM_HIGH - crate::bus::ROM_DRAM_LOW) as usize).then_some(off..end)
}
'''
mutations=[
 ('reject-empty-TX-buffer','bus.rs','if count != 0 {\n                let range = dma_range(d.buf, count).ok_or(DmaDescriptorFault::BufferRead','if true {\n                let range = dma_range(d.buf, count).ok_or(DmaDescriptorFault::BufferRead',['bus::tests::classic_spi_dma_empty_nodes_do_not_access_a_buffer'],False),
 ('reject-empty-RX-buffer','bus.rs','if count != 0 {\n                let range = dma_range(d.buf, count).ok_or(fault)?;','if true {\n                let range = dma_range(d.buf, count).ok_or(fault)?;',['bus::tests::classic_spi_dma_empty_nodes_do_not_access_a_buffer'],False),
 ('allow-ROM-prefix','bus.rs','!(DRAM_LOW..DRAM_HIGH).contains(&addr)','!(ROM_DRAM_LOW..DRAM_HIGH).contains(&addr)',[spi,wifi,i2s],False),
 ('reject-first-valid-byte','bus.rs','!(DRAM_LOW..DRAM_HIGH).contains(&addr)','!(DRAM_LOW + 1..DRAM_HIGH).contains(&addr)',[spi,wifi,i2s],False),
 ('omit-TX-buffer-reach','bus.rs','dma_range(d.buf, count).ok_or(DmaDescriptorFault::BufferRead','legacy_payload_range(d.buf, count).ok_or(DmaDescriptorFault::BufferRead',[spi],True),
 ('omit-RX-buffer-reach','bus.rs','dma_range(d.buf, count).ok_or(fault)?;','legacy_payload_range(d.buf, count).ok_or(fault)?;',[spi],True),
 ('omit-word-preflight','bus.rs','if dma_range(addr, 4).is_none() { return false; }','',[spi],False),
 ('wifi-private-prefix-rule','wifi.rs','use crate::bus::{SocBus, dma_range};','use crate::bus::SocBus;\nfn dma_range(addr: u32, len: usize) -> Option<std::ops::Range<usize>> { legacy_payload_range(addr, len) }',[wifi],True),
 ('TX-payload-prefix','wifi.rs','dma_range(d.buf, d.length as usize)','legacy_payload_range(d.buf, d.length as usize)',['wifi::tests::classic_wifi_dma_reach_tx_payload_engine'],True),
 ('RX-payload-prefix','wifi.rs','dma_range(d.buf, total)','legacy_payload_range(d.buf, total)',['wifi::tests::classic_wifi_dma_reach_rx_payload_engine'],True),
 ('allow-SRAM1-IRAM-alias','bus.rs','pub(crate) fn dma_range(addr: u32, len: usize) -> Option<std::ops::Range<usize>> {','pub(crate) fn dma_range(addr: u32, len: usize) -> Option<std::ops::Range<usize>> {\n    if addr == 0x400a_0000 { return Some(0..len); }',['bus::tests::classic_dma_reach_rejects_sram1_iram_alias'],False),
]
results=[]
for name,file,old,new,tests,add_helper in mutations:
 original=files[file]; assert original.count(old)==1,(name,original.count(old))
 try:
  (root/'esp32/src'/file).write_text(original.replace(old,new)+(helper if add_helper else ''))
  for test in tests:
   cmd=['nice','-n','10','cargo','+1.99.0','test','--release','-j2','-p','esp32','--lib',test,'--','--exact']
   p=subprocess.run(cmd,cwd=root,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
   killed=p.returncode!=0 and 'test result: FAILED' in p.stdout and 'panicked at' in p.stdout and 'assertion' in p.stdout and 'could not compile' not in p.stdout and 'out of bounds' not in p.stdout
   (out/(name+'-'+test.split('::')[0]+'.log')).write_text(p.stdout)
   results.append(dict(mutation=name,test=test,exit_code=p.returncode,killed=killed))
   (out/'mutations.json').write_text(json.dumps(results,indent=2)+'\n')
   print(name,test,'KILLED' if killed else 'SURVIVED',flush=True)
 finally: (root/'esp32/src'/file).write_text(original)
assert all(x['killed'] for x in results)
cmd=['nice','-n','10','cargo','+1.99.0','test','--release','-j2','-p','esp32','--lib','dma_']
p=subprocess.run(cmd,cwd=root,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
(out/'mutation-control.log').write_text(p.stdout)
assert p.returncode==0
