//! Item 11: malformed config.toml, missing fonts, unknown theme; critic H5 (config save).
//! Needs a sandbox HOME:  HOME=$(mktemp -d -t rift-audit) cargo test --bin rift audit::cfg
use super::{pane, Soft};
use crate::config::toml::{config_to_toml, load_config};

fn home_guard() -> Option<String> {
    super::sandbox_home("config")
}
fn write_cfg(home: &str, content: &[u8]) {
    let d = format!("{home}/.config/rift");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(format!("{d}/config.toml"), content).unwrap();
}
fn catch<R>(f: impl FnOnce() -> R + std::panic::UnwindSafe) -> Result<R, String> {
    std::panic::catch_unwind(f).map_err(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "panic".into()))
}

/// Full startup path used by main(): load -> resolve font -> Renderer::new -> pane -> feed -> render.
fn startup(home: &str, content: &[u8]) -> (Result<String, String>, Option<crate::config::Config>) {
    write_cfg(home, content);
    let cfg = match catch(load_config) {
        Ok(c) => c,
        Err(e) => return (Err(format!("load_config panicked: {e}")), None),
    };
    let summary = format!("font_size={} cols={} rows={} opacity={} theme={} font_path={:?} font_family={:?} llm.enabled={}", cfg.font_size, cfg.cols, cfg.rows, cfg.opacity, cfg.theme_name, cfg.font_path, cfg.font_family, cfg.llm.enabled);
    let r = catch(move || {
        let c2 = load_config();
        let font = crate::config::resolve_font_path(&c2);
        let mut r = crate::renderer::Renderer::new(&font, c2.font_size, c2.theme.clone());
        let mut wm = crate::window::WindowManager::headless(c2.cols as usize, c2.rows as usize);
        wm.active_pane_mut().feed(b"hello \xe4\xb8\xad\r\nworld\r\n");
        let (cw, ch) = (r.cell_width(), r.cell_height());
        let (w, h) = ((c2.cols as usize).min(300) * cw, (c2.rows as usize).min(100) * ch);
        if w * h > 0 && w * h < 200_000_000 {
            let mut buf = vec![0u32; w * h];
            let b = crate::tools::blocks::BlockManager::new();
            r.render_tabbed(&wm, crate::window::PaneRect { x: 0, y: 0, width: w, height: h }, &mut buf, w as u32, h as u32, &b);
        }
        format!("cell={cw}x{ch}")
    });
    (r.map(|x| format!("{summary}; {x}")).map_err(|e| format!("{summary}; startup panicked: {e}")), Some(cfg))
}

#[test]
fn malformed_and_hostile_configs() {
    let Some(home) = home_guard() else { return };
    let mut s = Soft::new("config");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", b"".to_vec()),
        ("binary_garbage", (0..4096u32).map(|i| (i * 7919 % 251) as u8).collect()),
        ("unterminated_string", b"[general]\ntheme = \"nord\nfont_size = 14\n".to_vec()),
        ("unclosed_section", b"[general\nfont_size = 22\n".to_vec()),
        ("font_size_string", b"[general]\nfont_size = \"abc\"\n".to_vec()),
        ("font_size_zero", b"[general]\nfont_size = 0\n".to_vec()),
        ("font_size_negative", b"[general]\nfont_size = -5\n".to_vec()),
        ("font_size_tiny_0.1", b"[general]\nfont_size = 0.1\n".to_vec()),
        ("font_size_huge_500", b"[general]\nfont_size = 500\n".to_vec()),
        ("cols_zero", b"[general]\ncols = 0\n".to_vec()),
        ("rows_zero", b"[general]\nrows = 0\n".to_vec()),
        ("cols_negative", b"[general]\ncols = -1\n".to_vec()),
        ("cols_70000", b"[general]\ncols = 70000\n".to_vec()),
        ("cols_rows_1", b"[general]\ncols = 1\nrows = 1\n".to_vec()),
        ("opacity_99", b"[general]\nopacity = 99\n".to_vec()),
        ("opacity_string", b"[general]\nopacity = \"x\"\n".to_vec()),
        ("unknown_theme", b"[general]\ntheme = \"no-such-theme\"\n".to_vec()),
        ("font_path_missing", b"[general]\nfont_path = \"/nonexistent/font.ttf\"\n".to_vec()),
        ("font_family_missing", b"[general]\nfont_family = \"NoSuchFontFamilyXYZ\"\n".to_vec()),
        ("font_path_not_a_font", b"[general]\nfont_path = \"/etc/hosts\"\n".to_vec()),
        ("font_path_directory", b"[general]\nfont_path = \"/tmp\"\n".to_vec()),
        ("bad_custom_colors", b"[theme.custom]\nfg = [999, 0, 0]\nbg = [1, 2]\ncursor = \"red\"\n".to_vec()),
        ("bad_effect", b"[general]\neffect = \"bogus\"\n".to_vec()),
        ("bom_prefix", b"\xef\xbb\xbf[general]\nfont_size = 22\ntheme = \"nord\"\n".to_vec()),
        ("crlf", b"[general]\r\nfont_size = 22\r\ntheme = \"nord\"\r\n".to_vec()),
        ("inline_comment", b"[general]\nfont_size = 22 # bigger\ntheme = \"nord\" # dark\n".to_vec()),
        ("single_quoted", b"[general]\ntheme = 'nord'\nfont_size = 22\n".to_vec()),
        ("dotted_key_table_syntax", b"general.font_size = 22\n[general]\ntheme = \"nord\"\n".to_vec()),
        ("string_with_equals_and_hash", b"[general]\nfont_path = \"/tmp/a=b#c.ttf\"\n".to_vec()),
        ("huge_line_10MB", { let mut v = b"[general]\nx = \"".to_vec(); v.extend(vec![b'a'; 10 << 20]); v.extend_from_slice(b"\"\n"); v }),
    ];
    for (name, content) in cases {
        let (r, cfg) = startup(&home, &content);
        let ok = r.is_ok();
        s.check(&format!("startup_survives_{name}"), ok, format!("{r:?}"));
        if let Some(c) = cfg {
            // semantic checks
            match name {
                "font_size_zero" | "font_size_negative" | "font_size_tiny_0.1" => s.check(&format!("font_size_sanitised_{name}"), c.font_size >= 6.0, format!("font_size={}", c.font_size)),
                "font_size_huge_500" => s.check("font_size_sanitised_huge", c.font_size <= 100.0, format!("font_size={}", c.font_size)),
                "cols_zero" | "rows_zero" | "cols_negative" | "cols_70000" => s.check(&format!("grid_dims_sanitised_{name}"), c.cols >= 2 && c.rows >= 2 && c.cols <= 1000, format!("cols={} rows={}", c.cols, c.rows)),
                "bom_prefix" => s.check("bom_file_parsed", c.font_size == 22.0 && c.theme_name == "nord", format!("font_size={} theme={}", c.font_size, c.theme_name)),
                "crlf" => s.check("crlf_file_parsed", c.font_size == 22.0 && c.theme_name == "nord", format!("font_size={} theme={:?}", c.font_size, c.theme_name)),
                "inline_comment" => s.check("inline_comments_supported", c.font_size == 22.0 && c.theme_name == "nord", format!("font_size={} theme={:?}", c.font_size, c.theme_name)),
                "single_quoted" => s.check("single_quoted_strings", c.theme_name == "nord", format!("{}", c.theme_name)),
                "string_with_equals_and_hash" => s.check("hash_inside_string_kept", c.font_path.as_deref() == Some("/tmp/a=b#c.ttf"), format!("{:?}", c.font_path)),
                "unknown_theme" => s.info("unknown_theme_fallback", format!("theme_name={} (warns via log::warn only; no user-visible message)", c.theme_name)),
                _ => {}
            }
        }
    }
    let _ = std::fs::remove_dir_all(format!("{home}/.config"));
    s.finish();
}

#[test]
fn config_save_roundtrip_h5() {
    let Some(home) = home_guard() else { return };
    let mut s = Soft::new("config");
    let original = "# my rift config\n# keep this comment\n[general]\nfont_size = 17.5\ntheme = \"nord\"\n\n[llm]\nprovider = \"openai\"\nmodel = \"deepseek-chat\"\napi_url = \"https://api.deepseek.com\"\napi_key = \"sk-secret-in-file\"\n";
    write_cfg(&home, original.as_bytes());
    let cfg = load_config();
    s.check("loaded_api_key", cfg.llm.api_key.as_deref() == Some("sk-secret-in-file"), format!("{:?}", cfg.llm.api_key));
    crate::config::toml::save_config(&cfg);
    let saved = std::fs::read_to_string(format!("{home}/.config/rift/config.toml")).unwrap();
    s.check("save_keeps_api_key", saved.contains("sk-secret-in-file"), format!("saved file:\n{saved}"));
    s.check("save_keeps_comments", saved.contains("keep this comment"), "comments are dropped by config_to_toml");
    let reload = load_config();
    s.check("reload_after_save_is_equivalent", reload.llm.api_key.is_some() && reload.llm.enabled, format!("api_key={:?} enabled={} (after quit the file no longer has the key -> AI silently off unless an env var is set)", reload.llm.api_key, reload.llm.enabled));
    // --config PATH
    let bin = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/rift");
    if std::path::Path::new(bin).exists() {
        let o = std::process::Command::new(bin).args(["--config", "/tmp/whatever.toml", "--version"]).output().unwrap();
        s.check("cli_dash_dash_config_path_accepted_as_documented_in_readme", o.status.success(), format!("`rift --config PATH --version` -> exit {:?} stderr={:?}", o.status.code(), String::from_utf8_lossy(&o.stderr)));
    }
    s.check("pane_survives", catch(|| { let mut p = pane(80, 24); p.feed(b"x"); }).is_ok(), "");
    let _ = config_to_toml(&cfg);
    let _ = std::fs::remove_dir_all(format!("{home}/.config"));
    s.finish();
}
