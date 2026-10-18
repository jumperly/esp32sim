#!/usr/bin/env python3
"""Compare llvm-objdump -d --demangle --no-show-raw-insn output.

Usage: compare-disassembly.py BASE.dis CANDIDATE.dis
Normalize addresses, branch target offsets, alignment nops/udf and
ADRP-associated constant-pool offsets (including omitted zero offsets).
Retain struct/stack offsets, opcode/register choices and immediates.
Call aliases match only if their target bodies have the same shape.
This is a static comparison, not a CPU-time or control-flow equivalence proof.
"""
import collections
import json
import re
import sys


def funcs(path):
    out = collections.defaultdict(list)
    cur = None
    adrp_regs = set()
    for line in open(path):
        if line.endswith(':\n') and not line.startswith(' ') and '<' in line:
            cur = line[line.index('<') + 1:line.rindex('>')]
            adrp_regs = set()
            out[cur].append([])
            continue
        if cur is None or not line.strip() or ':' not in line:
            continue
        ins = re.sub(r'^\s*[0-9a-f]+:\s*', '', line).strip()
        if '<' not in ins:
            ins = re.sub(r'\s*;.*$', '', ins)
        op = ins.split('\t')[0]
        args = ins.split('\t')[1] if '\t' in ins else ''
        if op in ('nop', 'udf'):
            continue
        if op == 'adrp':
            adrp_regs.add(args.split(',')[0])
            ins = 'adrp ' + args.split(',')[0]
        else:
            src = re.findall(r'\[(x\d+)|, (x\d+), #', args)
            regs = {r for t in src for r in t if r}
            if regs & adrp_regs:
                ins = re.sub(r'#-?0x[0-9a-f]+|#\d+', '#PAGEOFF', ins)
                ins = re.sub(r'\[(x\d+)\]', r'[\1, #PAGEOFF]', ins)
            adrp_regs.discard(args.split(',')[0].strip())
        ins = re.sub(r'0x[0-9a-f]+ <(.*?)(\+0x[0-9a-f]+)?>$', r'<\1>', ins)
        ins = re.sub(r'<(GCC_except_table|ltmp|l_|dyld_stub_binder)\w*>', '<DATA>', ins)
        out[cur][-1].append(ins)
    return {n: [i for body in v for i in body] for n, v in out.items()}


TGT = re.compile(r'<(.*)>$')


def shape(fs, name):
    return tuple(TGT.sub('<FN>', i) for i in fs.get(name, ()))


def same(a, b, name):
    x, y = a[name], b[name]
    if len(x) != len(y):
        return False
    for i, j in zip(x, y):
        if i == j:
            continue
        mi, mj = TGT.search(i), TGT.search(j)
        if not (mi and mj) or TGT.sub('', i) != TGT.sub('', j):
            return False
        ti, tj = mi.group(1), mj.group(1)
        if ti == name or tj == name or not shape(a, ti) or shape(a, ti) != shape(b, tj):
            return False
    return True


if __name__ == '__main__':
    a, b = funcs(sys.argv[1]), funcs(sys.argv[2])
    common = sorted(set(a) & set(b))
    changed = [{'function': n, 'base_instructions': len(a[n]),
                'candidate_instructions': len(b[n])}
               for n in common if not same(a, b, n)]
    print(json.dumps({'common': len(common), 'identical': len(common)-len(changed),
                      'changed': changed, 'only_base': sorted(set(a)-set(b)),
                      'only_candidate': sorted(set(b)-set(a))}, indent=2))
