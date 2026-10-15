#!/usr/bin/env python3
import os,pathlib,subprocess,json,re,hashlib,sys
root=pathlib.Path(sys.argv[1]).resolve(); out=pathlib.Path(os.environ['EVIDENCE_OUT']).resolve()
args=['--chip','esp32','--rom','web/wasm/fw/esp32_rev300_rom.elf','--board','esp32dev','--boot','rom','--no-dump','--console','uart0','--bootloader','web/wasm/fw/public/classic-hello-bootloader.bin','--ptable','web/wasm/fw/public/classic-hello-ptable.bin','--app','web/wasm/fw/public/classic-hello_world.bin','--max-seconds','30']
results={}
for label,target in [('base','target-base'),('candidate','target')]:
 r=subprocess.run(['nice','-n','10',str(out/target/'release/esp32sim')]+args,cwd=root,capture_output=True,check=True)
 (out/(label+'-native.console')).write_bytes(r.stdout); (out/(label+'-native.stderr')).write_bytes(r.stderr)
 stop=next(l for l in r.stderr.decode().splitlines() if l.startswith('[emu] stop:'))
 insns=[int(x) for x in re.search(r'core0 (\d+) \+ core1 (\d+)',stop).groups()]
 results[label]={'core_insns':insns,'console_sha256':hashlib.sha256(r.stdout).hexdigest(),'events':stop.split('); ')[1]}
assert results['base']==results['candidate'],results
(out/'native-comparison.json').write_text(json.dumps({'args':args,'results':results},indent=2)+'\n');print(json.dumps(results,indent=2))
