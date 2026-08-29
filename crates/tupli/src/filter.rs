//! What is in the filter above a browsed table.
//!
//! A stack of rows, read top to bottom, each one a condition: `status = active`,
//! `created_at > 2026-01-01`. It needs no SQL and no quoting, and it can be
//! edited a piece at a time — a row can be untucked to see the rows without it
//! and ticked back on, which is most of what filtering actually is.
//!
//! Two of the rows are not about a column. [`Subject::Any`] asks the same
//! question of every column, which is what people want before they know the
//! schema; [`Subject::Raw`] holds a fragment of SQL, for the day the operators
//! run out — a subquery, a function call, `tsvector @@ plainto_tsquery(...)`.
//!
//! Everything here is plain data. The widgets that edit it live in the filter
//! band under the results toolbar and the values it holds go into the session
//! file, so a filter typed today is still there tomorrow — per tab, because a
//! `where` written against `orders` means nothing against `users`.

use serde::{Deserialize, Serialize};

/// What a chip does to its column.
///
/// Deliberately short. Every entry here is one somebody would otherwise have
/// typed by hand in the box next door, and the ones that are missing —
/// `between`, `similar to`, anything with a function in it — are exactly the
/// cases the raw clause is for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    #[default]
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Contains,
    StartsWith,
    EndsWith,
    In,
    IsNull,
    IsNotNull,
}

impl Op {
    /// Every operator, in the order the menu offers them: the comparisons
    /// first, then the text searches, then the two that need no value.
    pub const ALL: [Op; 12] = [
        Op::Eq,
        Op::Ne,
        Op::Gt,
        Op::Ge,
        Op::Lt,
        Op::Le,
        Op::Contains,
        Op::StartsWith,
        Op::EndsWith,
        Op::In,
        Op::IsNull,
        Op::IsNotNull,
    ];

    /// What the chip shows. The mathematical signs rather than `<>` and `>=`:
    /// the chip is read, not typed, and `≠` is one glyph instead of two
    /// characters that have to be recognised as a pair.
    pub fn symbol(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "≠",
            Op::Gt => ">",
            Op::Ge => "≥",
            Op::Lt => "<",
            Op::Le => "≤",
            Op::Contains => "contains",
            Op::StartsWith => "starts with",
            Op::EndsWith => "ends with",
            Op::In => "in",
            Op::IsNull => "is null",
            Op::IsNotNull => "is not null",
        }
    }

    /// Whether the operator has a right-hand side at all. `is null` does not,
    /// and a chip that asked for one would be asking a question with no answer.
    pub fn takes_value(self) -> bool {
        !matches!(self, Op::IsNull | Op::IsNotNull)
    }

    /// The inverse of [`Op::symbol`], and also of what a keyboard can produce:
    /// `>=` is accepted for `≥` and `!=` and `<>` for `≠`, because the display
    /// glyphs are chosen for reading and nobody has them under a key.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        Op::ALL
            .into_iter()
            .find(|op| op.symbol() == text)
            .or_else(|| match text.as_str() {
                ">=" => Some(Op::Ge),
                "<=" => Some(Op::Le),
                "!=" | "<>" => Some(Op::Ne),
                "==" => Some(Op::Eq),
                _ => None,
            })
    }
}

/// How a chip joins the one before it. The first chip in a row has one of
/// these too and ignores it, which is simpler than making the field optional
/// and having every caller ask which chip it is holding.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Join {
    #[default]
    And,
    Or,
}

impl Join {
    pub fn keyword(self) -> &'static str {
        match self {
            Join::And => "and",
            Join::Or => "or",
        }
    }

    pub fn flipped(self) -> Self {
        match self {
            Join::And => Join::Or,
            Join::Or => Join::And,
        }
    }
}

/// What a row is *about*.
///
/// Almost always a column. The other two are the escapes: [`Subject::Any`] for
/// "is that string anywhere in this table", which is the question people ask
/// before they know the schema, and [`Subject::Raw`] for the day the operators
/// run out and the honest answer is a fragment of SQL.
///
/// Raw is a row rather than a mode. An earlier version had one hand-written
/// clause that replaced the whole filter, which meant that reaching for a
/// function call cost you the three plain conditions you already had.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    #[default]
    Column,
    Any,
    Raw,
}

impl Subject {
    fn is_column(&self) -> bool {
        *self == Subject::Column
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chip {
    /// Whether this row is in force.
    ///
    /// Unticking is not deleting, and the difference is the whole reason the
    /// tick is there: half of filtering is trying a condition, taking it off
    /// to see the rows without it, and putting it back. Retyping it each time
    /// is what stops people trying.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Subject::is_column")]
    pub subject: Subject,
    pub column: String,
    #[serde(default)]
    pub op: Op,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub join: Join,
}

fn yes() -> bool {
    true
}

fn is_yes(value: &bool) -> bool {
    *value
}

impl Default for Chip {
    fn default() -> Self {
        Self {
            enabled: true,
            subject: Subject::default(),
            column: String::new(),
            op: Op::default(),
            value: String::new(),
            join: Join::default(),
        }
    }
}

impl Chip {
    pub fn new(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            ..Default::default()
        }
    }

    /// A row holding a hand-written predicate.
    pub fn raw(text: impl Into<String>) -> Self {
        Self {
            subject: Subject::Raw,
            value: text.into(),
            ..Default::default()
        }
    }

    /// What the row's first menu shows. The column's own name when there is
    /// one, because the name is the answer and "Column: id" would be the app
    /// reading its own label out loud.
    pub fn subject_label(&self) -> &str {
        match self.subject {
            Subject::Column => self.column.as_str(),
            Subject::Any => "Any column",
            Subject::Raw => "Raw SQL",
        }
    }

    /// This row as a predicate, or nothing when it is not finished: a row with
    /// no column, or one that wants a value and has not been given one. Half a
    /// condition is not a condition, and sending it would turn a typo into a
    /// syntax error from the server.
    ///
    /// `columns` is what the rows on screen have, and is only read by an "any
    /// column" row — which has to name them all, because there is no `*` in a
    /// `where` clause.
    pub fn to_sql(&self, columns: &[String]) -> Option<String> {
        if !self.enabled {
            return None;
        }
        match self.subject {
            Subject::Column => self.against(&self.column),
            // Bracketed, always: this is a run of `or`s going into a clause
            // that may join it to its neighbours with `and`, and an unbracketed
            // one would quietly swallow them.
            Subject::Any => {
                let parts: Vec<String> = columns
                    .iter()
                    .filter_map(|name| self.against(name))
                    .collect();
                match parts.is_empty() {
                    true => None,
                    false => Some(format!("({})", parts.join(" or "))),
                }
            }
            // Bracketed for the same reason, and not read at all beyond that:
            // the row is somebody writing SQL, and second-guessing it is how
            // an escape hatch stops being one.
            Subject::Raw => {
                let text = self.value.trim();
                match text.is_empty() {
                    true => None,
                    false => Some(format!("({text})")),
                }
            }
        }
    }

    /// The comparison this row makes, asked of one named column.
    fn against(&self, column: &str) -> Option<String> {
        let column = column.trim();
        if column.is_empty() {
            return None;
        }
        let column = db::schema::quote_ident(column);
        if !self.op.takes_value() {
            return Some(format!(
                "{column} {}",
                match self.op {
                    Op::IsNotNull => "is not null",
                    _ => "is null",
                }
            ));
        }
        let value = self.value.trim();
        if value.is_empty() {
            return None;
        }
        Some(match self.op {
            Op::Eq => format!("{column} = {}", literal(value)),
            Op::Ne => format!("{column} <> {}", literal(value)),
            Op::Gt => format!("{column} > {}", literal(value)),
            Op::Ge => format!("{column} >= {}", literal(value)),
            Op::Lt => format!("{column} < {}", literal(value)),
            Op::Le => format!("{column} <= {}", literal(value)),
            // Cast, because "contains" is a question about the text of a
            // value and people ask it of numbers and uuids as readily as of
            // names. `ilike` rather than `like` for the same reason: nobody
            // typing three letters into a filter box means them case-sensitively.
            Op::Contains => format!("{column}::text ilike {}", pattern(value, true, true)),
            Op::StartsWith => format!("{column}::text ilike {}", pattern(value, false, true)),
            Op::EndsWith => format!("{column}::text ilike {}", pattern(value, true, false)),
            Op::In => format!(
                "{column} in ({})",
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(literal)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Op::IsNull | Op::IsNotNull => unreachable!("handled above"),
        })
    }
}

/// A value as a SQL literal.
///
/// Always quoted, never interpreted. Postgres reads an unadorned string
/// literal as `unknown` and coerces it to whatever the column is, so `id = '7'`
/// finds integer 7 and `flag = 'true'` finds a true boolean — which means there
/// is no reason to guess at types here, and one very good reason not to: a
/// postcode of `01234` is not the number 1234, and a filter that quietly
/// decided otherwise would be wrong in a way nobody would think to check.
///
/// Doubling the quote is also the whole of the injection story. There is no
/// path from this function to an unquoted fragment.
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The same, as a `like` pattern with the wildcards the user did not ask for
/// taken out. Someone typing `50%` into a "contains" chip means the two
/// characters, not "anything at all"; the pattern's own `%` are the ones this
/// function adds.
fn pattern(value: &str, leading: bool, trailing: bool) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    if leading {
        escaped.push('%');
    }
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    if trailing {
        escaped.push('%');
    }
    literal(&escaped)
}

/// The whole filter for one tab: a stack of rows, read top to bottom.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chips: Vec<Chip>,
    /// Written by an older version, which had one hand-written clause standing
    /// in for the whole filter rather than a row holding one. Read once by
    /// [`Filter::migrated`], folded into a [`Subject::Raw`] row, and never
    /// written again.
    #[serde(default, skip_serializing_if = "is_false")]
    raw: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    text: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Filter {
    /// A filter of exactly one row. Used where the app writes the condition
    /// itself and there is nothing to add to.
    pub fn just(chip: Chip) -> Self {
        Self {
            chips: vec![chip],
            ..Default::default()
        }
    }

    /// A filter as read from a session file, with an old version's clause
    /// turned into a row of its own. Only the clause that was actually in
    /// force comes across: text left behind by someone who switched back to
    /// the chips was not filtering anything then and must not start now.
    pub fn migrated(mut self) -> Self {
        let text = std::mem::take(&mut self.text);
        if self.raw && !text.trim().is_empty() {
            self.chips.push(Chip::raw(text));
        }
        self.raw = false;
        self
    }

    /// What goes after `where`, or nothing at all.
    pub fn predicate(&self, columns: &[String]) -> String {
        self.chips_sql(columns)
    }

    /// The chips as one clause.
    ///
    /// The row reads strictly left to right, which is the only reading it can
    /// show: there is nowhere on a flat row of pills to put a bracket. SQL does
    /// not agree — `and` binds tighter than `or` there — so a bracket goes in
    /// wherever the join changes and nowhere else. An unbroken run of `and`s or
    /// of `or`s means the same thing either way and comes out clean, which
    /// matters because this string is also what the funnel hands over when
    /// someone switches to writing the clause by hand.
    ///
    /// Anyone who wants `a and (b or c)` has that clause for it.
    fn chips_sql(&self, columns: &[String]) -> String {
        let mut out = String::new();
        // The join holding what is in `out` together, once there is more than
        // one chip in it.
        let mut joined: Option<Join> = None;
        for chip in &self.chips {
            let Some(sql) = chip.to_sql(columns) else {
                continue;
            };
            if out.is_empty() {
                out = sql;
                continue;
            }
            if joined.is_some_and(|previous| previous != chip.join) {
                out = format!("({out})");
            }
            out = format!("{out} {} {sql}", chip.join.keyword());
            joined = Some(chip.join);
        }
        out
    }

    /// Is there anything to apply? A filter with three unfinished rows in it
    /// is empty as far as the server is concerned, and the toolbar says so by
    /// not lighting the funnel.
    pub fn is_active(&self, columns: &[String]) -> bool {
        !self.predicate(columns).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip(column: &str, op: Op, value: &str) -> Chip {
        Chip {
            column: column.to_string(),
            op,
            value: value.to_string(),
            join: Join::And,
            ..Default::default()
        }
    }

    #[test]
    fn a_value_is_a_literal_and_never_a_fragment() {
        let chip = chip("name", Op::Eq, "O'Brien'; drop table users --");
        assert_eq!(
            chip.to_sql(&[]).unwrap(),
            r#"name = 'O''Brien''; drop table users --'"#
        );
    }

    /// The reason nothing here tries to spot a number: Postgres does it better,
    /// and a leading zero is data.
    #[test]
    fn numbers_are_quoted_too_because_postgres_will_coerce_them() {
        assert_eq!(chip("id", Op::Eq, "7").to_sql(&[]).unwrap(), r#"id = '7'"#);
        assert_eq!(
            chip("zip", Op::Eq, "01234").to_sql(&[]).unwrap(),
            r#"zip = '01234'"#
        );
    }

    #[test]
    fn contains_searches_the_text_of_whatever_the_column_is() {
        assert_eq!(
            chip("id", Op::Contains, "4f2").to_sql(&[]).unwrap(),
            r#"id::text ilike '%4f2%'"#
        );
        assert_eq!(
            chip("email", Op::StartsWith, "ada@").to_sql(&[]).unwrap(),
            r#"email::text ilike 'ada@%'"#
        );
        assert_eq!(
            chip("email", Op::EndsWith, ".org").to_sql(&[]).unwrap(),
            r#"email::text ilike '%.org'"#
        );
    }

    #[test]
    fn a_percent_someone_typed_is_a_percent_and_not_a_wildcard() {
        assert_eq!(
            chip("note", Op::Contains, "50%_off").to_sql(&[]).unwrap(),
            r#"note::text ilike '%50\%\_off%'"#
        );
    }

    #[test]
    fn is_null_needs_no_value_and_everything_else_does() {
        assert_eq!(
            chip("deleted_at", Op::IsNull, "").to_sql(&[]).unwrap(),
            r#"deleted_at is null"#
        );
        assert_eq!(
            chip("deleted_at", Op::IsNotNull, "").to_sql(&[]).unwrap(),
            r#"deleted_at is not null"#
        );
        // Unfinished, so it contributes nothing rather than a syntax error.
        assert_eq!(chip("status", Op::Eq, "  ").to_sql(&[]), None);
        assert_eq!(chip("", Op::Eq, "x").to_sql(&[]), None);
    }

    #[test]
    fn in_splits_on_commas_and_quotes_each_side() {
        assert_eq!(
            chip("plan", Op::In, "free, pro , enterprise")
                .to_sql(&[])
                .unwrap(),
            r#"plan in ('free', 'pro', 'enterprise')"#
        );
    }

    #[test]
    fn chips_read_left_to_right_whatever_the_operators_are() {
        let filter = Filter {
            chips: vec![
                chip("a", Op::Eq, "1"),
                Chip {
                    join: Join::Or,
                    ..chip("b", Op::Eq, "2")
                },
                chip("c", Op::Eq, "3"),
            ],
            ..Default::default()
        };
        assert_eq!(filter.predicate(&[]), r#"(a = '1' or b = '2') and c = '3'"#);
    }

    #[test]
    fn a_run_of_one_join_needs_no_brackets() {
        // The bracket is only there to stop SQL reading `and` first. With one
        // kind of join in the stack there is nothing to stop, and this clause
        // is also what the band prints along its foot — brackets around every
        // term would make that unreadable for no gain.
        for join in [Join::And, Join::Or] {
            let filter = Filter {
                chips: vec![
                    chip("a", Op::Eq, "1"),
                    Chip {
                        join,
                        ..chip("b", Op::Eq, "2")
                    },
                    Chip {
                        join,
                        ..chip("c", Op::Eq, "3")
                    },
                ],
                ..Default::default()
            };
            let keyword = join.keyword();
            assert_eq!(
                filter.predicate(&[]),
                format!("a = '1' {keyword} b = '2' {keyword} c = '3'")
            );
        }
    }

    #[test]
    fn a_bracket_goes_in_wherever_the_join_changes() {
        // `a or b and c` is `(a or b) and c` read left to right, and plain SQL
        // would read it as `a or (b and c)` — the one case the bracket exists
        // for, in the direction that is easy to get wrong.
        let filter = Filter {
            chips: vec![
                chip("a", Op::Eq, "1"),
                Chip {
                    join: Join::Or,
                    ..chip("b", Op::Eq, "2")
                },
                Chip {
                    join: Join::And,
                    ..chip("c", Op::Eq, "3")
                },
            ],
            ..Default::default()
        };
        assert_eq!(filter.predicate(&[]), r#"(a = '1' or b = '2') and c = '3'"#);
    }

    #[test]
    fn an_unfinished_chip_does_not_break_the_ones_around_it() {
        let filter = Filter {
            chips: vec![
                chip("a", Op::Eq, "1"),
                chip("b", Op::Eq, ""),
                chip("c", Op::Eq, "3"),
            ],
            ..Default::default()
        };
        assert_eq!(filter.predicate(&[]), r#"a = '1' and c = '3'"#);
        assert!(filter.is_active(&[]));
    }

    #[test]
    fn nothing_typed_is_no_predicate_at_all() {
        assert_eq!(Filter::default().predicate(&[]), "");
        assert!(!Filter::default().is_active(&[]));
    }

    #[test]
    fn an_any_column_row_asks_the_same_question_of_every_column() {
        let columns = vec!["id".to_string(), "email".to_string()];
        let chip = Chip {
            subject: Subject::Any,
            op: Op::Contains,
            value: "acme".to_string(),
            ..Default::default()
        };
        assert_eq!(
            chip.to_sql(&columns).unwrap(),
            r#"(id::text ilike '%acme%' or email::text ilike '%acme%')"#
        );
    }

    #[test]
    fn a_raw_row_goes_in_bracketed_and_otherwise_untouched() {
        let filter = Filter {
            chips: vec![
                chip("a", Op::Eq, "1"),
                Chip::raw("b @@ plainto_tsquery('x')"),
            ],
            ..Default::default()
        };
        assert_eq!(
            filter.predicate(&[]),
            r#"a = '1' and (b @@ plainto_tsquery('x'))"#
        );
    }

    #[test]
    fn an_unticked_row_filters_nothing() {
        let filter = Filter {
            chips: vec![
                chip("a", Op::Eq, "1"),
                Chip {
                    enabled: false,
                    ..chip("b", Op::Eq, "2")
                },
            ],
            ..Default::default()
        };
        assert_eq!(filter.predicate(&[]), r#"a = '1'"#);
    }

    #[test]
    fn an_older_sessions_clause_comes_back_as_a_row() {
        let filter = Filter {
            raw: true,
            text: "id > 100".to_string(),
            ..Default::default()
        }
        .migrated();
        assert_eq!(filter.chips.len(), 1);
        assert_eq!(filter.predicate(&[]), "(id > 100)");
    }

    /// The clause was not in force when the session was written, so it was not
    /// filtering anything then and must not start now.
    #[test]
    fn a_clause_an_older_session_had_switched_away_from_is_dropped() {
        let filter = Filter {
            chips: vec![chip("a", Op::Eq, "1")],
            text: "id > 100".to_string(),
            ..Default::default()
        }
        .migrated();
        assert_eq!(filter.predicate(&[]), r#"a = '1'"#);
    }
}
