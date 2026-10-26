// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The container image's promises, read from its Dockerfile.
//!
//! CI does not build the image on every push, so the rules that matter for a
//! deployment are pinned here, where a change that breaks one fails the suite
//! rather than a production redeploy.

/// The Dockerfile's instructions, comments and blank lines dropped, and a
/// line ending in `\` joined to the next, as Docker reads them.
fn instructions() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Dockerfile");
    let text = std::fs::read_to_string(&path).expect("the registry's Dockerfile");
    let mut instructions = Vec::new();
    let mut pending = String::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        match line.strip_suffix('\\') {
            Some(start) => {
                pending.push_str(start.trim_end());
                pending.push(' ');
            }
            None => {
                pending.push_str(line);
                instructions.push(std::mem::take(&mut pending));
            }
        }
    }
    instructions
}

/// The index of the last instruction starting with `keyword`.
fn last(instructions: &[String], keyword: &str) -> Option<usize> {
    instructions
        .iter()
        .rposition(|line| line.split_whitespace().next() == Some(keyword))
}

#[test]
fn the_service_does_not_run_as_root() {
    let instructions = instructions();
    let user = last(&instructions, "USER").expect("the runtime stage sets a USER");
    assert_eq!(instructions[user], "USER registry");
    // In the runtime stage, and before the volume and the entrypoint, so the
    // service and a fresh volume's owner are both the unprivileged user.
    let runtime = last(&instructions, "FROM").expect("a runtime stage");
    let volume = last(&instructions, "VOLUME").expect("a data volume");
    let entrypoint = last(&instructions, "ENTRYPOINT").expect("an entrypoint");
    assert!(runtime < user && user < volume && user < entrypoint);

    // The fixed UID the operations guide tells an operator to `chown` to.
    let creates = instructions
        .iter()
        .any(|line| line.contains("useradd") && line.contains("--uid 10001"));
    assert!(creates, "the registry user is created with UID 10001");
}

#[test]
fn the_image_checks_its_own_health() {
    let instructions = instructions();
    let check = last(&instructions, "HEALTHCHECK").expect("a HEALTHCHECK instruction");
    let runtime = last(&instructions, "FROM").expect("a runtime stage");
    assert!(runtime < check, "the health check is in the runtime stage");
    let words: Vec<&str> = instructions[check].split_whitespace().collect();
    // The schedule the operations guide documents.
    for option in [
        "--interval=30s",
        "--timeout=5s",
        "--start-period=10s",
        "--retries=3",
    ] {
        assert!(words.contains(&option), "{option} in {words:?}");
    }
    // The binary's own probe: the image has no curl.
    assert!(
        instructions[check].contains("CMD [\"/usr/local/bin/registry\", \"healthcheck\"]"),
        "{}",
        instructions[check]
    );
}
