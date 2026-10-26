// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The limits and the retention sweep, driven at volume through months.
//!
//! Review B9's decision was "secure, double-checked and stress-tested": the
//! retention §8 promises must hold for a registry that has run for a year
//! under a crowd of identities, not only for the hand-built rows of the unit
//! tests. So these tests drive the real handlers (`api::*`, the same functions
//! the HTTP workers call) with a moving clock and a seeded pseudo-random mix of
//! claims, renewals, releases, address binds, link follows, recovery starts,
//! confirmations and cancellations, from many identities, addresses and
//! networks — and check, as they go:
//!
//! - **every limit held**: per address, per network and per identity for
//!   claims; per identity, per (folded) address, per network and globally for
//!   mail — counted from what the service *accepted*, independently of its own
//!   counters;
//! - **every row is inside its retention** (`retention`'s table): nothing kept
//!   past its promise by more than the sweep's own hour of slack;
//! - **nothing personal in the wrong place**: no raw address in a quota key,
//!   no IP in an audit row's free text.
//!
//! The renewal-only and recovery-only loops then check the other half of
//! "bounded": run the same loop at two request rates over the same months and
//! the tables end the same size, because nothing a refused or repeated request
//! does is kept.
//!
//! Each scenario has a quick variant that always runs (a few seconds) and a
//! full one behind `#[ignore]`: `cargo test -p registry stress -- --ignored`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tiamat_core::identity::Identity;
use tiamat_core::identity::registry::{
    CancelRecoveryRequest, ClaimRequest, EMAIL_VERSION, EmailRequest, Name, RECOVERY_VERSION,
    RecoveryRequest, ReleaseRequest, Signature64,
};

use crate::clock::Clock;
use crate::config::Config;
use crate::deadline::Deadline;
use crate::limits::{self, DAY, HOUR, SWEEP_INTERVAL, THIRTY_DAYS};
use crate::mail::SinkMailer;
use crate::mailquota;
use crate::names::{
    CERTIFICATE_LIVE_SECONDS, EXPIRY_AFTER_RENEWAL, EXPIRY_NEVER_RENEWED, HOLD_AFTER_RELEASE,
};
use crate::retention::{
    IP_RETENTION, NAMELESS_ADDRESS_GRACE, RECOVERY_RETENTION, RELEASED_RETENTION,
};
use crate::service::Service;
use crate::tokens::TOKEN_LIFETIME_SECONDS;

const START: i64 = 1_800_000_000;

/// How late a sweep may be: it runs at most hourly, from the next request.
const SLACK: i64 = 2 * SWEEP_INTERVAL;

/// The mail ceiling the mixed scenario runs under: low enough that the crowd
/// meets it.
const CROWDED_MAIL_PER_HOUR: i64 = 2;

/// The mail ceiling the loops run under: above the one address's allowance,
/// so the address's cap is the one that binds.
const LOOP_MAIL_PER_HOUR: i64 = 20;

/// A clock the scenario moves.
#[derive(Debug)]
struct Moving(AtomicI64);

impl Clock for Moving {
    fn now(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// A small deterministic generator: the mix must be the same on every run and
/// every machine, so a failure is reproducible from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// A service, its clock and its mail sink.
struct Rig {
    service: Service,
    clock: Arc<Moving>,
    mailer: Arc<SinkMailer>,
    /// The identities that held a name row at the last retention check.
    ///
    /// An address goes with its identity's last name, and a name row can stop
    /// being an identity's without being deleted: another player claims it
    /// once its hold is over, and the row becomes theirs. The address is then
    /// due at the next hourly sweep — not at `verified_at` plus the grace — so
    /// the check excuses an identity that still had a name at the previous
    /// check and catches it at the next one if the sweep missed it.
    named_before: RefCell<BTreeSet<Vec<u8>>>,
}

impl Rig {
    fn new(mail_per_hour: i64) -> Self {
        let clock = Arc::new(Moving(AtomicI64::new(START)));
        let mailer = Arc::new(SinkMailer::default());
        let config = Config {
            data_dir: None,
            bind: "127.0.0.1:0".parse().expect("bind"),
            key_seed: [7; 32],
            next_key_hash: None,
            base_url: "https://registry.example".to_owned(),
            mail_from: None,
            smtp_url: None,
            trust_forwarded_for: false,
            // Two bits at rest; a busy network pays four or eight more.
            work_bits: 2,
            reserved: vec!["core_testkit".to_owned()],
            workers: 1,
            mail_per_hour,
            request_timeout: crate::http::REQUEST_TIMEOUT,
        };
        let service =
            Service::open(config, mailer.clone(), clock.clone()).expect("start a service");
        Self {
            service,
            clock,
            mailer,
            named_before: RefCell::new(BTreeSet::new()),
        }
    }

    fn now(&self) -> i64 {
        self.clock.now()
    }

    fn set(&self, now: i64) {
        self.clock.0.store(now, Ordering::Relaxed);
    }

    fn count(&self, sql: &str) -> i64 {
        self.service
            .db()
            .with(|connection| connection.query_row(sql, [], |row| row.get(0)))
            .expect("count")
    }

    /// A claim, re-mined at the difficulty the service asks for if the first
    /// attempt is told the network is busy. Returns the status.
    fn claim(&self, who: &Identity, canonical: &str, display: &str, ip: IpAddr) -> u16 {
        let name = Name::from_parts(canonical, display).expect("a form-valid name");
        let mut bits = self.service.config().work_bits;
        loop {
            let mut request = ClaimRequest::new(who, &name, self.now());
            request.solve_work(bits).expect("a nonce");
            request.sign(who);
            let body = postcard::to_allocvec(&request).expect("encode");
            let reply = crate::api::claim(&self.service, &body, Some(ip), &Deadline::none());
            if let Some(required) = required_bits(&reply)
                && required > bits
            {
                bits = required;
                continue;
            }
            return reply.status();
        }
    }

    fn renew(&self, who: &Identity, canonical: &str, display: &str) -> u16 {
        let name = Name::from_parts(canonical, display).expect("a form-valid name");
        let mut request = ClaimRequest::new(who, &name, self.now());
        request.sign(who);
        let body = postcard::to_allocvec(&request).expect("encode");
        crate::api::claim(&self.service, &body, Some(local()), &Deadline::none()).status()
    }

    fn release(&self, who: &Identity, canonical: &str) -> u16 {
        let mut request = ReleaseRequest::new(who, canonical, self.now());
        request.sign(who);
        let body = postcard::to_allocvec(&request).expect("encode");
        crate::api::release(&self.service, &body, &Deadline::none()).status()
    }

    /// An address bind, with the proof of work an unnamed identity needs
    /// (re-mined at the asked difficulty, as a client would).
    fn email(&self, who: &Identity, address: &str, ip: IpAddr) -> u16 {
        let mut request = EmailRequest {
            version: EMAIL_VERSION,
            uuid: *who.uuid_as_root().as_bytes(),
            public_key: [0; 32],
            address: Some(address.to_owned()),
            timestamp: self.now(),
            signature: Signature64([0; 64]),
        };
        request.sign(who);
        let body = postcard::to_allocvec(&request).expect("encode");
        let mut bits = self.service.config().work_bits;
        loop {
            let nonce = crate::work::solve_email_work(&request, bits).expect("a nonce");
            let reply = crate::api::email(
                &self.service,
                &body,
                Some(ip),
                Some(nonce),
                &Deadline::none(),
            );
            if let Some(required) = required_bits(&reply)
                && required > bits
            {
                bits = required;
                continue;
            }
            return reply.status();
        }
    }

    fn recover(&self, who: &Identity, canonical: &str, ip: IpAddr) -> u16 {
        let mut request = RecoveryRequest {
            version: RECOVERY_VERSION,
            name: canonical.to_owned(),
            new_uuid: *who.uuid_as_root().as_bytes(),
            public_key: [0; 32],
            timestamp: self.now(),
            signature: Signature64([0; 64]),
        };
        request.sign(who);
        let body = postcard::to_allocvec(&request).expect("encode");
        crate::api::recover_start(&self.service, &body, Some(ip), &Deadline::none()).status()
    }

    fn cancel(&self, who: &Identity, canonical: &str) -> u16 {
        let mut request = CancelRecoveryRequest::new(who, canonical, self.now());
        request.sign(who);
        let body = postcard::to_allocvec(&request).expect("encode");
        crate::api::recover_cancel(&self.service, &body, &Deadline::none()).status()
    }

    /// Follows (presses the button of) every link mailed since the last call,
    /// each with probability `percent`.
    fn follow_links(&self, rng: &mut Rng, percent: u64) {
        for mail in self.mailer.take() {
            if !rng.chance(percent) {
                continue;
            }
            let Some(token) = mail
                .body
                .split("t=")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
            else {
                continue;
            };
            let form = format!("t={token}");
            if mail.body.contains("/v1/email/verify") {
                crate::api::email_verify(&self.service, "", form.as_bytes(), &Deadline::none());
            } else {
                crate::api::recover_confirm(&self.service, "", form.as_bytes(), &Deadline::none());
            }
        }
    }

    /// The name an identity holds, from the database.
    fn held_by(&self, who: &Identity) -> Option<String> {
        self.service
            .db()
            .with(|connection| {
                Ok(
                    crate::names::active_binding(connection, &who.uuid_as_root())?
                        .map(|binding| binding.canonical),
                )
            })
            .expect("binding")
    }

    /// The verified address behind a held name, from the database.
    fn address_behind(&self, canonical: &str) -> Option<String> {
        self.service
            .db()
            .with(|connection| {
                use rusqlite::OptionalExtension as _;
                connection
                    .query_row(
                        "SELECT e.address FROM emails e JOIN names n ON n.uuid = e.uuid
                         WHERE n.canonical = ?1 AND n.released IS NULL",
                        [canonical],
                        |row| row.get(0),
                    )
                    .optional()
            })
            .expect("address")
    }

    /// Every table's size, by name.
    fn sizes(&self) -> BTreeMap<&'static str, i64> {
        [
            "audit",
            "emails",
            "issued",
            "limits",
            "names",
            "recoveries",
            "reserved",
            "revocations",
            "revoked_holders",
            "tokens",
        ]
        .into_iter()
        .map(|table| (table, self.count(&format!("SELECT COUNT(*) FROM {table}"))))
        .collect()
    }

    /// Asserts that every row is inside the retention §8 promises (with the
    /// sweep's slack), and that nothing personal sits where it should not.
    fn assert_retention(&self) {
        let now = self.now();
        let late = now - SLACK;
        let checks: [(&str, String); 8] = [
            (
                "a link past its expiry",
                format!("SELECT COUNT(*) FROM tokens WHERE expires < {late}"),
            ),
            (
                "an audit address older than seven days",
                format!(
                    "SELECT COUNT(*) FROM audit WHERE ip IS NOT NULL AND when_at < {}",
                    late - IP_RETENTION
                ),
            ),
            (
                "a name row past its hold and thirty days",
                format!(
                    "SELECT COUNT(*) FROM names WHERE CASE
                         WHEN released IS NOT NULL THEN hold_until
                         WHEN renewals > 0 THEN renewed + {EXPIRY_AFTER_RENEWAL} + {HOLD_AFTER_RELEASE}
                         ELSE claimed + {EXPIRY_NEVER_RENEWED} + {HOLD_AFTER_RELEASE}
                     END + {RELEASED_RETENTION} < {late}"
                ),
            ),
            (
                "a recovery record ninety days after its end",
                format!(
                    "SELECT COUNT(*) FROM recoveries WHERE state != 'pending'
                       AND COALESCE(ended_at, notice_until) + {RECOVERY_RETENTION} < {late}"
                ),
            ),
            (
                "a serial that can no longer verify",
                format!(
                    "SELECT COUNT(*) FROM issued WHERE issued < {}",
                    late - CERTIFICATE_LIVE_SECONDS
                ),
            ),
            (
                "a revocation whose certificate is gone",
                "SELECT COUNT(*) FROM revocations r WHERE NOT EXISTS (
                     SELECT 1 FROM issued i
                     WHERE i.serial = r.serial AND i.registry_key = r.registry_key)"
                    .to_owned(),
            ),
            (
                "a limit row a week past its window",
                format!(
                    "SELECT COUNT(*) FROM limits WHERE window + CASE
                         WHEN scope LIKE 'ip:%' OR scope LIKE 'net:%' OR scope LIKE 'mailid:%'
                             THEN {DAY}
                         WHEN scope LIKE 'mail:%' OR scope LIKE 'mailnet:%' OR scope LIKE 'mailall:%'
                             THEN {HOUR}
                         ELSE {THIRTY_DAYS}
                     END + {} < {late}",
                    limits::LIMIT_RETENTION
                ),
            ),
            (
                "a raw address in a quota key",
                "SELECT COUNT(*) FROM limits WHERE instr(scope, '@') > 0".to_owned(),
            ),
        ];
        for (what, sql) in checks {
            assert_eq!(self.count(&sql), 0, "{what} at t+{}s", now - START);
        }

        let (orphaned, named) = self
            .service
            .db()
            .with(|connection| {
                let mut statement = connection.prepare(&format!(
                    "SELECT uuid FROM emails WHERE verified_at + {NAMELESS_ADDRESS_GRACE} < {late}
                       AND NOT EXISTS (SELECT 1 FROM names n WHERE n.uuid = emails.uuid)"
                ))?;
                let orphaned: Vec<Vec<u8>> = statement
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let mut statement = connection.prepare("SELECT DISTINCT uuid FROM names")?;
                let named: BTreeSet<Vec<u8>> = statement
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok((orphaned, named))
            })
            .expect("addresses");
        let missed: Vec<&Vec<u8>> = orphaned
            .iter()
            .filter(|uuid| !self.named_before.borrow().contains(*uuid))
            .collect();
        assert!(
            missed.is_empty(),
            "{} address(es) outlived their identity's last name by a whole check, at t+{}s",
            missed.len(),
            now - START
        );
        *self.named_before.borrow_mut() = named;
        let whys: Vec<String> = self
            .service
            .db()
            .with(|connection| {
                let mut statement = connection.prepare("SELECT DISTINCT why FROM audit")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect()
            })
            .expect("whys");
        for why in whys {
            assert!(
                why.parse::<IpAddr>().is_err(),
                "an IP in an audit row's free text: {why}"
            );
        }
    }
}

/// The difficulty a `work` refusal asks for.
fn required_bits(reply: &crate::http::Reply) -> Option<u32> {
    let text = reply.text();
    let rest = text.split("\"required_bits\":").nth(1)?;
    rest.split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn local() -> IpAddr {
    "127.0.0.1".parse().expect("ip")
}

fn identity(index: u64) -> Identity {
    let mut seed = [0u8; 32];
    seed[..8].copy_from_slice(&index.to_le_bytes());
    seed[31] = 0xA5;
    Identity::from_seed(&seed)
}

/// What the scenario counts of what the service accepted, by window.
#[derive(Default)]
struct Ledger {
    ceiling: i64,
    claims_ip: BTreeMap<(String, i64), i64>,
    claims_net: BTreeMap<(String, i64), i64>,
    claims_identity: BTreeMap<(String, i64), i64>,
    mail_identity: BTreeMap<(String, i64), i64>,
    mail_address: BTreeMap<(String, i64), i64>,
    mail_net: BTreeMap<(String, i64), i64>,
    mail_global: BTreeMap<i64, i64>,
    accepted: BTreeMap<&'static str, u64>,
    refused: BTreeMap<&'static str, u64>,
}

impl Ledger {
    fn bump(map: &mut BTreeMap<(String, i64), i64>, key: String, window: i64) -> i64 {
        let entry = map.entry((key, window)).or_default();
        *entry += 1;
        *entry
    }

    fn outcome(&mut self, what: &'static str, accepted: bool) {
        let map = if accepted {
            &mut self.accepted
        } else {
            &mut self.refused
        };
        *map.entry(what).or_default() += 1;
    }

    /// A claim the service accepted: none of its quotas may now be over.
    fn claimed(&mut self, who: &Identity, ip: IpAddr, now: i64) {
        let day = limits::window_start(now, DAY);
        let thirty = limits::window_start(now, THIRTY_DAYS);
        let at_ip = Self::bump(&mut self.claims_ip, ip.to_string(), day);
        let at_net = Self::bump(&mut self.claims_net, limits::subnet_scope(ip), day);
        let mine = Self::bump(
            &mut self.claims_identity,
            who.uuid_as_root().to_hex(),
            thirty,
        );
        assert!(
            at_ip <= limits::CLAIMS_PER_IP,
            "{ip} claimed {at_ip} names in a day"
        );
        assert!(
            at_net <= limits::CLAIMS_PER_SUBNET,
            "{ip}'s network claimed {at_net} in a day"
        );
        assert!(
            mine <= limits::CLAIMS_PER_IDENTITY + limits::RENAMES_PER_IDENTITY,
            "an identity claimed {mine} names in thirty days"
        );
    }

    /// A mail the service accepted.
    fn mailed(&mut self, asker: &Identity, address: &str, ip: IpAddr, now: i64) {
        let day = limits::window_start(now, DAY);
        let hour = limits::window_start(now, HOUR);
        let mine = Self::bump(&mut self.mail_identity, asker.uuid_as_root().to_hex(), day);
        let there = Self::bump(
            &mut self.mail_address,
            mailquota::fold_address(address),
            hour,
        );
        let net = Self::bump(&mut self.mail_net, limits::subnet_scope(ip), hour);
        let all = self.mail_global.entry(hour).or_default();
        *all += 1;
        assert!(
            mine <= mailquota::MAILS_PER_IDENTITY,
            "an identity caused {mine} mails in a day"
        );
        assert!(
            there <= mailquota::MAILS_PER_ADDRESS,
            "{address} was mailed {there} times in an hour"
        );
        assert!(
            net <= mailquota::MAILS_PER_SUBNET,
            "{ip}'s network asked for {net} mails in an hour"
        );
        assert!(
            *all <= self.ceiling,
            "the registry sent {all} mails in an hour"
        );
    }
}

/// The scale of a mixed scenario.
struct Scale {
    days: i64,
    ops_per_hour: usize,
    identities: u64,
    networks: u8,
    hosts: u8,
}

/// The mixed scenario: many identities, addresses and networks, months.
fn mixed(scale: &Scale) -> BTreeMap<&'static str, i64> {
    let rig = Rig::new(CROWDED_MAIL_PER_HOUR);
    let mut rng = Rng(0x7EA5_E1A5_0000_0001);
    let mut ledger = Ledger {
        ceiling: CROWDED_MAIL_PER_HOUR,
        ..Ledger::default()
    };
    let people: Vec<Identity> = (0..scale.identities).map(identity).collect();
    // A pool of names twice the crowd, so claims collide and names change hands.
    let names: Vec<String> = (0..scale.identities * 2)
        .map(|n| format!("player{n}"))
        .collect();
    // Addresses with tags and casing a quota must fold together.
    let addresses: Vec<String> = (0..scale.identities / 2)
        .flat_map(|n| {
            [
                format!("user{n}@example.com"),
                format!("User{n}+tiamat@Example.com"),
            ]
        })
        .collect();
    let ip = |rng: &mut Rng| -> IpAddr {
        let network = rng.below(usize::from(scale.networks)) as u8;
        let host = rng.below(usize::from(scale.hosts)) as u8;
        if network % 5 == 4 {
            format!("2001:db8:{network:x}::{host:x}")
                .parse()
                .expect("v6")
        } else {
            format!("10.{network}.0.{host}").parse().expect("v4")
        }
    };
    let mut fresh = scale.identities;

    let hours = scale.days * 24;
    for hour in 0..hours {
        rig.set(START + hour * HOUR + (rng.below(3000) as i64));
        for _ in 0..scale.ops_per_hour {
            let who = &people[rng.below(people.len())];
            let from = ip(&mut rng);
            match rng.below(100) {
                0..=34 => {
                    let name = &names[rng.below(names.len())];
                    let accepted = rig.claim(who, name, name, from) == 200;
                    if accepted {
                        ledger.claimed(who, from, rig.now());
                    }
                    ledger.outcome("claim", accepted);
                }
                35..=59 => {
                    if let Some(name) = rig.held_by(who) {
                        // Mostly the same casing; sometimes a new one.
                        let display = if rng.chance(30) {
                            name.to_ascii_uppercase()
                        } else {
                            name.clone()
                        };
                        let accepted = rig.renew(who, &name, &display) == 200;
                        ledger.outcome("renew", accepted);
                    }
                }
                60..=69 => {
                    if let Some(name) = rig.held_by(who) {
                        let accepted = rig.release(who, &name) == 200;
                        ledger.outcome("release", accepted);
                    }
                }
                70..=81 => {
                    let address = &addresses[rng.below(addresses.len())];
                    let accepted = rig.email(who, address, from) == 202;
                    if accepted {
                        ledger.mailed(who, address, from, rig.now());
                    }
                    ledger.outcome("email", accepted);
                }
                82..=95 => {
                    // A stranger, often a fresh key, asks for somebody's name.
                    let asker = if rng.chance(70) {
                        fresh += 1;
                        identity(fresh)
                    } else {
                        identity(rng.below(people.len()) as u64)
                    };
                    let name = &names[rng.below(names.len())];
                    let behind = rig.address_behind(name);
                    let accepted = rig.recover(&asker, name, from) == 202;
                    if accepted {
                        let address = behind.expect("a recovery mail goes to a bound address");
                        ledger.mailed(&asker, &address, from, rig.now());
                    }
                    ledger.outcome("recover", accepted);
                }
                _ => {
                    if let Some(name) = rig.held_by(who) {
                        let accepted = rig.cancel(who, &name) == 200;
                        ledger.outcome("cancel", accepted);
                    }
                }
            }
        }
        // Most address links are followed; some recovery links are too.
        rig.follow_links(&mut rng, 60);
        if hour % 24 == 23 {
            rig.assert_retention();
        }
    }
    // A last look a sweep later, after everything has had its chance to age.
    rig.set(rig.now() + SWEEP_INTERVAL);
    rig.service.advance().expect("advance");
    rig.assert_retention();

    let sizes = rig.sizes();
    eprintln!(
        "stress mixed: {} days, accepted {:?}, refused {:?}, final rows {sizes:?}",
        scale.days, ledger.accepted, ledger.refused
    );
    // The scenario must have exercised what it claims to.
    for what in ["claim", "renew", "release", "email", "recover"] {
        assert!(
            ledger.accepted.get(what).copied().unwrap_or(0) > 0,
            "no {what} was accepted"
        );
    }
    for what in ["claim", "email", "recover"] {
        assert!(
            ledger.refused.get(what).copied().unwrap_or(0) > 0,
            "no {what} was refused"
        );
    }
    // Bounds that follow from §8 and the limits, whatever the months: links
    // live a day and the registry mails at most its ceiling an hour; an
    // address is kept on at most one audit row per request of the last seven
    // days.
    let links = CROWDED_MAIL_PER_HOUR * (TOKEN_LIFETIME_SECONDS + SLACK) / HOUR;
    assert!(sizes["tokens"] <= links, "{sizes:?}");
    let addressed = rig.count("SELECT COUNT(*) FROM audit WHERE ip IS NOT NULL");
    let recent_requests = (IP_RETENTION + SLACK) / HOUR * scale.ops_per_hour as i64;
    assert!(
        addressed <= recent_requests,
        "{addressed} audit rows keep an address"
    );
    sizes
}

#[test]
fn stress_mixed_quick() {
    mixed(&Scale {
        days: 75,
        ops_per_hour: 3,
        identities: 120,
        networks: 10,
        hosts: 6,
    });
}

#[test]
#[ignore = "about a minute; run with --ignored"]
fn stress_mixed_full() {
    mixed(&Scale {
        days: 400,
        ops_per_hour: 6,
        identities: 1_500,
        networks: 60,
        hosts: 12,
    });
}

/// One holder renewing every `every` seconds for `days`, alternating casing.
fn renewal_loop(days: i64, every: i64) -> BTreeMap<&'static str, i64> {
    let rig = Rig::new(LOOP_MAIL_PER_HOUR);
    let holder = identity(1);
    assert_eq!(rig.claim(&holder, "looper", "looper", local()), 200);
    let mut flip = false;
    let mut at = START;
    let end = START + days * DAY;
    while at < end {
        at += every;
        rig.set(at);
        flip = !flip;
        let display = if flip { "Looper" } else { "LOOPER" };
        // Refused once the month's casing allowance is spent; that is the point.
        let status = rig.renew(&holder, "looper", display);
        assert!(status == 200 || status == 429, "renewal answered {status}");
    }
    rig.set(end + SWEEP_INTERVAL);
    rig.service.advance().expect("advance");
    rig.assert_retention();
    rig.sizes()
}

fn assert_renewal_loop_bounded(days: i64) {
    let hourly = renewal_loop(days, HOUR);
    let thrice = renewal_loop(days, HOUR / 3);
    eprintln!("stress renewal loop over {days} days: {hourly:?}");
    assert_eq!(
        hourly, thrice,
        "three times the renewals left different tables: the loop is being stored"
    );
    // The bound itself: one name, the certificates that can still verify,
    // and an audit row per mint (about every sixty days) and per allowed casing
    // change (three a month), plus the claim.
    let months = days / 30 + 2;
    assert_eq!(hourly["names"], 1);
    assert!(hourly["issued"] <= 2, "{hourly:?}");
    assert_eq!(hourly["tokens"], 0);
    assert!(
        hourly["audit"] <= 1 + days / 60 + 2 + limits::DISPLAY_CHANGES_PER_IDENTITY * months,
        "{hourly:?}"
    );
    assert!(hourly["limits"] <= 8, "{hourly:?}");
}

#[test]
fn stress_renewal_only_loop_quick() {
    assert_renewal_loop_bounded(130);
}

#[test]
#[ignore = "tens of seconds; run with --ignored"]
fn stress_renewal_only_loop_full() {
    assert_renewal_loop_bounded(2 * 365);
}

/// Strangers asking to recover one held name every `every` seconds for `days`.
fn recovery_loop(days: i64, every: i64) -> (BTreeMap<&'static str, i64>, i64) {
    let rig = Rig::new(LOOP_MAIL_PER_HOUR);
    let holder = identity(1);
    assert_eq!(rig.claim(&holder, "target", "target", local()), 200);
    assert_eq!(rig.email(&holder, "holder@example.com", local()), 202);
    let mut rng = Rng(42);
    rig.follow_links(&mut rng, 100);
    assert_eq!(
        rig.address_behind("target").as_deref(),
        Some("holder@example.com")
    );
    let mut ledger = Ledger {
        ceiling: LOOP_MAIL_PER_HOUR,
        ..Ledger::default()
    };
    let mut accepted = 0;
    // From the next hour, so the bind's mail does not share the loop's first
    // hour of the address's allowance.
    let begin = START + HOUR;
    let end = begin + days * DAY;
    let mut at = begin;
    let mut stranger = 1_000_000;
    while at < end {
        rig.set(at);
        stranger += 1;
        let asker = identity(stranger);
        let from: IpAddr = format!("10.{}.{}.1", stranger % 200, (stranger / 200) % 200)
            .parse()
            .expect("ip");
        if rig.recover(&asker, "target", from) == 202 {
            ledger.mailed(&asker, "holder@example.com", from, rig.now());
            accepted += 1;
        }
        // Nobody follows the links: they expire, and must be swept.
        let _ = rig.mailer.take();
        // The holder renews once a day, as a playing client does, on a
        // schedule that does not depend on the loop's rate.
        if (at - begin) % DAY < every {
            let _ = rig.renew(&holder, "target", "target");
        }
        at += every;
    }
    rig.set(end + SWEEP_INTERVAL);
    rig.service.advance().expect("advance");
    rig.assert_retention();
    (rig.sizes(), accepted)
}

fn assert_recovery_loop_bounded(days: i64) {
    let (often, accepted_often) = recovery_loop(days, 5 * 60);
    let (rarely, accepted_rarely) = recovery_loop(days, 20 * 60);
    eprintln!("stress recovery loop over {days} days: {often:?}, {accepted_often} mails accepted");
    // Both rates meet the holder's address cap every hour: the same mails go
    // out, and the same rows are left, however hard the loop pushes.
    assert_eq!(accepted_often, accepted_rarely);
    assert_eq!(
        often, rarely,
        "four times the requests left different tables"
    );
    assert_eq!(
        accepted_often,
        days * 24 * mailquota::MAILS_PER_ADDRESS,
        "the holder's address is mailed exactly its hourly allowance"
    );
    // Links live a day; nothing else of a refused or an unfollowed request is
    // kept but its audit row, one per accepted mail (the claim, the bind and
    // its follow are the other three).
    assert!(
        often["tokens"] <= (TOKEN_LIFETIME_SECONDS + SLACK) / HOUR * mailquota::MAILS_PER_ADDRESS,
        "{often:?}"
    );
    let mints = days / 60 + 1;
    assert!(
        often["audit"] >= 3 + accepted_often && often["audit"] <= 3 + accepted_often + mints,
        "{often:?}: more audit rows than accepted mails and the holder's mints"
    );
    assert_eq!(often["recoveries"], 0);
    assert_eq!(often["names"], 1);
}

#[test]
fn stress_recovery_only_loop_quick() {
    assert_recovery_loop_bounded(12);
}

#[test]
#[ignore = "tens of seconds; run with --ignored"]
fn stress_recovery_only_loop_full() {
    assert_recovery_loop_bounded(120);
}

#[test]
fn the_ledger_s_checks_fire() {
    // The scenario's own assertions must be able to fail, or they prove
    // nothing: a fourth mail to one address in an hour trips them.
    let result = std::panic::catch_unwind(|| {
        let mut ledger = Ledger {
            ceiling: LOOP_MAIL_PER_HOUR,
            ..Ledger::default()
        };
        let ip = local();
        for n in 0..4 {
            let address = if n % 2 == 0 {
                "same+x@example.com"
            } else {
                "SAME@example.com"
            };
            ledger.mailed(&identity(n), address, ip, START);
        }
    });
    assert!(
        result.is_err(),
        "a fourth mail to one folded address went unnoticed"
    );
}
