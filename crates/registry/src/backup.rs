// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Daily `VACUUM INTO` backups, and the fourteen-day rotation.
//!
//! # Why on a request, not a timer
//!
//! A backup thread would need a shutdown story (a join that waits for an hour of
//! sleep, or a wake channel) and a way to prove it does not race the one
//! database connection. The alternative is simpler and just as safe: the first
//! name-reading request after midnight makes the day's backup, and startup makes
//! one if the day's is missing. An idle service changes nothing, so its last
//! backup is still a complete picture of it.
//!
//! The write is `VACUUM INTO`, which SQLite guarantees is a consistent snapshot
//! even with the database in WAL mode and other statements in flight — it
//! copies a transaction, not a file.

use std::path::{Path, PathBuf};

use crate::db::{Db, DbError};

/// How many daily backups are kept.
pub const KEEP: usize = 14;

/// Writes today's backup if it does not exist yet, then prunes old ones.
///
/// Returns the path written, or `None` when today's was already there.
///
/// # Errors
///
/// [`DbError`] when the directory cannot be made, SQLite refuses the copy, or
/// pruning fails.
pub fn backup_once(db: &Db, data_dir: &Path, now: i64) -> Result<Option<PathBuf>, DbError> {
    let dir = data_dir.join("backups");
    std::fs::create_dir_all(&dir).map_err(|source| DbError::Backup {
        path: dir.display().to_string(),
        source,
    })?;
    let path = dir.join(format!("registry-{}.sqlite", date_of(now)));
    if path.exists() {
        return Ok(None);
    }
    let target = path.display().to_string();
    db.with(|connection| connection.execute("VACUUM INTO ?1", [target.as_str()]))?;
    prune(&dir)?;
    Ok(Some(path))
}

/// Keeps the newest [`KEEP`] backups and deletes the rest.
///
/// Names are the date, so lexical order is chronological.
///
/// # Errors
///
/// [`DbError::Backup`] when the directory cannot be read or a file cannot be
/// removed.
pub fn prune(dir: &Path) -> Result<usize, DbError> {
    let entries = std::fs::read_dir(dir).map_err(|source| DbError::Backup {
        path: dir.display().to_string(),
        source,
    })?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("registry-") && name.ends_with(".sqlite"))
        })
        .collect();
    files.sort();
    let mut removed = 0;
    while files.len() > KEEP {
        let oldest = files.remove(0);
        std::fs::remove_file(&oldest).map_err(|source| DbError::Backup {
            path: oldest.display().to_string(),
            source,
        })?;
        removed += 1;
    }
    Ok(removed)
}

/// The date `now` falls on, as `YYYY-MM-DD`, in UTC.
///
/// UTC rather than a local zone on purpose: two hosts in different zones must
/// produce the same file name for the same instant, or a redeploy across a
/// timezone makes a second backup with no new data in it.
#[must_use]
pub fn date_of(now: i64) -> String {
    let (year, month, day) = civil_from_days(now.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the Unix epoch to a civil date.
///
/// Howard Hinnant's `civil_from_days`, integer-only: no dependency, no float,
/// and exact for every date the service will see.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/registry-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    #[test]
    fn dates_are_the_ones_a_person_would_write() {
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(date_of(86_400 - 1), "1970-01-01");
        assert_eq!(date_of(86_400), "1970-01-02");
        // The instant the tests call NOW.
        assert_eq!(date_of(1_800_000_000), "2027-01-15");
        // A leap day, which is where a hand-rolled date goes wrong.
        assert_eq!(date_of(1_709_164_800), "2024-02-29");
    }

    #[test]
    fn a_backup_is_made_once_a_day_and_rotated() {
        let dir = test_dir("backups");
        let db = Db::in_memory().expect("db");
        let now = 1_800_000_000;

        let first = backup_once(&db, &dir, now)
            .expect("backup")
            .expect("a file");
        assert!(first.is_file());
        // The same day again is a no-op, not a second copy.
        assert!(backup_once(&db, &dir, now + 60).expect("backup").is_none());
        // The next day makes another.
        let second = backup_once(&db, &dir, now + 86_400)
            .expect("backup")
            .expect("a file");
        assert_ne!(first, second);

        // Fifteen more days of old files prune back to fourteen.
        for day in 0..20 {
            let path = dir
                .join("backups")
                .join(format!("registry-2020-01-{day:02}.sqlite"));
            std::fs::write(&path, b"old").expect("old backup");
        }
        prune(&dir.join("backups")).expect("prune");
        let files: Vec<_> = std::fs::read_dir(dir.join("backups"))
            .expect("list")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(files.len(), KEEP);
    }
}
