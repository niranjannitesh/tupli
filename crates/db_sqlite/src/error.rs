//! `rusqlite::Error` as something the window can act on.
//!
//! SQLite has no SQLSTATE and no `DETAIL`/`HINT`, so most of [`db::DbError`]
//! stays empty. The one field worth the trouble is [`db::DbError::position`]:
//! since 3.38 SQLite reports the byte offset of the token it choked on, and
//! that offset is what puts the editor's caret on the word rather than at the
//! top of the statement.

use db::{DbError, ErrorClass};
use rusqlite::ffi::ErrorCode;
use rusqlite::Error;

/// What went wrong, classified by what the app should offer to do about it.
pub fn classify(error: Error) -> DbError {
    match error {
        // The parse failed and SQLite said where.
        Error::SqlInputError {
            error,
            msg,
            sql,
            offset,
        } => {
            let mut out = DbError::new(class_for(error.code, &msg), msg);
            out.position = char_position(&sql, offset);
            out
        }
        Error::SqliteFailure(code, message) => {
            let message = message.unwrap_or_else(|| code.to_string());
            DbError::new(class_for(code.code, &message), message)
        }
        // A statement with something after the semicolon. The console splits
        // on semicolons before it gets here, so this means a splitter and a
        // parser disagreed — which is a bug in the app, not in the SQL.
        Error::MultipleStatement => {
            DbError::internal("This is more than one statement. Run them one at a time.")
        }
        other => DbError::new(ErrorClass::Internal, other.to_string()),
    }
}

/// A failure while opening the file, which is never the statement's fault.
pub fn connection_error(error: Error) -> DbError {
    let mut out = classify(error);
    if !out.is_canceled() {
        out.class = ErrorClass::Connection;
    }
    out
}

fn class_for(code: ErrorCode, message: &str) -> ErrorClass {
    match code {
        ErrorCode::OperationInterrupted => ErrorClass::Canceled,
        ErrorCode::CannotOpen | ErrorCode::NotADatabase | ErrorCode::DatabaseBusy => {
            ErrorClass::Connection
        }
        ErrorCode::PermissionDenied | ErrorCode::ReadOnly => ErrorClass::Server,
        ErrorCode::ConstraintViolation => ErrorClass::Server,
        // `SQLITE_ERROR` covers both "you typed this wrong" and "there is no
        // such table", and only the message tells them apart. Both belong on
        // the statement, which is what `Syntax` means to the caller: put the
        // caret there and do not offer a retry.
        ErrorCode::Unknown => match message.contains("no such") || message.contains("syntax error")
        {
            true => ErrorClass::Syntax,
            false => ErrorClass::Server,
        },
        _ => ErrorClass::Server,
    }
}

/// SQLite counts bytes and [`db::DbError::position`] counts characters, from
/// one. A statement with an accented word before the error would otherwise
/// point past it.
fn char_position(sql: &str, offset: i32) -> Option<u32> {
    let offset = usize::try_from(offset).ok()?;
    let head = sql.get(..offset)?;
    Some(head.chars().count() as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_counts_characters_and_not_bytes() {
        // The `ü` is the eighth character and the ninth byte.
        assert_eq!(char_position("sélect ü, x", 8), Some(8));
        assert_eq!(char_position("select x", 0), Some(1));
        // An offset SQLite reports as "nowhere" is no position at all, and
        // neither is one that lands inside a character.
        assert_eq!(char_position("select x", -1), None);
        assert_eq!(char_position("sélect x", 2), None);
    }

    #[test]
    fn a_missing_table_lands_on_the_statement_and_not_on_the_server() {
        assert_eq!(
            class_for(ErrorCode::Unknown, "no such table: users"),
            ErrorClass::Syntax
        );
        assert_eq!(
            class_for(ErrorCode::Unknown, "database is locked"),
            ErrorClass::Server
        );
        assert_eq!(
            class_for(ErrorCode::OperationInterrupted, "interrupted"),
            ErrorClass::Canceled
        );
    }
}
