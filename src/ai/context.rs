#[derive(Clone)]
pub struct TermContext {
    pub os: String,
    pub shell: String,
    pub cwd: String,
    pub git_branch: Option<String>,
    pub project_type: Option<String>,
    pub recent_commands: Vec<String>,
}

impl TermContext {
    pub fn collect() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            git_branch: detect_git_branch(),
            project_type: detect_project_type(),
            recent_commands: Vec::new(),
        }
    }

    pub fn with_recent(mut self, commands: Vec<String>) -> Self {
        self.recent_commands = commands;
        self
    }
}

fn detect_git_branch() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() {
            None
        } else {
            Some(branch)
        }
    } else {
        None
    }
}

fn detect_project_type() -> Option<String> {
    let indicators = [
        ("Cargo.toml", "rust"),
        ("package.json", "node"),
        ("go.mod", "go"),
        ("pyproject.toml", "python"),
        ("requirements.txt", "python"),
        ("Gemfile", "ruby"),
        ("pom.xml", "java"),
        ("build.gradle", "java"),
        ("CMakeLists.txt", "cpp"),
        ("Makefile", "make"),
    ];
    for (file, ptype) in &indicators {
        if std::path::Path::new(file).exists() {
            return Some(ptype.to_string());
        }
    }
    None
}
