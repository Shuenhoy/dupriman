#!/bin/sh
# 把 assets/*.svg 渲染成内嵌用的 PNG。
#
# 为什么要 PNG 而不是直接内嵌 SVG：这张机器上的 gdk-pixbuf 没有 SVG loader
# （librsvg 只提供了 rsvg-convert 命令），GTK 的 gdk::Texture 读不了 SVG。
# 用 PNG 就只依赖 GTK 自带的 PNG loader，运行时零新增依赖。
#
# 依赖：rsvg-convert（Arch: pacman -S librsvg）
set -eu

cd "$(dirname "$0")/.."
out=assets/paper-orientation.png

if ! command -v rsvg-convert >/dev/null 2>&1; then
    echo "rsvg-convert not found; install librsvg (Arch: pacman -S librsvg)" >&2
    exit 1
fi

# 2x 分辨率，HiDPI 屏上不会糊。
rsvg-convert --background-color=none --zoom=2 \
    -o "$out" assets/paper-orientation.svg

echo "rendered $out"
