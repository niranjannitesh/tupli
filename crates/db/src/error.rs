//! What went wrong, in a form the UI can act on.
//!
//! A driver error that arrives as a string can only be shown. This one can be
//! shown *and* used: a syntax error carries the byte offset the server
//! complained about, so the editor can put the caret there, and a class carries
//! whether retrying is worth offering.

use std::fmt;
use std::sync::Arc;

/// Broad categories, chosen by what the UI does differently for each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorClass {
    /// Could not reach or authenticate to the server. Offer to edit the
    /// connection.
    Connection,
    /// The statement is wrong. Put the caret on it; do not offer a retry.
    Syntax,
    /// The statement is fine but the data or the server said no — constraint
    /// violation, permission denied, deadlock.
    Server,
    /// The user pressed cancel. Not an error to report, only to unwind.
    Canceled,
    /// Something in the app, not the database.
    Internal,
}

impl ErrorClass {
    /// Is running the same statement again a reasonable thing to offer?
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Connection | Self::Server)
    }
}

/// A database error with everything the server told us kept intact.
#[derive(Clone, Debug)]
pub struct DbError {
    pub class: ErrorClass,
    /// The primary message, already stripped of the `ERROR: ` prefix.
    pub message: Arc<str>,
    /// The five-character SQLSTATE, when there is one.
    pub code: Option<Arc<str>>,
    /// The server's `DETAIL`.
    pub detail: Option<Arc<str>>,
    /// The server's `HINT`. Postgres' hints are unusually good and worth
    /// surfacing verbatim rather than paraphrasing.
    pub hint: Option<Arc<str>>,
    /// 1-based character position in the statement, as `ERROR … at character N`
    /// reports it. Converted to a 0-based char offset by [`Self::offset`].
    pub position: Option<u32>,
    /// The relation, column, or constraint the error names, for the cases where
    /// the grid can highlight it.
    pub table: Option<Arc<str>>,
    pub column: Option<Arc<str>>,
    pub constraint: Option<Arc<str>>,
}

impl DbError {
    pub fn new(class: ErrorClass, message: impl Into<Arc<str>>) -> Self {
        Self {
            class,
            message: message.into(),
            code: None,
            detail: None,
            hint: None,
            position: None,
            table: None,
            column: None,
            constraint: None,
        }
    }

    pub fn connection(message: impl Into<Arc<str>>) -> Self {
        Self::new(ErrorClass::Connection, message)
    }

    pub fn internal(message: impl Into<Arc<str>>) -> Self {
        Self::new(ErrorClass::Internal, message)
    }

    pub fn canceled() -> Self {
        Self::new(ErrorClass::Canceled, "Canceled")
    }

    /// Where in the statement to put the caret, as a 0-based char offset.
    pub fn offset(&self) -> Option<usize> {
        self.position.map(|p| p.saturating_sub(1) as usize)
    }

    pub fn is_canceled(&self) -> bool {
        self.class == ErrorClass::Canceled
    }

    /// Everything the server said, in the order psql prints it. This is what
    /// the Messages tab shows.
    pub fn full_text(&self) -> String {
        let mut out = String::new();
        if let Some(code) = &self.code {
            out.push_str(&format!("[{code}] "));
        }
        out.push_str(&self.message);
        for (label, value) in [("DETAIL", &self.detail), ("HINT", &self.hint)] {
            if let Some(value) = value {
                out.push_str(&format!("\n{label}:  {value}"));
            }
        }
        out
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DbError {}

/// SQLSTATE → class.
///
/// Classified by the first two characters, which is what the standard makes
/// meaningful; the rest of the code varies per error and per version.
pub fn class_for_sqlstate(code: &str) -> ErrorClass {
    match code.get(..2) {
        Some("42") => ErrorClass::Syntax, // syntax error or access rule violation
        Some("08") => ErrorClass::Connection, // connection exception
        Some("28") => ErrorClass::Connection, // invalid authorization
        Some("3D") | Some("3F") => ErrorClass::Connection, // no such database / schema
        Some("57") if code == "57014" => ErrorClass::Canceled, // query_canceled
        Some("XX") => ErrorClass::Internal, // internal error
        _ => ErrorClass::Server,
    }
}

pub type DbResult<T> = Result<T, DbError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_syntax_error_positions_the_caret() {
        let mut e = DbError::new(ErrorClass::Syntax, "syntax error at or near \"slect\"");
        e.position = Some(1);
        assert_eq!(e.offset(), Some(0));
        e.position = Some(18);
        assert_eq!(e.offset(), Some(17));
    }

    #[test]
    fn sqlstates_are_classified_by_family() {
        assert_eq!(class_for_sqlstate("42601"), ErrorClass::Syntax);
        assert_eq!(class_for_sqlstate("28P01"), ErrorClass::Connection);
        assert_eq!(class_for_sqlstate("57014"), ErrorClass::Canceled);
        assert_eq!(class_for_sqlstate("23505"), ErrorClass::Server);
        assert!(class_for_sqlstate("40P01").is_retryable());
        assert!(!class_for_sqlstate("42601").is_retryable());
    }

    #[test]
    fn the_messages_tab_gets_everything_the_server_said() {
        let mut e = DbError::new(
            ErrorClass::Server,
            "duplicate key value violates unique constraint \"users_pkey\"",
        );
        e.code = Some("23505".into());
        e.detail = Some("Key (id)=(1) already exists.".into());
        let text = e.full_text();
        assert!(text.starts_with("[23505] duplicate key"));
        assert!(text.contains("DETAIL:  Key (id)=(1)"));
    }
}

/// Something the server said while a statement ran, that was not an error.
///
/// `RAISE NOTICE` from a function, `WARNING: there is already a transaction in
/// progress`, the `NOTICE: relation already exists, skipping` that every
/// `create table if not exists` produces. `psql` prints these and every GUI
/// client that hides them makes its users wonder why their migration silently
/// did nothing, so they are carried back with the run rather than dropped on
/// the floor of the driver task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// `NOTICE`, `WARNING`, `INFO`, `LOG`, `DEBUG` — as the server spelled it,
    /// since a localised server spells it in its own language and second
    /// guessing that would lose information rather than tidy it.
    pub severity: Arc<str>,
    pub message: Arc<str>,
    pub detail: Option<Arc<str>>,
    pub hint: Option<Arc<str>>,
}

impl Notice {
    /// Everything the server said, on as few lines as it takes.
    pub fn full_text(&self) -> String {
        format!("{}: {}", self.severity, self.full_text_without_severity())
    }

    /// The same, for a caller that is already showing the severity itself.
    pub fn full_text_without_severity(&self) -> String {
        let mut text = self.message.to_string();
        for extra in [self.detail.as_ref(), self.hint.as_ref()]
            .into_iter()
            .flatten()
        {
            text.push('\n');
            text.push_str(extra);
        }
        text
    }

    /// Is this one worth colouring? `WARNING` and worse is the server telling
    /// you something went differently than you asked; a `NOTICE` is it telling
    /// you what it did.
    pub fn is_warning(&self) -> bool {
        matches!(&*self.severity, "WARNING" | "EXCEPTION" | "PANIC" | "FATAL")
    }
}
