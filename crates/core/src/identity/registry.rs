// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Name certificates: a name that is yours everywhere, vouched for offline.
//!
//! # What a certificate is
//!
//! A display name is, on its own, whatever a client asks for. This module is
//! the second half of charter rule 13's name story: a short-lived record,
//! signed by Iridesium's registry key, saying *this UUID is called X*. A server
//! that holds the registry's public key can check that claim without a network,
//! which matters more than it looks — the server is the game (charter rule 2),
//! and a join must never wait on a host that might be down.
//!
//! The certificate is bound to the **UUID**, never to a key. An identity is a
//! set of keys that rotate and gain devices ([`super::keyset`]); the UUID is
//! `BLAKE3` of the root key and never moves, so a name follows its holder
//! through every rotation and onto every device.
//!
//! # Why the registry's keys are two keys
//!
//! The registry signs with one key and has already committed, by hash, to the
//! key that will replace it ([`RegistryTrust::accept_rotation`]). A stolen
//! current key therefore cannot move the registry to a key of the thief's
//! choosing — the same pre-rotation argument as [`super::keyset`], applied to
//! the registry itself. A server built before a rotation keeps verifying
//! certificates signed by the key it knows until they expire, which is the 90
//! days every client has to renew.
//!
//! # Hostile input
//!
//! Every certificate arrives from a client that a server does not trust
//! (charter rule 14). Everything here is therefore parsed **before** it is
//! acted on, under a byte cap checked before decoding; the parse is total —
//! any bytes are a value or an error, never a panic — and it re-checks the name
//! rules and the lifetime, so nothing downstream is handed a number the caps
//! were meant to keep out. `fuzz/fuzz_targets/name_certificate_decode.rs`
//! exercises that promise on the same commit as the parsers.
//!
//! # The honest limit
//!
//! The registry makes a name **unique**; it does not make a player
//! **accountable**. Keys are free to mint, so names are free up to the rate
//! limits, and a UUID ban remains trivially evaded by a new identity. That is
//! stated in the design (§1) and repeated here because a reader who believes
//! otherwise will build moderation on top of it.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::keyset::{COMMITMENT_BYTES, commit_to};
use super::{Identity, PlayerUuid, public_key_from_bytes};

/// Shortest canonical name, in characters.
pub const MIN_NAME_CHARS: usize = 3;

/// Longest canonical name, in characters.
///
/// Eighteen leaves room inside the protocol's 32-byte name limit and inside
/// this module's 256-byte certificate cap with every other field maxed out.
pub const MAX_NAME_CHARS: usize = 18;

/// How long a certificate is good for: 90 days.
///
/// Short lifetimes are the revocation mechanism a server that never calls home
/// can rely on: a stolen or abused name stops being honoured when the record
/// naming it expires, even if nobody has published a revocation.
pub const CERTIFICATE_LIFETIME_SECONDS: i64 = 90 * 24 * 60 * 60;

/// The client renews once this little of the certificate is left: 30 days.
pub const RENEW_BEFORE_SECONDS: i64 = 30 * 24 * 60 * 60;

/// How far two clocks may disagree before a record is refused: five minutes.
///
/// Applied symmetrically: a certificate issued less than five minutes in the
/// future verifies, and one that expired less than five minutes ago still does.
pub const CLOCK_SKEW_SECONDS: i64 = 5 * 60;

/// The longest lifetime a certificate may claim before the parser refuses it.
///
/// The registry issues [`CERTIFICATE_LIFETIME_SECONDS`]; this is the "sane
/// lifetime" bound that keeps a signed-but-wrong record from naming a holder
/// until the heat death of the universe. A year is generous enough that a
/// future lifetime change needs no parser change.
pub const MAX_CERTIFICATE_LIFETIME_SECONDS: i64 = 366 * 24 * 60 * 60;

/// Largest certificate read at all, in bytes.
///
/// A certificate is under 200 bytes with every field at its limit; the cap is
/// checked before a single byte is decoded.
pub const MAX_CERTIFICATE_BYTES: usize = 256;

/// Largest registry rotation record read at all, in bytes.
pub const MAX_ROTATION_BYTES: usize = 512;

/// Largest claim or release request read at all, in bytes.
pub const MAX_REQUEST_BYTES: usize = 512;

/// Largest revocation list read at all, in bytes.
pub const MAX_REVOCATION_BYTES: usize = 256 * 1024;

/// Most revocations a list may carry.
///
/// A list is fetched with a renewal and held by a server that chose to; four
/// thousand entries is far past any honest year of operator action, and it is
/// the bound that stops a signed list from being a memory attack in itself.
pub const MAX_REVOCATIONS: usize = 4096;

/// Leading zero bits a claim's proof of work must have by default.
///
/// About a second of one core at `BLAKE3`'s small-input rate, which a player
/// never notices and a thousand-name burst notices. The service raises it as a
/// subnet's claim rate rises (§7 of the design).
pub const DEFAULT_WORK_BITS: u32 = 20;

/// The most work this module will try to solve for.
///
/// `2^40` hashes is days of one core; a caller asking for more has a bug, and
/// [`ClaimRequest::solve_work`] refuses rather than spin.
pub const MAX_WORK_BITS: u32 = 40;

/// Certificate format this build reads.
pub const CERTIFICATE_VERSION: u8 = 1;

/// Rotation-record format this build reads.
pub const ROTATION_VERSION: u8 = 1;

/// Revocation-list format this build reads.
pub const REVOCATION_VERSION: u8 = 1;

/// Claim/renewal request format this build reads.
pub const CLAIM_VERSION: u8 = 1;

/// Release request format this build reads.
pub const RELEASE_VERSION: u8 = 1;

/// Names the registry will not issue.
///
/// First the administrative set — names a player could mistake for Iridesium's
/// own — then every default mod's id, because a player called `core_worldgen`
/// would be indistinguishable from the mod. Ids longer than [`MAX_NAME_CHARS`]
/// (`tiamat_default_science` and its siblings) can never be canonical names, so
/// they need no entry. The operator's own short list lives in the registry's
/// database (§3), which is the right place for something that changes without
/// a release.
pub const RESERVED_NAMES: &[&str] = &[
    "admin",
    "console",
    "core",
    "core_chest",
    "core_gear",
    "core_milk",
    "core_mimic",
    "core_plans",
    "core_sky",
    "core_space",
    "core_tools",
    "core_ui",
    "core_worldgen",
    "iridesium",
    "moderator",
    "relief",
    "server",
    "support",
    "system",
    "tiamat",
    "tiamat_weather",
];

/// Something a registry record or request got wrong.
///
/// The wording is deliberate: a verification failure's message is shown to the
/// player as the reason their join was refused, so each one is a sentence they
/// can act on rather than a code they cannot.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// Bigger than its cap before it was even parsed.
    #[error("the {what} is {len} bytes, over the {limit}-byte limit")]
    TooLarge {
        /// Which record.
        what: &'static str,
        /// How big it was.
        len: usize,
        /// The cap.
        limit: usize,
    },

    /// Not the shape of the record, or not encoded as it claims.
    #[error("the {what} could not be read: {detail}")]
    Malformed {
        /// Which record.
        what: &'static str,
        /// What the decoder said.
        detail: String,
    },

    /// A format version this build does not read.
    #[error("the {what} is version {found}, and this build reads {expected}")]
    Version {
        /// Which record.
        what: &'static str,
        /// What it claimed.
        found: u8,
        /// What this build reads.
        expected: u8,
    },

    /// Shorter than [`MIN_NAME_CHARS`].
    #[error("a name is {found} characters, and the shortest allowed is {MIN_NAME_CHARS}")]
    NameTooShort {
        /// How many characters it was.
        found: usize,
    },

    /// Longer than [`MAX_NAME_CHARS`].
    #[error("a name is {found} characters, and the longest allowed is {MAX_NAME_CHARS}")]
    NameTooLong {
        /// How many characters it was.
        found: usize,
    },

    /// A canonical name's first character is not a letter.
    #[error("a name must start with a letter")]
    NameMustStartWithLetter,

    /// A character outside `a-z`, `0-9`, `_`.
    #[error("`{character}` may not appear in a name; it may hold a-z, 0-9 and _")]
    NameBadCharacter {
        /// The offending character.
        character: char,
    },

    /// A name in [`RESERVED_NAMES`].
    #[error("the name `{name}` is reserved")]
    NameReserved {
        /// The name asked for.
        name: String,
    },

    /// The display casing is not the canonical name in another casing.
    #[error("`{display}` is not `{canonical}` in another casing")]
    DisplayMismatch {
        /// The canonical form.
        canonical: String,
        /// What the display claimed.
        display: String,
    },

    /// A character outside `A-Z`, `a-z`, `0-9`, `_` in a display name.
    #[error("`{character}` may not appear in a display name")]
    DisplayBadCharacter {
        /// The offending character.
        character: char,
    },

    /// A recovery address that is not one.
    ///
    /// Shallow on purpose: the only proof an address is real is the link that
    /// comes back from it, so a deep parser would earn nothing.
    #[error("`{address}` is not an email address")]
    EmailAddress {
        /// What was supplied.
        address: String,
    },

    /// `expires` is not after `issued`, or the span is not a sane lifetime.
    #[error("a certificate runs from {issued} to {expires}, which is not a sane lifetime")]
    CertificateLifetime {
        /// What it claimed to be issued at.
        issued: i64,
        /// What it claimed to expire at.
        expires: i64,
    },

    /// Past its expiry, beyond the skew.
    #[error("the name certificate has expired")]
    CertificateExpired,

    /// Not valid yet, beyond the skew.
    #[error("the name certificate is not valid yet")]
    CertificateNotYetValid,

    /// A certificate for somebody else, presented by this connection.
    #[error("this certificate names a different identity")]
    WrongIdentity,

    /// Signed by a key this build does not trust, or by a retired one too late.
    #[error("the certificate is not signed by a registry key this build trusts")]
    UntrustedRegistryKey,

    /// The signature does not verify over the payload.
    #[error("the signature does not verify")]
    SignatureInvalid,

    /// The certificate's serial is on the revocation list.
    #[error("the name certificate has been revoked")]
    SerialRevoked,

    /// The registry key held here has no commitment to a successor.
    #[error(
        "this build holds no committed successor for the registry key, so it cannot be rotated"
    )]
    NoCommittedSuccessor,

    /// The successor does not hash to the commitment held here.
    #[error("the proposed successor does not match the commitment this build holds")]
    SuccessorMismatch,

    /// A request that is too old, or from the future, by more than the skew.
    #[error("the request's timestamp is more than {CLOCK_SKEW_SECONDS} seconds from this clock")]
    StaleRequest,

    /// A claim that carried no proof of work.
    #[error("a claim must carry a proof of work")]
    MissingWork,

    /// A proof of work below the difficulty the service set.
    #[error("the proof of work has {found} leading zero bits, and {required} were required")]
    InsufficientWork {
        /// Bits the nonce actually has.
        found: u32,
        /// Bits the service required.
        required: u32,
    },

    /// Bytes that are not a usable Ed25519 public key.
    #[error("the public key presented is malformed")]
    BadPublicKey,

    /// More entries than [`MAX_REVOCATIONS`].
    #[error("the revocation list holds {found} entries, over the limit of {MAX_REVOCATIONS}")]
    TooManyRevocations {
        /// How many it held.
        found: usize,
    },
}

/// A 64-byte `Ed25519` signature, on the wire.
///
/// A newtype rather than a bare array because `serde` implements arrays only up
/// to 32 elements, and because its [`Debug`](std::fmt::Debug) writes nothing a
/// log can be searched for a signature by.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature64(
    /// The raw signature bytes.
    pub [u8; 64],
);

impl std::fmt::Debug for Signature64 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Signature64(..)")
    }
}

impl Serialize for Signature64 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Signature64 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Signature64;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a 64-byte Ed25519 signature")
            }

            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
                let array: [u8; 64] = bytes
                    .try_into()
                    .map_err(|_| E::invalid_length(bytes.len(), &self))?;
                Ok(Signature64(array))
            }
        }
        deserializer.deserialize_bytes(Visitor)
    }
}

/// A canonical name and the casing its holder chose.
///
/// **`canonical` is what is unique.** Two players cannot hold `alice` and
/// `Alice`; the second claim is refused because its canonical form is taken.
/// `display` is what screens show, and it must be `canonical` in another casing
/// so a display name can never say something the canonical form does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    canonical: String,
    display: String,
}

impl Name {
    /// Checks a pair the registry has decided is available and not reserved.
    ///
    /// This is the **claim** path: it applies every rule, reservation included.
    ///
    /// # Errors
    ///
    /// [`RegistryError`] naming the first rule the pair breaks; see
    /// [`Name::from_parts`] for the list, plus [`RegistryError::NameReserved`].
    pub fn claim(canonical: &str, display: &str) -> Result<Self, RegistryError> {
        let name = Self::from_parts(canonical, display)?;
        if is_reserved(&name.canonical) {
            return Err(RegistryError::NameReserved {
                name: name.canonical,
            });
        }
        Ok(name)
    }

    /// Checks the form of a pair, without consulting the reserved list.
    ///
    /// This is what parsing a certificate uses. Reservation is the registry's
    /// decision at claim time, and a server verifying a signed certificate is
    /// answering a different question — "did Iridesium's key vouch for this?" —
    /// so it must not re-litigate a list it does not own.
    ///
    /// # Errors
    ///
    /// [`RegistryError::NameTooShort`], [`NameTooLong`](RegistryError::NameTooLong),
    /// [`NameMustStartWithLetter`](RegistryError::NameMustStartWithLetter),
    /// [`NameBadCharacter`](RegistryError::NameBadCharacter),
    /// [`DisplayBadCharacter`](RegistryError::DisplayBadCharacter) or
    /// [`DisplayMismatch`](RegistryError::DisplayMismatch).
    pub fn from_parts(canonical: &str, display: &str) -> Result<Self, RegistryError> {
        check_canonical(canonical)?;
        check_display(canonical, display)?;
        Ok(Self {
            canonical: canonical.to_owned(),
            display: display.to_owned(),
        })
    }

    /// The canonical form: lowercase, and what uniqueness is decided on.
    #[must_use]
    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    /// The holder's casing, for screens and chat.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }
}

/// Whether a canonical name is on the reserved list.
#[must_use]
pub fn is_reserved(canonical: &str) -> bool {
    RESERVED_NAMES.contains(&canonical)
}

/// The canonical-name rules, every way they can fail.
///
/// # Errors
///
/// As listed on [`Name::from_parts`].
pub fn check_canonical(canonical: &str) -> Result<(), RegistryError> {
    // Characters, not bytes: a name is 3 to 18 characters, and the count must
    // be of what a reader sees. Every character that can appear is ASCII, so a
    // valid name's bytes and characters are the same number.
    let length = canonical.chars().count();
    if length < MIN_NAME_CHARS {
        return Err(RegistryError::NameTooShort { found: length });
    }
    if length > MAX_NAME_CHARS {
        return Err(RegistryError::NameTooLong { found: length });
    }
    for character in canonical.chars() {
        let allowed =
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_';
        if !allowed {
            return Err(RegistryError::NameBadCharacter { character });
        }
    }
    let Some(first) = canonical.chars().next() else {
        // Unreachable: the length check above refused the empty name.
        return Err(RegistryError::NameTooShort { found: 0 });
    };
    if !first.is_ascii_lowercase() {
        return Err(RegistryError::NameMustStartWithLetter);
    }
    Ok(())
}

/// The display-casing rules.
///
/// # Errors
///
/// [`RegistryError::DisplayBadCharacter`] for a character outside `A-Z a-z 0-9 _`,
/// or [`RegistryError::DisplayMismatch`] when the display is not the canonical
/// form in another casing.
fn check_display(canonical: &str, display: &str) -> Result<(), RegistryError> {
    for character in display.chars() {
        let allowed = character.is_ascii_alphanumeric() || character == '_';
        if !allowed {
            return Err(RegistryError::DisplayBadCharacter { character });
        }
    }
    // `eq_ignore_ascii_case` compares the bytes, so a display of a different
    // length fails here too — there is no separate length rule to drift from
    // the canonical one.
    if !display.eq_ignore_ascii_case(canonical) {
        return Err(RegistryError::DisplayMismatch {
            canonical: canonical.to_owned(),
            display: display.to_owned(),
        });
    }
    Ok(())
}

/// Appends a length-prefixed string to a signing payload.
///
/// The length is `u32` rather than `postcard`'s compact form because this
/// payload is **not** the wire format: it is a canonical byte layout for a
/// signature, and an unambiguous length is worth four bytes here.
fn push_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// The leading zero bits of a hash.
fn leading_zero_bits(hash: &[u8; 32]) -> u32 {
    let mut bits = 0;
    for byte in hash {
        if *byte == 0 {
            bits += 8;
            continue;
        }
        bits += byte.leading_zeros();
        break;
    }
    bits
}

/// Verifies a signature over a payload, naming the failure for a disconnect.
fn verify_signature(
    signer: &VerifyingKey,
    payload: &[u8],
    signature: &Signature64,
) -> Result<(), RegistryError> {
    // `verify_strict` rather than `verify`, for the same reason the handshake
    // uses it: non-strict verification admits malleable signatures.
    signer
        .verify_strict(payload, &Signature::from_bytes(&signature.0))
        .map_err(|_| RegistryError::SignatureInvalid)
}

/// Decodes a capped record, refusing an oversized one before reading it.
///
/// The whole message must be the message: `postcard::from_bytes` leaves trailing
/// bytes unread, and a certificate with bytes appended would otherwise be a
/// second valid encoding of the same record. Two encodings of one record are
/// not a signature problem — the signature covers the fields — but they are a
/// parser that promises less than it appears to.
fn decode<'de, T: serde::Deserialize<'de>>(
    what: &'static str,
    bytes: &'de [u8],
    limit: usize,
) -> Result<T, RegistryError> {
    if bytes.len() > limit {
        return Err(RegistryError::TooLarge {
            what,
            len: bytes.len(),
            limit,
        });
    }
    let (value, rest) =
        postcard::take_from_bytes(bytes).map_err(|err| RegistryError::Malformed {
            what,
            detail: err.to_string(),
        })?;
    if !rest.is_empty() {
        return Err(RegistryError::Malformed {
            what,
            detail: format!("{} trailing bytes", rest.len()),
        });
    }
    Ok(value)
}

/// A name bound to a UUID by the registry, for ninety days.
///
/// The signature covers every field above it, in a canonical byte layout the
/// registry and the server both derive from the parsed values
/// ([`NameCertificate::signing_payload`]) — the postcard wire form is a
/// transport, not the signed document. That separation is what lets the wire
/// format gain a field later without silently changing what an old signature
/// meant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameCertificate {
    /// [`CERTIFICATE_VERSION`].
    pub version: u8,
    /// The canonical form: 3–18 characters, lowercase, unique.
    pub name: String,
    /// The holder's casing of the same name.
    pub display: String,
    /// `BLAKE3` of the root public key of the identity the name belongs to.
    pub uuid: [u8; 32],
    /// When the registry issued it, Unix seconds.
    pub issued: i64,
    /// When it stops being honoured, Unix seconds.
    pub expires: i64,
    /// Monotonic per registry key; what a revocation names.
    pub serial: u64,
    /// Which registry public key signed it, raw bytes.
    pub registry_key: [u8; 32],
    /// The registry key's signature over [`Self::signing_payload`].
    pub signature: Signature64,
}

impl NameCertificate {
    /// Issues a certificate for a name the registry has decided is available.
    ///
    /// The lifetime is fixed at [`CERTIFICATE_LIFETIME_SECONDS`] from `issued`;
    /// the serial is the service's monotonic counter per registry key.
    #[must_use]
    pub fn issue(
        registry: &SigningKey,
        name: &Name,
        uuid: PlayerUuid,
        issued: i64,
        serial: u64,
    ) -> Self {
        let mut certificate = Self {
            version: CERTIFICATE_VERSION,
            name: name.canonical().to_owned(),
            display: name.display().to_owned(),
            uuid: *uuid.as_bytes(),
            issued,
            expires: issued.saturating_add(CERTIFICATE_LIFETIME_SECONDS),
            serial,
            registry_key: [0; 32],
            signature: Signature64([0; 64]),
        };
        certificate.sign(registry);
        certificate
    }

    /// Reads a certificate, checks every cap and rule, and trusts nothing else.
    ///
    /// **This does not check the signature** — see [`verify_certificate`],
    /// which is what a server calls. This exists for reading one back in a
    /// test and for the registry that writes them.
    ///
    /// # Errors
    ///
    /// [`RegistryError::TooLarge`] before decoding, then `Malformed`/`Version`
    /// from the wire form, then the name rules and
    /// [`RegistryError::CertificateLifetime`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RegistryError> {
        let certificate: Self = decode("name certificate", bytes, MAX_CERTIFICATE_BYTES)?;
        certificate.check()?;
        Ok(certificate)
    }

    /// Writes the certificate in its postcard wire form.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Malformed`] if `postcard` refuses the value. Every
    /// field is fixed-size or capped, so this is unreachable for a certificate
    /// this module built; it is a `Result` rather than a panic because the
    /// rule is that core does not panic on data.
    pub fn to_bytes(&self) -> Result<Vec<u8>, RegistryError> {
        postcard::to_allocvec(self).map_err(|err| RegistryError::Malformed {
            what: "name certificate",
            detail: err.to_string(),
        })
    }

    /// The bytes the registry signs: a domain tag, then every field but the
    /// signature.
    ///
    /// The tag is what stops a signature gathered for a certificate being
    /// presented as a rotation record's or a claim's — every signed payload in
    /// this module starts with its own.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(128);
        payload.extend_from_slice(b"tiamat:name-certificate:v1");
        payload.push(self.version);
        push_str(&mut payload, &self.name);
        push_str(&mut payload, &self.display);
        payload.extend_from_slice(&self.uuid);
        payload.extend_from_slice(&self.issued.to_le_bytes());
        payload.extend_from_slice(&self.expires.to_le_bytes());
        payload.extend_from_slice(&self.serial.to_le_bytes());
        payload.extend_from_slice(&self.registry_key);
        payload
    }

    /// Signs the certificate and records the registry key that did it.
    ///
    /// Setting `registry_key` here rather than trusting a caller to is the one
    /// invariant this type has: a certificate cannot name a key that did not
    /// sign it.
    pub fn sign(&mut self, registry: &SigningKey) {
        self.registry_key = registry.verifying_key().to_bytes();
        self.signature = Signature64(registry.sign(&self.signing_payload()).to_bytes());
    }

    /// Every rule a parsed certificate must hold.
    fn check(&self) -> Result<(), RegistryError> {
        if self.version != CERTIFICATE_VERSION {
            return Err(RegistryError::Version {
                what: "name certificate",
                found: self.version,
                expected: CERTIFICATE_VERSION,
            });
        }
        Name::from_parts(&self.name, &self.display)?;
        // Checked before subtracting so no input can overflow it: `issued` is
        // non-negative and `expires` is after it, so their difference is too.
        if self.issued < 0 || self.expires <= self.issued {
            return Err(RegistryError::CertificateLifetime {
                issued: self.issued,
                expires: self.expires,
            });
        }
        if self.expires - self.issued > MAX_CERTIFICATE_LIFETIME_SECONDS {
            return Err(RegistryError::CertificateLifetime {
                issued: self.issued,
                expires: self.expires,
            });
        }
        Ok(())
    }
}

/// A registry key that signed before it was rotated out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredRegistryKey {
    /// The key.
    pub key: VerifyingKey,
    /// When its successor took over, Unix seconds.
    pub retired_at: i64,
}

/// What a server or client trusts about the registry, and how it rotates.
///
/// The pinned key plus the hash of its pre-committed successor is exactly what
/// an update client holds for the release key (`crate::release`), applied to
/// names. A rotation is accepted only when the successor hashes to the
/// commitment already held, so a thief with the current key cannot move the
/// registry to a key of their own.
///
/// Retired keys stay in the trust set because a certificate they signed before
/// the rotation is still valid until it expires — the 90 days a client has to
/// renew under the successor key.
#[derive(Debug, Clone)]
pub struct RegistryTrust {
    current: VerifyingKey,
    next: Option<[u8; COMMITMENT_BYTES]>,
    retired: Vec<RetiredRegistryKey>,
}

impl RegistryTrust {
    /// Builds a trust set from the pinned key and its committed successor.
    #[must_use]
    pub fn new(current: VerifyingKey, next: Option<[u8; COMMITMENT_BYTES]>) -> Self {
        Self {
            current,
            next,
            retired: Vec::new(),
        }
    }

    /// The key the registry signs with now.
    #[must_use]
    pub fn current(&self) -> &VerifyingKey {
        &self.current
    }

    /// `BLAKE3` of the successor the current key committed to, if any.
    #[must_use]
    pub fn next_commitment(&self) -> Option<[u8; COMMITMENT_BYTES]> {
        self.next
    }

    /// Keys that have signed in the past, newest last.
    #[must_use]
    pub fn retired(&self) -> &[RetiredRegistryKey] {
        &self.retired
    }

    /// Accepts a rotation record, moving the trust set to its successor.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Version`] for another format,
    /// [`RegistryError::UntrustedRegistryKey`] when the record's retiring key is
    /// not the current one, [`RegistryError::SignatureInvalid`] when it is not
    /// signed by that key, [`RegistryError::BadPublicKey`] for a malformed
    /// successor, [`RegistryError::NoCommittedSuccessor`] when this build holds
    /// no commitment, and [`RegistryError::SuccessorMismatch`] when the named
    /// successor does not hash to it.
    pub fn accept_rotation(&mut self, record: &RotationRecord) -> Result<(), RegistryError> {
        if record.version != ROTATION_VERSION {
            return Err(RegistryError::Version {
                what: "rotation record",
                found: record.version,
                expected: ROTATION_VERSION,
            });
        }
        let retiring =
            public_key_from_bytes(&record.retired).map_err(|_| RegistryError::Malformed {
                what: "rotation record",
                detail: "the retiring key is not a public key".to_owned(),
            })?;
        if retiring != self.current {
            return Err(RegistryError::UntrustedRegistryKey);
        }
        let successor =
            public_key_from_bytes(&record.successor).map_err(|_| RegistryError::BadPublicKey)?;
        verify_signature(&retiring, &record.signing_payload(), &record.signature)?;
        let Some(committed) = self.next else {
            // Deliberate, and the same rule as an uncommitted identity key: a
            // rotation nobody pre-committed to is a rotation a thief can make.
            return Err(RegistryError::NoCommittedSuccessor);
        };
        if commit_to(&successor) != committed {
            return Err(RegistryError::SuccessorMismatch);
        }
        if record.rotated_at < 0 {
            return Err(RegistryError::Malformed {
                what: "rotation record",
                detail: "the rotation is stamped before 1970".to_owned(),
            });
        }
        self.retired.push(RetiredRegistryKey {
            key: retiring,
            retired_at: record.rotated_at,
        });
        self.current = successor;
        self.next = record.next_key_hash;
        Ok(())
    }

    /// The key that may sign a record issued at `issued`, if this build trusts
    /// it: the current key, or a retired key for a record issued before it was
    /// retired.
    fn signer_for(&self, key: &[u8; 32], issued: i64) -> Option<VerifyingKey> {
        let key = public_key_from_bytes(key).ok()?;
        if key == self.current {
            return Some(key);
        }
        self.retired
            .iter()
            .find(|entry| entry.key == key && issued < entry.retired_at)
            .map(|entry| entry.key)
    }
}

/// Checks a certificate against the joining identity, the clock, the trust set
/// and, if the caller holds one, the revocation list.
///
/// **This is the whole offline join check** (§5 of the design). The order is
/// identity, then trust, then time, then signature, then revocation; each
/// failure's [`RegistryError`] display string is the reason to show the player.
///
/// `revocations` is not verified here — a caller that fetched a list verifies
/// it once with [`RevocationList::verify`] and then passes it; verifying it on
/// every join would be wasted work.
///
/// # Errors
///
/// Anything [`NameCertificate::from_bytes`] returns, then
/// [`RegistryError::WrongIdentity`], [`RegistryError::UntrustedRegistryKey`],
/// [`RegistryError::CertificateNotYetValid`], [`RegistryError::CertificateExpired`],
/// [`RegistryError::SignatureInvalid`] or [`RegistryError::SerialRevoked`].
pub fn verify_certificate(
    bytes: &[u8],
    joining: &PlayerUuid,
    now: i64,
    trust: &RegistryTrust,
    revocations: Option<&RevocationList>,
) -> Result<Name, RegistryError> {
    let certificate = NameCertificate::from_bytes(bytes)?;
    if certificate.uuid != *joining.as_bytes() {
        return Err(RegistryError::WrongIdentity);
    }
    let Some(signer) = trust.signer_for(&certificate.registry_key, certificate.issued) else {
        return Err(RegistryError::UntrustedRegistryKey);
    };
    if certificate.issued > now.saturating_add(CLOCK_SKEW_SECONDS) {
        return Err(RegistryError::CertificateNotYetValid);
    }
    if certificate.expires < now.saturating_sub(CLOCK_SKEW_SECONDS) {
        return Err(RegistryError::CertificateExpired);
    }
    verify_signature(
        &signer,
        &certificate.signing_payload(),
        &certificate.signature,
    )?;
    if revocations
        .is_some_and(|list| list.is_revoked(&certificate.registry_key, certificate.serial))
    {
        return Err(RegistryError::SerialRevoked);
    }
    Name::from_parts(&certificate.name, &certificate.display)
}

/// The registry's statement that `retired` handed over to `successor`.
///
/// Published by the registry and fetched with a client's renewal; a server
/// accepts it only against a commitment it already held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationRecord {
    /// [`ROTATION_VERSION`].
    pub version: u8,
    /// The key being replaced, raw bytes.
    pub retired: [u8; 32],
    /// The key replacing it, raw bytes.
    pub successor: [u8; 32],
    /// The successor's own commitment to what will replace it, carried forward.
    pub next_key_hash: Option<[u8; COMMITMENT_BYTES]>,
    /// When the handover happened, Unix seconds.
    pub rotated_at: i64,
    /// The retiring key's signature over [`Self::signing_payload`].
    pub signature: Signature64,
}

impl RotationRecord {
    /// Reads a rotation record under its cap.
    ///
    /// # Errors
    ///
    /// [`RegistryError::TooLarge`], `Malformed`, or [`RegistryError::Version`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RegistryError> {
        let record: Self = decode("rotation record", bytes, MAX_ROTATION_BYTES)?;
        if record.version != ROTATION_VERSION {
            return Err(RegistryError::Version {
                what: "rotation record",
                found: record.version,
                expected: ROTATION_VERSION,
            });
        }
        Ok(record)
    }

    /// The bytes the retiring key signs.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(128);
        payload.extend_from_slice(b"tiamat:registry-rotation:v1");
        payload.push(self.version);
        payload.extend_from_slice(&self.retired);
        payload.extend_from_slice(&self.successor);
        match self.next_key_hash {
            Some(hash) => {
                payload.push(1);
                payload.extend_from_slice(&hash);
            }
            None => payload.push(0),
        }
        payload.extend_from_slice(&self.rotated_at.to_le_bytes());
        payload
    }

    /// Signs the record and records the key that did it.
    pub fn sign(&mut self, retiring: &SigningKey) {
        self.retired = retiring.verifying_key().to_bytes();
        self.signature = Signature64(retiring.sign(&self.signing_payload()).to_bytes());
    }
}

/// One revoked certificate: which registry key issued it, and its serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    /// The registry key that issued the revoked certificate, raw bytes.
    pub registry_key: [u8; 32],
    /// The certificate's serial.
    pub serial: u64,
}

/// A signed list of revoked certificate serials.
///
/// Servers that have internet may fetch one (`GET /v1/revocations` in the
/// design) and hand it to [`verify_certificate`]; servers that do not rely on
/// the certificate's lifetime, which is why it is ninety days. Entries are
/// keyed by registry key as well as serial: a new registry key starts its own
/// serial numbering, and an old serial must not silence a new certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationList {
    /// [`REVOCATION_VERSION`].
    pub version: u8,
    /// Which registry public key signed it, raw bytes.
    pub registry_key: [u8; 32],
    /// When it was issued, Unix seconds.
    pub issued: i64,
    /// The revoked serials.
    pub entries: Vec<Revocation>,
    /// The registry key's signature over [`Self::signing_payload`].
    pub signature: Signature64,
}

impl RevocationList {
    /// Reads a list, refusing an oversized one before decoding it.
    ///
    /// # Errors
    ///
    /// [`RegistryError::TooLarge`], `Malformed`, `Version`, or
    /// [`RegistryError::TooManyRevocations`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RegistryError> {
        let list: Self = decode("revocation list", bytes, MAX_REVOCATION_BYTES)?;
        if list.version != REVOCATION_VERSION {
            return Err(RegistryError::Version {
                what: "revocation list",
                found: list.version,
                expected: REVOCATION_VERSION,
            });
        }
        if list.entries.len() > MAX_REVOCATIONS {
            return Err(RegistryError::TooManyRevocations {
                found: list.entries.len(),
            });
        }
        if list.issued < 0 {
            return Err(RegistryError::Malformed {
                what: "revocation list",
                detail: "the list is stamped before 1970".to_owned(),
            });
        }
        Ok(list)
    }

    /// The bytes the registry signs.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(64 + self.entries.len() * 40);
        payload.extend_from_slice(b"tiamat:revocations:v1");
        payload.push(self.version);
        payload.extend_from_slice(&self.registry_key);
        payload.extend_from_slice(&self.issued.to_le_bytes());
        payload.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for entry in &self.entries {
            payload.extend_from_slice(&entry.registry_key);
            payload.extend_from_slice(&entry.serial.to_le_bytes());
        }
        payload
    }

    /// Signs the list and records the key that did it.
    pub fn sign(&mut self, registry: &SigningKey) {
        self.registry_key = registry.verifying_key().to_bytes();
        self.signature = Signature64(registry.sign(&self.signing_payload()).to_bytes());
    }

    /// Checks that a trusted registry key signed this list.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UntrustedRegistryKey`] when the signing key is not one
    /// this build trusts for a list issued then, and
    /// [`RegistryError::SignatureInvalid`] when the signature does not verify.
    pub fn verify(&self, trust: &RegistryTrust) -> Result<(), RegistryError> {
        let Some(signer) = trust.signer_for(&self.registry_key, self.issued) else {
            return Err(RegistryError::UntrustedRegistryKey);
        };
        verify_signature(&signer, &self.signing_payload(), &self.signature)
    }

    /// Whether this list revokes a serial issued by a registry key.
    #[must_use]
    pub fn is_revoked(&self, registry_key: &[u8; 32], serial: u64) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.registry_key == *registry_key && entry.serial == serial)
    }
}

/// A claim or renewal that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedClaim {
    /// The name, canonical form and display casing.
    pub name: Name,
    /// The identity it belongs to.
    pub uuid: PlayerUuid,
    /// The key that signed the request.
    ///
    /// Whether that key belongs to `uuid` is the **service's** question, not
    /// this module's: a root key *is* the UUID, an added device proves itself
    /// with the key-set record the client sends beside the request (§4), and
    /// that lookup is database work.
    pub public_key: VerifyingKey,
    /// The request's timestamp, already inside the skew.
    pub timestamp: i64,
}

/// A claim, or a renewal of one, signed by the player's own identity.
///
/// `work` is what distinguishes them: a **claim** carries a proof-of-work
/// nonce, a **renewal** is the same request with the same name and no work
/// (§4 of the design). Two verify methods, so a caller cannot accidentally
/// accept a claim without one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRequest {
    /// [`CLAIM_VERSION`].
    pub version: u8,
    /// The canonical name asked for.
    pub name: String,
    /// The casing asked for.
    pub display: String,
    /// The identity claiming it.
    pub uuid: [u8; 32],
    /// The public key that signed this — a root or an authorised device key.
    pub public_key: [u8; 32],
    /// When it was made, Unix seconds.
    pub timestamp: i64,
    /// The proof-of-work nonce: `None` makes this a renewal.
    pub work: Option<u64>,
    /// The player key's signature over [`Self::signing_payload`].
    pub signature: Signature64,
}

impl ClaimRequest {
    /// Builds a request for `identity` to claim `name`.
    ///
    /// The public key is set here, **before** any mining: the proof of work
    /// hashes the signed payload, which includes the key, so a claim mined
    /// against one key and signed by another would be refused for insufficient
    /// work. The constructor exists so that order cannot be got wrong.
    ///
    /// The request starts without work, which makes it renewal-shaped; a claim
    /// is this plus [`ClaimRequest::solve_work`] and [`ClaimRequest::sign`].
    #[must_use]
    pub fn new(identity: &Identity, name: &Name, timestamp: i64) -> Self {
        Self {
            version: CLAIM_VERSION,
            name: name.canonical().to_owned(),
            display: name.display().to_owned(),
            uuid: *identity.uuid_as_root().as_bytes(),
            public_key: identity.public_key().to_bytes(),
            timestamp,
            work: None,
            signature: Signature64([0; 64]),
        }
    }

    /// Reads a request under its cap.
    ///
    /// # Errors
    ///
    /// [`RegistryError::TooLarge`], `Malformed`, [`RegistryError::Version`], or
    /// the name rules from [`Name::from_parts`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RegistryError> {
        let request: Self = decode("claim request", bytes, MAX_REQUEST_BYTES)?;
        if request.version != CLAIM_VERSION {
            return Err(RegistryError::Version {
                what: "claim request",
                found: request.version,
                expected: CLAIM_VERSION,
            });
        }
        // Form only: whether a name may be *claimed* — reserved and free — is
        // the registry's question, answered in `verify_claim`.
        Name::from_parts(&request.name, &request.display)?;
        Ok(request)
    }

    /// The bytes the player's key signs.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(128);
        payload.extend_from_slice(b"tiamat:name-claim:v1");
        payload.push(self.version);
        push_str(&mut payload, &self.name);
        push_str(&mut payload, &self.display);
        payload.extend_from_slice(&self.uuid);
        payload.extend_from_slice(&self.public_key);
        payload.extend_from_slice(&self.timestamp.to_le_bytes());
        match self.work {
            Some(work) => {
                payload.push(1);
                payload.extend_from_slice(&work.to_le_bytes());
            }
            None => payload.push(0),
        }
        payload
    }

    /// Signs the request and records the key that did it.
    pub fn sign(&mut self, identity: &Identity) {
        self.public_key = identity.public_key().to_bytes();
        self.signature = Signature64(identity.sign(&self.signing_payload()).to_bytes());
    }

    /// The proof of work this request carries, in leading zero bits.
    ///
    /// `None` for a renewal, which carries none.
    #[must_use]
    pub fn work_bits(&self) -> Option<u32> {
        self.work.map(|_| work_bits(&self.signing_payload()))
    }

    /// Mines a nonce whose proof of work reaches `bits`, and stores it.
    ///
    /// Run **before** [`ClaimRequest::sign`]: the nonce is part of the signed
    /// payload, so a claim is signed once, after the mine, rather than once per
    /// attempt. Every field the payload covers is fixed by then — a request
    /// built by [`ClaimRequest::new`] already carries its key, which is the
    /// field a caller could otherwise change after mining.
    ///
    /// Returns `None` only when `bits` exceeds [`MAX_WORK_BITS`] (a caller's
    /// bug: `2^40` hashes is days of one core) or when every nonce in the
    /// space was tried, which at any bit count that is read this way cannot
    /// happen.
    pub fn solve_work(&mut self, bits: u32) -> Option<u64> {
        if bits > MAX_WORK_BITS {
            return None;
        }
        self.work = Some(0);
        let mut buffer = Vec::with_capacity(160);
        buffer.extend_from_slice(b"tiamat:name-work:v1");
        buffer.extend_from_slice(&self.signing_payload());
        // The payload ends with the work's eight bytes (the present flag is
        // before them), so only those change as the nonce is searched.
        let start = buffer.len().saturating_sub(8);
        for nonce in 0..=u64::MAX {
            buffer[start..].copy_from_slice(&nonce.to_le_bytes());
            if leading_zero_bits(blake3::hash(&buffer).as_bytes()) >= bits {
                self.work = Some(nonce);
                return Some(nonce);
            }
        }
        None
    }

    /// Verifies a **claim**: the name may be taken, the work is enough, the
    /// clock agrees, and the signature holds.
    ///
    /// # Errors
    ///
    /// The name rules plus [`RegistryError::NameReserved`],
    /// [`RegistryError::StaleRequest`], [`RegistryError::MissingWork`],
    /// [`RegistryError::InsufficientWork`], [`RegistryError::BadPublicKey`], or
    /// [`RegistryError::SignatureInvalid`].
    pub fn verify_claim(
        &self,
        now: i64,
        required_bits: u32,
    ) -> Result<VerifiedClaim, RegistryError> {
        let name = Name::claim(&self.name, &self.display)?;
        self.check_clock(now)?;
        if self.work.is_none() {
            return Err(RegistryError::MissingWork);
        }
        let found = work_bits(&self.signing_payload());
        if found < required_bits {
            return Err(RegistryError::InsufficientWork {
                found,
                required: required_bits,
            });
        }
        let key = self.verified_key()?;
        Ok(VerifiedClaim {
            name,
            uuid: PlayerUuid::from_bytes(self.uuid),
            public_key: key,
            timestamp: self.timestamp,
        })
    }

    /// Verifies a **renewal**: the same request, without the work.
    ///
    /// Reservation is deliberately not re-checked: a name that was free when it
    /// was claimed must keep renewing even if a later release reserves it, or a
    /// release of the engine would take names away from players.
    ///
    /// # Errors
    ///
    /// The name rules, [`RegistryError::StaleRequest`],
    /// [`RegistryError::BadPublicKey`], or [`RegistryError::SignatureInvalid`].
    pub fn verify_renewal(&self, now: i64) -> Result<VerifiedClaim, RegistryError> {
        let name = Name::from_parts(&self.name, &self.display)?;
        self.check_clock(now)?;
        let key = self.verified_key()?;
        Ok(VerifiedClaim {
            name,
            uuid: PlayerUuid::from_bytes(self.uuid),
            public_key: key,
            timestamp: self.timestamp,
        })
    }

    /// The signing key, parsed, and the signature checked over the payload.
    fn verified_key(&self) -> Result<VerifyingKey, RegistryError> {
        let key =
            public_key_from_bytes(&self.public_key).map_err(|_| RegistryError::BadPublicKey)?;
        verify_signature(&key, &self.signing_payload(), &self.signature)?;
        Ok(key)
    }

    /// Refuses a timestamp outside the skew, in either direction.
    fn check_clock(&self, now: i64) -> Result<(), RegistryError> {
        check_timestamp(self.timestamp, now)
    }
}

/// The leading zero bits of a claim's proof of work over its signing payload.
fn work_bits(payload: &[u8]) -> u32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tiamat:name-work:v1");
    hasher.update(payload);
    leading_zero_bits(hasher.finalize().as_bytes())
}

/// A release that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRelease {
    /// The canonical name being given up.
    pub name: String,
    /// The identity that held it.
    pub uuid: PlayerUuid,
    /// The key that signed the request.
    pub public_key: VerifyingKey,
}

/// A signed statement that an identity is giving up a name.
///
/// A rename is a release followed by a claim (§3); the released name is held
/// for its old UUID for 30 days, which is the service's bookkeeping, not this
/// type's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRequest {
    /// [`RELEASE_VERSION`].
    pub version: u8,
    /// The canonical name being released.
    pub name: String,
    /// The identity giving it up.
    pub uuid: [u8; 32],
    /// The public key that signed this.
    pub public_key: [u8; 32],
    /// When it was made, Unix seconds.
    pub timestamp: i64,
    /// The player key's signature over [`Self::signing_payload`].
    pub signature: Signature64,
}

impl ReleaseRequest {
    /// Builds a release by `identity` of `canonical`.
    ///
    /// The name is not checked here — [`ReleaseRequest::from_bytes`] and
    /// [`ReleaseRequest::verify`] check it, and a constructor that refused one
    /// would only move the error somewhere less obvious.
    #[must_use]
    pub fn new(identity: &Identity, canonical: &str, timestamp: i64) -> Self {
        Self {
            version: RELEASE_VERSION,
            name: canonical.to_owned(),
            uuid: *identity.uuid_as_root().as_bytes(),
            public_key: identity.public_key().to_bytes(),
            timestamp,
            signature: Signature64([0; 64]),
        }
    }

    /// Reads a release under its cap.
    ///
    /// # Errors
    ///
    /// [`RegistryError::TooLarge`], `Malformed`, [`RegistryError::Version`], or
    /// the canonical-name rules from [`check_canonical`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RegistryError> {
        let request: Self = decode("release request", bytes, MAX_REQUEST_BYTES)?;
        if request.version != RELEASE_VERSION {
            return Err(RegistryError::Version {
                what: "release request",
                found: request.version,
                expected: RELEASE_VERSION,
            });
        }
        check_canonical(&request.name)?;
        Ok(request)
    }

    /// The bytes the player's key signs.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(96);
        payload.extend_from_slice(b"tiamat:name-release:v1");
        payload.push(self.version);
        push_str(&mut payload, &self.name);
        payload.extend_from_slice(&self.uuid);
        payload.extend_from_slice(&self.public_key);
        payload.extend_from_slice(&self.timestamp.to_le_bytes());
        payload
    }

    /// Signs the release and records the key that did it.
    pub fn sign(&mut self, identity: &Identity) {
        self.public_key = identity.public_key().to_bytes();
        self.signature = Signature64(identity.sign(&self.signing_payload()).to_bytes());
    }

    /// Verifies the release.
    ///
    /// # Errors
    ///
    /// [`RegistryError::StaleRequest`], [`RegistryError::BadPublicKey`] or
    /// [`RegistryError::SignatureInvalid`].
    pub fn verify(&self, now: i64) -> Result<VerifiedRelease, RegistryError> {
        check_canonical(&self.name)?;
        check_timestamp(self.timestamp, now)?;
        let key =
            public_key_from_bytes(&self.public_key).map_err(|_| RegistryError::BadPublicKey)?;
        verify_signature(&key, &self.signing_payload(), &self.signature)?;
        Ok(VerifiedRelease {
            name: self.name.clone(),
            uuid: PlayerUuid::from_bytes(self.uuid),
            public_key: key,
        })
    }
}

/// The longest recovery address accepted: the RFC's 254-byte path limit.
///
/// Validation here is deliberately shallow — one `@`, both sides non-empty, a
/// dot in the domain, printable ASCII — because the only proof an address is
/// real is the link that comes back from it. A deep RFC 5322 parser would be a
/// parser on the hostile-input path earning nothing.
pub const MAX_EMAIL_BYTES: usize = 254;

/// Email-request format this build reads.
pub const EMAIL_VERSION: u8 = 1;

/// Recovery-request format this build reads.
pub const RECOVERY_VERSION: u8 = 1;

/// Recovery-cancellation format this build reads.
pub const CANCEL_VERSION: u8 = 1;

/// Checks a recovery address, shallowly.
///
/// # Errors
///
/// [`RegistryError::EmailAddress`] when it is not one.
pub fn check_email_address(address: &str) -> Result<(), RegistryError> {
    let invalid = || RegistryError::EmailAddress {
        address: address.to_owned(),
    };
    if address.len() > MAX_EMAIL_BYTES
        || !address.is_ascii()
        || address
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(invalid());
    }
    let mut parts = address.split('@');
    let Some(local) = parts.next() else {
        return Err(invalid());
    };
    let Some(domain) = parts.next() else {
        return Err(invalid());
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(invalid());
    }
    Ok(())
}

/// A request's timestamp must be within [`CLOCK_SKEW_SECONDS`] of ours.
fn check_timestamp(at: i64, now: i64) -> Result<(), RegistryError> {
    match now.checked_sub(at) {
        Some(delta) if delta.unsigned_abs() <= CLOCK_SKEW_SECONDS.unsigned_abs() => Ok(()),
        _ => Err(RegistryError::StaleRequest),
    }
}

/// A signed request to bind, change, or remove the recovery address.
///
/// The address is **not** stored by a request: a token is mailed to it, and
/// only following the link stores it (§4). `address: None` removes a bound
/// address, with the same signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailRequest {
    /// [`EMAIL_VERSION`].
    pub version: u8,
    /// The identity the address belongs to.
    pub uuid: [u8; 32],
    /// The public key that signed this.
    pub public_key: [u8; 32],
    /// The address to bind or change to; `None` removes the bound one.
    pub address: Option<String>,
    /// When it was made, Unix seconds.
    pub timestamp: i64,
    /// The player key's signature over [`Self::signing_payload`].
    pub signature: Signature64,
}

impl EmailRequest {
    /// Reads a request under the cap, checking its version and its address.
    ///
    /// # Errors
    ///
    /// [`RegistryError::TooLarge`], `Malformed`, [`RegistryError::Version`], or
    /// [`RegistryError::EmailAddress`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RegistryError> {
        let request: Self = decode("email request", bytes, MAX_REQUEST_BYTES)?;
        if request.version != EMAIL_VERSION {
            return Err(RegistryError::Version {
                what: "email request",
                found: request.version,
                expected: EMAIL_VERSION,
            });
        }
        if let Some(address) = &request.address {
            check_email_address(address)?;
        }
        Ok(request)
    }

    /// The bytes the player's key signs.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(96);
        payload.extend_from_slice(b"tiamat:registry-email:v1");
        payload.push(self.version);
        payload.extend_from_slice(&self.uuid);
        payload.extend_from_slice(&self.public_key);
        match &self.address {
            Some(address) => {
                payload.push(1);
                push_str(&mut payload, address);
            }
            None => payload.push(0),
        }
        payload.extend_from_slice(&self.timestamp.to_le_bytes());
        payload
    }

    /// Signs the request and records the key that did it.
    pub fn sign(&mut self, identity: &Identity) {
        self.public_key = identity.public_key().to_bytes();
        self.signature = Signature64(identity.sign(&self.signing_payload()).to_bytes());
    }

    /// Verifies the signature and the clock.
    ///
    /// Whether the key belongs to the UUID is the service's question, as it is
    /// for a claim.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Version`], [`RegistryError::EmailAddress`],
    /// [`RegistryError::StaleRequest`], [`RegistryError::BadPublicKey`] or
    /// [`RegistryError::SignatureInvalid`].
    pub fn verify(&self, now: i64) -> Result<VerifiedEmail, RegistryError> {
        if self.version != EMAIL_VERSION {
            return Err(RegistryError::Version {
                what: "email request",
                found: self.version,
                expected: EMAIL_VERSION,
            });
        }
        if let Some(address) = &self.address {
            check_email_address(address)?;
        }
        check_timestamp(self.timestamp, now)?;
        let key =
            public_key_from_bytes(&self.public_key).map_err(|_| RegistryError::BadPublicKey)?;
        verify_signature(&key, &self.signing_payload(), &self.signature)?;
        Ok(VerifiedEmail {
            uuid: PlayerUuid::from_bytes(self.uuid),
            address: self.address.clone(),
            public_key: key,
            timestamp: self.timestamp,
        })
    }
}

/// An email request that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedEmail {
    /// The identity the address belongs to.
    pub uuid: PlayerUuid,
    /// The address to bind, or `None` to remove the bound one.
    pub address: Option<String>,
    /// The key that signed the request.
    pub public_key: VerifyingKey,
    /// The request's timestamp, already inside the skew.
    pub timestamp: i64,
}

/// A signed request from a **new** identity to recover a name.
///
/// Signed by the new key, not the old one: this is the player who has lost the
/// phrase and the device and wants the name moved to what they hold now. The
/// registry mails the bound address, and the notice that follows is what the
/// current holder can cancel (§4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryRequest {
    /// [`RECOVERY_VERSION`].
    pub version: u8,
    /// The canonical name being recovered.
    pub name: String,
    /// The identity it would move to.
    pub new_uuid: [u8; 32],
    /// The new identity's public key, which signed this.
    pub public_key: [u8; 32],
    /// When it was made, Unix seconds.
    pub timestamp: i64,
    /// The new key's signature over [`Self::signing_payload`].
    pub signature: Signature64,
}

impl RecoveryRequest {
    /// Reads a request under the cap.
    ///
    /// # Errors
    ///
    /// [`RegistryError::TooLarge`], `Malformed`, [`RegistryError::Version`], or
    /// the canonical-name rules.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RegistryError> {
        let request: Self = decode("recovery request", bytes, MAX_REQUEST_BYTES)?;
        if request.version != RECOVERY_VERSION {
            return Err(RegistryError::Version {
                what: "recovery request",
                found: request.version,
                expected: RECOVERY_VERSION,
            });
        }
        check_canonical(&request.name)?;
        Ok(request)
    }

    /// The bytes the new identity's key signs.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(128);
        payload.extend_from_slice(b"tiamat:registry-recovery:v1");
        payload.push(self.version);
        push_str(&mut payload, &self.name);
        payload.extend_from_slice(&self.new_uuid);
        payload.extend_from_slice(&self.public_key);
        payload.extend_from_slice(&self.timestamp.to_le_bytes());
        payload
    }

    /// Signs the request and records the key that did it.
    pub fn sign(&mut self, identity: &Identity) {
        self.public_key = identity.public_key().to_bytes();
        self.signature = Signature64(identity.sign(&self.signing_payload()).to_bytes());
    }

    /// Verifies the signature and the clock.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Version`], the canonical-name rules,
    /// [`RegistryError::StaleRequest`], [`RegistryError::BadPublicKey`] or
    /// [`RegistryError::SignatureInvalid`].
    pub fn verify(&self, now: i64) -> Result<VerifiedRecovery, RegistryError> {
        if self.version != RECOVERY_VERSION {
            return Err(RegistryError::Version {
                what: "recovery request",
                found: self.version,
                expected: RECOVERY_VERSION,
            });
        }
        check_canonical(&self.name)?;
        check_timestamp(self.timestamp, now)?;
        let key =
            public_key_from_bytes(&self.public_key).map_err(|_| RegistryError::BadPublicKey)?;
        verify_signature(&key, &self.signing_payload(), &self.signature)?;
        Ok(VerifiedRecovery {
            name: self.name.clone(),
            new_uuid: PlayerUuid::from_bytes(self.new_uuid),
            public_key: key,
            timestamp: self.timestamp,
        })
    }
}

/// A recovery request that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRecovery {
    /// The canonical name asked for.
    pub name: String,
    /// The identity it would move to.
    pub new_uuid: PlayerUuid,
    /// The new identity's key.
    pub public_key: VerifyingKey,
    /// The request's timestamp, already inside the skew.
    pub timestamp: i64,
}

/// A signed request from the **current** holder to stop a recovery.
///
/// This is the defence against a stolen mailbox: the holder is still playing,
/// and one signed request ends the fourteen-day notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelRecoveryRequest {
    /// [`CANCEL_VERSION`].
    pub version: u8,
    /// The canonical name whose recovery is cancelled.
    pub name: String,
    /// The identity that holds it now.
    pub uuid: [u8; 32],
    /// The holder's public key, which signed this.
    pub public_key: [u8; 32],
    /// When it was made, Unix seconds.
    pub timestamp: i64,
    /// The holder's signature over [`Self::signing_payload`].
    pub signature: Signature64,
}

impl CancelRecoveryRequest {
    /// Reads a request under the cap.
    ///
    /// # Errors
    ///
    /// [`RegistryError::TooLarge`], `Malformed`, [`RegistryError::Version`], or
    /// the canonical-name rules.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RegistryError> {
        let request: Self = decode("recovery cancellation", bytes, MAX_REQUEST_BYTES)?;
        if request.version != CANCEL_VERSION {
            return Err(RegistryError::Version {
                what: "recovery cancellation",
                found: request.version,
                expected: CANCEL_VERSION,
            });
        }
        check_canonical(&request.name)?;
        Ok(request)
    }

    /// The bytes the holder's key signs.
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(128);
        payload.extend_from_slice(b"tiamat:registry-recovery-cancel:v1");
        payload.push(self.version);
        push_str(&mut payload, &self.name);
        payload.extend_from_slice(&self.uuid);
        payload.extend_from_slice(&self.public_key);
        payload.extend_from_slice(&self.timestamp.to_le_bytes());
        payload
    }

    /// Signs the request and records the key that did it.
    pub fn sign(&mut self, identity: &Identity) {
        self.public_key = identity.public_key().to_bytes();
        self.signature = Signature64(identity.sign(&self.signing_payload()).to_bytes());
    }

    /// Verifies the signature and the clock.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Version`], the canonical-name rules,
    /// [`RegistryError::StaleRequest`], [`RegistryError::BadPublicKey`] or
    /// [`RegistryError::SignatureInvalid`].
    pub fn verify(&self, now: i64) -> Result<VerifiedCancellation, RegistryError> {
        if self.version != CANCEL_VERSION {
            return Err(RegistryError::Version {
                what: "recovery cancellation",
                found: self.version,
                expected: CANCEL_VERSION,
            });
        }
        check_canonical(&self.name)?;
        check_timestamp(self.timestamp, now)?;
        let key =
            public_key_from_bytes(&self.public_key).map_err(|_| RegistryError::BadPublicKey)?;
        verify_signature(&key, &self.signing_payload(), &self.signature)?;
        Ok(VerifiedCancellation {
            name: self.name.clone(),
            uuid: PlayerUuid::from_bytes(self.uuid),
            public_key: key,
            timestamp: self.timestamp,
        })
    }
}

/// A cancellation that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCancellation {
    /// The canonical name whose recovery is cancelled.
    pub name: String,
    /// The identity that holds it.
    pub uuid: PlayerUuid,
    /// The holder's key.
    pub public_key: VerifyingKey,
    /// The request's timestamp, already inside the skew.
    pub timestamp: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    /// A fixed instant, so the tests read in calendar terms rather than
    /// relative ones.
    const NOW: i64 = 1_800_000_000;

    /// A registry key from a fixed seed: deterministic tests, and no entropy
    /// source in a unit test.
    fn registry_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    /// A player from a fixed seed.
    fn player(seed: u8) -> Identity {
        Identity::from_seed(&[seed; 32])
    }

    /// A certificate for `identity`, signed by `registry`.
    fn certificate(
        registry: &SigningKey,
        identity: &Identity,
        canonical: &str,
        display: &str,
        issued: i64,
        serial: u64,
    ) -> NameCertificate {
        let name = Name::from_parts(canonical, display).expect("a form-valid test name");
        NameCertificate::issue(registry, &name, identity.uuid_as_root(), issued, serial)
    }

    /// The ordinary case: alice, named by the pinned key, valid now.
    fn trust(registry: &SigningKey) -> RegistryTrust {
        RegistryTrust::new(registry.verifying_key(), None)
    }

    fn bytes(certificate: &NameCertificate) -> Vec<u8> {
        certificate.to_bytes().expect("encode")
    }

    #[test]
    fn the_name_rules_hold() {
        // The length rule counts characters.
        assert_eq!(
            Name::from_parts("ab", "ab"),
            Err(RegistryError::NameTooShort { found: 2 })
        );
        assert_eq!(
            Name::from_parts("abcdefghijklmnopqrs", "abcdefghijklmnopqrs"),
            Err(RegistryError::NameTooLong { found: 19 })
        );
        // The first character must be a letter, and letters are lowercase.
        assert_eq!(
            Name::from_parts("1abc", "1abc"),
            Err(RegistryError::NameMustStartWithLetter)
        );
        assert_eq!(
            Name::from_parts("_abc", "_abc"),
            Err(RegistryError::NameMustStartWithLetter)
        );
        assert_eq!(
            Name::from_parts("Alice", "Alice"),
            Err(RegistryError::NameBadCharacter { character: 'A' })
        );
        assert_eq!(
            Name::from_parts("ab-c", "ab-c"),
            Err(RegistryError::NameBadCharacter { character: '-' })
        );
        assert_eq!(
            Name::from_parts("naïve", "naïve"),
            Err(RegistryError::NameBadCharacter { character: 'ï' })
        );
        // The display is the canonical form in another casing, and nothing
        // else: neither a different name nor a different length, and no
        // characters the canonical form could not hold.
        assert_eq!(
            Name::from_parts("alice", "alic3").unwrap_err(),
            RegistryError::DisplayMismatch {
                canonical: "alice".to_owned(),
                display: "alic3".to_owned(),
            }
        );
        assert_eq!(
            Name::from_parts("alice", "alice_").unwrap_err(),
            RegistryError::DisplayMismatch {
                canonical: "alice".to_owned(),
                display: "alice_".to_owned(),
            }
        );
        assert_eq!(
            Name::from_parts("alice", "ali ce").unwrap_err(),
            RegistryError::DisplayBadCharacter { character: ' ' }
        );
        // The accepted forms, and the accessors that carry them.
        let name = Name::from_parts("alice", "ALICE").expect("display casing is free");
        assert_eq!(name.canonical(), "alice");
        assert_eq!(name.display(), "ALICE");
        // Reservation is a claim rule, not a form rule.
        assert_eq!(
            Name::claim("admin", "Admin").unwrap_err(),
            RegistryError::NameReserved {
                name: "admin".to_owned()
            }
        );
        assert!(is_reserved("tiamat_weather"));
        assert!(!is_reserved("alice"));
        assert!(Name::claim("alice", "Alice").is_ok());
    }

    #[test]
    fn a_certificate_round_trips_under_its_cap() {
        let registry = registry_key(7);
        let alice = player(1);
        // Every field at its limit: an 18-character name and display.
        let certificate = certificate(
            &registry,
            &alice,
            "abcdefghijklmnopqr",
            "ABCDEFGHIJKLMNOPQR",
            NOW,
            9,
        );
        let wire = bytes(&certificate);
        assert!(wire.len() <= MAX_CERTIFICATE_BYTES, "{} bytes", wire.len());
        let parsed = NameCertificate::from_bytes(&wire).expect("parse");
        assert_eq!(parsed, certificate);
        // The signed layout is a function of the parsed value, so a record read
        // back signs the same bytes it did when it was written.
        assert_eq!(parsed.signing_payload(), certificate.signing_payload());
    }

    #[test]
    fn a_certificate_parse_is_total_and_strict() {
        // Over the cap, refused before a byte is decoded.
        assert!(matches!(
            NameCertificate::from_bytes(&[0u8; MAX_CERTIFICATE_BYTES + 1]),
            Err(RegistryError::TooLarge { .. })
        ));
        // Empty, and truncated after a valid prefix.
        assert!(matches!(
            NameCertificate::from_bytes(&[]),
            Err(RegistryError::Malformed { .. })
        ));
        let wire = bytes(&certificate(
            &registry_key(7),
            &player(1),
            "alice",
            "Alice",
            NOW,
            1,
        ));
        let mut trailing = wire.clone();
        trailing.push(0);
        assert!(matches!(
            NameCertificate::from_bytes(&trailing),
            Err(RegistryError::Malformed { .. })
        ));
        // A version from the future.
        let mut future = certificate(&registry_key(7), &player(1), "alice", "Alice", NOW, 1);
        future.version = CERTIFICATE_VERSION + 1;
        assert!(matches!(
            NameCertificate::from_bytes(&bytes(&future)),
            Err(RegistryError::Version { .. })
        ));
        // A lifetime that is backwards, or absurd.
        let mut backwards = certificate(&registry_key(7), &player(1), "alice", "Alice", NOW, 1);
        backwards.expires = backwards.issued;
        assert!(matches!(
            NameCertificate::from_bytes(&bytes(&backwards)),
            Err(RegistryError::CertificateLifetime { .. })
        ));
        let mut absurd = certificate(&registry_key(7), &player(1), "alice", "Alice", NOW, 1);
        absurd.expires = absurd.issued + MAX_CERTIFICATE_LIFETIME_SECONDS + 1;
        assert!(matches!(
            NameCertificate::from_bytes(&bytes(&absurd)),
            Err(RegistryError::CertificateLifetime { .. })
        ));
        // The name rules are re-checked on the way in.
        let mut bad_name = certificate(&registry_key(7), &player(1), "alice", "Alice", NOW, 1);
        bad_name.name = "ALICE".to_owned();
        assert!(matches!(
            NameCertificate::from_bytes(&bytes(&bad_name)),
            Err(RegistryError::NameBadCharacter { .. })
        ));
    }

    #[test]
    fn a_good_certificate_names_its_holder() {
        let registry = registry_key(7);
        let alice = player(1);
        let certificate = certificate(&registry, &alice, "alice", "Alice", NOW - 1_000, 1);
        let name = verify_certificate(
            &bytes(&certificate),
            &alice.uuid_as_root(),
            NOW,
            &trust(&registry),
            None,
        )
        .expect("a certificate signed by the pinned key, for this identity, valid now");
        assert_eq!(name.canonical(), "alice");
        assert_eq!(name.display(), "Alice");
        // Five minutes of skew, in both directions, is exactly the bound.
        assert!(
            verify_certificate(
                &bytes(&certificate),
                &alice.uuid_as_root(),
                NOW,
                &RegistryTrust::new(registry.verifying_key(), None),
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn a_forged_certificate_is_refused() {
        let registry = registry_key(7);
        let alice = player(1);
        let certificate = certificate(&registry, &alice, "alice", "Alice", NOW, 1);
        let mut wire = bytes(&certificate);
        // The signature is the last 64 bytes of the wire form; flipping one bit
        // in it must take the certificate down without touching a field.
        let last = wire.len() - 1;
        wire[last] ^= 0b0001;
        assert_eq!(
            verify_certificate(&wire, &alice.uuid_as_root(), NOW, &trust(&registry), None),
            Err(RegistryError::SignatureInvalid)
        );
    }

    #[test]
    fn a_certificate_for_another_identity_is_refused() {
        let registry = registry_key(7);
        let alice = player(1);
        let mallory = player(2);
        let certificate = certificate(&registry, &alice, "alice", "Alice", NOW, 1);
        assert_eq!(
            verify_certificate(
                &bytes(&certificate),
                &mallory.uuid_as_root(),
                NOW,
                &trust(&registry),
                None
            ),
            Err(RegistryError::WrongIdentity)
        );
    }

    #[test]
    fn expiry_and_not_yet_validity_respect_the_skew() {
        let registry = registry_key(7);
        let alice = player(1);
        let trust = trust(&registry);
        let certificate = certificate(&registry, &alice, "alice", "Alice", NOW - 1_000, 1);
        let wire = bytes(&certificate);
        let uuid = alice.uuid_as_root();

        let expired = certificate.expires + CLOCK_SKEW_SECONDS;
        assert_eq!(
            verify_certificate(&wire, &uuid, expired + 1, &trust, None),
            Err(RegistryError::CertificateExpired)
        );
        assert!(verify_certificate(&wire, &uuid, expired, &trust, None).is_ok());

        let issued = certificate.issued - CLOCK_SKEW_SECONDS;
        assert_eq!(
            verify_certificate(&wire, &uuid, issued - 1, &trust, None),
            Err(RegistryError::CertificateNotYetValid)
        );
        assert!(verify_certificate(&wire, &uuid, issued, &trust, None).is_ok());
    }

    #[test]
    fn a_certificate_from_an_untrusted_key_is_refused() {
        let registry = registry_key(7);
        let impostor = registry_key(8);
        let alice = player(1);
        let uuid = alice.uuid_as_root();

        // Signed by a key that is not the pinned one.
        let impostor_certificate = certificate(&impostor, &alice, "alice", "Alice", NOW, 1);
        assert_eq!(
            verify_certificate(
                &bytes(&impostor_certificate),
                &uuid,
                NOW,
                &trust(&registry),
                None
            ),
            Err(RegistryError::UntrustedRegistryKey)
        );

        // The registry_key field forged to name the pinned key, while the
        // signature stays the impostor's: trust passes, the signature does not.
        let mut forged = certificate(&impostor, &alice, "alice", "Alice", NOW, 1);
        forged.registry_key = registry.verifying_key().to_bytes();
        assert_eq!(
            verify_certificate(&bytes(&forged), &uuid, NOW, &trust(&registry), None),
            Err(RegistryError::SignatureInvalid)
        );
    }

    #[test]
    fn a_revoked_serial_is_refused() {
        let registry = registry_key(7);
        let alice = player(1);
        let alices = certificate(&registry, &alice, "alice", "Alice", NOW, 1);
        let wire = bytes(&alices);
        let mut list = RevocationList {
            version: REVOCATION_VERSION,
            registry_key: [0; 32],
            issued: NOW,
            entries: vec![Revocation {
                registry_key: registry.verifying_key().to_bytes(),
                serial: 1,
            }],
            signature: Signature64([0; 64]),
        };
        list.sign(&registry);
        let list = RevocationList::from_bytes(&postcard::to_allocvec(&list).expect("encode"))
            .expect("parse");
        list.verify(&trust(&registry))
            .expect("signed by the pinned key");

        assert!(list.is_revoked(&registry.verifying_key().to_bytes(), 1));
        assert!(!list.is_revoked(&registry.verifying_key().to_bytes(), 2));
        assert_eq!(
            verify_certificate(
                &wire,
                &alice.uuid_as_root(),
                NOW,
                &trust(&registry),
                Some(&list)
            ),
            Err(RegistryError::SerialRevoked)
        );
        // The same list beside a different serial leaves the certificate alone.
        let other = certificate(&registry, &alice, "alice", "Alice", NOW, 2);
        assert!(
            verify_certificate(
                &bytes(&other),
                &alice.uuid_as_root(),
                NOW,
                &trust(&registry),
                Some(&list)
            )
            .is_ok()
        );
    }

    #[test]
    fn a_revocation_list_is_capped_and_signed() {
        let registry = registry_key(7);
        // One entry over the limit, still inside the byte cap (4097 × 40).
        let mut list = RevocationList {
            version: REVOCATION_VERSION,
            registry_key: [0; 32],
            issued: NOW,
            entries: vec![
                Revocation {
                    registry_key: [0; 32],
                    serial: 0,
                };
                MAX_REVOCATIONS + 1
            ],
            signature: Signature64([0; 64]),
        };
        list.sign(&registry);
        let wire = postcard::to_allocvec(&list).expect("encode");
        assert!(wire.len() < MAX_REVOCATION_BYTES);
        assert!(matches!(
            RevocationList::from_bytes(&wire),
            Err(RegistryError::TooManyRevocations { found }) if found == MAX_REVOCATIONS + 1
        ));

        // Over the byte cap, refused before decoding.
        assert!(matches!(
            RevocationList::from_bytes(&vec![0u8; MAX_REVOCATION_BYTES + 1]),
            Err(RegistryError::TooLarge { .. })
        ));

        // Signed by a key outside the trust set.
        let mut impostor = RevocationList {
            version: REVOCATION_VERSION,
            registry_key: [0; 32],
            issued: NOW,
            entries: Vec::new(),
            signature: Signature64([0; 64]),
        };
        impostor.sign(&registry_key(8));
        assert_eq!(
            impostor.verify(&trust(&registry)),
            Err(RegistryError::UntrustedRegistryKey)
        );
    }

    /// A rotation record from `retiring` to `successor`, carrying `next`.
    fn rotation(
        retiring: &SigningKey,
        successor: &SigningKey,
        next: Option<[u8; COMMITMENT_BYTES]>,
        at: i64,
    ) -> RotationRecord {
        let mut record = RotationRecord {
            version: ROTATION_VERSION,
            retired: [0; 32],
            successor: successor.verifying_key().to_bytes(),
            next_key_hash: next,
            rotated_at: at,
            signature: Signature64([0; 64]),
        };
        record.sign(retiring);
        record
    }

    #[test]
    fn a_rotation_needs_its_commitment_and_its_signature() {
        let first = registry_key(1);
        let second = registry_key(2);
        let third = registry_key(3);

        // No commitment held: refused, deliberately.
        let mut uncommitted = RegistryTrust::new(first.verifying_key(), None);
        assert_eq!(
            uncommitted.accept_rotation(&rotation(&first, &second, None, NOW)),
            Err(RegistryError::NoCommittedSuccessor)
        );

        // A successor that is not the committed one: refused.
        let mut trust = RegistryTrust::new(
            first.verifying_key(),
            Some(commit_to(&second.verifying_key())),
        );
        assert_eq!(
            trust.accept_rotation(&rotation(&first, &third, None, NOW)),
            Err(RegistryError::SuccessorMismatch)
        );

        // A record signed by a key that is not the current one: refused.
        assert_eq!(
            trust.accept_rotation(&rotation(&second, &second, None, NOW)),
            Err(RegistryError::UntrustedRegistryKey)
        );

        // The committed successor itself: accepted, and the advice carried on.
        let committed = commit_to(&third.verifying_key());
        trust
            .accept_rotation(&rotation(&first, &second, Some(committed), NOW))
            .expect("the committed successor");
        assert_eq!(trust.current(), &second.verifying_key());
        assert_eq!(trust.next_commitment(), Some(committed));
        assert_eq!(trust.retired().len(), 1);
        assert_eq!(trust.retired()[0].key, first.verifying_key());

        // And the chain continues: the successor's successor is now accepted.
        trust
            .accept_rotation(&rotation(&second, &third, None, NOW + 10))
            .expect("the next link");
        assert_eq!(trust.current(), &third.verifying_key());
        assert_eq!(trust.retired().len(), 2);
    }

    #[test]
    fn a_retired_key_vouches_only_for_earlier_certificates() {
        let first = registry_key(1);
        let second = registry_key(2);
        let alice = player(1);
        let mut trust = RegistryTrust::new(
            first.verifying_key(),
            Some(commit_to(&second.verifying_key())),
        );
        trust
            .accept_rotation(&rotation(&first, &second, None, NOW))
            .expect("rotate");

        // Issued before the rotation: still good until it expires.
        let earlier = certificate(&first, &alice, "alice", "Alice", NOW - 100, 1);
        assert!(
            verify_certificate(&bytes(&earlier), &alice.uuid_as_root(), NOW, &trust, None).is_ok()
        );

        // Issued after it: the retired key no longer vouches.
        let later = certificate(&first, &alice, "alice", "Alice", NOW + 100, 2);
        assert_eq!(
            verify_certificate(&bytes(&later), &alice.uuid_as_root(), NOW, &trust, None),
            Err(RegistryError::UntrustedRegistryKey)
        );

        // The successor's certificates verify.
        let successor = certificate(&second, &alice, "alice", "Alice", NOW + 100, 3);
        assert!(
            verify_certificate(
                &bytes(&successor),
                &alice.uuid_as_root(),
                NOW + 100,
                &trust,
                None
            )
            .is_ok()
        );
    }

    /// A claim for alice, mined at `bits` and signed.
    fn claim(identity: &Identity, bits: u32) -> ClaimRequest {
        let name = Name::from_parts("alice", "Alice").expect("a form-valid test name");
        let mut claim = ClaimRequest::new(identity, &name, NOW);
        claim.solve_work(bits).expect("a nonce at this difficulty");
        claim.sign(identity);
        claim
    }

    #[test]
    fn a_claim_is_signed_verified_and_mined() {
        let alice = player(1);
        let claim = claim(&alice, 10);
        assert!(claim.work_bits().expect("a claim carries work") >= 10);

        let wire = postcard::to_allocvec(&claim).expect("encode");
        let parsed = ClaimRequest::from_bytes(&wire).expect("parse");
        let verified = parsed.verify_claim(NOW, 10).expect("verify");
        assert_eq!(verified.name.display(), "Alice");
        assert_eq!(verified.uuid, alice.uuid_as_root());
        assert_eq!(verified.public_key, alice.public_key());
        assert_eq!(verified.timestamp, NOW);

        // A renewal is the same request without the work.
        let mut renewal = claim;
        renewal.work = None;
        renewal.sign(&alice);
        assert_eq!(renewal.work_bits(), None);
        assert!(renewal.verify_renewal(NOW).is_ok());
        assert_eq!(
            renewal.verify_claim(NOW, 10),
            Err(RegistryError::MissingWork)
        );
    }

    #[test]
    fn a_stale_claim_and_insufficient_work_are_refused() {
        let alice = player(1);
        let mut stale = claim(&alice, 8);
        stale.timestamp = NOW - CLOCK_SKEW_SECONDS - 1;
        stale.sign(&alice);
        assert_eq!(stale.verify_claim(NOW, 8), Err(RegistryError::StaleRequest));

        // One bit more than the nonce actually has: deterministic, not a
        // flake. A search for a nonce *below* a fixed difficulty can fail by
        // chance; asking for one bit above what this exact nonce has cannot.
        let mined = claim(&alice, 8);
        let found = mined.work_bits().expect("work");
        assert!(matches!(
            mined.verify_claim(NOW, found + 1),
            Err(RegistryError::InsufficientWork { found: f, required: r }) if f == found && r == found + 1
        ));
    }

    #[test]
    fn a_reserved_name_cannot_be_claimed() {
        let alice = player(1);
        // Form-valid on purpose: reservation is what must refuse it.
        let name = Name::from_parts("admin", "Admin").expect("a form-valid name");
        let mut claim = ClaimRequest::new(&alice, &name, NOW);
        claim.solve_work(8).expect("mine");
        claim.sign(&alice);
        assert_eq!(
            claim.verify_claim(NOW, 8),
            Err(RegistryError::NameReserved {
                name: "admin".to_owned()
            })
        );
    }

    #[test]
    fn a_release_is_signed_and_verified() {
        let alice = player(1);
        let mut release = ReleaseRequest::new(&alice, "alice", NOW);
        release.sign(&alice);
        let wire = postcard::to_allocvec(&release).expect("encode");
        let parsed = ReleaseRequest::from_bytes(&wire).expect("parse");
        let verified = parsed.verify(NOW).expect("verify");
        assert_eq!(verified.name, "alice");
        assert_eq!(verified.uuid, alice.uuid_as_root());

        let mut stale = parsed;
        stale.timestamp = NOW + CLOCK_SKEW_SECONDS + 1;
        stale.sign(&alice);
        assert_eq!(stale.verify(NOW), Err(RegistryError::StaleRequest));
    }

    #[test]
    fn an_email_request_binds_removes_and_refuses_a_bad_address() {
        let alice = player(1);
        let mut request = EmailRequest {
            version: EMAIL_VERSION,
            uuid: *alice.uuid_as_root().as_bytes(),
            public_key: [0; 32],
            address: Some("alice@example.com".to_owned()),
            timestamp: NOW,
            signature: Signature64([0; 64]),
        };
        request.sign(&alice);
        let wire = postcard::to_allocvec(&request).expect("encode");
        let parsed = EmailRequest::from_bytes(&wire).expect("parse");
        let verified = parsed.verify(NOW).expect("verify");
        assert_eq!(verified.address.as_deref(), Some("alice@example.com"));
        assert_eq!(verified.uuid, alice.uuid_as_root());

        // Removal is the same request with no address.
        let mut removal = EmailRequest {
            address: None,
            ..parsed
        };
        removal.sign(&alice);
        assert_eq!(removal.verify(NOW).expect("verify").address, None);

        // A bad address never reaches a mailer.
        let mut bad = EmailRequest {
            version: EMAIL_VERSION,
            uuid: *alice.uuid_as_root().as_bytes(),
            public_key: [0; 32],
            address: Some("not an address".to_owned()),
            timestamp: NOW,
            signature: Signature64([0; 64]),
        };
        bad.sign(&alice);
        assert!(matches!(
            EmailRequest::from_bytes(&postcard::to_allocvec(&bad).expect("encode")),
            Err(RegistryError::EmailAddress { .. })
        ));
        assert!(matches!(
            bad.verify(NOW),
            Err(RegistryError::EmailAddress { .. })
        ));

        // A valid address on a stale request is still stale.
        let mut stale = EmailRequest {
            timestamp: NOW - CLOCK_SKEW_SECONDS - 1,
            ..request
        };
        stale.sign(&alice);
        assert_eq!(stale.verify(NOW), Err(RegistryError::StaleRequest));
    }

    #[test]
    fn a_recovery_and_its_cancellation_verify() {
        let new_device = player(2);
        let holder = player(1);
        let mut recovery = RecoveryRequest {
            version: RECOVERY_VERSION,
            name: "alice".to_owned(),
            new_uuid: *new_device.uuid_as_root().as_bytes(),
            public_key: [0; 32],
            timestamp: NOW,
            signature: Signature64([0; 64]),
        };
        recovery.sign(&new_device);
        let parsed =
            RecoveryRequest::from_bytes(&postcard::to_allocvec(&recovery).expect("encode"))
                .expect("parse");
        let verified = parsed.verify(NOW).expect("verify");
        assert_eq!(verified.name, "alice");
        assert_eq!(verified.new_uuid, new_device.uuid_as_root());
        assert_eq!(verified.public_key, new_device.public_key());

        // A forged signature is refused.
        let mut forged = parsed.clone();
        forged.new_uuid = *holder.uuid_as_root().as_bytes();
        assert_eq!(forged.verify(NOW), Err(RegistryError::SignatureInvalid));

        let mut cancel = CancelRecoveryRequest {
            version: CANCEL_VERSION,
            name: "alice".to_owned(),
            uuid: *holder.uuid_as_root().as_bytes(),
            public_key: [0; 32],
            timestamp: NOW,
            signature: Signature64([0; 64]),
        };
        cancel.sign(&holder);
        let parsed =
            CancelRecoveryRequest::from_bytes(&postcard::to_allocvec(&cancel).expect("encode"))
                .expect("parse");
        let verified = parsed.verify(NOW).expect("verify");
        assert_eq!(verified.name, "alice");
        assert_eq!(verified.uuid, holder.uuid_as_root());
    }
}
