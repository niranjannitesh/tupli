//! What is in the filter box above a browsed table.
//!
//! Two ways of saying the same thing. The one people reach for first is a row
//! of chips — `status = active`, `created_at > 2026-01-01` — because it needs
//! no SQL and no quoting, and because it can be edited a piece at a time. The
//! other is the `where` clause written out, for the day the chips run out of
//! vocabulary: a subquery, a function call, `tsvector @@ plainto_tsquery(...)`.
//! The funnel switches between them, and switching from chips to text hands
//! over the SQL the chips were producing, so nothing is lost on the way.
//!
//! Everything here is plain data. The widgets that edit it live in the results
//! toolbar and the values it holds go into the session file, so a filter typed
//! today is still there tomorrow — per tab, because a `where` written against
//! `orders` means nothing against `users`.

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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chip {
    pub column: String,
    #[serde(default)]
    pub op: Op,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub join: Join,
}

impl Chip {
    pub fn new(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            ..Default::default()
        }
    }

    /// This chip as a predicate, or nothing when it is not finished: a chip
    /// with no column, or one that wants a value and has not been given one.
    /// Half a condition is not a condition, and sending it would turn a typo
    /// into a syntax error from the server.
    pub fn to_sql(&self) -> Option<String> {
        let column = self.column.trim();
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

/// The whole filter for one tab.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    /// Whether the raw clause is the one in force. Chips by default: they are
    /// the mode you can use without knowing anything.
    #[serde(default, skip_serializing_if = "is_false")]
    pub raw: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chips: Vec<Chip>,
    /// The hand-written `where` clause, without the keyword.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Filter {
    /// What goes after `where`, or nothing at all.
    pub fn predicate(&self) -> String {
        match self.raw {
            true => self.text.trim().to_string(),
            false => self.chips_sql(),
        }
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
    fn chips_sql(&self) -> String {
        let mut out = String::new();
        // The join holding what is in `out` together, once there is more than
        // one chip in it.
        let mut joined: Option<Join> = None;
        for chip in &self.chips {
            let Some(sql) = chip.to_sql() else { continue };
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

    /// Is there anything to apply? A filter with three unfinished chips in it
    /// is empty as far as the server is concerned, and the toolbar says so by
    /// not lighting the funnel.
    pub fn is_active(&self) -> bool {
        !self.predicate().is_empty()
    }

    /// Switch to the hand-written clause, carrying the chips over as its
    /// starting text. The chips are kept rather than thrown away: switching
    /// back is one click, and losing six chips to a mis-click is not.
    pub fn to_raw(&mut self) {
        if !self.raw {
            let sql = self.chips_sql();
            if !sql.is_empty() {
                self.text = sql;
            }
            self.raw = true;
        }
    }

    pub fn to_chips(&mut self) {
        self.raw = false;
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
        }
    }

    #[test]
    fn a_value_is_a_literal_and_never_a_fragment() {
        let chip = chip("name", Op::Eq, "O'Brien'; drop table users --");
        assert_eq!(
            chip.to_sql().unwrap(),
            r#"name = 'O''Brien''; drop table users --'"#
        );
    }

    /// The reason nothing here tries to spot a number: Postgres does it better,
    /// and a leading zero is data.
    #[test]
    fn numbers_are_quoted_too_because_postgres_will_coerce_them() {
        assert_eq!(chip("id", Op::Eq, "7").to_sql().unwrap(), r#"id = '7'"#);
        assert_eq!(
            chip("zip", Op::Eq, "01234").to_sql().unwrap(),
            r#"zip = '01234'"#
        );
    }

    #[test]
    fn contains_searches_the_text_of_whatever_the_column_is() {
        assert_eq!(
            chip("id", Op::Contains, "4f2").to_sql().unwrap(),
            r#"id::text ilike '%4f2%'"#
        );
        assert_eq!(
            chip("email", Op::StartsWith, "ada@").to_sql().unwrap(),
            r#"email::text ilike 'ada@%'"#
        );
        assert_eq!(
            chip("email", Op::EndsWith, ".org").to_sql().unwrap(),
            r#"email::text ilike '%.org'"#
        );
    }

    #[test]
    fn a_percent_someone_typed_is_a_percent_and_not_a_wildcard() {
        assert_eq!(
            chip("note", Op::Contains, "50%_off").to_sql().unwrap(),
            r#"note::text ilike '%50\%\_off%'"#
        );
    }

    #[test]
    fn is_null_needs_no_value_and_everything_else_does() {
        assert_eq!(
            chip("deleted_at", Op::IsNull, "").to_sql().unwrap(),
            r#"deleted_at is null"#
        );
        assert_eq!(
            chip("deleted_at", Op::IsNotNull, "").to_sql().unwrap(),
            r#"deleted_at is not null"#
        );
        // Unfinished, so it contributes nothing rather than a syntax error.
        assert_eq!(chip("status", Op::Eq, "  ").to_sql(), None);
        assert_eq!(chip("", Op::Eq, "x").to_sql(), None);
    }

    #[test]
    fn in_splits_on_commas_and_quotes_each_side() {
        assert_eq!(
            chip("plan", Op::In, "free, pro , enterprise")
                .to_sql()
                .unwrap(),
            r#"plan in ('free', 'pro', 'enterprise')"#
        );
    }

    #[test]
    fn chips_read_left_to_right_whatever_the_operators_are() {
        let filter = Filter {
            raw: false,
            chips: vec![
                chip("a", Op::Eq, "1"),
                Chip {
                    join: Join::Or,
                    ..chip("b", Op::Eq, "2")
                },
                chip("c", Op::Eq, "3"),
            ],
            text: String::new(),
        };
        assert_eq!(filter.predicate(), r#"(a = '1' or b = '2') and c = '3'"#);
    }

    #[test]
    fn a_run_of_one_join_needs_no_brackets() {
        // The bracket is only there to stop SQL reading `and` first. With one
        // kind of join in the row there is nothing to stop, and the clause is
        // also what the funnel hands over to be edited by hand — brackets
        // around every term make that a worse starting point than a blank box.
        for join in [Join::And, Join::Or] {
            let filter = Filter {
                raw: false,
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
                text: String::new(),
            };
            let keyword = join.keyword();
            assert_eq!(
                filter.predicate(),
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
            raw: false,
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
            text: String::new(),
        };
        assert_eq!(filter.predicate(), r#"(a = '1' or b = '2') and c = '3'"#);
    }

    #[test]
    fn an_unfinished_chip_does_not_break_the_ones_around_it() {
        let filter = Filter {
            raw: false,
            chips: vec![
                chip("a", Op::Eq, "1"),
                chip("b", Op::Eq, ""),
                chip("c", Op::Eq, "3"),
            ],
            text: String::new(),
        };
        assert_eq!(filter.predicate(), r#"a = '1' and c = '3'"#);
        assert!(filter.is_active());
    }

    #[test]
    fn nothing_typed_is_no_predicate_at_all() {
        assert_eq!(Filter::default().predicate(), "");
        assert!(!Filter::default().is_active());
    }

    #[test]
    fn switching_to_text_hands_over_what_the_chips_were_saying() {
        let mut filter = Filter {
            raw: false,
            chips: vec![chip("plan", Op::Eq, "pro")],
            text: String::new(),
        };
        filter.to_raw();
        assert!(filter.raw);
        assert_eq!(filter.text, r#"plan = 'pro'"#);
        // And the chips are still there to go back to.
        filter.to_chips();
        assert_eq!(filter.predicate(), r#"plan = 'pro'"#);
    }

    /// Switching an empty chip row to text must not wipe a clause that is
    /// already written: someone toggling the funnel to look at their chips and
    /// toggling straight back has not asked to lose anything.
    #[test]
    fn an_empty_chip_row_does_not_erase_the_clause() {
        let mut filter = Filter {
            raw: false,
            chips: Vec::new(),
            text: "id > 100".to_string(),
        };
        filter.to_raw();
        assert_eq!(filter.text, "id > 100");
    }
}
