#!/usr/bin/env python3
"""Compare public hello and inherited synthetic fixtures, without measuring time.
Usage: compare.py BASE_TARGET CANDIDATE_TARGET FIXTURE_DIR OUTPUT_DIR
Build gpio/i2c ELF inputs with classic-gpio-irq/fixture.S as documented there.
"""
from pathlib import Path
import hashlib,json,re,subprocess,sys
base,candidate,fixtures,out=map(Path,sys.argv[1:]);out.mkdir(parents=True,exist_ok=True)
repo=Path(__file__).resolve().parents[3]
args=['--chip','esp32','--rom','web/wasm/fw/esp32_rev300_rom.elf','--board','esp32dev','--boot','rom','--no-dump','--console','uart0','--bootloader','web/wasm/fw/public/classic-hello-bootloader.bin','--ptable','web/wasm/fw/public/classic-hello-ptable.bin','--app','web/wasm/fw/public/classic-hello_world.bin','--max-seconds','3']
rows={}
for kind in ['hello','gpio','i2c']:
 pair={}
 for label,target in [('base',base),('candidate',candidate)]:
  cmd=[str(target/'release/esp32sim')]+args if kind=='hello' else [str(target/'release/examples/gpio_irq_fixture'),kind,str(fixtures/(kind+'.elf'))]
  r=subprocess.run(['nice','-n','10']+cmd,cwd=repo,capture_output=True,check=True)
  (out/(label+'-'+kind+'.stdout')).write_bytes(r.stdout); (out/(label+'-'+kind+'.stderr')).write_bytes(r.stderr)
  if kind=='hello':
   lines=r.stderr.decode().splitlines(); stop=next(l for l in lines if l.startswith('[emu] stop:'))
   match=re.search(r'core0 (\d+) \+ core1 (\d+)',stop); assert match,stop
   pair[label]=dict(core_insns=[int(x) for x in match.groups()],console_sha256=hashlib.sha256(r.stdout).hexdigest(),events=stop.split('); ')[1],irq_report=[l for l in lines if l.startswith('[irq]') or l.startswith('  core')])
  else: pair[label]=dict(output=r.stdout.decode().strip(),output_sha256=hashlib.sha256(r.stdout).hexdigest())
 assert pair['base']==pair['candidate'],(kind,pair)
 rows[kind]=pair
 print(kind,json.dumps(pair),flush=True)
(out/'fixtures.json').write_text(json.dumps(dict(base_revision='ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea',hello_args=args,results=rows),indent=2)+'\n')
inputs={str(p.relative_to(repo)):hashlib.sha256(p.read_bytes()).hexdigest() for p in [repo/'docs/evidence/classic-gpio-irq/fixture.S',repo/'esp32/examples/gpio_irq_fixture.rs',*[repo/x for x in args if x.endswith(('.bin','.elf'))]]}
inputs.update({p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in [fixtures/'gpio.elf',fixtures/'i2c.elf']})
(out/'inputs.json').write_text(json.dumps(inputs,indent=2)+'\n')
