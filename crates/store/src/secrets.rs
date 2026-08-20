//! Passwords.
//!
//! The Keychain, and only the Keychain. The SQLite store deliberately has no
//! column that could hold a password: a client that keeps credentials in a file
//! it owns has to invent key management, and every homegrown answer to that is
//! worse than the one the OS ships.
//!
//! Items are generic passwords under service `tupli` with the connection's UUID
//! as the account, so a connection's secret dies with the connection and two
//! connections to the same host never collide.

use anyhow::{anyhow, Context, Result};
use security_framework::passwords;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use uuid::Uuid;

const SERVICE: &str = "tupli";

/// The service the items were written under before the app was called Tupli.
///
/// A renamed service is a different keyring as far as macOS is concerned, so
/// every saved password went quiet on the launch after the rename. Read through
/// once, and written back under the new name so it is asked for once and never
/// again.
const LEGACY_SERVICE: &str = "tqlui";

/// What the Keychain has already said, for the life of the process.
///
/// macOS asks the user before handing a password to a process, and it remembers
/// the answer against that process's code signature. A locally built bundle is
/// signed ad hoc, so its signature changes with every rebuild and the
/// remembered answer never matches the next launch — which is why the prompt
/// came back on every tab that opened another database, every reconnect and
/// every time the connection sheet filled its password field. Reading once per
/// connection per launch makes that at most one prompt instead of one per
/// action.
///
/// Errors are deliberately not remembered: a locked keychain and a dismissed
/// prompt are both things the user can fix and then retry.
static CACHE: LazyLock<Mutex<HashMap<Uuid, Option<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The lock is only ever held across a `HashMap` operation, never across the
/// Keychain call, so a poisoned lock means a panic somewhere unrelated and the
/// right response is to carry on without the cache rather than to bring the app
/// down over it.
fn cache() -> std::sync::MutexGuard<'static, HashMap<Uuid, Option<String>>> {
    CACHE.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// No such item. The one Keychain failure that is not a failure.
const ITEM_NOT_FOUND: i32 = -25300;

/// Read the password for a connection. `None` when there is no item, which is
/// the normal case for trust/peer authentication — not an error.
///
/// Anything else *is* an error and is returned as one. It used to be swallowed
/// into `None`, and the result was that a connection whose password could not
/// be read looked exactly like a connection that never had one: the server said
/// "password missing" about a password that was sitting in the Keychain the
/// whole time, and nothing anywhere said the word Keychain. A locked keychain,
/// a denied prompt and a refused ACL are all things a person can act on, but
/// only if they are told.
pub fn password(id: Uuid) -> Result<Option<String>> {
    if let Some(known) = cache().get(&id) {
        return Ok(known.clone());
    }
    let answer = match passwords::get_generic_password(SERVICE, &id.to_string()) {
        Ok(bytes) => Some(String::from_utf8(bytes).context("keychain item is not UTF-8")?),
        Err(error) if error.code() == ITEM_NOT_FOUND => adopt_legacy(id)?,
        Err(error) => {
            return Err(anyhow!(
                "could not read the password from the Keychain: {} ({})",
                error.message().unwrap_or_else(|| error.to_string()),
                error.code()
            ))
        }
    };
    cache().insert(id, answer.clone());
    Ok(answer)
}

/// The password this connection had under the old service name, moved over.
///
/// `None` for the ordinary case of a connection that never had one. A failure
/// to *write* the item back is not a failure to read it: the password is
/// returned either way, and the worst that a failed write costs is doing this
/// again next launch.
fn adopt_legacy(id: Uuid) -> Result<Option<String>> {
    let bytes = match passwords::get_generic_password(LEGACY_SERVICE, &id.to_string()) {
        Ok(bytes) => bytes,
        // Anything at all, including a locked keychain: this is a fallback for
        // a name nobody uses any more, and it must not turn "no password" into
        // an error on the path that already answered that question.
        Err(_) => return Ok(None),
    };
    let password = String::from_utf8(bytes).context("keychain item is not UTF-8")?;
    if let Err(error) = passwords::set_generic_password(SERVICE, &id.to_string(), password.as_bytes())
    {
        log::warn!("could not re-file the Keychain item under {SERVICE}: {error}");
    }
    Ok(Some(password))
}

/// Store (or replace) the password for a connection. An empty password deletes
/// the item, because "" and "unset" mean the same thing to libpq and keeping a
/// blank secret around only makes the Keychain harder to read.
pub fn set_password(id: Uuid, password: &str) -> Result<()> {
    if password.is_empty() {
        return delete_password(id);
    }
    passwords::set_generic_password(SERVICE, &id.to_string(), password.as_bytes())
        .context("could not write the password to the Keychain")?;
    cache().insert(id, Some(password.to_string()));
    Ok(())
}

/// Forget a connection's password. Succeeds when there was nothing to forget.
pub fn delete_password(id: Uuid) -> Result<()> {
    let _ = passwords::delete_generic_password(SERVICE, &id.to_string());
    cache().insert(id, None);
    Ok(())
}
