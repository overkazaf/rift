//! Unified-diff parser for `git diff` output (hunks, +/- lines, renames,
//! binary files, deletions, "\ No newline at end of file").
//!
//! The parser is deliberately forgiving: unknown extended-header lines are
//! ignored, and hunk bodies are consumed by their `@@ -a,b +c,d @@` line counts
//! so that content lines that merely *look* like headers (`--- foo`,
//! `diff --git ...`) are never misread.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
}

impl FileStatus {
    /// One-letter badge: A / M / D / R / C.
    pub fn letter(self) -> char {
        match self {
            FileStatus::Added => 'A',
            FileStatus::Modified => 'M',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
            FileStatus::Copied => 'C',
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Context,
    Add,
    Del,
    /// `\ No newline at end of file` (attaches to the previous line).
    NoNewline,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    /// Text after the closing `@@` (usually the enclosing function).
    pub section: String,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    pub fn header(&self) -> String {
        let sec = if self.section.is_empty() { String::new() } else { format!(" {}", self.section) };
        format!("@@ -{},{} +{},{} @@{}", self.old_start, self.old_len, self.new_start, self.new_len, sec)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileDiff {
    /// Path before the change (equals `new_path` unless renamed/copied).
    pub old_path: String,
    /// Path after the change (for deletions: the deleted path).
    pub new_path: String,
    pub status: FileStatus,
    pub binary: bool,
    pub similarity: Option<u32>,
    /// e.g. `mode 100644 -> 100755`.
    pub mode_note: Option<String>,
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
    /// The file's complete patch text (header + hunks), for "Copy patch".
    pub raw: String,
}

/// A row of the flattened diff view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row<'a> {
    Hunk(usize, &'a Hunk),
    Line(&'a DiffLine),
}

impl FileDiff {
    /// Total rows when flattened (one header row per hunk + its lines).
    pub fn row_count(&self) -> usize {
        self.hunks.iter().map(|h| 1 + h.lines.len()).sum()
    }

    /// Flattened row index of each hunk header.
    pub fn hunk_rows(&self) -> Vec<usize> {
        let mut at = 0;
        self.hunks
            .iter()
            .map(|h| {
                let r = at;
                at += 1 + h.lines.len();
                r
            })
            .collect()
    }

    /// Up to `count` flattened rows starting at row `start`.
    pub fn rows(&self, start: usize, count: usize) -> Vec<Row<'_>> {
        let mut out = Vec::with_capacity(count.min(256));
        let mut at = 0usize;
        for (hi, h) in self.hunks.iter().enumerate() {
            let span = 1 + h.lines.len();
            if at + span <= start {
                at += span;
                continue;
            }
            if at >= start {
                out.push(Row::Hunk(hi, h));
                if out.len() >= count {
                    return out;
                }
            }
            let skip = start.saturating_sub(at + 1);
            for l in h.lines.iter().skip(skip) {
                out.push(Row::Line(l));
                if out.len() >= count {
                    return out;
                }
            }
            at += span;
        }
        out
    }

    /// `old -> new` for renames, the path otherwise.
    pub fn display_path(&self) -> String {
        if matches!(self.status, FileStatus::Renamed | FileStatus::Copied) && self.old_path != self.new_path {
            format!("{} \u{2192} {}", self.old_path, self.new_path)
        } else {
            self.new_path.clone()
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct ParsedDiff {
    pub files: Vec<FileDiff>,
    /// The producer cut the diff short (size cap): the last file may be partial.
    pub truncated: bool,
}

impl ParsedDiff {
    pub fn totals(&self) -> (usize, usize, usize) {
        let a = self.files.iter().map(|f| f.added).sum();
        let r = self.files.iter().map(|f| f.removed).sum();
        (self.files.len(), a, r)
    }

    /// All files' patches concatenated.
    pub fn full_patch(&self) -> String {
        self.files.iter().map(|f| f.raw.as_str()).collect()
    }
}

/// Undo git's C-style path quoting (`"a\tb"`, `"\303\251"`). Unquoted input is returned as is.
pub fn unquote_path(s: &str) -> String {
    let s = s.trim_end_matches('\r');
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return s.to_string();
    };
    let b = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        match b[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 3 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                    v = v * 8 + (b[i] - b'0') as u32;
                    i += 1;
                    n += 1;
                }
                out.push(v as u8);
                continue;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Strip a leading `a/` or `b/` (git's default prefixes) from an unquoted path.
fn strip_ab(p: &str) -> &str {
    p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(p)
}

/// Paths from `diff --git a/X b/Y` (what follows `diff --git `).
fn parse_git_header(rest: &str) -> (String, String) {
    let rest = rest.trim_end_matches('\r');
    // Quoted forms: "a/x" "b/y", "a/x" b/y, a/x "b/y".
    if rest.starts_with('"') {
        // find end of the first quoted token
        let bytes = rest.as_bytes();
        let mut i = 1;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                break;
            }
            i += 1;
        }
        let first = &rest[..(i + 1).min(rest.len())];
        let second = rest.get(i + 1..).unwrap_or("").trim_start();
        return (strip_ab(&unquote_path(first)).to_string(), strip_ab(&unquote_path(second)).to_string());
    }
    if rest.contains(" \"b/") {
        if let Some(idx) = rest.find(" \"b/") {
            return (strip_ab(&unquote_path(&rest[..idx])).to_string(), strip_ab(&unquote_path(&rest[idx + 1..])).to_string());
        }
    }
    // Unquoted: prefer the split where both sides are identical (no rename).
    let mut best: Option<(String, String)> = None;
    for (idx, _) in rest.match_indices(" b/") {
        let l = rest[..idx].strip_prefix("a/").unwrap_or(&rest[..idx]);
        let r = &rest[idx + 3..];
        if l == r {
            return (l.to_string(), r.to_string());
        }
        if best.is_none() {
            best = Some((l.to_string(), r.to_string()));
        }
    }
    best.unwrap_or_else(|| (strip_ab(rest).to_string(), strip_ab(rest).to_string()))
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    // "12,5" or "12"
    let s = s.trim_start_matches(['-', '+']);
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// `@@ -a,b +c,d @@ section`
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, section) = rest.split_once(" @@")?;
    let (o, n) = ranges.split_once(' ')?;
    let (old_start, old_len) = parse_range(o)?;
    let (new_start, new_len) = parse_range(n)?;
    Some(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
        section: section.trim().to_string(),
        lines: Vec::new(),
    })
}

struct Builder {
    file: FileDiff,
    raw_start: usize,
    saw_old: Option<String>,
    saw_new: Option<String>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    old_mode: Option<String>,
    new_mode: Option<String>,
}

impl Builder {
    fn finish(mut self, raw: String) -> FileDiff {
        let f = &mut self.file;
        if let Some(p) = self.rename_from.take() {
            f.old_path = p;
        } else if let Some(p) = self.saw_old.take() {
            if f.status != FileStatus::Added {
                f.old_path = p;
            }
        }
        if let Some(p) = self.rename_to.take() {
            f.new_path = p;
        } else if let Some(p) = self.saw_new.take() {
            if f.status != FileStatus::Deleted {
                f.new_path = p;
            }
        }
        match f.status {
            FileStatus::Deleted => f.new_path = f.old_path.clone(),
            FileStatus::Added => f.old_path = f.new_path.clone(),
            _ => {}
        }
        if let (Some(o), Some(n)) = (&self.old_mode, &self.new_mode) {
            if o != n && f.status == FileStatus::Modified {
                f.mode_note = Some(format!("mode {o} \u{2192} {n}"));
            }
        }
        f.raw = raw;
        self.file
    }
}

/// Parse `git diff` output. `truncated` marks input that was cut by a size cap.
pub fn parse_unified(text: &str, truncated: bool) -> ParsedDiff {
    let mut files: Vec<FileDiff> = Vec::new();
    // A trailing newline terminates the last line; it does not start another.
    let lines: Vec<&str> = text.strip_suffix('\n').unwrap_or(text).split('\n').collect();
    // Byte offset of each line start, for slicing raw patches.
    let mut offs: Vec<usize> = Vec::with_capacity(lines.len() + 1);
    let mut o = 0usize;
    for l in &lines {
        offs.push(o);
        o += l.len() + 1;
    }
    offs.push(text.len());

    let mut cur: Option<Builder> = None;
    let mut i = 0usize;
    let close = |cur: &mut Option<Builder>, end_line: usize, files: &mut Vec<FileDiff>, offs: &[usize]| {
        if let Some(b) = cur.take() {
            let end = offs[end_line.min(offs.len() - 1)].min(text.len());
            let mut raw = text[b.raw_start..end].to_string();
            if !raw.is_empty() && !raw.ends_with('\n') {
                raw.push('\n');
            }
            files.push(b.finish(raw));
        }
    };

    while i < lines.len() {
        let line = lines[i];
        if let Some(rest) = line.strip_prefix("diff --git ") {
            close(&mut cur, i, &mut files, &offs);
            let (a, b) = parse_git_header(rest);
            cur = Some(Builder {
                file: FileDiff {
                    old_path: a,
                    new_path: b,
                    status: FileStatus::Modified,
                    binary: false,
                    similarity: None,
                    mode_note: None,
                    hunks: Vec::new(),
                    added: 0,
                    removed: 0,
                    raw: String::new(),
                },
                raw_start: offs[i],
                saw_old: None,
                saw_new: None,
                rename_from: None,
                rename_to: None,
                old_mode: None,
                new_mode: None,
            });
            i += 1;
            continue;
        }
        let Some(b) = cur.as_mut() else {
            i += 1;
            continue;
        };
        if line.starts_with("@@ ") {
            if let Some(mut h) = parse_hunk_header(line) {
                i += 1;
                let (mut old_left, mut new_left) = (h.old_len, h.new_len);
                let (mut on, mut nn) = (h.old_start, h.new_start);
                while i < lines.len() {
                    let l = lines[i];
                    if l.starts_with('\\') {
                        h.lines.push(DiffLine { kind: LineKind::NoNewline, text: l.trim_start_matches('\\').trim().to_string(), old_no: None, new_no: None });
                        i += 1;
                        continue;
                    }
                    if old_left == 0 && new_left == 0 {
                        break;
                    }
                    let (kind, body) = match l.as_bytes().first() {
                        Some(b'+') => (LineKind::Add, &l[1..]),
                        Some(b'-') => (LineKind::Del, &l[1..]),
                        Some(b' ') => (LineKind::Context, &l[1..]),
                        None => (LineKind::Context, ""), // blank context line with stripped space
                        _ => break,                        // malformed: stop the hunk
                    };
                    let body = body.trim_end_matches('\r').to_string();
                    match kind {
                        LineKind::Add => {
                            if new_left == 0 {
                                break;
                            }
                            new_left -= 1;
                            h.lines.push(DiffLine { kind, text: body, old_no: None, new_no: Some(nn) });
                            nn += 1;
                            b.file.added += 1;
                        }
                        LineKind::Del => {
                            if old_left == 0 {
                                break;
                            }
                            old_left -= 1;
                            h.lines.push(DiffLine { kind, text: body, old_no: Some(on), new_no: None });
                            on += 1;
                            b.file.removed += 1;
                        }
                        _ => {
                            if old_left == 0 || new_left == 0 {
                                break;
                            }
                            old_left -= 1;
                            new_left -= 1;
                            h.lines.push(DiffLine { kind, text: body, old_no: Some(on), new_no: Some(nn) });
                            on += 1;
                            nn += 1;
                        }
                    }
                    i += 1;
                }
                b.file.hunks.push(h);
                continue;
            }
            i += 1;
            continue;
        }
        // Extended header lines (only meaningful before the first hunk).
        if b.file.hunks.is_empty() {
            if let Some(m) = line.strip_prefix("new file mode ") {
                b.file.status = FileStatus::Added;
                b.new_mode = Some(m.trim().to_string());
            } else if let Some(m) = line.strip_prefix("deleted file mode ") {
                b.file.status = FileStatus::Deleted;
                b.old_mode = Some(m.trim().to_string());
            } else if let Some(m) = line.strip_prefix("old mode ") {
                b.old_mode = Some(m.trim().to_string());
            } else if let Some(m) = line.strip_prefix("new mode ") {
                b.new_mode = Some(m.trim().to_string());
            } else if let Some(s) = line.strip_prefix("similarity index ") {
                b.file.similarity = s.trim().trim_end_matches('%').parse().ok();
            } else if let Some(p) = line.strip_prefix("rename from ") {
                b.file.status = FileStatus::Renamed;
                b.rename_from = Some(unquote_path(p));
            } else if let Some(p) = line.strip_prefix("rename to ") {
                b.file.status = FileStatus::Renamed;
                b.rename_to = Some(unquote_path(p));
            } else if let Some(p) = line.strip_prefix("copy from ") {
                b.file.status = FileStatus::Copied;
                b.rename_from = Some(unquote_path(p));
            } else if let Some(p) = line.strip_prefix("copy to ") {
                b.file.status = FileStatus::Copied;
                b.rename_to = Some(unquote_path(p));
            } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                b.file.binary = true;
            } else if let Some(p) = line.strip_prefix("--- ") {
                let p = unquote_path(p);
                if p != "/dev/null" {
                    b.saw_old = Some(strip_ab(&p).to_string());
                }
            } else if let Some(p) = line.strip_prefix("+++ ") {
                let p = unquote_path(p);
                if p != "/dev/null" {
                    b.saw_new = Some(strip_ab(&p).to_string());
                }
            }
        }
        i += 1;
    }
    close(&mut cur, lines.len(), &mut files, &offs);
    ParsedDiff { files, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODIFIED: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 111..222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@ fn main() {
 one
-two
+TWO
+two and a half
 three
 four
@@ -20,2 +21,2 @@
 x
-y
+z
";

    #[test]
    fn parses_hunks_lines_and_counts() {
        let d = parse_unified(MODIFIED, false);
        assert_eq!(d.files.len(), 1);
        let f = &d.files[0];
        assert_eq!(f.new_path, "src/lib.rs");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!((f.added, f.removed), (3, 2));
        assert_eq!(f.hunks.len(), 2);
        let h = &f.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (1, 4, 1, 5));
        assert_eq!(h.section, "fn main() {");
        assert_eq!(h.lines[0].kind, LineKind::Context);
        assert_eq!((h.lines[0].old_no, h.lines[0].new_no), (Some(1), Some(1)));
        assert_eq!((h.lines[1].kind, h.lines[1].old_no, h.lines[1].new_no), (LineKind::Del, Some(2), None));
        assert_eq!((h.lines[2].kind, h.lines[2].old_no, h.lines[2].new_no), (LineKind::Add, None, Some(2)));
        assert_eq!(h.lines[3].new_no, Some(3));
        assert_eq!(f.hunks[1].new_start, 21);
        assert_eq!(f.raw, MODIFIED);
        assert_eq!(h.header(), "@@ -1,4 +1,5 @@ fn main() {");
    }

    #[test]
    fn rows_flatten_and_window() {
        let f = &parse_unified(MODIFIED, false).files[0];
        assert_eq!(f.row_count(), (1 + 6) + (1 + 3));
        assert_eq!(f.hunk_rows(), vec![0, 7]);
        let all = f.rows(0, 100);
        assert_eq!(all.len(), 11);
        assert!(matches!(all[0], Row::Hunk(0, _)));
        assert!(matches!(all[7], Row::Hunk(1, _)));
        let w = f.rows(4, 3);
        assert_eq!(w.len(), 3);
        assert!(matches!(w[0], Row::Line(l) if l.text == "two and a half"));
        let w = f.rows(6, 3);
        assert!(matches!(w[0], Row::Line(l) if l.text == "four"));
        assert!(matches!(w[1], Row::Hunk(1, _)));
        assert!(f.rows(50, 3).is_empty());
    }

    #[test]
    fn added_and_deleted_files() {
        let t = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
diff --git a/old.txt b/old.txt
deleted file mode 100644
index e69de29..0000000
--- a/old.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
";
        let d = parse_unified(t, false);
        assert_eq!(d.files.len(), 2);
        assert_eq!((d.files[0].status, d.files[0].new_path.as_str(), d.files[0].added), (FileStatus::Added, "new.txt", 2));
        assert_eq!(d.files[0].hunks[0].lines[1].new_no, Some(2));
        assert_eq!((d.files[1].status, d.files[1].new_path.as_str(), d.files[1].removed), (FileStatus::Deleted, "old.txt", 1));
        assert_eq!(d.totals(), (2, 2, 1));
        assert_eq!(d.full_patch(), t);
    }

    #[test]
    fn rename_with_and_without_edits() {
        let t = "\
diff --git a/a b.txt b/c d.txt
similarity index 100%
rename from a b.txt
rename to c d.txt
diff --git a/x.rs b/y.rs
similarity index 88%
rename from x.rs
rename to y.rs
index 1..2 100644
--- a/x.rs
+++ b/y.rs
@@ -1 +1 @@
-old
+new
";
        let d = parse_unified(t, false);
        assert_eq!(d.files.len(), 2);
        let a = &d.files[0];
        assert_eq!((a.status, a.old_path.as_str(), a.new_path.as_str(), a.similarity), (FileStatus::Renamed, "a b.txt", "c d.txt", Some(100)));
        assert!(a.hunks.is_empty());
        let b = &d.files[1];
        assert_eq!((b.old_path.as_str(), b.new_path.as_str(), b.similarity), ("x.rs", "y.rs", Some(88)));
        assert_eq!((b.added, b.removed), (1, 1));
        assert_eq!(b.display_path(), "x.rs \u{2192} y.rs");
    }

    #[test]
    fn binary_files_and_mode_changes() {
        let t = "\
diff --git a/img.png b/img.png
index 1..2 100644
Binary files a/img.png and b/img.png differ
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
diff --git a/blob.bin b/blob.bin
new file mode 100644
index 0000000..abc
Binary files /dev/null and b/blob.bin differ
";
        let d = parse_unified(t, false);
        assert_eq!(d.files.len(), 3);
        assert!(d.files[0].binary && d.files[0].hunks.is_empty());
        assert_eq!(d.files[1].mode_note.as_deref(), Some("mode 100644 \u{2192} 100755"));
        assert!(d.files[2].binary);
        assert_eq!(d.files[2].status, FileStatus::Added);
        assert_eq!(d.files[2].new_path, "blob.bin");
    }

    #[test]
    fn no_newline_marker_attaches_without_counting() {
        let t = "\
diff --git a/f b/f
index 1..2 100644
--- a/f
+++ b/f
@@ -1 +1 @@
-a
\\ No newline at end of file
+b
\\ No newline at end of file
";
        let f = &parse_unified(t, false).files[0];
        let kinds: Vec<LineKind> = f.hunks[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![LineKind::Del, LineKind::NoNewline, LineKind::Add, LineKind::NoNewline]);
        assert_eq!((f.added, f.removed), (1, 1));
        assert_eq!(f.hunks[0].lines[1].text, "No newline at end of file");
    }

    #[test]
    fn content_that_looks_like_headers_stays_in_the_hunk() {
        let t = "\
diff --git a/f b/f
index 1..2 100644
--- a/f
+++ b/f
@@ -1,2 +1,2 @@
 keep
--- not a header
+++ not a header
";
        // ` keep`, `--- ...` (deleted `-- not a header`) and `+++ ...` (added `++ not a header`).
        let d = parse_unified(t, false);
        assert_eq!(d.files.len(), 1, "{:?}", d.files);
        let h = &d.files[0].hunks[0];
        assert_eq!(h.lines.len(), 3);
        assert_eq!(h.lines[1].kind, LineKind::Del);
        assert_eq!(h.lines[1].text, "-- not a header");
        assert_eq!(h.lines[2].text, "++ not a header");
        assert_eq!(h.lines[2].kind, LineKind::Add);
    }

    #[test]
    fn quoted_paths_are_unescaped() {
        assert_eq!(unquote_path("\"a\\tb\""), "a\tb");
        assert_eq!(unquote_path("\"caf\\303\\251.txt\""), "caf\u{e9}.txt");
        assert_eq!(unquote_path("plain"), "plain");
        let t = "\
diff --git \"a/we\\\"ird.txt\" \"b/we\\\"ird.txt\"
new file mode 100644
--- /dev/null
+++ \"b/we\\\"ird.txt\"
@@ -0,0 +1 @@
+x
";
        let f = &parse_unified(t, false).files[0];
        assert_eq!(f.new_path, "we\"ird.txt");
        assert_eq!(f.status, FileStatus::Added);
    }

    #[test]
    fn crlf_and_empty_input_and_truncation_flag() {
        assert!(parse_unified("", false).files.is_empty());
        assert!(parse_unified("garbage\nmore", true).truncated);
        let t = "diff --git a/f b/f\r\nindex 1..2 100644\r\n--- a/f\r\n+++ b/f\r\n@@ -1 +1 @@\r\n-a\r\n+b\r\n";
        let f = &parse_unified(t, false).files[0];
        assert_eq!(f.new_path, "f");
        assert_eq!(f.hunks[0].lines[0].text, "a");
    }

    #[test]
    fn truncated_hunk_does_not_panic() {
        let t = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,10 +1,10 @@\n a\n-b\n";
        let d = parse_unified(t, true);
        assert_eq!(d.files[0].hunks[0].lines.len(), 2);
    }
}
