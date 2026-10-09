//! Item 5: command blocks: folding, navigation, copy-output, scrollback trimming.
use super::{pane, Soft};
use crate::blocks_ui::view;

fn stream(n: usize, lines: usize, width: usize) -> Vec<u8> {
    let mut s = String::new();
    for k in 0..n {
        s += &format!("\x1b]133;A\x07$ \x1b]133;B\x07cmd{k}\r\n\x1b]133;C\x07");
        for i in 0..lines {
            let mut l = format!("blk{k} line{i}");
            while l.len() < width { l.push('.'); }
            s += &l;
            s += "\r\n";
        }
        s += &format!("\x1b]133;D;{}\x07", if k % 7 == 3 { 1 } else { 0 });
    }
    s += "\x1b]133;A\x07$ \x1b]133;B\x07";
    s.into_bytes()
}

#[test]
fn fold_view_and_navigation_offsets() {
    let mut s = Soft::new("blocks");
    let mut p = pane(80, 10);
    p.feed(&stream(6, 25, 0));
    let t = &mut p.terminal;
    s.check("six_blocks", t.blocks.blocks().len() == 6, format!("{}", t.blocks.blocks().len()));
    let total = t.scrollback.len() + t.grid.len();
    // Navigation: for every block jump so command_line is the top row; check the first view row.
    let mut bad = Vec::new();
    for collapsed in [false, true] {
        if collapsed {
            t.blocks.toggle_collapse(1);
            t.blocks.toggle_collapse(3);
        }
        let folds = view::folds_of(&t.blocks);
        for (i, b) in t.blocks.blocks().iter().enumerate() {
            let off = view::scroll_offset_for_top(total, t.rows, &folds, b.command_line);
            let v = view::build_view(total, t.rows, off, &folds);
            let top = v.first().map(|r| r.abs());
            let max_top = total - t.rows; // cannot start later than this (non folded)
            if top != Some(b.command_line) && b.command_line <= max_top.saturating_sub(0) && off < t.scrollback.len() {
                bad.push((collapsed, i, b.command_line, top));
            }
        }
    }
    s.check("jump_places_block_command_line_at_top", bad.is_empty(), format!("mismatches (collapsed,idx,cmd_line,top): {bad:?}"));
    // Fold hides exactly the output lines.
    let folds = view::folds_of(&t.blocks);
    s.check("two_folds", folds.len() == 2, format!("{folds:?}"));
    let b1 = &t.blocks.blocks()[1];
    s.check("fold_range_equals_output_range", folds[0].start == b1.output_start && folds[0].end == b1.output_end, format!("{:?} vs {}..{}", folds[0], b1.output_start, b1.output_end));
    // Every absolute line appears exactly once (or hidden in a fold) when scrolled through the whole buffer
    let mut seen = vec![0u32; total];
    let mut off = 0;
    loop {
        let v = view::build_view(total, t.rows, off, &folds);
        for r in &v {
            match r { view::ViewRow::Line(l) => seen[*l] += 1, view::ViewRow::Folded { start, hidden, .. } => for l in *start..*start + *hidden { seen[l] += 1 } }
        }
        if v.first().map(|r| r.abs()) == Some(0) { break; }
        off += 1;
        if off > total { break; }
    }
    s.check("full_scroll_covers_every_line", seen.iter().all(|c| *c >= 1), format!("uncovered lines: {}", seen.iter().filter(|c| **c == 0).count()));
    // placeholder row for a fold in the visible window
    t.scroll_offset = 0;
    let fv = view::folded_view(t);
    s.info("folded_view_at_bottom", format!("{}", fv.is_some()));
    s.finish();
}

#[test]
fn copy_output_wrapped_and_edge_lines() {
    let mut s = Soft::new("blocks");
    let mk = |output: &str, cols: usize| {
        let mut p = pane(cols, 12);
        p.feed(format!("\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ").as_bytes());
        let t = &p.terminal;
        let b = &t.blocks.blocks()[0];
        crate::blocks_ui::output_text(t, b.output_start, b.output_end)
    };
    let long = "x".repeat(200);
    let got = mk(&long, 80);
    s.check("soft_wrapped_long_line_rejoined", got == long, format!("len={} newlines={}", got.len(), got.matches('\n').count()));
    // exactly 80 wide line followed by a SEPARATE short line: must stay two lines
    let exact = "A".repeat(80);
    let got = mk(&format!("{exact}\r\nB"), 80);
    s.check("exact_width_line_then_newline_not_merged", got == format!("{exact}\nB"), format!("{got:?}"));
    // CJK text that wraps because the wide char does not fit in the last column
    let cjk: String = format!("a{}", "中".repeat(60));
    let got = mk(&cjk, 80);
    s.check("cjk_soft_wrap_rejoined", got == cjk, format!("{got:?}"));
    // trailing spaces and blank lines preserved/trimmed sensibly
    let got = mk("a   \r\n\r\n\r\nb", 80);
    s.check("blank_lines_kept", got == "a\n\n\nb", format!("{got:?}"));
    // tabs
    let got = mk("a\tb", 80);
    s.info("tab_output", format!("{got:?}"));
    s.check("tab_expanded_to_spaces_in_copy", !got.contains('\t'), "terminal expands tabs by cursor movement, so copied text has spaces not \\t (documented limitation)");
    s.finish();
}

#[test]
fn scrollback_trimming_keeps_blocks_consistent() {
    let mut s = Soft::new("blocks");
    let mut p = pane(80, 24);
    let n = 140usize;
    let lines = 100usize;
    p.feed(&stream(n, lines, 0));
    let t = &p.terminal;
    s.info("scrollback", format!("scrollback={} blocks={}", t.scrollback.len(), t.blocks.blocks().len()));
    s.check("scrollback_capped_10000", t.scrollback.len() == 10_000, format!("{}", t.scrollback.len()));
    let mut wrong = Vec::new();
    let mut stale = Vec::new();
    for (i, b) in t.blocks.blocks().iter().enumerate() {
        let out = crate::blocks_ui::output_text(t, b.output_start, b.output_end);
        let first = out.lines().next().unwrap_or("");
        // Blocks scrolled out of the buffer are dropped, so the list index no
        // longer equals the stream index: take it from the command text.
        let k: usize = b.command.trim_start_matches("cmd").parse().unwrap_or(i);
        let want = format!("blk{} line0", k);
        let want_prefix = format!("blk{} ", k);
        if !first.starts_with(&want_prefix) {
            if b.output_start == 0 && b.output_end == 0 || b.output_end < 5 {
                stale.push((i, b.command.clone(), b.output_start, b.output_end, first.chars().take(20).collect::<String>()));
            } else {
                wrong.push((i, first.chars().take(20).collect::<String>()));
            }
        } else if first != want && b.output_start > 0 {
            wrong.push((i, first.to_string()));
        }
        let _ = want;
    }
    s.check("no_block_copies_foreign_output", wrong.is_empty(), format!("blocks whose copy output belongs to another block: {wrong:?}"));
    s.check("fully_trimmed_blocks_dropped", stale.is_empty(), format!("{} blocks fully/partly scrolled out of the 10k buffer are still listed with clamped ranges (first 3: {:?})", stale.len(), &stale[..stale.len().min(3)]));
    let first_partial = t.blocks.blocks().iter().position(|b| b.output_start == 0);
    s.info("first_clamped_block", format!("{first_partial:?}"));
    // clear buffer
    let mut p2 = pane(80, 24);
    p2.feed(&stream(5, 50, 0));
    p2.terminal.clear_buffer();
    let t2 = &p2.terminal;
    let bad: Vec<_> = t2.blocks.blocks().iter().enumerate().filter(|(_, b)| b.output_start == 0 && b.output_end == 0).map(|(i, _)| i).collect();
    s.check("clear_buffer_drops_gone_blocks", t2.blocks.blocks().is_empty(), format!("after Clear Buffer {} blocks remain pointing at cleared lines: {bad:?}", t2.blocks.blocks().len()));
    s.finish();
}

#[test]
fn many_blocks_cap_and_perf() {
    let mut s = Soft::new("blocks");
    let mut p = pane(80, 24);
    let t0 = std::time::Instant::now();
    p.feed(&stream(5000, 1, 0));
    s.info("5000_blocks_ms", format!("{}", t0.elapsed().as_millis()));
    let n = p.terminal.blocks.blocks().len();
    s.check("block_cap_2000", n == 2000, format!("{n}"));
    s.finish();
}
