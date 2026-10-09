# Changelog

All notable changes to Rift are listed here.

## 0.4.0 — 2026-10-10

Rift 0.4.0 turns the terminal into mission control for AI coding agents: see every agent across your tabs, answer their prompts from one dock, let a policy handle the routine ones, and run several agents on one task.

### Agents mission control

- Detects Claude Code, Codex CLI, Gemini CLI, opencode, Aider and Cursor CLI in any pane (from the command you ran, the foreground process or the window title) and tracks their state: working, waiting for you, idle, done, error.
- **Mission Control dock** (`Cmd+Shift+;`): one card per agent across all tabs with repo, branch, state, elapsed time, last output line, model, tokens, cost, context use and a per-turn timeline. Resizable, with compact and expanded cards.
- Answer approval prompts from the dock with `1` / `2` / `3` (the agent's own options). Risky commands get a **RISKY** badge and critical ones need a second press; Rift re-reads the screen before answering.
- Interrupt (`Esc`), reply inline (`r`), broadcast to marked agents (`b`), restart (`R`) and close (`x`) without switching panes.
- `Cmd+Shift+.` jumps to the next agent that needs you; tab badges, amber pane borders, macOS notifications and a dock badge when an agent waits.
- **New Agent**: start an agent in the current directory or in a fresh git worktree, or a 2×1 / 2×2 / 3×2 grid of agents. Same-worktree collisions are flagged.
- `rift agent-event` for Claude Code hooks and Codex `notify`, for exact state tracking.
- **MCP server**: `claude mcp add rift -- rift mcp` lets coding agents list and read panes, blocks and scrollback (redacted, size-capped) and ask to run commands, always with your approval. Local Unix socket, owner-only.

### Autopilot & policy

- **Autopilot** answers routine approval prompts by policy after a visible, cancellable countdown. Off by default, per agent (`p`) or for all agents (`P`).
- Rules in `~/.config/rift/policy.toml` and per-repo `.rift/policy.toml`. A repo policy is ignored until you trust it, and trust is reset whenever the file changes.
- Safety floors: critical commands are never auto-approved. When an agent asks in its pane they are answered No and you are notified; when they come from an MCP client you get the critical confirm dialog (Deny is the default). Risky commands need a rule that names the program, and protected paths and anything outside the repo always ask.
- **Always allow this** (`w`) appends a precise rule after you confirm it. Every automatic decision is logged to `~/.config/rift/policy.log` (*Agents: Policy Log*).
- MCP `run_command` requests go through the same policy.

### Workflows

- **Best of N**: give the same task to 2–4 agents (mixed CLIs), each in its own worktree. Then compare files, `+`/`-`, test results, duration and cost side by side, merge the winner (`--no-ff`, aborted cleanly on conflict), discard the others or ask AI to judge.
- **Write & Review**: a reviewer agent checks each of the writer's turns and returns a verdict. Forward its feedback with `f`, or set `auto_forward`.
- **Fix failing tests**: runs your test command and loops the failures back to the agent until the tests pass or the attempt limit is reached.
- **Task queue** per agent (`t`): the next task is sent when the agent goes idle, after a 3 s countdown you can cancel. Queues are saved with the session.
- Custom workflows in `~/.config/rift/workflows.toml` with `{task}`, `{branch}` and `{cwd}` placeholders.

### Review

- Per-turn checkpoints of the repo, stored as git trees so your index, branches and stash are never touched. Untracked files are included.
- Change Review overlay (`Cmd+Shift+J`): turns, files and a unified diff. Revert a file or a whole turn (a "before revert" checkpoint is taken automatically), copy the patch or ask AI. A "N files changed" chip appears on the pane.

### Multi-window

- `Cmd+N` opens a new window in the focused pane's directory, and `Cmd+Alt+W` closes a window. Each window has its own tabs, panes and docks, while agents, MCP and workflows are shared across all windows.
- Session restore brings back every window with its tabs, splits and position.

### Browser

- Full toolbar (back, forward, reload, stop, maximize, close) with a progress bar, an editable address field with smart URL-or-search, title and URL sync, and a resizable dock.
- `Cmd`+click opens terminal links inside Rift, and `Cmd+Shift`+click opens them in your default browser.

### Splits

- Recursive split tree with geometric focus (`Cmd+Alt+Arrow`), keyboard resize, zoom, equalize, swap, drag-resize, minimum sizes and inherited working directory.
- Thin dividers and dimmed unfocused panes. Double-click a divider to equalize. Layouts are saved in sessions.

### Performance

- Damage-tracked rendering: a keystroke frame takes about 6 ms instead of about 42 ms.
- GPU text renderer (`--features gpu`) with a glyph atlas, damage tracking and programming ligatures (`font_ligatures`).
- Faster parsing: plain text 83 → 158 MB/s, ANSI-heavy output 42 → 191 MB/s.
- Lower memory: 10 panes with 10k scrollback each went from 568 MB to 298 MB.

### Fixes

- Reverse video on the default background no longer hides text (zsh paste highlight, `less`, `man`).
- Latin-1 characters render with the right glyphs.
- No duplicated prompts after a resize, and inline TUIs such as Claude Code redraw correctly.
- Docks, browser, chat and HUD now agree on the terminal area, so ghost panes no longer appear after a relayout.
- Hardening: cloud AI needs your consent, secrets are redacted from AI requests, OSC 52 clipboard reads are blocked by default, pastes are sanitized and SSH host keys are verified.
- Terminal correctness: reflow on resize, grapheme clusters, kitty keyboard protocol, OSC 8 hyperlinks, synchronized output and more.
- Settings saved from Rift are merged into your config file, keeping comments, unknown keys and `api_key`.

## 0.3.0

- Initial release.
