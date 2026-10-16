from pathlib import Path
import subprocess, os, json, sys, re
root = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()
out.mkdir(parents=True, exist_ok=True)
env = os.environ.copy()
env.update(CARGO_TARGET_DIR=str(out/'target'), RUSTUP_TOOLCHAIN='1.99.0', RUSTUP_HOME=str(Path.home()/'.rustup'), CARGO_HOME=str(Path.home()/'.cargo'), CARGO_BUILD_JOBS='2')
empty = out/'empty-home'
empty.mkdir(exist_ok=True)
steps = [
 ('clippy-native','nice -n 10 cargo +1.99.0 clippy --workspace --all-targets -- -D warnings',{}),
 ('clippy-wasm','nice -n 10 cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings',{}),
 ('test-rom-empty-home','nice -n 10 cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_',{'HOME':str(empty),'ESP32SIM_ROM_DIR':str(root/'web/wasm/fw')}),
 ('test-plain-empty-home','nice -n 10 cargo +1.99.0 test --release --workspace',{'HOME':str(empty),'ESP32SIM_ROM_DIR':None}),
 ('wasm-build-test','nice -n 10 tools/wasm-build.sh && nice -n 10 node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel',{}),
 ('privacy','node tools/check-evidence-privacy.mjs',{}),
 ('head-build','nice -n 10 cargo +1.99.0 build --release --bins',{}),
]
results=[]
for name,command,extra in steps:
 e=env.copy()
 for k,v in extra.items():
  if v is None: e.pop(k,None)
  else: e[k]=v
 with (out/(name+'.log')).open('w') as log:
  code=subprocess.run(command,shell=True,cwd=root,env=e,stdout=log,stderr=subprocess.STDOUT).returncode
 row=dict(name=name,command=command,exit_code=code)
 if name.startswith('test-'):
  counts=re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored', (out/(name+'.log')).read_text())
  row['totals']=dict(zip(['passed','failed','ignored'],[sum(int(c[i]) for c in counts) for i in range(3)]))
 results.append(row)
 (out/'checks.json').write_text(json.dumps(results,indent=2)+'\n')
 print(name,code,row.get('totals',''),flush=True)
 if code: raise SystemExit(code)
