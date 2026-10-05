#!/usr/bin/env bash
set +x
set -Eeuo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
umask 077
readonly FORWARD_OPS=/opt/forward-panel/ops/manage.py
forward_color='' forward_dim='' forward_reset=''
if [[ -t 1 && ${TERM:-dumb} != dumb && -z ${NO_COLOR:-} ]]; then
 forward_color=$'\033[36m';forward_dim=$'\033[2m';forward_reset=$'\033[0m'
fi
if (( $# )); then exec python3 "$FORWARD_OPS" "$@";fi
(( EUID == 0 )) || { printf '请使用 sudo forward-panel。\n';exit 1;}
while true; do
 printf '\n  %sVistart 转发面板%s  /  管理\n' "$forward_color" "$forward_reset"
 printf '  %s──────────────────────────────%s\n' "$forward_dim" "$forward_reset"
 python3 "$FORWARD_OPS" summary
 forward_columns=$(tput cols 2>/dev/null || printf 80)
 [[ "$forward_columns" =~ ^[0-9]+$ ]] || forward_columns=80
 if (( forward_columns < 48 )); then
 printf '\n  维护\n    1  检查并更新\n    2  重启服务\n    3  启动服务\n    4  停止服务\n    5  查看日志\n    6  运行详情\n\n  配置与数据\n    7  设置访问地址\n    8  创建备份\n    9  重置管理员密码\n   10  反向代理文档\n   11  回退上个版本\n   12  卸载程序\n\n    0  退出\n\n'
 else
 printf '\n  维护\n    1  检查并更新       2  重启服务\n    3  启动服务         4  停止服务\n    5  查看日志         6  运行详情\n\n  配置与数据\n    7  设置访问地址     8  创建备份\n    9  重置管理员密码   10  反向代理文档\n   11  回退上个版本     12  卸载程序\n\n    0  退出\n\n'
 fi
 read -r -p '  选择：' forward_choice || exit 0
 case "$forward_choice" in
  0) exit 0;;
  1) python3 "$FORWARD_OPS" update || true;;
  2) python3 "$FORWARD_OPS" restart || true;;
  3) python3 "$FORWARD_OPS" start || true;;
  4) python3 "$FORWARD_OPS" stop || true;;
  5) journalctl -u forward-panel.service -n 80 --no-pager;;
  6) systemctl status forward-panel.service --no-pager || true;;
  7) python3 "$FORWARD_OPS" address || true;;
  8) python3 "$FORWARD_OPS" backup || true;;
  9) python3 "$FORWARD_OPS" reset-password || true;;
  10) printf '\n  https://github.com/coexacx/forward-panel/blob/main/docs/反向代理.md\n';;
  11) python3 "$FORWARD_OPS" rollback || true;;
  12) python3 "$FORWARD_OPS" uninstall || true; [[ -f "$FORWARD_OPS" ]] || exit 0;;
  *) printf '\n  请输入菜单中的编号。\n';;
 esac
 read -r -p $'\n  按回车返回…' _ || exit 0
done
