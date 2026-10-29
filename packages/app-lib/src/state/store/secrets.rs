//! The account secrets, kept in the system credential store instead of the rows
//! that name them.
//!
//! A row keeps a mark of which of its secret columns the store holds; the column
//! itself is cleared once the secret is there. Every secret the store will not
//! take — this host has none, or the value is longer than one entry holds — stays
//! in its column with its mark clear, so signing in never depends on the store
//! answering: the row is then read exactly as an older build wrote it.
//!
//! This module is the only way the crate reaches the store.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use super::Handle;

/// How much one entry holds, in the unit the platform's cap counts.
///
/// The Windows credential store caps a blob at 2560 bytes and keeps passwords as
/// UTF-16, so an entry holds at most 1280 code units. The Secret Service on Linux
/// answers "password is too long" above 8192 bytes and stores a shortened value
/// instead of refusing, so a secret it is given must stay well under that. macOS
/// has no cap worth naming; it is held to the Linux one, which is measured.
const MOST: usize = if cfg!(windows) { 1280 } else { 8192 };

/// What every key the launcher stores starts with.
const PREFIX: &str = "axolotl-account";

/// What the store answered about one key.
pub(crate) enum Stored {
    /// The secret the key names.
    Value(String),
    /// The store answered and holds nothing under the key.
    Missing,
    /// No store answered, so nothing was read.
    Unavailable,
}

thread_local! {
    /// Whether this thread already found no credential store to work with, which
    /// is as far as it needs to look: from then on the rows keep their own
    /// secrets.
    static NO_STORE: Cell<bool> = const { Cell::new(false) };
}

/// Whether the run has already said that this host has no credential store.
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Whether this thread has no credential store the launcher can use.
pub(crate) fn unavailable() -> bool {
    NO_STORE.with(Cell::get)
}

/// The key one row's secret is stored under.
pub(crate) fn key(parts: &[&str]) -> String {
    let mut key = String::from(PREFIX);
    for part in parts {
        key.push(':');
        key.push_str(part);
    }
    key
}

/// Stores one secret, and says whether the store holds it now.
pub(crate) async fn keep(key: String, value: &str) -> bool {
    if unavailable() {
        return false;
    }

    if size(value) >= MOST {
        tracing::warn!(
            %key,
            bytes = value.len(),
            "A secret is longer than one credential store entry holds, so it stays in the database"
        );
        return false;
    }

    match backend().set(&key, value) {
        Ok(()) => true,
        Err(refusal) => {
            report(&key, refusal);
            false
        }
    }
}

/// How much of one entry `value` spends.
#[cfg(windows)]
fn size(value: &str) -> usize {
    value.encode_utf16().count()
}

#[cfg(not(windows))]
fn size(value: &str) -> usize {
    value.len()
}

/// The secret one key names, when the store still has it.
pub(crate) async fn take(key: String) -> Stored {
    if unavailable() {
        return Stored::Unavailable;
    }

    match backend().get(&key) {
        Ok(Some(value)) => Stored::Value(value),
        Ok(None) => Stored::Missing,
        Err(refusal) => {
            report(&key, refusal);
            Stored::Unavailable
        }
    }
}

/// Drops whatever one key names, which a row that no longer has the secret needs.
pub(crate) async fn forget(key: String) {
    if unavailable() {
        return;
    }

    if let Err(refusal) = backend().delete(&key) {
        report(&key, refusal);
    }
}

/// One secret column of a row: the store's value when the row's mark says the
/// store holds it, and the column's own value otherwise.
pub(crate) async fn resolve(key: String, kept: bool, column: String) -> String {
    if !kept {
        return column;
    }

    match take(key).await {
        Stored::Value(value) => value,
        // Nothing left to fall back on: a row whose secret the store no longer
        // has reads as an empty secret, which asks for a new sign-in instead of
        // failing. A store that does not answer at all leaves the row as it was.
        Stored::Missing | Stored::Unavailable => column,
    }
}

/// Moves the secrets the account rows still carry into the store, and clears each
/// column once its secret is there.
///
/// Called at startup. It is idempotent — a row that was moved has nothing left in
/// its columns — and every step writes the store before the row, so an
/// interruption leaves a secret in both places rather than in neither.
pub(crate) async fn externalize(handle: &Handle) -> crate::Result<()> {
    let mut connection = handle.connection().await?;

    super::minecraft_users::externalize(&mut connection).await?;
    super::modrinth_users::externalize(&mut connection).await?;
    super::minecraft_device_tokens::externalize(&mut connection).await?;

    Ok(())
}

/// A system credential store.
///
/// The launcher reaches no other one: the backend here is the platform's own, and
/// only a test build substitutes another.
pub(crate) trait Backend: Send + Sync {
    fn set(&self, key: &str, value: &str) -> Result<(), Refused>;

    /// The value under `key`, or nothing when the store holds none.
    fn get(&self, key: &str) -> Result<Option<String>, Refused>;

    fn delete(&self, key: &str) -> Result<(), Refused>;
}

/// Why a store would not take part, in the words the platform gave.
pub(crate) struct Refused(String);

/// The platform's credential store.
#[cfg(not(test))]
struct System;

#[cfg(not(test))]
impl Backend for System {
    fn set(&self, key: &str, value: &str) -> Result<(), Refused> {
        entry(key)?.set_password(value).map_err(refused)
    }

    fn get(&self, key: &str) -> Result<Option<String>, Refused> {
        match entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(refused(error)),
        }
    }

    fn delete(&self, key: &str) -> Result<(), Refused> {
        match entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(refused(error)),
        }
    }
}

#[cfg(not(test))]
fn entry(key: &str) -> Result<keyring::Entry, Refused> {
    // The same name the data directory uses, so a build with a directory of its
    // own keeps its own secrets. A release resolves to the plain identifier.
    keyring::Entry::new(
        &crate::brand::app_data_dir_identifier(crate::brand::BUNDLE_IDENTIFIER),
        key,
    )
    .map_err(refused)
}

#[cfg(not(test))]
fn refused(error: keyring::Error) -> Refused {
    Refused(error.to_string())
}

/// Reports a refusal once: a host without a credential store refuses every
/// secret, and the run needs to say so once rather than once per token.
fn report(key: &str, Refused(reason): Refused) {
    NO_STORE.with(|flag| flag.set(true));
    if !REPORTED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            %key,
            %reason,
            "The system credential store is unavailable, so account secrets stay in the database"
        );
    }
}

#[cfg(test)]
fn backend() -> &'static dyn Backend {
    testing::backend()
}

#[cfg(not(test))]
fn backend() -> &'static dyn Backend {
    &System
}

/// The stores a test may put in place of the platform's own.
///
/// A test build reaches no real credential store: without a substitute every call
/// is refused, which is also what a host without one answers. The substitute is
/// per-thread, so a test that puts one in place leaves the tests running beside it
/// on that default.
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::{Backend, NO_STORE, Refused};

    /// A store that holds its secrets in memory.
    #[derive(Default)]
    pub(crate) struct Memory {
        values: Mutex<HashMap<String, String>>,
    }

    impl Memory {
        /// The secret under one key, for a test that has to look behind a read.
        pub(crate) fn value(&self, key: &str) -> Option<String> {
            self.lock().get(key).cloned()
        }

        /// Writes a secret the way another build would have left it.
        pub(crate) fn put(&self, key: &str, value: &str) {
            self.lock().insert(key.to_string(), value.to_string());
        }

        /// Every key the store holds, so a test can see what a move left behind.
        pub(crate) fn keys(&self) -> Vec<String> {
            self.lock().keys().cloned().collect()
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
            self.values
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        }
    }

    impl Backend for Memory {
        fn set(&self, key: &str, value: &str) -> Result<(), Refused> {
            self.put(key, value);
            Ok(())
        }

        fn get(&self, key: &str) -> Result<Option<String>, Refused> {
            Ok(self.value(key))
        }

        fn delete(&self, key: &str) -> Result<(), Refused> {
            self.lock().remove(key);
            Ok(())
        }
    }

    /// A store that refuses every call, the way a host without one does.
    pub(crate) struct Refusing;

    impl Backend for Refusing {
        fn set(&self, _: &str, _: &str) -> Result<(), Refused> {
            Err(Refused("no credential store".to_string()))
        }

        fn get(&self, _: &str) -> Result<Option<String>, Refused> {
            Err(Refused("no credential store".to_string()))
        }

        fn delete(&self, _: &str) -> Result<(), Refused> {
            Err(Refused("no credential store".to_string()))
        }
    }

    static REFUSING: Refusing = Refusing;

    thread_local! {
        /// The substitute this thread put in place, if any.
        static SUBSTITUTE: RefCell<Option<&'static (dyn Backend)>> =
            const { RefCell::new(None) };
    }

    pub(super) fn backend() -> &'static dyn Backend {
        SUBSTITUTE
            .with_borrow(|substitute| *substitute)
            .unwrap_or(&REFUSING)
    }

    /// A substitute in place on this thread for as long as the guard lives.
    pub(crate) struct Injection {
        memory: Option<&'static Memory>,
    }

    impl Injection {
        /// The store this injection put in place; no store means the host has
        /// none, which is what a test build starts from.
        pub(crate) fn store(&self) -> Option<&'static Memory> {
            self.memory
        }
    }

    /// Leaves this thread without a credential store.
    pub(crate) fn without_store() -> Injection {
        install(None);
        Injection { memory: None }
    }

    /// Puts a store that answers in place, which the test then reads back.
    pub(crate) fn with_store() -> Injection {
        let memory: &'static Memory = Box::leak(Box::new(Memory::default()));
        install(Some(memory));
        Injection {
            memory: Some(memory),
        }
    }

    /// The verdict on the store starts over with every substitute: the refusal a
    /// previous test in this thread saw must not silence this one.
    fn install(memory: Option<&'static Memory>) {
        NO_STORE.with(|flag| flag.set(false));
        let substitute: Option<&'static dyn Backend> = match memory {
            Some(memory) => Some(memory),
            None => None,
        };
        SUBSTITUTE.with_borrow_mut(|slot| *slot = substitute);
    }

    impl Drop for Injection {
        fn drop(&mut self) {
            install(None);
        }
    }
}
