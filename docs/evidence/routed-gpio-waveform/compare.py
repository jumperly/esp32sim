#!/usr/bin/env python3
"""Compare normalized native hot functions using the upstream comparator."""
import argparse, importlib.util, json, pathlib, re, types
p=argparse.ArgumentParser()
p.add_argument('main')
p.add_argument('candidate')
p.add_argument('--out',required=True,type=pathlib.Path)
a=p.parse_args()
source=pathlib.Path(__file__).resolve().parents[3]/'docs/evidence/classic-core/asmcmp.py'
m=types.ModuleType('asmcmp')
exec(source.read_text().split('a, b = funcs(sys.argv[1])')[0],m.__dict__)
fa,fb=m.funcs(a.main),m.funcs(a.candidate)
hot=re.compile(r'Machine<esp32(?:s3|c3|c6)?::soc::.*::(?:run|step_core|after_round|present_irqs|settle_modeled_time|deliver_events|drain_console)\b|esp32(?:s3|c3|c6)?::bus::SocBus.*::(?:tick|tick_impl|tick_with_pins|refresh_tick_budget|next_deadline|refresh_irq|periph_read|periph_write|periph_write_inner|store|pending_work|devices|pin_deadline|fetch|read8|read16|read32|write8|write16|write32|read32_access|write32_access|read8_access|write8_access)\b|xtensa_lx7::.*::(?:step|run|exec)|riscv_rv32::.*::(?:step|run|exec)')
names=sorted(n for n in fa.keys()|fb.keys() if hot.search(n) and '{{closure}}' not in n)
rows=[]
for n in names:
 status='missing' if n not in fa or n not in fb else ('identical' if m.same(fa,fb,n) else 'different')
 rows.append(dict(function=n,status=status,main_instructions=len(fa.get(n,[])),candidate_instructions=len(fb.get(n,[]))))
result=dict(counts={k:sum(r['status']==k for r in rows) for k in ['identical','different','missing']},functions=rows)
a.out.write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result['counts']))
for r in rows:
 if r['status']!='identical': print(r)
