// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The service's configuration, read from the environment.
//!
//! Environment variables rather than a TOML file because that is how Dokploy
//! passes a secret to a container, and the registry's key **is** a secret. The
//! names are the design's (§7): `REGISTRY_DATA`, `REGISTRY_KEY`,
//! `REGISTRY_NEXT_KEY_HASH`, `REGISTRY_SMTP_URL`, `REGISTRY_MAIL_FROM`,
//! `REGISTRY_BASE_URL`. The rest are ours: `REGISTRY_BIND` (the port inside the
//! container), `REGISTRY_TRUST_PROXY` (whether to believe `X-Forwarded-For`),
//! `REGISTRY_WORK_BITS`, `REGISTRY_RESERVED`, `REGISTRY_WORKERS` (how many
//! requests are served at once) and `REGISTRY_MAIL_PER_HOUR` (the registry's
//! hourly mail ceiling).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use tiamat_core::identity::registry::{DEFAULT_WORK_BITS, MAX_WORK_BITS};

use crate::hex;

/// Everything the service needs to start.
///
/// `data_dir` is `None` only in tests: the database is then in memory and
/// disappears with the process, which is exactly what a test wants and exactly
/// what a deployment must never do. [`Config::from_env`] has no default for it
/// for that reason — a registry that silently forgets every name on restart is
/// worse than one that refuses to start.
#[derive(Clone)]
pub struct Config {
    /// Where `registry.sqlite` lives. `None` = in memory, for tests.
    pub data_dir: Option<PathBuf>,
    /// The address to listen on inside the container.
    pub bind: SocketAddr,
    /// The 32-byte seed of the registry's current signing key.
    pub key_seed: [u8; 32],
    /// `BLAKE3` of the pre-committed successor key, if one has been committed.
    pub next_key_hash: Option<[u8; 32]>,
    /// The public base URL, without a trailing slash. Mail links are built
    /// from it.
    pub base_url: String,
    /// The `From:` address for the two mails this service sends.
    pub mail_from: Option<String>,
    /// `smtp://` or `smtps://user:pass@host:port` — **contains a password**.
    pub smtp_url: Option<String>,
    /// Whether `X-Forwarded-For` may be believed. On at Dokploy, behind
    /// Traefik; off everywhere else.
    pub trust_forwarded_for: bool,

    /// The proof-of-work difficulty a quiet network is asked for.
    ///
    /// [`DEFAULT_WORK_BITS`] in production; a lower number exists for tests,
    /// which would otherwise mine for a second per request. Above
    /// [`MAX_WORK_BITS`] a client could not solve it.
    pub work_bits: u32,

    /// Names the host wants nobody to claim, seeded into the reserved table at
    /// startup: the default mods' ids, mostly.
    ///
    /// **Here rather than in core, because they are content.** The engine's own
    /// reserved list is the administrative set; a mod's id belongs with the
    /// deployment that runs the mods, and an engine test exists to keep those
    /// names out of `crates/`. See [`MAX_RESERVED_NAMES`].
    pub reserved: Vec<String>,

    /// How many requests are served at once ([`DEFAULT_WORKERS`]).
    ///
    /// Each worker owns one request from its body to its answer, so a client
    /// that trickles a body holds one worker, not the service; when every
    /// worker is busy and as many requests again are waiting, the listener
    /// answers `503 busy` itself.
    pub workers: usize,

    /// The most mail the registry sends in an hour, across every flow that
    /// sends any ([`crate::DEFAULT_MAILS_PER_HOUR`]).
    pub mail_per_hour: i64,

    /// How long a request may take, from leaving the listener to its answer,
    /// before the client is told to retry. The design's two seconds
    /// ([`crate::REQUEST_TIMEOUT`]); not read from the environment, and a
    /// field only so the deadline's tests need not wait two seconds.
    pub request_timeout: Duration,
}

/// How many requests are served at once by default.
///
/// The work is a few milliseconds of SQLite behind one connection, so more
/// workers than this only queue on the database lock; eight is enough that a
/// handful of slow clients do not starve everyone else.
pub const DEFAULT_WORKERS: usize = 8;

/// The most workers the environment may ask for.
///
/// Each is a thread with a request in hand; a misconfigured variable should not
/// be able to ask for ten thousand.
pub const MAX_WORKERS: usize = 256;

impl Config {
    /// Reads the configuration from the process environment.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] naming the variable at fault. A secret's value is never
    /// echoed in the error.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// The testable form of [`Config::from_env`]: every variable comes from
    /// `lookup`.
    ///
    /// # Errors
    ///
    /// As [`Config::from_env`].
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let key_seed = read_key(&lookup, "REGISTRY_KEY")?.ok_or(ConfigError::Missing {
            name: "REGISTRY_KEY",
        })?;
        let next_key_hash = read_key(&lookup, "REGISTRY_NEXT_KEY_HASH")?;

        let data_dir = lookup("REGISTRY_DATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or(ConfigError::Missing {
                name: "REGISTRY_DATA",
            })?;

        let bind = Self::bind_from_lookup(&lookup)?;

        let base_url = lookup("REGISTRY_BASE_URL")
            .filter(|value| !value.is_empty())
            .ok_or(ConfigError::Missing {
                name: "REGISTRY_BASE_URL",
            })?;
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err(ConfigError::Invalid {
                name: "REGISTRY_BASE_URL",
                why: "it must start with https:// (or http:// on a test host)".to_owned(),
            });
        }
        let base_url = base_url.trim_end_matches('/').to_owned();

        let mail_from = lookup("REGISTRY_MAIL_FROM").filter(|value| !value.is_empty());
        let smtp_url = lookup("REGISTRY_SMTP_URL").filter(|value| !value.is_empty());
        if mail_from.is_some() != smtp_url.is_some() {
            return Err(ConfigError::Invalid {
                name: "REGISTRY_SMTP_URL",
                why: "REGISTRY_SMTP_URL and REGISTRY_MAIL_FROM are set together or not at all"
                    .to_owned(),
            });
        }

        let trust_forwarded_for = match lookup("REGISTRY_TRUST_PROXY")
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("0" | "false" | "no" | "off") => false,
            Some("1" | "true" | "yes" | "on") => true,
            Some(other) => {
                return Err(ConfigError::Invalid {
                    name: "REGISTRY_TRUST_PROXY",
                    why: format!("`{other}` is not a yes or a no"),
                });
            }
        };

        let work_bits = match lookup("REGISTRY_WORK_BITS").filter(|value| !value.is_empty()) {
            None => DEFAULT_WORK_BITS,
            Some(value) => {
                let bits: u32 = value.parse().map_err(|_| ConfigError::Invalid {
                    name: "REGISTRY_WORK_BITS",
                    why: format!("`{value}` is not a whole number"),
                })?;
                if bits > MAX_WORK_BITS {
                    return Err(ConfigError::Invalid {
                        name: "REGISTRY_WORK_BITS",
                        why: format!("nothing can be mined above {MAX_WORK_BITS} bits"),
                    });
                }
                bits
            }
        };

        let reserved = match lookup("REGISTRY_RESERVED").filter(|value| !value.is_empty()) {
            None => Vec::new(),
            Some(value) => {
                let names: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect();
                if names.len() > MAX_RESERVED_NAMES {
                    return Err(ConfigError::Invalid {
                        name: "REGISTRY_RESERVED",
                        why: format!("more than {MAX_RESERVED_NAMES} names"),
                    });
                }
                for name in &names {
                    tiamat_core::identity::registry::check_canonical(name).map_err(|error| {
                        ConfigError::Invalid {
                            name: "REGISTRY_RESERVED",
                            why: format!("`{name}` is not a canonical name: {error}"),
                        }
                    })?;
                }
                names
            }
        };

        let workers = read_count(&lookup, "REGISTRY_WORKERS", DEFAULT_WORKERS, MAX_WORKERS)?;
        let mail_per_hour = read_count(
            &lookup,
            "REGISTRY_MAIL_PER_HOUR",
            crate::mailquota::DEFAULT_MAILS_PER_HOUR as usize,
            MAX_MAIL_PER_HOUR,
        )?;

        Ok(Self {
            data_dir: Some(data_dir),
            bind,
            key_seed,
            next_key_hash,
            base_url,
            mail_from,
            smtp_url,
            trust_forwarded_for,
            work_bits,
            reserved,
            workers,
            // Bounded by `MAX_MAIL_PER_HOUR`, so the conversion cannot fail.
            mail_per_hour: i64::try_from(mail_per_hour).unwrap_or(i64::MAX),
            request_timeout: crate::http::REQUEST_TIMEOUT,
        })
    }

    /// The listening address alone, from `REGISTRY_BIND` (default
    /// `0.0.0.0:8080`).
    ///
    /// Separate from [`Config::from_lookup`] because the health check needs
    /// only this: a probe that demanded the signing key in its environment
    /// would be one more process holding a secret it never uses.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Invalid`] when the variable is not a socket address.
    pub fn bind_from_lookup(
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<SocketAddr, ConfigError> {
        match lookup("REGISTRY_BIND").filter(|value| !value.is_empty()) {
            None => Ok(SocketAddr::from(([0, 0, 0, 0], 8080))),
            Some(value) => value.parse().map_err(|_| ConfigError::Invalid {
                name: "REGISTRY_BIND",
                why: format!("`{value}` is not an address like 0.0.0.0:8080"),
            }),
        }
    }
}

/// The highest `REGISTRY_MAIL_PER_HOUR` accepted: far above anything a
/// transactional provider would let one sender do, and a bound on a typo.
pub const MAX_MAIL_PER_HOUR: usize = 10_000;

/// A whole number from the environment in `1..=max`, or `default` when unset.
fn read_count(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: usize,
    max: usize,
) -> Result<usize, ConfigError> {
    let Some(value) = lookup(name).filter(|value| !value.is_empty()) else {
        return Ok(default);
    };
    let count: usize = value.trim().parse().map_err(|_| ConfigError::Invalid {
        name,
        why: format!("`{value}` is not a whole number"),
    })?;
    if count == 0 || count > max {
        return Err(ConfigError::Invalid {
            name,
            why: format!("it must be between 1 and {max}"),
        });
    }
    Ok(count)
}

/// The most names a host may reserve through the environment.
///
/// The default mods are a dozen; sixty-four leaves room and bounds a
/// misconfigured variable.
pub const MAX_RESERVED_NAMES: usize = 64;

/// A key from the environment: `None` when unset, an error when it is not 64
/// hex characters.
fn read_key(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &'static str,
) -> Result<Option<[u8; 32]>, ConfigError> {
    let Some(value) = lookup(name).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    hex::decode::<32>(&value)
        .map(Some)
        .ok_or_else(|| ConfigError::Invalid {
            name,
            why: "expected 64 hex characters".to_owned(),
        })
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The seed and the SMTP URL carry secrets. A `{:?}` of a config lands
        // in a log line the first time someone debugs a startup failure, and a
        // secret in a log is a secret leaked.
        f.debug_struct("Config")
            .field("data_dir", &self.data_dir)
            .field("bind", &self.bind)
            .field("key_seed", &"<redacted>")
            .field("next_key_hash", &self.next_key_hash.map(|_| "<set>"))
            .field("base_url", &self.base_url)
            .field("mail_from", &self.mail_from)
            .field("smtp_url", &self.smtp_url.as_ref().map(|_| "<redacted>"))
            .field("trust_forwarded_for", &self.trust_forwarded_for)
            .field("work_bits", &self.work_bits)
            .field("reserved", &self.reserved.len())
            .field("workers", &self.workers)
            .field("mail_per_hour", &self.mail_per_hour)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

/// What went wrong reading the configuration.
///
/// The message names the variable and never its value, because one of them is
/// a signing seed and another is an SMTP password.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A required variable is absent.
    #[error("{name} is not set")]
    Missing {
        /// The environment variable's name.
        name: &'static str,
    },

    /// A variable is set but unusable.
    #[error("{name} is invalid: {why}")]
    Invalid {
        /// The environment variable's name.
        name: &'static str,
        /// What is wrong with it, in terms an operator can act on.
        why: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lookup over a fixed list of pairs.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    fn minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "REGISTRY_KEY",
                "0707070707070707070707070707070707070707070707070707070707070707",
            ),
            ("REGISTRY_DATA", "/data"),
            ("REGISTRY_BASE_URL", "https://registry.tiamatengine.com/"),
        ]
    }

    #[test]
    fn the_minimal_environment_is_enough() {
        let config = Config::from_lookup(env(&minimal())).expect("minimal config");
        assert_eq!(config.data_dir, Some(PathBuf::from("/data")));
        assert_eq!(config.bind, "0.0.0.0:8080".parse().expect("default bind"));
        assert_eq!(config.key_seed, [7u8; 32]);
        assert_eq!(config.next_key_hash, None);
        // The trailing slash is normalised away so mail links never double it.
        assert_eq!(config.base_url, "https://registry.tiamatengine.com");
        assert!(!config.trust_forwarded_for);
        assert!(config.smtp_url.is_none());
        assert_eq!(config.work_bits, DEFAULT_WORK_BITS);
        // Nothing is reserved unless the host says so: content names are the
        // deployment's, never the engine's.
        assert!(config.reserved.is_empty());
        assert_eq!(config.workers, DEFAULT_WORKERS);
        assert_eq!(
            config.mail_per_hour,
            crate::mailquota::DEFAULT_MAILS_PER_HOUR
        );
        assert_eq!(config.request_timeout, crate::http::REQUEST_TIMEOUT);
    }

    #[test]
    fn the_worker_count_and_the_mail_ceiling_are_whole_numbers_in_range() {
        let mut pairs = minimal();
        pairs.push(("REGISTRY_WORKERS", "3"));
        pairs.push(("REGISTRY_MAIL_PER_HOUR", "12"));
        let config = Config::from_lookup(env(&pairs)).expect("counts");
        assert_eq!(config.workers, 3);
        assert_eq!(config.mail_per_hour, 12);

        for (name, value) in [
            ("REGISTRY_WORKERS", "0"),
            ("REGISTRY_WORKERS", "100000"),
            ("REGISTRY_WORKERS", "eight"),
            ("REGISTRY_MAIL_PER_HOUR", "0"),
            ("REGISTRY_MAIL_PER_HOUR", "-5"),
        ] {
            let mut pairs = minimal();
            pairs.push((name, value));
            assert!(
                matches!(
                    Config::from_lookup(env(&pairs)),
                    Err(ConfigError::Invalid { name: found, .. }) if found == name
                ),
                "{name}={value}"
            );
        }
    }

    #[test]
    fn the_bind_is_readable_without_the_secrets() {
        // The health check reads only this; nothing else need be set.
        let bind = Config::bind_from_lookup(&env(&[])).expect("default");
        assert_eq!(bind, "0.0.0.0:8080".parse().expect("addr"));
        let bind =
            Config::bind_from_lookup(&env(&[("REGISTRY_BIND", "127.0.0.1:9")])).expect("set");
        assert_eq!(bind, "127.0.0.1:9".parse().expect("addr"));
        assert!(Config::bind_from_lookup(&env(&[("REGISTRY_BIND", "nope")])).is_err());
    }

    #[test]
    fn the_host_may_reserve_names() {
        let mut pairs = minimal();
        pairs.push(("REGISTRY_RESERVED", "alice, bobby"));
        let config = Config::from_lookup(env(&pairs)).expect("reserved");
        assert_eq!(
            config.reserved,
            vec!["alice".to_owned(), "bobby".to_owned()]
        );

        let mut pairs = minimal();
        pairs.push(("REGISTRY_RESERVED", "Alice"));
        assert!(matches!(
            Config::from_lookup(env(&pairs)),
            Err(ConfigError::Invalid {
                name: "REGISTRY_RESERVED",
                ..
            })
        ));
    }

    #[test]
    fn a_missing_variable_is_named() {
        let mut pairs = minimal();
        pairs.retain(|(name, _)| *name != "REGISTRY_DATA");
        assert_eq!(
            Config::from_lookup(env(&pairs)).expect_err("no data dir"),
            ConfigError::Missing {
                name: "REGISTRY_DATA"
            }
        );
    }

    #[test]
    fn a_bad_key_is_refused_without_being_echoed() {
        let mut pairs = minimal();
        pairs.retain(|(name, _)| *name != "REGISTRY_KEY");
        pairs.push(("REGISTRY_KEY", "not-a-secret-at-all"));
        let error = Config::from_lookup(env(&pairs)).expect_err("bad key");
        let text = error.to_string();
        assert_eq!(
            error,
            ConfigError::Invalid {
                name: "REGISTRY_KEY",
                why: "expected 64 hex characters".to_owned()
            }
        );
        assert!(!text.contains("not-a-secret"));
    }

    #[test]
    fn mail_is_all_or_nothing() {
        let mut pairs = minimal();
        pairs.push(("REGISTRY_MAIL_FROM", "accounts@tiamatengine.com"));
        assert!(matches!(
            Config::from_lookup(env(&pairs)),
            Err(ConfigError::Invalid {
                name: "REGISTRY_SMTP_URL",
                ..
            })
        ));

        pairs.push(("REGISTRY_SMTP_URL", "smtps://user:pass@smtp.example:465"));
        let config = Config::from_lookup(env(&pairs)).expect("both mail settings");
        assert_eq!(
            config.mail_from.as_deref(),
            Some("accounts@tiamatengine.com")
        );
        assert!(config.smtp_url.is_some());
    }

    #[test]
    fn the_proxy_flag_is_a_yes_or_a_no() {
        for (value, expected) in [
            ("1", true),
            ("true", true),
            ("YES", true),
            ("on", true),
            ("0", false),
            ("false", false),
            ("nonsense", false),
        ] {
            let mut pairs = minimal();
            pairs.push(("REGISTRY_TRUST_PROXY", value));
            let result = Config::from_lookup(env(&pairs));
            if value == "nonsense" {
                assert!(matches!(result, Err(ConfigError::Invalid { .. })));
            } else {
                assert_eq!(
                    result.expect("flag").trust_forwarded_for,
                    expected,
                    "{value}"
                );
            }
        }
    }

    #[test]
    fn the_work_difficulty_is_a_number_in_range() {
        let mut pairs = minimal();
        pairs.push(("REGISTRY_WORK_BITS", "8"));
        assert_eq!(Config::from_lookup(env(&pairs)).expect("bits").work_bits, 8);

        let mut pairs = minimal();
        pairs.push(("REGISTRY_WORK_BITS", "nonsense"));
        assert!(matches!(
            Config::from_lookup(env(&pairs)),
            Err(ConfigError::Invalid {
                name: "REGISTRY_WORK_BITS",
                ..
            })
        ));

        let mut pairs = minimal();
        pairs.push(("REGISTRY_WORK_BITS", "64"));
        assert!(matches!(
            Config::from_lookup(env(&pairs)),
            Err(ConfigError::Invalid {
                name: "REGISTRY_WORK_BITS",
                ..
            })
        ));
    }

    #[test]
    fn a_bad_bind_or_base_url_is_refused() {
        let mut pairs = minimal();
        pairs.push(("REGISTRY_BIND", "not an address"));
        assert!(matches!(
            Config::from_lookup(env(&pairs)),
            Err(ConfigError::Invalid {
                name: "REGISTRY_BIND",
                ..
            })
        ));

        let mut pairs = minimal();
        pairs.retain(|(name, _)| *name != "REGISTRY_BASE_URL");
        pairs.push(("REGISTRY_BASE_URL", "registry.tiamatengine.com"));
        assert!(matches!(
            Config::from_lookup(env(&pairs)),
            Err(ConfigError::Invalid {
                name: "REGISTRY_BASE_URL",
                ..
            })
        ));
    }

    #[test]
    fn debug_does_not_print_the_secrets() {
        let mut pairs = minimal();
        pairs.push(("REGISTRY_MAIL_FROM", "accounts@tiamatengine.com"));
        pairs.push(("REGISTRY_SMTP_URL", "smtps://user:hunter2@smtp.example:465"));
        let config = Config::from_lookup(env(&pairs)).expect("config");
        let text = format!("{config:?}");
        assert!(!text.contains("070707"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }
}
