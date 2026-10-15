#!/usr/bin/env python3
"""Static other-chip hot-path qualification using EX230 asmcmp normalizer.
Usage: codegen.py BASE_BINARY CANDIDATE_BINARY OUTPUT_DIR
No timing is collected. Retains field offsets/opcodes/register allocation.
"""
from pathlib import Path
import hashlib, json, re, subprocess, sys, types
base,candidate,out=map(Path,sys.argv[1:]);out.mkdir(parents=True,exist_ok=True)
repo=Path(__file__).resolve().parents[3]
m=types.ModuleType('asmcmp')
exec((repo/'docs/evidence/classic-core/asmcmp.py').read_text().split('a, b = funcs(sys.argv[1])')[0],m.__dict__)
parsed=[]; artifacts=[]
for label,binary in [('base',base),('candidate',candidate)]:
 dis=out/(label+'.dis')
 with dis.open('w') as f: subprocess.run(['nice','-n','10','xcrun','llvm-objdump','--disassemble','--demangle','--no-show-raw-insn',str(binary)],stdout=f,check=True)
 parsed.append(m.funcs(dis))
 artifacts.append(dict(label=label,binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),disassembly_sha256=hashlib.sha256(dis.read_bytes()).hexdigest()))
a,b=parsed
other=re.compile(r'(Machine<esp32(?:s3|c3|c6)::.*>::(?:run|step_core|after_round|present_irqs)|esp32(?:s3|c3|c6)::bus::SocBus.*::(?:tick|read32|write32|fetch|next_deadline)|(?:xtensa_lx7|riscv_rv32)::.*::(?:run|step|exec).*esp32(?:s3|c3|c6)::bus::SocBus)')
classic=re.compile(r'(Machine<esp32::.*>::(?:run|step_core|after_round|present_irqs)|esp32::bus::SocBus.*::(?:tick|read32|write32|fetch|next_deadline)|esp32::periph::.*::(?:source_status|all_cpu_lines|raw_status|irq_status|refresh_level_status|tick|read|write))')
for label,selector in [('other',other),('classic',classic)]:
 rows=[]
 for name in sorted(set(a)|set(b)):
  if not selector.search(name) or '{{closure}}' in name:continue
  same=name in a and name in b and m.same(a,b,name)
  rows.append(dict(name=name,base_instructions=len(a.get(name,[])),candidate_instructions=len(b.get(name,[])),same=same))
  if not same:(out/(label+'-diff-'+str(len(rows))+'.json')).write_text(json.dumps(dict(name=name,base=a.get(name,[]),candidate=b.get(name,[])),indent=2)+'\n')
 assert rows,(label,'empty selector')
 summary=dict(method='EX230 address normalization, no timing',base_revision='ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea',artifacts=artifacts,selector=selector.pattern,selected=len(rows),same=sum(r['same'] for r in rows),rows=rows)
 (out/('codegen-'+label+'.json')).write_text(json.dumps(summary,indent=2)+'\n')
 print(label,'selected',len(rows),'same',sum(r['same'] for r in rows))
 for r in rows:
  if not r['same']: print(r['base_instructions'],r['candidate_instructions'],r['name'])
 if label=='other': assert all(r['same'] for r in rows),'other-chip hot-path mismatch'
