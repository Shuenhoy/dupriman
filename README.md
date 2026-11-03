# DupriMan — Manual Duplex Printing Assistant

**English** | [简体中文](README.zh_CN.md) | [繁體中文](README.zh_TW.md)

Gives **single-sided printers** a near-automatic duplex experience:
In any application's print dialog, select the virtual printer **dupriman-duplex** and choose
**Off / Long-edge / Short-edge** duplex. It will:

1. Print **all fronts** (odd pages, all copies);
2. Show a GTK dialog prompting to reinsert the paper;
3. Print **all backs** (even pages) after you click **OK**.

## Architecture

```mermaid
flowchart LR
    A[Application<br/>prints to dupriman-duplex] -->|PDF + Duplex option| B[CUPS queue<br/>dupriman://]
    B --> C[dupriman backend<br/>CUPS backend process]
    C -->|qpdf split| D[Front PDF<br/>odd pages]
    C -->|qpdf split + reverse/rotate| E[Back PDF<br/>even pages]
    D --> F[Real printer queue]
    C -->|private request directory| G[dupriman prompt<br/>GTK prompt daemon<br/>chosen session supervisor]
    G -->|user clicks OK| C
    E --> F
```

- **Backend** (`dupriman backend`, called by cupsd): parses duplex options, splits odd/even pages with qpdf, submits to the real queue and **waits for physical completion** before proceeding.
- **Prompt daemon** (`dupriman prompt`, runs in the user's graphical session): the backend creates a private directory for the submitting user below `/run/dupriman`. Requests and responses are owner-checked and capability-bound, so another local user cannot read job metadata or approve a job. Cancel or timeout safely aborts the back-side job.
- **Multiple copies**: The PPD declares `cupsManualCopies`, so copies are passed through to the real queue — all fronts (×N) first, one prompt, then all backs (×N).

## Dependencies

Runtime: `cups`, `qpdf`, `poppler` (pdfinfo), `gtk4` (>= 4.10)
Build: `rust` / `cargo`, `pkg-config`

Arch Linux:

```bash
sudo pacman -S --needed cups qpdf poppler gtk4 rust
```

## Manual Integration

### Arch Linux:

```bash
paru -S dupriman-git
```

DupriMan deliberately does not install files, create queues, manage runtime
directories, or enable a service.  Packages and administrators own those
actions so the program works with any init system.

### From source

```bash
cargo build --release
```

Install the binary, CUPS backend symlink, and PPD through your package manager
or chosen prefix.  Generate the PPD with `dupriman ppd`; generate an annotated
configuration with `dupriman sample-config`.  Then complete the system-specific
integration explicitly:

```bash
# The account cupsd runs backends as: `lp` on Debian/Ubuntu, `cups` on Arch.
getent passwd lp >/dev/null && BACKEND=lp || BACKEND=cups
echo "backend account: $BACKEND"

sudo install -d -o root -g root -m 0711 /run/dupriman
sudo install -d -o "$BACKEND" -g "$BACKEND" -m 0700 /var/tmp/dupriman
sudo install -d -m 0755 /etc/dupriman
sudo sh -c 'dupriman sample-config > /etc/dupriman/config.toml'
sudo lpadmin -p dupriman-duplex -v dupriman://duplex \
  -P /usr/share/cups/model/dupriman.ppd -E
```

Set `printer.real` and explicitly list permitted desktop accounts in
`prompt.allowed_users`.

**The backend account is detected, not assumed.** Upstream CUPS (and
Debian/Ubuntu) run backends as `lp`, but Arch Linux and Fedora ship only a
`cups` account — `getent passwd lp` is empty there. DupriMan resolves the
backend account from `paths.backend_user` if set, otherwise from the first
existing entry of `lp`, `cups`. Once the layout exists, the owner of
`requests/` is authoritative, so no guessing happens at print time.

For every permitted account, create a dedicated bridge group and the private
layout below; replace `alice` with that account name (`$BACKEND` is set above):

```bash
sudo groupadd --system dupriman-alice
sudo usermod -a -G dupriman-alice alice
sudo usermod -a -G dupriman-alice "$BACKEND"
sudo install -d -o root -g root -m 0711 /run/dupriman/alice
sudo install -d -o "$BACKEND" -g dupriman-alice -m 2750 /run/dupriman/alice/requests
sudo install -d -o alice -g dupriman-alice -m 2770 /run/dupriman/alice/responses
```

`/run` is a tmpfs on most systems, so this layout disappears on reboot. Make it
persistent with a tmpfiles.d entry (systemd hosts), which also lets you skip the
`install -d` lines above:

```bash
sudo tee /etc/tmpfiles.d/dupriman.conf >/dev/null <<EOF
d /run/dupriman 0711 root root -
d /run/dupriman/alice 0711 root root -
d /run/dupriman/alice/requests 2750 $BACKEND dupriman-alice -
d /run/dupriman/alice/responses 2770 alice dupriman-alice -
EOF
sudo systemd-tmpfiles --create /etc/tmpfiles.d/dupriman.conf
```

The backend writes requests as the CUPS backend account; the desktop user reads
them and writes responses.  Other local users receive no access, while response
UID and a random capability are still verified before backs print.  Log out and
back in after changing group membership.  Finally, supervise `dupriman prompt` in each
permitted graphical session using your init system, desktop autostart mechanism,
or service manager.  A systemd user unit may be useful as a local example, but
is not installed or enabled by DupriMan.

Run `dupriman test` to check all of the above; it reports the detected backend
account, the state directory, and whether each allowed user's bridge layout is
provisioned.

## Configuration

**You should add your physical printer by yourself with CUPS first.**

The backend reads only `/etc/dupriman/config.toml` (and an explicit `--config`
argument).  User-level configuration is for prompt presentation only; do not
override printer or path settings there.  Full annotated example: `dupriman sample-config`.

Key options (defaults work for most face-down-output laser printers):

```toml
[printer]
#Special value "save pdf": Do not print, save sub jobs to spool-dir/saved/(test mode, default)
#When printing on a real machine, change to the actual queue name, such as "HP_LaserJet_1020" (check with lpstat -v)
real = "HP_LaserJet_1020"        # Real printer queue name, check with lpstat -v
virtual_name = "dupriman-duplex" # Virtual queue name created by lpadmin

[paths]
# Account cupsd runs backends as. Leave empty to auto-detect ("lp", then "cups").
backend_user = ""

[duplex]
even_order = "reverse"           # Back page order: reverse | normal

[duplex.long_edge]
even_rotate = 180                # Long-edge flip: back rotation angle

[duplex.short_edge]
even_rotate = 0                  # Short-edge flip: back rotation angle

[prompt]
language = "auto"                # Dialog language: auto | en | zh_CN | zh_TW
timeout_secs = 600               # 0 = wait forever; timeout aborts
allowed_users = ["alice"]         # Accounts allowed to approve duplex jobs
```

### Internationalization (i18n)

Dialog text is selected by `prompt.language`:

| Value | Language |
|---|---|
| `auto` | Detect from `LC_ALL` / `LC_MESSAGES` / `LANG` (default) |
| `en` / `en_US` | English |
| `zh_CN` | Simplified Chinese |
| `zh_TW` | Traditional Chinese |

All translations are **compiled into the binary**, so an installed copy needs no
data directory. Files in `/usr/share/dupriman/i18n/` (or `./i18n/` next to the
binary) override the embedded copy, which lets a packager ship updates without
rebuilding.

The paper-orientation picture in the dialog is `assets/paper-orientation.svg`,
rendered to `assets/paper-orientation.png` by `scripts/render-diagram.sh` and
compiled into the binary. Edit the SVG (not the PNG), re-run the script, and
rebuild. It is rasterised rather than embedded as SVG because gdk-pixbuf does
not always ship the SVG loader (on Arch, `librsvg` provides `rsvg-convert` but
not the loader).

You can also override any text directly:
```toml
[prompt]
title = "My Custom Title"
message = "Please reinsert the paper"
ok_label = "Continue"
cancel_label = "Stop"
```

Custom text always takes precedence over language defaults.

### Wrong orientation or order?

Defaults assume a **face-down output tray** and the user action of
**taking the stack from the output tray and reinserting it as-is**:

| Symptom | Fix |
|---|---|
| Back order reversed (last page on top) | `duplex.even_order = "normal"` |
| Back upside-down (long-edge) | `duplex.long_edge.even_rotate = 0` |
| Back upside-down (short-edge) | `duplex.short_edge.even_rotate = 180` |
| Face-up output printer (e.g., some inkjets) | `duplex.even_order = "normal"` + swap the two `even_rotate` values |

Test with a 3–4 page document first.

## Usage

In the print dialog, select **dupriman-duplex** and choose duplex mode:

- **Off**: forwards directly to the real printer;
- **Long-edge** (default): for portrait documents flipped along the long edge (backs rotated 180°);
- **Short-edge**: for landscape documents flipped along the short edge (backs not rotated).

After fronts are printed, **do not flip or reorder** — take the stack from the output tray and reinsert it into the input tray as-is, then click OK. The dialog shows a picture of this: **A** marks the text direction, the top of the A is the head of the page, and in the tray the printed side (**A**) faces **down** with the head of the page pointing **toward the printer**.

## Logs & Troubleshooting

- Backend logs: `journalctl -u cups` or `/var/log/cups/error_log` (`[dupriman]` prefix)
- Prompt daemon: inspect the logs of the session supervisor you chose
- **Self-check**: `dupriman test`
- If the prompt daemon is not running when duplex is requested: fronts print, then the backend waits and safely aborts after `prompt.timeout_secs`.
- **Fronts print, then no dialog ever appears and the job waits in the queue**: the usual cause is that the **current login session predates the `dupriman-<user>` group**. Group membership is fixed at login, so after the group is created you must log out and back in before `dupriman prompt` can read `requests/`. `dupriman test` reports a failing "This session can read the bridge directory" check, and the daemon now exits with that reason instead of polling silently. To verify without logging out, run `newgrp dupriman-<user>`.
- **A dialog keeps reappearing for a job that was cancelled long ago**: the file in `requests/` outlived its backend. The prompt skips requests whose backend PID is gone (or that are older than `prompt.timeout_secs + 60`), and the backend deletes leftovers before writing a new request. Files left by older versions have no PID and are only swept after 24 hours; `sudo rm /run/dupriman/<user>/requests/*.json` clears them immediately.
- **Fronts print, then the job stalls or fails with "required account `lp` does not exist" / "prompt state ... is not provisioned"**: the bridge layout is missing or the backend runs under an account DupriMan did not provision. Check with `dupriman test` — it names the detected backend account and the exact `install -d` commands for your user. On Arch/Fedora the backend account is `cups`, not `lp`.
- **`dupriman test` reports the virtual printer missing while the queue prints fine**: `printer.virtual_name` must equal the queue name you created with `lpadmin` (case-sensitive). `lpstat -v` lists the real names.
- **The dialog appears in English although `prompt.language = "zh_CN"`**: an explicit `language` always wins, but `auto` follows your locale, and `LANG=en_US.UTF-8` alone selects English even when your other `LC_*` categories are Chinese. Set `language = "zh_CN"` explicitly, or export `LC_MESSAGES=zh_CN.UTF-8`.
- **Job sits in the queue disabled after a failed duplex run**: CUPS disables a queue whose backend exited non-zero. Re-enable it with `cupsenable <queue>` and clear the stuck job with `cancel -a <queue>`.
- Debug split results: set `print.keep_temp = true`; temporary PDFs are kept in `paths.spool_dir`.

## Removal

Remove the package with your package manager, stop the prompt service through
its owning session manager, and remove the CUPS queue with
`lpadmin -x dupriman-duplex` if it is no longer wanted.  Configuration and
runtime directories are explicitly administrator-owned state.

## Development

```bash
cargo test            # Unit tests + real qpdf split test
tests/e2e.sh          # End-to-end simulation (mock lp/lpstat, no paper wasted)
cupstestppd <(./target/debug/dupriman ppd)   # Validate PPD
```

### Adding a new language

1. Create `i18n/<locale>.toml` from `i18n/en_US.toml` and translate all keys.
2. Register in `src/i18n.rs` (all in one place):
   - `Lang` enum: add variant with serde rename + alias
   - `Lang::ALL`: add the new variant
   - `Lang::from_env()`: add language detection from `LC_ALL`/`LC_MESSAGES`/`LANG`
   - `Lang::file_stem()`: map to the locale filename stem
   - `BUILTIN`: add `("<locale>", include_str!("../i18n/<locale>.toml"))`
     so the translation ships inside the binary
3. Update the language table in [Internationalization](#internationalization-i18n) (optional).

Missing keys fall back to `en_US` automatically.  `dupriman ppd` reads the
language list from `Lang::ALL`, so no packaging-script changes are needed.

### Tested hardware

This software has only been tested on **HP LaserJet 1020** with the **foo2zjs** driver in **Arch Linux**.
If you test it on other printer models or drivers, please report your results or send a PR, 
especially the correct `even_order` and `even_rotate` values for your setup.
