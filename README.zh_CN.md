# DupriMan — 手动双面打印助手

[English](README.md) | **简体中文** | [繁體中文](README.zh_TW.md)

为**单面打印机**提供近乎自动的双面打印体验：
在任意应用的打印对话框中，选择虚拟打印机 **dupriman-duplex**，并选择
**关闭 / 长边翻转 / 短边翻转** 双面模式。它将：

1. 打印**所有正面**（奇数页，全部份数）；
2. 弹出 GTK 对话框，提示重新放入纸张；
3. 点击**确定**后打印**所有背面**（偶数页）。

## 架构

```mermaid
flowchart LR
    A[应用程序<br/>打印到 dupriman-duplex] -->|PDF + 双面选项| B[CUPS 队列<br/>dupriman://]
    B --> C[dupriman 后端<br/>CUPS 后端进程]
    C -->|qpdf 拆分| D[正面 PDF<br/>奇数页]
    C -->|qpdf 拆分 + 倒序/旋转| E[背面 PDF<br/>偶数页]
    D --> F[真实打印机队列]
    C -->|写入私有请求目录| G[dupriman prompt<br/>GTK 弹窗守护进程<br/>会话管理器]
    G -->|用户点击确定| C
    E --> F
```

- **后端**（`dupriman backend`，由 cupsd 调用）：解析双面选项，用 qpdf 拆分奇偶页，提交到真实打印机队列，并在继续之前**等待物理打印完成**。
- **弹窗守护进程**（`dupriman prompt`，运行在用户图形会话中）：后端会在 `/run/dupriman` 下为提交用户创建私有目录。请求与响应均校验文件所有者和随机 capability，本机其他用户无法读取任务元数据或伪造确认。取消或超时会安全中止背面打印任务。
- **多份拷贝**：PPD 声明了 `cupsManualCopies`，因此份数会透传到真实队列——先打印全部正面（×N），弹窗一次，再打印全部背面（×N）。

## 依赖

运行时：`cups`、`qpdf`、`poppler`（pdfinfo）、`gtk4`（>= 4.10）
构建：`rust` / `cargo`、`pkg-config`

Arch Linux：

```bash
sudo pacman -S --needed cups qpdf poppler gtk4 rust
```

## 手动集成

### Arch Linux:

```bash
paru -S dupriman-git
```

### 从源码安装

```bash
cargo build --release
```

DupriMan 不会自行安装文件、创建 CUPS 队列、管理运行目录或启用服务。
包管理器和管理员必须显式拥有这些动作，因而可适配任意 init 系统。
安装二进制、CUPS backend 符号链接及 `dupriman ppd` 生成的 PPD 后，执行：

```bash
# cupsd 运行 backend 所用的账户：Debian/Ubuntu 为 lp，Arch 为 cups
getent passwd lp >/dev/null && BACKEND=lp || BACKEND=cups
echo "backend account: $BACKEND"

sudo install -d -o root -g root -m 0711 /run/dupriman
sudo install -d -o "$BACKEND" -g "$BACKEND" -m 0700 /var/tmp/dupriman
sudo install -d -m 0755 /etc/dupriman
sudo sh -c 'dupriman sample-config > /etc/dupriman/config.toml'
sudo lpadmin -p dupriman-duplex -v dupriman://duplex \
  -P /usr/share/cups/model/dupriman.ppd -E
```

配置 `printer.real` 与明确的 `prompt.allowed_users`。

**后端账户是探测出来的，不是写死的。** 上游 CUPS（以及 Debian/Ubuntu）以 `lp`
运行 backend，但 Arch Linux 与 Fedora 只提供 `cups` 账户——在那里
`getent passwd lp` 是空的。DupriMan 优先使用 `paths.backend_user`；未设置时按
`lp`、`cups` 的顺序取第一个存在的账户；目录已存在后以 `requests/` 的属主为准，
打印时不再做任何猜测。

每个允许的账户均须创建专用 bridge group 和私有目录布局；以下以 `alice` 为例
（`$BACKEND` 已在上方设置）：

```bash
sudo groupadd --system dupriman-alice
sudo usermod -a -G dupriman-alice alice
sudo usermod -a -G dupriman-alice "$BACKEND"
sudo install -d -o root -g root -m 0711 /run/dupriman/alice
sudo install -d -o "$BACKEND" -g dupriman-alice -m 2750 /run/dupriman/alice/requests
sudo install -d -o alice -g dupriman-alice -m 2770 /run/dupriman/alice/responses
```

`/run` 在多数系统上是 tmpfs，重启后上述布局会消失。可用 tmpfiles.d 条目持久化
（systemd 系统），并省去上面的 `install -d`：

```bash
sudo tee /etc/tmpfiles.d/dupriman.conf >/dev/null <<EOF
d /run/dupriman 0711 root root -
d /run/dupriman/alice 0711 root root -
d /run/dupriman/alice/requests 2750 $BACKEND dupriman-alice -
d /run/dupriman/alice/responses 2770 alice dupriman-alice -
EOF
sudo systemd-tmpfiles --create /etc/tmpfiles.d/dupriman.conf
```

backend 以 CUPS backend 账户写入请求，图形用户读取请求并写入响应。其他本地用户没有访问
权限，且打印背面前仍会验证响应 UID 和随机 capability。修改组成员后需注销再登录。
最后用所选 init、桌面 autostart 或会话管理器在每个允许的图形会话中监管
`dupriman prompt`。systemd user unit 只能作为本地示例，DupriMan 不会安装或启用它。

运行 `dupriman test` 可检查以上各项：它会显示探测到的后端账户、状态目录，以及每个
允许用户的 bridge 布局是否已供应。

## 配置

**你应当先使用CUPS添加你的真实打印机。**

backend 只读取 `/etc/dupriman/config.toml` 和显式的 `--config` 参数。
用户级配置只适合覆盖弹窗文案；不要在那里覆盖打印机或路径设置。完整注释示例：`dupriman sample-config`。

关键选项（默认值适用于大多数出纸面朝下的激光打印机）：

```toml
[printer]
# 特殊值 "save-pdf"：不打印，子作业保存到 spool_dir/saved/（测试模式，默认）
# 真机打印时改为实际队列名，如 "HP_LaserJet_1020"（lpstat -v 查看）
real = "save-pdf"
virtual_name = "dupriman-duplex"  # lpadmin 创建的虚拟队列名，须与实际队列一致

[paths]
# cupsd 运行 backend 所用的账户；留空则自动探测（先 lp，再 cups）
backend_user = ""

[duplex]
even_order = "reverse"           # 背面页序：reverse（倒序）| normal（正序）

[duplex.long_edge]
even_rotate = 180                # 长边翻转：背面旋转角度

[duplex.short_edge]
even_rotate = 0                  # 短边翻转：背面旋转角度

[prompt]
language = "auto"                # 对话框语言：auto | en | zh_CN | zh_TW
timeout_secs = 600               # 0 = 永久等待；超时后中止
allowed_users = ["alice"]         # 允许确认双面作业的本地账户
```

### 国际化（i18n）

对话框文本由 `prompt.language` 选择：

| 值 | 语言 |
|---|---|
| `auto` | 从 `LC_ALL` / `LC_MESSAGES` / `LANG` 自动检测（默认） |
| `en` / `en_US` | 英语 |
| `zh_CN` | 简体中文 |
| `zh_TW` | 繁体中文 |

所有翻译都已**编译进二进制**，因此安装后的副本不需要额外的数据目录。
`/usr/share/dupriman/i18n/`（或二进制旁边的 `./i18n/`）中的文件会覆盖内嵌版本，
便于打包者不重新编译就更新译文。

弹窗里的放纸示意图源文件是 `assets/paper-orientation.svg`，由
`scripts/render-diagram.sh` 渲染成 `assets/paper-orientation.png` 后编译进二进制。
要改图请编辑 SVG（不是 PNG），重跑该脚本再重新编译。之所以转成 PNG 而不是
直接内嵌 SVG，是因为 gdk-pixbuf 不一定带 SVG loader（Arch 的 `librsvg` 只提供
`rsvg-convert`，不装 loader）。

也可以直接覆盖任意文本：

```toml
[prompt]
title = "自定义标题"
message = "请重新放入纸张"
ok_label = "继续"
cancel_label = "停止"
```

自定义文本始终优先于语言默认值。

### 方向或页序不对？

默认值假设**出纸面朝下的输出托盘**，用户操作是
**从输出托盘取出整叠纸，原样放回进纸槽**：

| 症状 | 修复方法 |
|---|---|
| 背面页序反了（最后一页在最上面） | `duplex.even_order = "normal"` |
| 背面颠倒（长边翻转） | `duplex.long_edge.even_rotate = 0` |
| 背面颠倒（短边翻转） | `duplex.short_edge.even_rotate = 180` |
| 出纸面朝上的打印机（如部分喷墨） | `duplex.even_order = "normal"` + 对调两个 `even_rotate` 值 |

建议先用 3–4 页的文档测试。

## 使用方法

在打印对话框中选择 **dupriman-duplex**，然后选择双面模式：

- **关闭**：直接转发到真实打印机；
- **长边翻转**（默认）：适用于纵向文档，沿长边翻转（背面旋转 180°）；
- **短边翻转**：适用于横向文档，沿短边翻转（背面不旋转）。

正面打印完成后，**不要翻转或重新排序**——从输出托盘取出整叠纸，原样放入进纸槽，然后点击确定。弹窗里有一张示意图说明这一点：**A** 代表文字方向，A 的顶端就是页头；放入纸盒时 A 面（已打印面）朝**下**，页头朝向**打印机**。

## 日志与故障排除

- 后端日志：`journalctl -u cups` 或 `/var/log/cups/error_log`（前缀 `[dupriman]`）
- 弹窗守护进程：查看所选会话管理器的日志
- 自检：`dupriman test`
- 如果请求双面打印时弹窗守护进程未运行：正面会正常打印，之后后端等待并在 `prompt.timeout_secs` 后安全中止。
- **正面打印完后卡住，或报 "required account `lp` does not exist" / "prompt state ... is not provisioned"**：说明 bridge 目录未供应，或后端运行在 DupriMan 未供应的账户下。用 `dupriman test` 检查——它会显示探测到的后端账户以及你账户需要的 `install -d` 命令。在 Arch/Fedora 上后端账户是 `cups`，不是 `lp`。
- **正面打印完后一直没有弹窗，队列里作业一直等**：最常见的原因是**当前登录会话早于 `dupriman-<用户>` 组创建**。组成员关系在登录时固定，所以新建组后必须注销再登录，`dupriman prompt` 才能读到 `requests/`。`dupriman test` 会明确报「当前会话可读取 bridge 目录」一项未通过；守护进程启动时也会直接以该原因退出，而不是空转。临时验证可先 `newgrp dupriman-<用户>`。
- **早已取消的作业仍不断弹出确认框**：`requests/` 里的请求文件比它的后端活得久。现在弹窗会跳过后端 PID 已消失的请求（或超过 `prompt.timeout_secs + 60` 的），后端在写新请求前也会顺手清理遗留文件。旧版本写的文件没有 PID，只能等 24 小时后清理；可立即用 `sudo rm /run/dupriman/<用户>/requests/*.json` 清除。
- **`dupriman test` 报虚拟打印机缺失，但队列能正常打印**：`printer.virtual_name` 必须与 `lpadmin` 创建的队列名完全一致（区分大小写）。`lpstat -v` 可列出真实队列名。
- **虽然设置了 `prompt.language = "zh_CN"`，弹窗仍显示英文**：显式 `language` 总是优先，但 `auto` 跟随 locale，且仅 `LANG=en_US.UTF-8` 就会选中英文，即使其他 `LC_*` 类别是中文。请显式设置 `language = "zh_CN"`，或导出 `LC_MESSAGES=zh_CN.UTF-8`。
- **双面任务失败后队列变成 disabled**：backend 非零退出会让 CUPS 禁用队列。用 `cupsenable <队列>` 重新启用，用 `cancel -a <队列>` 清掉卡住的作业。
- 调试拆分结果：设置 `print.keep_temp = true`；临时 PDF 将保留在 `paths.spool_dir` 中。

## 移除

用包管理器删除软件包，用其所属会话管理器停止 prompt 服务；如不再使用队列，
执行 `lpadmin -x dupriman-duplex`。配置与运行目录由管理员显式管理。

## 开发

```bash
cargo test            # 单元测试 + 真实 qpdf 拆分测试
tests/e2e.sh          # 端到端模拟（mock lp/lpstat，不浪费纸张）
cupstestppd <(./target/debug/dupriman ppd)   # 验证 PPD
```

### 添加新语言

1. 从 `i18n/en_US.toml` 复制创建 `i18n/<locale>.toml`，翻译所有键。
2. 在 `src/i18n.rs` 中注册（全部集中在一处）：
   - `Lang` 枚举：添加变体，带上 serde rename 和 alias
   - `Lang::ALL`：加入新变体
   - `Lang::from_env()`：添加从 `LC_ALL`/`LC_MESSAGES`/`LANG` 检测语言的逻辑
   - `Lang::file_stem()`：映射到语言文件名
   - `BUILTIN`：加入 `("<locale>", include_str!("../i18n/<locale>.toml"))`，
     让译文一并编译进二进制
3. 更新[国际化](#国际化i18n)中的语言表（可选）。

缺失的键会自动回退到 `en_US`。`dupriman ppd` 通过 `Lang::ALL` 自动发现所有语言，无需修改打包脚本。

### 已测试硬件

本软件仅在 **Arch Linux** 上的 **HP LaserJet 1020** + **foo2zjs** 驱动上测试过。
如果你在其他打印机型号或驱动上测试，欢迎反馈结果或提交 PR，
尤其是适合你设备的 `even_order` 和 `even_rotate` 配置值。
