"""Package the reviewed Windows x64 release, including pinned offline assets."""
from pathlib import Path
import argparse,hashlib,json,os,shutil,subprocess,zipfile
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--skip-build',action='store_true');args=p.parse_args()
if os.name!='nt':raise SystemExit('This binary package is Windows x64 only.')
if not args.skip_build:subprocess.run(['cargo','build','--release','--locked','-p','am-cli'],cwd=ROOT,check=True)
target=Path(os.environ.get('CARGO_TARGET_DIR',str(ROOT/'target')))
exe=target/'release/antmemory.exe'
lock=json.loads((ROOT/'assets.lock.json').read_text(encoding='utf-8'))
for a in lock['assets']:
    file=ROOT/a['path']
    if not file.is_file() or hashlib.sha256(file.read_bytes()).hexdigest()!=a['sha256']:raise SystemExit('Missing/unverified asset: '+a['path']+'; run scripts/fetch_assets.py')
stage=ROOT/'dist/ANTmemory-4.0.0-windows-x64'
if stage.exists():raise SystemExit('Staging folder exists; preserve it or choose a fresh checkout.')
stage.mkdir(parents=True)
def copy(src,rel):
    dst=stage/rel;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(src,dst)
copy(exe,'runtime/bin/antmemory.exe')
for a in lock['assets']:copy(ROOT/a['path'],a['path'])
for name in ('README.md','README.ja.md','LICENSE','THIRD_PARTY_NOTICES.md','CHANGELOG.md','SECURITY.md','assets.lock.json','記憶を見る.bat','bin/ant-memory.mjs','server/native-server.mjs','server/package.json','viewer/start-viewer.bat'):
    copy(ROOT/name,name)
for folder in ('docs','third_party'):
    for file in (ROOT/folder).rglob('*'):
        if file.is_file():copy(file,file.relative_to(ROOT))
(stage/'stores').mkdir()
files={str(f.relative_to(stage)).replace('\\','/'):hashlib.sha256(f.read_bytes()).hexdigest() for f in stage.rglob('*') if f.is_file()}
manifest={'version':'4.0.0','platform':'windows-x64','source_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),'files':files}
(stage/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2),encoding='utf-8')
archive=stage.parent/(stage.name+'.zip')
with zipfile.ZipFile(archive,'w',zipfile.ZIP_DEFLATED,compresslevel=6) as z:
    for file in sorted(stage.rglob('*')):z.write(file,Path(stage.name)/file.relative_to(stage))
checksum=hashlib.sha256(archive.read_bytes()).hexdigest()
(ROOT/'dist/SHA256SUMS.txt').write_text(checksum+'  '+archive.name+'\n',encoding='utf-8')
print(json.dumps({'archive':str(archive),'bytes':archive.stat().st_size,'sha256':checksum,'stage':str(stage)},ensure_ascii=False))
