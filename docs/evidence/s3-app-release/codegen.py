#!/usr/bin/env python3
"""Usage: codegen.py BASE_CLI CANDIDATE_CLI OUTPUT_DIRECTORY. No timing."""
from pathlib import Path
import difflib
import hashlib
import json
import re
import subprocess
import sys
import types

base, candidate, out = map(Path, sys.argv[1:])
out.mkdir(parents=True, exist_ok=True)
repo = Path(__file__).resolve().parents[3]
m = types.ModuleType('asmcmp')
exec((repo / 'docs/evidence/classic-core/asmcmp.py').read_text().split('a, b = funcs(sys.argv[1])')[0], m.__dict__)
parsed, artifacts = [], []
for label, binary in [('base', base), ('candidate', candidate)]:
    dis = out / (label + '.dis')
    with dis.open('w') as f:
        subprocess.run(['nice', '-n', '10', 'xcrun', 'llvm-objdump', '--disassemble', '--demangle', '--no-show-raw-insn', str(binary)], stdout=f, check=True)
    parsed.append(m.funcs(dis))
    artifacts.append(dict(label=label, binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), disassembly_sha256=hashlib.sha256(dis.read_bytes()).hexdigest()))
a, b = parsed
selector = re.compile(r'(Machine<esp32(?:::|s3::|c3::|c6::).*>::(?:run|step_core|after_round|present_irqs)|esp32(?:::|s3::|c3::|c6::)bus::SocBus.*::(?:tick|read32|write32|fetch|next_deadline)|(?:xtensa_lx7|riscv_rv32)::.*::(?:run|step|exec).*esp32(?:::|s3::|c3::|c6::)bus::SocBus)')
rows = []
for name in sorted(set(a) | set(b)):
    if not selector.search(name) or '{{closure}}' in name:
        continue
    same = name in a and name in b and m.same(a, b, name)
    rows.append(dict(name=name, base_instructions=len(a.get(name, [])), candidate_instructions=len(b.get(name, [])), same=same))
    if not same:
        diff = list(difflib.unified_diff(a.get(name, []), b.get(name, []), n=4, lineterm=''))
        (out / ('diff-' + str(len(rows)) + '.txt')).write_text(name + '\n' + '\n'.join(diff) + '\n')
assert rows
summary = dict(base_revision='f5a038bf917ccb9787714dc1d20c7ae33482337c', method='EX230 asmcmp address normalization, preserves field offsets/opcodes/register allocation; no timing', artifacts=artifacts, selector=selector.pattern, selected=len(rows), same=sum(r['same'] for r in rows), rows=rows)
(out / 'codegen.json').write_text(json.dumps(summary, indent=2) + '\n')
print('selected', len(rows), 'same', summary['same'])
for row in rows:
    if not row['same']:
        print(row)
