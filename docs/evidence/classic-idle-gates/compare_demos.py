#!/usr/bin/env python3
"""Run the standard WASM set on main and candidate, exact counters/hashes, no timing comparison."""
import pathlib,subprocess,os,json,sys,re
root=pathlib.Path(sys.argv[1]).resolve(); out=pathlib.Path(os.environ['EVIDENCE_OUT']).resolve()
files=['esp32/src/'+f+'.rs' for f in ['periph','bus','wifi','i2s','ledc','rmt']]
original={p:(root/p).read_bytes() for p in files+['tools/wasm-test.mjs','wasm/src/network.rs','web/wasm/esp32sim.wasm']}
names='hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel'.split()
try:
 js=original['tools/wasm-test.mjs'].decode()
 js=js.replace('  w.esp32sim_net_delete(net);',"  console.log('EVIDENCE '+JSON.stringify({name, nodes: text.map((t,i)=>({insns:stat(i,7),console_sha256:hash(t),tx:stat(i,0),rx:stat(i,1)}))}));\n  w.esp32sim_net_delete(net);")
 js=js.replace('  w.esp32sim_delete(emu);\n  const wall',"  console.log('EVIDENCE '+JSON.stringify({name,insns,console_sha256:hash(text),frames}));\n  w.esp32sim_delete(emu);\n  const wall")
 js=js.replace('\n', "\nimport {createHash} from 'node:crypto';\nconst hash=t=>createHash('sha256').update(t).digest('hex');\n", 1)
 (root/'tools/wasm-test.mjs').write_text(js)
 network=original['wasm/src/network.rs'].decode().replace('        6 => nd.halted as u32 as f64,','        6 => nd.halted as u32 as f64,\n        7 => nd.m.insns() as f64,')
 (root/'wasm/src/network.rs').write_text(network)
 results={}
 for label in ['base','candidate']:
  for p in files:
   (root/p).write_bytes(subprocess.check_output(['git','show','ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea:'+p],cwd=root) if label=='base' else original[p])
  env=dict(os.environ,CARGO_TARGET_DIR=str(out/('target-'+label+'-wasm')),RUSTUP_TOOLCHAIN='1.99.0')
  command='. ./tools/wasm-rustflags.sh; export RUSTFLAGS; cargo +1.99.0 build --release --target wasm32-unknown-unknown -p esp32sim-wasm'
  with (out/(label+'-wasm-build.log')).open('w') as f: subprocess.run(['nice','-n','10','sh','-c',command],cwd=root,env=env,stdout=f,stderr=subprocess.STDOUT,check=True)
  artifact=pathlib.Path(env['CARGO_TARGET_DIR'])/'wasm32-unknown-unknown/release/esp32sim_wasm.wasm'
  (root/'web/wasm/esp32sim.wasm').write_bytes(artifact.read_bytes())
  r=subprocess.run(['nice','-n','10','node','tools/wasm-test.mjs']+names,cwd=root,capture_output=True,check=True)
  (out/(label+'-wasm-compare.log')).write_bytes(r.stdout+r.stderr)
  rows=[json.loads(l[9:]) for l in r.stdout.decode().splitlines() if l.startswith('EVIDENCE ')]
  assert len(rows)==len(names),(label,len(rows)); results[label]=rows
  (out/'demo-comparison.json').write_text(json.dumps(results,indent=2)+'\n')
 assert results['base']==results['candidate'], results
 print(json.dumps(results,indent=2)); print('PASS exact counters and console hashes for all 8 standard WASM demos')
finally:
 for p,data in original.items(): (root/p).write_bytes(data)
