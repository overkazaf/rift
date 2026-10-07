# Rift — Dimension Rift Terminal

> A cyberpunk terminal emulator built in Rust. SSH + AI + 20 visual effects + 32 built-in tools.

Rift is a modern terminal emulator that combines the reliability of a traditional terminal with an integrated AI assistant, cyberpunk visual effects, and a comprehensive developer toolkit — all in a single binary.

## Features

### Terminal Core

- Full VT100/xterm terminal emulation with scrollback (10K lines)
- Multiple tabs + unlimited split panes (auto-grid layout)
- Ghostty-style tab bar with accent indicators
- Mouse support (SGR 1000/1002/1003), text selection, Cmd+Click URLs
- CJK/double-width character support
- Bracketed paste, cursor styles (Block/Bar/Underline)
- macOS native transparency + Retina 2x scaling

### AI Assistant

- Built-in LLM integration (DeepSeek, Ollama, OpenAI-compatible)
- `Cmd+Shift+A` — Natural language → shell commands
- AI Observer mode — passively learns your workflow patterns (privacy-first, local-only)
- Teaching mode — explains commands before execution
- Context-aware error diagnosis with auto-fix suggestions

### 20 Visual Effects

| Shortcut | Effect | | Menu only | Effect |
|----------|--------|-|-----------|--------|
| `Ctrl+Shift+1` | CRT (scanlines + aberration) | | Raindrop | Ripple distortion |
| `Ctrl+Shift+2` | Glitch (data corruption) | | VHS | Tape degradation |
| `Ctrl+Shift+3` | NeonGlow (bloom) | | CyberpunkGrid | Cyan grid overlay |
| `Ctrl+Shift+4` | MatrixRain (falling chars) | | FilmGrain | Cinema noise |
| `Ctrl+Shift+5` | Amber (80s IBM) | | Invert | Color negative |
| `Ctrl+Shift+6` | Hologram (blue projection) | | Desaturate | Grayscale |
| `Ctrl+Shift+7` | Pixelate (mosaic) | | Chromatic | Hue rotation |
| `Ctrl+Shift+8` | Thermal (heat vision) | | Pulse | Breathing glow |
| `Ctrl+Shift+0` | Off | | Snow / Underwater / NeonOutline / ScanlineRGB |

### Built-in Tools (32)

- **SSH Client** — saved connections with aliases, non-blocking connect
- **Time Warp** — rewind terminal state frame-by-frame (`Ctrl+Shift+Z`)
- **HUD Dashboard** — cyberpunk system monitor (CPU/MEM/disk/git/uptime)
- **Git Panel** — branch/status/log visualization (`Cmd+Shift+G`)
- **Docker Panel** — container/image management (`Cmd+Shift+O`)
- **CI/CD Panel** — GitHub Actions / GitLab CI status (`Cmd+Shift+I`)
- **File Manager** — side panel file browser (`Cmd+Shift+E`)
- **Session Recording** — asciinema v2 format (`Cmd+Shift+R`)
- **Secret Masking** — auto-detect and hide API keys/tokens (`Cmd+Shift+M`)
- **Command Heatmap** — GitHub-style usage calendar (`Cmd+Shift+Y`)
- **Broadcast Mode** — type once, send to all panes (`Cmd+Shift+P`)
- **Output Compare** — diff output across panes (`Cmd+Shift+K`)
- **Error Detection** — 10 error patterns with LLM diagnosis
- **Exec Preview** — shows impact before dangerous commands
- **Env Detection** — auto-detect .nvmrc/.python-version
- **Workflow Automation** — record and replay command sequences
- **Audit Log** — command audit trail
- And more: search, autocomplete, snippets, hex viewer, base64...

### 9 Themes

`catppuccin-mocha` · `hacker-green` · `dracula` · `nord` · `solarized-dark` · `tokyo-night` · `cyberpunk` · `gruvbox` · `monokai`

## Installation

### From source

```bash
git clone https://github.com/user/rift.git
cd rift
cargo build --release
./target/release/rift
```

### Optional features

```bash
# GPU rendering (wgpu)
cargo build --release --features gpu

# Embedded browser
cargo build --release --features webview

# WASM plugin system
cargo build --release --features plugins
```

## Configuration

Config file: `~/.config/rift/config.toml`

```toml
[general]
font_size = 15.0
cols = 120
rows = 36
opacity = 0.92
theme = "cyberpunk"

[llm]
provider = "openai"
model = "deepseek-chat"
api_url = "https://api.deepseek.com"
# api_key auto-detected from $DEEPSEEK_API_KEY
```

Or use the GUI: `Cmd+Shift+,` opens Preferences with live preview.

## Key Shortcuts

| Shortcut | Action |
|----------|--------|
| `Cmd+D` | Split vertical |
| `Cmd+Shift+D` | Split horizontal |
| `Cmd+Shift+T` | New tab |
| `Cmd+Shift+W` | Close pane/tab |
| `Cmd+Shift+[/]` | Switch tabs |
| `Cmd+F` | Search scrollback |
| `Cmd+C/V/A` | Copy/Paste/Select all |
| `Cmd+=/-/0` | Zoom in/out/reset |
| `Cmd+Q` | Quit (auto-saves config) |
| `Alt+Arrow` | Switch pane focus |
| `Cmd+Shift+S` | SSH connect |
| `Cmd+Shift+A` | AI assistant |
| `Cmd+Shift+H` | HUD dashboard |
| `Ctrl+Shift+Z` | Time warp |
| `Ctrl+Shift+1-8` | Visual effects (8 via keys, 12 more via menu) |
| `Cmd+Shift+,` | Preferences |

## Architecture

```
16,000+ lines of Rust · 80 files

src/
├── config/       Configuration + 9 themes
├── app/          Application layer (4 modules)
├── terminal/     VT100 parser + state machine
├── renderer/     softbuffer + wgpu GPU pipeline
├── window/       Tab/Pane/WindowManager
├── ui/           Preferences, Welcome, Menu bar
├── network/      SSH client + WebView
├── ai/           LLM backend + Observer + Autocomplete
├── effects/      8 visual effects (CPU shaders)
├── tools/        32 built-in tools
├── plugin/       WASM plugin system
└── platform/     macOS native integration
```

## License

MIT — see [LICENSE](LICENSE)
