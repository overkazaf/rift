pub struct FuzzyMatch {
    pub index: usize,
    pub score: i32,
    pub positions: Vec<usize>,
}

pub struct FuzzyMatcher;

impl FuzzyMatcher {
    pub fn score(pattern: &str, text: &str) -> Option<FuzzyMatch> {
        if pattern.is_empty() {
            return Some(FuzzyMatch {
                index: 0,
                score: 0,
                positions: vec![],
            });
        }

        let pattern_chars: Vec<char> = pattern.to_lowercase().chars().collect();
        let text_chars: Vec<char> = text.to_lowercase().chars().collect();
        let text_orig: Vec<char> = text.chars().collect();

        let mut pi = 0;
        let mut score = 0i32;
        let mut positions = Vec::new();
        let mut prev_pos: Option<usize> = None;

        for (ti, &tc) in text_chars.iter().enumerate() {
            if pi < pattern_chars.len() && tc == pattern_chars[pi] {
                positions.push(ti);

                if let Some(pp) = prev_pos {
                    if ti == pp + 1 {
                        score += 3;
                    } else {
                        score += 1;
                    }
                } else {
                    score += 1;
                }

                if ti == 0 || !text_orig[ti - 1].is_alphanumeric() {
                    score += 5;
                }

                if pattern.chars().nth(pi) == Some(text_orig[ti]) {
                    score += 1;
                }

                prev_pos = Some(ti);
                pi += 1;
            }
        }

        if pi == pattern_chars.len() {
            Some(FuzzyMatch {
                index: 0,
                score,
                positions,
            })
        } else {
            None
        }
    }

    pub fn search<'a>(pattern: &str, items: &'a [String]) -> Vec<(usize, i32, &'a str)> {
        let mut results: Vec<_> = items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                FuzzyMatcher::score(pattern, item).map(|m| (i, m.score, item.as_str()))
            })
            .collect();
        results.sort_by(|a, b| b.1.cmp(&a.1));
        results
    }
}
