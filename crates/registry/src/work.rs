// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The proof of work an email request carries when its identity holds no name.
//!
//! `/v1/email` mails an address of the asker's choosing, so a fresh key —
//! free to mint — must pay for the mail the way a fresh key pays for a name
//! (review B13, decided 2026-10-26). An identity that **holds a name** has paid
//! already and carries nothing.
//!
//! # The same shape as the claim's
//!
//! A claim's work is a nonce that makes `BLAKE3(tag ‖ signed payload)` start
//! with enough zero bits, at the difficulty the service asks for and raises
//! while the asker's network is busy. The email's is the same construction
//! over the email request's signed payload, under its own domain tag so a
//! nonce mined for one record is never valid for the other:
//!
//! ```text
//! BLAKE3("tiamat:email-work:v1" ‖ EmailRequest::signing_payload() ‖ nonce as u64 LE)
//! ```
//!
//! The payload names the identity, the key, the address and the timestamp, so
//! the work cannot be lifted onto another request, and it is stale with the
//! request after five minutes.
//!
//! # Why a header
//!
//! `EmailRequest` is a signed record in `core::identity::registry` whose wire
//! form is shared with the client, and it has no work field. The nonce travels
//! beside it, in the [`WORK_HEADER`] header, as a decimal `u64`; the body is
//! unchanged, so an identity that holds a name sends exactly what it sent
//! before. Moving the field into the record is a core change for a later
//! version (appended after `signature`, as `ClaimRequest::key_set` was).

use tiamat_core::identity::registry::{EmailRequest, MAX_WORK_BITS};

/// The request header that carries an email request's nonce.
pub const WORK_HEADER: &str = "Tiamat-Work";

/// The domain tag of the email request's proof of work.
const EMAIL_WORK_DOMAIN: &[u8] = b"tiamat:email-work:v1";

/// The leading zero bits `nonce` gives `request`.
#[must_use]
pub fn email_work_bits(request: &EmailRequest, nonce: u64) -> u32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EMAIL_WORK_DOMAIN);
    hasher.update(&request.signing_payload());
    hasher.update(&nonce.to_le_bytes());
    leading_zero_bits(hasher.finalize().as_bytes())
}

/// Mines a nonce that gives `request` at least `bits` leading zero bits.
///
/// `None` when `bits` is above [`MAX_WORK_BITS`], as the claim's miner. The
/// request's fields must be final first — the work hashes its signed payload —
/// but the signature itself is not hashed, so signing may come before or after.
#[must_use]
pub fn solve_email_work(request: &EmailRequest, bits: u32) -> Option<u64> {
    if bits > MAX_WORK_BITS {
        return None;
    }
    let mut prefix = blake3::Hasher::new();
    prefix.update(EMAIL_WORK_DOMAIN);
    prefix.update(&request.signing_payload());
    (0..=u64::MAX).find(|nonce| {
        let mut hasher = prefix.clone();
        hasher.update(&nonce.to_le_bytes());
        leading_zero_bits(hasher.finalize().as_bytes()) >= bits
    })
}

/// The number of leading zero bits in a hash.
fn leading_zero_bits(hash: &[u8; 32]) -> u32 {
    let mut bits = 0;
    for byte in hash {
        if *byte == 0 {
            bits += 8;
        } else {
            bits += byte.leading_zeros();
            break;
        }
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiamat_core::identity::Identity;
    use tiamat_core::identity::registry::{EMAIL_VERSION, Signature64};

    fn request(address: &str) -> EmailRequest {
        let identity = Identity::from_seed(&[3; 32]);
        let mut request = EmailRequest {
            version: EMAIL_VERSION,
            uuid: *identity.uuid_as_root().as_bytes(),
            public_key: [0; 32],
            address: Some(address.to_owned()),
            timestamp: 1_800_000_000,
            signature: Signature64([0; 64]),
        };
        request.sign(&identity);
        request
    }

    #[test]
    fn a_mined_nonce_meets_its_difficulty_for_its_own_request_only() {
        let alice = request("alice@example.com");
        let nonce = solve_email_work(&alice, 8).expect("a nonce at eight bits");
        assert!(email_work_bits(&alice, nonce) >= 8);
        // The work hashes the payload, so another request's sequence of
        // difficulties is a different one: a nonce is not transferable.
        let other = request("mallory@example.com");
        let sequence = |request: &EmailRequest| -> Vec<u32> {
            (0..32)
                .map(|nonce| email_work_bits(request, nonce))
                .collect()
        };
        assert_ne!(sequence(&alice), sequence(&other));
    }

    #[test]
    fn leading_zero_bits_counts_across_bytes() {
        let mut hash = [0xFF; 32];
        assert_eq!(leading_zero_bits(&hash), 0);
        hash[0] = 0;
        hash[1] = 0x10;
        assert_eq!(leading_zero_bits(&hash), 11);
        assert_eq!(leading_zero_bits(&[0; 32]), 256);
    }

    #[test]
    fn an_impossible_difficulty_is_refused_rather_than_mined() {
        assert_eq!(
            solve_email_work(&request("a@example.com"), MAX_WORK_BITS + 1),
            None
        );
    }
}
