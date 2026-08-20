//! Redis errors in the shape the rest of the app already handles.
//!
//! [`db::DbError`] was designed around Postgres and carries a SQLSTATE, a
//! caret position, and the name of a constraint. Redis has none of those, and
//! filling them with plausible-looking nonsense would be worse than leaving
//! them empty. What it does have is a leading uppercase word — `WRONGTYPE`,
//! `NOAUTH`, `MOVED` — that plays exactly the role SQLSTATE does, so that is
//! what goes in `code`, and it is what [`class_for_code`] classifies by.

use db::{DbError, ErrorClass};
use redis::{ErrorKind, RedisError};

/// A Redis error word → what the UI should do about it.
///
/// The distinction that earns its keep is `Syntax` versus `Server`: a mistyped
/// command in the console should not offer a retry button, and a `WRONGTYPE`
/// against a key somebody else changed should.
pub fn class_for_code(code: &str) -> ErrorClass {
    match code {
        // Nobody is logged in, or the password was wrong. The fix is in the
        // connection sheet, not in the command.
        "NOAUTH" | "WRONGPASS" | "ERR_AUTH" => ErrorClass::Connection,
        // The command does not exist, has the wrong arity, or was handed an
        // argument it cannot parse. All of them are the text the user typed.
        "ERR" => ErrorClass::Syntax,
        // The user is authenticated and not allowed. That is the server's
        // answer about this command, not about the connection.
        "NOPERM" => ErrorClass::Server,
        _ => ErrorClass::Server,
    }
}

/// The leading uppercase word of a Redis error message, which is the closest
/// thing the protocol has to an error code.
///
/// `-WRONGTYPE Operation against a key…` — the word is not quoted, not
/// delimited, and not guaranteed, so anything that is not a bare run of capital
/// letters is treated as having no code at all rather than as having a strange
/// one.
pub fn code_in(message: &str) -> Option<&str> {
    let word = message.split_whitespace().next()?;
    let is_code = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit());
    is_code.then_some(word)
}

/// A driver error → [`DbError`], keeping everything the server said.
///
/// `fallback` is the class for an error that came from the client rather than
/// the server, and so has no code to classify it by — the same role it plays in
/// `db_pg`.
pub fn classify(error: &RedisError, fallback: ErrorClass) -> DbError {
    let detail = error.detail().map(str::to_owned);
    let message = match &detail {
        // `RedisError`'s own `Display` prefixes the category, which reads as
        // "response error: WRONGTYPE Operation against…" — twice as long and no
        // more informative than what the server actually sent.
        Some(detail) => detail.clone(),
        None => error.to_string(),
    };

    let code = error
        .code()
        .map(str::to_owned)
        .or_else(|| code_in(&message).map(str::to_owned));

    let class = match error.kind() {
        // The socket, not the server.
        ErrorKind::Io | ErrorKind::InvalidClientConfig | ErrorKind::RESP3NotSupported => {
            ErrorClass::Connection
        }
        ErrorKind::AuthenticationFailed => ErrorClass::Connection,
        ErrorKind::Parse | ErrorKind::UnexpectedReturnType => ErrorClass::Internal,
        ErrorKind::Client => fallback,
        _ => match &code {
            Some(code) => class_for_code(code),
            None => fallback,
        },
    };

    let mut db_error = DbError::new(class, strip_code(&message, code.as_deref()));
    db_error.hint = hint_for(code.as_deref());
    db_error.code = code.map(Into::into);
    db_error
}

/// The message without its leading code word, since the code is shown beside it.
fn strip_code<'a>(message: &'a str, code: Option<&str>) -> &'a str {
    match code {
        Some(code) => message
            .strip_prefix(code)
            .map(|rest| rest.trim_start())
            .filter(|rest| !rest.is_empty())
            .unwrap_or(message),
        None => message,
    }
}

/// Advice for the handful of errors where the next step is not obvious from the
/// message. Deliberately short of exhaustive: a hint that restates the error is
/// noise, and noise trains people to stop reading hints.
fn hint_for(code: Option<&str>) -> Option<std::sync::Arc<str>> {
    let text = match code? {
        "WRONGTYPE" => "This key holds a different type. Open it to see which.",
        "NOAUTH" | "WRONGPASS" => "Check the password on this connection.",
        "NOPERM" => "This ACL user is not allowed to run that command.",
        "LOADING" => "The server is loading its dataset. Try again shortly.",
        "MASTERDOWN" | "READONLY" => "This node is a replica and will not accept writes.",
        "OOM" => "The server is at its memory limit and refuses writes.",
        _ => return None,
    };
    Some(text.into())
}

/// The error for a write attempted on a connection that does not allow one.
///
/// Raised before anything reaches the socket. A read-only connection that
/// refuses writes only when the server happens to agree is not a guardrail —
/// most Redis servers will happily do as they are told.
pub fn refused(command: &str) -> DbError {
    let mut error = DbError::new(
        ErrorClass::Server,
        format!("{command} would change data, and this connection is read-only."),
    );
    error.hint = Some("Change the connection's safety level to allow writes.".into());
    error
}

/// The error for a command that would wait for something that may never come.
///
/// Refused for a technical reason rather than a safety one: this crate's
/// connection is multiplexed, so a command that never replies takes every
/// other pane on the same connection down with it. Worded so that the reader
/// knows it is the tool's limit and not the server's.
pub fn blocked(command: &str) -> DbError {
    let mut error = DbError::new(
        ErrorClass::Syntax,
        format!("{command} waits for the server to have something to say, which this connection cannot do."),
    );
    error.hint = Some("Blocking and subscription commands need a connection of their own.".into());
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_leading_capitals_are_the_error_code() {
        assert_eq!(code_in("WRONGTYPE Operation against a key"), Some("WRONGTYPE"));
        assert_eq!(code_in("NOAUTH Authentication required."), Some("NOAUTH"));
        assert_eq!(code_in("MOVED 3999 127.0.0.1:6381"), Some("MOVED"));
        // Not a code: an ordinary sentence, or one that only looks like one.
        assert_eq!(code_in("unknown command 'blah'"), None);
        assert_eq!(code_in(""), None);
    }

    #[test]
    fn a_mistyped_command_is_not_offered_a_retry_and_a_busy_server_is() {
        assert_eq!(class_for_code("ERR"), ErrorClass::Syntax);
        assert!(!class_for_code("ERR").is_retryable());
        assert_eq!(class_for_code("WRONGTYPE"), ErrorClass::Server);
        assert!(class_for_code("LOADING").is_retryable());
        assert_eq!(class_for_code("NOAUTH"), ErrorClass::Connection);
    }

    #[test]
    fn a_read_only_connection_says_so_before_anything_is_sent() {
        let error = refused("DEL");
        assert!(error.message.contains("read-only"));
        assert!(error.hint.is_some());
    }

    #[test]
    fn the_code_is_lifted_out_of_the_message_rather_than_repeated_in_it() {
        assert_eq!(
            strip_code("WRONGTYPE Operation against a key", Some("WRONGTYPE")),
            "Operation against a key"
        );
        // A message that is only its code keeps it, because the alternative is
        // an empty error.
        assert_eq!(strip_code("LOADING", Some("LOADING")), "LOADING");
        assert_eq!(strip_code("plain words", None), "plain words");
    }
}
