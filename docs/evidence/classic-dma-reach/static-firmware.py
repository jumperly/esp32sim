from pathlib import Path
import subprocess, hashlib, json, sys, types, difflib
root=Path(sys.argv[1]).resolve(); out=Path(sys.argv[2]).resolve()
m=types.ModuleType('asmcmp')
exec((root/'docs/evidence/classic-core/asmcmp.py').read_text().split('a, b = funcs(sys.argv[1])')[0],m.__dict__)
summary=[]
for binary in ['esp32sim','esp32sim-c3','esp32sim-c6']:
 paths={}
 for side,folder in [('base','baseline'),('head','target/release')]:
  dis=out/(side+'-'+binary+'.dis'); paths[side]=dis
  with dis.open('w') as log:
   subprocess.run(['/Library/Developer/CommandLineTools/usr/bin/llvm-objdump','-d','--demangle','--no-show-raw-insn',str(out/folder/binary)],stdout=log,check=True)
 a=m.funcs(str(paths['base'])); b=m.funcs(str(paths['head']))
 common=set(a)&set(b); changed=[n for n in sorted(common) if not m.same(a,b,n)]
 hot=[n for n in sorted(common) if any(x in n for x in ['run_inner','::run_round','::refresh_irq','::refresh_pin_lines','::read8','::read32','::page_versions','SocBus as emu_core::bus::Bus>::tick'])]
 def pageoff_only(n):
  return len(a[n])==len(b[n]) and all(i==j or ('#PAGEOFF' in i+j and i.replace(']', ', #PAGEOFF]')==j) or ('#PAGEOFF' in i+j and j.replace(']', ', #PAGEOFF]')==i) for i,j in zip(a[n],b[n]))
 relocation_only=[n for n in changed if pageoff_only(n)]
 substantive=[n for n in changed if n not in relocation_only]
 assert substantive == ['<esp32::bus::SocBus>::store'], substantive
 assert len(hot)>10
 assert all(m.same(a,b,n) for n in hot)
 summary.append(dict(binary=binary,common=len(common),identical=len(common)-len(changed),identical_after_zero_pageoff_normalization=len(common)-len(substantive),relocation_only_functions=relocation_only,hot_functions_identical=hot,classic_tick=dict(instructions=len(a['<esp32::bus::SocBus as emu_core::bus::Bus>::tick']),prologue=a['<esp32::bus::SocBus as emu_core::bus::Bus>::tick'][:9]),changed=[dict(name=n,base_instructions=len(a[n]),head_instructions=len(b[n])) for n in changed]))
 (out/(binary+'-asm.diff')).write_text('\n\n'.join(n+'\n'+'\n'.join(difflib.unified_diff(a[n],b[n],fromfile='base',tofile='head',lineterm='')) for n in changed)+'\n')
(out/'static-summary.json').write_text(json.dumps(summary,indent=2)+'\n')
print(json.dumps([{k:v for k,v in row.items() if k!='hot_functions_identical'} for row in summary],indent=2))
fw=root/'web/wasm/fw/public'
args=['--chip','esp32','--rom',str(root/'web/wasm/fw/esp32_rev300_rom.elf'),'--board','esp32dev','--boot','rom','--no-dump','--console','uart0','--bootloader',str(fw/'classic-hello-bootloader.bin'),'--ptable',str(fw/'classic-hello-ptable.bin'),'--app',str(fw/'classic-hello_world.bin'),'--max-seconds','3']
results=[]
for side,folder in [('base','baseline'),('head','target/release')]:
 p=subprocess.run(['nice','-n','10',str(out/folder/'esp32sim')]+args,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 (out/(side+'-classic.console')).write_bytes(p.stdout)
 (out/(side+'-classic.stderr')).write_bytes(p.stderr)
 stderr=p.stderr.decode()
 stop=next((l for l in stderr.splitlines() if l.startswith('[emu] stop:')), '')
 assert stop
 events=stop.rsplit('); ',1)[-1] if '); ' in stop else ''
 report='\n'.join([events]+[l for l in stderr.splitlines() if l.startswith('  core')])+'\n'
 results.append(dict(side=side,exit_code=p.returncode,console_sha256=hashlib.sha256(p.stdout).hexdigest(),report_sha256=hashlib.sha256(report.encode()).hexdigest(),report=report))
assert results[0]['exit_code']==results[1]['exit_code']==0
assert results[0]['console_sha256']==results[1]['console_sha256']
assert results[0]['report']==results[1]['report']
(out/'firmware-comparison.json').write_text(json.dumps(results,indent=2)+'\n')
print(json.dumps(results,indent=2))
