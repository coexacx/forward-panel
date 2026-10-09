# Agent 资源保护

0.3.4 修复了高 TCP 缓冲配置下 Agent 触发内存上限、重启并重新加载规则的问题。面板的重新下发是进程重新上线后的校对，故障源在节点内存预算。

## 部署预算

主控远程部署或更新 Agent 时按节点物理内存的四分之一设置 systemd MemoryMax，下限 64 MiB，上限 2 GiB。这是进程与其内核内存的上限，不是启动时预占。已有安装需要更新 Agent 才会写入新设置。容器或上级 systemd slice 有更低限制时，Agent 按可见 cgroup 层级中更低的限制计算。

Agent 从该额度预留基础开销与回收余量，再为每条监听规则、TCP 连接和 UDP 会话预留缓冲容量。TCP 两端的收发缓冲按 Linux 实际计账方式计算。窗口根据剩余容量调整；轻载保留较大的窗口，并发增加时新连接使用较小窗口。Linux 自身较低的 socket 上限也会纳入计算。

不修改服务器全局 TCP 参数，不影响其他应用的缓冲设置。默认 TCP 512 / UDP 256 等计数上限，以及套餐连接限制，仍是准入上限；实际可同时接纳数量也受节点内存、监听规则数量和当时已预留资源影响。

## 已有连接与关闭

内存预算不足时拒绝新增连接或会话，已工作的连接保留原有预留。并非所有资源不足都可通过提高连接数上限解决。

正常 TCP 半关闭等待已发送数据被确认后释放预留，保留 FIN 语义。慢速读取不会额外触发固定的收尾超时；错误、规则撤销、到期和安全策略取消仍关闭连接并及时回收队列。规则未变更时不会为了计算预算而重启监听。

主控失联保护、流量达量暂停、套餐到期、目标修改和手动删除规则仍按原有业务规则执行；这些明确事件可能中止转发。本次修复不放宽 WSS 认证或失联后的额度保护。

## 排查

查看 Agent 服务与内核事件：

~~~bash
systemctl show vistart-agent.service -p MainPID -p NRestarts -p MemoryCurrent -p MemoryPeak -p MemoryMax
journalctl -u vistart-agent.service --since '-1 hour'
journalctl -k --since '-1 hour'
~~~

部分较旧 systemd 不提供 MemoryPeak。关注 OOM、进程重启、控制会话失联与规则真正变更的时间顺序。内存压力检查需包含 socket 内核计账，不能只查看进程 RSS。手动调整 MemoryMax 后，需重启 Agent 才会重新读取预算；重启会中断已有转发连接。
