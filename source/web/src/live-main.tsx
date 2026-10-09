import React, { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Activity,
  ArrowDown,
  ArrowLeftRight,
  ArrowRight,
  ArrowUp,
  Check,
  ChevronRight,
  ClipboardList,
  Copy,
  Globe2,
  History,
  LayoutDashboard,
  LogOut,
  Mail,
  Menu,
  Moon,
  Network,
  Package,
  Plus,
  RefreshCw,
  Search,
  Server,
  Settings2,
  ShieldCheck,
  ShoppingBag,
  Sun,
  Users,
  WalletCards,
  X,
} from "lucide-react";
import "./live.css";
import QRCode from "qrcode";
type Any = Record<string, any>;
type Boot = {
  ready: boolean;
  csrf: string;
  user: Any | null;
  site_name: string;
  registration: boolean;
  email_verification?: boolean;
  mail_enabled?: boolean;
  checks?: Record<string, boolean>;
};
type Snapshot = {
  users: Any[];
  nodes: Any[];
  pools: Any[];
  plans: Any[];
  leases: Any[];
  allocations: Any[];
  orders: Any[];
  jobs: Any[];
  agent_release: Any;
  audit: Any[];
  payment: Any;
  mail: Any;
  settings: Any;
  user: Any;
  now: number;
  receivedAt: number;
  revision: number;
};
const money = (n: number) => "¥" + (n / 100).toFixed(2);
const date = (n: number) =>
  n
    ? new Intl.DateTimeFormat("zh-CN", {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
      }).format(n * 1000)
    : "—";
const bytes = (n: number = 0) =>
  n >= 1e12
    ? (n / 1e12).toFixed(2) + " TB"
    : n >= 1e9
      ? (n / 1e9).toFixed(2) + " GB"
      : n >= 1e6
        ? (n / 1e6).toFixed(1) + " MB"
        : n >= 1e3
          ? (n / 1e3).toFixed(1) + " KB"
          : Math.round(n) + " B";
const endpoint = (ip: string, p: number) =>
  (ip?.includes(":") ? "[" + ip + "]" : ip) + ":" + p;
const formdata = (e: React.FormEvent<HTMLFormElement>) => {
  e.preventDefault();
  return Object.fromEntries(new FormData(e.currentTarget)) as Record<
    string,
    string
  >;
};
const leaseState = (l: Any) =>
  l.ended
    ? "已结束"
    : l.expires_at <= Date.now() / 1000
      ? "已到期"
      : l.manual_paused
        ? "已暂停"
        : l.traffic_limit_bytes > 0 &&
            l.used_up + l.used_down >= l.traffic_limit_bytes
          ? "流量用尽"
          : "使用中";
const forwardingLabel = (a: Any) => ({
  listening: "节点已监听",
  accepted: "配置已接收",
  pending: "正在下发",
  offline: "节点离线",
  paused: "已暂停",
  apply_failed: "监听失败",
  not_listening: "未监听",
  unconfigured: "待配置目标",
  agent_upgrade_required: "需升级 Agent",
} as Any)[a.apply_status] ?? "等待确认";
const forwardingWarn = (a: Any) => ["apply_failed", "not_listening", "agent_upgrade_required"].includes(a.apply_status);
const orderLabel: Any = {
  pending: "待付款",
  paid: "已支付",
  paid_review: "付款待处理",
  cancelled: "已取消",
};
const kindLabel: Any = {
  new: "开通套餐",
  renew: "续费套餐",
  reset: "重置流量",
};
function Logo({ name = "Vistart Ports" }: { name?: string }) {
  return (
    <div className="brand">
      <span className="brand-icon">
        <Network size={22} />
      </span>
      <span>{name}</span>
    </div>
  );
}
function Button({
  children,
  primary = false,
  danger = false,
  ...p
}: React.ButtonHTMLAttributes<HTMLButtonElement> & {
  primary?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      {...p}
      className={
        "button " +
        (primary ? "primary " : "") +
        (danger ? "danger " : "") +
        (p.className ?? "")
      }
    >
      {children}
    </button>
  );
}
function Field({
  label,
  name,
  children,
  hint,
  ...p
}: React.InputHTMLAttributes<HTMLInputElement> & {
  label: string;
  name: string;
  children?: React.ReactNode;
  hint?: string;
}) {
  const id = React.useId();
  return (
    <label className="field" htmlFor={id}>
      <span id={id + "-label"}>{label}</span>
      {children ?? (
        <input
          name={name}
          {...p}
          id={id}
          aria-labelledby={id + "-label"}
          aria-describedby={hint ? id + "-hint" : undefined}
        />
      )}{" "}
      {hint && <small id={id + "-hint"}>{hint}</small>}
    </label>
  );
}
function Select({
  label,
  name,
  children,
  ...p
}: React.SelectHTMLAttributes<HTMLSelectElement> & {
  label: string;
  name: string;
}) {
  const id = React.useId();
  return (
    <label className="field" htmlFor={id}>
      <span id={id + "-label"}>{label}</span>
      <select name={name} {...p} id={id} aria-labelledby={id + "-label"}>
        {children}
      </select>
    </label>
  );
}
function Tag({
  children,
  good = false,
  warn = false,
}: {
  children: React.ReactNode;
  good?: boolean;
  warn?: boolean;
}) {
  return (
    <span className={"tag " + (good ? "good" : warn ? "warn" : "")}>
      {children}
    </span>
  );
}
function Empty({
  title,
  detail,
  action,
}: {
  title: string;
  detail?: string;
  action?: React.ReactNode;
}) {
  return (
    <div className="empty">
      <Network size={30} />
      <h3>{title}</h3>
      {detail && <p>{detail}</p>}
      {action}
    </div>
  );
}
function Box({
  title,
  action,
  children,
  className = "",
}: {
  title?: string;
  action?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section className={"glass box " + className}>
      {title && (
        <div className="box-head">
          <h2>{title}</h2>
          {action}
        </div>
      )}
      {children}
    </section>
  );
}
function Modal({
  title,
  onClose,
  children,
  busy = false,
}: {
  title: string;
  onClose: () => void;
  children: React.ReactNode;
  busy?: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const before = document.activeElement as HTMLElement;
    const timer = setTimeout(
      () =>
        ref.current?.querySelector<HTMLElement>("[data-autofocus],input,select,button")?.focus(),
      20,
    );
    function key(e: KeyboardEvent) {
      if (e.key === "Escape" && !busy) onClose();
      if (e.key === "Tab") {
        const list = Array.from(
          ref.current?.querySelectorAll<HTMLElement>(
            'button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),[tabindex="0"]',
          ) ?? [],
        ).filter((e) => e.offsetParent !== null);
        if (!list.length) return;
        const first = list[0],
          last = list[list.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    }
    document.addEventListener("keydown", key);
    const overflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      clearTimeout(timer);
      document.removeEventListener("keydown", key);
      document.body.style.overflow = overflow;
      before?.focus();
    };
  }, [busy]);
  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
    >
      <div
        ref={ref}
        className="glass modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="modal-title"
      >
        <div className="modal-head">
          <h2 id="modal-title">{title}</h2>
          <button
            className="icon-button"
            aria-label="关闭窗口"
            disabled={busy}
            onClick={onClose}
          >
            <X size={20} />
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}
function Probe({ node: n }: { node: Any }) {
  const p = n.probe ?? {},
    online = n.online,
    cpu = online ? Math.max(0, Math.min(100, p.cpu_percent ?? 0)) : 0,
    mem = p.memory_total ? (100 * p.memory_used) / p.memory_total : 0;
  return (
    <div className="probe">
      <div>
        <span>CPU</span>
        <strong>{online ? cpu.toFixed(1) + "%" : "—"}</strong>
        <i>
          <b style={{ width: cpu + "%" }} />
        </i>
      </div>
      <div>
        <span>内存</span>
        <strong>{online ? mem.toFixed(1) + "%" : "—"}</strong>
        <i>
          <b style={{ width: Math.min(100, mem) + "%" }} />
        </i>
      </div>
      <div className="rate">
        <span>
          <ArrowUp size={13} />
          上行
        </span>
        <strong>{online ? bytes(p.up_bytes_per_second) + "/s" : "—"}</strong>
      </div>
      <div className="rate">
        <span>
          <ArrowDown size={13} />
          下行
        </span>
        <strong>{online ? bytes(p.down_bytes_per_second) + "/s" : "—"}</strong>
      </div>
    </div>
  );
}
const userNav = [
  ["/app", "工作概览", LayoutDashboard],
  ["/app/ports", "我的转发", Network],
  ["/app/subscriptions", "我的套餐", Package],
  ["/app/plans", "购买套餐", ShoppingBag],
  ["/app/orders", "我的订单", ClipboardList],
  ["/app/monitor", "服务器监测", Activity],
  ["/app/activity", "我的动态", History],
  ["/account", "账户设置", Settings2],
] as const;
const adminNav = [
  ["/admin", "管理概览", LayoutDashboard],
  ["/admin/nodes", "转发服务器", Server],
  ["/admin/forwards", "全部转发", ArrowLeftRight],
  ["/admin/pools", "端口资源池", Network],
  ["/admin/plans", "套餐管理", Package],
  ["/admin/leases", "用户套餐", WalletCards],
  ["/admin/users", "用户管理", Users],
  ["/admin/orders", "全部订单", ClipboardList],
  ["/admin/payments", "支付配置", WalletCards],
  ["/admin/audit", "操作记录", History],
  ["/admin/mail", "邮件配置", Mail],
  ["/admin/settings", "面板设置", Settings2],
] as const;
function DataTable({
  heads,
  rows,
  empty,
}: {
  heads: string[];
  rows: React.ReactNode[];
  empty: string;
}) {
  const [page, setPage] = useState(0),
    size = 20,
    total = Math.max(1, Math.ceil(rows.length / size)),
    current = Math.min(page, total - 1);
  return (
    <>
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              {heads.map((h) => (
                <th key={h} scope="col">
                  {h}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>{rows.slice(current * size, (current + 1) * size)}</tbody>
        </table>
        {!rows.length && <Empty title={empty} />}
      </div>
      {rows.length > 0 && (
        <div className="table-pagination">
          <span>共 {rows.length} 条</span>
          <div>
            <Button
              disabled={current === 0}
              onClick={() => setPage(current - 1)}
            >
              上一页
            </Button>
            <span>
              {current + 1} / {total}
            </span>
            <Button
              disabled={current + 1 === total}
              onClick={() => setPage(current + 1)}
            >
              下一页
            </Button>
          </div>
        </div>
      )}
    </>
  );
}
function App() {
  const [boot, setBoot] = useState<Boot | null>(null),
    [data, setData] = useState<Snapshot | null>(null),
    [path, setPath] = useState(location.pathname),
    [error, setError] = useState(""),
    [toast, setToast] = useState(""),
    [busy, setBusy] = useState(false),
    [popup, setPopup] = useState<{ kind: string; item?: Any } | null>(null),
    [formError, setFormError] = useState(""),
    [mobile, setMobile] = useState(false),
    [search, setSearch] = useState(""),
    [dark, setDark] = useState(
      () => localStorage.getItem("vistart.theme") === "dark",
    );
  const [probeNow, setProbeNow] = useState(Date.now());
  useEffect(() => {
    const timer = setInterval(() => setProbeNow(Date.now()), 3000);
    return () => clearInterval(timer);
  }, []);
  const csrf = useRef(""),
    toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null),
    pollRunning = useRef(false);
  const user = boot?.user,
    admin = !!user && user.role === "admin" && path.startsWith("/admin"),
    nav = admin ? adminNav : userNav;
  function go(p: string) {
    if (!p.startsWith("/") || p.startsWith("//")) return;
    history.pushState(null, "", p);
    setPath(p);
    setPopup(null);
    setMobile(false);
    setSearch("");
    window.scrollTo(0, 0);
  }
  function notice(s: string) {
    setToast(s);
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(""), 4500);
  }
  function open(kind: string, item?: Any) {
    setFormError("");
    setPopup({ kind, item });
  }
  async function api(route: string, body?: Any) {
    const response = await fetch("/api/" + route, {
      credentials: "same-origin",
      headers: {
        "Content-Type": "application/json",
        ...(body ? { "X-CSRF-Token": csrf.current } : {}),
      },
      ...(body ? { method: "POST", body: JSON.stringify(body) } : {}),
    });
    let out;
    try {
      out = await response.json();
    } catch {
      throw Error("服务器返回异常，请稍后重试");
    }
    if (out.csrf) csrf.current = out.csrf;
    if (!response.ok) {
      if (response.status === 401 && boot?.user) {
        setBoot((b) => (b ? { ...b, user: null } : b));
        setData(null);
        go("/login");
      }
      throw Error(out.error ?? "操作未完成");
    }
    return out;
  }
  async function connectNode(route: string, request: Any) {
    setBusy(true); setFormError("");
    try {
      const key = await api("admin/inspect-ssh", {
        ssh_host: request.ssh_host, ssh_port: request.ssh_port, username: request.username,
      });
      if (key.changed) {
        setPopup({kind:"ssh-key-confirm", item:{route,request,key}});
        return;
      }
      await run(route, {...request, confirmed_fingerprint:key.fingerprint},
        route === "admin/connection" && !request.redeploy ? "连接信息已验证并保存" : "部署任务已启动");
    } catch (e) {setFormError((e as Error).message);}
    finally {setBusy(false);}
  }
  async function bootstrap() {
    try {
      const b = await api("bootstrap");
      csrf.current = b.csrf;
      setBoot(b);
      setError("");
      return b;
    } catch (e) {
      setError((e as Error).message);
      return null;
    }
  }
  async function refresh(asAdmin = admin) {
    if (pollRunning.current) return;
    pollRunning.current = true;
    try {
      const d = await api("state" + (asAdmin ? "?scope=admin" : ""));
      if (asAdmin !== location.pathname.startsWith("/admin")) return;
      setData({ ...d, receivedAt: Date.now() });
      setBoot((b) =>
        b
          ? {
              ...b,
              user: d.user,
              site_name: d.settings.site_name,
              registration: d.settings.registration,
              email_verification: d.settings.email_verification,
            }
          : b,
      );
      setError("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      pollRunning.current = false;
    }
  }
  useEffect(() => {
    bootstrap();
    const pop = () => {
      setPath(location.pathname);
      setPopup(null);
      setSearch("");
    };
    addEventListener("popstate", pop);
    return () => removeEventListener("popstate", pop);
  }, []);
  useEffect(() => {
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    localStorage.setItem("vistart.theme", dark ? "dark" : "light");
  }, [dark]);
  useEffect(() => {
    if (!boot) return;
    if (!boot.ready) {
      if (path !== "/install") go("/install");
      return;
    }
    if (!boot.user) {
      if (!["/login", "/register", "/forgot-password"].includes(path))
        go("/login");
    } else if (
      path === "/install" ||
      path === "/login" ||
      path === "/register" ||
      path === "/" ||
      (path.startsWith("/admin") && boot.user.role !== "admin")
    )
      go("/app");
  }, [boot?.ready, user?.id, user?.role, path]);
  useEffect(() => {
    if (!user) return;
    setData(null);
    refresh(admin);
    const timer = setInterval(() => {
      if (!document.hidden) refresh(admin);
    }, 3000);
    return () => clearInterval(timer);
  }, [user?.id, admin]);
  useEffect(() => {
    document.title =
      (nav.find((n) => n[0] === path)?.[1] ??
        (!boot?.ready
          ? "安装面板"
          : path === "/register"
            ? "注册账户"
            : "登录")) +
      " · " +
      (boot?.site_name ?? "Vistart Ports");
  }, [path, boot?.site_name]);
  async function run(route: string, body: Any, message = "已保存") {
    setBusy(true);
    setFormError("");
    try {
      const r = await api(route, body);
      await refresh();
      setPopup(null);
      notice(message);
      return r;
    } catch (e) {
      setFormError((e as Error).message);
      throw e;
    } finally {
      setBusy(false);
    }
  }
  async function act(route: string, body: Any, message?: string) {
    try {
      return await run(route, body, message);
    } catch (e) {
      notice((e as Error).message);
    }
  }
  const submit =
    (
      route: string,
      transform: (v: Record<string, string>, f: HTMLFormElement) => Any = (v) =>
        v,
      message?: string,
    ) =>
    (e: React.FormEvent<HTMLFormElement>) => {
      const f = e.currentTarget,
        v = formdata(e);
      run(route, transform(v, f), message).catch(() => {});
    };
  const footer = (label = "保存") => (
    <div className="form-footer">
      <Button type="button" onClick={() => setPopup(null)} disabled={busy}>
        取消
      </Button>
      <Button primary type="submit" disabled={busy}>
        {busy ? "正在处理…" : label}
      </Button>
    </div>
  );
  const feedback = (
    <>
      {formError && (
        <p role="alert" className="form-error">
          {formError}
        </p>
      )}
    </>
  );
  if (!boot)
    return (
      <div className="loading">
        <Logo />
        <p>{error || "正在连接面板…"}</p>
        {error && <Button onClick={bootstrap}>重试</Button>}
      </div>
    );
  if (!boot.ready)
    return (
      <div className="install-shell">
        <div className="install-intro">
          <Logo />
          <span className="eyebrow">初次部署</span>
          <h1>安装转发面板</h1>
          <p>
            连接数据库，创建管理员账户。
            <br />
            安装后即可添加转发服务器。
          </p>
          <div className="checks">
            {Object.entries(boot.checks ?? {}).map(([k, ok]) => (
              <Tag key={k} good={ok} warn={!ok}>
                {ok ? <Check size={13} /> : <X size={13} />}{" "}
                {
                  (
                    {
                      php: "PHP 8.0+",
                      mysql: "MySQL 扩展",
                      curl: "cURL",
                      openssl: "OpenSSL",
                      mbstring: "多字节支持",
                      process: "进程支持",
                      writable: "配置目录",
                    } as Any
                  )[k]
                }
              </Tag>
            ))}
          </div>
          <small>转发程序运行在独立节点上。主控负责管理、计费与通信。</small>
        </div>
        <Box className="install-form">
          <div className="form-intro">
            <h2>配置你的面板</h2>
            <p>数据库需提前创建，安装程序仅创建以 vp_ 开头的数据表。</p>
          </div>
          <form
            onSubmit={async (e) => {
              const v = formdata(e);
              setBusy(true);
              setFormError("");
              try {
                await api("install", { ...v, db_port: Number(v.db_port) });
                await bootstrap();
                go("/login");
                notice("安装完成，请登录管理员账户");
              } catch (e) {
                setFormError((e as Error).message);
              } finally {
                setBusy(false);
              }
            }}
          >
            <h3 className="section-label">数据库连接</h3>
            <div className="form-grid">
              <Field
                label="数据库地址"
                name="db_host"
                defaultValue="127.0.0.1"
                required
              />
              <Field
                label="数据库端口"
                name="db_port"
                defaultValue="3306"
                type="number"
                min="1"
                max="65535"
                required
              />
              <Field label="数据库名称" name="db_name" required />
              <Field label="数据库用户名" name="db_user" required />
            </div>
            <Field
              label="数据库密码"
              name="db_password"
              type="password"
              autoComplete="off"
            />
            <h3 className="section-label">站点与管理员</h3>
            <Field
              label="站点名称"
              name="site_name"
              defaultValue="Vistart Ports"
              maxLength={60}
              required
            />
            <div className="form-grid">
              <Field
                label="管理员用户名"
                name="admin_username"
                defaultValue="admin"
                minLength={3}
                maxLength={80}
                required
              />
              <Field
                label="管理员密码"
                name="admin_password"
                type="password"
                autoComplete="new-password"
                minLength={12}
                maxLength={128}
                required
              />
            </div>
            <p className="hint">
              安装会锁定此入口，并启动随包提供的主控通信服务。Nginx 的 WSS
              代理按部署说明配置。
            </p>
            {feedback}
            <Button
              primary
              disabled={
                busy || Object.values(boot.checks ?? {}).some((x) => !x)
              }
            >
              {busy ? "正在安装，请稍候…" : "安装面板"}
              <ArrowRight size={16} />
            </Button>
          </form>
        </Box>
      </div>
    );
  if (!user)
    return (
      <AuthScreen
        boot={boot}
        path={path}
        go={go}
        api={api}
        onLogin={(u) => {
          setBoot((b) => (b ? { ...b, user: u } : b));
          go("/app");
        }}
      />
    );
  const nodes = data?.nodes ?? [],
    leases = data?.leases ?? [],
    ports = (data?.allocations ?? []).filter((a) => !a.released),
    plans = data?.plans ?? [],
    orders = [...(data?.orders ?? [])].sort(
      (a, b) => b.created_at - a.created_at,
    ),
    accounts = data?.users ?? [];
  const myLeases = leases.filter((l) => l.user_id === user.id),
    mine = ports.filter((a) => myLeases.some((l) => l.id === a.lease_id)),
    nodeName = (id: string) =>
      nodes.find((n) => n.id === id)?.name ?? "未知节点",
    accountName = (id: string) =>
      accounts.find((a) => a.id === id)?.name ?? id.slice(0, 8);
  const match = (...v: any[]) =>
    !search || v.join(" ").toLowerCase().includes(search.toLowerCase());
  const heading = (title: string, desc: string, action?: React.ReactNode) => (
    <div className="page-heading">
      <div>
        <h1>{title}</h1>
        <p>{desc}</p>
      </div>
      {action}
    </div>
  );
  const stats = (items: [string, React.ReactNode, string][]) => (
    <div className="stats">
      {items.map(([title, num, small]) => (
        <div className="glass stat" key={title}>
          <span>{title}</span>
          <strong>{num}</strong>
          <small>{small}</small>
        </div>
      ))}
    </div>
  );
  const toolbar = (placeholder = "搜索名称或地址") => (
    <div className="table-toolbar">
      <Search size={16} />
      <input
        aria-label="搜索"
        placeholder={placeholder}
        value={search}
        onChange={(e) => setSearch(e.target.value)}
      />
      <span>
        最近更新{" "}
        {data
          ? new Date(data.now * 1000).toLocaleTimeString("zh-CN", {
              hour12: false,
            })
          : "—"}
      </span>
    </div>
  );
  const table = (
    heads: string[],
    rows: React.ReactNode[],
    empty = "暂无记录",
  ) => (
    <DataTable key={path + search} heads={heads} rows={rows} empty={empty} />
  );
  async function newOrder(plan: Any, lease?: Any, kind = "purchase") {
    try {
      const r = await run(
        "order",
        { plan_id: plan.id, lease_id: lease?.id ?? "", kind },
        "订单已创建",
      );
      open("checkout", r);
    } catch (e) {
      notice((e as Error).message);
    }
  }
  function nodeCards(list: Any[], manage = false) {
    return (
      <div className="node-grid">
        {list.map((n) => (
          <Box key={n.id} className="node-card">
            <div className="node-card-head">
              <span className="small-icon">
                <Server size={22} />
              </span>
              <div>
                <h3>{n.name}</h3>
                <p>{n.meta?.region || "未设置地区"}</p>
              </div>
              <Tag good={n.online} warn={n.enabled && !n.online}>
                {!n.enabled ? "已停用" : n.online ? "在线" : "离线"}
              </Tag>
            </div>
            <div className="node-address">
              {n.meta?.public_ip || "—"}
              <span>连接地址</span>
            </div>
            <Probe node={n} />
            <div className="node-card-footer">
              <small>最近上报 {date(n.last_seen)}</small>
              <button
                className="text-button"
                onClick={() => open(manage ? "node-detail" : "probe", n)}
              >
                查看详情
                <ChevronRight size={14} />
              </button>
            </div>
          </Box>
        ))}
      </div>
    );
  }
  function eventTable() {
    return table(
      ["时间", "操作人", "操作", "详情"],
      (data?.audit ?? [])
        .filter((a) => match(a.actor, a.action, a.detail))
        .map((a) => (
          <tr key={a.id}>
            <td className="muted">{date(Number(a.at))}</td>
            <td>{a.actor}</td>
            <td>{a.action}</td>
            <td className="wrap">{a.detail || "—"}</td>
          </tr>
        )),
    );
  }
  let page: React.ReactNode;
  if (!data)
    page = (
      <Box>
        <Empty
          title={error || "正在加载…"}
          action={
            error ? (
              <Button onClick={() => refresh()}>重新连接</Button>
            ) : undefined
          }
        />
      </Box>
    );
  else if (path === "/app") {
    const live = myLeases.filter((l) => leaseState(l) === "使用中"),
      used = myLeases.reduce((s, l) => s + l.used_up + l.used_down, 0);
    page = (
      <>
        {heading("工作概览", "查看当前套餐用量和转发状态。")}
        {stats([
          ["有效套餐", live.length, "独立额度"],
          ["使用中的端口", mine.length, "TCP + UDP"],
          ["本期已用流量", bytes(used), "上行 + 下行"],
          [
            "可用节点",
            new Set(live.flatMap((l) => l.node_ids)).size,
            "套餐内节点",
          ],
        ])}
        <div className="content-grid">
          <Box
            title="我的套餐"
            action={
              <button
                className="text-button"
                onClick={() => go("/app/subscriptions")}
              >
                查看全部
                <ChevronRight size={14} />
              </button>
            }
          >
            {!myLeases.length ? (
              <Empty
                title="尚未开通套餐"
                detail="选择套餐后即可添加转发。"
                action={
                  <Button primary onClick={() => go("/app/plans")}>
                    浏览套餐
                  </Button>
                }
              />
            ) : (
              myLeases.slice(0, 4).map((l) => (
                <div className="summary-row" key={l.id}>
                  <span className="small-icon">
                    <Package size={19} />
                  </span>
                  <div>
                    <strong>{l.plan_name}</strong>
                    <small>
                      {l.used_ports} / {l.port_limit} 端口 ·{" "}
                      {date(l.expires_at)} 到期
                    </small>
                  </div>
                  <Tag good={leaseState(l) === "使用中"}>{leaseState(l)}</Tag>
                </div>
              ))
            )}
          </Box>
          <Box
            title="最近转发"
            action={
              <button className="text-button" onClick={() => go("/app/ports")}>
                查看全部
                <ChevronRight size={14} />
              </button>
            }
          >
            {mine.slice(0, 4).map((a) => (
              <div className="summary-row" key={a.id}>
                <Network size={18} />
                <div>
                  <strong className="mono">
                    {endpoint(a.public_ip, a.port)}
                  </strong>
                  <small>{a.remark || nodeName(a.node_id)}</small>
                </div>
                <Tag good={nodes.find((n) => n.id === a.node_id)?.online}>
                  {nodes.find((n) => n.id === a.node_id)?.online
                    ? "节点在线"
                    : "节点离线"}
                </Tag>
              </div>
            ))}
            {!mine.length && (
              <Empty
                title="暂无转发记录"
                detail="开通套餐后，可在「我的转发」中配置目标。"
              />
            )}
          </Box>
        </div>
      </>
    );
  } else if (path === "/app/ports") {
    page = (
      <>
        {heading(
          "我的转发",
          "TCP + UDP 共用端口与目标；节点监听与目标连通分别显示。",
          <Button primary onClick={() => open("claim")}>
            <Plus size={16} />
            添加转发
          </Button>,
        )}
        <Box>
          {toolbar()}
          {table(
            ["节点 / 连接地址", "目标地址", "所属套餐", "实时连接", "状态", "操作"],
            mine
              .filter((a) =>
                match(
                  nodeName(a.node_id),
                  a.public_ip,
                  a.port,
                  a.target_host,
                  a.remark,
                  ...(a.targets ?? []).map((t: Any) => t.host),
                ),
              )
              .map((a) => {
                const l = myLeases.find((l) => l.id === a.lease_id)!,
                  n = nodes.find((n) => n.id === a.node_id);
                return (
                  <tr key={a.id}>
                    <td>
                      <strong>{endpoint(a.public_ip, a.port)}</strong>
                      <small>{nodeName(a.node_id)} · TCP + UDP</small>
                    </td>
                    <td>
                      <TargetAddresses
                        allocation={a}
                        now={
                          data.now +
                          Math.max(0, probeNow - data.receivedAt) / 1000
                        }
                      />
                      {a.remark && <small>{a.remark}</small>}
                    </td>
                    <td>{l.plan_name}</td>
                    <td>
                      <RuleConnections allocation={a} now={data.now + Math.max(0, probeNow - data.receivedAt) / 1000} />
                    </td>
                    <td>
                      <Tag good={a.apply_status === "listening"} warn={forwardingWarn(a)}>
                        {leaseState(l) !== "使用中" ? leaseState(l) : forwardingLabel(a)}
                      </Tag>
                    </td>
                    <td>
                      <div className="row-actions">
                        <button
                          onClick={() => {
                            navigator.clipboard
                              .writeText(endpoint(a.public_ip, a.port))
                              .then(() => notice("连接地址已复制"))
                              .catch(() => notice("请手动复制连接地址"));
                          }}
                          aria-label="复制连接地址"
                        >
                          <Copy size={15} />
                        </button>
                        <button onClick={() => open("target", a)}>配置</button>
                        <button
                          className="danger-text"
                          onClick={() => open("release", a)}
                        >
                          释放
                        </button>
                      </div>
                    </td>
                  </tr>
                );
              }),
            "还没有转发入口",
          )}
        </Box>
      </>
    );
  } else if (path === "/app/subscriptions" || path === "/admin/leases") {
    const ls = admin ? leases : myLeases;
    page = (
      <>
        {heading(
          admin ? "用户套餐" : "我的套餐",
          "各套餐独立计量，同一套餐的节点共用额度。",
          admin ? (
            <Button primary onClick={() => open("grant")}>
              <Plus size={16} />
              开通套餐
            </Button>
          ) : (
            <Button primary onClick={() => go("/app/plans")}>
              购买套餐
              <ArrowRight size={16} />
            </Button>
          ),
        )}
        <div className="lease-grid">
          {ls.map((l) => {
            const p = plans.find((p) => p.id === l.plan_id),
              used = l.used_up + l.used_down;
            return (
              <Box key={l.id} className="lease-card">
                <div className="lease-title">
                  <span className="small-icon">
                    <Package size={20} />
                  </span>
                  <div>
                    <h3>{l.plan_name}</h3>
                    <small>
                      {admin
                        ? accountName(l.user_id)
                        : "套餐编号 " + l.id.slice(0, 8)}
                    </small>
                  </div>
                  <Tag good={leaseState(l) === "使用中"}>{leaseState(l)}</Tag>
                </div>
                <div className="quota-main">
                  <strong>
                    {l.used_ports}
                    <small> / {l.port_limit}</small>
                  </strong>
                  <span>已用端口</span>
                </div>
                <div className="meter">
                  <i
                    style={{
                      width:
                        Math.min(100, (l.used_ports / l.port_limit) * 100) +
                        "%",
                    }}
                  />
                </div>
                <div className="traffic-line">
                  <span>本期流量</span>
                  <strong>
                    {bytes(used)} /{" "}
                    {l.traffic_limit_bytes
                      ? bytes(l.traffic_limit_bytes)
                      : "不限量"}
                  </strong>
                </div>
                <div className="meter thin">
                  <i
                    style={{
                      width: l.traffic_limit_bytes
                        ? Math.min(100, (used / l.traffic_limit_bytes) * 100) +
                          "%"
                        : "0%",
                    }}
                  />
                </div>
                <dl className="details">
                  <div><dt>跨节点总带宽</dt><dd>{l.bandwidth_mbps ? l.bandwidth_mbps+" Mbps" : "不限"}<small>上行 + 下行</small></dd></div>
                  <div><dt>TCP / UDP 上限</dt><dd>{l.tcp_limit || "不限"} / {l.udp_limit || "不限"}</dd></div>
                  <div>
                    <dt>下次自然重置</dt>
                    <dd>{date(l.next_reset_at)}</dd>
                  </div>
                  <div>
                    <dt>套餐到期</dt>
                    <dd>{date(l.expires_at)}</dd>
                  </div>
                  <div>
                    <dt>可用节点</dt>
                    <dd>{l.node_ids.map(nodeName).join("、")}</dd>
                  </div>
                </dl>
                <div className="card-actions">
                  {admin ? (
                    <>
                      <Button onClick={() => open("lease", l)}>管理套餐</Button>
                      <Button danger onClick={() => open("delete-lease", l)}>删除套餐</Button>
                    </>
                  ) : (
                    !l.ended && (
                      <>
                        <Button
                          disabled={!p?.enabled}
                          onClick={() => p && newOrder(p, l)}
                        >
                          续费
                        </Button>
                        <Button
                          disabled={
                            !p?.enabled || l.expires_at <= Date.now() / 1000
                          }
                          onClick={() => p && open("reset", { ...l, plan: p })}
                        >
                          重置流量 {p ? money(p.reset_price_cents) : ""}
                        </Button>
                      </>
                    )
                  )}
                  {!admin && (l.ended || l.expires_at <= Date.now() / 1000) && (
                    <Button danger onClick={() => open("delete-lease", l)}>删除套餐</Button>
                  )}
                </div>
              </Box>
            );
          })}
        </div>
        {!ls.length && (
          <Box>
            <Empty title="暂无套餐" detail="购买套餐后即可添加转发。" />
          </Box>
        )}
      </>
    );
  } else if (path === "/app/plans" || path === "/admin/plans") {
    page = (
      <>
        {heading(
          admin ? "套餐管理" : "购买套餐",
          admin
            ? "设置价格、流量、端口额度与可用节点。"
            : "套餐内节点共用端口与流量额度。",
          admin ? (
            <Button primary onClick={() => open("plan")}>
              <Plus size={16} />
              添加套餐
            </Button>
          ) : undefined,
        )}
        <div className="plan-grid">
          {plans
            .filter((p) => admin || p.enabled)
            .map((p) => (
              <Box key={p.id} className="plan-card">
                <div className="plan-name">
                  <Package size={22} />
                  <Tag good={p.enabled}>{p.enabled ? "可购买" : "已下架"}</Tag>
                </div>
                <h2>{p.name}</h2>
                <div className="price">
                  {money(p.price_cents)}
                  <span> / {p.period_days} 天</span>
                </div>
                <ul className="features">
                  <li>
                    <Check size={15} />
                    {p.port_limit} 组 TCP + UDP 端口
                  </li>
                  <li>
                    <Check size={15} />
                    {p.traffic_limit_bytes
                      ? bytes(p.traffic_limit_bytes) + " 周期流量"
                      : "不限流量"}
                  </li>
                  <li>
                    <Check size={15} />
                    {p.node_ids.length} 台可用节点
                  </li>
                  <li><Check size={15} />{p.bandwidth_mbps ? p.bandwidth_mbps+" Mbps 总带宽" : "不限带宽"}</li>
                  <li><Check size={15} />TCP {p.tcp_limit || "不限"} · UDP {p.udp_limit || "不限"}</li>
                  <li>
                    <Check size={15} />
                    单次流量重置 {money(p.reset_price_cents)}
                  </li>
                </ul>
                <p className="node-list">
                  {p.node_ids.map(nodeName).join(" · ")}
                </p>
                {admin ? (
                  <div className="card-actions">
                    <Button onClick={() => open("plan", p)}>编辑套餐</Button>
                    <Button danger onClick={() => open("delete-plan", p)}>删除套餐</Button>
                  </div>
                ) : (
                  <Button
                    primary
                    disabled={busy}
                    onClick={() => {
                      const ls = myLeases.filter(
                        (l) => l.plan_id === p.id && !l.ended,
                      );
                      if (ls.length > 1)
                        open("renew-select", { plan: p, leases: ls });
                      else newOrder(p, ls[0]);
                    }}
                  >
                    {myLeases.some((l) => l.plan_id === p.id && !l.ended)
                      ? "续费已有套餐"
                      : "购买套餐"}
                    <ArrowRight size={16} />
                  </Button>
                )}
              </Box>
            ))}
        </div>
        {!plans.some((p) => admin || p.enabled) && (
          <Box>
            <Empty title="暂无可购买套餐" detail="请稍后再来查看。" />
          </Box>
        )}
        <p className="page-note">续费延长有效期，端口额度和已用流量不变。</p>
      </>
    );
  } else if (path === "/app/orders" || path === "/admin/orders") {
    page = (
      <>
        {heading(admin ? "全部订单" : "我的订单", "付款成功后自动更新状态。")}
        <Box>
          {toolbar("搜索订单号或套餐")}
          {table(
            ["订单", "套餐 / 类型", "金额", "状态", "创建时间", "操作"],
            orders
              .filter((o) => match(o.id, o.snapshot.name))
              .map((o) => (
                <tr key={o.id}>
                  <td className="mono">
                    {o.id.slice(0, 12)}
                    <small>{admin ? accountName(o.user_id) : ""}</small>
                  </td>
                  <td>
                    <strong>{o.snapshot.name}</strong>
                    <small>{kindLabel[o.kind]}</small>
                  </td>
                  <td>{money(o.amount_cents)}</td>
                  <td>
                    <Tag
                      good={o.status === "paid"}
                      warn={o.status === "paid_review"}
                    >
                      {orderLabel[o.status] ?? o.status}
                    </Tag>
                  </td>
                  <td className="muted">{date(o.created_at)}</td>
                  <td>
                    <div className="row-actions">
                      <button onClick={() => open("order", o)}>详情</button>
                      {!admin && o.status === "pending" && (
                        <>
                          <button onClick={() => open("checkout", o)}>
                            付款
                          </button>
                          <button onClick={() => open("cancel-order", o)}>
                            取消
                          </button>
                        </>
                      )}
                    </div>
                  </td>
                </tr>
              )),
          )}
        </Box>
      </>
    );
  } else if (path === "/app/monitor") {
    const ids = new Set(
        myLeases
          .filter((l) => !l.ended && l.expires_at > Date.now() / 1000)
          .flatMap((l) => l.node_ids),
      ),
      visible = nodes.filter((n) => ids.has(n.id));
    page = (
      <>
        {heading("服务器监测", "节点 CPU、内存与实时网速。")}
        {nodeCards(visible)}
        {!visible.length && (
          <Box>
            <Empty
              title="暂无可查看的节点"
              detail="购买套餐后可查看节点状态。"
            />
          </Box>
        )}
        <p className="page-note">网速为节点总速率；套餐仅统计你的转发流量。</p>
      </>
    );
  } else if (path === "/admin") {
    const online = nodes.filter((n) => n.online).length,
      pendingJobs = data.jobs.filter((j) => !j.done);
    page = (
      <>
        {heading(
          "管理概览",
          "资源、订单与节点运行状态。",
          <Button primary onClick={() => go("/admin/nodes")}>
            管理节点
            <ArrowRight size={16} />
          </Button>,
        )}
        {stats([
          [
            "在线节点",
            <>
              {online}
              <em> / {nodes.length}</em>
            </>,
            "独立转发服务器",
          ],
          ["注册用户", accounts.length, "包含管理员账户"],
          ["已分配端口", ports.length, "TCP + UDP 成对分配"],
          [
            "有效套餐",
            leases.filter((l) => leaseState(l) === "使用中").length,
            "各套餐独立计量",
          ],
        ])}
        {pendingJobs.length > 0 && (
          <Box title="部署进度">
            {pendingJobs.map((j) => (
              <div className="summary-row" key={j.id}>
                <RefreshCw size={18} className="spinning" />
                <div>
                  <strong>{j.name}</strong>
                  <small>{j.message}</small>
                </div>
              </div>
            ))}
          </Box>
        )}
        <div className="content-grid">
          <Box
            title="节点状态"
            action={
              <button
                className="text-button"
                onClick={() => go("/admin/nodes")}
              >
                查看全部
                <ChevronRight size={14} />
              </button>
            }
          >
            {nodes.length ? (
              nodes.map((n) => (
                <div className="summary-row" key={n.id}>
                  <Server size={19} />
                  <div>
                    <strong>{n.name}</strong>
                    <small>
                      {n.meta.region} · {n.meta.public_ip}
                    </small>
                  </div>
                  <Tag good={n.online}>{n.online ? "在线" : "离线"}</Tag>
                </div>
              ))
            ) : (
              <Empty
                title="尚未添加转发服务器"
                action={
                  <Button onClick={() => open("node")}>添加第一台节点</Button>
                }
              />
            )}
          </Box>
          <Box title="最近订单">
            {orders.slice(0, 5).map((o) => (
              <div className="summary-row" key={o.id}>
                <Package size={18} />
                <div>
                  <strong>{o.snapshot.name}</strong>
                  <small>
                    {accountName(o.user_id)} · {kindLabel[o.kind]}
                  </small>
                </div>
                <strong>{money(o.amount_cents)}</strong>
              </div>
            ))}
            {!orders.length && <Empty title="暂无订单" />}
          </Box>
        </div>
      </>
    );
  } else if (path === "/admin/nodes") {
    page = (
      <>
        {heading(
          "转发服务器",
          "添加独立服务器，自动检测系统并部署 Agent 与转发内核。",
          <Button primary onClick={() => open("node")}>
            <Plus size={16} />
            添加服务器
          </Button>,
        )}
        <div className="toolbar">
          <span className="hint">
            {data.agent_release?.checking ? "正在检查 Agent 版本…" :
              data.agent_release?.status === "ok" ? "最新稳定版 " + data.agent_release.version :
              "版本检查暂不可用"}
          </span>
          <Button disabled={busy || data.agent_release?.checking}
            onClick={() => act("admin/check-agent-release", {}, "已提交版本检查")}>检查更新</Button>
        </div>
        {data.jobs.length > 0 && (
          <Box title="部署任务">
            <div className="job-list">
              {[...data.jobs]
                .sort((a, b) => b.at - a.at)
                .slice(0, 5)
                .map((j) => (
                  <div key={j.id} className="summary-row">
                    {!j.done ? (
                      <RefreshCw size={18} className="spinning" />
                    ) : j.ok ? (
                      <Check size={18} />
                    ) : (
                      <X size={18} />
                    )}
                    <div>
                      <strong>{j.name}</strong>
                      <small>{j.message}</small>
                    </div>
                    <Tag good={j.ok} warn={j.done && !j.ok}>
                      {j.done ? (j.ok ? "完成" : "未完成") : "部署中"}
                    </Tag>
                    {j.done && !j.ok && (
                      <button
                        className="text-button"
                        onClick={() =>
                          nodes.some((n)=>n.id===j.node_id)
                            ? open("connection", nodes.find((n)=>n.id===j.node_id))
                            : open("node", {name:j.name})
                        }
                      >
                        重试
                      </button>
                    )}
                  </div>
                ))}
            </div>
          </Box>
        )}
        <Box>
          {toolbar("搜索服务器名称、地区或 IP")}
          {table(
            [
              "服务器",
              "连接地址",
              "状态",
              "资源使用",
              "实时网速",
              "已用端口",
              "操作",
            ],
            nodes
              .filter((n) => match(n.name, n.meta?.region, n.meta?.public_ip))
              .map((n) => (
                <tr key={n.id}>
                  <td>
                    <div className="node-compact">
                      <span className="small-icon">
                        <Server size={18} />
                      </span>
                      <div>
                        <strong>{n.name}</strong>
                        <small>{n.meta?.region || "—"}</small>
                      </div>
                    </div>
                  </td>
                  <td className="mono">
                    {n.meta?.public_ip || "—"}
                    <small>Agent {n.agent_version || "待部署"}</small>
                  </td>
                  <td>
                    <Tag good={n.online} warn={n.enabled && !n.online}>
                      {n.removal_status === "waiting"
                        ? "正在移除"
                        : !n.enabled
                          ? "已停用"
                          : n.online
                            ? "在线"
                            : "离线"}
                    </Tag>
                  </td>
                  <td>
                    <div className="resource-inline">
                      CPU{" "}
                      {n.online
                        ? Number(n.probe?.cpu_percent ?? 0).toFixed(1) + "%"
                        : "—"}
                      <span>
                        内存{" "}
                        {n.online && n.probe?.memory_total
                          ? Math.round(
                              (n.probe.memory_used / n.probe.memory_total) *
                                100,
                            ) + "%"
                          : "—"}
                      </span>
                    </div>
                  </td>
                  <td>
                    <div className="resource-inline">
                      ↑{" "}
                      {n.online
                        ? bytes(n.probe?.up_bytes_per_second) + "/s"
                        : "—"}
                      <span>
                        ↓{" "}
                        {n.online
                          ? bytes(n.probe?.down_bytes_per_second) + "/s"
                          : "—"}
                      </span>
                    </div>
                  </td>
                  <td>{ports.filter((a) => a.node_id === n.id).length}</td>
                  <td>
                    <div className="row-actions">
                      <button onClick={() => open("node-detail", n)}>
                        详情
                      </button>
                      <button
                        onClick={() => open("node-edit", n)}
                        disabled={!!n.removal_status}
                      >
                        编辑
                      </button>
                      <button onClick={() => open("connection", n)} disabled={!!n.removal_status}>
                        连接信息
                      </button>
                      <button onClick={() => act("admin/update-agent", {node_id:n.id}, "更新任务已启动")}
                        disabled={busy || !!n.removal_status || !n.update_available || !n.meta?.has_ssh_password}
                        title={!n.meta?.has_ssh_password ? "请先保存 SSH 连接信息" : n.update_available ? "更新至 " + data.agent_release.version : "尚未检测到新版本"}>
                        更新
                      </button>
                      <button
                        className="danger-text"
                        onClick={() => open("remove-node", n)}
                      >
                        删除
                      </button>
                    </div>
                  </td>
                </tr>
              )),
          )}
        </Box>
        {!nodes.length && (
          <Box>
            <Empty
              title="等待第一台转发服务器"
              detail="仅支持 Debian / Ubuntu，程序在面板侧下载和校验后上传部署。"
            />
          </Box>
        )}
      </>
    );
  } else if (path === "/admin/forwards") {
    page = (
      <>
        {heading("全部转发", "查看用户的转发目标、租约状态及节点下发结果。")}
        <Box>
          {toolbar("搜索用户、节点、地址或备注")}
          {table(
            ["用户 / 套餐", "入口地址", "目标地址", "实时连接", "状态", "操作"],
            ports
              .filter((a) => {
                const l = leases.find((l) => l.id === a.lease_id);
                return match(
                  accountName(l?.user_id ?? ""),
                  nodeName(a.node_id),
                  a.public_ip,
                  a.port,
                  a.target_host,
                  a.remark,
                );
              })
              .map((a) => {
                const l = leases.find((l) => l.id === a.lease_id)!,
                  n = nodes.find((n) => n.id === a.node_id);
                return (
                  <tr key={a.id}>
                    <td>
                      <strong>{accountName(l.user_id)}</strong>
                      <small>{l.plan_name}</small>
                    </td>
                    <td className="mono">
                      {endpoint(a.public_ip, a.port)}
                      <small>{nodeName(a.node_id)}</small>
                    </td>
                    <td>
                      <TargetAddresses
                        allocation={a}
                        now={
                          data.now +
                          Math.max(0, probeNow - data.receivedAt) / 1000
                        }
                      />
                      {a.remark && <small>{a.remark}</small>}
                    </td>
                    <td>
                      <RuleConnections allocation={a} now={data.now + Math.max(0, probeNow - data.receivedAt) / 1000} />
                    </td>
                    <td>
                      <Tag good={a.apply_status === "listening"} warn={forwardingWarn(a)}>
                        {leaseState(l) !== "使用中" ? leaseState(l) : forwardingLabel(a)}
                      </Tag>
                    </td>
                    <td>
                      <div className="row-actions">
                        <button onClick={() => open("lease", l)}>租约</button>
                        <button
                          className="danger-text"
                          onClick={() => open("admin-release", a)}
                        >
                          释放
                        </button>
                      </div>
                    </td>
                  </tr>
                );
              }),
          )}
        </Box>
      </>
    );
  } else if (path === "/admin/pools") {
    page = (
      <>
        {heading(
          "端口资源池",
          "只在允许范围内按需分配，每个端口同时管理 TCP 和 UDP。",
          <Button primary onClick={() => open("pool")}>
            <Plus size={16} />
            添加端口池
          </Button>,
        )}
        <Box>
          {table(
            ["节点", "连接 IP", "端口范围", "已分配 / 总量", "操作"],
            data.pools.map((p) => (
              <tr key={p.node_id + p.public_ip + p.start}>
                <td>{nodeName(p.node_id)}</td>
                <td className="mono">{p.public_ip}</td>
                <td>
                  {p.start}–{p.end}
                </td>
                <td>
                  {
                    ports.filter(
                      (a) =>
                        a.node_id === p.node_id &&
                        a.public_ip === p.public_ip &&
                        a.port >= p.start &&
                        a.port <= p.end,
                    ).length
                  }{" "}
                  / {p.end - p.start + 1}
                </td>
                <td>
                  <button
                    className="text-button danger-text"
                    onClick={() => open("remove-pool", p)}
                  >
                    删除
                  </button>
                </td>
              </tr>
            )),
            "尚未配置端口池",
          )}
        </Box>
        <p className="page-note">
          端口池须与服务器和云平台的入站防火墙规则一致。面板不会修改其他服务的防火墙规则。
        </p>
      </>
    );
  } else if (path === "/admin/users") {
    page = (
      <>
        {heading(
          "用户管理",
          "管理员同时拥有普通用户的购买与使用权限。",
          <Button primary onClick={() => open("user")}>
            <Plus size={16} />
            创建用户
          </Button>,
        )}
        <Box>
          {toolbar("搜索名称或用户名")}
          {table(
            ["用户", "用户名 / 邮箱", "角色", "状态", "注册时间", "操作"],
            accounts
              .filter((a) => match(a.name, a.username))
              .map((a) => (
                <tr key={a.id}>
                  <td>
                    <strong>{a.name}</strong>
                  </td>
                  <td>{a.username}</td>
                  <td>{a.role === "admin" ? "管理员" : "普通用户"}</td>
                  <td>
                    <Tag good={!a.disabled}>
                      {a.disabled ? "已停用" : "正常"}
                    </Tag>
                  </td>
                  <td>{date(a.created_at)}</td>
                  <td>
                    <div className="row-actions">
                      <button onClick={() => open("user-leases", a)}>
                        套餐 / 租约
                      </button>
                      <button onClick={() => open("user", a)}>管理</button>
                    </div>
                  </td>
                </tr>
              )),
          )}
        </Box>
      </>
    );
  } else if (path === "/admin/payments") {
    page = (
      <>
        {heading(
          "支付配置",
          "配置易支付商户与可选支付方式。密钥仅在服务端保存。",
        )}
        <PaymentForm
          key={data.payment.merchant_id}
          payment={data.payment}
          busy={busy}
          onSave={(v) => act("admin/payment", v, "支付配置已更新")}
        />
      </>
    );
  } else if (path === "/admin/mail") {
    page = (
      <>
        {heading("邮件配置", "配置 SMTP 投递，用于注册验证与密码找回。")}
        <MailForm
          mail={data.mail}
          busy={busy}
          onSave={(v) => act("admin/mail", v, "邮件配置已保存")}
          onTest={(v) => act("admin/mail/test", v, "测试邮件已发送")}
        />
      </>
    );
  } else if (path === "/admin/settings") {
    page = (
      <>
        {heading("面板设置", "站点信息与访问设置。")}
        <Box title="基本设置">
          <form
            className="settings-form"
            onSubmit={submit(
              "admin/settings",
              (v, f) => ({
                site_name: v.site_name,
                registration: new FormData(f).has("registration"),
                email_verification: new FormData(f).has("email_verification"),
              }),
              "站点设置已更新",
            )}
          >
            <Field
              label="站点名称"
              name="site_name"
              defaultValue={data.settings.site_name}
              maxLength={60}
              required
            />
            <label className="check-line">
              <input
                type="checkbox"
                name="registration"
                defaultChecked={data.settings.registration}
              />
              <span>允许新用户注册</span>
            </label>
            <label className="check-line">
              <input
                type="checkbox"
                name="email_verification"
                defaultChecked={data.settings.email_verification}
              />
              <span>注册时验证邮箱</span>
            </label>
            <p className="hint">
              启用后需正确填写邮件验证码才能注册；服务端限制每 60
              秒最多获取一次。
            </p>
            <Field
              label="节点连接地址"
              name="controller_url"
              readOnly
              value={data.settings.controller_url.replace(/^https:/, "wss:")}
            />
            <p className="hint">
              Agent
              与转发内核使用预编译版本，部署时自动写入当前主控地址及独立节点密钥。迁移主控前请完整备份数据库与
              state 目录。
            </p>
            {feedback}
            <Button primary disabled={busy}>
              保存设置
            </Button>
          </form>
        </Box>
      </>
    );
  } else if (path === "/account") {
    page = (
      <>
        {heading("账户设置", "个人资料与账户安全。")}
        <Box title="个人资料">
          <form className="settings-form" onSubmit={submit("account")}>
            <Field
              label="用户名"
              name="username"
              value={user.username}
              readOnly
            />
            <Field
              label="显示名称"
              name="name"
              defaultValue={user.name}
              required
              maxLength={40}
            />
            <h3 className="section-label">修改密码</h3>
            <p className="hint">不修改密码可留空。</p>
            <Field
              label="当前密码"
              name="current_password"
              type="password"
              autoComplete="current-password"
            />
            <Field
              label="新密码"
              name="new_password"
              type="password"
              autoComplete="new-password"
              minLength={12}
              maxLength={128}
            />
            {user.two_factor && (
              <Field
                label="二步验证码或恢复码（修改密码时填写）"
                name="code"
                autoComplete="one-time-code"
                maxLength={32}
              />
            )}{" "}
            {feedback}
            <Button primary disabled={busy}>
              保存账户设置
            </Button>
          </form>
        </Box>
        <Box title="二步验证">
          <div className="security-row">
            <span className="small-icon">
              <ShieldCheck size={23} />
            </span>
            <div>
              <h3>验证器动态码</h3>
              <p>登录时需输入动态码，可使用恢复码。</p>
            </div>
            <Tag good={user.two_factor}>
              {user.two_factor ? "已启用" : "未启用"}
            </Tag>
            <Button
              onClick={() =>
                open(user.two_factor ? "2fa-disable" : "2fa-start")
              }
            >
              {user.two_factor ? "关闭二步验证" : "启用二步验证"}
            </Button>
          </div>
        </Box>
      </>
    );
  } else if (path === "/admin/audit" || path === "/app/activity") {
    page = (
      <>
        {heading(admin ? "操作记录" : "我的动态", "账户与资源操作记录。")}
        <Box>
          {toolbar()}
          {eventTable()}
        </Box>
      </>
    );
  } else
    page = (
      <Box>
        <Empty
          title="页面不存在"
          action={<Button onClick={() => go("/app")}>返回工作概览</Button>}
        />
      </Box>
    );
  function modalContent() {
    if (!popup || !data) return null;
    const item = popup.item ?? {};
    const form = (
      title: string,
      content: React.ReactNode,
      onSubmit: (e: React.FormEvent<HTMLFormElement>) => void,
      label = "保存",
    ) => (
      <Modal title={title} onClose={() => setPopup(null)} busy={busy}>
        <form onSubmit={onSubmit}>
          <div className="modal-body">
            {content}
            {feedback}
          </div>
          {footer(label)}
        </form>
      </Modal>
    );

    if (popup.kind === "user-leases") {
      const ls = leases.filter((l) => l.user_id === item.id);
      return (
        <Modal title={item.name + " · 套餐租约"} onClose={() => setPopup(null)}>
          <div className="modal-body">
            {ls.length ? (
              ls.map((l) => (
                <div className="user-lease-detail" key={l.id}>
                  <div>
                    <h3>{l.plan_name}</h3>
                    <Tag good={leaseState(l) === "使用中"}>{leaseState(l)}</Tag>
                  </div>
                  <dl className="details">
                    <div>
                      <dt>已用 / 总流量</dt>
                      <dd>
                        {bytes(l.used_up + l.used_down)} /{" "}
                        {l.traffic_limit_bytes
                          ? bytes(l.traffic_limit_bytes)
                          : "不限量"}
                      </dd>
                    </div>
                    <div>
                      <dt>上行 / 下行</dt>
                      <dd>
                        {bytes(l.used_up)} / {bytes(l.used_down)}
                      </dd>
                    </div>
                    <div>
                      <dt>端口使用</dt>
                      <dd>
                        {l.used_ports} / {l.port_limit}
                      </dd>
                    </div>
                    <div>
                      <dt>套餐到期</dt>
                      <dd>{date(l.expires_at)}</dd>
                    </div>
                    <div>
                      <dt>下次自然重置</dt>
                      <dd>{date(l.next_reset_at)}</dd>
                    </div>
                  </dl>
                  <Button onClick={() => open("lease", l)}>管理租约</Button>
                </div>
              ))
            ) : (
              <Empty title="该用户暂无套餐" />
            )}
          </div>
        </Modal>
      );
    }
    if (popup.kind === "2fa-start")
      return form(
        "启用二步验证",
        <>
          <p className="hint">输入当前密码，继续绑定验证器。</p>
          <Field
            label="当前密码"
            name="password"
            type="password"
            autoComplete="current-password"
            required
          />
        </>,
        async (e) => {
          const v = formdata(e);
          setBusy(true);
          setFormError("");
          try {
            const r = await api("2fa/setup", v);
            const qr = await QRCode.toDataURL(r.uri, {
              width: 220,
              margin: 1,
              errorCorrectionLevel: "M",
            });
            open("2fa-enable", { ...r, qr });
          } catch (e) {
            setFormError((e as Error).message);
          } finally {
            setBusy(false);
          }
        },
        "下一步",
      );
    if (popup.kind === "2fa-enable")
      return form(
        "绑定验证器",
        <>
          <p className="hint">用验证器扫码，或手动输入密钥。</p>
          <div className="totp-qr">
            <img
              src={item.qr}
              alt="二步验证设置二维码"
              width="220"
              height="220"
            />
          </div>
          <Field label="设置密钥" name="secret" readOnly value={item.secret} />
          <Field
            label="验证器中的六位动态码"
            name="code"
            inputMode="numeric"
            pattern="[0-9]{6}"
            maxLength={6}
            autoComplete="one-time-code"
            required
          />
        </>,
        async (e) => {
          const v = formdata(e);
          setBusy(true);
          setFormError("");
          try {
            const r = await api("2fa/enable", { code: v.code });
            await refresh();
            open("2fa-recovery", r);
          } catch (e) {
            setFormError((e as Error).message);
          } finally {
            setBusy(false);
          }
        },
        "验证并启用",
      );
    if (popup.kind === "2fa-recovery")
      return (
        <Modal title="保存恢复码" onClose={() => setPopup(null)}>
          <div className="modal-body">
            <p className="hint">
              二步验证已启用。请妥善保存恢复码：仅显示一次，每个限用一次。
            </p>
            <div className="recovery-codes">
              {item.recovery_codes.map((c: string) => (
                <code key={c}>{c}</code>
              ))}
            </div>
            <div className="card-actions">
              <Button
                onClick={() => {
                  const url = URL.createObjectURL(
                    new Blob([item.recovery_codes.join("\n")], {
                      type: "text/plain",
                    }),
                  );
                  const a = document.createElement("a");
                  a.href = url;
                  a.download = "vistart-recovery-codes.txt";
                  a.click();
                  setTimeout(() => URL.revokeObjectURL(url), 1000);
                }}
              >
                下载恢复码
              </Button>
              <Button primary onClick={() => setPopup(null)}>
                我已保存
              </Button>
            </div>
          </div>
        </Modal>
      );
    if (popup.kind === "2fa-disable")
      return form(
        "关闭二步验证",
        <>
          <Field
            label="当前密码"
            name="password"
            type="password"
            autoComplete="current-password"
            required
          />
          <Field
            label="二步验证码或恢复码"
            name="code"
            autoComplete="one-time-code"
            maxLength={32}
            required
          />
        </>,
        submit("2fa/disable", (v) => v, "二步验证已关闭"),
        "验证并关闭",
      );

    if (popup.kind === "ssh-key-confirm")
      return form("确认 SSH 主机变更", <>
        <p>此服务器的 SSH 主机指纹已改变。如果刚重装系统，请核对新指纹后继续。</p>
        <dl className="details">
          <div><dt>原指纹</dt><dd className="mono" style={{overflowWrap:"anywhere"}}>{item.key.previous}</dd></div>
          <div><dt>新指纹</dt><dd className="mono" style={{overflowWrap:"anywhere"}}>{item.key.fingerprint}</dd></div>
        </dl>
        <p className="hint">确认只对当前显示的指纹有效；连接过程中再次变化会停止认证。</p>
      </>, submit(item.route, () => ({...item.request, confirmed_fingerprint:item.key.fingerprint}),
        "连接已验证，操作已提交"), "确认重装并继续");
    if (popup.kind === "node" || popup.kind === "connection") {
      const connection = popup.kind === "connection";
      return form(connection ? "连接信息" : "添加转发服务器", <>
        <p className="hint">支持 Debian 和 Ubuntu。SSH 密码加密保存，用于后续部署与更新。</p>
        <div className="form-grid">
          {!connection && <>
            <Field label="节点名称" name="name" defaultValue={item.name} maxLength={40} required />
            <Field label="所在地区" name="region" maxLength={40} required />
          </>}
          <Field label="SSH 连接 IP" name="ssh_host" defaultValue={item.meta?.ssh_host} required />
          <Field label="SSH 端口" name="ssh_port" type="number" min="1" max="65535"
            defaultValue={item.meta?.ssh_port ?? 22} required />
          <Field label="SSH 用户名" name="username" defaultValue={item.meta?.username ?? "root"} required />
          <Field label="SSH 密码" name="password" type="password" autoComplete="new-password"
            required={!connection || !item.meta?.has_ssh_password}
            hint={connection && item.meta?.has_ssh_password ? "已加密保存；留空保留。更改地址或用户名时需重新填写。" : undefined} />
        </div>
        {!connection && <Field label="对外显示连接 IP" name="public_ip" required />}
        {connection && <label className="check-line">
          <input type="checkbox" name="redeploy" /><span>系统已重装，保存后重新部署 Agent</span>
        </label>}
      </>, (e) => {
        const form = e.currentTarget;
        const v = formdata(e);
        connectNode(connection ? "admin/connection" : "admin/deploy", {
          ...v, ssh_port:Number(v.ssh_port), node_id:connection ? item.id : "",
          redeploy:new FormData(form).has("redeploy"),
        });
      }, connection ? "验证并保存" : "连接并部署");
    }
    if (popup.kind === "node-detail" || popup.kind === "probe") {
      const n = nodes.find((n) => n.id === item.id) ?? item;
      return (
        <Modal title={n.name} onClose={() => setPopup(null)}>
          <div className="modal-body">
            <div className="detail-intro">
              <Tag good={n.online}>
                {n.online ? "在线" : n.enabled ? "离线" : "已停用"}
              </Tag>
              <span>
                {n.meta?.region} · {n.meta?.public_ip}
              </span>
            </div>
            <Probe node={n} />
            <dl className="details">
              <div>
                <dt>最近上报</dt>
                <dd>{date(n.last_seen)}</dd>
              </div>
              <div>
                <dt>Agent 版本</dt>
                <dd>{n.agent_version || "尚未上报"}</dd>
              </div>
              <div>
                <dt>转发内核版本</dt>
                <dd>{n.kernel_version || "尚未上报"}</dd>
              </div>
              <div>
                <dt>内存使用</dt>
                <dd>
                  {bytes(n.probe?.memory_used)} / {bytes(n.probe?.memory_total)}
                </dd>
              </div>
              {admin && (
                <>
                  <div>
                    <dt>系统 / 架构</dt>
                    <dd>
                      {n.meta?.os ?? "—"} / {n.meta?.arch ?? "—"}
                    </dd>
                  </div>
                  <div>
                    <dt>配置版本</dt>
                    <dd>{n.applied_revision ?? "—"}</dd>
                  </div>
                </>
              )}
            </dl>
            {admin &&
              (n.errors ?? []).map((e: Any) => (
                <p className="form-error" key={e.rule_id}>
                  {e.rule_id.slice(0, 8)}：{e.message}
                </p>
              ))}
            {admin && (
              <div className="card-actions">
                <Button onClick={() => open("node-edit", n)}>编辑资料</Button>
                <Button onClick={() => open("connection", n)}>连接信息</Button>
                <Button disabled={!n.update_available || !n.meta?.has_ssh_password || busy}
                  onClick={() => act("admin/update-agent", {node_id:n.id}, "更新任务已启动")}>更新</Button>
                <Button onClick={() => open("rule-migration", n)}>规则导入 / 导出</Button>
                <Button danger onClick={() => open("remove-node", n)}>
                  删除服务器
                </Button>
              </div>
            )}
          </div>
        </Modal>
      );
    }
    if (popup.kind === "rule-migration")
      return <Modal title={item.name + " · 规则导入 / 导出"} onClose={() => setPopup(null)} busy={busy}>
        <div className="modal-body">
          <p>导出当前服务器的转发规则，或将规则文件恢复到这台服务器。</p>
          <Button disabled={busy} onClick={async () => {
            setBusy(true); setFormError("");
            try {
              const result = await api("admin/export-rules", {node_id:item.id});
              const url = URL.createObjectURL(new Blob([JSON.stringify(result.file)],{type:"application/json"}));
              const a=document.createElement("a"); a.href=url;
              a.download="forward-rules-"+item.id.slice(0,8)+".json"; a.click();
              setTimeout(()=>URL.revokeObjectURL(url),1000);
              notice("规则文件已导出");
            } catch(e) {setFormError((e as Error).message);} finally {setBusy(false);}
          }}>导出规则</Button>
          <label className="field"><span>导入规则文件</span>
            <input type="file" accept=".json,application/json" disabled={busy || !item.online}
              onChange={async (e) => {
                const file=e.currentTarget.files?.[0]; if(!file)return;
                setBusy(true);setFormError("");
                try {
                  if(file.size>4*1024*1024)throw Error("规则文件过大");
                  const data=JSON.parse(await file.text());
                  const result=await api("admin/preview-rules",{node_id:item.id,file:data});
                  setPopup({kind:"rule-preview",item:{node:item,file:data,...result}});
                } catch(e) {setFormError((e as Error).message);} finally {setBusy(false);}
              }} />
          </label>
          <p className="hint">导入前先检查端口，不会立即变更规则。支持当前面板内迁移和同一服务器重装恢复。请先完成目标服务器部署并保存 SSH 连接信息。</p>
          {feedback}
        </div>
      </Modal>;
    if (popup.kind === "rule-preview")
      return form(item.preview.source_id === item.preview.node_id ? "确认恢复规则" : "确认迁移规则", <>
        <p>{item.preview.entries.length} 条规则将恢复到「{item.node.name}」。用户归属、转发目标、备注、已用流量、暂停状态和到期时间保留。</p>
        <p className="hint">同步 {item.preview.leases.length} 个用户套餐与 {item.preview.plans.length} 个套餐商品的节点绑定。跨服务器迁移会关闭旧入口，用户页面自动显示新入口。</p>
        <div className="table-wrap"><table><thead><tr><th>原入口</th><th>新入口</th><th>目标</th></tr></thead><tbody>
          {item.preview.entries.map((r:Any)=><tr key={r.old_id}>
            <td className="mono">{endpoint(r.old_ip,r.old_port)}</td>
            <td className="mono">{endpoint(r.public_ip,r.port)}{r.old_port!==r.port && <small>原端口不可用，已选择池内空闲端口</small>}</td>
            <td className="mono">{r.target_host ? endpoint(r.target_host,r.target_port) : "待配置"}</td>
          </tr>)}
        </tbody></table></div>
        <p className="hint">预览有效期 5 分钟。端口占用或原规则发生变化时，会要求重新预览。</p>
      </>, async(e)=>{
        e.preventDefault();
        try {
          const result=await run("admin/import-rules",{node_id:item.node.id,file:item.file,preview_token:item.preview_token},
            "规则已恢复，正在等待节点确认监听");
          setPopup({kind:"rule-result",item:result});
        }catch{}
      }, "确认导入");
    if (popup.kind === "rule-result")
      return <Modal title="规则已导入" onClose={()=>setPopup(null)}>
        <div className="modal-body">
          <p>已处理 {item.entries.length} 条规则。请在“全部转发”查看节点监听与目标连通状态。</p>
          <Button primary onClick={()=>go("/admin/forwards")}>查看转发状态</Button>
        </div>
      </Modal>;
    if (popup.kind === "remove-node")
      return form(
        "删除转发服务器",
        <>
          <p>
            删除「{item.name}」将释放该节点的{" "}
            {ports.filter((a) => a.node_id === item.id).length}{" "}
            个端口，并移除套餐中的节点关联。历史订单与已用流量保留。
          </p>
          <p className="hint">
            {item.online
              ? "在线节点会先停止转发、上报剩余流量，再撤销连接配置。"
              : "节点离线，将撤销其凭据并移除面板记录；无法清除离线服务器上的文件。"}
          </p>
          <Field
            label="输入服务器名称确认"
            name="confirm_name"
            required
            autoComplete="off"
          />
          <Field
            label="当前账户密码"
            name="password"
            type="password"
            autoComplete="current-password"
            required
          />
          {user!.two_factor && (
            <Field
              label="二步验证码或恢复码"
              name="code"
              maxLength={32}
              required
            />
          )}
        </>,
        submit(
          "admin/remove-node",
          (v) => ({ ...v, id: item.id }),
          "删除请求已提交，节点状态会自动更新",
        ),
        "删除服务器",
      );
    if (popup.kind === "admin-release")
      return form(
        "释放用户端口",
        <p>
          确认释放 {endpoint(item.public_ip, item.port)}？该端口的 TCP 与 UDP
          转发将同时停止。
        </p>,
        submit("admin/release", () => ({ id: item.id }), "端口已释放"),
        "确认释放",
      );
    if (popup.kind === "node-edit")
      return form(
        "编辑节点",
        <>
          <Field
            label="节点名称"
            name="name"
            defaultValue={item.name}
            required
          />
          <Field
            label="所在地区"
            name="region"
            defaultValue={item.meta?.region}
            required
          />
          <Field label="对外显示连接 IP" name="public_ip"
            defaultValue={item.meta?.public_ip} required
            hint="保存后同步端口池与现有转发的连接地址；可填写互联或 NAT 入口 IP。"
          />
          <label className="check-line">
            <input
              type="checkbox"
              name="enabled"
              defaultChecked={item.enabled}
            />
            <span>启用节点（关闭后停止该节点全部转发）</span>
          </label>
        </>,
        submit("admin/node", (v, f) => ({
          ...v,
          id: item.id,
          enabled: new FormData(f).has("enabled"),
        })),
      );
    if (popup.kind === "pool")
      return (
        <PoolForm
          nodes={nodes}
          busy={busy}
          error={formError}
          onClose={() => setPopup(null)}
          onSubmit={(v) => run("admin/pool", v, "端口池已添加").catch(() => {})}
        />
      );
    if (popup.kind === "delete-plan")
      return form("删除套餐商品", <>
        <p>删除「{item.name}」后不再出售，已购买用户的套餐与转发继续保留。</p>
        <Field label="输入套餐名称确认" name="confirm_name" required autoComplete="off" />
      </>, submit("admin/delete-plan", (v) => ({ ...v, id: item.id }), "套餐已删除"), "删除套餐");
    if (popup.kind === "delete-lease")
      return form("删除用户套餐", <>
        <p>删除「{item.plan_name}」并释放其全部端口，关联转发将停止。历史订单与已结算流量保留。</p>
        {admin && <Field label="输入套餐名称确认" name="confirm_name" required autoComplete="off" />}
      </>, submit(admin ? "admin/delete-lease" : "delete-lease",
        (v) => ({ ...v, id: item.id }), "套餐已删除，端口已释放"), "确认删除");
    if (popup.kind === "plan-preview") {
      const p=item.preview, request=item.request;
      const value=(key:string,v:any):string=>key==="node_ids"?(v??[]).map(nodeName).join("、"):
        key==="traffic_limit_bytes"?(v?bytes(v):"不限"):
        key==="bandwidth_mbps"?(v?v+" Mbps":"不限"):
        key==="period_days"?v+" 天":(["tcp_limit","udp_limit"].includes(key)?(v?String(v):"不限"):String(v??0));
      const labels:Any={plan_name:"套餐名称",port_limit:"端口数",traffic_limit_bytes:"周期流量",
        period_days:"后续周期",node_ids:"可用节点",bandwidth_mbps:"总带宽",tcp_limit:"TCP 连接",udp_limit:"UDP 会话"};
      return <Modal title="确认套餐更新影响" onClose={()=>setPopup(null)} busy={busy}>
        <form key={item.preview_token} className="modal-body impact-preview" onSubmit={e=>{
          e.preventDefault();run("admin/plan",{...request,preview_token:item.preview_token},"套餐及已有租约已更新").catch(()=>{});
        }}>
          <p data-autofocus tabIndex={-1}>将「{p.plan.name}」同步给 {p.users_count} 位用户的 {p.leases_count} 份套餐。</p>
          <div className="impact-totals">
            <span>释放转发 <strong>{p.release_count}</strong></span>
            <span>流量暂停 <strong>{p.pause_count}</strong></span>
            <span>恢复使用 <strong>{p.resume_count}</strong></span>
          </div>
          <p className="hint">已用流量、到期日与自然重置日保留。降低连接数后，超额的新连接会等待或被拒绝，已有连接继续至关闭。</p>
          {!!p.upgrade_nodes.length && <p className="form-error">
            {p.upgrade_nodes.map((n:Any)=>n.name).join("、")} 需要升级 Agent 后才能执行套餐限额；升级前，带限额的规则会停止转发。
          </p>}
          <div className="impact-leases">
            {p.leases.map((l:Any)=><details key={l.id} open={l.release_rules.length>0||l.will_pause||l.will_resume}>
              <summary><strong>{l.user_name} · {l.username || l.user_id.slice(0,8)}</strong>
                <span>套餐 {l.id.slice(0,8)}{l.release_rules.length>0&&" · 释放 "+l.release_rules.length+" 条"}
                  {l.will_pause&&" · 流量暂停"}{l.will_resume&&" · 恢复使用"}</span></summary>
              <dl className="details">
                {Object.keys(labels).filter(k=>JSON.stringify(l.before[k])!==JSON.stringify(l.after[k])).map(k=>
                  <div key={k}><dt>{labels[k]}</dt><dd>{value(k,l.before[k])} → {value(k,l.after[k])}</dd></div>)}
                <div><dt>已用流量</dt><dd>{bytes(l.used_bytes)}</dd></div>
              </dl>
              {l.release_rules.length>0&&<div className="table-scroll"><table>
                <thead><tr><th>将释放的入口</th><th>转发目标</th><th>原因</th></tr></thead>
                <tbody>{l.release_rules.map((r:Any)=><tr key={r.id}><td>{nodeName(r.node_id)}<small>{endpoint(r.public_ip,r.port)}</small></td>
                  <td className="mono">{r.target_host?endpoint(r.target_host,r.target_port):"未配置"}</td>
                  <td>{r.reason==="node_removed"?"节点被移除":"超出端口额度"}</td></tr>)}</tbody>
              </table></div>}
            </details>)}
          </div>
          {!p.leases_count&&<p className="hint">当前没有可同步的已购套餐，仅更新套餐商品。</p>}
          <label className="check-line"><input type="checkbox" required name="reviewed" /><span>我已核对以上变更和将释放的转发</span></label>
          {feedback}
          <div className="form-footer">
            <Button type="button" disabled={busy} onClick={()=>open("plan",request)}>返回修改</Button>
            <Button type="button" disabled={busy} onClick={async()=>{
              setBusy(true);setFormError("");
              try {const r=await api("admin/preview-plan",request);open("plan-preview",{...r,request});}
              catch(e){setFormError((e as Error).message);}finally{setBusy(false);}
            }}>重新预览</Button>
            <Button primary danger={p.release_count>0||p.pause_count>0} type="submit" disabled={busy}>确认更新</Button>
          </div>
        </form>
      </Modal>;
    }
    if (popup.kind === "plan")
      return form(
        item.id ? "编辑套餐" : "添加套餐",
        <>
          <Field
            label="套餐名称"
            name="name"
            defaultValue={item.name}
            required
            maxLength={40}
          />
          <div className="form-grid">
            <Field
              label="端口总额度"
              name="port_limit"
              type="number"
              min="1"
              max="500"
              defaultValue={item.port_limit ?? 6}
              required
            />
            <Field
              label="有效周期（天）"
              name="period_days"
              type="number"
              min="1"
              max="366"
              defaultValue={item.period_days ?? 30}
              required
            />
            <Field
              label="套餐价格（元）"
              name="price"
              type="number"
              min="0"
              max="99999.99"
              step=".01"
              defaultValue={(item.price_cents ?? 1990) / 100}
              required
            />
            <Field
              label="重置流量价格（元）"
              name="reset_price"
              type="number"
              min="0"
              max="99999.99"
              step=".01"
              defaultValue={(item.reset_price_cents ?? 500) / 100}
              required
            />
          </div>
          <Field
            label="周期流量（GB）"
            name="traffic"
            type="number"
            min="0"
            max="1000000"
            step=".001"
            defaultValue={(item.traffic_limit_bytes ?? 100e9) / 1e9}
            required
            hint="上行 + 下行累计；填 0 表示不限量。"
          />
          <div className="limit-fields">
            <Field label="总带宽（Mbps）" name="bandwidth_mbps" type="number" min="0" max="1000000" step="1" defaultValue={item.bandwidth_mbps ?? 0} required />
            <Field label="TCP 连接上限" name="tcp_limit" type="number" min="0" max="65536" step="1" defaultValue={item.tcp_limit ?? 0} required />
            <Field label="UDP 会话上限" name="udp_limit" type="number" min="0" max="16384" step="1" defaultValue={item.udp_limit ?? 0} required />
          </div>
          <p className="hint">填 0 表示不限。同一份已购套餐的所有节点、端口合计；带宽为上行 + 下行。限额需要 Agent 0.3.2 及以上版本，仍受节点自身安全容量约束。</p>
          <div className="field">
            <span>绑定转发服务器</span>
            <div className="checkbox-grid">
              {nodes.map((n) => (
                <label className="check-line" key={n.id}>
                  <input
                    type="checkbox"
                    name="node_ids"
                    value={n.id}
                    defaultChecked={item.node_ids?.includes(n.id)}
                  />
                  <span>{n.name}</span>
                </label>
              ))}
            </div>
            {!nodes.length && <small>请先添加转发服务器。</small>}
          </div>
          <label className="check-line">
            <input
              type="checkbox"
              name="enabled"
              defaultChecked={item.enabled ?? true}
            />
            <span>上架此套餐</span>
          </label>
          {item.id && <label className="check-line">
            <input type="checkbox" name="update_existing" defaultChecked={item.update_existing ?? false} />
            <span>强制更新已有用户套餐</span>
          </label>}
          <p className="hint">
            默认保留已有套餐额度。勾选后先预览影响，再同步名称、额度、限额、节点和后续周期，
            保留到期时间、下次重置日与已用流量。
            移除节点及超出新额度的转发会被释放，优先保留较小端口。
          </p>
        </>,
        async e => {
          const f=e.currentTarget,v=formdata(e),fields=new FormData(f);
          const request={id:item.id??"",name:v.name,port_limit:Number(v.port_limit),
            period_days:Number(v.period_days),price_cents:Math.round(Number(v.price)*100),
            reset_price_cents:Math.round(Number(v.reset_price)*100),traffic_limit_bytes:Math.round(Number(v.traffic)*1e9),
            bandwidth_mbps:Number(v.bandwidth_mbps),tcp_limit:Number(v.tcp_limit),udp_limit:Number(v.udp_limit),
            node_ids:fields.getAll("node_ids"),enabled:fields.has("enabled"),update_existing:fields.has("update_existing")};
          if(!request.update_existing){await run("admin/plan",request).catch(()=>{});return;}
          setBusy(true);setFormError("");
          try {const r=await api("admin/preview-plan",request);open("plan-preview",{...r,request});}
          catch(e){setFormError((e as Error).message);}finally{setBusy(false);}
        },
      );
    if (popup.kind === "claim")
      return (
        <ClaimForm
          data={data}
          userId={user!.id}
          error={formError}
          busy={busy}
          onClose={() => setPopup(null)}
          onSubmit={(v) =>
            run("claim", v, "转发已创建，配置正在下发").catch(() => {})
          }
        />
      );
    if (popup.kind === "target")
      return form(
        "目标配置",
        <>
          <div className="target-summary">
            <Tag>TCP + UDP</Tag>
            <strong>{endpoint(item.public_ip, item.port)}</strong>
          </div>
          <TargetFields item={item} />
        </>,
        submit(
          "target",
          (v, f) => ({
            ...v,
            id: item.id,
            target_port: Number(v.target_port),
            load_balance: new FormData(f).has("load_balance"),
            targets: v.targets ? JSON.parse(v.targets) : [],
          }),
          "目标配置已保存",
        ),
        "保存并下发",
      );
    if (popup.kind === "release")
      return form(
        "释放端口",
        <p>
          释放 {endpoint(item.public_ip, item.port)} 后将停止 TCP 与 UDP
          转发，端口回到资源池，套餐恢复一个可用端口额度。
        </p>,
        submit("release", () => ({ id: item.id }), "端口已释放"),
        "确认释放",
      );
    if (popup.kind === "remove-pool")
      return form(
        "删除端口池",
        <p>
          删除 {nodeName(item.node_id)} 的 {item.start}–{item.end}{" "}
          端口池。仍有端口被使用时无法删除。
        </p>,
        submit("admin/pool", () => ({ ...item, remove: true }), "端口池已删除"),
        "删除",
      );
    if (popup.kind === "grant")
      return form(
        "开通用户套餐",
        <>
          <Select label="用户" name="user_id" required>
            <option value="">选择用户</option>
            {accounts
              .filter((a) => !a.disabled)
              .map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name} · {a.username}
                </option>
              ))}
          </Select>
          <Select label="套餐" name="plan_id" required>
            <option value="">选择套餐</option>
            {plans
              .filter((p) => p.enabled)
              .map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
          </Select>
          <p className="hint">
            直接开通套餐额度，不占用端口；有效期从现在开始计算。
          </p>
        </>,
        submit("admin/grant"),
        "开通套餐",
      );
    if (popup.kind === "lease")
      return form(
        "管理用户套餐",
        <>
          <p>
            {accountName(item.user_id)} · {item.plan_name}
          </p>
          <Select label="操作" name="action">
            <option value={item.manual_paused ? "resume" : "pause"}>
              {item.manual_paused ? "恢复使用" : "暂停全部节点转发"}
            </option>
            <option value="expiry">调整到期时间</option>
            <option value="end">结束套餐并回收端口</option>
          </Select>
          <Field
            label="新的到期时间（选择调整到期时生效）"
            name="date"
            type="datetime-local"
            defaultValue={new Date(
              item.expires_at * 1000 - new Date().getTimezoneOffset() * 60000,
            )
              .toISOString()
              .slice(0, 16)}
          />
          <p className="hint">
            到期时间调整不会清除已用流量或改变自然重置日。结束套餐会关闭其全部转发。
          </p>
        </>,
        submit("admin/lease", (v) => ({
          id: item.id,
          action: v.action,
          ...(v.action === "expiry"
            ? { expires: Math.floor(new Date(v.date).getTime() / 1000) }
            : {}),
        })),
      );
    if (popup.kind === "user")
      return form(
        item.id ? "管理用户" : "创建用户",
        <>
          <Field
            label="显示名称"
            name="name"
            defaultValue={item.name}
            required
            maxLength={40}
          />
          <Field
            label="用户名或邮箱"
            name="username"
            defaultValue={item.username}
            readOnly={!!item.id}
            required
            minLength={3}
            maxLength={150}
          />
          <Field
            label={item.id ? "重置密码（不修改请留空）" : "初始密码"}
            name="password"
            type="password"
            autoComplete="new-password"
            minLength={12}
            maxLength={128}
            required={!item.id}
          />
          <Select
            label="账户权限"
            name="role"
            defaultValue={item.role ?? "user"}
          >
            <option value="user">普通用户</option>
            <option value="admin">管理员</option>
          </Select>
          {item.id && (
            <label className="check-line">
              <input
                type="checkbox"
                name="disabled"
                defaultChecked={item.disabled}
              />
              <span>停用此账户</span>
            </label>
          )}
        </>,
        submit("admin/user", (v, f) => ({
          ...v,
          id: item.id ?? "",
          disabled: new FormData(f).has("disabled"),
        })),
      );
    if (popup.kind === "reset")
      return form(
        "重置已用流量",
        <>
          <p>
            为「{item.plan_name}」购买一次流量重置，价格{" "}
            <strong>{money(item.plan.reset_price_cents)}</strong>。
          </p>
          <p className="hint">付款后清零已用流量，到期日和周期重置日不变。</p>
        </>,
        (e) => {
          e.preventDefault();
          newOrder(item.plan, item, "reset");
        },
        "创建重置订单",
      );
    if (popup.kind === "renew-select")
      return form(
        "选择要续费的套餐",
        <Select label="已有套餐" name="lease_id">
          {item.leases.map((l: Any) => (
            <option key={l.id} value={l.id}>
              {l.plan_name} · 到期 {date(l.expires_at)} · {l.id.slice(0, 8)}
            </option>
          ))}
        </Select>,
        (e) => {
          const v = formdata(e);
          newOrder(
            item.plan,
            item.leases.find((l: Any) => l.id === v.lease_id),
          );
        },
        "创建续费订单",
      );
    if (popup.kind === "cancel-order")
      return form(
        "取消订单",
        <p>
          取消「{item.snapshot.name}」的待付款订单。已完成的付款不会被取消。
        </p>,
        submit("cancel-order", () => ({ id: item.id }), "订单已取消"),
        "取消订单",
      );
    if (popup.kind === "checkout")
      return form(
        "订单付款",
        <>
          <div className="checkout-amount">
            <span>
              {kindLabel[item.kind]} · {item.snapshot.name}
            </span>
            <strong>{money(item.amount_cents)}</strong>
          </div>
          <p className="hint">
            {item.kind === "renew"
              ? "续费延长有效期，已用流量不变。"
              : item.kind === "reset"
                ? "重置已用流量，到期日不变。"
                : "开通后即可添加转发。"}
          </p>
          {item.amount_cents > 0 ? (
            <>
              <span className="section-label">选择付款方式</span>
              <div className="payment-options">
                {(data.payment.methods ?? [])
                  .filter((m: Any) => m.enabled)
                  .map((m: Any, i: number) => (
                    <label key={m.id}>
                      <input
                        type="radio"
                        name="method"
                        value={m.id}
                        defaultChecked={i === 0}
                        required
                      />
                      <span>{m.name}</span>
                    </label>
                  ))}
              </div>
              {!data.payment.enabled && (
                <p className="form-error">管理员尚未启用支付。</p>
              )}
            </>
          ) : (
            <p className="hint">无需付款，确认即可开通。</p>
          )}
        </>,
        async (e) => {
          const v = formdata(e);
          setBusy(true);
          setFormError("");
          try {
            const r = await api("checkout", {
              id: item.id,
              method: v.method ?? "",
            });
            if (r.free) {
              await refresh();
              setPopup(null);
              notice("套餐已生效");
            } else {
              const f = document.createElement("form");
              f.method = "POST";
              f.action = r.action;
              for (const [k, vs] of Object.entries(r.fields)) {
                for (const value of vs as string[]) {
                  const input = document.createElement("input");
                  input.type = "hidden";
                  input.name = k;
                  input.value = value;
                  f.appendChild(input);
                }
              }
              document.body.appendChild(f);
              f.submit();
            }
          } catch (e) {
            setFormError((e as Error).message);
          } finally {
            setBusy(false);
          }
        },
        item.amount_cents ? "前往支付" : "确认开通",
      );
    if (popup.kind === "order") {
      const o = orders.find((o) => o.id === item.id) ?? item;
      return (
        <Modal title="订单详情" onClose={() => setPopup(null)} busy={busy}>
          <div className="modal-body">
            <div className="checkout-amount">
              <span>{o.snapshot.name}</span>
              <strong>{money(o.amount_cents)}</strong>
            </div>
            <dl className="details">
              <div>
                <dt>订单编号</dt>
                <dd className="mono">{o.id}</dd>
              </div>
              {admin && (
                <div>
                  <dt>所属用户</dt>
                  <dd>{accountName(o.user_id)}</dd>
                </div>
              )}
              <div>
                <dt>订单类型</dt>
                <dd>{kindLabel[o.kind]}</dd>
              </div>
              <div>
                <dt>状态</dt>
                <dd>{orderLabel[o.status]}</dd>
              </div>
              <div>
                <dt>创建时间</dt>
                <dd>{date(o.created_at)}</dd>
              </div>
              <div>
                <dt>付款时间</dt>
                <dd>{date(o.paid_at)}</dd>
              </div>
            </dl>
            {o.status === "paid_review" && (
              <p className="form-error">
                付款已收到，但套餐状态发生变化，需要管理员核实处理。
              </p>
            )}
            {admin && (
              <>
                {["pending", "cancelled"].includes(o.status) ? (
                  <div className="card-actions">
                    <Button
                      primary
                      onClick={() =>
                        open("admin-order", { ...o, nextStatus: "paid" })
                      }
                    >
                      标记已支付
                    </Button>
                    {o.status === "pending" && (
                      <Button
                        danger
                        onClick={() =>
                          open("admin-order", { ...o, nextStatus: "cancelled" })
                        }
                      >
                        取消订单
                      </Button>
                    )}
                  </div>
                ) : (
                  <p className="hint">已收款订单不能取消。</p>
                )}
              </>
            )}
          </div>
        </Modal>
      );
    }
    if (popup.kind === "admin-order" && admin) {
      const paid = item.nextStatus === "paid";
      return (
        <Modal
          title={paid ? "确认订单收款" : "取消订单"}
          onClose={() => setPopup(null)}
          busy={busy}
        >
          <form
            onSubmit={async (e) => {
              const v = formdata(e);
              setBusy(true);
              setFormError("");
              try {
                const r = await api("admin/order-status", {
                  id: item.id,
                  status: item.nextStatus,
                  note: v.note,
                });
                await refresh();
                setData((d) =>
                  d
                    ? {
                        ...d,
                        orders: d.orders.map((o) =>
                          o.id === r.order.id ? r.order : o,
                        ),
                      }
                    : d,
                );
                open("order", r.order);
                notice(
                  r.order.status === "paid_review"
                    ? "已记录收款，套餐需要核实处理"
                    : paid
                      ? "订单已标记为已支付"
                      : "订单已取消",
                );
              } catch (e) {
                setFormError((e as Error).message);
              } finally {
                setBusy(false);
              }
            }}
          >
            <div className="modal-body">
              <div className="checkout-amount">
                <span>
                  {item.snapshot.name} · {kindLabel[item.kind]}
                </span>
                <strong>{money(item.amount_cents)}</strong>
              </div>
              <p className="mono">{item.id}</p>
              <p className="hint">
                {paid
                  ? "请确认已收到款项。提交后将" +
                    (item.kind === "reset"
                      ? "重置已用流量，到期日不变。"
                      : item.kind === "renew"
                        ? "延长套餐有效期，已用流量不变。"
                        : "开通对应套餐。")
                  : "取消后无法继续付款，已有套餐不受影响。"}
              </p>
              <Field
                label="操作备注"
                name="note"
                maxLength={200}
                required
                placeholder={paid ? "例如：已核实线下收款" : "填写取消原因"}
              />
              {feedback}
            </div>
            <div className="form-footer">
              <Button
                type="button"
                disabled={busy}
                onClick={() => open("order", item)}
              >
                返回详情
              </Button>
              <Button primary={paid} danger={!paid} disabled={busy}>
                {busy ? "正在处理…" : paid ? "确认已收款" : "确认取消"}
              </Button>
            </div>
          </form>
        </Modal>
      );
    }
    return null;
  }
  const sidebar = (
    <>
      <Logo name={boot.site_name} />
      <div className="workspace">
        <span className="small-icon">
          {admin ? <ShieldCheck size={16} /> : <Globe2 size={16} />}
        </span>
        <span>{admin ? "管理员" : "个人服务"}</span>
      </div>
      <div className="nav-caption">{admin ? "管理控制台" : "服务控制台"}</div>
      <nav aria-label={admin ? "管理端导航" : "用户端导航"}>
        {nav.map(([url, label, Icon]) => (
          <a
            key={url}
            href={url}
            aria-current={path === url ? "page" : undefined}
            className={"nav-item " + (path === url ? "active" : "")}
            onClick={(e) => {
              if (!e.ctrlKey && !e.metaKey) {
                e.preventDefault();
                go(url);
              }
            }}
          >
            <Icon size={18} strokeWidth={1.7} />
            {label}
            {path === url && <i />}
          </a>
        ))}
      </nav>
      <div className="sidebar-bottom">
        {user.role === "admin" && (
          <button
            className="switch-view"
            onClick={() => go(admin ? "/app" : "/admin")}
          >
            <ArrowLeftRight size={16} />
            {admin ? "切换用户端" : "切换管理员"}
            <ChevronRight size={15} />
          </button>
        )}
        <div className="sidebar-account">
          <button onClick={() => go("/account")}>
            <span className="avatar">{user.name.slice(0, 1)}</span>
            <span>
              <strong>{user.name}</strong>
              <small>{admin ? "管理员" : "个人账户"}</small>
            </span>
          </button>
          <button
            className="icon-button"
            aria-label="退出登录"
            onClick={async () => {
              try {
                await api("logout", {});
                setBoot((b) => (b ? { ...b, user: null } : b));
                setData(null);
                go("/login");
              } catch (e) {
                notice((e as Error).message);
              }
            }}
          >
            <LogOut size={17} />
          </button>
        </div>
      </div>
    </>
  );
  return (
    <>
      <div className="app-layout">
        <a className="skip-link" href="#main">
          跳到主要内容
        </a>
        <aside className="glass sidebar">{sidebar}</aside>
        {mobile && (
          <div className="mobile-nav-backdrop" onClick={() => setMobile(false)}>
            <aside
              className="glass sidebar mobile"
              onClick={(e) => e.stopPropagation()}
            >
              {sidebar}
            </aside>
          </div>
        )}
        <div className="main-shell">
          <header className="glass topbar">
            <div>
              <button
                className="icon-button mobile-menu"
                aria-label="打开导航"
                onClick={() => setMobile(true)}
              >
                <Menu size={20} />
              </button>
              <span className="muted">{admin ? "管理端" : "用户端"}</span>
              <ChevronRight size={14} />
              <strong>
                {nav.find((n) => n[0] === path)?.[1] ?? "账户设置"}
              </strong>
            </div>
            <div>
              <span className="connection-status">
                <i className={error ? "offline" : ""} />
                {error ? "连接中断" : "主控已连接"}
              </span>
              <button
                className="icon-button"
                aria-label="切换主题"
                onClick={() => setDark(!dark)}
              >
                {dark ? <Sun size={18} /> : <Moon size={18} />}
              </button>
              <button
                className="avatar"
                aria-label="账户设置"
                onClick={() => go("/account")}
              >
                {user.name.slice(0, 1)}
              </button>
            </div>
          </header>
          <main id="main" tabIndex={-1}>
            {error && data && (
              <p className="form-error" role="alert">
                {error}，正在尝试重新连接。
              </p>
            )}
            {page}
          </main>
          <footer className="app-footer">
            <span>
              {boot.site_name} <i /> 转发管理
            </span>
            <span>TCP + UDP</span>
          </footer>
        </div>
      </div>
      {modalContent()}
      {toast && (
        <div role="status" className="toast">
          <Check size={17} />
          {toast}
          <button aria-label="关闭提示" onClick={() => setToast("")}>
            <X size={16} />
          </button>
        </div>
      )}
    </>
  );
}
function PoolForm({
  nodes,
  busy,
  error,
  onClose,
  onSubmit,
}: {
  nodes: Any[];
  busy: boolean;
  error: string;
  onClose: () => void;
  onSubmit: (v: Any) => void;
}) {
  const [id, setId] = useState(
      nodes.find((n) => n.enabled && n.meta?.bind_ip)?.id ?? "",
    ),
    n = nodes.find((n) => n.id === id);
  return (
    <Modal title="添加端口池" onClose={onClose} busy={busy}>
      <form
        onSubmit={(e) => {
          const v = formdata(e);
          onSubmit({
            ...v,
            node_id: id,
            public_ip: n?.meta?.public_ip,
            start: Number(v.start),
            end: Number(v.end),
          });
        }}
      >
        <div className="modal-body">
          <Select
            label="转发服务器"
            name="node_id"
            value={id}
            onChange={(e) => setId(e.target.value)}
            required
          >
            <option value="">选择已部署的节点</option>
            {nodes
              .filter((n) => n.enabled && n.meta?.bind_ip)
              .map((n) => (
                <option value={n.id} key={n.id}>
                  {n.name}
                </option>
              ))}
          </Select>
          <Field
            label="对外连接 IP"
            name="public_ip"
            readOnly
            value={n?.meta?.public_ip ?? ""}
          />
          <div className="form-grid">
            <Field
              label="开始端口"
              name="start"
              type="number"
              min="1024"
              max="65535"
              required
            />
            <Field
              label="结束端口"
              name="end"
              type="number"
              min="1024"
              max="65535"
              required
            />
          </div>
          <p className="hint">
            每段最多 10,000
            个端口。系统自动使用部署时检测到的网卡地址监听；不提前分配给用户。
          </p>
          {error && (
            <p className="form-error" role="alert">
              {error}
            </p>
          )}
        </div>
        <div className="form-footer">
          <Button type="button" onClick={onClose}>
            取消
          </Button>
          <Button primary disabled={busy}>
            {busy ? "正在保存…" : "添加端口池"}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
function ClaimForm({
  data,
  userId,
  error,
  busy,
  onClose,
  onSubmit,
}: {
  data: Snapshot;
  userId: string;
  error: string;
  busy: boolean;
  onClose: () => void;
  onSubmit: (v: Any) => void;
}) {
  const leases = data.leases.filter(
      (l) =>
        l.user_id === userId &&
        leaseState(l) === "使用中" &&
        l.used_ports < l.port_limit,
    ),
    [leaseId, setLease] = useState(leases[0]?.id ?? ""),
    lease = leases.find((l) => l.id === leaseId),
    nodes = data.nodes.filter(
      (n) =>
        n.enabled &&
        lease?.node_ids.includes(n.id) &&
        data.pools.some((p) => p.node_id === n.id),
    ),
    [nodeId, setNode] = useState(nodes[0]?.id ?? ""),
    [method, setMethod] = useState("random");
  const actual = nodes.some((n) => n.id === nodeId)
      ? nodeId
      : (nodes[0]?.id ?? ""),
    pools = data.pools.filter((p) => p.node_id === actual),
    ips = [...new Set(pools.map((p) => p.public_ip))],
    [ip, setIP] = useState(ips[0] ?? ""),
    actualIP = ips.includes(ip) ? ip : (ips[0] ?? "");
  return (
    <Modal title="添加转发" onClose={onClose} busy={busy}>
      <form
        onSubmit={(e) => {
          const v = formdata(e);
          onSubmit({
            lease_id: leaseId,
            node_id: actual,
            public_ip: actualIP,
            port: method === "random" ? 0 : Number(v.port),
            target_host: v.target_host,
            target_port: Number(v.target_port),
            remark: v.remark ?? "",
            load_balance: v.load_balance === "on",
            targets: v.targets ? JSON.parse(v.targets) : [],
          });
        }}
      >
        <div className="modal-body">
          {!leases.length ? (
            <Empty title="暂无可用套餐额度" detail="请购买套餐或释放端口。" />
          ) : (
            <>
              <Select
                label="使用套餐"
                name="lease_id"
                value={leaseId}
                onChange={(e) => setLease(e.target.value)}
                required
              >
                {leases.map((l) => (
                  <option key={l.id} value={l.id}>
                    {l.plan_name} · 剩余 {l.port_limit - l.used_ports} /{" "}
                    {l.port_limit}
                  </option>
                ))}
              </Select>
              <Select
                label="转发服务器"
                name="node_id"
                value={actual}
                onChange={(e) => setNode(e.target.value)}
                required
              >
                <option value="">选择转发节点</option>
                {nodes.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                    {n.online ? "" : "（离线）"}
                  </option>
                ))}
              </Select>
              <Select
                label="入口 IP"
                name="public_ip"
                value={actualIP}
                onChange={(e) => setIP(e.target.value)}
                required
              >
                {ips.map((ip) => (
                  <option value={ip} key={ip}>
                    {ip}
                  </option>
                ))}
              </Select>
              <div className="segmented">
                <label>
                  <input
                    type="radio"
                    name="allocation"
                    value="random"
                    checked={method === "random"}
                    onChange={() => setMethod("random")}
                  />
                  系统随机分配
                </label>
                <label>
                  <input
                    type="radio"
                    name="allocation"
                    value="custom"
                    checked={method === "custom"}
                    onChange={() => setMethod("custom")}
                  />
                  自定义端口
                </label>
              </div>
              {method === "custom" && (
                <Field
                  label="自定义端口"
                  name="port"
                  type="number"
                  min="1024"
                  max="65535"
                  required
                />
              )}
              <p className="hint">
                允许范围：
                {pools
                  .filter((p) => p.public_ip === actualIP)
                  .map((p) => p.start + "–" + p.end)
                  .join("、") || "暂无端口池"}
              </p>
              <TargetFields />
              <p className="hint">TCP + UDP 共用，占用 1 个端口额度。</p>
            </>
          )}
          {error && (
            <p className="form-error" role="alert">
              {error}
            </p>
          )}
        </div>
        <div className="form-footer">
          <Button type="button" onClick={onClose}>
            取消
          </Button>
          <Button primary disabled={busy || !leases.length || !actualIP}>
            {busy ? "正在创建…" : "创建转发"}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
function PaymentForm({
  payment: p,
  busy,
  onSave,
}: {
  payment: Any;
  busy: boolean;
  onSave: (v: Any) => void;
}) {
  const [methods, setMethods] = useState<Any[]>(
    (p.methods ?? []).map((m: Any) => ({ ...m })),
  );
  function update(i: number, k: string, v: any) {
    setMethods((ms) => ms.map((m, j) => (j === i ? { ...m, [k]: v } : m)));
  }
  return (
    <Box title="易支付">
      <form
        className="payment-form"
        onSubmit={(e) => {
          const f = e.currentTarget,
            v = formdata(e),
            fd = new FormData(f);
          onSave({
            ...v,
            enabled: fd.has("enabled"),
            exclude_zero: fd.has("exclude_zero"),
            methods,
          });
        }}
      >
        <div className="settings-grid">
          <div>
            <Field
              label="支付网关"
              name="gateway"
              type="url"
              defaultValue={p.gateway}
              placeholder="https://pay.example.com"
            />
            <Field
              label="商户 ID"
              name="merchant_id"
              defaultValue={p.merchant_id}
            />
            <Field
              label="商户密钥"
              name="secret"
              type="password"
              autoComplete="new-password"
              placeholder={
                p.has_secret ? "已配置，留空保持原密钥" : "填写商户密钥"
              }
            />
            <label className="check-line">
              <input
                type="checkbox"
                name="enabled"
                defaultChecked={p.enabled}
              />
              <span>启用在线支付</span>
            </label>
            <label className="check-line">
              <input
                type="checkbox"
                name="exclude_zero"
                defaultChecked={p.exclude_zero ?? true}
              />
              <span>签名排除值为 0 的字段（当前支付商户要求）</span>
            </label>
            <p className="hint">
              使用易支付 V1 / MD5 签名。支付成功由服务端验签回调处理。
            </p>
          </div>
          <div>
            <h3>用户可选付款方式</h3>
            <p className="hint">
              显示名称可自定义；接口类型需与支付商户支持的类型一致。
            </p>
            {methods.map((m, i) => (
              <div className="method-row" key={m.id}>
                <Field
                  label={"支付名称 " + (i + 1)}
                  name={"method_name_" + i}
                  value={m.name}
                  onChange={(e) => update(i, "name", e.target.value)}
                  required
                />
                <Field
                  label={"接口类型 " + (i + 1)}
                  name={"method_type_" + i}
                  value={m.type}
                  onChange={(e) => update(i, "type", e.target.value)}
                  required
                />
                <label className="check-line" title="启用">
                  <input
                    aria-label={"启用支付方式 " + (i + 1)}
                    type="checkbox"
                    checked={m.enabled}
                    onChange={(e) => update(i, "enabled", e.target.checked)}
                  />
                </label>
                <button
                  type="button"
                  className="icon-button"
                  aria-label={"删除支付方式 " + (i + 1)}
                  onClick={() =>
                    setMethods((ms) => ms.filter((_, j) => j !== i))
                  }
                >
                  <X size={17} />
                </button>
              </div>
            ))}
            <Button
              type="button"
              disabled={methods.length >= 12}
              onClick={() =>
                setMethods((ms) => [
                  ...ms,
                  {
                    id: crypto.randomUUID().replaceAll("-", ""),
                    name: "",
                    type: "",
                    enabled: true,
                  },
                ])
              }
            >
              <Plus size={15} />
              添加支付方式
            </Button>
          </div>
        </div>
        <div className="settings-actions">
          <Button primary disabled={busy}>
            {busy ? "正在保存…" : "保存支付配置"}
          </Button>
        </div>
      </form>
    </Box>
  );
}

function AuthScreen({
  boot,
  path,
  go,
  api,
  onLogin,
}: {
  boot: Boot;
  path: string;
  go: (p: string) => void;
  api: (route: string, body?: Any) => Promise<any>;
  onLogin: (u: Any) => void;
}) {
  const register = path === "/register",
    forgot = path === "/forgot-password";
  const [pending, setPending] = useState(false),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false),
    [email, setEmail] = useState(""),
    [sent, setSent] = useState(false),
    [until, setUntil] = useState(() =>
      Number(sessionStorage.getItem("vistart.emailCooldown") ?? 0),
    ),
    [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    setPending(false);
    setError("");
    setSent(false);
  }, [path]);
  const left = Math.max(0, Math.ceil((until - now) / 1000));
  async function send() {
    setBusy(true);
    setError("");
    try {
      const r = await api("email/code", {
        email,
        purpose: forgot ? "reset" : "register",
      });
      const t = Date.now() + r.retry_after * 1000;
      setUntil(t);
      sessionStorage.setItem("vistart.emailCooldown", String(t));
      setSent(true);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="auth-shell">
      <div className="auth-brand">
        <Logo name={boot.site_name} />
      </div>
      <section className="glass auth-card">
        <div className="auth-card-head">
          <span className="small-icon">
            <ShieldCheck size={23} />
          </span>
          <h2>
            {pending
              ? "二步验证"
              : forgot
                ? "找回密码"
                : register
                  ? "创建账户"
                  : "登录账户"}
          </h2>
          <p>
            {pending
              ? "输入验证器动态码，或一个未使用的恢复码。"
              : forgot
                ? "使用已验证的邮箱重置登录密码。"
                : register
                  ? "创建账户后即可选购转发套餐。"
                  : "使用账户和密码登录。"}
          </p>
        </div>
        <form
          onSubmit={async (e) => {
            const v = formdata(e);
            setBusy(true);
            setError("");
            try {
              if (pending) {
                const r = await api("login/2fa", v);
                onLogin(r.user);
                return;
              }
              if (forgot) {
                await api("password/reset", { ...v, email });
                go("/login");
                return;
              }
              const r = await api(register ? "register" : "login", {
                ...v,
                ...(register ? { email } : {}),
              });
              if (r.two_factor_required) {
                setPending(true);
                return;
              }
              onLogin(r.user);
            } catch (e) {
              setError((e as Error).message);
            } finally {
              setBusy(false);
            }
          }}
        >
          {pending ? (
            <Field
              label="二步验证码或恢复码"
              name="code"
              autoComplete="one-time-code"
              maxLength={32}
              required
              autoFocus
            />
          ) : (
            <>
              {register && (
                <Field
                  label="显示名称"
                  name="name"
                  maxLength={40}
                  autoComplete="nickname"
                  required
                />
              )}
              {!forgot && (
                <Field
                  label="用户名或邮箱"
                  name="username"
                  autoComplete="username"
                  minLength={3}
                  maxLength={150}
                  required
                />
              )}
              {(register || forgot) && (
                <Field
                  label="邮箱地址"
                  name="email"
                  type="email"
                  autoComplete="email"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  required={forgot || boot.email_verification}
                />
              )}
              {(forgot || (register && boot.email_verification)) && (
                <div className="email-code">
                  <Field
                    label="邮箱验证码"
                    name="email_code"
                    autoComplete="one-time-code"
                    inputMode="numeric"
                    pattern="[0-9]{6}"
                    maxLength={6}
                    required
                  />
                  <Button
                    type="button"
                    disabled={busy || left > 0 || !email}
                    onClick={send}
                  >
                    {left > 0 ? left + " 秒后重试" : "获取验证码"}
                  </Button>
                </div>
              )}
              {sent && (
                <p className="hint">
                  如邮箱符合条件，验证码将发送至该邮箱，10 分钟内有效。
                </p>
              )}
              <Field
                label={forgot ? "新密码" : "密码"}
                name="password"
                type="password"
                autoComplete={
                  register || forgot ? "new-password" : "current-password"
                }
                minLength={register || forgot ? 12 : undefined}
                maxLength={128}
                required
                hint={
                  register
                    ? "至少 12 个字符，建议使用独立的长密码。"
                    : undefined
                }
              />
            </>
          )}
          {error && (
            <p role="alert" className="form-error">
              {error}
            </p>
          )}
          <Button primary disabled={busy}>
            {busy
              ? "请稍候…"
              : pending
                ? "验证并登录"
                : forgot
                  ? "重置密码"
                  : register
                    ? "创建账户"
                    : "登录"}
            <ArrowRight size={16} />
          </Button>
        </form>
        <div className="auth-tail">
          {pending ? (
            <button onClick={() => setPending(false)}>重新输入登录信息</button>
          ) : register || forgot ? (
            <>
              <span>已有账户</span>
              <button onClick={() => go("/login")}>返回登录</button>
            </>
          ) : (
            <>
              {boot.registration && (
                <button onClick={() => go("/register")}>注册账户</button>
              )}
              {boot.mail_enabled && (
                <button onClick={() => go("/forgot-password")}>忘记密码</button>
              )}
            </>
          )}
        </div>
      </section>
      <footer className="auth-footer">
        {boot.site_name}
        <span>转发管理</span>
      </footer>
    </div>
  );
}
const checkLabels: Record<string, string> = {
  checking: "检测中",
  pending: "下发中",
  offline: "节点离线",
  paused: "已暂停",
  stale: "检测过期",
  unsupported: "待升级 Agent",
  apply_failed: "下发失败",
  timeout: "TCP 超时",
  refused: "连接被拒绝",
  unreachable: "不可达",
  dns_error: "解析失败",
  blocked: "目标受限",
};
function RuleConnections({ allocation: a, now }: { allocation: Any; now: number }) {
  const c = a.connections;
  const fresh = c?.status === "ok" && c.updated_at > now - 15 && c.updated_at <= now + 10;
  const count = (key: "tcp" | "udp") =>
    fresh && Number.isSafeInteger(c?.[key]) && c[key] >= 0 ? c[key].toLocaleString() : "—";
  const unavailable = c?.status === "offline" ? "节点离线，暂无实时数据" : "等待 Agent 上报最新连接数";
  return (
    <div className="rule-connections" data-rule-id={a.id}>
      <span title={fresh ? "当前已建立的 TCP 转发连接" : unavailable}>
        <span>TCP</span><strong>{count("tcp")}</strong>
      </span>
      <span title={fresh ? "当前 UDP 活跃会话，连续 30 秒无流量后回收" : unavailable}>
        <span>UDP</span><strong>{count("udp")}</strong>
      </span>
    </div>
  );
}
function TargetAddresses({
  allocation: a,
  now,
}: {
  allocation: Any;
  now: number;
}) {
  const targets = a.load_balance
    ? a.targets
    : a.target_host
      ? [{ host: a.target_host, port: a.target_port }]
      : [];
  return (
    <div className="target-addresses">
      {targets.length > 1 && <small>轮询 · {targets.length} 个目标</small>}
      {!targets.length
        ? "未配置"
        : targets.map((target: Any, i: number) => {
            const c = (a.target_checks ?? []).find(
              (c: Any) => c.host === target.host && c.port === target.port,
            );
            const status =
              c?.checked_at &&
              (now - c.checked_at > 35 || c.checked_at > now + 10)
                ? "stale"
                : (c?.status ?? "checking");
            const good = status === "ok" && Number.isFinite(c?.latency_ms),
              bad = [
                "timeout",
                "refused",
                "unreachable",
                "dns_error",
                "blocked",
                "apply_failed",
              ].includes(status);
            const text = good
              ? (c.latency_ms < 1 ? "<1" : Math.round(c.latency_ms)) + " ms"
              : (checkLabels[status] ?? "检测中");
            const title =
              "节点至目标的 TCP 握手检测" +
              (c?.checked_at ? " · " + date(c.checked_at) : "") +
              "；UDP 和应用状态需单独确认。";
            return (
              <div className="target-address" key={i}>
                <span className="mono">
                  {endpoint(target.host, target.port)}
                </span>
                <span
                  className={
                    "tcping-badge " + (good ? "good" : bad ? "bad" : "muted")
                  }
                  title={title}
                  aria-label={
                    endpoint(target.host, target.port) + "，TCP 检测：" + text
                  }
                >
                  <i aria-hidden="true" />
                  {text}
                </span>
              </div>
            );
          })}
    </div>
  );
}
function TargetFields({ item = {} }: { item?: Any }) {
  const [balance, setBalance] = useState(!!item.load_balance),
    [targets, setTargets] = useState<Any[]>(
      item.targets?.length
        ? item.targets.map((t: Any) => ({ ...t }))
        : [
            { host: item.target_host ?? "", port: item.target_port || "" },
            { host: "", port: "" },
          ],
    );
  return (
    <>
      <label className="check-line">
        <input
          type="checkbox"
          name="load_balance"
          checked={balance}
          onChange={(e) => setBalance(e.target.checked)}
        />
        <span>启用目标负载均衡</span>
      </label>
      {balance ? (
        <>
          <input
            type="hidden"
            name="targets"
            value={JSON.stringify(
              targets.map((t) => ({ host: t.host, port: Number(t.port) })),
            )}
          />
          <input
            type="hidden"
            name="target_host"
            value={targets[0]?.host ?? ""}
          />
          <input
            type="hidden"
            name="target_port"
            value={targets[0]?.port ?? ""}
          />
          {targets.map((t, i) => (
            <div className="target-row" key={i}>
              <Field
                label={"目标地址 " + (i + 1)}
                name={"target_host_" + i}
                value={t.host}
                onChange={(e) =>
                  setTargets((ts) =>
                    ts.map((t, j) =>
                      i === j ? { ...t, host: e.target.value } : t,
                    ),
                  )
                }
                placeholder="IP 或域名"
                required
                maxLength={253}
              />
              <Field
                label={"目标端口 " + (i + 1)}
                name={"target_port_" + i}
                type="number"
                min="1"
                max="65535"
                value={t.port}
                onChange={(e) =>
                  setTargets((ts) =>
                    ts.map((t, j) =>
                      i === j ? { ...t, port: e.target.value } : t,
                    ),
                  )
                }
                required
              />
              <Button
                type="button"
                aria-label={"删除目标 " + (i + 1)}
                disabled={targets.length <= 2}
                onClick={() => setTargets((ts) => ts.filter((_, j) => i !== j))}
              >
                <X size={15} />
              </Button>
            </div>
          ))}
          <Button
            type="button"
            disabled={targets.length >= 16}
            onClick={() => setTargets((ts) => [...ts, { host: "", port: "" }])}
          >
            <Plus size={14} />
            添加目标
          </Button>
          <p className="hint">
            新连接轮流使用各目标，同一会话保持目标不变；TCP
            连接失败时尝试其他目标。
          </p>
        </>
      ) : (
        <>
          <Field
            label="目标地址"
            name="target_host"
            defaultValue={item.target_host}
            placeholder="IP 或域名"
            maxLength={253}
            required
          />
          <Field
            label="目标端口"
            name="target_port"
            type="number"
            min="1"
            max="65535"
            defaultValue={item.target_port || ""}
            required
          />
        </>
      )}
      <p className="hint">域名自动重新解析，支持 DDNS。</p>
      <Field
        label="目标备注（选填）"
        name="remark"
        defaultValue={item.remark ?? ""}
        maxLength={120}
        placeholder="例如：网站、游戏服务"
      />
    </>
  );
}
function MailForm({
  mail: m,
  busy,
  onSave,
  onTest,
}: {
  mail: Any;
  busy: boolean;
  onSave: (v: Any) => void;
  onTest: (v: Any) => void;
}) {
  return (
    <Box title="SMTP 邮件服务">
      <form
        className="settings-form"
        onSubmit={(e) => {
          const f = e.currentTarget,
            v = formdata(e);
          onSave({
            ...v,
            port: Number(v.port),
            enabled: new FormData(f).has("enabled"),
          });
        }}
      >
        <div className="form-grid">
          <Field
            label="SMTP 主机"
            name="host"
            defaultValue={m.host}
            placeholder="mail.example.com"
            required
          />
          <Field
            label="端口"
            name="port"
            type="number"
            min="1"
            max="65535"
            defaultValue={m.port ?? 465}
            required
          />
        </div>
        <Select
          label="加密方式"
          name="security"
          defaultValue={m.security ?? "ssl"}
        >
          <option value="ssl">SSL / TLS（通常 465）</option>
          <option value="tls">STARTTLS（通常 587）</option>
        </Select>
        <Field label="SMTP 用户名" name="username" defaultValue={m.username} />
        <Field
          label="SMTP 密码"
          name="password"
          type="password"
          autoComplete="new-password"
          placeholder={
            m.has_password ? "已配置，留空保持原密码" : "填写 SMTP 密码"
          }
        />
        <div className="form-grid">
          <Field
            label="发件邮箱"
            name="from_email"
            type="email"
            defaultValue={m.from_email}
            required
          />
          <Field
            label="发件名称"
            name="from_name"
            defaultValue={m.from_name}
            required
          />
        </div>
        <label className="check-line">
          <input type="checkbox" name="enabled" defaultChecked={m.enabled} />
          <span>启用邮件服务</span>
        </label>
        <p className="hint">
          验证服务端证书，密码加密保存。注册验证码开关位于面板设置。
        </p>
        <Button primary disabled={busy}>
          保存邮件配置
        </Button>
      </form>
      <div className="mail-test">
        <h3>发送测试邮件</h3>
        <p className="hint">使用已保存的配置向指定邮箱发送一封测试消息。</p>
        <form onSubmit={(e) => onTest(formdata(e))}>
          <Field
            label="接收测试邮件的邮箱"
            name="email"
            type="email"
            required
          />
          <Button disabled={busy}>发送测试邮件</Button>
        </form>
      </div>
    </Box>
  );
}

class Boundary extends React.Component<
  { children: React.ReactNode },
  { failed: boolean }
> {
  state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  render() {
    return this.state.failed ? (
      <div className="loading">
        <Logo />
        <h2>页面暂时无法显示</h2>
        <p>请重新载入；已保存的数据不会丢失。</p>
        <Button onClick={() => location.reload()}>重新载入</Button>
      </div>
    ) : (
      this.props.children
    );
  }
}
createRoot(document.getElementById("root")!).render(
  <Boundary>
    <App />
  </Boundary>,
);
