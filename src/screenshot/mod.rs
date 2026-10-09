//! Headless product-screenshot renderer.
//!
//! `rift --screenshot <scene|all> --out <dir> [--width 1600 --height 1000]
//! [--theme rift-neon] [--font-size PX]` renders marketing scenes **without a
//! window, GPU, shell or screen-recording permission**, using the real code
//! paths of the app: scripted panes fed through the real VT parser,
//! `Renderer::render_tabbed_with_cmd`, `blocks_ui::draw`, the chat sidebar,
//! inline AI bars, command palette, exec preview, HUD and browser chrome.
//! Only the macOS window frame and the backdrop around it are drawn here
//! (see [`frame`]).
//!
//! Nothing in this module opens a window or spawns a process.

mod frame;
mod mission;
mod page;
mod png;
mod scenes;
mod shell;
mod workflow;

use std::path::{Path, PathBuf};

use crate::config::Theme;

/// Parsed command line of `--screenshot`.
pub struct Options {
    pub scene: String,
    pub out: PathBuf,
    pub width: usize,
    pub height: usize,
    pub theme: Theme,
    /// Base font size in physical px for a 1000px-high image (scenes may
    /// scale it); `None` = 24.
    pub font_px: Option<f32>,
}

fn usage() {
    eprintln!("usage: rift --screenshot <scene|all> --out <dir> [--width 1600] [--height 1000] [--theme rift-neon] [--font-size 24]");
    eprintln!("scenes: {}", scenes::NAMES.join(", "));
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut scene = None;
    let mut out = None;
    let (mut width, mut height) = (1600usize, 1000usize);
    let mut theme = "rift-neon".to_string();
    let mut font_px = None;
    let mut i = 0;
    let next = |i: &mut usize, name: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| format!("{name} needs a value"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--screenshot" => scene = Some(next(&mut i, a)?),
            "--out" => out = Some(PathBuf::from(next(&mut i, a)?)),
            "--width" => width = next(&mut i, a)?.parse().map_err(|_| "bad --width".to_string())?,
            "--height" => height = next(&mut i, a)?.parse().map_err(|_| "bad --height".to_string())?,
            "--theme" => theme = next(&mut i, a)?,
            "--font-size" => font_px = Some(next(&mut i, a)?.parse().map_err(|_| "bad --font-size".to_string())?),
            other => return Err(format!("unknown option for --screenshot: {other}")),
        }
        i += 1;
    }
    if width < 640 || height < 400 || width > 8000 || height > 6000 {
        return Err("--width/--height out of range (640x400 .. 8000x6000)".into());
    }
    let theme = crate::config::Config::theme_by_name(&theme)
        .ok_or_else(|| format!("unknown theme '{theme}' (try: {})", crate::config::Config::available_themes().join(", ")))?;
    Ok(Options {
        scene: scene.ok_or("missing scene")?,
        out: out.ok_or("missing --out <dir>")?,
        width,
        height,
        theme,
        font_px,
    })
}

/// Entry point from `main`. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let _ = env_logger::try_init();
    let opts = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("rift: {e}");
            usage();
            return 2;
        }
    };
    let names: Vec<&str> = if opts.scene == "all" {
        scenes::NAMES.to_vec()
    } else if scenes::NAMES.contains(&opts.scene.as_str()) {
        vec![opts.scene.as_str()]
    } else {
        eprintln!("rift: unknown scene '{}'", opts.scene);
        usage();
        return 2;
    };
    if let Err(e) = std::fs::create_dir_all(&opts.out) {
        eprintln!("rift: cannot create {}: {e}", opts.out.display());
        return 1;
    }
    for name in names {
        match render_scene(name, &opts) {
            Ok(path) => println!("wrote {}", path.display()),
            Err(e) => {
                eprintln!("rift: scene '{name}' failed: {e}");
                return 1;
            }
        }
    }
    0
}

/// Preferred monospace font for screenshots: JetBrains Mono Nerd Font if
/// installed (powerline glyphs), otherwise whatever the app would pick.
fn font_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Regular.ttf"),
        format!("{home}/Library/Fonts/JetBrainsMonoNerdFont-Regular.ttf"),
        format!("{home}/.local/share/fonts/JetBrainsMonoNerdFontMono-Regular.ttf"),
    ];
    for c in candidates {
        if Path::new(&c).exists() {
            return c;
        }
    }
    crate::config::find_font_path()
}

pub fn render_scene(name: &str, opts: &Options) -> Result<PathBuf, String> {
    let geom = frame::FrameGeom::new(opts.width, opts.height);
    let (aw, ah) = geom.app_size();
    let spec = scenes::spec(name).ok_or("unknown scene")?;
    let base = opts.font_px.unwrap_or(24.0) * (opts.height as f32 / 1000.0);
    let mut stage = scenes::Stage::new(aw, ah, &font_path(), base * spec.font_mul, opts.theme.clone());
    log::info!("scene {name}: font {:.1}px, cell {}x{}, app {aw}x{ah}", base * spec.font_mul, stage.renderer.cell_width(), stage.renderer.cell_height());
    (spec.build)(&mut stage);
    let app = stage.render_final();

    let prop = frame::PropFont::load();
    let title = stage.window_title.clone();
    let mut font = std::mem::replace(&mut stage.renderer.font, crate::renderer::font::FontManager::new(&font_path(), 12.0));
    let mut mono_title = |buf: &mut [u32], w: usize, t: &str, th: usize, _ww: usize, fg: crate::config::Rgb| {
        let tw = t.chars().count() * font.cell_width;
        let y = th.saturating_sub(font.cell_height) / 2;
        crate::ui::render_text(buf, w, &mut font, t, w.saturating_sub(tw) / 2, y, fg);
    };
    let rgba = frame::compose(&geom, &opts.theme, &app, &title, prop.as_ref(), &mut mono_title);
    let png = png::encode_rgba(opts.width as u32, opts.height as u32, &rgba).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&opts.out).map_err(|e| format!("create {}: {e}", opts.out.display()))?;
    let path = opts.out.join(format!("{name}.png"));
    std::fs::write(&path, png).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_options_and_defaults() {
        let o = parse(&args(&["--screenshot", "hero", "--out", "x"])).unwrap();
        assert_eq!((o.scene.as_str(), o.width, o.height, o.theme.name), ("hero", 1600, 1000, "rift-neon"));
        let o = parse(&args(&["--screenshot", "all", "--out", "x", "--width", "800", "--height", "500", "--theme", "nord"])).unwrap();
        assert_eq!((o.width, o.height, o.theme.name), (800, 500, "nord"));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&args(&["--screenshot", "hero"])).is_err(), "--out is required");
        assert!(parse(&args(&["--out", "x"])).is_err(), "scene is required");
        assert!(parse(&args(&["--screenshot", "hero", "--out", "x", "--width", "10"])).is_err());
        assert!(parse(&args(&["--screenshot", "hero", "--out", "x", "--theme", "nope"])).is_err());
        assert!(parse(&args(&["--screenshot", "hero", "--out", "x", "--bogus"])).is_err());
    }

    #[test]
    fn every_scene_name_has_a_spec() {
        for n in scenes::NAMES {
            assert!(scenes::spec(n).is_some(), "{n}");
        }
        assert!(scenes::spec("nope").is_none());
    }

    /// End to end at a small size: scripted panes -> real renderer -> frame -> PNG.
    #[test]
    fn renders_scenes_to_png() {
        if !Path::new(&font_path()).exists() {
            return; // no monospace font on this machine
        }
        let dir = std::env::temp_dir().join(format!("rift-shot-test-{}", std::process::id()));
        for scene in ["blocks", "fix-suggestion", "cmdk", "nl-command", "palette", "preview-accept", "mission-control"] {
            let o = parse(&args(&["--screenshot", scene, "--out", dir.to_str().unwrap(), "--width", "800", "--height", "500"])).unwrap();
            let path = render_scene(scene, &o).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
            assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 800);
            assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 500);
            assert!(bytes.len() > 20_000, "{scene} looks blank ({} bytes)", bytes.len());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
