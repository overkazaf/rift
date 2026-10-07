pub struct DiffResult {
    pub lines: Vec<DiffLine>,
}

pub enum DiffLine {
    Same(String),
    Added(String),
    Removed(String),
}

pub fn diff(old: &str, new: &str) -> DiffResult {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let m = old_lines.len();
    let n = new_lines.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if old_lines[i - 1] == new_lines[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    let mut result = Vec::new();
    let mut i = m;
    let mut j = n;

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old_lines[i - 1] == new_lines[j - 1] {
            result.push(DiffLine::Same(old_lines[i - 1].to_string()));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            result.push(DiffLine::Added(new_lines[j - 1].to_string()));
            j -= 1;
        } else {
            result.push(DiffLine::Removed(old_lines[i - 1].to_string()));
            i -= 1;
        }
    }
    result.reverse();

    DiffResult { lines: result }
}

pub fn render_diff(result: &DiffResult) -> String {
    let mut output = String::new();
    for line in &result.lines {
        match line {
            DiffLine::Same(s) => {
                output.push_str(&format!("  {}\n", s));
            }
            DiffLine::Added(s) => {
                output.push_str(&format!("\x1b[32m+ {}\x1b[0m\n", s));
            }
            DiffLine::Removed(s) => {
                output.push_str(&format!("\x1b[31m- {}\x1b[0m\n", s));
            }
        }
    }
    output
}
