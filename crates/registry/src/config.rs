// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The service's configuration, read from the environment.
//!
//! Environment variables rather than a TOML file because that is how Dokploy
//! passes a secret to a container, and the registry's key **is** a secret. The
//! names are the design's (§7): `REGISTRY_DATA`, `REGISTRY_KEY`,
//! `REGISTRY_NEXT_KEY_HASH`, `REGISTRY_SMTP_URL`, `REGISTRY_MAIL_FROM`,
//! `REGISTRY_BASE_URL`. Two more are ours: `REGISTRY_BIND` (the port inside the
//! container) and `REGISTRY_TRUST_PROXY` (whether to believe
//! `X-Forwarded-For`).

use std::net::SocketAddr;
use std::path::PathBuf;

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
}

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

        let bind = match lookup("REGISTRY_BIND").filter(|value| !value.is_empty()) {
            None => "0.0.0.0:8080".parse().map_err(|_| ConfigError::Invalid {
                name: "REGISTRY_BIND",
                why: "the built-in default is not an address".to_owned(),
            })?,
            Some(value) => value.parse().map_err(|_| ConfigError::Invalid {
                name: "REGISTRY_BIND",
                why: format!("`{value}` is not an address like 0.0.0.0:8080"),
            })?,
        };

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

        Ok(Self {
            data_dir: Some(data_dir),
            bind,
            key_seed,
            next_key_hash,
            base_url,
            mail_from,
            smtp_url,
            trust_forwarded_for,
        })
    }
}

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
