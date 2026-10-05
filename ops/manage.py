#!/usr/bin/env python3
"""Management of the isolated Forward Panel installation."""
import fcntl,getpass,ipaddress,json,os,pathlib,pwd,re,shutil,subprocess,sys,tarfile,tempfile,time,urllib.parse,urllib.request
ROOT=pathlib.Path('/opt/forward-panel')
STATE=pathlib.Path('/var/lib/forward-panel')
CONF=pathlib.Path('/etc/forward-panel')
SERVICE='forward-panel.service'
ARCH='arm64' if os.uname().machine=='aarch64' else 'amd64'
KEY='KIIxr0QlDRHjO6RTCGNmUJ3tYlbbun2wWTYmaMctbOI=' # Public Ed25519 verification key; gitleaks:allow
WORK=pathlib.Path('/var/lib/forward-panel-updater')
BACKUPS=pathlib.Path('/var/backups/forward-panel')
def run(*args,**kwargs):return subprocess.run(list(args),check=True,**kwargs)
def active():return subprocess.run(['systemctl','is-active','--quiet',SERVICE]).returncode==0
def config():return json.loads((STATE/'controller.json').read_text())
def version(root=ROOT):return subprocess.check_output([str(root/'bin'/('controller-linux-'+ARCH)),'--version'],text=True,timeout=8).split()[1]
def atomic(path,value,owner=None):
 raw=json.dumps(value,ensure_ascii=False,indent=2) if not isinstance(value,str) else value
 fd,name=tempfile.mkstemp(prefix='.forward-',dir=path.parent)
 try:
  with os.fdopen(fd,'w') as f:
   os.fchmod(f.fileno(),0o600)
   if owner:os.fchown(f.fileno(),*owner)
   f.write(raw);f.flush();os.fsync(f.fileno())
  os.replace(name,path)
  d=os.open(path.parent,os.O_RDONLY|os.O_DIRECTORY)
  try:os.fsync(d)
  finally:os.close(d)
 finally:
  if os.path.exists(name):os.unlink(name)
def owner():
 st=STATE.stat();return st.st_uid,st.st_gid
def save_config(c):atomic(STATE/'controller.json',c,owner())
def health(port):
 for _ in range(25):
  try:
   with urllib.request.urlopen('http://127.0.0.1:'+str(port)+'/health',timeout=2) as r:d=json.load(r)
   if d.get('ok') and d.get('runtime')=='rust':return
  except (OSError,ValueError):pass
  time.sleep(1)
 raise ValueError('主控健康检查失败')
def canonical(value):
 u=urllib.parse.urlsplit(value)
 if u.scheme not in ('http','https') or not u.hostname or u.username or u.password or u.path not in ('','/') or u.query or u.fragment or any(c.isspace() or c in '%\\"' for c in value):raise ValueError('面板地址格式不正确')
 if u.port is not None and not 1<=u.port<=65535:raise ValueError('端口不正确')
 if u.scheme=='http':
  ipaddress.ip_address(u.hostname)
  if u.port is None:raise ValueError('HTTP 地址应包含 IP 和端口')
 host=u.hostname.encode('idna').decode().lower()
 if ':' in host:host='['+host+']'
 elif not re.fullmatch(r'[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?',host):raise ValueError('主机名格式不正确')
 port='' if u.port is None or u.port==({'http':80,'https':443}[u.scheme]) else ':'+str(u.port)
 return u.scheme+'://'+host+port
def configure(origin,port):
 origin=canonical(origin);port=int(port)
 if not 1024<=port<=65535:raise ValueError('端口范围为 1024–65535')
 u=urllib.parse.urlsplit(origin)
 if u.scheme=='http' and u.port!=port:raise ValueError('HTTP 地址中的端口需与服务端口相同')
 c=config();old=json.loads(json.dumps(c));unit=pathlib.Path('/etc/systemd/system')/SERVICE
 prior=unit.read_text() if unit.exists() else None;running=active()
 c['origin']=origin;c['listen']=('0.0.0.0' if u.scheme=='http' else '127.0.0.1')+':'+str(port)
 if u.scheme=='http' and ':' in u.hostname:c['listen']='[::]:'+str(port)
 c['controller_urls']=['wss://'+origin.split('://',1)[1]+'/control/agent'] if u.scheme=='https' else []
 c['payment']['public_origin']=origin
 try:
  save_config(c);text=(ROOT/'ops/templates/forward-panel.service').read_text().replace('__ARCH__',ARCH)
  unit.write_text(text);unit.chmod(0o644)
  run('systemctl','daemon-reload');run('systemctl','enable',SERVICE,stdout=subprocess.DEVNULL)
  run('systemctl','restart',SERVICE);health(port)
  atomic(CONF/'instance.json',{'origin':origin,'port':port,'service':SERVICE})
 except Exception:
  save_config(old)
  if prior is not None:unit.write_text(prior)
  else:unit.unlink(missing_ok=True)
  run('systemctl','daemon-reload')
  if running:run('systemctl','restart',SERVICE)
  else:subprocess.run(['systemctl','stop',SERVICE],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
  raise
def mysql(args=(),sql=None,output=None,restore=None):
 c=config()['mysql'];name=c['name']
 if not re.fullmatch(r'[a-zA-Z0-9_]{1,64}',name):raise ValueError('数据库名格式不正确')
 with tempfile.TemporaryDirectory(prefix='forward-db-',dir='/run') as folder:
  f=pathlib.Path(folder)/'client.cnf'
  def esc(value):return '"'+str(value).replace('\\','\\\\').replace('"','\\"').replace('\n','\\n').replace('\r','\\r')+'"'
  f.write_text('[client]\nprotocol=tcp\n'+''.join(k+'='+esc(v)+'\n' for k,v in [('host',c['host']),('port',c['port']),('user',c['user']),('password',c['password'])]));f.chmod(0o600)
  program='mariadb-dump' if output is not None else 'mariadb'
  cmd=[program,'--defaults-file='+str(f)]
  if c.get('tls'):cmd.extend(['--ssl','--ssl-verify-server-cert'])
  if output is not None:
   with open(output,'xb') as out:run(*cmd,'--single-transaction','--skip-lock-tables','--skip-comments',name,stdout=out,stderr=subprocess.PIPE)
  elif restore is not None:
   with open(restore,'rb') as src:run(*cmd,name,stdin=src,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
  else:return run(*cmd,'-N',*args,name,input=sql.encode() if sql else None,stdout=subprocess.PIPE,stderr=subprocess.PIPE).stdout.decode()
def snapshot(destination):
 destination.mkdir(mode=0o700)
 mysql(output=destination/'database.sql')
 (destination/'state').mkdir(mode=0o700)
 for item in STATE.rglob('*'):
  rel=item.relative_to(STATE)
  if rel.parts[0] in ('runtime','release-cache'):continue
  if item.is_symlink():raise ValueError('状态目录含异常链接')
  dest=destination/'state'/rel
  if item.is_dir():dest.mkdir(mode=0o700,exist_ok=True)
  elif item.is_file():
   if item.stat().st_nlink!=1:raise ValueError('状态文件含异常硬链接')
   shutil.copy2(item,dest)
  else:raise ValueError('状态目录含特殊文件')
 shutil.copytree(CONF,destination/'config')
 unit=pathlib.Path('/etc/systemd/system')/SERVICE
 if unit.exists():shutil.copy2(unit,destination/SERVICE)
 atomic(destination/'meta.json',{'version':version(),'at':int(time.time())})
def restore_snapshot(snapshot):
 # Only internally created, root-private snapshots reach this function.
 mysql(restore=snapshot/'database.sql')
 uid,gid=owner()
 for item in (snapshot/'state').rglob('*'):
  dest=STATE/item.relative_to(snapshot/'state')
  if item.is_dir():dest.mkdir(mode=0o700,exist_ok=True)
  else:shutil.copy2(item,dest);dest.chmod(0o600)
  os.chown(dest,uid,gid)
 for item in (snapshot/'config').iterdir():shutil.copy2(item,CONF/item.name)
def backup():
 BACKUPS.mkdir(mode=0o700,parents=True,exist_ok=True);running=active();path=BACKUPS/('forward-panel-'+time.strftime('%Y%m%d-%H%M%S')+'.tar.gz')
 try:
  if running:run('systemctl','stop',SERVICE)
  with tempfile.TemporaryDirectory(prefix='.snapshot-',dir=BACKUPS) as folder:
   p=pathlib.Path(folder)/'data';snapshot(p)
   with tarfile.open(path,'x:gz') as archive:archive.add(p,arcname='forward-panel')
   path.chmod(0o600)
 finally:
  if running:run('systemctl','start',SERVICE)
 print('\n  备份已保存：'+str(path))
def summary():
 c=config();print('  版本  '+version()+'    状态  '+('运行中' if active() else '已停止'));print('  地址  '+c['origin'])
def update(rollback=False):
 WORK.mkdir(mode=0o700,parents=True,exist_ok=True);previous=WORK/'previous';prior_data=WORK/'previous-data'
 current=version()
 with tempfile.TemporaryDirectory(prefix='.forward-update-',dir=ROOT.parent) as folder:
  work=pathlib.Path(folder)
  if rollback:
   if not previous.is_dir() or not prior_data.is_dir():raise ValueError('没有可回退的版本')
   if input('  回退会恢复升级前的数据库，输入 ROLLBACK 确认：')!='ROLLBACK':return
   nextroot=work/'next';shutil.copytree(previous,nextroot,ignore=shutil.ignore_patterns('state'));restore=prior_data
  else:
   with urllib.request.urlopen(urllib.request.Request('https://api.github.com/repos/coexacx/forward-panel/releases/latest',headers={'User-Agent':'Forward-Panel'}),timeout=15) as r:release=json.loads(r.read(131072))
   target=release.get('tag_name','').removeprefix('v')
   if not re.fullmatch(r'\d{1,5}\.\d{1,5}\.\d{1,5}',target):raise ValueError('版本信息无效')
   if tuple(map(int,target.split('.')))<=tuple(map(int,current.split('.'))):print('\n  当前已是最新版本：'+current);return
   if input('  '+current+' → '+target+'，确认更新？[y/N]：').lower()!='y':return
   base='https://github.com/coexacx/forward-panel/releases/download/v'+target
   run('python3',str(ROOT/'ops/release-download.py'),base,KEY,str(work),target,'')
   nextroot=work/'unpacked'/('forward-panel-'+target);restore=None
  if shutil.disk_usage(ROOT.parent).free<512*1024*1024:raise ValueError('至少需要 512 MiB 可用空间')
  running=active();moved=False;swapped=False
  try:
   run('systemctl','stop',SERVICE);snapshot(work/'snapshot')
   if restore:restore_snapshot(restore)
   if (nextroot/'state').exists():shutil.rmtree(nextroot/'state')
   (nextroot/'state').symlink_to(STATE,target_is_directory=True)
   nextroot.chmod(0o755)
   for item in nextroot.rglob('*'):
    if item.is_symlink():continue
    if item.is_file():item.chmod(0o755 if item.parent.name=='bin' or item.name.endswith('.sh') else 0o644)
    elif item.is_dir():item.chmod(0o755)
   os.rename(ROOT,work/'old');moved=True;os.rename(nextroot,ROOT);swapped=True
   run('systemctl','start',SERVICE);health(config()['listen'].rsplit(':',1)[1])
   if previous.exists():shutil.rmtree(previous)
   if prior_data.exists():shutil.rmtree(prior_data)
   shutil.copytree(work/'old',previous,ignore=shutil.ignore_patterns('state'));shutil.copytree(work/'snapshot',prior_data)
   if not running:run('systemctl','stop',SERVICE)
  except Exception:
   if swapped:
    subprocess.run(['systemctl','stop',SERVICE],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);os.rename(ROOT,work/'failed');os.rename(work/'old',ROOT)
   elif moved:os.rename(work/'old',ROOT)
   if (work/'snapshot').exists():restore_snapshot(work/'snapshot')
   if running:run('systemctl','start',SERVICE)
   raise
 print('\n  当前版本 '+version())
def address():
 c=config();print('\n  当前地址  '+c['origin'])
 if int(mysql(sql="SELECT COUNT(*) FROM vp_nodes WHERE deleted=0;").strip() or '0'):
  raise ValueError('已有转发节点时请按迁移文档设置并验证新地址；本入口用于首次配置')
 value=input('  新面板地址（http://IP:端口 或 https://域名）：').strip()
 if not value:return
 port=input('  服务端口 ['+c['listen'].rsplit(':',1)[1]+']：').strip() or c['listen'].rsplit(':',1)[1]
 configure(value,port);print('\n  已保存：'+value)
def reset_password():
 username=input('  管理员用户名 [admin]：').strip() or 'admin'
 password=getpass.getpass('  新密码（12–128 字节）：')
 if password!=getpass.getpass('  再次输入：'):raise ValueError('两次密码不一致')
 running=active()
 try:
  if running:run('systemctl','stop',SERVICE)
  run('runuser','-u','forward-panel','--',str(ROOT/'bin'/('controller-linux-'+ARCH)),'-config',str(ROOT/'state/controller.json'),'--reset-admin-password',input=json.dumps({'username':username,'password':password}).encode())
 finally:
  if running:run('systemctl','start',SERVICE)
 print('\n  管理员密码已更新，旧会话已失效；二步验证设置保留。')
def uninstall():
 print('\n  卸载程序，保留私有状态、数据库及备份。')
 if input('  输入 UNINSTALL 确认：')!='UNINSTALL':return
 subprocess.run(['systemctl','disable','--now',SERVICE],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 (pathlib.Path('/etc/systemd/system')/SERVICE).unlink(missing_ok=True);pathlib.Path('/usr/local/bin/forward-panel').unlink(missing_ok=True)
 shutil.rmtree(ROOT);run('systemctl','daemon-reload');print('\n  程序已卸载，数据已保留。')
def main():
 if os.geteuid()!=0:raise ValueError('请使用 root 运行')
 os.umask(0o077);action=sys.argv[1] if len(sys.argv)>1 else 'summary'
 with open('/run/forward-panel-manage.lock','a') as lock:
  fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
  if action=='configure' and len(sys.argv)==4:configure(sys.argv[2],sys.argv[3])
  elif action=='summary':summary()
  elif action in ('start','stop','restart'):run('systemctl',action,SERVICE);summary()
  elif action=='update':update()
  elif action=='rollback':update(True)
  elif action=='backup':backup()
  elif action=='address':address()
  elif action=='reset-password':reset_password()
  elif action=='uninstall':uninstall()
  else:raise ValueError('未知管理命令')
if __name__=='__main__':
 try:main()
 except (Exception,KeyboardInterrupt) as e:print('\n  操作未完成：'+str(e),file=sys.stderr);sys.exit(1)
