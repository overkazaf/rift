use std::collections::HashMap;

use muda::{
    accelerator::Accelerator, AboutMetadata, Menu, MenuEvent, MenuItem, MenuId,
    PredefinedMenuItem, Submenu,
};

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub enum MenuAction {
    NewTab,
    CloseTab,
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
    PixelateEffect,
    ThermalEffect,
    RaindropEffect,
    VhsEffect,
    GridEffect,
    FilmGrainEffect,
    InvertEffect,
    DesaturateEffect,
    ChromaticEffect,
    PulseEffect,
    SnowEffect,
    UnderwaterEffect,
    NeonOutlineEffect,
    ScanlineRgbEffect,
    NoEffect,
    Preferences,
    Welcome,
    WebView,
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
    TimeWarp,
    HudToggle,
    BroadcastToggle,
    Find,
    CompareOutput,
}

pub struct AppMenuBar {
    menu: Menu,
    actions: HashMap<MenuId, MenuAction>,
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
        let ssh = MenuItem::new("SSH Connect...", true, accel("CmdOrCtrl+Shift+S"));
        let rec = MenuItem::new("Toggle Recording", true, accel("CmdOrCtrl+Shift+R"));

        actions.insert(new_tab.id().clone(), MenuAction::NewTab);
        actions.insert(close_tab.id().clone(), MenuAction::CloseTab);
        actions.insert(ssh.id().clone(), MenuAction::SshConnect);
        actions.insert(rec.id().clone(), MenuAction::Recording);

        let _ = file_menu.append_items(&[
            &new_tab,
            &close_tab,
            &PredefinedMenuItem::separator(),
            &ssh,
            &PredefinedMenuItem::separator(),
            &rec,
        ]);

        // ── Edit menu ──
        let edit_menu = Submenu::new("Edit", true);
        let find = MenuItem::new("Find...", true, accel("CmdOrCtrl+F"));
        actions.insert(find.id().clone(), MenuAction::Find);

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
        ]);

        // ── View menu ──
        let view_menu = Submenu::new("View", true);
        let fullscreen = MenuItem::new("Toggle Full Screen", true, accel("Ctrl+CmdOrCtrl+F"));
        let zoom_in = MenuItem::new("Zoom In", true, accel("CmdOrCtrl+="));
        let zoom_out = MenuItem::new("Zoom Out", true, accel("Ctrl+-"));
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
            (MenuItem::new("NeonGlow", true, None::<Accelerator>), MenuAction::NeonEffect),
            (MenuItem::new("MatrixRain", true, None::<Accelerator>), MenuAction::MatrixEffect),
            (MenuItem::new("Amber", true, None::<Accelerator>), MenuAction::AmberEffect),
            (MenuItem::new("Hologram", true, None::<Accelerator>), MenuAction::HologramEffect),
            (MenuItem::new("Pixelate", true, None::<Accelerator>), MenuAction::PixelateEffect),
            (MenuItem::new("Thermal", true, None::<Accelerator>), MenuAction::ThermalEffect),
            (MenuItem::new("Raindrop", true, None::<Accelerator>), MenuAction::RaindropEffect),
            (MenuItem::new("VHS Tape", true, None::<Accelerator>), MenuAction::VhsEffect),
            (MenuItem::new("Cyberpunk Grid", true, None::<Accelerator>), MenuAction::GridEffect),
            (MenuItem::new("Film Grain", true, None::<Accelerator>), MenuAction::FilmGrainEffect),
            (MenuItem::new("Invert", true, None::<Accelerator>), MenuAction::InvertEffect),
            (MenuItem::new("Desaturate", true, None::<Accelerator>), MenuAction::DesaturateEffect),
            (MenuItem::new("Chromatic Shift", true, None::<Accelerator>), MenuAction::ChromaticEffect),
            (MenuItem::new("Pulse", true, None::<Accelerator>), MenuAction::PulseEffect),
            (MenuItem::new("Snow", true, None::<Accelerator>), MenuAction::SnowEffect),
            (MenuItem::new("Underwater", true, None::<Accelerator>), MenuAction::UnderwaterEffect),
            (MenuItem::new("Neon Outline", true, None::<Accelerator>), MenuAction::NeonOutlineEffect),
            (MenuItem::new("Scanline RGB", true, None::<Accelerator>), MenuAction::ScanlineRgbEffect),
        ];
        for (item, act) in &items_fx {
            actions.insert(item.id().clone(), *act);
            let _ = effects_menu.append(item);
        }
        let no_fx = MenuItem::new("No Effect", true, None::<Accelerator>);
        actions.insert(no_fx.id().clone(), MenuAction::NoEffect);
        let _ = effects_menu.append(&PredefinedMenuItem::separator());
        let _ = effects_menu.append(&no_fx);

        let webview_item = MenuItem::new("Toggle WebView", true, accel("CmdOrCtrl+Shift+B"));
        actions.insert(webview_item.id().clone(), MenuAction::WebView);

        let _ = view_menu.append_items(&[
            &fullscreen,
            &PredefinedMenuItem::separator(),
            &zoom_in,
            &zoom_out,
            &zoom_reset,
            &PredefinedMenuItem::separator(),
            &effects_menu,
            &PredefinedMenuItem::separator(),
            &webview_item,
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

        let _ = tools_menu.append_items(&[
            &fm, &git, &docker, &cicd,
            &PredefinedMenuItem::separator(),
            &netmon, &proctree, &sysinfo, &portdash,
            &PredefinedMenuItem::separator(),
            &regex_play,
            &PredefinedMenuItem::separator(),
            &heatmap, &secret, &audit, &teach,
        ]);

        // ── AI menu ──
        let ai_menu = Submenu::new("AI", true);
        let ai_assist = MenuItem::new("AI Assistant", true, accel("CmdOrCtrl+Shift+A"));
        let observer = MenuItem::new("Observer Mode", true, None::<Accelerator>);
        let advisor = MenuItem::new("Advisor Mode", true, None::<Accelerator>);
        actions.insert(ai_assist.id().clone(), MenuAction::AiAssistant);
        actions.insert(observer.id().clone(), MenuAction::ObserverMode);
        actions.insert(advisor.id().clone(), MenuAction::AdvisorMode);
        let _ = ai_menu.append_items(&[&ai_assist, &observer, &advisor]);

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
        let _ = help_menu.append_items(&[&welcome]);

        // ── Assemble menu bar ──
        let _ = menu.append_items(&[
            &app_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &term_menu,
            &tools_menu,
            &ai_menu,
            &window_menu,
            &help_menu,
        ]);

        Self { menu, actions }
    }

    pub fn init_for_nsapp(&self) {
        #[cfg(target_os = "macos")]
        {
            self.menu.init_for_nsapp();
        }
    }

    pub fn poll_event(&self) -> Option<MenuAction> {
        if let Ok(event) = MenuEvent::receiver().try_recv() {
            self.actions.get(event.id()).copied()
        } else {
            None
        }
    }
}

fn accel(s: &str) -> Option<Accelerator> {
    s.parse().ok()
}
