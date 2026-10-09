#[cfg(target_os = "macos")]
#[macro_use]
extern crate objc;

mod app;
mod blocks_ui;
mod config;
mod input;
mod mcp;
mod pty;
mod shell_integration;
mod wake;
mod terminal;
mod renderer;
mod window;
mod ui;
mod network;
#[allow(dead_code)]
mod ai;
#[allow(dead_code)]
mod effects;
#[allow(dead_code)]
mod tools;
#[allow(dead_code)]
mod plugin;
mod platform;
mod screenshot;
#[cfg(test)]
mod audit;

use winit::event_loop::EventLoop;

fn main() {
    // CLI argument parsing (no external crate)
    let args: Vec<String> = std::env::args().collect();
    // `rift mcp`: stdio <-> running Rift's MCP socket (for `claude mcp add rift -- rift mcp`).
    if args.get(1).map(String::as_str) == Some("mcp") {
        std::process::exit(mcp::bridge::run());
    }
    // Headless scene renderer: no window, no event loop, no shell.
    if args.iter().any(|a| a == "--screenshot") {
        std::process::exit(screenshot::run(&args[1..]));
    }
    let mut it = args[1..].iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("rift {}", config::VERSION);
                return;
            }
            "--list-keybindings" => {
                let cfg = config::toml::load_config();
                print!("{}", app::keymap::Keymap::build(&cfg.input.keybindings).table());
                return;
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--config" => match it.next() {
                Some(path) if !path.is_empty() => config::toml::set_config_path(path.into()),
                _ => {
                    eprintln!("--config needs a PATH");
                    std::process::exit(1);
                }
            },
            _ if arg.starts_with("--config=") && arg.len() > "--config=".len() => {
                config::toml::set_config_path(arg["--config=".len()..].into());
            }
            _ => {
                eprintln!("Unknown option: {arg}");
                eprintln!("Try 'rift --help'");
                std::process::exit(1);
            }
        }
    }

    env_logger::init();

    let config = config::toml::load_config();
    config::set_osc52_policy(config.osc52);
    log::info!("Theme: {}", config.theme_name);
    log::info!(
        "LLM: provider={}, model={}, enabled={}",
        config.llm.provider, config.llm.model, config.llm.enabled
    );

    let font_path = config::resolve_font_path(&config);
    let theme = config.theme.clone();
    let mut renderer = renderer::Renderer::new(&font_path, config.font_size, theme);
    renderer.set_bold_is_bright(config.bold_is_bright);
    renderer.opacity = config.opacity;
    renderer.shader.set_intensity(config.effect_intensity);
    renderer.shader.set_effect(config.effect);

    let event_loop = EventLoop::new().unwrap();
    let proxy = event_loop.create_proxy();
    wake::init(proxy.clone());
    let mut wm = window::WindowManager::new(proxy, config.cols as usize, config.rows as usize);
    wm.set_scrollback_lines(config.scrollback_lines);

    // Look for local model servers (Ollama, LM Studio, ...) in the background
    // so the first-run prompt and the model picker already know the answer.
    if config.ai_consent != ai::consent::Consent::Declined {
        ai::local::discovery::start();
    }

    let mut app = app::App::new(config, renderer, wm);
    event_loop.run_app(&mut app).unwrap();
}

fn print_help() {
    let mk = config::mod_key();
    println!("rift {} — A cyberpunk terminal emulator built in Rust", config::VERSION);
    println!();
    println!("USAGE: rift [OPTIONS]");
    println!("       rift mcp          MCP stdio bridge to the running Rift (see README)");
    println!();
    println!("OPTIONS:");
    println!("  -h, --help       Print this help message");
    println!("  -V, --version    Print version");
    println!("  --list-keybindings  Print the effective keybinding table");
    println!("  --config PATH    Use custom config file");
    println!("  --screenshot SCENE|all --out DIR [--width 1600 --height 1000 --theme NAME]");
    println!("                   Render product screenshots offscreen (no window)");
    println!();
    println!("MCP (let Claude Code / Codex read terminal state; commands need your approval):");
    println!("  claude mcp add rift -- rift mcp");
    println!("  config: [mcp] enabled = true, allow_run = \"ask\" | \"never\"");
    println!();
    println!("CONFIG: ~/.config/rift/config.toml");
    println!();
    println!("KEY SHORTCUTS:");
    println!("  (see --list-keybindings for the full table; override in [keybindings])");
    println!("  {mk}+D              Split vertical (left/right)");
    println!("  {mk}+Shift+D        Split horizontal (up/down)");
    println!("  {mk}+Shift+T        New tab");
    println!("  {mk}+Shift+W        Close pane/tab");
    println!("  {mk}+Shift+[/]      Switch tabs");
    println!("  {mk}+F              Search scrollback");
    println!("  {mk}+Shift+S        SSH connect");
    println!("  {mk}+Shift+A        AI assistant (LLM)");
    println!("  {mk}+Shift+H        HUD system dashboard");
    println!("  {mk}+Shift+Z        Time warp (history replay)");
    println!("  {mk}+Shift+E        File manager");
    println!("  {mk}+Shift+G        Git panel");
    println!("  Ctrl+Shift+1-6    Effects: CRT/Glitch/Neon/Matrix/Amber/Hologram (0 = off, +/- intensity)");
    println!("  {mk}+Shift+,        Preferences");
    println!("  {mk}+Shift+?        Welcome guide");
    println!("  {mk}+Q              Quit");
    println!();
    println!("FEATURES (compile-time):");
    println!("  --features gpu       Enable wgpu GPU rendering");
    println!("  --features webview   Enable embedded browser");
    println!("  --features plugins   Enable WASM plugin system");
}
