// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The two mails the registry sends, behind a trait.
//!
//! Mail is the only part of this service that can be slow, fail, or be
//! unavailable, and none of those may take a request down with them: a claim
//! that cannot mail is still a claim. So the transport is a trait with a
//! [`SinkMailer`] for tests — the bot tests (N6) follow the links out of the
//! sink rather than reading a mailbox — and an [`Unavailable`] transport for a
//! deployment whose SMTP has not been configured yet.
//!
//! The SMTP implementation arrives with the email flow; this module deliberately
//! does not depend on it, so an operator with no mail configured still gets a
//! registry that issues certificates.

use std::sync::Mutex;

/// One plain-text message. The service never sends HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    /// The recipient.
    pub to: String,
    /// The subject line.
    pub subject: String,
    /// The body, which contains one link and nothing else.
    pub body: String,
}

/// Somewhere a mail can be handed off.
pub trait Mailer: Send + Sync {
    /// Sends one message.
    ///
    /// # Errors
    ///
    /// [`MailError`] when the transport is unconfigured or the send failed. A
    /// caller decides whether that is fatal; the flows in this service treat it
    /// as "tell the player to try again", never as a rollback.
    fn send(&self, mail: &Mail) -> Result<(), MailError>;
}

/// What went wrong sending a mail.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MailError {
    /// No SMTP transport is configured.
    #[error("mail is not configured on this registry")]
    NotConfigured,

    /// The transport reported a failure.
    #[error("the mail could not be sent: {0}")]
    Transport(String),
}

/// A transport that keeps messages in memory, for tests.
#[derive(Debug, Default)]
pub struct SinkMailer {
    sent: Mutex<Vec<Mail>>,
}

impl SinkMailer {
    /// Takes everything sent so far, oldest first.
    ///
    /// Draining rather than peeking keeps a test honest about ordering and
    /// stops a long-running sink from growing.
    #[must_use]
    pub fn take(&self) -> Vec<Mail> {
        // A poisoned sink still has its messages; recovering them is better
        // than losing the one piece of evidence a failing test needs.
        let mut guard = match self.sent.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *guard)
    }

    /// How many messages have been sent and not taken.
    #[must_use]
    pub fn count(&self) -> usize {
        match self.sent.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }
}

impl Mailer for SinkMailer {
    fn send(&self, mail: &Mail) -> Result<(), MailError> {
        let mut guard = match self.sent.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.push(mail.clone());
        Ok(())
    }
}

/// A transport that is not configured, and says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unavailable;

impl Mailer for Unavailable {
    fn send(&self, _mail: &Mail) -> Result<(), MailError> {
        Err(MailError::NotConfigured)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(to: &str) -> Mail {
        Mail {
            to: to.to_owned(),
            subject: "hello".to_owned(),
            body: "one link".to_owned(),
        }
    }

    #[test]
    fn the_sink_keeps_and_drains_messages() {
        let sink = SinkMailer::default();
        sink.send(&mail("a@example.com")).expect("send");
        sink.send(&mail("b@example.com")).expect("send");
        assert_eq!(sink.count(), 2);
        let taken = sink.take();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].to, "a@example.com");
        assert_eq!(taken[1].to, "b@example.com");
        assert_eq!(sink.count(), 0);
    }

    #[test]
    fn the_unconfigured_transport_refuses_with_a_reason() {
        let error = Unavailable
            .send(&mail("a@example.com"))
            .expect_err("refused");
        assert_eq!(error, MailError::NotConfigured);
    }
}
