#[cfg(target_os = "macos")]
#[macro_use]
extern crate objc;

mod app;
mod blocks_ui;
mod config;
mod input;
mod pty;
mod shell_integration;
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

use winit::event_loop::EventLoop;

fn main() {
    // CLI argument parsing (no external crate)
    let args: Vec<String> = std::env::args().collect();
    for arg in &args[1..] {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("rift {}", config::VERSION);
                return;
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            _ if arg.starts_with("--config=") || arg == "--config" => {
                // handled by load_config (future)
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
    log::info!("Theme: {}", config.theme_name);
    log::info!(
        "LLM: provider={}, model={}, enabled={}",
        config.llm.provider, config.llm.model, config.llm.enabled
    );

    let font_path = config::resolve_font_path(&config);
    let theme = config.theme.clone();
    let mut renderer = renderer::Renderer::new(&font_path, config.font_size, theme);
    renderer.opacity = config.opacity;
    renderer.shader.set_intensity(config.effect_intensity);
    renderer.shader.set_effect(config.effect);

    let event_loop = EventLoop::new().unwrap();
    let proxy = event_loop.create_proxy();
    let wm = window::WindowManager::new(proxy, config.cols as usize, config.rows as usize);

    let mut app = app::App::new(config, renderer, wm);
    event_loop.run_app(&mut app).unwrap();
}

fn print_help() {
    let mk = config::mod_key();
    println!("rift {} — A cyberpunk terminal emulator built in Rust", config::VERSION);
    println!();
    println!("USAGE: rift [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("  -h, --help       Print this help message");
    println!("  -V, --version    Print version");
    println!("  --config PATH    Use custom config file");
    println!();
    println!("CONFIG: ~/.config/rift/config.toml");
    println!();
    println!("KEY SHORTCUTS:");
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
