# Rift — Dimension Rift Terminal

> A cyberpunk terminal emulator built in Rust. SSH + AI + 20 visual effects + 32 built-in developer tools.

Rift is a modern terminal emulator that combines the reliability of a traditional terminal with an integrated AI assistant, cyberpunk visual effects, and a comprehensive developer toolkit — all in a single binary. It's designed for developers, DevOps engineers, security researchers, and terminal enthusiasts who want more than just a shell.

## Highlights

- **20 visual effects** — CRT scanlines, hologram, matrix rain, thermal vision, and more. No other terminal has this.
- **AI assistant** — Natural language → shell commands via DeepSeek/Ollama/OpenAI. Privacy-first, local-only.
- **Time warp** — Rewind your terminal state frame-by-frame. See what was on screen 30 seconds ago.
- **32 built-in tools** — SSH manager, Git panel, Docker panel, CI/CD status, file manager, and more.
- **HUD dashboard** — Cyberpunk-style system monitor with CPU/MEM/disk/git/uptime in the terminal.
- **wgpu GPU rendering** — Optional GPU-accelerated rendering with CRT shader on the GPU.

## Features

### Terminal Core

- VT100/xterm terminal emulation (~90% coverage)
- Scrollback buffer (10,000 lines) with `Cmd+F` search
- Multiple tabs + unlimited split panes (auto-grid layout for 3+ panes)
- Ghostty-style tab bar with accent indicators and close buttons
- Mouse support (SGR 1000/1002/1003) — vim, htop, less work with mouse
- Text selection + copy/paste (`Cmd+C/V/A`, right-click paste)
- CJK/double-width character support (Chinese, Japanese, Korean, emoji)
- Bracketed paste mode (safe multi-line paste)
- Cursor styles: Block, Bar (vim insert), Underline
- Alternate charset (G0/G1) — tmux borders render correctly
- Focus reporting, Device Attributes, OSC 52 clipboard
- URL detection with `Cmd+Click` to open in browser (highlights on Cmd hover)
- macOS native window transparency + Retina 2x scaling
- Native macOS menu bar with all features accessible

### AI Assistant

- Built-in LLM integration (DeepSeek, Ollama, any OpenAI-compatible API)
- `Cmd+Shift+A` — Open AI panel, type natural language, get shell commands
- `Enter` — Execute the suggested command directly
- `Tab` — Paste command into terminal for editing
- `Shift+Enter` — Ask a follow-up question
- AI Observer mode (`Cmd+Shift+V`) — passively learns your workflow patterns
  - Tracks command frequency, error patterns, working directories
  - All data stored locally only (`~/.config/rift/observer/`)
  - 21 sensitive commands excluded (sudo, passwd, ssh-keygen, etc.)
  - One-click data clear
- Teaching mode (`Cmd+Shift+L`) — LLM explains each command before execution
- Context-aware error diagnosis — detects stack traces and suggests fixes
- API key auto-detected from environment (`$DEEPSEEK_API_KEY`, `$OPENAI_API_KEY`)

### 20 Visual Effects

| Shortcut | Effect | Description |
|----------|--------|-------------|
| `Ctrl+Shift+1` | **CRT** | Scanlines + chromatic aberration + barrel distortion + vignette |
| `Ctrl+Shift+2` | **Glitch** | Random data corruption + horizontal band shifts |
| `Ctrl+Shift+3` | **NeonGlow** | Bright pixel bloom / glow effect |
| `Ctrl+Shift+4` | **MatrixRain** | Green falling characters overlay |
| `Ctrl+Shift+5` | **Amber** | 80s IBM monochrome amber monitor |
| `Ctrl+Shift+6` | **Hologram** | Blue-tinted holographic projection with scan line |
| `Ctrl+Shift+7` | **Pixelate** | Mosaic / low-resolution pixel blocks |
| `Ctrl+Shift+8` | **Thermal** | Heat vision colormap (black→blue→red→yellow→white) |
| `Ctrl+Shift+0` | **Off** | Disable all effects |

**Menu-only effects** (View → Effects):

| Effect | Description |
|--------|-------------|
| Raindrop | Ripple distortion from random drop points |
| VHS | Tape degradation + tracking errors + noise |
| CyberpunkGrid | Semi-transparent cyan grid overlay |
| FilmGrain | Cinema-style random luminance noise |
| Invert | Color negative / film negative |
| Desaturate | Adjustable grayscale conversion |
| Chromatic | Psychedelic hue rotation over time |
| Pulse | Screen brightness breathing animation |
| Snow | TV static / no-signal noise |
| Underwater | Blue-green tint + wave distortion |
| NeonOutline | Edge detection with neon-colored outlines |
| ScanlineRGB | LCD sub-pixel stripe simulation |

### Built-in Tools (32)

| Tool | Shortcut | Description |
|------|----------|-------------|
| **SSH Client** | `Cmd+Shift+S` | Saved connections with aliases, non-blocking connect, key auth |
| **Time Warp** | `Ctrl+Shift+Z` | Rewind terminal state frame-by-frame (500 snapshots, ~50s history) |
| **HUD Dashboard** | `Cmd+Shift+H` | 3-row cyberpunk monitor: CPU/MEM/disk/git/uptime/load |
| **Search** | `Cmd+F` | Scrollback search with match highlighting and navigation |
| **Git Panel** | `Cmd+Shift+G` | Branch/status/log visualization with colored file states |
| **Docker Panel** | `Cmd+Shift+O` | Container/image listing, start/stop, log viewing |
| **CI/CD Panel** | `Cmd+Shift+I` | GitHub Actions / GitLab CI run status |
| **File Manager** | `Cmd+Shift+E` | Side panel file browser with type-colored icons |
| **Recording** | `Cmd+Shift+R` | Session recording in asciinema v2 format (.cast) |
| **Secret Masking** | `Cmd+Shift+M` | Auto-detect and hide 15 types of API keys/tokens/passwords |
| **Heatmap** | `Cmd+Shift+Y` | GitHub-style command usage calendar |
| **Broadcast** | `Cmd+Shift+P` | Type once, send to all panes simultaneously |
| **Compare** | `Cmd+Shift+K` | Side-by-side diff of output across panes |
| **Audit Log** | `Cmd+Shift+U` | Command audit trail to `~/.config/rift/audit.log` |
| **Teaching Mode** | `Cmd+Shift+L` | LLM explains commands before execution |
| **AI Observer** | `Cmd+Shift+V` | View workflow analysis summary |
| **Preferences** | `Cmd+Shift+,` | GUI settings panel with live preview |
| **Welcome** | `Cmd+Shift+?` | Interactive onboarding guide (4 pages) |
| Error Detection | auto | 10 error patterns (Python traceback, Rust panic, Node error, etc.) |
| Exec Preview | auto | Shows impact before dangerous commands (rm -rf, git reset --hard) |
| Env Detection | auto | Detects .nvmrc/.python-version and suggests activation |
| Command Blocks | auto | Tracks command boundaries and execution time |
| Command Timer | auto | Measures execution time of each command |
| Alias Suggest | auto | Detects repeated commands and suggests aliases |
| Notification | auto | Desktop notification when long commands complete |
| Autocomplete | `Ctrl+Space` | PATH commands + file paths + shell history |
| Snippets | — | Save/search/execute command snippets |
| Hex Viewer | — | Colored hex dump with ASCII column |
| Base64/URL Codec | — | Encode/decode Base64, URL, hex |
| Network Monitor | menu | Active network connections viewer |
| Process Tree | menu | Current shell process tree |
| SSH Tunnel View | menu | Active SSH tunnel/port forwarding viewer |
| System Info | menu | Neofetch-style system information panel |

### 9 Themes

| Theme | Style |
|-------|-------|
| `catppuccin-mocha` | Warm dark (default) |
| `hacker-green` | Classic green-on-black |
| `dracula` | Purple-tinted dark |
| `nord` | Cool Nordic blue |
| `solarized-dark` | Ethan Schoonover's classic |
| `tokyo-night` | VS Code-inspired dark blue |
| `cyberpunk` | Neon magenta + electric cyan |
| `gruvbox` | Retro warm brown |
| `monokai` | Sublime Text classic |

## Installation

### From source

```bash
git clone https://github.com/overkazaf/rift.git
cd rift
cargo build --release
./target/release/rift
```

### Optional features

```bash
# GPU rendering (wgpu) — CRT effect runs on GPU
cargo build --release --features gpu

# Embedded browser (WebView)
cargo build --release --features webview

# WASM plugin system
cargo build --release --features plugins
```

### Command line

```bash
rift              # Start terminal
rift --help       # Show help + all shortcuts
rift --version    # Show version
rift --config PATH  # Use custom config file
```

## Configuration

Config file: `~/.config/rift/config.toml` (auto-saved on exit)

### Configuration Reference

#### General

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `font_size` | float | `15.0` | Font size in points (scaled for Retina) |
| `font_family` | string | — | Font name, e.g. `"JetBrains Mono"` |
| `font_path` | string | — | Direct path to `.ttf`/`.otf`/`.ttc` file |
| `cols` | int | `120` | Initial terminal columns |
| `rows` | int | `36` | Initial terminal rows |
| `opacity` | float | `0.92` | Window opacity (0.0 = transparent, 1.0 = opaque) |
| `theme` | string | `"catppuccin-mocha"` | Color theme name |

#### LLM

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `provider` | string | `"ollama"` | LLM provider (`ollama` or `openai` for any OpenAI-compatible API) |
| `model` | string | `"llama3.2"` | Model name |
| `api_url` | string | `"http://localhost:11434"` | API endpoint URL |
| `api_key` | string | — | API key (auto-detected from `$DEEPSEEK_API_KEY` or `$OPENAI_API_KEY`) |

### Example config

```toml
[general]
font_size = 15.0
font_family = "JetBrains Mono"
cols = 120
rows = 36
opacity = 0.85
theme = "cyberpunk"

[llm]
provider = "openai"
model = "deepseek-chat"
api_url = "https://api.deepseek.com"
# api_key auto-detected from $DEEPSEEK_API_KEY
```

GUI configuration: `Cmd+Shift+,` opens Preferences panel with live preview. Press `S` to save. All changes auto-saved on `Cmd+Q` exit.

## Key Shortcuts

### Window Management

| Shortcut | Action |
|----------|--------|
| `Cmd+Shift+T` | New tab |
| `Cmd+Shift+W` | Close pane/tab |
| `Cmd+Shift+[` | Previous tab |
| `Cmd+Shift+]` | Next tab |
| `Ctrl+Tab` | Cycle tabs |
| `Cmd+D` | Split vertical (left/right) |
| `Cmd+Shift+D` | Split horizontal (up/down) |
| `Alt+Arrow` | Switch pane focus |
| `Cmd+Q` | Quit (auto-saves config) |

### Editing

| Shortcut | Action |
|----------|--------|
| `Cmd+C` | Copy selection |
| `Cmd+V` | Paste (bracketed paste aware) |
| `Cmd+A` | Select all |
| `Cmd+F` | Search scrollback |
| `Cmd+=` | Zoom in (font size +1) |
| `Ctrl+-` | Zoom out (font size -1) |
| `Cmd+0` | Reset zoom |

### Tools

| Shortcut | Action |
|----------|--------|
| `Cmd+Shift+S` | SSH connect |
| `Cmd+Shift+A` | AI assistant |
| `Cmd+Shift+H` | HUD dashboard |
| `Ctrl+Shift+Z` | Time warp |
| `Cmd+Shift+R` | Toggle recording |
| `Cmd+Shift+E` | File manager |
| `Cmd+Shift+G` | Git panel |
| `Cmd+Shift+O` | Docker panel |
| `Cmd+Shift+I` | CI/CD panel |
| `Cmd+Shift+Y` | Command heatmap |
| `Cmd+Shift+M` | Toggle secret masking |
| `Cmd+Shift+U` | Toggle audit log |
| `Cmd+Shift+L` | Toggle teaching mode |
| `Cmd+Shift+V` | Observer summary |
| `Cmd+Shift+P` | Toggle broadcast mode |
| `Cmd+Shift+K` | Compare pane output |
| `Cmd+Shift+,` | Preferences |
| `Cmd+Shift+?` | Welcome guide |
| `Ctrl+Space` | Autocomplete |

### Visual Effects

| Shortcut | Effect |
|----------|--------|
| `Ctrl+Shift+1` | CRT |
| `Ctrl+Shift+2` | Glitch |
| `Ctrl+Shift+3` | NeonGlow |
| `Ctrl+Shift+4` | MatrixRain |
| `Ctrl+Shift+5` | Amber |
| `Ctrl+Shift+6` | Hologram |
| `Ctrl+Shift+7` | Pixelate |
| `Ctrl+Shift+8` | Thermal |
| `Ctrl+Shift+0` | Off |

12 additional effects available via **View → Effects** menu.

## FAQ

**Q: Font icons (Powerline/Nerd Font) show as blank**
A: Configure your font in `~/.config/rift/config.toml`:
```toml
font_family = "MesloLGS NF"
```

**Q: `Cmd+Shift+1-5` doesn't work for effects**
A: macOS intercepts `Cmd+Shift+1-5` for screenshots. Use `Ctrl+Shift+1-8` instead, or the **View → Effects** menu.

**Q: How to use Time Warp?**
A: Press `Ctrl+Shift+Z` to enter. Use `←/→` to browse frames, `Shift+Arrow` to jump 10 frames, `Esc` to return.

**Q: AI assistant says "Error"**
A: Configure your LLM provider in config.toml. Set `$DEEPSEEK_API_KEY` or `$OPENAI_API_KEY` environment variable.

**Q: How to see the Observer analysis?**
A: Press `Cmd+Shift+V`. Observer must be enabled first (same shortcut enables it). Data is stored locally in `~/.config/rift/observer/`.

**Q: Terminal colors look wrong in vim**
A: Ensure your shell has `TERM=xterm-256color` and `COLORTERM=truecolor` (Rift sets these automatically).

**Q: How to use a custom theme?**
A: Edit `~/.config/rift/config.toml` and add custom colors under `[theme.custom]`:
```toml
[theme.custom]
fg = [220, 220, 220]
bg = [20, 20, 30]
cursor = [255, 100, 100]
```

## Architecture

```
16,000+ lines of Rust · 80 files

src/
├── config/       Configuration, 9 themes, font discovery
├── app/          Application layer (mod/shortcuts/overlays/lifecycle)
├── terminal/     VT100 parser, state machine, grid
├── renderer/     softbuffer CPU + wgpu GPU pipeline, font manager
├── window/       Tab, Pane, WindowManager, Selection
├── ui/           Preferences, Welcome, Menu bar, UI primitives
├── network/      SSH client (russh) + WebView (wry)
├── ai/           LLM backend, Observer, Autocomplete, Teaching
├── effects/      20 visual effects (CPU shaders + GPU CRT)
├── tools/        32 built-in developer tools
├── plugin/       WASM plugin system (wasmtime)
└── platform/     macOS native integration (transparency)
```

## License

MIT — see [LICENSE](LICENSE)
