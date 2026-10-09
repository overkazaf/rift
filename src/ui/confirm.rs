//! Generic confirmation modal (kit-styled) with a queue of requests.
//!
//! Used for the one-time cloud-AI consent, multi-line paste protection and
//! SSH host-key trust. Each request carries a [`ConfirmAction`] that
//! [`resolve`] applies once the user picks a button (or cancels).

use std::time::{Duration, Instant};

use crate::app::App;
use crate::ui::kit::{ButtonKind, ButtonState, Ctx, PanelSpec, Rect, Tokens, Tone};

pub enum ConfirmAction {
    /// Buttons: Enable / Local only / Not now. Esc leaves the decision open.
    AiConsent(crate::ai::consent::EnvCandidate),
    /// Buttons: Paste / Cancel.
    Paste { text: String, bracketed: bool },
    /// Buttons: Trust & connect / Cancel. Dropping the sender refuses.
    SshHostKey { reply: Option<tokio::sync::oneshot::Sender<bool>> },
}

pub struct ConfirmRequest {
    pub title: String,
    pub badge: Option<(String, Tone)>,
    pub lines: Vec<String>,
    pub buttons: Vec<String>,
    /// Index selected initially (the safe choice for risky prompts).
    pub default_sel: usize,
    /// Button index chosen by Esc; `None` = dismiss without deciding.
    pub esc_choice: Option<usize>,
    pub tone: Tone,
    pub action: ConfirmAction,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfirmKey {
    Left,
    Right,
    Enter,
    Escape,
    /// `1`..`9` selects (and confirms) that button.
    Digit(usize),
}

#[derive(Default)]
pub struct ConfirmModal {
    queue: std::collections::VecDeque<ConfirmRequest>,
    sel: usize,
    shown_at: Option<Instant>,
    /// Button rectangles from the last render (for mouse hit-testing).
    rects: Vec<Rect>,
}

/// Enter within this window after a modal appears is ignored, so a keystroke
/// the user was already making cannot answer a prompt they never saw.
const ENTER_GUARD: Duration = Duration::from_millis(400);

impl ConfirmModal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn visible(&self) -> bool {
        !self.queue.is_empty()
    }

    pub fn push(&mut self, req: ConfirmRequest) {
        let first = self.queue.is_empty();
        self.queue.push_back(req);
        if first {
            self.begin();
        }
    }

    fn begin(&mut self) {
        self.sel = self.queue.front().map_or(0, |r| r.default_sel.min(r.buttons.len().saturating_sub(1)));
        self.shown_at = Some(Instant::now());
        self.rects.clear();
    }

    /// Feed a key. Returns `Some((request, choice))` when the front request
    /// was answered; `choice` is `None` for a dismissal.
    pub fn handle_key(&mut self, key: ConfirmKey) -> Option<(ConfirmRequest, Option<usize>)> {
        let n = self.queue.front()?.buttons.len();
        let choice = match key {
            ConfirmKey::Left => {
                self.sel = if self.sel == 0 { n - 1 } else { self.sel - 1 };
                return None;
            }
            ConfirmKey::Right => {
                self.sel = (self.sel + 1) % n;
                return None;
            }
            ConfirmKey::Enter => {
                if self.shown_at.is_some_and(|t| t.elapsed() < ENTER_GUARD) {
                    return None;
                }
                Some(self.sel)
            }
            ConfirmKey::Escape => self.queue.front()?.esc_choice,
            ConfirmKey::Digit(d) if d >= 1 && d <= n => Some(d - 1),
            ConfirmKey::Digit(_) => return None,
        };
        self.finish(choice)
    }

    /// Mouse press at (x, y); returns like [`handle_key`](Self::handle_key).
    pub fn click(&mut self, x: usize, y: usize) -> Option<(ConfirmRequest, Option<usize>)> {
        let i = self.rects.iter().position(|r| r.contains(x, y))?;
        self.sel = i;
        self.finish(Some(i))
    }

    fn finish(&mut self, choice: Option<usize>) -> Option<(ConfirmRequest, Option<usize>)> {
        let req = self.queue.pop_front()?;
        if !self.queue.is_empty() {
            self.begin();
        }
        Some((req, choice))
    }

    pub fn render(
        &mut self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme,
    ) {
        let Some(req) = self.queue.front() else { return };
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(0.7);

        let cols = 76usize.min((width / tk.cw.max(1)).saturating_sub(8)).max(24);
        let wrapped: Vec<String> = req.lines.iter().flat_map(|l| wrap(l, cols.saturating_sub(2))).collect();
        let hints: &[(&str, &str)] = &[("\u{2190}/\u{2192}", "select"), ("Enter", "confirm"), ("1-9", "choose"), ("Esc", "cancel")];
        let want_h = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + wrapped.len() * tk.row_h + tk.sp.lg + tk.button_h;
        let rect = cx.centered_cols(cols, want_h);
        let mut spec = PanelSpec::new(&req.title).edge(req.tone).hints(hints).no_close();
        if let Some((b, t)) = &req.badge {
            spec = spec.badge(b, *t);
        }
        let body = cx.panel(rect, &spec);

        let mut y = body.y;
        for l in &wrapped {
            cx.line_fit(body.x, y, body.w, l, tk.text);
            y += tk.row_h;
        }

        // Buttons, right-aligned along the bottom of the body.
        let total: usize = req.buttons.iter().map(|b| cx.button_w(b)).sum::<usize>() + tk.sp.md * req.buttons.len().saturating_sub(1);
        let mut x = body.right().saturating_sub(total).max(body.x);
        let by = body.bottom().saturating_sub(tk.button_h);
        let mut rects = Vec::with_capacity(req.buttons.len());
        for (i, label) in req.buttons.iter().enumerate() {
            let w = cx.button_w(label);
            let r = Rect::new(x, by, w, tk.button_h);
            let focused = i == self.sel;
            let kind = match (focused, req.tone) {
                (true, Tone::Danger) => ButtonKind::Danger,
                (true, _) => ButtonKind::Primary,
                _ => ButtonKind::Secondary,
            };
            cx.button(r, label, kind, if focused { ButtonState::Focused } else { ButtonState::Normal });
            rects.push(r);
            x += w + tk.sp.md;
        }
        self.rects = rects;
    }
}

/// Greedy word wrap to `max` columns (long words are split).
pub fn wrap(s: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let mut word = word.to_string();
        while word.chars().count() > max {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            let head: String = word.chars().take(max).collect();
            word = word.chars().skip(max).collect();
            out.push(head);
        }
        let need = if cur.is_empty() { word.chars().count() } else { cur.chars().count() + 1 + word.chars().count() };
        if need > max {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&word);
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

// ---- request builders ------------------------------------------------------

pub fn show_ai_consent(app: &mut App, c: crate::ai::consent::EnvCandidate) {
    let lines = vec![
        format!("Rift found ${}.", c.var),
        format!(
            "Enable cloud AI with provider {}? Terminal output you send (and, with auto-fix, the output of failed commands) will go to {}.",
            c.provider,
            c.host()
        ),
        "\"Local only\" uses Ollama on this machine and sends nothing off-device. \"Not now\" disables AI; change it later in config.toml ([ai] consent).".into(),
    ];
    app.confirm.push(ConfirmRequest {
        title: "Enable AI features?".into(),
        badge: Some(("PRIVACY".into(), Tone::Warning)),
        lines,
        buttons: vec!["Enable".into(), "Local only".into(), "Not now".into()],
        default_sel: 2,
        esc_choice: None,
        tone: Tone::Warning,
        action: ConfirmAction::AiConsent(c),
    });
}

pub fn show_paste_confirm(app: &mut App, text: String, bracketed: bool) {
    let n = text.lines().count().max(1);
    let preview: String = text.lines().next().unwrap_or("").chars().take(60).collect();
    app.confirm.push(ConfirmRequest {
        title: format!("Paste {n} line{}?", if n == 1 { "" } else { "s" }),
        badge: None,
        lines: vec![
            "The shell is not in bracketed-paste mode, so each line will run as soon as it is pasted.".into(),
            format!("First line: {preview}"),
        ],
        buttons: vec!["Paste".into(), "Cancel".into()],
        default_sel: 1,
        esc_choice: Some(1),
        tone: Tone::Warning,
        action: ConfirmAction::Paste { text, bracketed },
    });
}

/// A one-button, loud notice (e.g. SSH host key mismatch).
pub fn show_notice(app: &mut App, title: &str, msg: &str) {
    app.confirm.push(ConfirmRequest {
        title: title.into(),
        badge: Some(("DANGER".into(), Tone::Danger)),
        lines: vec![msg.into()],
        buttons: vec!["OK".into()],
        default_sel: 0,
        esc_choice: Some(0),
        tone: Tone::Danger,
        action: ConfirmAction::SshHostKey { reply: None },
    });
}

pub fn show_ssh_host_key(app: &mut App, p: crate::network::ssh::session::HostKeyPrompt) {
    let mut lines = vec![
        format!("The authenticity of host {}:{} can't be established.", p.host, p.port),
        format!("{} key fingerprint:", p.key_type),
        p.fingerprint.clone(),
    ];
    if p.other_keys_known {
        lines.push("known_hosts has other key types for this host; verify this one out of band.".into());
    }
    lines.push("Trusting adds it to ~/.ssh/known_hosts and connects.".into());
    app.confirm.push(ConfirmRequest {
        title: "Unknown SSH host".into(),
        badge: Some(("VERIFY".into(), Tone::Warning)),
        lines,
        buttons: vec!["Trust & connect".into(), "Cancel".into()],
        default_sel: 1,
        esc_choice: Some(1),
        tone: Tone::Warning,
        action: ConfirmAction::SshHostKey { reply: Some(p.reply) },
    });
}

/// Apply the user's answer to `req`.
pub fn resolve(app: &mut App, req: ConfirmRequest, choice: Option<usize>) {
    use crate::ai::consent::{self, Consent};
    match req.action {
        ConfirmAction::AiConsent(env) => match choice {
            Some(0) => consent::apply(app, Consent::Cloud, Some(&env)),
            Some(1) => consent::apply(app, Consent::Local, None),
            Some(2) => consent::apply(app, Consent::Declined, None),
            _ => {}
        },
        ConfirmAction::Paste { text, bracketed } => {
            if choice == Some(0) {
                crate::app::mouse::write_paste(app, &text, bracketed);
            }
        }
        ConfirmAction::SshHostKey { mut reply } => {
            if let Some(tx) = reply.take() {
                let _ = tx.send(choice == Some(0));
            }
        }
    }
    app.request_redraw();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(n: usize, def: usize, esc: Option<usize>) -> ConfirmRequest {
        ConfirmRequest {
            title: "t".into(),
            badge: None,
            lines: vec![],
            buttons: (0..n).map(|i| format!("b{i}")).collect(),
            default_sel: def,
            esc_choice: esc,
            tone: Tone::Neutral,
            action: ConfirmAction::SshHostKey { reply: None },
        }
    }

    fn settled(m: &mut ConfirmModal) {
        m.shown_at = Some(Instant::now() - Duration::from_secs(5));
    }

    #[test]
    fn navigation_wraps_and_enter_confirms_selection() {
        let mut m = ConfirmModal::new();
        m.push(req(3, 2, None));
        settled(&mut m);
        assert_eq!(m.sel, 2);
        assert!(m.handle_key(ConfirmKey::Right).is_none());
        assert_eq!(m.sel, 0, "wraps forward");
        assert!(m.handle_key(ConfirmKey::Left).is_none());
        assert_eq!(m.sel, 2, "wraps backward");
        assert_eq!(m.handle_key(ConfirmKey::Enter).unwrap().1, Some(2));
    }

    #[test]
    fn enter_is_ignored_right_after_the_modal_appears() {
        let mut m = ConfirmModal::new();
        m.push(req(2, 0, Some(1)));
        assert!(m.handle_key(ConfirmKey::Enter).is_none());
        assert!(m.visible());
        settled(&mut m);
        let (_, c) = m.handle_key(ConfirmKey::Enter).unwrap();
        assert_eq!(c, Some(0));
        assert!(!m.visible());
    }

    #[test]
    fn escape_uses_the_request_choice() {
        let mut m = ConfirmModal::new();
        m.push(req(2, 0, Some(1)));
        assert_eq!(m.handle_key(ConfirmKey::Escape).unwrap().1, Some(1));
        m.push(req(3, 2, None));
        assert_eq!(m.handle_key(ConfirmKey::Escape).unwrap().1, None);
    }

    #[test]
    fn digits_choose_and_requests_queue() {
        let mut m = ConfirmModal::new();
        m.push(req(3, 0, None));
        m.push(req(2, 1, None));
        assert!(m.handle_key(ConfirmKey::Digit(7)).is_none());
        assert_eq!(m.handle_key(ConfirmKey::Digit(2)).unwrap().1, Some(1));
        assert!(m.visible(), "second request now showing");
        assert_eq!(m.sel, 1);
    }

    #[test]
    fn click_hits_button_rects() {
        let mut m = ConfirmModal::new();
        m.push(req(2, 0, None));
        m.rects = vec![Rect::new(10, 10, 20, 10), Rect::new(40, 10, 20, 10)];
        assert!(m.click(5, 5).is_none());
        assert_eq!(m.click(45, 12).unwrap().1, Some(1));
    }

    #[test]
    fn wrap_splits_on_words_and_long_tokens() {
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 5), vec![""]);
    }
}
