// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Key rotation at the registry, over HTTP (review A3).
//!
//! An identity's root commits to a successor; the successor proves itself by
//! carrying the rotation in its key-set proof; and from the moment the service
//! has seen that rotation, the old root — and anything it authorised, and any
//! proof that does not know about the rotation — is refused everywhere: claim,
//! renewal, release, address bind, recovery start and cancellation. These
//! tests drive the real service on a loopback socket, as a client would.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use common::{config, request, start_with, start_with_mail, test_dir};
use tiamat_core::identity::keyset::{
    add_key_payload, commit_to, root_commitment_payload, rotate_key_payload,
};
use tiamat_core::identity::registry::{
    CancelRecoveryRequest, ClaimRequest, EmailRequest, KEY_SET_PROOF_VERSION, KeyAuthorisation,
    KeySetProof, LinkKind, Name, RecoveryRequest, ReleaseRequest, Signature64,
};
use tiamat_core::identity::{Identity, PlayerUuid};
use tiamat_registry::Clock;
use tiamat_registry::mail::SinkMailer;

const NOW: i64 = 1_800_000_000;

#[derive(Debug)]
struct TestClock(AtomicI64);

impl TestClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::Relaxed);
    }
}

impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

fn player(seed: u8) -> Identity {
    Identity::from_seed(&[seed; 32])
}

/// The root declares `successor` as the key it will rotate to.
fn declare(root: &Identity, successor: &Identity) -> KeyAuthorisation {
    let uuid = root.uuid_as_root();
    let commitment = commit_to(&successor.public_key());
    KeyAuthorisation {
        kind: LinkKind::RootCommitment,
        key: root.public_key().to_bytes(),
        next_key_hash: Some(commitment),
        added_by: root.public_key().to_bytes(),
        signature: Signature64(
            root.sign(&root_commitment_payload(&uuid, &commitment))
                .to_bytes(),
        ),
    }
}

/// `from` rotates the identity of `root` to `to`, which commits to `next`.
fn rotate(
    root: &Identity,
    from: &Identity,
    to: &Identity,
    next: Option<&Identity>,
) -> KeyAuthorisation {
    let uuid = root.uuid_as_root();
    let next = next.map(|next| commit_to(&next.public_key()));
    KeyAuthorisation {
        kind: LinkKind::Rotation,
        key: to.public_key().to_bytes(),
        next_key_hash: next,
        added_by: from.public_key().to_bytes(),
        signature: Signature64(
            from.sign(&rotate_key_payload(&uuid, &to.public_key(), next.as_ref()))
                .to_bytes(),
        ),
    }
}

/// `from` adds `key` to the identity of `root`.
fn add(root: &Identity, from: &Identity, key: &Identity) -> KeyAuthorisation {
    let uuid = root.uuid_as_root();
    KeyAuthorisation {
        kind: LinkKind::AddKey,
        key: key.public_key().to_bytes(),
        next_key_hash: None,
        added_by: from.public_key().to_bytes(),
        signature: Signature64(
            from.sign(&add_key_payload(&uuid, &key.public_key(), None))
                .to_bytes(),
        ),
    }
}

fn proof(root: &Identity, chain: Vec<KeyAuthorisation>) -> KeySetProof {
    KeySetProof {
        version: KEY_SET_PROOF_VERSION,
        root: root.public_key().to_bytes(),
        chain,
    }
}

/// Who signs: a key of `root`'s identity, with the proof that reaches it (none
/// for the root by hash).
struct Signer<'a> {
    uuid: PlayerUuid,
    key: &'a Identity,
    proof: Option<KeySetProof>,
}

impl<'a> Signer<'a> {
    fn root(root: &'a Identity) -> Self {
        Self {
            uuid: root.uuid_as_root(),
            key: root,
            proof: None,
        }
    }

    fn with(root: &Identity, key: &'a Identity, chain: Vec<KeyAuthorisation>) -> Self {
        Self {
            uuid: root.uuid_as_root(),
            key,
            proof: Some(proof(root, chain)),
        }
    }

    fn claim(&self, canonical: &str, at: i64, mine: bool) -> Vec<u8> {
        let name = Name::from_parts(canonical, canonical).expect("name");
        let mut claim = match &self.proof {
            Some(proof) => ClaimRequest::for_device(self.key, &self.uuid, proof.clone(), &name, at),
            None => ClaimRequest::new(self.key, &name, at),
        };
        if mine {
            claim.solve_work(4).expect("work");
        }
        claim.sign(self.key);
        postcard::to_allocvec(&claim).expect("encode")
    }

    fn release(&self, canonical: &str, at: i64) -> Vec<u8> {
        let mut release = match &self.proof {
            Some(proof) => {
                ReleaseRequest::for_device(self.key, &self.uuid, proof.clone(), canonical, at)
            }
            None => ReleaseRequest::new(self.key, canonical, at),
        };
        release.sign(self.key);
        postcard::to_allocvec(&release).expect("encode")
    }

    fn email(&self, address: &str, at: i64) -> Vec<u8> {
        let mut email = match &self.proof {
            Some(proof) => {
                EmailRequest::for_device(self.key, &self.uuid, proof.clone(), Some(address), at)
            }
            None => EmailRequest::new(self.key, Some(address), at),
        };
        email.solve_work(4).expect("work");
        email.sign(self.key);
        postcard::to_allocvec(&email).expect("encode")
    }

    fn recover(&self, canonical: &str, at: i64) -> Vec<u8> {
        let mut recovery = match &self.proof {
            Some(proof) => {
                RecoveryRequest::for_device(self.key, &self.uuid, proof.clone(), canonical, at)
            }
            None => RecoveryRequest::new(self.key, canonical, at),
        };
        recovery.sign(self.key);
        postcard::to_allocvec(&recovery).expect("encode")
    }

    fn cancel(&self, canonical: &str, at: i64) -> Vec<u8> {
        let mut cancel = match &self.proof {
            Some(proof) => CancelRecoveryRequest::for_device(
                self.key,
                &self.uuid,
                proof.clone(),
                canonical,
                at,
            ),
            None => CancelRecoveryRequest::new(self.key, canonical, at),
        };
        cancel.sign(self.key);
        postcard::to_allocvec(&cancel).expect("encode")
    }
}

/// The token from the one link in the mail.
fn token_of(mailer: &SinkMailer) -> String {
    let mails = mailer.take();
    assert_eq!(mails.len(), 1, "expected exactly one mail");
    mails[0]
        .body
        .split("t=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("a token")
        .to_owned()
}

/// Posts a body; the answer's status, and its body as text where it is
/// text (a certificate is not, and is not read here).
fn post(addr: std::net::SocketAddr, path: &str, body: &[u8]) -> (u16, String) {
    let (status, body) = request(addr, "POST", path, body);
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// Asserts a `403 retired_key`.
fn assert_retired(answer: (u16, String), what: &str) {
    assert_eq!(answer.0, 403, "{what}: {}", answer.1);
    assert!(
        answer.1.contains("\"error\":\"retired_key\""),
        "{what}: {}",
        answer.1
    );
}

#[test]
fn a_rotated_away_root_can_do_nothing_and_its_successor_everything() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, mailer) = start_with_mail(config(None), clock.clone());
    let addr = handle.local_addr();
    let root = player(1);
    let successor = player(2);
    let old = Signer::root(&root);
    let new = Signer::with(
        &root,
        &successor,
        vec![
            declare(&root, &successor),
            rotate(&root, &root, &successor, None),
        ],
    );

    // Before the rotation the root is the identity: a name and an address.
    assert_eq!(
        post(addr, "/v1/claim", &old.claim("alice", NOW, true)).0,
        200
    );
    assert_eq!(
        post(addr, "/v1/email", &old.email("alice@example.com", NOW)).0,
        202
    );
    let token = token_of(&mailer);
    assert_eq!(
        post(addr, "/v1/email/verify", format!("t={token}").as_bytes()).0,
        200
    );
    // Another holder, for the recovery the successor starts below.
    let bob = player(9);
    let bob_signer = Signer::root(&bob);
    assert_eq!(
        post(addr, "/v1/claim", &bob_signer.claim("bobby", NOW, true)).0,
        200
    );
    assert_eq!(
        post(addr, "/v1/email", &bob_signer.email("bob@example.com", NOW)).0,
        202
    );
    let token = token_of(&mailer);
    assert_eq!(
        post(addr, "/v1/email/verify", format!("t={token}").as_bytes()).0,
        200
    );

    // The successor renews with the rotation: the registry learns of it.
    let (status, body) = post(addr, "/v1/claim", &new.claim("alice", NOW, false));
    assert_eq!(status, 200, "{body}");

    // A stranger's recovery of the name is running, so a cancel has
    // something to cancel.
    let mallory = player(3);
    assert_eq!(
        post(
            addr,
            "/v1/recover",
            &Signer::root(&mallory).recover("alice", NOW)
        )
        .0,
        202
    );
    let token = token_of(&mailer);
    assert_eq!(
        post(addr, "/v1/recover/confirm", format!("t={token}").as_bytes()).0,
        200
    );

    // The old root, by hash, is refused at every door.
    assert_retired(
        post(addr, "/v1/claim", &old.claim("another", NOW, true)),
        "claim",
    );
    assert_retired(
        post(addr, "/v1/claim", &old.claim("alice", NOW, false)),
        "renew",
    );
    assert_retired(
        post(addr, "/v1/release", &old.release("alice", NOW)),
        "release",
    );
    assert_retired(
        post(addr, "/v1/email", &old.email("thief@example.com", NOW)),
        "email",
    );
    assert_retired(
        post(addr, "/v1/recover", &old.recover("bobby", NOW)),
        "recover",
    );
    assert_retired(
        post(addr, "/v1/recover/cancel", &old.cancel("alice", NOW)),
        "cancel",
    );
    assert_eq!(mailer.count(), 0, "a retired key made the registry mail");

    // The successor does all of it.
    let (status, body) = post(addr, "/v1/recover/cancel", &new.cancel("alice", NOW));
    assert_eq!(status, 200, "cancel: {body}");
    let (status, body) = post(addr, "/v1/email", &new.email("alice@example.org", NOW));
    assert_eq!(status, 202, "email: {body}");
    let _ = mailer.take();
    let (status, body) = post(addr, "/v1/release", &new.release("alice", NOW));
    assert_eq!(status, 200, "release: {body}");
    // The next day, past this address's two claims of today.
    let tomorrow = NOW + 24 * 60 * 60;
    clock.set(tomorrow);
    let (status, body) = post(addr, "/v1/claim", &new.claim("alice", tomorrow, true));
    assert_eq!(status, 200, "claim: {body}");
    let (status, body) = post(addr, "/v1/recover", &new.recover("bobby", tomorrow));
    assert_eq!(status, 202, "recover: {body}");
    assert_eq!(mailer.take().len(), 1);

    // Later, the same: the memory does not fade while the identity holds a
    // name.
    clock.set(NOW + 60 * 24 * 60 * 60);
    let later = NOW + 60 * 24 * 60 * 60;
    assert_retired(
        post(addr, "/v1/claim", &old.claim("alice", later, false)),
        "renew later",
    );
    assert_eq!(
        post(addr, "/v1/claim", &new.claim("alice", later, false)).0,
        200
    );
    assert!(handle.stop());
}

#[test]
fn a_device_the_old_root_added_works_again_only_once_the_successor_adds_it() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let addr = handle.local_addr();
    let root = player(10);
    let successor = player(11);
    let device = player(12);

    // The device, added by the root, claims the identity's name.
    let by_root = Signer::with(&root, &device, vec![add(&root, &root, &device)]);
    assert_eq!(
        post(addr, "/v1/claim", &by_root.claim("dave", NOW, true)).0,
        200
    );

    // The root rotates.
    let new = Signer::with(
        &root,
        &successor,
        vec![
            declare(&root, &successor),
            rotate(&root, &root, &successor, None),
        ],
    );
    assert_eq!(
        post(addr, "/v1/claim", &new.claim("dave", NOW, false)).0,
        200
    );

    // The device's old proof runs through the retired root's authorisation.
    assert_retired(
        post(addr, "/v1/claim", &by_root.claim("dave", NOW, false)),
        "the device on the old root's word",
    );
    // A proof that includes the rotation but leans on the old authorisation is
    // not a chain at all: after the rotation only the successor can add.
    let mut leaning = proof(
        &root,
        vec![
            declare(&root, &successor),
            rotate(&root, &root, &successor, None),
        ],
    );
    leaning.chain.push(add(&root, &root, &device));
    let mut claim = ClaimRequest::for_device(
        &device,
        &root.uuid_as_root(),
        leaning,
        &Name::from_parts("dave", "dave").expect("name"),
        NOW,
    );
    claim.sign(&device);
    let (status, body) = post(
        addr,
        "/v1/claim",
        &postcard::to_allocvec(&claim).expect("encode"),
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("\"error\":\"proof\""), "{body}");

    // Re-authorised by the successor, through the rotation, it works.
    let by_successor = Signer::with(
        &root,
        &device,
        vec![
            declare(&root, &successor),
            rotate(&root, &root, &successor, None),
            add(&root, &successor, &device),
        ],
    );
    let (status, body) = post(addr, "/v1/claim", &by_successor.claim("dave", NOW, false));
    assert_eq!(status, 200, "{body}");
    assert!(handle.stop());
}

#[test]
fn an_older_proof_is_refused_once_a_later_rotation_is_recorded() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let addr = handle.local_addr();
    let root = player(20);
    let second = player(21);
    let third = player(22);
    let to_second = vec![
        declare(&root, &second),
        rotate(&root, &root, &second, Some(&third)),
    ];
    let as_second = Signer::with(&root, &second, to_second.clone());
    let mut to_third = to_second;
    to_third.push(rotate(&root, &second, &third, None));
    let as_third = Signer::with(&root, &third, to_third);

    assert_eq!(
        post(addr, "/v1/claim", &as_second.claim("carol", NOW, true)).0,
        200
    );
    // The second key renews happily until the third proves itself...
    assert_eq!(
        post(addr, "/v1/claim", &as_second.claim("carol", NOW, false)).0,
        200
    );
    assert_eq!(
        post(addr, "/v1/claim", &as_third.claim("carol", NOW, false)).0,
        200
    );
    // ...and then its proof, which knows nothing of the second rotation, is a
    // replay of the past: refused, as the root by hash is.
    assert_retired(
        post(addr, "/v1/claim", &as_second.claim("carol", NOW, false)),
        "the older proof",
    );
    assert_retired(
        post(
            addr,
            "/v1/claim",
            &Signer::root(&root).claim("carol", NOW, false),
        ),
        "the root",
    );
    assert_eq!(
        post(addr, "/v1/claim", &as_third.claim("carol", NOW, false)).0,
        200
    );
    assert!(handle.stop());
}

#[test]
fn a_thief_with_the_root_cannot_rotate_it_once_its_commitment_is_known() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let addr = handle.local_addr();
    let root = player(30);
    let successor = player(31);
    let mallory = player(32);

    // The client declares its root's commitment with its claim, signed by the
    // root alone: the registry keeps the first one it sees.
    let declared = Signer::with(&root, &root, vec![declare(&root, &successor)]);
    assert_eq!(
        post(addr, "/v1/claim", &declared.claim("erin", NOW, true)).0,
        200
    );

    // The root is stolen. The thief rotates it to a key of their own, with a
    // declaration of their own; the root's signatures are genuine.
    let thief = Signer::with(
        &root,
        &mallory,
        vec![
            declare(&root, &mallory),
            rotate(&root, &root, &mallory, None),
        ],
    );
    let (status, body) = post(addr, "/v1/claim", &thief.claim("erin", NOW, false));
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("\"error\":\"commitment\""), "{body}");

    // The owner rotates to the successor committed before the theft, and the
    // thief's root is shut out.
    let owner = Signer::with(
        &root,
        &successor,
        vec![
            declare(&root, &successor),
            rotate(&root, &root, &successor, None),
        ],
    );
    assert_eq!(
        post(addr, "/v1/claim", &owner.claim("erin", NOW, false)).0,
        200
    );
    assert_retired(
        post(
            addr,
            "/v1/release",
            &Signer::root(&root).release("erin", NOW),
        ),
        "the stolen root",
    );
    assert!(handle.stop());
}

#[test]
fn the_memory_of_a_rotation_survives_a_restart() {
    let dir = test_dir("rotation-restart");
    let clock = Arc::new(TestClock::new(NOW));
    let root = player(40);
    let successor = player(41);
    let new = Signer::with(
        &root,
        &successor,
        vec![
            declare(&root, &successor),
            rotate(&root, &root, &successor, None),
        ],
    );
    {
        let handle = start_with(config(Some(dir.clone())), clock.clone());
        let addr = handle.local_addr();
        assert_eq!(
            post(addr, "/v1/claim", &new.claim("frank", NOW, true)).0,
            200
        );
        assert!(handle.stop());
    }
    let handle = start_with(config(Some(dir)), clock);
    let addr = handle.local_addr();
    assert_retired(
        post(
            addr,
            "/v1/claim",
            &Signer::root(&root).claim("frank", NOW, false),
        ),
        "the root after a restart",
    );
    assert_eq!(
        post(addr, "/v1/claim", &new.claim("frank", NOW, false)).0,
        200
    );
    assert!(handle.stop());
}
