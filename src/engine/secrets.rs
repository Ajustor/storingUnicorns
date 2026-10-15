//! Secrets (Entra ID refresh tokens) kept in the OS credential store:
//! Windows Credential Manager, macOS Keychain, Linux Secret Service.
//!
//! When that store can't be used (no Secret Service on a headless Linux…),
//! secrets live in memory for the session only.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// A key → secret store. Errors are short descriptions that never contain
/// the secret.
pub trait SecretStore: Send + Sync {
    /// The secret of `key`, `None` when there is none.
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    /// Create or replace the secret of `key`.
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    /// Remove the secret of `key` (nothing to do when there is none).
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// Keyring service under which every secret is stored.
const SERVICE: &str = "storingUnicorns";

/// Key written then deleted to check that the OS store works.
const PROBE_KEY: &str = "storingUnicorns:probe";

/// Shown once when the OS store isn't usable.
pub const UNAVAILABLE_WARNING: &str =
    "Trousseau du système indisponible : il faudra se reconnecter au prochain lancement.";

/// Largest part stored in one OS entry: Windows caps a credential at 2560
/// bytes and an Entra ID refresh token can come close, so longer secrets are
/// split over several entries.
const PART_BYTES: usize = 2048;

// ========== OS store ==========

/// One OS keyring entry per key, holding the secret's UTF-8 bytes (not
/// keyring's `set_password`, which stores UTF-16 on Windows and so halves
/// the room).
struct RawKeyring;

impl RawKeyring {
    fn entry(key: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, key).map_err(|e| e.to_string())
    }
}

impl SecretStore for RawKeyring {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        match Self::entry(key)?.get_secret() {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| "secret illisible (pas en UTF-8)".to_string()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        Self::entry(key)?
            .set_secret(value.as_bytes())
            .map_err(|e| e.to_string())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// The OS credential store (service `storingUnicorns`).
pub struct KeyringStore(Parts<RawKeyring>);

impl Default for KeyringStore {
    fn default() -> Self {
        Self(Parts::new(RawKeyring, PART_BYTES))
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        self.0.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.0.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.0.delete(key)
    }
}

/// Splits secrets longer than `max` bytes over several entries of `inner`:
/// `key` holds `<count>:<first part>`, `key#1`… the following parts.
struct Parts<S> {
    inner: S,
    max: usize,
}

impl<S: SecretStore> Parts<S> {
    fn new(inner: S, max: usize) -> Self {
        Self { inner, max }
    }

    fn part_key(key: &str, i: usize) -> String {
        format!("{key}#{i}")
    }

    /// `value` cut at char boundaries into parts of at most `max` bytes
    /// (one empty part for an empty value).
    fn split<'a>(&self, value: &'a str) -> Vec<&'a str> {
        let mut parts = Vec::new();
        let mut rest = value;
        loop {
            if rest.len() <= self.max {
                parts.push(rest);
                return parts;
            }
            let mut cut = self.max;
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            let (head, tail) = rest.split_at(cut);
            parts.push(head);
            rest = tail;
        }
    }

    /// Number of parts and first part of the stored head.
    fn parse_head(head: &str) -> Option<(usize, &str)> {
        let (count, first) = head.split_once(':')?;
        let count = count.parse::<usize>().ok().filter(|n| *n >= 1)?;
        Some((count, first))
    }
}

impl<S: SecretStore> SecretStore for Parts<S> {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let Some(head) = self.inner.get(key)? else {
            return Ok(None);
        };
        let Some((count, first)) = Self::parse_head(&head) else {
            return Err("secret enregistré illisible".into());
        };
        let mut value = first.to_string();
        for i in 1..count {
            match self.inner.get(&Self::part_key(key, i))? {
                Some(part) => value.push_str(&part),
                None => return Err("secret enregistré incomplet".into()),
            }
        }
        Ok(Some(value))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        // Drop the previous parts so that a shorter secret leaves none behind.
        self.delete(key)?;
        let parts = self.split(value);
        for (i, part) in parts.iter().enumerate().skip(1) {
            self.inner.set(&Self::part_key(key, i), part)?;
        }
        // The head last: readers never see a count of missing parts.
        self.inner
            .set(key, &format!("{}:{}", parts.len(), parts[0]))
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        let count = match self.inner.get(key) {
            Ok(Some(head)) => Self::parse_head(&head).map_or(1, |(n, _)| n),
            Ok(None) => return Ok(()),
            // Unreadable: delete the head anyway.
            Err(_) => 1,
        };
        self.inner.delete(key)?;
        for i in 1..count {
            self.inner.delete(&Self::part_key(key, i))?;
        }
        Ok(())
    }
}

// ========== Memory store ==========

/// Secrets for the session only (and for tests).
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl MemoryStore {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.map().get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.map().insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        self.map().remove(key);
        Ok(())
    }
}

// ========== Process-wide store ==========

struct Selected {
    store: Box<dyn SecretStore>,
    available: bool,
}

static STORE: OnceLock<Selected> = OnceLock::new();
static WARNED: AtomicBool = AtomicBool::new(false);

/// `os` when a probe entry can be written and deleted, else a memory store.
fn select(os: Box<dyn SecretStore>) -> Selected {
    let probe = os
        .set(PROBE_KEY, "probe")
        .and_then(|()| os.delete(PROBE_KEY));
    match probe {
        Ok(()) => Selected {
            store: os,
            available: true,
        },
        Err(e) => {
            tracing::warn!("OS credential store unusable, secrets kept for this session: {e}");
            Selected {
                store: Box::new(MemoryStore::default()),
                available: false,
            }
        }
    }
}

fn selected() -> &'static Selected {
    STORE.get_or_init(|| select(Box::<KeyringStore>::default()))
}

/// The process-wide store: the OS keyring, or memory when it isn't usable
/// (`available()` false). The first call probes the keyring, which may
/// block (Secret Service over D-Bus, an unlock prompt…): call it off the
/// async executor.
pub fn store() -> &'static dyn SecretStore {
    selected().store.as_ref()
}

/// Whether secrets outlive the session (the OS keyring works).
pub fn available() -> bool {
    selected().available
}

/// `UNAVAILABLE_WARNING` the first time it is asked for while the OS store
/// isn't usable, `None` otherwise.
pub fn take_warning() -> Option<&'static str> {
    warning_once(available(), &WARNED)
}

fn warning_once(available: bool, warned: &AtomicBool) -> Option<&'static str> {
    (!available && !warned.swap(true, Ordering::Relaxed)).then_some(UNAVAILABLE_WARNING)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store whose every operation fails, like a missing Secret Service.
    struct Broken;

    impl SecretStore for Broken {
        fn get(&self, _: &str) -> Result<Option<String>, String> {
            Err("no D-Bus".into())
        }
        fn set(&self, _: &str, _: &str) -> Result<(), String> {
            Err("no D-Bus".into())
        }
        fn delete(&self, _: &str) -> Result<(), String> {
            Err("no D-Bus".into())
        }
    }

    #[test]
    fn memory_store_creates_rotates_and_deletes() {
        let s = MemoryStore::default();
        assert_eq!(s.get("k").unwrap(), None);
        s.set("k", "refresh-1").unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some("refresh-1"));
        s.set("k", "refresh-2").unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some("refresh-2"));
        s.delete("k").unwrap();
        assert_eq!(s.get("k").unwrap(), None);
        // Deleting what isn't there is fine.
        s.delete("k").unwrap();
    }

    #[test]
    fn usable_store_is_selected_and_the_probe_removed() {
        let selected = select(Box::<MemoryStore>::default());
        assert!(selected.available);
        assert_eq!(selected.store.get(PROBE_KEY).unwrap(), None);
    }

    #[test]
    fn broken_store_falls_back_to_memory() {
        let selected = select(Box::new(Broken));
        assert!(!selected.available);
        selected.store.set("k", "v").unwrap();
        assert_eq!(selected.store.get("k").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn unavailable_warning_is_given_once() {
        let warned = AtomicBool::new(false);
        assert_eq!(warning_once(true, &warned), None);
        assert_eq!(warning_once(false, &warned), Some(UNAVAILABLE_WARNING));
        assert_eq!(warning_once(false, &warned), None);
    }

    fn parts(max: usize) -> Parts<MemoryStore> {
        Parts::new(MemoryStore::default(), max)
    }

    #[test]
    fn short_secret_is_one_entry() {
        let s = parts(8);
        s.set("k", "abc").unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some("abc"));
        assert_eq!(s.inner.map().len(), 1);
        s.set("e", "").unwrap();
        assert_eq!(s.get("e").unwrap().as_deref(), Some(""));
        assert_eq!(s.get("missing").unwrap(), None);
    }

    #[test]
    fn long_secret_is_split_and_reassembled() {
        let s = parts(4);
        s.set("k", "0123456789").unwrap();
        assert_eq!(s.inner.map().len(), 3);
        assert_eq!(s.inner.map()["k"], "3:0123");
        assert_eq!(s.inner.map()["k#2"], "89");
        assert_eq!(s.get("k").unwrap().as_deref(), Some("0123456789"));
    }

    #[test]
    fn parts_never_split_a_character() {
        let s = parts(4);
        let value = "aéééb"; // é is 2 bytes
        s.set("k", value).unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some(value));
        assert!(s.inner.map().values().all(|v| v.len() <= 2 + 4));
    }

    #[test]
    fn shorter_secret_leaves_no_old_part() {
        let s = parts(4);
        s.set("k", "0123456789").unwrap();
        s.set("k", "ab").unwrap();
        assert_eq!(s.inner.map().len(), 1);
        assert_eq!(s.get("k").unwrap().as_deref(), Some("ab"));
        s.delete("k").unwrap();
        assert!(s.inner.map().is_empty());
    }

    #[test]
    fn delete_removes_every_part() {
        let s = parts(4);
        s.set("k", "0123456789").unwrap();
        s.set("other", "x").unwrap();
        s.delete("k").unwrap();
        assert_eq!(s.get("k").unwrap(), None);
        assert_eq!(s.inner.map().len(), 1);
    }

    #[test]
    fn missing_or_garbled_parts_are_errors() {
        let s = parts(4);
        s.set("k", "0123456789").unwrap();
        s.inner.delete("k#1").unwrap();
        assert!(s.get("k").is_err());
        s.inner.set("g", "no count").unwrap();
        assert!(s.get("g").is_err());
        // A garbled head can still be deleted.
        s.delete("g").unwrap();
        assert_eq!(s.inner.get("g").unwrap(), None);
    }
}
