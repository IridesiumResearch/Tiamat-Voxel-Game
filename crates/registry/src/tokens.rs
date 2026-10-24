// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! One-shot links: email verification and recovery confirmation.
//!
//! What is stored is the **hash** of the token, never the token. A database
//! dump therefore cannot be replayed against the registry, which is the same
//! reason a password file holds hashes. The token itself exists only in the
//! mail and in the client that follows the link.

use rusqlite::{Connection, OptionalExtension, params};
use tiamat_core::identity::PlayerUuid;

use crate::hex;

/// Bytes of randomness in a link.
pub const TOKEN_BYTES: usize = 32;

/// How long a link works.
pub const TOKEN_LIFETIME_SECONDS: i64 = 24 * 60 * 60;

/// What a link is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Bind a recovery address.
    Email,
    /// Start a recovery.
    Recovery,
}

impl Purpose {
    /// The column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Recovery => "recovery",
        }
    }
}

/// What a token row holds after it is taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    /// What the link was for.
    pub purpose: Purpose,
    /// The identity that asked for it.
    pub uuid: [u8; 32],
    /// The name a recovery is about, if any.
    pub canonical: Option<String>,
    /// The address being verified, if any.
    pub address: Option<String>,
}

/// What went wrong making or taking a link.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// A statement failed.
    #[error("the registry database reported: {0}")]
    Db(#[from] rusqlite::Error),

    /// The system randomness source failed.
    #[error("could not obtain secure randomness for a link")]
    Entropy,
}

/// Makes a link token, stores its hash, and returns the token text and hash.
///
/// The text goes in the mail; the hash is for the row (and for deleting the row
/// if the mail could not be sent).
///
/// # Errors
///
/// [`TokenError`] when entropy or the database fails.
pub fn issue(
    connection: &Connection,
    purpose: Purpose,
    uuid: &PlayerUuid,
    canonical: Option<&str>,
    address: Option<&str>,
    now: i64,
) -> Result<(String, [u8; 32]), TokenError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| TokenError::Entropy)?;
    let hash = hash_of(&bytes);
    connection.execute(
        "INSERT INTO tokens (hash, purpose, uuid, canonical, address, expires, created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            hash.as_slice(),
            purpose.as_str(),
            uuid.as_bytes().as_slice(),
            canonical,
            address,
            now + TOKEN_LIFETIME_SECONDS,
            now
        ],
    )?;
    Ok((hex::encode(&bytes), hash))
}

/// Takes a token: reads it, deletes it, and returns what it was for.
///
/// One-shot by construction — the row is gone whether or not the caller uses
/// the answer — so a link cannot be followed twice.
///
/// # Errors
///
/// [`TokenError`] when the database fails.
pub fn take(
    connection: &Connection,
    token_hex: &str,
    purpose: Purpose,
    now: i64,
) -> Result<Option<TokenRecord>, TokenError> {
    let Some(bytes) = hex::decode::<TOKEN_BYTES>(token_hex) else {
        return Ok(None);
    };
    let hash = hash_of(&bytes);
    let row = connection
        .query_row(
            "SELECT uuid, canonical, address FROM tokens
             WHERE hash = ?1 AND purpose = ?2 AND expires > ?3",
            params![hash.as_slice(), purpose.as_str(), now],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((uuid, canonical, address)) = row else {
        return Ok(None);
    };
    connection.execute("DELETE FROM tokens WHERE hash = ?1", [hash.as_slice()])?;
    let Ok(uuid) = <[u8; TOKEN_BYTES]>::try_from(uuid.as_slice()) else {
        // The row was written by this service and is always 32 bytes; a
        // malformed one is a corrupt database, and refusing is honest.
        return Ok(None);
    };
    Ok(Some(TokenRecord {
        purpose,
        uuid,
        canonical,
        address,
    }))
}

/// Deletes a token that was issued but whose mail could not be sent.
///
/// # Errors
///
/// [`TokenError`] when the database fails.
pub fn discard(connection: &Connection, hash: &[u8; TOKEN_BYTES]) -> Result<(), TokenError> {
    connection.execute("DELETE FROM tokens WHERE hash = ?1", [hash.as_slice()])?;
    Ok(())
}

/// The hash a token is stored under.
fn hash_of(bytes: &[u8; TOKEN_BYTES]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    // Domain-separated like every other payload in this project, so a hash of a
    // token can never be mistaken for a hash of something else with the same
    // bytes.
    hasher.update(b"tiamat:registry-token:v1");
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    const NOW: i64 = 1_800_000_000;

    fn uuid() -> PlayerUuid {
        PlayerUuid::from_bytes([7; 32])
    }

    #[test]
    fn a_token_is_taken_once_and_only_for_its_purpose() {
        let db = Db::in_memory().expect("db");
        let (text, _hash) = db
            .with(|connection| {
                Ok(issue(
                    connection,
                    Purpose::Email,
                    &uuid(),
                    None,
                    Some("alice@example.com"),
                    NOW,
                )
                .expect("issue"))
            })
            .expect("issue");
        assert_eq!(text.len(), TOKEN_BYTES * 2);

        let taken = db
            .with(|connection| Ok(take(connection, &text, Purpose::Email, NOW).expect("take")))
            .expect("take")
            .expect("a fresh token");
        assert_eq!(taken.address.as_deref(), Some("alice@example.com"));
        assert_eq!(taken.uuid, *uuid().as_bytes());

        // One-shot: the second read finds nothing.
        let again = db
            .with(|connection| Ok(take(connection, &text, Purpose::Email, NOW).expect("take")))
            .expect("take");
        assert!(again.is_none());
    }

    #[test]
    fn an_expired_or_wrong_purpose_token_is_refused() {
        let db = Db::in_memory().expect("db");
        let (email, _) = db
            .with(|connection| {
                Ok(issue(
                    connection,
                    Purpose::Email,
                    &uuid(),
                    None,
                    Some("a@example.com"),
                    NOW,
                )
                .expect("issue"))
            })
            .expect("issue");
        // Wrong purpose.
        let wrong = db
            .with(|connection| Ok(take(connection, &email, Purpose::Recovery, NOW).expect("take")))
            .expect("take");
        assert!(wrong.is_none());
        // Expired.
        let (expired, _) = db
            .with(|connection| {
                Ok(issue(
                    connection,
                    Purpose::Recovery,
                    &uuid(),
                    Some("alice"),
                    None,
                    NOW,
                )
                .expect("issue"))
            })
            .expect("issue");
        let late = db
            .with(|connection| {
                Ok(take(
                    connection,
                    &expired,
                    Purpose::Recovery,
                    NOW + TOKEN_LIFETIME_SECONDS + 1,
                )
                .expect("take"))
            })
            .expect("take");
        assert!(late.is_none());
    }
}
