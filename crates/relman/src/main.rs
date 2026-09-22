// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The release manager: what a release says about itself, and the signature
//! that makes it believable.
//!
//! [`docs/distribution.md`](../../../docs/distribution.md) §8 is the procedure.
//! This tool runs on the maintainer's machine, with the signing key, and
//! **speaks to no network at all** — a tool that could fetch could be made to
//! fetch, and this is the one program whose output decides what other people's
//! machines will run.
//!
//! The rules it enforces live in `tiamat_core::release`, which is also what
//! the client and the updater check with. One implementation, because two
//! would eventually disagree about what is authentic.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use tiamat_core::release::{Artifact, Manifest, ReleaseError, SCHEMA, to_hex};

#[derive(Parser)]
#[command(name = "relman", about = "Build and sign a Tiamat release manifest.")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Makes a signing key. Do this once, keep the file, back it up offline.
    Keygen {
        /// Where to write the secret key.
        #[arg(long, default_value = "release-key.hex")]
        out: PathBuf,
    },
    /// Prints a key file's public half and the hash to commit to it by.
    Show {
        /// The secret key file.
        #[arg(long, default_value = "release-key.hex")]
        key: PathBuf,
    },
    /// Builds a manifest from the archives in a directory.
    Manifest {
        /// Where `scripts/package.sh` left its archives.
        #[arg(long, default_value = "dist")]
        dist: PathBuf,
        /// Which channel this is the head of.
        #[arg(long, default_value = "test")]
        channel: String,
        /// The release's version. Defaults to the one in the archive names.
        #[arg(long)]
        version: Option<String>,
        /// The commit the binaries were built from.
        #[arg(long)]
        commit: Option<String>,
        /// The protocol version these binaries speak.
        #[arg(long)]
        protocol: u32,
        /// The signing key, so the manifest names the key that will sign it.
        #[arg(long, default_value = "release-key.hex")]
        key: PathBuf,
        /// The BLAKE3 hash of the key that will sign the NEXT manifest.
        #[arg(long)]
        next_key_hash: Option<String>,
        /// Where the archives will be downloaded from. `{name}` is replaced
        /// by each archive's file name. Repeat for a mirror.
        #[arg(long = "url", required = true)]
        urls: Vec<String>,
        /// Where a human can read about this release.
        #[arg(long)]
        notes: Option<String>,
        /// Where to write it.
        #[arg(long, default_value = "dist/manifest.json")]
        out: PathBuf,
    },
    /// Signs a manifest, writing `<manifest>.sig`.
    Sign {
        /// The manifest to sign.
        #[arg(long, default_value = "dist/manifest.json")]
        manifest: PathBuf,
        /// The secret key file.
        #[arg(long, default_value = "release-key.hex")]
        key: PathBuf,
    },
    /// Checks a manifest the way a client will, and says what it found.
    Verify {
        /// The manifest.
        #[arg(long, default_value = "dist/manifest.json")]
        manifest: PathBuf,
        /// Its detached signature. Defaults to `<manifest>.sig`.
        #[arg(long)]
        signature: Option<PathBuf>,
        /// The public key a client would have compiled in, as hex.
        #[arg(long)]
        key: String,
        /// The successor hash a client would be holding, if any.
        #[arg(long)]
        expect_next: Option<String>,
        /// The version a client would already have installed, if any.
        #[arg(long)]
        installed: Option<String>,
        /// A directory of archives to check the hashes against.
        #[arg(long)]
        dist: Option<PathBuf>,
    },
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("relman: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<(), String> {
    match args.command {
        Command::Keygen { out } => keygen(&out),
        Command::Show { key } => show(&key),
        Command::Manifest {
            dist,
            channel,
            version,
            commit,
            protocol,
            key,
            next_key_hash,
            urls,
            notes,
            out,
        } => manifest(&Build {
            dist,
            channel,
            version,
            commit,
            protocol,
            key,
            next_key_hash,
            urls,
            notes,
            out,
        }),
        Command::Sign { manifest, key } => sign(&manifest, &key),
        Command::Verify {
            manifest,
            signature,
            key,
            expect_next,
            installed,
            dist,
        } => verify(&Check {
            manifest,
            signature,
            key,
            expect_next,
            installed,
            dist,
        }),
    }
}

/// Everything `manifest` needs, as one argument: clippy's limit is seven and
/// this genuinely wants ten.
struct Build {
    dist: PathBuf,
    channel: String,
    version: Option<String>,
    commit: Option<String>,
    protocol: u32,
    key: PathBuf,
    next_key_hash: Option<String>,
    urls: Vec<String>,
    notes: Option<String>,
    out: PathBuf,
}

/// The same for `verify`.
struct Check {
    manifest: PathBuf,
    signature: Option<PathBuf>,
    key: String,
    expect_next: Option<String>,
    installed: Option<String>,
    dist: Option<PathBuf>,
}

fn keygen(out: &Path) -> Result<(), String> {
    if out.exists() {
        return Err(format!(
            "{} already exists. A release key is not something to overwrite by \
             accident; move it aside first if you really mean to.",
            out.display()
        ));
    }
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|err| format!("no entropy to make a key with: {err}"))?;
    let signing = SigningKey::from_bytes(&seed);
    write_secret(out, &to_hex(&seed))?;

    let public = signing.verifying_key();
    println!("wrote {}", out.display());
    println!("public key:   {}", to_hex(public.as_bytes()));
    println!("commit hash:  {}", blake3::hash(public.as_bytes()).to_hex());
    println!();
    println!("Back the key file up offline. It is the only thing that says a");
    println!("release is yours, and nothing can reissue it if it is lost — a");
    println!("new key means every installed build has to be replaced by hand.");
    Ok(())
}

fn show(key: &Path) -> Result<(), String> {
    let signing = read_key(key)?;
    let public = signing.verifying_key();
    println!("public key:   {}", to_hex(public.as_bytes()));
    println!("commit hash:  {}", blake3::hash(public.as_bytes()).to_hex());
    Ok(())
}

fn manifest(build: &Build) -> Result<(), String> {
    let signing = read_key(&build.key)?;
    let mut artifacts = Vec::new();
    let entries = std::fs::read_dir(&build.dist)
        .map_err(|err| format!("cannot read {}: {err}", build.dist.display()))?;
    // Sorted, so the same directory always produces the same manifest — a
    // file order that depended on the filesystem would make two runs differ
    // for no reason anybody could see.
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            name.ends_with(".tar.gz") || name.ends_with(".zip")
        })
        .collect();
    files.sort();

    for path in &files {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{} has no file name", path.display()))?;
        let bytes = std::fs::read(path).map_err(|err| format!("cannot read {name}: {err}"))?;
        let target =
            target_of(&name).ok_or_else(|| format!("cannot tell which target `{name}` is for"))?;
        artifacts.push(Artifact {
            target,
            urls: build
                .urls
                .iter()
                .map(|url| url.replace("{name}", &name))
                .collect(),
            size: bytes.len() as u64,
            hash: blake3::hash(&bytes).to_hex().to_string(),
            name,
        });
    }

    if artifacts.is_empty() {
        return Err(format!(
            "no archives in {} — run scripts/package.sh first",
            build.dist.display()
        ));
    }

    let version = match &build.version {
        Some(version) => version.clone(),
        None => version_of(&artifacts[0].name)
            .ok_or_else(|| "cannot tell the version from the archive names".to_owned())?,
    };

    let manifest = Manifest {
        schema: SCHEMA,
        channel: build.channel.clone(),
        version,
        commit: build.commit.clone(),
        released: now_rfc3339(),
        protocol: build.protocol,
        key: to_hex(signing.verifying_key().as_bytes()),
        next_key: build.next_key_hash.clone(),
        notes: build.notes.clone(),
        artifacts,
    };

    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| format!("cannot write the manifest: {err}"))?;
    // **Read back through the same door a client uses**, so a manifest that
    // would be refused is refused here rather than on somebody's machine.
    Manifest::parse(&bytes)
        .map_err(|err| format!("the manifest this made is not usable: {err}"))?;
    std::fs::write(&build.out, &bytes)
        .map_err(|err| format!("cannot write {}: {err}", build.out.display()))?;

    println!("wrote {}", build.out.display());
    for artifact in &manifest.artifacts {
        println!("  {:<28} {:>12} bytes", artifact.target, artifact.size);
    }
    println!();
    println!(
        "Now sign it:  relman sign --manifest {}",
        build.out.display()
    );
    Ok(())
}

fn sign(manifest: &Path, key: &Path) -> Result<(), String> {
    let signing = read_key(key)?;
    let bytes = std::fs::read(manifest)
        .map_err(|err| format!("cannot read {}: {err}", manifest.display()))?;
    let parsed = Manifest::parse(&bytes)
        .map_err(|err| format!("refusing to sign a manifest that is not usable: {err}"))?;
    if parsed.key != to_hex(signing.verifying_key().as_bytes()) {
        return Err(
            "the manifest names a different signing key than this one. A client \
             checks the key the manifest names, so signing it with another would \
             produce a release nobody can install."
                .to_owned(),
        );
    }
    let signature = signing.sign(&bytes);
    let out = with_extension(manifest, "sig");
    std::fs::write(&out, signature.to_bytes())
        .map_err(|err| format!("cannot write {}: {err}", out.display()))?;
    println!("signed {} -> {}", manifest.display(), out.display());
    Ok(())
}

fn verify(check: &Check) -> Result<(), String> {
    let bytes = std::fs::read(&check.manifest)
        .map_err(|err| format!("cannot read {}: {err}", check.manifest.display()))?;
    let signature_path = check
        .signature
        .clone()
        .unwrap_or_else(|| with_extension(&check.manifest, "sig"));
    let signature = std::fs::read(&signature_path)
        .map_err(|err| format!("cannot read {}: {err}", signature_path.display()))?;

    let key = decode_key(&check.key)?;
    let manifest = Manifest::verify(&bytes, &signature, &key, check.expect_next.as_deref())
        .map_err(|err: ReleaseError| format!("a client would refuse this: {err}"))?;

    println!("signature   ok");
    println!("channel     {}", manifest.channel);
    println!("version     {}", manifest.version);
    println!("protocol    {}", manifest.protocol);
    if let Some(commit) = &manifest.commit {
        println!("commit      {commit}");
    }
    if let Some(next) = &manifest.next_key {
        println!("next key    {next}");
    }

    if let Some(installed) = &check.installed {
        match manifest.newer_than(installed) {
            Ok(()) => println!("newer than  {installed}"),
            Err(err) => return Err(format!("a client on {installed} would refuse this: {err}")),
        }
    }

    if let Some(dist) = &check.dist {
        for artifact in &manifest.artifacts {
            let path = dist.join(&artifact.name);
            let bytes = std::fs::read(&path)
                .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
            if artifact.matches(&bytes) {
                println!("hash ok     {}", artifact.name);
            } else {
                return Err(format!(
                    "{} is not the file the manifest describes",
                    artifact.name
                ));
            }
        }
    }
    Ok(())
}

/// `tiamat-0.2.0-x86_64-unknown-linux-gnu.tar.gz` -> the triple.
fn target_of(name: &str) -> Option<String> {
    let stem = name
        .strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".zip"))?;
    let rest = stem.strip_prefix("tiamat-")?;
    // The version comes first and has no dashes in it, so the target is
    // everything after the first one.
    let (_, target) = rest.split_once('-')?;
    (!target.is_empty()).then(|| target.to_owned())
}

/// The same name's version.
fn version_of(name: &str) -> Option<String> {
    let stem = name
        .strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".zip"))?;
    let rest = stem.strip_prefix("tiamat-")?;
    let (version, _) = rest.split_once('-')?;
    (!version.is_empty()).then(|| version.to_owned())
}

/// The time, as RFC 3339 in UTC, without pulling in a date library.
fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (days, seconds) = (now / 86_400, now % 86_400);
    let (hour, minute, second) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days since the epoch as a calendar date (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn with_extension(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(extension);
    path.with_file_name(name)
}

fn read_key(path: &Path) -> Result<SigningKey, String> {
    let text = std::fs::read_to_string(path).map_err(|err| {
        format!(
            "cannot read the signing key at {}: {err}. Make one with `relman keygen`.",
            path.display()
        )
    })?;
    let seed = decode_hex_32(text.trim())
        .ok_or_else(|| format!("{} does not hold 32 hex bytes", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}

fn decode_key(hex: &str) -> Result<VerifyingKey, String> {
    let bytes =
        decode_hex_32(hex.trim()).ok_or_else(|| "the key is not 32 hex bytes".to_owned())?;
    VerifyingKey::from_bytes(&bytes).map_err(|err| format!("the key is not a public key: {err}"))
}

fn decode_hex_32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0_u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Writes a secret, readable only by its owner where the platform can say so.
fn write_secret(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|err| format!("cannot restrict {}: {err}", path.display()))?;
    }
    Ok(())
}
