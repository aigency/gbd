use assert_cmd::prelude::*;
use assert_cmd::Command;

#[test]
fn version_prints_crate_version() {
    let mut cmd = Command::cargo_bin("gbd").unwrap();
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::contains("gbd"));
}

#[test]
fn help_lists_ready() {
    let mut cmd = Command::cargo_bin("gbd").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("ready"))
        .stdout(predicates::str::contains("create"));
}

#[test]
fn ping_boots_without_auth() {
    let mut cmd = Command::cargo_bin("gbd").unwrap();
    cmd.arg("ping")
        .assert()
        .success()
        .stdout(predicates::str::contains("gbd"));
}

#[test]
fn install_sh_help() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh");
    std::process::Command::new("sh")
        .arg(&script)
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("Cloud agents"))
        .stdout(predicates::str::contains("curl -fsSL"));
}

#[test]
fn install_sh_uses_utf8_checkmark_not_hex_escape() {
    let script = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh"),
    )
    .unwrap();
    assert!(
        script.contains('✓'),
        "installer should print a real checkmark"
    );
    assert!(
        !script.contains("\\xe2\\x9c\\x93"),
        "POSIX sh printf does not expand hex escapes"
    );
}

/// `gbd init` generates these; the checked-in copies must not drift.
#[test]
fn checked_in_agent_files_match_init_constants() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in gbd::init::SKILL_DIRS {
        let path = root.join(dir).join("SKILL.md");
        let skill = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}; run gbd init", path.display()));
        assert_eq!(
            skill,
            gbd::init::SKILL_MD,
            "{} is stale; run gbd init",
            path.display()
        );
    }
    for file in gbd::init::INSTRUCTION_FILES {
        let text = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|e| panic!("{file}: {e}; run gbd init"));
        assert!(
            text.contains(gbd::init::AGENTS_BLOCK.trim_end()),
            "{file} gbd block is stale; run gbd init"
        );
    }
}
