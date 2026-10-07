#[allow(dead_code)]

pub struct ExecPreview;

pub struct PreviewResult {
    pub command: String,
    pub risk_level: RiskLevel,
    pub summary: String,
    pub details: Vec<String>,
}

pub enum RiskLevel {
    Safe,
    Warning,
    Danger,
}

impl ExecPreview {
    pub fn analyze(cmd: &str) -> Option<PreviewResult> {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        match parts[0] {
            "rm" => Self::analyze_rm(&parts),
            "mv" => Self::analyze_mv(&parts),
            "chmod" => Self::analyze_chmod(&parts),
            "chown" => Self::analyze_chown(&parts),
            "git" if parts.get(1) == Some(&"reset") => Self::analyze_git_reset(&parts),
            "git" if parts.get(1) == Some(&"clean") => Some(PreviewResult {
                command: cmd.into(),
                risk_level: RiskLevel::Danger,
                summary: "Removes untracked files from working tree".into(),
                details: vec!["Files not in git will be permanently deleted".into()],
            }),
            "docker" if parts.get(1) == Some(&"rm") => Some(PreviewResult {
                command: cmd.into(),
                risk_level: RiskLevel::Warning,
                summary: "Removes Docker container(s)".into(),
                details: vec!["Container data will be lost unless volumes are used".into()],
            }),
            "dd" => Some(PreviewResult {
                command: cmd.into(),
                risk_level: RiskLevel::Danger,
                summary: "Low-level disk write — can overwrite entire drives".into(),
                details: vec!["Double-check 'of=' target before executing".into()],
            }),
            "mkfs" => Some(PreviewResult {
                command: cmd.into(),
                risk_level: RiskLevel::Danger,
                summary: "Formats a filesystem — ALL DATA WILL BE LOST".into(),
                details: vec![],
            }),
            "kill" if parts.contains(&"-9") => Some(PreviewResult {
                command: cmd.into(),
                risk_level: RiskLevel::Warning,
                summary: "Force-kills process (SIGKILL, no cleanup)".into(),
                details: vec!["Process cannot save state or release resources".into()],
            }),
            "sudo" if parts.len() > 1 => {
                let sub_cmd = parts[1..].join(" ");
                Self::analyze(&sub_cmd).map(|mut r| {
                    r.risk_level = RiskLevel::Danger;
                    r.summary = format!("[sudo] {}", r.summary);
                    r
                })
            }
            _ => None,
        }
    }

    fn analyze_rm(parts: &[&str]) -> Option<PreviewResult> {
        let has_r = parts.iter().any(|p| {
            *p == "-r" || *p == "-R" || *p == "-rf" || *p == "-fr"
                || (p.starts_with('-') && p.contains('r'))
        });
        let has_f = parts.iter().any(|p| {
            *p == "-f" || *p == "-rf" || *p == "-fr"
                || (p.starts_with('-') && p.contains('f'))
        });
        let targets: Vec<&str> = parts
            .iter()
            .filter(|p| !p.starts_with('-') && **p != "rm")
            .copied()
            .collect();

        let risk = if has_r
            && targets
                .iter()
                .any(|t| *t == "/" || *t == "/*" || *t == "~" || *t == "~/*")
        {
            RiskLevel::Danger
        } else if has_r && has_f {
            RiskLevel::Danger
        } else if has_r {
            RiskLevel::Warning
        } else {
            RiskLevel::Safe
        };

        let mut details = Vec::new();
        for target in &targets {
            let path = std::path::Path::new(target);
            if path.is_dir() && has_r {
                if let Ok(count) = count_files_recursive(path) {
                    details.push(format!("{}: {} files/dirs", target, count));
                }
            } else if path.is_file() {
                if let Ok(meta) = std::fs::metadata(path) {
                    details.push(format!("{}: {}", target, format_size(meta.len())));
                }
            } else {
                details.push(format!("{}: (not found)", target));
            }
        }

        let summary = if has_r && has_f {
            "Force-deletes files/directories recursively (NO confirmation)"
        } else if has_r {
            "Deletes files/directories recursively"
        } else {
            "Deletes files"
        };

        Some(PreviewResult {
            command: parts.join(" "),
            risk_level: risk,
            summary: summary.into(),
            details,
        })
    }

    fn analyze_mv(parts: &[&str]) -> Option<PreviewResult> {
        let args: Vec<&str> = parts
            .iter()
            .filter(|p| !p.starts_with('-') && **p != "mv")
            .copied()
            .collect();
        if args.len() >= 2 {
            let src = args[0];
            let dst = args[args.len() - 1];
            Some(PreviewResult {
                command: parts.join(" "),
                risk_level: RiskLevel::Warning,
                summary: format!("Moves {} -> {}", src, dst),
                details: vec!["Destination will be overwritten if it exists".into()],
            })
        } else {
            None
        }
    }

    fn analyze_chmod(parts: &[&str]) -> Option<PreviewResult> {
        let recursive = parts.contains(&"-R");
        Some(PreviewResult {
            command: parts.join(" "),
            risk_level: if recursive {
                RiskLevel::Warning
            } else {
                RiskLevel::Safe
            },
            summary: format!(
                "Changes file permissions{}",
                if recursive { " recursively" } else { "" }
            ),
            details: vec![],
        })
    }

    fn analyze_chown(parts: &[&str]) -> Option<PreviewResult> {
        let recursive = parts.contains(&"-R");
        Some(PreviewResult {
            command: parts.join(" "),
            risk_level: if recursive {
                RiskLevel::Warning
            } else {
                RiskLevel::Safe
            },
            summary: format!(
                "Changes file ownership{}",
                if recursive { " recursively" } else { "" }
            ),
            details: vec![],
        })
    }

    fn analyze_git_reset(parts: &[&str]) -> Option<PreviewResult> {
        let hard = parts.contains(&"--hard");
        Some(PreviewResult {
            command: parts.join(" "),
            risk_level: if hard {
                RiskLevel::Danger
            } else {
                RiskLevel::Warning
            },
            summary: if hard {
                "Discards ALL uncommitted changes permanently".into()
            } else {
                "Resets staging area (working directory preserved)".into()
            },
            details: if hard {
                vec!["Run 'git stash' first to save changes".into()]
            } else {
                vec![]
            },
        })
    }
}

fn count_files_recursive(path: &std::path::Path) -> std::io::Result<usize> {
    let mut count = 0;
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            count += 1;
            if entry.file_type()?.is_dir() {
                count += count_files_recursive(&entry.path()).unwrap_or(0);
            }
        }
    }
    Ok(count)
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    }
}
