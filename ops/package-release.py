#!/usr/bin/env python3
"""Build a signed distribution from the clean public source tree."""
import argparse,base64,hashlib,json,pathlib,shutil,stat,time,zipfile
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization
a=argparse.ArgumentParser();a.add_argument('--signing-key',type=pathlib.Path,required=True);a.add_argument('--output',type=pathlib.Path,required=True);a.add_argument('--agent-amd64',type=pathlib.Path,required=True);a.add_argument('--agent-arm64',type=pathlib.Path,required=True);args=a.parse_args()
root=pathlib.Path(__file__).resolve().parent.parent;version='0.3.0';out=args.output.resolve();keyfile=args.signing_key.resolve()
if root==out or root in out.parents or root==keyfile or root in keyfile.parents:raise SystemExit('Output and signing key must be outside source')
if stat.S_IMODE(keyfile.stat().st_mode)&0o077:raise SystemExit('Signing key must be private')
key=Ed25519PrivateKey.from_private_bytes(keyfile.read_bytes()[:32]);public=base64.b64encode(key.public_key().public_bytes(serialization.Encoding.Raw,serialization.PublicFormat.Raw)).decode()
if public!='KIIxr0QlDRHjO6RTCGNmUJ3tYlbbun2wWTYmaMctbOI=':raise SystemExit('Signing key mismatch')
out.mkdir(parents=True,exist_ok=True)
def elf(p,arch):
 b=p.read_bytes()
 if b[:4]!=b'\x7fELF' or int.from_bytes(b[18:20],'little')!=({'amd64':62,'arm64':183}[arch]):raise SystemExit('Invalid ELF architecture')
 return b
for arch in ['amd64','arm64']:elf(root/'bin'/('controller-linux-'+arch),arch)
files={}
for section in ['source','bin','ops','docs','THIRD-PARTY-NOTICES']:
 for p in sorted((root/section).rglob('*')):
  rel=p.relative_to(root)
  if any(x in ('target','node_modules','__pycache__','.git') for x in rel.parts):continue
  if p.is_symlink():raise SystemExit('Unexpected symlink '+str(rel))
  if p.is_file():
   if p.suffix in ('.log','.pyc','.zip') or p.name in ('controller.json','app.key','signing.key','.env'):raise SystemExit('Unexpected runtime file '+str(rel))
   files[str(rel)]=p.read_bytes()
for f in ['README.md','LICENSE','SECURITY.md','install.sh','manage.sh','.gitignore','.gitattributes']:files[f]=(root/f).read_bytes()
files['state/.gitkeep']=b''
files['SHA256SUMS.json']=(json.dumps({f:hashlib.sha256(b).hexdigest() for f,b in files.items()},sort_keys=True,indent=2)+'\n').encode()
name='forward-panel-'+version+'.zip';archive=out/name
with zipfile.ZipFile(archive,'w',zipfile.ZIP_DEFLATED,compresslevel=9) as z:
 for f,b in sorted(files.items()):
  info=zipfile.ZipInfo('forward-panel-'+version+'/'+f,(2026,10,5,0,0,0));info.compress_type=zipfile.ZIP_DEFLATED
  mode=0o755 if f.startswith('bin/') or f.endswith('.sh') else 0o644
  if f=='state/.gitkeep':mode=0o600
  info.external_attr=(stat.S_IFREG|mode)<<16;z.writestr(info,b)
entry={'name':name,'size':archive.stat().st_size,'sha256':hashlib.sha256(archive.read_bytes()).hexdigest()}
payload=json.dumps({'version':version,'files':{'panel':entry}},sort_keys=True,separators=(',',':')).encode()
(out/'panel-stable.json').write_text(json.dumps({'payload':base64.b64encode(payload).decode(),'signature':base64.b64encode(key.sign(payload)).decode()},separators=(',',':'))+'\n')
artifacts=[]
for arch in ['amd64','arm64']:
 p=getattr(args,'agent_'+arch);data=elf(p,arch);name='vistart-agent-linux-'+arch;(out/name).write_bytes(data);(out/name).chmod(0o755)
 artifacts.append({'arch':arch,'os':'linux','name':name,'size':len(data),'sha256':hashlib.sha256(data).hexdigest()})
 # Separate controller assets are convenient for manual operation.
 shutil.copy2(root/'bin'/('controller-linux-'+arch),out/('vistart-controller-linux-'+arch))
manifest=json.dumps({'version':version,'sequence':int(time.strftime('%Y%m%d%H%M%S',time.gmtime())),'built_at':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),'artifacts':artifacts},separators=(',',':')).encode()
(out/'manifest.json').write_bytes(manifest);(out/'manifest.sig').write_bytes(base64.b64encode(key.sign(manifest))+b'\n')
(out/'release-public.txt').write_text(public+'\n')
(out/(entry['name']+'.sha256')).write_text(entry['sha256']+'  '+entry['name']+'\n')
shutil.copy2(root/'install.sh',out/'install.sh')
print(json.dumps({'package':entry,'files':len(files),'agents':artifacts},indent=2))
