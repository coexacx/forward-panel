#!/usr/bin/env bash
set +x
set -Eeuo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
umask 077
readonly FORWARD_VERSION=0.3.7
readonly FORWARD_RELEASE_BASE=https://github.com/coexacx/forward-panel/releases/download/v0.3.7
readonly FORWARD_PUBLIC_KEY=KIIxr0QlDRHjO6RTCGNmUJ3tYlbbun2wWTYmaMctbOI= # Public Ed25519 verification key; gitleaks:allow
forward_work='' forward_name='' forward_port=19280 forward_cache='' forward_check=0
die(){ printf '\n  %s\n\n' "$*" >&2; exit 1; }
cleanup(){ local rc=$?; [[ -z "$forward_work" ]] || rm -rf -- "$forward_work"; exit "$rc"; }
trap cleanup EXIT
while (( $# )); do
 case "$1" in
  --site-name|--port|--release-dir)
   (( $# >= 2 )) || die "$1 缺少参数"
   case "$1" in --site-name) forward_name=$2;; --port) forward_port=$2;; --release-dir) forward_cache=$2;; esac
   shift 2;;
  --check) forward_check=1;shift;;
  -h|--help)
   printf '%s\n' 'Vistart 转发面板 · Rust' '' '用法：bash install.sh [--site-name 站点名称] [--port 19280]' '      bash install.sh --check' '' '交互安装只需填写站点名称。自动生成管理员密码，显示 IP:端口。' '支持 Debian 12/13、Ubuntu 22.04/24.04/26.04、Rocky/AlmaLinux 9/10、' 'CentOS Stream 9/10、Fedora 42/43/44，amd64/arm64，systemd。' '不安装 Nginx，不申请证书。HTTPS 反向代理见 docs/反向代理.md。'
   exit 0;;
  *) die "未知参数：$1";;
 esac
done
(( EUID == 0 )) || die '请使用 root 运行。'
[[ -f /etc/os-release ]] || die '无法识别操作系统。'
# shellcheck disable=SC1091
. /etc/os-release
case "${ID:-}:${VERSION_ID:-}" in
 debian:12|debian:13|ubuntu:22.04|ubuntu:24.04|ubuntu:26.04) forward_platform=apt;;
 rocky:9|rocky:9.*|rocky:10|rocky:10.*|almalinux:9|almalinux:9.*|almalinux:10|almalinux:10.*|fedora:42|fedora:43|fedora:44) forward_platform=dnf;;
 centos:9|centos:10) [[ ${NAME:-} == *Stream* ]] || die '仅支持 CentOS Stream 9/10。';forward_platform=dnf;;
 *) die '此系统未在安装脚本的支持列表中，请查看 --help。';;
esac
[[ -d /run/systemd/system ]] || die '需要使用 systemd 的 Linux 系统。'
case "$(uname -m)" in x86_64) forward_arch=amd64;; aarch64|arm64) forward_arch=arm64;; *) die '仅支持 amd64、arm64。';; esac
if ! [[ "$forward_port" =~ ^[0-9]{4,5}$ ]] || (( 10#$forward_port < 1024 || 10#$forward_port > 65535 )); then die '端口范围为 1024–65535。'; fi
if (( forward_check )); then printf '支持安装：%s %s · %s\n' "$ID" "$VERSION_ID" "$forward_arch";exit 0;fi
command -v flock >/dev/null || die '请先安装 util-linux。'
exec 9>/run/forward-panel-install.lock
flock -n 9 || die '另一项安装正在进行。'
if [[ -f /opt/forward-panel/manage.sh && -f /etc/forward-panel/instance.json ]]; then
 exec bash /opt/forward-panel/manage.sh
fi
for forward_path in /opt/forward-panel /var/lib/forward-panel /etc/forward-panel /etc/systemd/system/forward-panel.service; do
 [[ ! -e "$forward_path" && ! -L "$forward_path" ]] || die "已有 $forward_path，安装已停止。现有站点请按升级文档操作。"
done
getent passwd forward-panel >/dev/null && die 'forward-panel 系统账户已存在，请先核对现有部署。'
printf '\n  Vistart 转发面板  /  安装\n  ──────────────────────────────\n  版本  %s    端口  %s\n\n' "$FORWARD_VERSION" "$forward_port"
if [[ -z "$forward_name" ]]; then
 exec 3<>/dev/tty || die '需要交互终端，或使用 --site-name。'
 read -r -u 3 -p '  站点名称：' forward_name
fi
[[ -n "$forward_name" && ${#forward_name} -le 60 && ! "$forward_name" =~ [[:cntrl:]] ]] || die '站点名称需要 1–60 个字符。'
forward_db_packages=(mariadb-server mariadb-client)
forward_fresh_db=1
forward_db_running=0
for forward_proc in /proc/[0-9]*/comm; do
 [[ -r "$forward_proc" ]] || continue
 read -r forward_process < "$forward_proc" || continue
 case "$forward_process" in mysqld|mariadbd) forward_db_running=1;; esac
done
if { command -v ss >/dev/null && [[ -n "$(ss -H -ltn '( sport = :3306 )')" ]]; } || (( forward_db_running )) || [[ -d /www/server/mysql || -d /var/lib/mysql/mysql ]]; then
 command -v mariadb >/dev/null || die '已有数据库占用 3306，自动安装已停止。请按手动部署文档配置独立 MariaDB。'
 forward_db_version=$(mariadb --no-defaults --protocol=socket --socket=/run/mysqld/mysqld.sock -Nse 'SELECT VERSION()' 2>/dev/null) || die '无法通过本机 root 套接字访问 MariaDB，请使用手动部署。'
 [[ "$forward_db_version" == *MariaDB* ]] || die '已有数据库不是 MariaDB，不会自动替换。请使用独立 MariaDB。'
 forward_db_packages=()
 forward_fresh_db=0
fi
printf '\n  正在准备运行环境…\n' 
if [[ "$forward_platform" == apt ]]; then
 export DEBIAN_FRONTEND=noninteractive
 apt-get update -qq
 apt-get install -y --no-install-recommends ca-certificates curl openssl python3 iproute2 util-linux "${forward_db_packages[@]}"
else
 forward_curl_package=();command -v curl >/dev/null || forward_curl_package=(curl-minimal)
 if (( ${#forward_db_packages[@]} )); then forward_db_packages=(mariadb-server mariadb); fi
 dnf install -y ca-certificates "${forward_curl_package[@]}" openssl python3 iproute util-linux "${forward_db_packages[@]}"
fi
[[ -z "$(ss -H -ltn "( sport = :$forward_port )")" ]] || die "端口 $forward_port 已被使用，请用 --port 指定其他端口。"
if (( forward_fresh_db )); then
 if [[ "$forward_platform" == apt ]]; then forward_db_conf=/etc/mysql/mariadb.conf.d/60-forward-panel.cnf;else forward_db_conf=/etc/my.cnf.d/forward-panel.cnf;fi
 printf '[mysqld]\nbind-address=127.0.0.1\n' > "$forward_db_conf"
 chmod 0644 "$forward_db_conf"
 systemctl enable mariadb
 systemctl restart mariadb
else
 systemctl start mariadb
fi
forward_existing_db=$(mariadb --no-defaults --protocol=socket --socket=/run/mysqld/mysqld.sock -Nse "SELECT (SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME='forward_panel')+(SELECT COUNT(*) FROM mysql.user WHERE User='forward_panel');") || die 'MariaDB 本机管理员连接失败。'
[[ "$forward_existing_db" == 0 ]] || die 'forward_panel 数据库或账户已存在，请使用手动部署或迁移。'
forward_work=$(mktemp -d /var/tmp/forward-panel-install.XXXXXXXX)
printf '\n  正在下载并验证发行包…\n'
timeout 240 python3 - "$FORWARD_RELEASE_BASE" "$FORWARD_PUBLIC_KEY" "$forward_work" "$FORWARD_VERSION" "$forward_cache" <<'PYDOWNLOAD'
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
PYDOWNLOAD
forward_ip=$(python3 - <<'PYIP'
import ipaddress,socket,urllib.request
try:
 with urllib.request.urlopen('https://api.ipify.org',timeout=6) as r: ip=r.read(64).decode().strip()
 assert ipaddress.ip_address(ip).is_global
 print(ip)
except Exception:
 s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.connect(('1.1.1.1',53));print(s.getsockname()[0]);s.close()
PYIP
)
forward_origin="http://$forward_ip:$forward_port"
install -d -m 0755 /opt/forward-panel
cp -a "$forward_work/unpacked/forward-panel-$FORWARD_VERSION/." /opt/forward-panel/
find /opt/forward-panel -type d -exec chmod 0755 {} +
find /opt/forward-panel -type f -exec chmod 0644 {} +
chmod 0755 /opt/forward-panel/bin/controller-linux-* /opt/forward-panel/manage.sh
rm -f /opt/forward-panel/state/.gitkeep
rmdir /opt/forward-panel/state
useradd --system --user-group --home-dir /var/lib/forward-panel --shell /usr/sbin/nologin forward-panel
install -d -m 0700 -o forward-panel -g forward-panel /var/lib/forward-panel /var/lib/forward-panel/runtime
ln -s /var/lib/forward-panel /opt/forward-panel/state
install -d -m 0700 /etc/forward-panel
python3 - "$forward_name" "$forward_origin" <<'PYINIT'
import json,pathlib,secrets,sys
name,origin=sys.argv[1:]
password=secrets.token_urlsafe(24)
path=pathlib.Path('/etc/forward-panel/initial-admin.json')
path.write_text(json.dumps({'name':name,'username':'admin','password':password,'url':origin},ensure_ascii=False))
path.chmod(0o600)
PYINIT
python3 - "$forward_origin" "$forward_port" "$forward_ip" <<'PYDATABASE'
import json,pathlib,secrets,subprocess,pwd,sys
origin,port,ip=sys.argv[1:]
database='forward_panel'
client=['mariadb','--no-defaults','--protocol=socket','--socket=/run/mysqld/mysqld.sock','-N']
check=subprocess.run(client,input=b"SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME='forward_panel';",check=True,stdout=subprocess.PIPE)
if int(check.stdout.strip()):raise SystemExit('forward_panel 数据库已经存在，禁止覆盖。')
password=secrets.token_hex(32)
sql="CREATE DATABASE forward_panel CHARACTER SET utf8mb4 COLLATE utf8mb4_bin; CREATE USER 'forward_panel'@'127.0.0.1' IDENTIFIED BY '"+password+"'; GRANT ALL PRIVILEGES ON forward_panel.* TO 'forward_panel'@'127.0.0.1';"
subprocess.run(client,input=sql.encode(),check=True,stdout=subprocess.DEVNULL)
config={'driver':'mysql','listen':'0.0.0.0:'+port,'origin':origin,'socket':'/opt/forward-panel/state/runtime/admin.sock','controller_urls':[],'panel_ips':[ip],'mysql':{'host':'127.0.0.1','port':3306,'name':database,'user':database,'password':password},'payment':{'enabled':False,'version':'v1','gateway':'','merchant_id':'','secret':'','public_origin':origin,'methods':[{'id':'alipay','name':'支付宝','type':'alipay','enabled':True},{'id':'wxpay','name':'微信','type':'wxpay','enabled':True}]}}
path=pathlib.Path('/var/lib/forward-panel/controller.json');path.write_text(json.dumps(config,ensure_ascii=False,indent=2));path.chmod(0o600)
owner=pwd.getpwnam('forward-panel');__import__('os').chown(path,owner.pw_uid,owner.pw_gid)
PYDATABASE
python3 -c 'import json;d=json.load(open("/etc/forward-panel/initial-admin.json"));d.pop("url");print(json.dumps(d))' | runuser -u forward-panel -- /opt/forward-panel/bin/controller-linux-"$forward_arch" -config /opt/forward-panel/state/controller.json --install

python3 /opt/forward-panel/ops/manage.py configure "$forward_origin" "$forward_port"
cat > /usr/local/bin/forward-panel <<'PYWRAPPER'
#!/bin/sh
exec /bin/bash /opt/forward-panel/manage.sh "$@"
PYWRAPPER
chmod 0755 /usr/local/bin/forward-panel
printf '\n  安装完成\n  ──────────────────────────────\n'
python3 - <<'PYSHOW'
import json
d=json.load(open('/etc/forward-panel/initial-admin.json'))
print('  访问地址  '+d['url']+'\n  管理员    '+d['username']+'\n  初始密码  '+d['password'])
print('\n  管理菜单  forward-panel\n  反向代理  https://github.com/coexacx/forward-panel/blob/main/docs/反向代理.md')
print('\n  初始凭据保存在 /etc/forward-panel/initial-admin.json（仅 root 可读）。')
PYSHOW
