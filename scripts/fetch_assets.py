"""Download pinned, checksum-verified Windows runtime/model assets (stdlib only)."""
from pathlib import Path
import hashlib,json,os,tempfile,urllib.request,zipfile
ROOT=Path(__file__).resolve().parents[1]
lock=json.loads((ROOT/'assets.lock.json').read_text(encoding='utf-8'))
def digest(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def fetch(url,destination,expected):
    destination.parent.mkdir(parents=True,exist_ok=True)
    if destination.is_file() and digest(destination)==expected:return
    fd,name=tempfile.mkstemp(prefix='.download-',dir=destination.parent);os.close(fd)
    temp=Path(name)
    try:
        print('Downloading '+destination.name,flush=True)
        urllib.request.urlretrieve(url,temp)
        if digest(temp)!=expected:raise ValueError('SHA-256 mismatch: '+url)
        os.replace(temp,destination)
    finally:
        temp.unlink(missing_ok=True)
for asset in lock['assets']:
    if 'url' in asset:fetch(asset['url'],ROOT/asset['path'],asset['sha256'])
archive=ROOT/'runtime/upstream/onnxruntime-win-x64-1.31.0.zip'
spec=lock['onnxruntime_archive'];fetch(spec['url'],archive,spec['sha256'])
with zipfile.ZipFile(archive) as z:
    for asset in lock['assets']:
        if 'archive_path' not in asset:continue
        content=z.read(asset['archive_path'])
        if hashlib.sha256(content).hexdigest()!=asset['sha256']:raise ValueError('Runtime checksum mismatch')
        destination=ROOT/asset['path'];destination.parent.mkdir(parents=True,exist_ok=True);destination.write_bytes(content)
print('Pinned model and ONNX Runtime files verified.')
