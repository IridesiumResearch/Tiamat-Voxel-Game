// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The container image's promises, read from its Dockerfile.
//!
//! CI does not build the image on every push, so the rules that matter for a
//! deployment are pinned here, where a change that breaks one fails the suite
//! rather than a production redeploy.

/// The Dockerfile's instructions, comments and blank lines dropped.
fn instructions() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Dockerfile");
    let text = std::fs::read_to_string(&path).expect("the registry's Dockerfile");
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
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
