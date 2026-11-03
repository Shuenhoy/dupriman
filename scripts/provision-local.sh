#!/usr/bin/env bash
# 本机一次性供应脚本（需要 root）：建 bridge 组、私有目录、tmpfiles.d 条目，
# 并把 CUPS 队列名与配置对齐。可重复执行。
#
#   sudo scripts/provision-local.sh [用户名] [队列名]
#
# 默认：用户 = 调用 sudo 的账户，队列名 = DupriMan-Manual-Duplex
set -euo pipefail

USER_NAME="${1:-${SUDO_USER:-}}"
QUEUE="${2:-DupriMan-Manual-Duplex}"
CONF="/etc/dupriman/config.toml"
STATE_ROOT="/run/dupriman"
SPOOL="/var/tmp/dupriman"

if [ "$(id -u)" -ne 0 ]; then
  echo "必须以 root 运行：sudo $0 $*" >&2
  exit 1
fi
if [ -z "$USER_NAME" ] || ! getent passwd "$USER_NAME" >/dev/null; then
  echo "用法：sudo $0 <已存在的本地用户名> [队列名]" >&2
  exit 1
fi

# cupsd 运行 backend 所用的账户：上游/Debian 用 lp，Arch/Fedora 用 cups
if getent passwd lp >/dev/null; then
  BACKEND="lp"
elif getent passwd cups >/dev/null; then
  BACKEND="cups"
else
  echo "找不到 lp 或 cups 账户，CUPS 可能未安装" >&2
  exit 1
fi
echo "用户=$USER_NAME  后端账户=$BACKEND  队列=$QUEUE"

GROUP="dupriman-$USER_NAME"
getent group "$GROUP" >/dev/null || groupadd --system "$GROUP"
usermod -a -G "$GROUP" "$USER_NAME"
usermod -a -G "$GROUP" "$BACKEND"

install -d -o root -g root -m 0711 "$STATE_ROOT"
install -d -o root -g root -m 0711 "$STATE_ROOT/$USER_NAME"
install -d -o "$BACKEND" -g "$GROUP" -m 2750 "$STATE_ROOT/$USER_NAME/requests"
install -d -o "$USER_NAME" -g "$GROUP" -m 2770 "$STATE_ROOT/$USER_NAME/responses"
install -d -o "$BACKEND" -g "$BACKEND" -m 0700 "$SPOOL"

# /run 是 tmpfs：重启后需要重建
tee /etc/tmpfiles.d/dupriman.conf >/dev/null <<EOF
d $STATE_ROOT 0711 root root -
d $STATE_ROOT/$USER_NAME 0711 root root -
d $STATE_ROOT/$USER_NAME/requests 2750 $BACKEND $GROUP -
d $STATE_ROOT/$USER_NAME/responses 2770 $USER_NAME $GROUP -
EOF
echo "已写入 /etc/tmpfiles.d/dupriman.conf"

# 让配置里的队列名与实际注册的队列一致（printer.virtual_name 与 real 各自独立）
if [ -f "$CONF" ]; then
  sed -i "s|^virtual_name *=.*|virtual_name = \"$QUEUE\"|" "$CONF"
  echo "已把 $CONF 的 printer.virtual_name 设为 $QUEUE"
else
  echo "警告：$CONF 不存在，跳过队列名对齐" >&2
fi

# 队列若因 backend 非零退出被禁用，重新启用并清掉卡住的作业
cupsenable "$QUEUE" 2>/dev/null || true
cancel -a "$QUEUE" 2>/dev/null || true

echo
echo "完成。新的组成员关系不会自动出现在已有会话里："
echo "  systemd 的 user@$USER_NAME.service 自登录起就一直在运行，"
echo "  它启动时捕获的组集合会被 dupriman-prompt.service 继承。"
echo "请彻底结束当前会话让 user@ 重新启动："
echo "  loginctl terminate-user $USER_NAME     # 会关闭所有会话，然后重新登录"
echo "临时验证（不注销）：先执行 newgrp $GROUP，再手动运行 dupriman prompt"
echo
echo "然后自检："
echo "  dupriman test"
