//! Synthetic data and frame timing for the M0 performance gate.
//!
//! The gate (docs/PLAN.md §16) is a 1M-row × 100-column result set held at a
//! p99 frame time under 8ms. Both halves live here so the benchmark is a real
//! run of the real element in a real window rather than a microbenchmark of
//! something adjacent to it.

use std::fmt::Write as _;

use db::ValueKind;
use db::{Column, ColumnData, ColumnMeta, NullMask, ResultSet, TextBuf, TextColumnBuilder};

/// Deterministic mixing so a benchmark run is byte-identical to the last one.
#[inline]
fn hash(row: usize, col: usize) -> u64 {
    let mut x = (row as u64)
        .wrapping_mul(6364136223846793005)
        .wrapping_add(col as u64 + 1);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x
}

const PLANS: [&str; 8] = [
    "free",
    "starter",
    "pro",
    "team",
    "business",
    "enterprise",
    "trial",
    "legacy",
];

/// A `rows` × `cols` result set that cycles through every storage layout, so a
/// benchmark exercises the borrowed path (text, dict, bool) and the formatted
/// path (i64, f64) in the same frame.
///
/// Columns repeat with period five: int, dictionary text, float, bool, text.
/// One row in seventeen is null in the nullable columns, which is what makes
/// the null-mask read part of the measured path rather than a branch that is
/// always false.
pub fn synthetic(rows: usize, cols: usize) -> ResultSet {
    let mut columns = Vec::with_capacity(cols);
    let mut scratch = String::new();

    for col in 0..cols {
        let name = match col {
            0 => "id".to_string(),
            _ => format!("col_{col:02}"),
        };
        let column = match col % 5 {
            0 => {
                let mut nulls = NullMask::with_capacity(rows);
                let mut v = Vec::with_capacity(rows);
                for row in 0..rows {
                    nulls.push(false, row);
                    v.push(if col == 0 {
                        row as i64 + 1
                    } else {
                        (hash(row, col) % 1_000_000) as i64
                    });
                }
                let meta = ColumnMeta::new(name, ValueKind::Int, "int8");
                let meta = if col == 0 {
                    meta.pk().not_null()
                } else {
                    meta.not_null()
                };
                Column {
                    meta,
                    nulls,
                    data: ColumnData::I64(v),
                }
            }
            1 => {
                let mut b = TextColumnBuilder::new();
                for row in 0..rows {
                    let h = hash(row, col);
                    if h % 17 == 0 {
                        b.push(None);
                    } else {
                        b.push(Some(PLANS[(h % 8) as usize]));
                    }
                }
                b.finish(ColumnMeta::new(name, ValueKind::Text, "text"))
            }
            2 => {
                let mut nulls = NullMask::with_capacity(rows);
                let mut v = Vec::with_capacity(rows);
                for row in 0..rows {
                    let h = hash(row, col);
                    let null = h % 17 == 0;
                    nulls.push(null, row);
                    v.push(if null {
                        0.
                    } else {
                        (h % 5_000_00) as f64 / 100.
                    });
                }
                Column {
                    meta: ColumnMeta::new(name, ValueKind::Decimal, "numeric"),
                    nulls,
                    data: ColumnData::F64(v),
                }
            }
            3 => {
                let mut nulls = NullMask::with_capacity(rows);
                let mut v = Vec::with_capacity(rows);
                for row in 0..rows {
                    nulls.push(false, row);
                    v.push(hash(row, col) % 3 != 0);
                }
                Column {
                    meta: ColumnMeta::new(name, ValueKind::Bool, "bool").not_null(),
                    nulls,
                    data: ColumnData::Bool(v),
                }
            }
            _ => {
                // Built directly rather than through `TextColumnBuilder` so the
                // benchmark is guaranteed a plain (un-dictionaried) text column
                // no matter what the encoder decides.
                let mut nulls = NullMask::with_capacity(rows);
                let mut t = TextBuf::with_capacity(rows, rows * 8);
                for row in 0..rows {
                    nulls.push(false, row);
                    scratch.clear();
                    let _ = write!(scratch, "v{:06x}", hash(row, col) & 0xff_ffff);
                    t.push(&scratch);
                }
                Column {
                    meta: ColumnMeta::new(name, ValueKind::Text, "varchar").not_null(),
                    nulls,
                    data: ColumnData::Text(t),
                }
            }
        };
        columns.push(column);
    }

    ResultSet::new(columns)
}

/// A fixed-capacity ring of frame durations with percentile readout.
///
/// Percentiles, not a mean: a grid that renders 99 frames in 2ms and one in
/// 200ms has a lovely mean and a visible stutter, and the stutter is the thing
/// worth measuring.
pub struct FrameMeter {
    samples: Vec<f32>,
    next: usize,
    filled: bool,
    since_report: usize,
    label: &'static str,
}

impl FrameMeter {
    pub fn new(label: &'static str, capacity: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            next: 0,
            filled: false,
            since_report: 0,
            label,
        }
    }

    /// True when the meter should be running at all. Timing every frame of an
    /// ordinary session would be harmless but pointless.
    pub fn enabled() -> bool {
        std::env::var_os("TUPLI_FPS").is_some()
    }

    pub fn record(&mut self, millis: f32) {
        if self.samples.len() < self.samples.capacity() {
            self.samples.push(millis);
        } else {
            self.samples[self.next] = millis;
            self.next = (self.next + 1) % self.samples.len();
            self.filled = true;
        }
        self.since_report += 1;
    }

    /// The count of frames recorded since the last report, cleared by `report`.
    pub fn due(&self, every: usize) -> bool {
        self.since_report >= every && self.samples.len() >= every
    }

    pub fn percentile(&self, p: f32) -> f32 {
        if self.samples.is_empty() {
            return 0.;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let i = ((sorted.len() - 1) as f32 * p).round() as usize;
        sorted[i]
    }

    /// Emits one line and resets the report counter (but not the ring, so the
    /// window stays rolling).
    pub fn report(&mut self) -> String {
        self.since_report = 0;
        let n = self.samples.len();
        format!(
            "{}: n={n} p50={:.2}ms p90={:.2}ms p99={:.2}ms max={:.2}ms",
            self.label,
            self.percentile(0.50),
            self.percentile(0.90),
            self.percentile(0.99),
            self.percentile(1.0),
        )
    }

    pub fn filled(&self) -> bool {
        self.filled
    }
}

/// The benchmark's per-grid state: two meters (whole frame, and the element's
/// own paint) plus the auto-scroll direction.
pub struct Bench {
    pub frame: FrameMeter,
    pub paint: FrameMeter,
    pub last: Option<std::time::Instant>,
    pub direction: f32,
}
