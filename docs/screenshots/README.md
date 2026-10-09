# Screenshots

These images are used by both the GitHub Pages site (`docs/index.html`) and the
repository `README.md`. They are **rendered offscreen by Rift itself** (no
window, no GPU, no screen-recording permission) with the real renderer and UI
code; see `src/screenshot/`. Keep the exact filenames.

Size: 1600 x 1000 px (16:10), PNG, rendered at 2x-style density (20-21 px font
on a Retina-like canvas). `hero.png` is also the Open Graph / Twitter card
image (`https://overkazaf.github.io/rift/screenshots/hero.png`), so its
important content stays inside the central 1200 x 630 area.

| File | What it shows |
|------|---------------|
| `hero.png` | rift-neon: stacked panes, a failed `cargo build` block with the inline fix bar, a passing `npm test` pane and the AI chat sidebar (answer with a SAFE advisor badge) |
| `blocks.png` | Command blocks: gutter bars, exit-code / duration chips, the hover toolbar (Copy cmd / Copy output / Ask AI / Rerun / Collapse), one folded block, one failed block |
| `ai-chat.png` | Docked AI chat sidebar mid-stream (cursor, Stop button) next to the terminal; answer with Run / Insert / Copy and the advisor verdict |
| `cmdk.png` | Cmd+K "ask about this" popover anchored under a selected failed block |
| `fix-suggestion.png` | Failed `cargo build` with the inline fix bar ("Tab accept / Esc dismiss") under the prompt |
| `nl-command.png` | `# find the 10 largest files` turned into a command typed at the prompt, with the AI hint bar (Enter run / Esc clear / Cmd+K refine) |
| `splits.png` | Four split panes (2x2), one focused (tinted divider), the rest dimmed, three tabs |
| `palette.png` | Command palette (Cmd+P) with a fuzzy query (`nt`): highlighted matches, category icons, shortcuts |
| `browser.png` | Built-in browser docked beside the terminal: toolbar, address field, and a placeholder docs page (see note) |
| `preview-accept.png` | Preview-Then-Accept modal for `curl -fsSL … | sh` (CRITICAL, classified by the real exec-preview rules; routine `rm -rf ./build` is Info and shows no modal) |
| `effects-crt.png` | CRT effect: the real GPU shader (curvature, scanlines, vignette, chromatic aberration) |
| `hud.png` | HUD strip (CPU / MEM sparklines, disk, git, load) |

Notes:

* The macOS window frame (traffic lights, title, shadow, gradient backdrop) is
  drawn by the screenshot module only, never by the app.
* The page in `browser.png` is a placeholder drawn with the UI kit: the real
  page is a native webview (wry) and cannot be captured offscreen.
* The palette cannot show the "Recent" section together with a fuzzy query (the
  app only lists sections for an empty query).
* The demo project ("aurora"), its commands and their output are scripted; the
  duration chips are set directly instead of waiting in real time.

Regenerate everything (CPU renderer, ~1 minute):

```bash
cargo run --release -- --screenshot all --out docs/screenshots
```

`effects-crt.png` uses the GPU shader when built with `--features gpu` (wgpu
renders it into an offscreen texture and reads it back); without the feature
the CPU fallback (scanlines + vignette) is used instead. The checked-in image
is the GPU one:

```bash
cargo run --release --features gpu -- --screenshot effects-crt --out docs/screenshots
```

Options: `--width 1600 --height 1000 --theme rift-neon --font-size 24`
(`--theme` takes any built-in theme name; sizes are physical pixels and the
font scales with the height).

`gen_placeholders.py` is the old stdlib-only placeholder generator, kept only
as a fallback for machines without a monospace font.
