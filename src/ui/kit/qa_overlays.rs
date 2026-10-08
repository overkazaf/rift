//! Smoke + visual-QA tests for overlays that expose enough public API to be
//! constructed from outside their module. (Overlays with private state carry
//! their own `qa_tests` modules.) Set `RIFT_QA_OUT=<dir>` to dump PPMs.

use super::gallery::qa::each_theme;

#[test]
fn preferences_render() {
    let cfg = crate::config::Config::default();
    let mut p = crate::ui::Preferences::new(&cfg);
    p.visible = true;
    each_theme("prefs", |b, w, h, f, t| p.render(b, w, h, f, t));
    p.handle_key(crate::ui::PrefsKey::Tab);
    p.handle_key(crate::ui::PrefsKey::Tab);
    p.handle_key(crate::ui::PrefsKey::Tab);
    p.handle_key(crate::ui::PrefsKey::Tab);
    each_theme("prefs-keys", |b, w, h, f, t| p.render(b, w, h, f, t));
}

#[test]
fn ssh_and_webview_dialogs_render() {
    let mut d = crate::network::SshDialog::new();
    d.visible = true;
    each_theme("ssh", |b, w, h, f, t| d.render(b, w, h, f, t));
    let mut v = crate::network::WebViewDialog::new();
    v.visible = true;
    each_theme("webview-dialog", |b, w, h, f, t| v.render(b, w, h, f, t));
}

#[test]
fn search_bar_renders() {
    let mut s = crate::tools::search::SearchOverlay::new();
    s.visible = true;
    s.query = "cargo build".into();
    s.matches.push(crate::tools::search::SearchMatch { row: 1, col_start: 0, col_end: 5 });
    s.matches.push(crate::tools::search::SearchMatch { row: 3, col_start: 0, col_end: 5 });
    each_theme("search", |b, w, h, f, t| s.render(b, w, h, f, t));
}

#[test]
fn simple_tool_panels_render() {
    let mut fm = crate::tools::file_manager::FileManager::new();
    fm.visible = true;
    each_theme("files", |b, w, h, f, t| fm.render(b, w, h, f, t));
    let mut pt = crate::tools::process_tree::ProcessTree::new();
    pt.toggle();
    each_theme("proctree", |b, w, h, f, t| pt.render(b, w, h, f, t));
    let mut nm = crate::tools::network_monitor::NetworkMonitor::new();
    nm.toggle();
    each_theme("netmon", |b, w, h, f, t| nm.render(b, w, h, f, t));
    let mut si = crate::tools::system_info::SystemInfo::new();
    si.toggle();
    each_theme("sysinfo", |b, w, h, f, t| si.render(b, w, h, f, t));
    let mut pd = crate::tools::port_dashboard::PortDashboard::new();
    pd.toggle();
    each_theme("ports", |b, w, h, f, t| pd.render(b, w, h, f, t));
}

#[test]
fn regex_playground_renders() {
    let mut r = crate::tools::regex_playground::RegexPlayground::new();
    r.visible = true;
    r.pattern = "(\\w+)@(\\w+)\\.com".into();
    r.test_text = "contact: alice@example.com, bob@test.com\nnothing here\ncarol@site.com".into();
    r.recompile();
    each_theme("regex", |b, w, h, f, t| r.render(b, w, h, f, t));
    r.pattern = "(unclosed".into();
    r.recompile();
    each_theme("regex-error", |b, w, h, f, t| r.render(b, w, h, f, t));
}

#[test]
fn exec_preview_and_error_notification_render() {
    let mut p = crate::tools::exec_preview::ExecPreview::check_command("sudo rm -rf /var/tmp/build")
        .expect("dangerous command previews");
    p.visible = true;
    each_theme("exec-preview", |b, w, h, f, t| p.render(b, w, h, f, t));
    let mut n = crate::tools::error_detect::ErrorNotification::new();
    n.show(
        crate::tools::error_detect::DetectedError {
            error_type: "rust_panic".into(),
            message: "thread 'main' panicked at src/main.rs:42:5: index out of bounds".into(),
            context: vec![],
            line_number: 1,
        },
        5,
    );
    n.set_diagnosis("Index past the end of a Vec.\nCheck the length before indexing.".into());
    each_theme("error-notif", |b, w, h, f, t| n.render(b, w, h, f, t));
}

#[test]
fn history_search_renders() {
    let mut hs = crate::tools::history::HistorySearch::new();
    hs.visible = true;
    for (i, c) in ["git commit -m \"fix: overlay padding\"", "cargo test --bin rift", "docker compose up -d", "ssh dev-server"].iter().enumerate() {
        hs.entries.push(crate::tools::history::HistoryEntry {
            command: c.to_string(),
            timestamp: 1_700_000_000 + i as u64 * 1000,
            frequency: 1 + i as u32,
            directory: String::new(),
            exit_code: if i == 1 { Some(1) } else { None },
        });
    }
    hs.filtered = vec![0, 1, 2, 3];
    hs.query = "c".into();
    each_theme("history", |b, w, h, f, t| hs.render(b, w, h, f, t));
}

#[test]
fn observer_summary_renders() {
    let summary = "## Session\n  commands run: 42\n  errors: 3\n## Patterns\n  frequent: cargo build\nplain line";
    each_theme("observer", |b, w, h, f, t| crate::ui::observer_summary::render(b, w, h, f, t, summary, true));
}
