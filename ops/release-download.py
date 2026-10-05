import base64, hashlib, json, pathlib, ssl, stat, subprocess, sys, urllib.parse, urllib.request, zipfile
base, key, work, version, cache = sys.argv[1:]
root = pathlib.Path(work)
prefix = urllib.parse.urlsplit(base).path + "/"
def allowed(url):
    u = urllib.parse.urlsplit(url)
    if u.scheme != "https" or u.port not in (None, 443) or u.username or u.password or u.fragment:
        return False
    return ((u.hostname == "github.com" and u.path.startswith(prefix) and not u.query)
        or (u.hostname == "release-assets.githubusercontent.com" and u.path.startswith("/github-production-release-asset/"))
        or (u.hostname == "objects.githubusercontent.com" and u.path.startswith("/github-production-release-asset-2e65be/")))
class Redirect(urllib.request.HTTPRedirectHandler):
    max_redirections = 4
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not allowed(newurl): raise RuntimeError("下载被引导至非 GitHub HTTPS 发布地址")
        return super().redirect_request(req, fp, code, msg, headers, newurl)
client = urllib.request.build_opener(urllib.request.ProxyHandler({}), Redirect(), urllib.request.HTTPSHandler(context=ssl.create_default_context()))
def download(name, limit):
    if cache:
        p=pathlib.Path(cache)/name
        info=p.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid!=0 or info.st_mode&0o022 or info.st_size>limit: raise RuntimeError("离线发布文件权限或大小不正确")
        return p.read_bytes()
    url = base + "/" + name
    if not allowed(url): raise RuntimeError("无效的 GitHub 发布地址")
    with client.open(urllib.request.Request(url, headers={"User-Agent":"Forward-Panel-Installer/"+version}), timeout=25) as response:
        if response.status != 200: raise RuntimeError("下载失败")
        raw = response.read(limit + 1)
    if len(raw) > limit: raise RuntimeError("发布文件超出限制")
    return raw
envelope = json.loads(download("panel-stable.json", 65536))
payload = base64.b64decode(envelope["payload"], validate=True)
signature = base64.b64decode(envelope["signature"], validate=True)
public = base64.b64decode(key, validate=True)
if len(public) != 32 or len(signature) != 64: raise RuntimeError("无效签名")
(root/"payload").write_bytes(payload)
(root/"signature").write_bytes(signature)
(root/"public.der").write_bytes(bytes.fromhex("302a300506032b6570032100")+public)
subprocess.run(["openssl","pkeyutl","-verify","-pubin","-keyform","DER","-inkey",str(root/"public.der"),"-rawin","-in",str(root/"payload"),"-sigfile",str(root/"signature")],check=True,stdout=subprocess.DEVNULL)
manifest=json.loads(payload)
entry=manifest["files"]["panel"]
name="forward-panel-"+version+".zip"
if manifest["version"] != version or entry["name"] != name or not 1024 <= entry["size"] <= 128*1024*1024:
    raise RuntimeError("发行版本或文件名称不匹配")
data=download(name, entry["size"])
if len(data) != entry["size"] or hashlib.sha256(data).hexdigest() != entry["sha256"]:
    raise RuntimeError("发行包哈希或大小不匹配")
archive=root/name
archive.write_bytes(data)
prefix="forward-panel-"+version+"/"
with zipfile.ZipFile(archive) as z:
    if len(z.infolist()) > 10000 or sum(i.file_size for i in z.infolist()) > 256*1024*1024:
        raise RuntimeError("发行包解压大小超出限制")
    seen=set()
    for i in z.infolist():
        name=i.filename
        path=pathlib.PurePosixPath(name)
        kind=stat.S_IFMT(i.external_attr >> 16)
        if not name.startswith(prefix) or "\\" in name or path.is_absolute() or ".." in path.parts or name in seen or kind not in (0,stat.S_IFREG,stat.S_IFDIR):
            raise RuntimeError("发行包包含无效路径或特殊文件")
        seen.add(name)
    z.extractall(root/"unpacked")
package=root/"unpacked"/prefix.rstrip("/")
for required in ["bin/controller-linux-amd64","bin/controller-linux-arm64","docs/反向代理.md","ops/templates/forward-panel.service"]:
    if not (package/required).is_file(): raise RuntimeError("发行包缺少必要文件")
print("发行包 Ed25519 签名、SHA-256、大小与解压路径校验通过。")