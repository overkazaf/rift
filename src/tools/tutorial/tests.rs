use super::cast::{AiOp, BlocksOp, Cast, Kind, Mark, PaneOp, Panel, Place, TabOp, UiOp};
use super::keys::{self, KeyRef};
use super::player::{Outcome, Player, TKey, World, KEY_TTL};
use super::*;
use crate::app::keymap::{Chord, Keymap};
use crate::window::pane::PtyKind;

const CELL: (usize, usize) = (8, 16);

fn bundled() -> Vec<(&'static DemoInfo, Cast)> {
    DEMOS.iter().map(|d| (d, load(d).unwrap_or_else(|e| panic!("{e}")))).collect()
}

fn player(id: &str) -> Player {
    let d = find(id).unwrap();
    Player::new(load(d).unwrap(), d.title.into(), Some(d.id), CELL)
}

fn screen(w: &World) -> Vec<String> {
    let t = &w.wm.active_pane().terminal;
    t.visible_rows().iter().map(|r| r.iter().map(|c| c.c).collect::<String>().trim_end().to_string()).collect()
}

/// Everything a frame depends on, as text.
fn state(p: &Player) -> String {
    let w = p.world();
    let mut s = String::new();
    for (ti, tab) in w.wm.tabs.iter().enumerate() {
        s.push_str(&format!("tab {ti} {} active={} zoom={}\n", tab.title, tab.active, tab.is_zoomed()));
        for pane in tab.panes() {
            let t = &pane.terminal;
            s.push_str(&format!("pane {} {}x{} off={}\n", pane.id, t.cols, t.rows, t.scroll_offset));
            for r in t.visible_rows() {
                s.push_str(r.iter().map(|c| c.c).collect::<String>().trim_end());
                s.push('\n');
            }
            for b in t.blocks.blocks() {
                s.push_str(&format!("block {} exit={:?} dur={} fold={}\n", b.command, b.exit_code, b.duration_ms, b.collapsed));
            }
        }
    }
    s.push_str(&format!(
        "active_tab={} caption={:?} key={:?} panel={} palette={} preview={:?} fix={} nl={} pop={} hover={:?} sel={:?}\n",
        w.wm.active_tab,
        w.caption,
        p.key_overlay(),
        w.panel.as_ref().map_or("-".into(), |p| p.title.clone()),
        w.palette.as_ref().map_or("-".into(), |p| p.query.clone()),
        w.preview.as_ref().map(|p| (p.command.clone(), p.confirm_input.clone())),
        w.inline_ai.fix.current.is_some(),
        matches!(w.inline_ai.nl, crate::ai::inline::NlState::Ghost(_)),
        w.inline_ai.popover.is_some(),
        w.blocks_ui.hover,
        w.blocks_ui.selected,
    ));
    s
}

// ───────────────────────────── demo files ─────────────────────────────

#[test]
fn every_bundled_demo_parses_with_sane_dimensions() {
    assert!(DEMOS.len() >= 6, "6-8 tutorials");
    for (d, c) in bundled() {
        assert!((40..=200).contains(&c.cols) && (10..=60).contains(&c.rows), "{}: {}x{}", d.id, c.cols, c.rows);
        assert!(!c.events.is_empty(), "{}", d.id);
        assert!(c.duration() > 10.0 && c.duration() < 120.0, "{}: {:.1}s", d.id, c.duration());
        assert!(c.steps().len() >= 3, "{}: a tutorial has several captioned steps", d.id);
        assert_eq!(c.title, d.title, "{}: header title matches the registry", d.id);
        assert!(!d.summary.is_empty() && d.summary.len() < 100, "{}", d.id);
    }
}

#[test]
fn demo_ids_are_unique_and_match_the_generator() {
    let ids: Vec<&str> = DEMOS.iter().map(|d| d.id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len());
    let gen_ids: Vec<&str> = super::gen::all().into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, gen_ids);
}

#[test]
fn timings_are_monotonic() {
    for (d, c) in bundled() {
        let mut last = 0.0;
        for (i, e) in c.events.iter().enumerate() {
            assert!(e.t >= 0.0 && e.t.is_finite(), "{} event {i}", d.id);
            assert!(e.t >= last, "{} event {i}: {} < {}", d.id, e.t, last);
            last = e.t;
        }
    }
}

#[test]
fn every_shortcut_mentioned_exists_in_the_keymap() {
    let km = Keymap::default();
    for (d, c) in bundled() {
        for m in c.marks() {
            for r in m.key_refs() {
                match keys::lookup(&r) {
                    Ok(KeyRef::Action(def)) => {
                        assert!(km.primary(def.action).is_some(), "{}: '{r}' is unbound by default", d.id);
                    }
                    Ok(_) => {}
                    Err(e) => panic!("{}: {e} (in {m:?})", d.id),
                }
                assert!(!keys::display(&r, &km).starts_with('?'), "{}: {r}", d.id);
            }
        }
    }
}

#[test]
fn texts_never_spell_out_a_chord() {
    // Shortcuts must go through {key:...} so they follow the keymap and platform.
    let banned = ["Cmd", "Ctrl", "Super", "Alt+", "Opt+", "Shift+", "\u{2318}", "Command+"];
    for (d, c) in bundled() {
        for m in c.marks() {
            for t in m.texts() {
                let mut plain = t.to_string();
                for p in cast::placeholders(t) {
                    plain = plain.replace(&format!("{{key:{p}}}"), "");
                }
                for b in banned {
                    assert!(!plain.contains(b), "{}: literal chord '{b}' in {t:?}", d.id);
                }
            }
        }
    }
}

#[test]
fn fixed_chords_parse_and_are_not_shadowed_by_the_keymap() {
    let km = Keymap::default();
    for f in keys::FIXED {
        assert!(!f.chords.is_empty(), "{}", f.id);
        for c in f.chords {
            let chord = Chord::parse(c).unwrap_or_else(|e| panic!("{}: {c}: {e}", f.id));
            assert!(km.lookup(&chord).is_none(), "@{} chord {c} is bound to {:?} in the keymap", f.id, km.lookup(&chord));
        }
        assert!(f.mac.starts_with("Cmd") && !f.other.contains("Cmd"), "{}", f.id);
    }
}

#[test]
fn key_display_follows_the_users_keymap() {
    let km = Keymap::build(&[("split_right".into(), vec!["ctrl+alt+s".into()])]);
    assert_eq!(keys::display("split_right", &km), Chord::parse("ctrl+alt+s").unwrap().display());
    let unbound = Keymap::build(&[("split_right".into(), vec!["none".into()])]);
    assert!(keys::display("split_right", &unbound).contains("unbound"));
    assert_eq!(keys::display("=Tab", &km), "Tab");
    assert!(keys::display("nope", &km).starts_with('?'));
    assert!(keys::lookup("=Cmd+Q").is_err(), "literal chords are rejected");
    let segs = keys::segments("Press {key:=Tab}.", &km);
    assert_eq!(segs, vec![keys::Seg::Text("Press ".into()), keys::Seg::Key("Tab".into()), keys::Seg::Text(".".into())]);
}

// ───────────────────────────── format ─────────────────────────────

#[test]
fn markers_round_trip_including_separators() {
    let marks = vec![
        Mark::Caption("Press {key:split_right} | then go".into()),
        Mark::Key("@ask_inline".into(), "Ask | AI".into()),
        Mark::Pane(PaneOp::SplitRight),
        Mark::Pane(PaneOp::SplitDown),
        Mark::Pane(PaneOp::Focus(2)),
        Mark::Pane(PaneOp::Zoom),
        Mark::Tab(TabOp::New("claude: main".into())),
        Mark::Tab(TabOp::Select(1)),
        Mark::Tab(TabOp::Title("aurora".into())),
        Mark::Blocks(BlocksOp::Fold(2)),
        Mark::Blocks(BlocksOp::Hover(3, Some("ask".into()))),
        Mark::Blocks(BlocksOp::Hover(3, None)),
        Mark::Blocks(BlocksOp::Select(1)),
        Mark::Blocks(BlocksOp::Jump(0)),
        Mark::Blocks(BlocksOp::Bottom),
        Mark::Blocks(BlocksOp::Clear),
        Mark::Ai(AiOp::Fix("a | b".into(), "pipes \\ too".into())),
        Mark::Ai(AiOp::Nl("q".into(), "du -ah . | sort -rh".into())),
        Mark::Ai(AiOp::Ask("why?".into())),
        Mark::Ai(AiOp::Clear),
        Mark::Ui(UiOp::Palette("theme no".into())),
        Mark::Ui(UiOp::Preview("curl x | sh".into(), "ye".into())),
        Mark::Ui(UiOp::Panel(Panel {
            place: Place::Left,
            title: "T".into(),
            badge: "SAMPLE".into(),
            lines: vec!["## a".into(), "x = 1".into(), String::new()],
            hints: vec![("1-3".into(), "answer, now".into()), ("{key:new_tab}".into(), "tab".into())],
        })),
        Mark::Ui(UiOp::Clear),
    ];
    for m in marks {
        assert_eq!(Mark::parse(&m.encode()), m, "{}", m.encode());
    }
    // Plain asciinema markers become captions.
    assert_eq!(Mark::parse("Chapter 2"), Mark::Caption("Chapter 2".into()));
}

#[test]
fn parses_asciicast_v2_and_drops_input_events() {
    let src = "{\"version\": 2, \"width\": 100, \"height\": 30, \"timestamp\": 1, \"env\": {\"SHELL\": \"/bin/zsh\"}, \"title\": \"t\"}\n\
               [0.5, \"o\", \"hi \\u001b[1mthere\\u001b[0m \\ud83d\\ude00\"]\n\
               [0.6, \"i\", \"rm -rf /\\r\"]\n\
               [0.7, \"r\", \"80x24\"]\n\
               \n\
               [0.9, \"m\", \"caption:hello\"]\n";
    let c = Cast::parse(src).unwrap();
    assert_eq!((c.cols, c.rows, c.title.as_str()), (100, 30, "t"));
    assert_eq!(c.events.len(), 2, "input and resize events are never replayed");
    assert_eq!(c.events[0].kind, Kind::Output("hi \x1b[1mthere\x1b[0m \u{1F600}".as_bytes().to_vec()));
    assert!(c.events.iter().all(|e| !matches!(&e.kind, Kind::Output(b) if b.starts_with(b"rm"))));
    assert_eq!(c.steps(), vec![0.9]);
    // Errors
    assert!(Cast::parse("").is_err());
    assert!(Cast::parse("{\"version\":1}").is_err());
    assert!(Cast::parse("{\"version\":2}\n[1, \"o\"]").is_err());
    assert!(Cast::parse("{\"version\":2}\n[-1, \"o\", \"x\"]").is_err());
    // Round trip
    let again = Cast::parse(&c.to_asciicast()).unwrap();
    assert_eq!(again.events, c.events);
}

#[test]
fn the_session_recorders_files_play() {
    // The recorder writes `{"version":2,...}` + `[t, "o"|"i", "..."]` lines.
    let dir = std::env::temp_dir().join(format!("rift-tut-rec-{}", std::process::id()));
    let path = dir.join("r.cast");
    let mut rec = crate::tools::recording::Recorder::start(&path, 80, 24).unwrap();
    rec.record_output(b"\x1b]133;A\x07$ ");
    rec.record_input(b"ls\r");
    rec.record_output(b"ls\r\nCargo.toml\r\n");
    rec.finish();
    let mut ui = TutorialUi::new();
    ui.play_file(&path, CELL).unwrap();
    let p = ui.player.as_mut().unwrap();
    assert!(p.title.starts_with("Recording"));
    p.seek(p.duration());
    assert!(screen(p.world()).iter().any(|l| l.contains("Cargo.toml")));
    let _ = std::fs::remove_dir_all(&dir);
}

// ───────────────────────────── player ─────────────────────────────

#[test]
fn plays_pauses_and_changes_speed() {
    let mut p = player("splits");
    assert!(p.playing);
    p.advance(1.0);
    assert!((p.pos() - 1.0).abs() < 1e-9);
    p.handle(TKey::Space);
    assert!(!p.playing);
    p.advance(5.0);
    assert!((p.pos() - 1.0).abs() < 1e-9, "paused: time stands still");
    p.handle(TKey::Space);
    p.handle(TKey::Char('4'));
    assert_eq!(p.speed, 4);
    p.advance(1.0);
    assert!((p.pos() - 5.0).abs() < 1e-9, "4x");
    p.handle(TKey::Char('2'));
    p.advance(1.0);
    assert!((p.pos() - 7.0).abs() < 1e-9, "2x");
    p.handle(TKey::Char('s'));
    assert_eq!(p.speed, 4);
    p.handle(TKey::Char('s'));
    assert_eq!(p.speed, 1);
    p.handle(TKey::Char('3'));
    assert_eq!(p.speed, 1, "only 1x/2x/4x");
}

#[test]
fn plays_to_the_end_and_space_replays() {
    let mut p = player("tabs");
    for _ in 0..1000 {
        p.advance(0.1);
    }
    assert!(p.finished() && !p.playing);
    assert_eq!(p.applied(), p.cast.events.len());
    p.handle(TKey::Space);
    assert!(p.playing && p.pos() == 0.0, "Space at the end restarts");
}

#[test]
fn seeking_backwards_rebuilds_exactly() {
    for (d, _) in bundled() {
        let mut p = player(d.id);
        let dur = p.duration();
        let mid = (dur * 0.6 * 1000.0).round() / 1000.0;
        // Forward playback in small frames ...
        let mut fwd = player(d.id);
        while fwd.pos() < mid - 1e-9 {
            fwd.advance((mid - fwd.pos()).min(0.033));
        }
        // ... equals a seek to the end and back.
        p.seek(dur);
        p.seek(mid);
        assert_eq!(state(&p), state(&fwd), "{}", d.id);
    }
}

#[test]
fn steps_move_between_captions() {
    let mut p = player("blocks");
    let steps = p.steps().to_vec();
    p.handle(TKey::Right);
    assert!((p.pos() - steps[0]).abs() < 1e-9 || (p.pos() - steps[1]).abs() < 1e-9);
    p.seek(steps[2] + 0.1);
    p.handle(TKey::Right);
    assert!((p.pos() - steps[3]).abs() < 1e-9);
    assert_eq!(p.step_index().0, 4);
    p.seek(steps[3] + 2.0);
    p.handle(TKey::Left);
    assert!((p.pos() - steps[3]).abs() < 1e-9, "back to the start of the current step");
    p.handle(TKey::Left);
    assert!((p.pos() - steps[2]).abs() < 1e-9, "then the previous one");
    p.handle(TKey::ShiftRight);
    assert!((p.pos() - (steps[2] + 5.0)).abs() < 1e-9);
    p.handle(TKey::ShiftLeft);
    p.handle(TKey::ShiftLeft);
    assert!((p.pos() - (steps[2] - 5.0).max(0.0)).abs() < 1e-9);
    p.handle(TKey::End);
    assert!(p.finished());
    p.handle(TKey::Char('r'));
    assert!(p.playing && p.pos() == 0.0);
}

#[test]
fn exit_list_and_next_outcomes() {
    let mut p = player("ai");
    assert_eq!(p.handle(TKey::Esc), Outcome::Exit);
    assert_eq!(p.handle(TKey::Char('q')), Outcome::Exit);
    assert_eq!(p.handle(TKey::Char('n')), Outcome::Next);
    assert_eq!(p.handle(TKey::Char('l')), Outcome::List);
    assert_eq!(p.handle(TKey::Char('x')), Outcome::None);
}

#[test]
fn key_overlay_expires() {
    let mut p = player("splits");
    let c = p.cast.clone();
    let t = c.events.iter().find(|e| matches!(e.kind, Kind::Mark(Mark::Key(..)))).unwrap().t;
    p.seek(t);
    assert!(p.key_overlay().is_some());
    p.seek(t + KEY_TTL + 0.01);
    assert!(p.key_overlay().is_none() || p.world().key.as_ref().unwrap().2 > t, "expired unless a newer key came");
}

#[test]
fn demos_reach_their_story_beats() {
    // splits: three panes, then zoom toggled back off.
    let mut p = player("splits");
    p.seek(p.duration());
    assert_eq!(p.world().wm.active_tab().pane_count(), 3);
    assert!(!p.world().wm.active_tab().is_zoomed());
    // tabs: two tabs.
    let mut p = player("tabs");
    p.seek(p.duration());
    assert_eq!(p.world().wm.tabs.len(), 2);
    // blocks: real OSC 133 blocks with demo-clock durations and the failed build.
    let mut p = player("blocks");
    p.seek(p.duration());
    let t = &p.world().wm.active_pane().terminal;
    let b = t.blocks.blocks();
    assert_eq!(b.len(), 5);
    assert_eq!(b[4].exit_code, Some(101));
    assert!(b[2].duration_ms >= 2000, "npm install took {}ms on the demo clock", b[2].duration_ms);
    assert!(b[2].collapsed);
    // preview: the real classifier, critical with "yes" typed, then dismissed.
    let mut p = player("preview");
    let c = p.cast.clone();
    let yes = c.events.iter().find(|e| matches!(&e.kind, Kind::Mark(Mark::Ui(UiOp::Preview(_, t))) if t == "yes")).unwrap().t;
    p.seek(yes);
    let pv = p.world().preview.as_ref().expect("critical preview shown");
    assert_eq!(pv.severity, crate::tools::exec_preview::Severity::Critical);
    assert!(pv.needs_typed_confirm());
    p.seek(p.duration());
    assert!(p.world().preview.is_none());
    // palette: the real palette with the query typed.
    let mut p = player("palette");
    let c = p.cast.clone();
    let at = c.events.iter().find(|e| matches!(&e.kind, Kind::Mark(Mark::Ui(UiOp::Palette(q))) if q == "spl")).unwrap().t;
    p.seek(at);
    assert_eq!(p.world().palette.as_ref().unwrap().query, "spl");
    // ai: the fix bar, then the # ghost.
    let mut p = player("ai");
    let c = p.cast.clone();
    let at = c.events.iter().find(|e| matches!(&e.kind, Kind::Mark(Mark::Ai(AiOp::Fix(..))))).unwrap().t;
    p.seek(at);
    assert!(p.world().inline_ai.fix.current.is_some());
    // time warp: leaves the alternate screen with the live view intact.
    let mut p = player("time-warp");
    p.seek(p.duration());
    let t = &p.world().wm.active_pane().terminal;
    assert!(!t.is_alt_screen());
    assert!(screen(p.world()).iter().any(|l| l.contains("INFO")));
}

/// The safety property: playback has no channel to a shell.
#[test]
fn playback_never_writes_to_a_pty() {
    let all_keys = [
        TKey::Space, TKey::Left, TKey::Right, TKey::ShiftLeft, TKey::ShiftRight, TKey::Up, TKey::Down, TKey::Enter,
        TKey::Home, TKey::End, TKey::Char('1'), TKey::Char('2'), TKey::Char('4'), TKey::Char('s'), TKey::Char('r'),
        TKey::Char('y'), TKey::Char('\r'), TKey::Char('a'),
    ];
    for (d, _) in bundled() {
        let mut p = player(d.id);
        for k in all_keys {
            p.handle(k);
            p.advance(0.7);
        }
        p.seek(p.duration());
        let w = p.world();
        assert!(w.wm.tabs.iter().all(|t| t.panes().iter().all(|pane| matches!(pane.pty, PtyKind::Inert))), "{}: only inert panes", d.id);
        assert!(w.wm.tabs.iter().all(|t| t.panes().iter().all(|pane| pane.terminal.response_queue.is_empty())), "{}: terminal replies are discarded", d.id);
        assert!(w.wm.tabs.iter().all(|t| t.panes().iter().all(|pane| pane.act.last_input.is_none())), "{}: nothing was written to any pane", d.id);
    }
    // Recordings' input events (what the user typed) are dropped at parse time.
    let c = Cast::parse("{\"version\":2}\n[0.1, \"i\", \"rm -rf ~\\r\"]\n").unwrap();
    assert!(c.events.is_empty());
}

#[test]
fn tutorial_ui_list_play_next_and_exit() {
    let mut ui = TutorialUi::new();
    assert!(!ui.visible());
    ui.open_picker();
    assert!(ui.visible() && !ui.playing());
    ui.handle_key(TKey::Down, CELL);
    ui.handle_key(TKey::Up, CELL);
    ui.handle_key(TKey::Up, CELL);
    assert_eq!(ui.picker, Some(DEMOS.len() - 1), "wraps");
    ui.handle_key(TKey::Char('1'), CELL);
    assert_eq!(ui.player.as_ref().and_then(|p| p.id), Some(DEMOS[0].id));
    assert!(ui.playing() && ui.picker.is_none());
    ui.handle_key(TKey::Char('n'), CELL);
    assert_eq!(ui.player.as_ref().and_then(|p| p.id), Some(DEMOS[1].id));
    ui.handle_key(TKey::Char('l'), CELL);
    assert_eq!(ui.picker, Some(1), "back to the list on the current demo");
    ui.handle_key(TKey::Enter, CELL);
    assert!(ui.playing());
    ui.handle_key(TKey::Esc, CELL);
    assert!(!ui.visible(), "Esc leaves the tutorial");
    assert!(ui.play("nope", CELL).is_err());
}

#[test]
fn tick_uses_wall_clock_capped() {
    let mut ui = TutorialUi::new();
    ui.play("blocks", CELL).unwrap();
    let t0 = std::time::Instant::now();
    ui.tick(t0);
    ui.tick(t0 + std::time::Duration::from_millis(100));
    let pos = ui.player.as_ref().unwrap().pos();
    assert!((pos - 0.1).abs() < 1e-6, "{pos}");
    ui.tick(t0 + std::time::Duration::from_secs(10));
    let pos2 = ui.player.as_ref().unwrap().pos();
    assert!(pos2 - pos <= 0.25 + 1e-6, "a stalled frame skips at most 250ms");
}

#[test]
fn startup_arguments() {
    assert!(set_startup("blocks").is_ok());
    let ui = TutorialUi::for_first_window();
    assert!(ui.playing(), "pending --demo shows the player");
    assert!(!startup_pending(), "taken by the first window");
    assert!(set_startup("no-such-demo").is_err());
    assert!(list_text().contains("time-warp"));
}

// ───────────────────────────── pixels ─────────────────────────────

#[test]
fn renders_every_demo_and_the_list() {
    let font = crate::config::find_font_path();
    if !std::path::Path::new(&font).exists() {
        return;
    }
    let theme = crate::config::Config::theme_by_name("rift-neon").unwrap();
    let mut r = crate::renderer::Renderer::new(&font, 14.0, theme.clone());
    let km = Keymap::default();
    let (w, h) = (1100, 760);
    for (d, _) in bundled() {
        let mut p = player(d.id);
        for frac in [0.05, 0.3, 0.55, 0.8, 1.0] {
            p.seek(p.duration() * frac);
            let mut buf = vec![0u32; w * h];
            view::draw_player(&mut p, &mut buf, w, h, &mut r, &km);
            let distinct: std::collections::HashSet<u32> = buf.iter().step_by(7).copied().collect();
            assert!(distinct.len() > 8, "{} at {frac}: frame looks blank", d.id);
        }
    }
    // Tiny window: clipped, no panic.
    let mut p = player("splits");
    let mut buf = vec![0u32; 300 * 200];
    view::draw_player(&mut p, &mut buf, 300, 200, &mut r, &km);
    let mut buf = vec![0u32; w * h];
    view::draw_picker(2, &TutorialUi::catalog(), &mut buf, w, h, &mut r.font, &theme);
}
