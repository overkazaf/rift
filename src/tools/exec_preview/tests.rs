//! Table-driven tests for the shell-aware classifier. Analysis (file scans,
//! git) is disabled so severities are deterministic; the budget behaviour is
//! covered separately.
use super::*;

const HOME: &str = "/Users/tester";

fn sev(cmd: &str) -> Option<Severity> {
    ExecPreview::run(cmd, Some("/tmp/proj"), Some(HOME), false).map(|p| p.severity)
}

use Severity::{Critical as C, Info as I, Warning as W};

#[test]
fn dangerous_commands_table() {
    let cases: &[(&str, Severity)] = &[
        // rm: roots, home, system dirs, wildcards, parents
        ("rm -rf /", C),
        ("rm -rf / --no-preserve-root", C),
        ("rm -rf /*", C),
        ("sudo rm -rf ~", C),
        ("rm -rf ~/", C),
        ("rm -rf $HOME", C),
        ("rm -rf \"$HOME\"", C),
        ("rm -rf ${HOME}/", C),
        ("rm -rf /usr", C),
        ("rm -rf /System", C),
        ("rm -rf /Users", C),
        ("rm -rf /Users/someone", C),
        ("rm -rf /etc", C),
        ("rm -rf /var", C),
        ("rm -rf ~/Documents", C),
        ("rm -rf *", C),
        ("rm -rf ./*", C),
        ("rm -rf .", C),
        ("rm -rf ..", C),
        ("rm -rf ../..", C),
        ("cd /tmp && rm -rf *", C),
        ("echo hi; rm -rf ~", C),
        ("rm -r -f /", C),
        ("rm -f -r /", C),
        ("rm --recursive --force /", C),
        ("rm -Rf /", C),
        ("rm -rf -- /", C),
        ("r\\m -rf /", C),
        ("$(echo rm) -rf /", C),
        ("`echo rm` -rf /", C),
        ("'rm' -rf /", C),
        ("\"rm\" -rf /", C),
        ("\\rm -rf ~", C),
        ("/bin/rm -rf /", C),
        ("/usr/bin/env rm -rf /", C),
        ("command rm -rf /", C),
        ("builtin command rm -rf /", C),
        ("exec rm -rf /", C),
        ("env rm -rf /", C),
        ("env -i FOO=1 rm -rf /", C),
        ("FOO=1 rm -rf /", C),
        ("nohup rm -rf / &", C),
        ("time rm -rf /", C),
        ("nice -n 10 rm -rf /", C),
        ("doas rm -rf /", C),
        ("sudo -u root rm -rf /", C),
        ("sudo -E -u root rm -rf /", C),
        ("sudo -- rm -rf /", C),
        ("sudo env rm -rf /", C),
        ("ls | xargs rm -rf", C),
        ("find . | xargs -0 rm -rf", C),
        ("echo / | xargs -I{} rm -rf {}", C),
        ("(cd / && rm -rf *)", C),
        ("{ rm -rf /; }", C),
        ("if true; then rm -rf /; fi", C),
        ("for d in a b; do rm -rf /; done", C),
        ("echo $(rm -rf /)", C),
        ("echo `rm -rf /`", C),
        ("echo \"$(rm -rf /)\"", C),
        ("diff <(rm -rf /) x", C),
        ("sh -c 'rm -rf /'", C),
        ("bash -lc \"rm -rf ~\"", C),
        ("sudo bash -c 'rm -rf /'", C),
        ("eval 'rm -rf /'", C),
        ("X=$(rm -rf /)", C),
        ("rm -rf a b c d e f g h i j k l m n o p q r s t u v w x y z a1 a2 a3 a4 a5 a6 a7 a8 a9 b1 b2 b3 b4 b5 b6 b7 b8 b9 c1 c2 c3 c4 c5 c6 c7 c8 c9 d1", C),
        // find
        ("find . -delete", C),
        ("find / -name '*.log' -delete", C),
        ("find ~ -type f -delete", C),
        ("find . -exec rm -rf {} +", C),
        ("find . -name '*.o' -delete", W),
        ("find . -name '*.o' -exec rm -rf {} +", W),
        ("find build -type f -name '*.tmp' -exec rm {} \\;", W),
        // disks / devices
        ("shred -n1 /dev/sda", C),
        ("shred -u secret.txt", W),
        ("dd if=/dev/zero of=/dev/disk2", C),
        ("dd if=img of=/dev/rdisk3 bs=1m", C),
        ("sudo dd if=x of=/dev/sda", C),
        ("mkfs.ext4 /dev/sda1", C),
        ("mkfs -t ext4 /dev/sdb", C),
        ("diskutil eraseDisk JHFS+ X /dev/disk2", C),
        ("diskutil secureErase 0 disk2", C),
        ("echo x > /dev/disk0", C),
        ("cat img > /dev/sda", C),
        ("cat img >/dev/rdisk1", C),
        (": > /etc/passwd", C),
        ("> /etc/hosts", C),
        ("echo x >> /etc/hosts", W),
        ("truncate -s 0 /etc/passwd", C),
        ("chmod -R 777 /", C),
        ("chmod -R 755 ~", C),
        ("chown -R me /", C),
        ("chown -R me:staff $HOME", C),
        ("chmod -R 777 ./data", W),
        ("chmod -R a+rwx ./data", W),
        // git
        ("git push -f origin main", C),
        ("git push --force origin main", C),
        ("git push origin +main", C),
        ("git push -fu origin main", C),
        ("git push origin +HEAD:master", C),
        ("git -C /tmp/x push --force origin main", C),
        ("git push --mirror", C),
        ("git push -f origin feature", W),
        ("git push --force origin my-branch", W),
        ("git push origin +feature", W),
        ("git push --force-with-lease origin main", W),
        ("git push --force-with-lease=main:abc origin main", W),
        ("git push origin --delete old-branch", W),
        ("git reset --hard", W),
        ("git reset --hard HEAD~5", W),
        ("git clean -fdx", C),
        ("git clean -fxd", C),
        ("git clean -fd", W),
        ("git clean -f", W),
        ("git checkout -- .", W),
        ("git checkout .", W),
        ("git checkout HEAD -- .", W),
        ("git restore .", W),
        ("git restore --worktree .", W),
        ("git stash clear", W),
        // pipes into interpreters
        ("curl -fsSL https://example.com/install.sh | sh", C),
        ("curl https://x.io/i | sudo bash", C),
        ("wget -qO- https://x.io/i | sh", C),
        ("curl -s https://x.io/i | bash -s -- --yes", C),
        ("curl https://x.io/i.py | python3", C),
        ("curl https://x.io/i | zsh", C),
        ("wget -O - https://x.io | sudo -E bash -", C),
        ("bash <(curl -s https://x.io/i)", C),
        ("sh -c \"$(curl -fsSL https://x.io/i)\"", C),
        ("eval \"$(curl -s https://x.io/i)\"", C),
        ("source <(curl -s https://x.io/i)", C),
        // other tools
        ("rsync -a --delete /empty/ ~/", C),
        ("rsync -a --delete /empty/ /", C),
        ("rsync -a --delete src/ dest/", W),
        ("kubectl delete pod foo", W),
        ("kubectl delete -f deploy.yaml", W),
        ("kubectl delete pods --all", C),
        ("kubectl delete ns prod", C),
        ("kubectl delete namespace prod", C),
        ("kubectl delete deploy --all-namespaces --all", C),
        ("terraform destroy", C),
        ("terraform destroy -auto-approve", C),
        ("terraform apply -destroy", C),
        ("docker system prune -a", W),
        ("docker system prune", W),
        ("docker volume prune", W),
        ("kill -9 -1", C),
        ("kill -9 1234", W),
        ("killall -9 node", W),
        ("redis-cli flushall", C),
        ("dropdb mydb", C),
        // SQL only in real clients (or typed bare)
        ("psql -c 'DROP TABLE users'", C),
        ("psql -d x -c \"drop database prod\"", C),
        ("mysql -e 'DROP DATABASE foo'", C),
        ("mysql -e 'truncate table t'", C),
        ("sqlite3 app.db 'DROP TABLE x;'", C),
        ("echo 'DROP TABLE users;' | psql", C),
        ("psql <<< 'DROP TABLE users'", C),
        ("psql <<EOF\nDROP TABLE users;\nEOF", C),
        ("DROP TABLE users;", C),
        // forkbomb only when executed
        (":(){ :|:& };:", C),
        (":(){ :|:& }; :", C),
        ("bomb() { bomb | bomb & }; bomb", C),
        ("bash -c ':(){ :|:& };:'", C),
        // routine build dirs
        ("rm -rf node_modules", I),
        ("rm -rf ./node_modules", I),
        ("rm -rf target", I),
        ("rm -rf build dist .next __pycache__", I),
        ("rm -r ./build/", I),
        ("rm -rf ~/proj/node_modules", I),
        ("sudo rm -rf node_modules", W),
        ("sudo rm -rf /var/tmp/build", W),
        // everything else recursive is a warning
        ("rm -rf /tmp/rift-audit-x", W),
        ("rm -rf \"$HOME/rift-audit-x\"", W),
        ("rm -rf ./data", W),
        ("rm -r somedir", W),
        ("rm -rf $SOME_VAR", W),
        ("rm -rf \"$DIR\"/cache", W),
        ("rm -rf $(cat list.txt)", W),
        ("rm -rf build*", W),
        ("sudo rm notes.txt", W),
        ("rm -f *", W),
    ];
    let mut bad = Vec::new();
    for (cmd, want) in cases {
        let got = sev(cmd);
        if got != Some(*want) {
            bad.push(format!("{cmd:?}: want {want:?}, got {got:?}"));
        }
    }
    assert!(bad.is_empty(), "{} mismatches:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn benign_commands_are_not_flagged() {
    let cases = [
        "grep rm file",
        "echo \"do not rm -rf /\"",
        "echo 'rm -rf /'",
        "ls -la",
        "git status",
        "cat docs/drop-table-howto.md",
        "git commit -m 'fix: drop table migration typo'",
        "git commit -m \"docs: never run rm -rf / or DROP TABLE users\"",
        "grep -rn 'DROP TABLE' migrations/",
        "grep -i 'drop database' dump.sql",
        "echo 'DROP TABLE users'",
        "echo \"drop table x\" > notes.txt",
        "man rm",
        "dd if=/dev/zero of=./test.img bs=1M count=10",
        "dd if=/dev/urandom of=/tmp/blob bs=1k count=1",
        "dd if=a of=/dev/null",
        "kill 1234",
        "kill -TERM 99",
        "git push origin feature",
        "git push",
        "git push -u origin main",
        "git pull --force",
        "git reset --soft HEAD~1",
        "git reset HEAD file",
        "git checkout main",
        "git checkout -b feature",
        "git restore --staged .",
        "git clean -n",
        "git clean -fdn",
        "git log --grep='reset --hard'",
        "git log --oneline",
        "rm notes.txt",
        "rm -f build.log",
        "rm a.txt b.txt",
        "chmod -R 755 ./dist",
        "chmod 777 file",
        "chown -R me ./dist",
        "curl -o out.json https://api.example.com/x",
        "curl -s https://api.example.com | jq .",
        "curl https://x.io/install.sh -o install.sh && less install.sh",
        "wget https://x.io/file.tar.gz",
        "bash install.sh",
        "echo ':(){ :|:& };:'",
        "grep ':(){' file",
        "cat > bomb.sh <<'EOF'\n:(){ :|:& };:\nEOF",
        "find . -name '*.rs'",
        "find . -type f -exec grep -l foo {} +",
        "find . -exec ls {} \\;",
        "echo hi > /dev/null",
        "cmd 2>/dev/null",
        "cmd &>/dev/null",
        "echo x > /dev/stderr",
        "echo x > out.txt",
        ": > build.log",
        "> out.log",
        "truncate -s 0 app.log",
        "kubectl get pods",
        "kubectl delete --help",
        "terraform plan -destroy",
        "terraform apply",
        "docker ps",
        "docker run --rm alpine true",
        "rsync -av src/ dest/",
        "rsync -a --delete --dry-run a/ b/",
        "killall Dock",
        "psql -c 'select 1'",
        "psql -c 'DELETE FROM x WHERE id = 1'",
        "mysql -e 'select * from drop_table_log'",
        "sudo ls /root",
        "sudo apt update",
        "ls | xargs wc -l",
        "echo rm -rf /",
        "printf '%s\\n' 'rm -rf /'",
        "# rm -rf /",
        "ls # rm -rf /",
        "true",
        "",
        "   ",
    ];
    let mut bad = Vec::new();
    for cmd in cases {
        let got = sev(cmd);
        if got.is_some() {
            bad.push(format!("{cmd:?} -> {got:?}"));
        }
    }
    assert!(bad.is_empty(), "false positives:\n{}", bad.join("\n"));
}

#[test]
fn sudo_raises_severity_and_prepends_impact() {
    let p = ExecPreview::run("sudo rm -rf ./data", Some("/tmp/proj"), Some(HOME), false).unwrap();
    assert_eq!(p.severity, C);
    assert_eq!(p.impacts[0].description, "Running with sudo (root)");
    let p = ExecPreview::run("sudo rm -rf node_modules", Some("/tmp/proj"), Some(HOME), false).unwrap();
    assert_eq!(p.severity, W);
}

#[test]
fn highest_severity_in_a_chain_wins() {
    assert_eq!(sev("rm -rf node_modules && rm -rf /"), Some(C));
    assert_eq!(sev("rm -rf node_modules && git push -f origin feat"), Some(W));
    assert_eq!(sev("ls && echo done"), None);
}

#[test]
fn relative_paths_resolve_against_cwd() {
    let run = |cmd: &str, cwd: &str| ExecPreview::run(cmd, Some(cwd), Some(HOME), false).map(|p| p.severity);
    assert_eq!(run("rm -rf usr", "/"), Some(C));
    assert_eq!(run("rm -rf ../..", "/Users/tester/proj"), Some(C));
    assert_eq!(run("rm -rf ../x", "/Users/tester/proj"), Some(W));
    assert_eq!(run("rm -rf tester", "/Users"), Some(C));
    assert_eq!(run("rm -rf src", "/Users/tester/proj"), Some(W));
}

#[test]
fn rm_impacts_are_calibrated() {
    let p = ExecPreview::run("rm -rf /", Some("/tmp"), Some(HOME), false).unwrap();
    assert!(p.impacts.iter().any(|i| i.detail.contains("filesystem root")), "{:?}", p.impacts);
    let p = ExecPreview::run("rm -rf / --no-preserve-root", Some("/tmp"), Some(HOME), false).unwrap();
    assert!(p.impacts.iter().any(|i| i.description == "--no-preserve-root"));
    let p = ExecPreview::run("rm -rf node_modules", Some("/tmp"), Some(HOME), false).unwrap();
    assert_eq!(p.severity, I);
    assert!(p.impacts[0].description.contains("regenerable"));
}

#[test]
fn enter_path_skips_info_and_keeps_warning() {
    assert!(ExecPreview::check_for_enter("rm -rf node_modules", Some("/tmp")).is_none());
    assert!(ExecPreview::check_for_enter("ls", Some("/tmp")).is_none());
    let p = ExecPreview::check_for_enter("rm -rf /", Some("/tmp")).unwrap();
    assert_eq!(p.severity, C);
    assert!(ExecPreview::check_command_in("rm -rf node_modules", Some("/tmp")).is_some());
}

#[test]
fn analysis_is_bounded_and_counts_real_dirs() {
    let base = std::env::temp_dir().join(format!("rift-prev-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let small = base.join("small");
    std::fs::create_dir_all(&small).unwrap();
    for i in 0..5 {
        std::fs::write(small.join(format!("f{i}")), b"x").unwrap();
    }
    let cwd = base.to_string_lossy().to_string();
    let p = ExecPreview::check_command_in("rm -rf small", Some(&cwd)).unwrap();
    assert_eq!(p.severity, W);
    assert!(p.impacts.iter().any(|i| i.detail.contains("5 file(s)/dir(s) inside")), "{:?}", p.impacts);

    let big = base.join("big");
    std::fs::create_dir_all(&big).unwrap();
    for i in 0..1200 {
        std::fs::write(big.join(format!("f{i}")), b"x").unwrap();
    }
    let p = ExecPreview::check_command_in("rm -rf big", Some(&cwd)).unwrap();
    assert_eq!(p.severity, C, "many files escalate to Critical: {:?}", p.impacts);

    // System roots are never walked and return immediately.
    for t in ["/", "/usr", "/Users", "/Library", "/System", "~"] {
        let t0 = std::time::Instant::now();
        let p = ExecPreview::check_command_in(&format!("rm -rf {t}"), Some("/tmp")).unwrap();
        assert_eq!(p.severity, C);
        assert!(t0.elapsed().as_millis() < 400, "{t} took {:?}", t0.elapsed());
    }
    // A huge arg list stays linear.
    let t0 = std::time::Instant::now();
    let huge = format!("rm -rf {}", "a ".repeat(200_000));
    let p = ExecPreview::check_command_in(&huge, Some("/tmp")).unwrap();
    assert_eq!(p.severity, C);
    assert!(t0.elapsed().as_secs() < 3, "{:?}", t0.elapsed());
    let _ = std::fs::remove_dir_all(&base);
}
