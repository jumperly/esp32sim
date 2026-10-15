#!/usr/bin/env python3
import os, pathlib, types, sys, re, json, difflib, hashlib
root=pathlib.Path(sys.argv[1]); out=pathlib.Path(os.environ['EVIDENCE_OUT']).resolve()
m=types.ModuleType('asmcmp'); exec((root/'docs/evidence/classic-core/asmcmp.py').read_text().split('a, b = funcs(sys.argv[1])')[0],m.__dict__)
a=m.funcs(str(out/'base.dis')); b=m.funcs(str(out/'candidate.dis'))
pattern=re.compile(r'esp32::.*(tick|wifi_step|i2s_step|periph_write|write32_access|::clock\b|::write\b|::read\b|::write32\b|sync_ledc_outputs|sync_rmt_outputs)|Machine<esp32::soc::Esp32>.*(run|step_core|after_round|settle_modeled_time)')
rows=[]; diffs=[]
for n in sorted(set(a)|set(b)):
 if not pattern.search(n) or '{{closure}}' in n: continue
 same=n in a and n in b and m.same(a,b,n)
 rows.append({'name':n,'base_insns':len(a.get(n,[])),'candidate_insns':len(b.get(n,[])),'same':same})
 if not same: diffs.append(n+'\n'+'\n'.join(difflib.unified_diff(a.get(n,[]),b.get(n,[]),fromfile='base',tofile='candidate',lineterm='')))
(out/'classic-hot.json').write_text(json.dumps(rows,indent=2)+'\n'); (out/'classic-hot.diff').write_text('\n\n'.join(diffs)+'\n')
print(json.dumps(rows,indent=2)); print('functions',len(rows),'same',sum(r['same'] for r in rows))
