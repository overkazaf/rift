<div align="center">

<img src="assets/icon.png" alt="Rift logo" width="112" height="112">

# Rift

**The AI-native terminal that understands your screen.**

Native and fast like Ghostty. Blocks and AI like Warp. Local-first, bring-your-own-key, no account, and a cyberpunk look to go with it.

[![License: MIT](https://img.shields.io/badge/license-MIT-ff2ebe.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-00e5ff.svg?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-7a86ff.svg)](#quickstart)
[![Ko-fi](https://img.shields.io/badge/support-Ko--fi-ff5e5b.svg?logo=ko-fi&logoColor=white)](https://ko-fi.com/john5555555555)

[**Website**](https://overkazaf.github.io/rift/) · [Quickstart](#quickstart) · [AI setup](#ai-setup) · [Shortcuts](#keyboard-shortcuts) · [Roadmap](#roadmap)

<img src="docs/screenshots/hero.png" alt="Rift with command blocks, a failed cargo build showing an inline AI fix suggestion, and the AI chat sidebar docked on the right" width="900">

</div>

## Why Rift

- **Native and fast.** Written in Rust. Rendering is damage-tracked, with an optional GPU path: a keystroke frame takes about 6 ms, down from about 42 ms before damage tracking. IME, CJK and Nerd Font fallback are built in.
- **Understands your screen.** Shell integration turns output into command blocks. The AI gets your selection, a block or the whole screen as context, and it never runs a command without you.
- **Yours, locally.** No account and no telemetry. Bring your own key (DeepSeek or any OpenAI-compatible API) or run fully offline with Ollama.

## Features

### Command blocks

Each command and its output form one block. This works through OSC 133 hooks that Rift injects into **zsh, bash and fish** automatically. It never edits your rc files.

- Exit-code gutter, duration and exit-status chips
- Hover toolbar: copy command, copy output, ask AI, rerun, fold
- Fold long output, jump between blocks with `Cmd+Shift+↑/↓`, copy a block's output with `Cmd+Shift+C`

<img src="docs/screenshots/blocks.png" alt="Command blocks with green and red exit-code gutters, duration chips and a hover toolbar" width="800">

### AI that lives where you work *(preview: under active development)*

| | |
|---|---|
| **Docked AI chat** (`Cmd+Shift+A`): a streaming sidebar next to your panes. Works with OpenAI-compatible APIs (DeepSeek, OpenAI…) and local Ollama. | <img src="docs/screenshots/ai-chat.png" alt="AI chat sidebar docked on the right streaming an answer" width="420"> |
| **Ask about this** (`Cmd+K`): a popover anchored to your selection, the hovered block, the last block or the screen. | <img src="docs/screenshots/cmdk.png" alt="Cmd+K popover anchored to a selected block" width="420"> |
| **Inline fixes**: when a command fails, a corrected command appears at the prompt. `Tab` accepts it, `Esc` dismisses it. | <img src="docs/screenshots/fix-suggestion.png" alt="Failed cargo build with an inline fix suggestion bar" width="420"> |
| **`# natural language`**: type `# find the 10 largest files` and press Enter. Rift types the generated command back into your prompt so you can review it. It is never executed for you. | <img src="docs/screenshots/nl-command.png" alt="A # comment at the prompt turned into a du and sort command" width="420"> |

Also available: **Advisor** gives a second-opinion risk review of suggested commands. **Teaching mode** explains each command before it runs. **Observer** offers opt-in workflow insights that stay on your machine.

### Preview-Then-Accept

Rift intercepts dangerous commands such as `rm -rf`, `git reset --hard` and force pushes before they run, and shows a real impact analysis. Critical commands need a typed "yes".

<img src="docs/screenshots/preview-accept.png" alt="Preview-Then-Accept modal listing what rm -rf would delete" width="800">

### Splits, tabs and sessions

Recursive splits with geometric focus, zoom, equalize, swap and drag-resize. You can rename and reorder tabs. Session restore brings back your tabs, split layouts and working directories.

<img src="docs/screenshots/splits.png" alt="Four split panes across two tabs" width="800">

### Command palette and built-in browser

| | |
|---|---|
| **Command palette** (`Cmd+P`): fuzzy search ranked by frecency, plus parameterized commands: `theme`, `font`, `open`, `ssh`, `cd`, `>` (shell), `?` (ask AI). | <img src="docs/screenshots/palette.png" alt="Command palette with fuzzy matches" width="420"> |
| **Built-in browser** (`Cmd+Shift+B`): a resizable docked panel with toolbar and a smart URL-or-search address bar. `Cmd`+click a link to open it inside Rift. | <img src="docs/screenshots/browser.png" alt="Built-in browser panel docked beside the terminal" width="420"> |

### Cyber identity

Six screen effects (**CRT, Neon Glow, Matrix Rain, Hologram, Glitch, Amber**) on `Ctrl+Shift+1…6`. The flagship theme is **`rift-neon`**, and nine more ship with it: catppuccin-mocha, hacker-green, dracula, nord, solarized-dark, tokyo-night, cyberpunk, gruvbox, monokai.

| | |
|---|---|
| <img src="docs/screenshots/effects-crt.png" alt="CRT effect with scanlines and curvature" width="420"> | <img src="docs/screenshots/hud.png" alt="HUD strip with CPU, memory, disk, git and uptime" width="420"> |

### Power tools

SSH manager · Time Warp (rewind the screen) · HUD · Git / Docker / CI panels · Port dashboard · Regex playground · History search (`Ctrl+R`) · asciinema recording · Secret masking · Inline images (Kitty graphics protocol) · Broadcast input · Compare pane output · File manager · Command heatmap · Audit log

### Core

Damage-tracked rendering with an optional wgpu backend · IME (Chinese input) · font fallback (Nerd Font / Powerline / CJK) · shell integration (OSC 133 / OSC 7) · smooth scrolling · rich selection · context menu · Kitty inline images · session restore

## How it compares

Rift is young. The others are excellent and much more mature. This table shows what Rift brings together, and where it is still catching up.

| | **Rift** | Ghostty | Warp | iTerm2 |
|---|---|---|---|---|
| Open source | ✅ MIT | ✅ MIT | ❌ | ✅ GPL |
| Native app | ✅ Rust | ✅ Zig | ✅ Rust | ✅ Obj-C |
| GPU rendering | ◐ optional (wgpu) | ✅ | ✅ | ✅ Metal |
| No account required | ✅ | ✅ | ◐ AI needs login | ✅ |
| Command blocks | ✅ | ❌ | ✅ | ◐ marks |
| Built-in AI chat / NL → command | ◐ preview | ❌ | ✅ | ◐ plugin |
| Bring your own key / local models | ✅ incl. Ollama | — | ◐ | ◐ plugin |
| Dangerous-command preview | ✅ | ❌ | ❌ | ❌ |
| Built-in browser panel | ✅ | ❌ | ❌ | ◐ |
| Screen effects / shaders | ✅ 6 built in | ✅ custom shaders | ❌ | ❌ |
| Platforms | macOS, Linux | macOS, Linux | macOS, Linux, Windows | macOS |
| Maturity | Early (v0.3) | Stable | Stable | Very mature |

<sub>✅ yes · ◐ partial / in progress · ❌ no. Based on our reading of each project's public docs as of 2026. If something is wrong, please open an issue.</sub>

## Quickstart

You need a stable Rust toolchain. Prebuilt binaries will be published on [Releases](https://github.com/overkazaf/rift/releases).

```bash
git clone https://github.com/overkazaf/rift.git
cd rift
cargo build --release --features gpu,webview
./target/release/rift
```

macOS app bundle (creates `target/Rift.app`):

```bash
./scripts/bundle_macos.sh
open target/Rift.app
```

| Feature flag | What it adds |
|---|---|
| `gpu` | wgpu renderer and GPU effects |
| `webview` | Built-in browser panel (system WebView; needs WebKitGTK on Linux) |
| `plugins` | Experimental WASM plugin host (wasmtime) |

```bash
rift --help            # usage
rift --version
rift --config PATH     # use a specific config file
```

## AI setup

Config lives at `~/.config/rift/config.toml`. If `api_key` is omitted, Rift reads `$DEEPSEEK_API_KEY` or `$OPENAI_API_KEY` from the environment.

**DeepSeek** (or any OpenAI-compatible API):

```toml
[llm]
provider = "openai"          # any OpenAI-compatible endpoint
model    = "deepseek-chat"
api_url  = "https://api.deepseek.com"
# api_key = "..."            # optional; falls back to $DEEPSEEK_API_KEY
```

**OpenAI:**

```toml
[llm]
provider = "openai"
model    = "gpt-4o-mini"
api_url  = "https://api.openai.com"
```

**Ollama** (fully local, no key):

```toml
[llm]
provider = "ollama"
model    = "llama3.2"
api_url  = "http://localhost:11434"
```

Turn the ambient AI features on or off:

```toml
[ai]
auto_fix = true   # suggest a fix when a command fails
nl_hash  = true   # "# ..." at the prompt → generated command
```

Other `[general]` keys: `theme` (default `"rift-neon"`), `font_family`, `font_path`, `font_size`, `opacity`, `cols`, `rows`, `effect`, `effect_intensity`, `startup_animation`. You can also open **Preferences** (`Cmd+,`).

### Privacy

- No account and no telemetry.
- AI requests go directly from your machine to the endpoint you configure. With Ollama they never leave your machine.
- Screen or block text is sent only for an AI action you trigger, or for auto-fix on failed commands, which you can turn off with `auto_fix = false`.
- Observer is opt-in and stores data locally under `~/.config/rift/observer/`. It skips sensitive commands and never records arguments.

## Keyboard shortcuts

macOS bindings. On Linux, the `Cmd+Shift+…` shortcuts use `Ctrl+Shift+…`. You can also search any action in the command palette (`Cmd+P`).

**Essentials**

| Shortcut | Action |
|---|---|
| `Cmd+P` | Command palette |
| `Cmd+K` | Ask AI about this (selection / block / screen) |
| `Cmd+Shift+A` | AI chat sidebar |
| `Tab` / `Esc` | Accept / dismiss an inline fix suggestion |
| `# …` + `Enter` | Natural language → command |
| `Ctrl+R` | History search |
| `Cmd+F` | Find in scrollback |
| `Ctrl+Space` | Autocomplete |
| `Cmd+,` | Preferences |
| `Cmd+C` / `Cmd+V` / `Cmd+A` | Copy / paste / select all |
| `Cmd+=` / `Ctrl+-` / `Cmd+0` | Font size up / down / reset |

**Blocks**

| Shortcut | Action |
|---|---|
| `Cmd+Shift+↑` / `Cmd+Shift+↓` | Previous / next block |
| `Cmd+Shift+C` | Copy output of the current block |
| `Cmd+C` (block selected) | Copy that block's output |

**Tabs and panes**

| Shortcut | Action |
|---|---|
| `Cmd+Shift+T` | New tab |
| `Cmd+Shift+W` | Close tab |
| `Cmd+Shift+[` / `Cmd+Shift+]` | Previous / next tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Cycle tabs |
| `Cmd+D` | Split right |
| `Cmd+Shift+D` | Split down |
| `Cmd+W` | Close pane |
| `Cmd+Shift+Enter` | Zoom pane |
| `Cmd+Ctrl+=` | Equalize panes |
| `Cmd+[` / `Cmd+]` | Previous / next pane |
| `Cmd+Alt+Arrow` (or `Alt+Arrow`) | Focus pane in that direction |
| `Cmd+Ctrl+Arrow` | Resize pane |
| `Cmd+Ctrl+Shift+Arrow` | Swap pane |

**Browser**

| Shortcut | Action |
|---|---|
| `Cmd+Shift+B` | Toggle browser panel |
| `Cmd+L` | Focus address bar |
| `Cmd+R` | Reload |
| `Cmd+[` / `Cmd+]` | Back / forward (when the browser has focus) |
| `Cmd+W` | Close browser (when the browser has focus) |
| `Cmd`+click a link | Open it inside Rift |

**Tools**

| Shortcut | Action |
|---|---|
| `Cmd+Shift+S` | SSH manager |
| `Cmd+Shift+H` | HUD |
| `Ctrl+Shift+Z` | Time Warp |
| `Cmd+Shift+G` | Git panel |
| `Cmd+Shift+O` | Docker panel |
| `Cmd+Shift+I` | CI/CD panel |
| `Cmd+Shift+E` | File manager |
| `Cmd+Shift+X` | Regex playground |
| `Cmd+Shift+Y` | Command heatmap |
| `Cmd+Shift+R` | Toggle recording (asciinema) |
| `Cmd+Shift+M` | Secret masking |
| `Cmd+Shift+U` | Audit log |
| `Cmd+Shift+L` | Teaching mode |
| `Ctrl+Shift+V` | Observer summary |
| `Cmd+Shift+P` | Broadcast input to all panes |
| `Cmd+Shift+K` | Compare pane output |
| `Cmd+Alt+K` | Clear buffer |
| `Ctrl+Cmd+F` | Full screen |
| `Cmd+Shift+?` | Welcome guide |

**Effects:** `Ctrl+Shift+1` CRT · `2` Glitch · `3` Neon Glow · `4` Matrix Rain · `5` Amber · `6` Hologram · `Ctrl+Shift+0` off. Effects use `Ctrl`, not `Cmd`, because macOS reserves `Cmd+Shift+3/4/5` for screenshots. They are also under **View → Effects**.

## Architecture

About 40k lines of Rust across roughly 125 files. Single binary.

```
src/
├── main.rs              CLI args, event loop bootstrap
├── app/                 App state + input routing (shortcuts, panes, tabs, mouse, IME, overlays, lifecycle)
├── terminal/            VT parser (vte), grid, scrollback, OSC 133/7 semantic marks, Kitty inline images
├── renderer/            softbuffer CPU renderer with damage tracking, optional wgpu GPU path, font fallback
├── window/              WindowManager, tabs, recursive split tree, panes, selection
├── shell_integration/   Embedded zsh/bash/fish hooks + auto-injection (never touches rc files)
├── blocks_ui/           Warp-style command blocks: gutter, chips, hover toolbar, folding, navigation
├── ai/                  LLM backends, chat sidebar (streaming), inline Cmd+K / fix / # NL, advisor, observer
├── network/             SSH (russh), built-in browser chrome + WebView (wry)
├── tools/               Command palette, exec preview, time warp, HUD, git/docker/CI panels, history, recording…
├── ui/                  UI kit (design tokens, widgets), menu bar, tab bar, preferences, welcome, context menu
├── effects/             Screen effects (CRT, Neon, Matrix, Hologram, Glitch, Amber)
├── config/              config.toml parsing, themes (rift-neon + 9)
├── plugin/              Experimental WASM plugin host (feature `plugins`)
└── platform/            macOS integration (transparency, native bits)
```

## Roadmap

**Done**
- Damage-tracked rendering, optional wgpu backend, IME, font fallback
- Shell integration with auto-injected hooks for zsh, bash and fish
- Command blocks: chips, hover toolbar, folding, navigation
- Recursive splits, tabs, session restore
- Command palette, built-in browser, Preview-Then-Accept, power tools

**In progress**
- AI: docked chat, `Cmd+K`, inline fixes, `# natural language`, Advisor, Teaching mode, Observer insights
- Real screenshots and demo recordings for the website and README

**Next**
- Prebuilt, signed releases for macOS; Linux packages
- Broader Linux testing
- Config docs and theme customization guide
- Plugin API stabilization

Windows is not planned for the near term.

## Contributing

Issues and PRs are welcome.

1. Fork and create a branch.
2. `cargo build` (add `--features gpu,webview` to exercise those paths) and `cargo test`.
3. Keep changes focused and describe what you tested. Screenshots help with UI changes.

Found a bug or a wrong claim in the comparison table? [Open an issue](https://github.com/overkazaf/rift/issues).

## Support

Rift is built by one person, in the open. If it saves you time, [**star the repo**](https://github.com/overkazaf/rift) or [**buy me a coffee on Ko-fi**](https://ko-fi.com/john5555555555).

## License

[MIT](LICENSE) © [overkazaf](https://github.com/overkazaf)
