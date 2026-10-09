use std::collections::HashMap;

use crate::window::tab::{Direction, PaneCmd};
use muda::{
    accelerator::Accelerator, AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuItem, MenuId,
    PredefinedMenuItem, Submenu,
};

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub enum MenuAction {
    NewTab,
    CloseTab,
    /// File > New Window: another window with its own tabs and panes.
    NewWindow,
    /// File > Close Window (the last window quits the app).
    CloseWindow,
    SshConnect,
    ToggleFullScreen,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    SplitH,
    SplitV,
    Recording,
    CrtEffect,
    GlitchEffect,
    NeonEffect,
    MatrixEffect,
    AmberEffect,
    HologramEffect,
    NoEffect,
    Preferences,
    Welcome,
    UiGallery,
    WebView,
    Browser(crate::network::browser::BrowserCmd),
    // New actions
    FileManager,
    GitPanel,
    DockerPanel,
    CicdPanel,
    NetworkMonitor,
    ProcessTree,
    SystemInfo,
    PortDashboard,
    RegexPlayground,
    Heatmap,
    SecretMask,
    AuditLog,
    TeachingMode,
    AiAssistant,
    ObserverMode,
    AdvisorMode,
    /// Cmd+K: ask about the selection / block / screen.
    AskAboutThis,
    AutoFixToggle,
    NaturalLanguageToggle,
    TimeWarp,
    HudToggle,
    BroadcastToggle,
    Find,
    ClearBuffer,
    CompareOutput,
    /// Change Review: open the diff since the last checkpoint.
    ReviewChanges,
    /// Change Review: snapshot the repo of the active pane.
    ReviewMark,
    /// Toggle the Agent Mission Control dock.
    AgentMissionControl,
    AgentNextAttention,
    /// "New Agent..." (opens the palette in agent mode).
    AgentNew,
    AgentLayout2x2,
    Pane(PaneCmd),
}

pub struct AppMenuBar {
    menu: Menu,
    actions: HashMap<MenuId, MenuAction>,
    ai_auto_fix: CheckMenuItem,
    ai_nl_hash: CheckMenuItem,
    /// Effects submenu items, so GPU-only ones can be disabled without a GPU.
    fx_items: Vec<(MenuItem, crate::effects::EffectKind)>,
    /// Menu clicks forwarded by the muda event handler (which also wakes the
    /// event loop, so the UI thread needs no polling timer).
    events: std::sync::mpsc::Receiver<MenuEvent>,
}

impl AppMenuBar {
    pub fn new() -> Self {
        let menu = Menu::new();
        let mut actions = HashMap::new();

        // ── App menu (macOS) ──
        let app_menu = Submenu::new("rift", true);
        let about = PredefinedMenuItem::about(
            Some("About rift"),
            Some(AboutMetadata {
                name: Some("Rift".into()),
                version: Some(crate::config::VERSION.into()),
                short_version: Some(crate::config::VERSION.into()),
                comments: None,
                copyright: Some("\u{00a9} 2024-2026 overkazaf. MIT License.".into()),
                license: Some("MIT".into()),
                website: Some("https://github.com/overkazaf/rift".into()),
                website_label: Some("GitHub".into()),
                authors: Some(vec!["overkazaf".into()]),
                credits: Some("Author: overkazaf\nGitHub: github.com/overkazaf/rift\nSupport: ko-fi.com/john5555555555".into()),
                icon: None,
            }),
        );
        let prefs = MenuItem::new("Preferences...", true, accel("CmdOrCtrl+,"));
        actions.insert(prefs.id().clone(), MenuAction::Preferences);

        let _ = app_menu.append_items(&[
            &about,
            &prefs,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Hide rift")),
            &PredefinedMenuItem::hide_others(Some("Hide Others")),
            &PredefinedMenuItem::show_all(Some("Show All")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(Some("Quit rift")),
        ]);

        // ── File menu ──
        let file_menu = Submenu::new("File", true);
        let new_tab = MenuItem::new("New Tab", true, accel("CmdOrCtrl+Shift+T"));
        let close_tab = MenuItem::new("Close Tab", true, accel("CmdOrCtrl+Shift+W"));
        // Shortcut shown in the label only: Cmd+N is also the chat composer's
        // "new chat" while it has focus, so the OS menu must not swallow it.
        let new_window = MenuItem::new(if cfg!(target_os = "macos") { "New Window  \u{2318}N" } else { "New Window" }, true, None::<Accelerator>);
        let close_window = MenuItem::new(if cfg!(target_os = "macos") { "Close Window  \u{2325}\u{2318}W" } else { "Close Window" }, true, None::<Accelerator>);
        let ssh = MenuItem::new("SSH Connect...", true, accel("CmdOrCtrl+Shift+S"));
        let rec = MenuItem::new("Toggle Recording", true, accel("CmdOrCtrl+Shift+R"));

        actions.insert(new_tab.id().clone(), MenuAction::NewTab);
        actions.insert(close_tab.id().clone(), MenuAction::CloseTab);
        actions.insert(new_window.id().clone(), MenuAction::NewWindow);
        actions.insert(close_window.id().clone(), MenuAction::CloseWindow);
        actions.insert(ssh.id().clone(), MenuAction::SshConnect);
        actions.insert(rec.id().clone(), MenuAction::Recording);

        let _ = file_menu.append_items(&[
            &new_window,
            &new_tab,
            &PredefinedMenuItem::separator(),
            &close_tab,
            &close_window,
            &PredefinedMenuItem::separator(),
            &ssh,
            &PredefinedMenuItem::separator(),
            &rec,
        ]);

        // ── Edit menu ──
        let edit_menu = Submenu::new("Edit", true);
        let find = MenuItem::new("Find...", true, accel("CmdOrCtrl+F"));
        actions.insert(find.id().clone(), MenuAction::Find);
        let clear_buffer = MenuItem::new("Clear Buffer", true, accel_mac("CmdOrCtrl+Alt+K"));
        actions.insert(clear_buffer.id().clone(), MenuAction::ClearBuffer);

        let _ = edit_menu.append_items(&[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
            &PredefinedMenuItem::separator(),
            &find,
            &clear_buffer,
        ]);

        // ── View menu ──
        let view_menu = Submenu::new("View", true);
        let fullscreen = MenuItem::new("Toggle Full Screen", true, accel("Ctrl+CmdOrCtrl+F"));
        let zoom_in = MenuItem::new("Zoom In", true, accel("CmdOrCtrl+="));
        let zoom_out = MenuItem::new("Zoom Out", true, accel("CmdOrCtrl+-"));
        let zoom_reset = MenuItem::new("Reset Zoom", true, accel("CmdOrCtrl+0"));

        actions.insert(fullscreen.id().clone(), MenuAction::ToggleFullScreen);
        actions.insert(zoom_in.id().clone(), MenuAction::ZoomIn);
        actions.insert(zoom_out.id().clone(), MenuAction::ZoomOut);
        actions.insert(zoom_reset.id().clone(), MenuAction::ZoomReset);

        // Effects submenu
        let effects_menu = Submenu::new("Effects", true);
        let items_fx: Vec<(MenuItem, MenuAction)> = vec![
            (MenuItem::new("CRT", true, None::<Accelerator>), MenuAction::CrtEffect),
            (MenuItem::new("Glitch", true, None::<Accelerator>), MenuAction::GlitchEffect),
            (MenuItem::new("Neon Glow", true, None::<Accelerator>), MenuAction::NeonEffect),
            (MenuItem::new("Matrix Rain", true, None::<Accelerator>), MenuAction::MatrixEffect),
            (MenuItem::new("Amber", true, None::<Accelerator>), MenuAction::AmberEffect),
            (MenuItem::new("Hologram", true, None::<Accelerator>), MenuAction::HologramEffect),
        ];
        let mut fx_items = Vec::new();
        for (item, act) in items_fx {
            actions.insert(item.id().clone(), act);
            let _ = effects_menu.append(&item);
            let kind = match act {
                MenuAction::CrtEffect => crate::effects::EffectKind::Crt,
                MenuAction::GlitchEffect => crate::effects::EffectKind::Glitch,
                MenuAction::NeonEffect => crate::effects::EffectKind::Neon,
                MenuAction::MatrixEffect => crate::effects::EffectKind::Matrix,
                MenuAction::AmberEffect => crate::effects::EffectKind::Amber,
                _ => crate::effects::EffectKind::Hologram,
            };
            fx_items.push((item, kind));
        }
        let no_fx = MenuItem::new("No Effect", true, None::<Accelerator>);
        actions.insert(no_fx.id().clone(), MenuAction::NoEffect);
        let _ = effects_menu.append(&PredefinedMenuItem::separator());
        let _ = effects_menu.append(&no_fx);

        // Panes submenu. Shortcuts are shown in the label text only: menu
        // accelerators would be swallowed by macOS before the key handler.
        let panes_menu = Submenu::new("Panes", true);
        let pane_items: Vec<(&str, PaneCmd)> = vec![
            ("Split Right  \u{2318}D", PaneCmd::SplitRight),
            ("Split Down  \u{21e7}\u{2318}D", PaneCmd::SplitDown),
            ("Close Pane  \u{2318}W", PaneCmd::ClosePane),
            ("Zoom Pane  \u{21e7}\u{2318}\u{21a9}", PaneCmd::Zoom),
            ("Equalize Panes  \u{2303}\u{2318}=", PaneCmd::Equalize),
            ("Next Pane  \u{2318}]", PaneCmd::FocusNext),
            ("Previous Pane  \u{2318}[", PaneCmd::FocusPrev),
            ("Focus Left  \u{2325}\u{2318}\u{2190}", PaneCmd::Focus(Direction::Left)),
            ("Focus Right  \u{2325}\u{2318}\u{2192}", PaneCmd::Focus(Direction::Right)),
            ("Focus Up  \u{2325}\u{2318}\u{2191}", PaneCmd::Focus(Direction::Up)),
            ("Focus Down  \u{2325}\u{2318}\u{2193}", PaneCmd::Focus(Direction::Down)),
            ("Swap Left  \u{21e7}\u{2303}\u{2318}\u{2190}", PaneCmd::Swap(Direction::Left)),
            ("Swap Right  \u{21e7}\u{2303}\u{2318}\u{2192}", PaneCmd::Swap(Direction::Right)),
            ("Swap Up  \u{21e7}\u{2303}\u{2318}\u{2191}", PaneCmd::Swap(Direction::Up)),
            ("Swap Down  \u{21e7}\u{2303}\u{2318}\u{2193}", PaneCmd::Swap(Direction::Down)),
        ];
        for (i, (label, cmd)) in pane_items.iter().enumerate() {
            // Separators between: splits | zoom/equalize | cycle | focus | swap
            if matches!(i, 3 | 5 | 7 | 11) {
                let _ = panes_menu.append(&PredefinedMenuItem::separator());
            }
            let item = MenuItem::new(*label, true, None::<Accelerator>);
            actions.insert(item.id().clone(), MenuAction::Pane(*cmd));
            let _ = panes_menu.append(&item);
        }

        let webview_item = MenuItem::new("Toggle WebView", true, accel("CmdOrCtrl+Shift+B"));
        actions.insert(webview_item.id().clone(), MenuAction::WebView);
        use crate::network::browser::BrowserCmd as Bc;
        let br_back = MenuItem::new("Browser: Back", true, accel_mac("CmdOrCtrl+["));
        let br_fwd = MenuItem::new("Browser: Forward", true, accel_mac("CmdOrCtrl+]"));
        let br_reload = MenuItem::new("Browser: Reload", true, accel_mac("CmdOrCtrl+R"));
        let br_addr = MenuItem::new("Browser: Focus Address Bar", true, accel_mac("CmdOrCtrl+L"));
        let br_close = MenuItem::new("Browser: Close", true, accel_mac("CmdOrCtrl+W"));
        for (item, cmd) in [
            (&br_back, Bc::Back),
            (&br_fwd, Bc::Forward),
            (&br_reload, Bc::Reload),
            (&br_addr, Bc::FocusAddress),
            (&br_close, Bc::Close),
        ] {
            actions.insert(item.id().clone(), MenuAction::Browser(cmd));
        }

        let _ = view_menu.append_items(&[
            &fullscreen,
            &PredefinedMenuItem::separator(),
            &zoom_in,
            &zoom_out,
            &zoom_reset,
            &PredefinedMenuItem::separator(),
            &effects_menu,
            &panes_menu,
            &PredefinedMenuItem::separator(),
            &webview_item,
            &br_back,
            &br_fwd,
            &br_reload,
            &br_addr,
            &br_close,
        ]);

        // ── Terminal menu ──
        let term_menu = Submenu::new("Terminal", true);
        let split_h = MenuItem::new("Split Right", true, accel("CmdOrCtrl+D"));
        let split_v = MenuItem::new("Split Down", true, None);
        let hud = MenuItem::new("Toggle HUD", true, accel("CmdOrCtrl+Shift+H"));
        let timewarp = MenuItem::new("Time Warp", true, None::<Accelerator>);
        let broadcast = MenuItem::new("Toggle Broadcast", true, accel("CmdOrCtrl+Shift+P"));
        let compare = MenuItem::new("Compare Output", true, accel("CmdOrCtrl+Shift+K"));

        actions.insert(split_h.id().clone(), MenuAction::SplitH);
        actions.insert(split_v.id().clone(), MenuAction::SplitV);
        actions.insert(hud.id().clone(), MenuAction::HudToggle);
        actions.insert(timewarp.id().clone(), MenuAction::TimeWarp);
        actions.insert(broadcast.id().clone(), MenuAction::BroadcastToggle);
        actions.insert(compare.id().clone(), MenuAction::CompareOutput);

        let _ = term_menu.append_items(&[
            &split_h,
            &split_v,
            &PredefinedMenuItem::separator(),
            &hud,
            &timewarp,
            &broadcast,
            &compare,
        ]);

        // ── Tools menu ──
        let tools_menu = Submenu::new("Tools", true);
        let fm = MenuItem::new("File Manager", true, accel("CmdOrCtrl+Shift+E"));
        let git = MenuItem::new("Git Panel", true, accel("CmdOrCtrl+Shift+G"));
        let docker = MenuItem::new("Docker Panel", true, accel("CmdOrCtrl+Shift+O"));
        let cicd = MenuItem::new("CI/CD Panel", true, accel("CmdOrCtrl+Shift+I"));
        let netmon = MenuItem::new("Network Monitor", true, None::<Accelerator>);
        let proctree = MenuItem::new("Process Tree", true, None::<Accelerator>);
        let sysinfo = MenuItem::new("System Info", true, None::<Accelerator>);
        let portdash = MenuItem::new("Port Dashboard", true, None::<Accelerator>);
        let regex_play = MenuItem::new("Regex Playground", true, accel("CmdOrCtrl+Shift+X"));
        let heatmap = MenuItem::new("Command Heatmap", true, accel("CmdOrCtrl+Shift+Y"));
        let secret = MenuItem::new("Secret Masking", true, accel("CmdOrCtrl+Shift+M"));
        let audit = MenuItem::new("Audit Log", true, accel("CmdOrCtrl+Shift+U"));
        let teach = MenuItem::new("Teaching Mode", true, accel("CmdOrCtrl+Shift+L"));
        let review = MenuItem::new("Review: Changes Since Checkpoint", true, accel("CmdOrCtrl+Shift+J"));
        let review_mark = MenuItem::new("Review: Mark Checkpoint", true, None::<Accelerator>);

        actions.insert(fm.id().clone(), MenuAction::FileManager);
        actions.insert(git.id().clone(), MenuAction::GitPanel);
        actions.insert(docker.id().clone(), MenuAction::DockerPanel);
        actions.insert(cicd.id().clone(), MenuAction::CicdPanel);
        actions.insert(netmon.id().clone(), MenuAction::NetworkMonitor);
        actions.insert(proctree.id().clone(), MenuAction::ProcessTree);
        actions.insert(sysinfo.id().clone(), MenuAction::SystemInfo);
        actions.insert(portdash.id().clone(), MenuAction::PortDashboard);
        actions.insert(regex_play.id().clone(), MenuAction::RegexPlayground);
        actions.insert(heatmap.id().clone(), MenuAction::Heatmap);
        actions.insert(secret.id().clone(), MenuAction::SecretMask);
        actions.insert(audit.id().clone(), MenuAction::AuditLog);
        actions.insert(teach.id().clone(), MenuAction::TeachingMode);
        actions.insert(review.id().clone(), MenuAction::ReviewChanges);
        actions.insert(review_mark.id().clone(), MenuAction::ReviewMark);

        let _ = tools_menu.append_items(&[
            &fm, &git, &docker, &cicd,
            &PredefinedMenuItem::separator(),
            &netmon, &proctree, &sysinfo, &portdash,
            &PredefinedMenuItem::separator(),
            &regex_play,
            &PredefinedMenuItem::separator(),
            &heatmap, &secret, &audit, &teach,
            &PredefinedMenuItem::separator(),
            &review, &review_mark,
        ]);

        // ── AI menu ──
        let ai_menu = Submenu::new("AI", true);
        let ai_assist = MenuItem::new("AI Assistant", true, accel("CmdOrCtrl+Shift+A"));
        let observer = MenuItem::new("Observer Mode", true, None::<Accelerator>);
        let advisor = MenuItem::new("Advisor Mode", true, None::<Accelerator>);
        actions.insert(ai_assist.id().clone(), MenuAction::AiAssistant);
        actions.insert(observer.id().clone(), MenuAction::ObserverMode);
        actions.insert(advisor.id().clone(), MenuAction::AdvisorMode);
        let ask_this = MenuItem::new("Ask AI About This", true, accel_mac("CmdOrCtrl+K"));
        let ai_auto_fix = CheckMenuItem::new("Auto Fix Suggestions", true, true, None::<Accelerator>);
        let ai_nl_hash = CheckMenuItem::new("# Natural Language", true, true, None::<Accelerator>);
        actions.insert(ask_this.id().clone(), MenuAction::AskAboutThis);
        actions.insert(ai_auto_fix.id().clone(), MenuAction::AutoFixToggle);
        actions.insert(ai_nl_hash.id().clone(), MenuAction::NaturalLanguageToggle);
        let _ = ai_menu.append_items(&[
            &ai_assist,
            &ask_this,
            &PredefinedMenuItem::separator(),
            &ai_auto_fix,
            &ai_nl_hash,
            &PredefinedMenuItem::separator(),
            &observer,
            &advisor,
        ]);

        // ── Agents menu ──
        let agents_menu = Submenu::new("Agents", true);
        let mc = MenuItem::new("Mission Control", true, accel("CmdOrCtrl+Shift+;"));
        let next_att = MenuItem::new("Next Agent Needing Attention", true, accel("CmdOrCtrl+Shift+."));
        let new_agent = MenuItem::new("New Agent...", true, None::<Accelerator>);
        let layout22 = MenuItem::new("Agent Layout: 2\u{d7}2", true, None::<Accelerator>);
        actions.insert(mc.id().clone(), MenuAction::AgentMissionControl);
        actions.insert(next_att.id().clone(), MenuAction::AgentNextAttention);
        actions.insert(new_agent.id().clone(), MenuAction::AgentNew);
        actions.insert(layout22.id().clone(), MenuAction::AgentLayout2x2);
        let _ = agents_menu.append_items(&[&mc, &next_att, &PredefinedMenuItem::separator(), &new_agent, &layout22]);

        // ── Window menu ──
        let window_menu = Submenu::new("Window", true);
        let _ = window_menu.append_items(&[
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::maximize(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::fullscreen(Some("Enter Full Screen")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::bring_all_to_front(None),
        ]);

        // ── Help menu ──
        let help_menu = Submenu::new("Help", true);
        let welcome = MenuItem::new("Welcome Guide", true, None::<Accelerator>);
        actions.insert(welcome.id().clone(), MenuAction::Welcome);
        let gallery = MenuItem::new("UI Gallery", true, None::<Accelerator>);
        actions.insert(gallery.id().clone(), MenuAction::UiGallery);
        let _ = help_menu.append_items(&[&welcome, &gallery]);

        // ── Assemble menu bar ──
        let _ = menu.append_items(&[
            &app_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &term_menu,
            &tools_menu,
            &ai_menu,
            &agents_menu,
            &window_menu,
            &help_menu,
        ]);

        let (tx, events) = std::sync::mpsc::channel();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = tx.send(e);
            crate::wake::wake();
        }));
        Self { menu, actions, ai_auto_fix, ai_nl_hash, fx_items, events }
    }

    /// Sync the AI toggle check marks with the config (muda flips a check
    /// item on click; this makes the config the single source of truth).
    pub fn set_ai_checks(&self, auto_fix: bool, nl_hash: bool) {
        self.ai_auto_fix.set_checked(auto_fix);
        self.ai_nl_hash.set_checked(nl_hash);
    }

    /// Enable / disable the effects the current renderer can draw. Without a
    /// GPU only the CPU-capable ones (CRT, Amber, Hologram) stay enabled.
    pub fn set_gpu_effects(&self, gpu: bool) {
        for (item, kind) in &self.fx_items {
            item.set_enabled(gpu || kind.cpu_capable());
        }
    }

    pub fn init_for_nsapp(&self) {
        #[cfg(target_os = "macos")]
        {
            self.menu.init_for_nsapp();
        }
    }

    pub fn poll_event(&self) -> Option<MenuAction> {
        if let Ok(event) = self.events.try_recv() {
            self.actions.get(event.id()).copied()
        } else {
            None
        }
    }
}

fn accel(s: &str) -> Option<Accelerator> {
    s.parse().ok()
}

/// Accelerator only on macOS, where Cmd+<letter> doesn't collide with shell
/// control keys (Ctrl+R history, Ctrl+W, Ctrl+L, Ctrl+[ = ESC).
fn accel_mac(s: &str) -> Option<Accelerator> {
    if cfg!(target_os = "macos") { accel(s) } else { None }
}
