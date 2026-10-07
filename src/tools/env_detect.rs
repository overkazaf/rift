pub struct DetectedEnv {
    pub project_type: String,
    pub version_file: Option<String>,
    pub suggested_action: Option<String>,
    pub env_file: bool,
}

pub fn detect(dir: &std::path::Path) -> DetectedEnv {
    let mut result = DetectedEnv {
        project_type: "unknown".into(),
        version_file: None,
        suggested_action: None,
        env_file: dir.join(".env").exists(),
    };

    let checks: &[(&str, &str, Option<(&str, &str)>)] = &[
        ("Cargo.toml", "rust", None),
        (
            "package.json",
            "node",
            Some((".nvmrc", "nvm use")),
        ),
        ("go.mod", "go", None),
        (
            "pyproject.toml",
            "python",
            Some((".python-version", "pyenv shell $(cat .python-version)")),
        ),
        (
            "requirements.txt",
            "python",
            Some((".python-version", "pyenv shell $(cat .python-version)")),
        ),
        (
            "Gemfile",
            "ruby",
            Some((".ruby-version", "rbenv shell $(cat .ruby-version)")),
        ),
        (
            "pom.xml",
            "java",
            Some((".java-version", "jenv shell $(cat .java-version)")),
        ),
        ("build.gradle", "java", None),
        ("CMakeLists.txt", "cpp", None),
        ("Makefile", "make", None),
        ("docker-compose.yml", "docker", None),
        ("Dockerfile", "docker", None),
        (".terraform", "terraform", None),
    ];

    for (file, ptype, version_info) in checks {
        if dir.join(file).exists() {
            result.project_type = ptype.to_string();
            if let Some((vfile, cmd)) = version_info {
                if dir.join(vfile).exists() {
                    result.version_file = Some(vfile.to_string());
                    result.suggested_action = Some(cmd.to_string());
                }
            }
            break;
        }
    }

    if dir.join(".tool-versions").exists() {
        result.version_file = Some(".tool-versions".into());
        result.suggested_action = Some("asdf install".into());
    }

    result
}
