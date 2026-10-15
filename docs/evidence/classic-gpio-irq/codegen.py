#!/usr/bin/env python3
"""Compare normalized release hot code with main; requires Xcode llvm-objdump.

Usage: python3 codegen.py BASE_BINARY CANDIDATE_BINARY OUTPUT_DIR
Reuses the maintained EX230 normalizer. No timing is collected.
"""
from pathlib import Path
import hashlib
import json
import re
import subprocess
import sys
import types

base, candidate, output = map(Path, sys.argv[1:])
output.mkdir(parents=True, exist_ok=True)
repo = Path(__file__).resolve().parents[3]
src = (repo / 'docs/evidence/classic-core/asmcmp.py').read_text().split('a, b = funcs(sys.argv[1])')[0]
m = types.ModuleType('asmcmp')
exec(src, m.__dict__)
parsed = []
artifacts = []
for label, binary in [('base', base), ('candidate', candidate)]:
    dis = output / (label + '.dis')
    with dis.open('w') as f:
        subprocess.run(['nice', '-n', '10', 'xcrun', 'llvm-objdump', '--disassemble', '--demangle', '--no-show-raw-insn', str(binary)], stdout=f, check=True)
    parsed.append(m.funcs(dis))
    artifacts.append(dict(label=label, binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), disassembly_sha256=hashlib.sha256(dis.read_bytes()).hexdigest()))
a, b = parsed
hot = re.compile(r'(Machine<.*>::(run|run_unmodeled|run_modeled|present_irqs|step_core|after_round)|::SocBus.*::(tick|read32|write32|fetch|next_deadline)|xtensa_lx7::.*::(run|step|exec)|esp32::periph::.*::(source_status|all_cpu_lines|raw_status|irq_status|refresh_level_status|note_driven|set_output_signal|sync_ledc_outputs|sync_rmt_outputs|read|write)|esp_periph::.*::(tick|next_deadline|irq_sources))')
rows = []
for name in sorted(set(a) | set(b)):
    if not hot.search(name) or '{{closure}}' in name:
        continue
    same = name in a and name in b and m.same(a, b, name)
    rows.append(dict(name=name, base_instructions=len(a.get(name, [])), candidate_instructions=len(b.get(name, [])), same=same, base_sha256=hashlib.sha256('\n'.join(a.get(name, [])).encode()).hexdigest(), candidate_sha256=hashlib.sha256('\n'.join(b.get(name, [])).encode()).hexdigest()))
    if not same:
        (output / ('difference-' + str(len(rows)) + '.json')).write_text(json.dumps(dict(name=name, base=a.get(name, []), candidate=b.get(name, [])), indent=2) + '\n')
assert rows, 'empty disassembly selection'
result = dict(method='EX230 address-normalized instructions, preserving field offsets and opcodes; no timing', base_revision='ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea', artifacts=artifacts, selected=len(rows), same=sum(r['same'] for r in rows), different=sum(not r['same'] for r in rows), rows=rows)
(output / 'codegen.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({k:v for k,v in result.items() if k != 'rows'}, indent=2))
for r in rows:
    if not r['same']:
        print(r['base_instructions'], r['candidate_instructions'], r['name'])
