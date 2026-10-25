"""Collect Cargo dependency notices for redistribution without host-specific paths."""
from pathlib import Path
import hashlib,json,subprocess
ROOT=Path(__file__).resolve().parents[1]
metadata=json.loads(subprocess.check_output(['cargo','metadata','--locked','--format-version','1','--filter-platform','x86_64-pc-windows-msvc'],cwd=ROOT))
out=ROOT/'third_party/rust';(out/'licenses').mkdir(parents=True,exist_ok=True)
packages=[];missing=[]
for package in sorted(metadata['packages'],key=lambda p:(p['name'],p['version'])):
    if package['source'] is None:continue
    base=Path(package['manifest_path']).parent.resolve();candidates=[]
    for file in base.iterdir():
        if any(k in file.name.lower() for k in ('license','licence','notice','copying','copyright')):
            candidates.extend(file.rglob('*') if file.is_dir() else [file])
    if package.get('license_file'):candidates.append(base/package['license_file'])
    notices=[]
    for file in sorted(set(candidates)):
        if not file.is_file() or not file.resolve().is_relative_to(base) or file.stat().st_size>2_000_000:continue
        content=file.read_bytes()
        if b'\0' in content:continue
        digest=hashlib.sha256(content).hexdigest();name=digest[:24]+'.txt'
        (out/'licenses'/name).write_bytes(content)
        notices.append({'source':str(file.relative_to(base)).replace('\\','/'),'file':'licenses/'+name,'sha256':digest})
    if not notices:missing.append(package['name']+' '+package['version'])
    packages.append({'name':package['name'],'version':package['version'],'license':package['license'],'repository':package['repository'],'notices':notices})
(out/'packages.json').write_text(json.dumps(packages,ensure_ascii=False,indent=2),encoding='utf-8')
lines=['# Rust dependency notices','','Generated from the locked Windows x64 Cargo dependency graph. Build/test dependencies may also appear.','']
for p in packages:
    refs=', '.join(f'[{n["source"]}]({n["file"]})' for n in p['notices'])
    lines.append(f'- **{p["name"]} {p["version"]}** — {p["license"]}; {refs}')
(out/'README.md').write_text('\n'.join(lines)+'\n',encoding='utf-8')
print(json.dumps({'packages':len(packages),'notice_files':len(list((out/'licenses').iterdir())),'missing':missing}))
if missing:raise SystemExit('Missing dependency notices; inspect before publishing')
