//! 弹窗守护进程：在用户桌面会话中运行（由所选会话管理器监管）。
//!
//! 轮询状态目录，发现后端发来的确认请求后弹出 GTK4 对话框，
//! 将用户选择写回响应文件。只处理与当前用户同名的请求，
//! 文案全部来自配置（含用户级覆盖）。

use anyhow::{Context, Result};
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use crate::config::Config;
use crate::prompt_proto::{self, PromptRequest, PromptResponse};

pub fn run(cfg: &Config) -> Result<()> {
    ensure_display_env();
    gtk4::init().context(
        "GTK initialization failed: please run inside a graphical session (WAYLAND_DISPLAY/DISPLAY available)",
    )?;

    let current_user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default();
    if !cfg
        .prompt
        .allowed_users
        .iter()
        .any(|user| user == &current_user)
    {
        anyhow::bail!("current user is not listed in prompt.allowed_users");
    }
    let state =
        prompt_proto::user_state(&cfg.paths.state_dir, &current_user, &cfg.paths.backend_user)?;

    // The bridge directories are reachable only through `dupriman-<user>`.
    // Group membership is fixed when the login session starts, so an account
    // added to a freshly created group is invisible to a session that predates
    // it — the daemon then starts cleanly and can never read a request, which
    // looks exactly like "no dialog ever appears".  Probe the directory itself
    // rather than the group table: that is the condition that actually matters.
    if let Err(error) = std::fs::read_dir(state.requests_dir()) {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            let group = format!("dupriman-{current_user}");
            let gid = prompt_proto::group_id(&group)
                .map(|gid| gid.to_string())
                .unwrap_or_else(|| "?".to_string());
            anyhow::bail!(
                "{}",
                cfg.prompt
                    .cli_fmt("cli_group_missing", &[("group", &group), ("gid", &gid)])
            );
        }
        return Err(error)
            .with_context(|| format!("cannot read {}", state.requests_dir().display()));
    }

    let state_dir_display = state.requests_dir().display().to_string();
    let prompt = cfg.prompt.clone();
    let busy = Rc::new(Cell::new(false));
    // Report a broken bridge once, not on every polling tick.
    let unreadable = Rc::new(std::cell::RefCell::new(String::new()));
    // A backend gives up after `timeout_secs`; anything much older belongs to a
    // job CUPS already killed and must not re-open a dialog.
    let max_age = (cfg.prompt.timeout_secs > 0)
        .then(|| Duration::from_secs(cfg.prompt.timeout_secs.saturating_add(60)));

    glib::timeout_add_seconds_local(1, {
        let busy = Rc::clone(&busy);
        let unreadable = Rc::clone(&unreadable);
        move || {
            if busy.get() {
                return glib::ControlFlow::Continue;
            }
            let pending = match prompt_proto::pending_requests(&state, max_age) {
                Ok(pending) => {
                    unreadable.borrow_mut().clear();
                    pending
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    if *unreadable.borrow() != message {
                        eprintln!("[dupriman-prompt] {message}");
                        *unreadable.borrow_mut() = message;
                    }
                    return glib::ControlFlow::Continue;
                }
            };
            let Some((_, req)) = pending.first().cloned() else {
                return glib::ControlFlow::Continue;
            };
            eprintln!(
                "[dupriman-prompt] {}",
                prompt.cli_fmt(
                    "cli_confirmation_request",
                    &[("job_id", &req.job_id), ("title", &req.title)]
                )
            );
            busy.set(true);
            glib::MainContext::default().spawn_local(show_dialog(
                state.clone(),
                req,
                prompt.clone(),
                Rc::clone(&busy),
            ));
            glib::ControlFlow::Continue
        }
    });

    eprintln!(
        "[dupriman-prompt] {}",
        cfg.prompt
            .cli_fmt("cli_daemon_started", &[("dir", &state_dir_display)])
    );
    glib::MainLoop::new(None, false).run();
    Ok(())
}

/// 确保 GTK 能连上显示服务器。
///
/// 若 WAYLAND_DISPLAY/DISPLAY 均未设置（例如服务先于会话启动、
/// compositor 未做 import-environment），扫描 $XDG_RUNTIME_DIR
/// 下的 wayland-* socket 自动设置 WAYLAND_DISPLAY。
fn ensure_display_env() {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some() {
        return;
    }
    let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") else {
        return;
    };
    let mut sockets: Vec<_> = std::fs::read_dir(&runtime_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter_map(|n| n.into_string().ok())
        .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
        .collect();
    sockets.sort();
    if let Some(name) = sockets.first() {
        // 此处无法访问 cfg，直接用英文（属诊断信息，不影响用户界面）
        eprintln!(
            "[dupriman-prompt] No display env var found, auto-detecting WAYLAND_DISPLAY={name}"
        );
        // SAFETY: setting process env var during single-threaded startup
        unsafe { std::env::set_var("WAYLAND_DISPLAY", name) };
    }
}

async fn show_dialog(
    state: prompt_proto::UserState,
    req: PromptRequest,
    prompt: crate::config::PromptCfg,
    busy: Rc<Cell<bool>>,
) {
    // The answer is filled in by the buttons; polling it keeps the UI
    // responsive without pulling in another channel crate.
    let answer: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));

    let window = gtk4::Window::builder()
        .modal(true)
        .resizable(false)
        .title(prompt.resolved_title())
        .build();
    window.add_css_class("dialog");

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);
    window.set_child(Some(&root));

    let heading = gtk4::Label::new(None);
    heading.set_markup(&format!(
        "<span size=\"large\" weight=\"bold\">{}</span>",
        glib_markup_escape(&prompt.resolved_title())
    ));
    heading.set_xalign(0.0);
    root.append(&heading);

    let message = gtk4::Label::new(Some(&prompt.resolved_message()));
    message.set_wrap(true);
    message.set_xalign(0.0);
    message.set_max_width_chars(52);
    root.append(&message);

    // The picture is the point: words alone left users guessing which face
    // goes down and which edge points at the printer.
    if let Some(diagram) = crate::diagram::paper_diagram() {
        root.append(&diagram);
    }

    let captions = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    captions.set_halign(gtk4::Align::Center);
    for text in [
        prompt.resolved_diagram_caption_1(),
        prompt.resolved_diagram_caption_2(),
    ] {
        let label = gtk4::Label::new(Some(&text));
        label.add_css_class("dim-label");
        label.set_wrap(true);
        label.set_justify(gtk4::Justification::Center);
        captions.append(&label);
    }
    root.append(&captions);

    if !req.detail.is_empty() {
        let detail = gtk4::Label::new(Some(&req.detail));
        detail.set_wrap(true);
        detail.set_xalign(0.0);
        detail.add_css_class("dim-label");
        root.append(&detail);
    }

    let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    buttons.set_halign(gtk4::Align::End);
    buttons.set_margin_top(6);

    let cancel = gtk4::Button::with_label(&prompt.resolved_cancel_label());
    let ok = gtk4::Button::with_label(&prompt.resolved_ok_label());
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    root.append(&buttons);

    {
        let window = window.clone();
        let answer = Rc::clone(&answer);
        cancel.connect_clicked(move |_| {
            answer.set(Some(false));
            window.close();
        });
    }
    {
        let window = window.clone();
        let answer = Rc::clone(&answer);
        ok.connect_clicked(move |_| {
            answer.set(Some(true));
            window.close();
        });
    }
    {
        let answer = Rc::clone(&answer);
        window.connect_close_request(move |_| {
            if answer.get().is_none() {
                answer.set(Some(false));
            }
            glib::Propagation::Proceed
        });
    }

    // Esc 取消、回车确认，与常见对话框一致。
    {
        let window = window.clone();
        let answer = Rc::clone(&answer);
        let key = gtk4::EventControllerKey::new();
        let window_for_key = window.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            use gtk4::gdk::Key;
            match keyval {
                Key::Escape => {
                    answer.set(Some(false));
                    window_for_key.close();
                    glib::Propagation::Stop
                }
                Key::Return | Key::KP_Enter => {
                    answer.set(Some(true));
                    window_for_key.close();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        window.add_controller(key);
    }

    window.present();
    ok.grab_focus();

    while answer.get().is_none() {
        glib::timeout_future(Duration::from_millis(50)).await;
    }

    let (confirmed, reason) = match answer.get() {
        Some(true) => (true, String::new()),
        Some(false) => (false, "user clicked cancel".into()),
        None => (false, "dialog closed".into()),
    };

    let resp = PromptResponse {
        id: req.id.clone(),
        capability: req.capability.clone(),
        confirmed,
        reason,
    };
    if let Err(e) = prompt_proto::send_response(&state, &resp) {
        eprintln!(
            "[dupriman-prompt] {}",
            prompt.cli_fmt("cli_failed_write_response", &[("error", &format!("{e:#}"))])
        );
    }
    busy.set(false);
}

/// GTK 的 markup 只认少数实体，任务标题里出现 `&` 或 `<` 会让标签消失。
fn glib_markup_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
