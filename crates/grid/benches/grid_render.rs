//! The M0 performance gate: a real grid, in a real window, at real sizes.
//!
//! Run with `cargo bench -p grid`. This is a headless benchmark — it shapes
//! text with the platform text system and submits scenes to Metal, but never
//! puts a window on screen, so it produces the same numbers on a machine whose
//! display is asleep as on one being watched.
//!
//! The number that matters is the frame budget line printed by GPUI's report:
//! docs/PLAN.md §16 requires p99 under 8ms at 1M rows × 100 columns.

use gpui::BenchAppContext;
use grid::Grid;
use ui::{Appearance, Theme};

/// The shapes worth measuring. The first is an ordinary query result; the last
/// is the gate. Both matter: a grid that is fast only when it is small is a
/// grid that has cheated somewhere, and one that is slow when small is one
/// nobody will use.
fn sizes() -> Vec<Shape> {
    vec![
        Shape {
            rows: 1_000,
            cols: 8,
        },
        Shape {
            rows: 100_000,
            cols: 8,
        },
        Shape {
            rows: 1_000_000,
            cols: 8,
        },
        Shape {
            rows: 1_000_000,
            cols: 100,
        },
    ]
}

#[derive(Clone, Copy)]
struct Shape {
    rows: usize,
    cols: usize,
}

impl std::fmt::Display for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.rows, self.cols)
    }
}

#[gpui::bench(
    inputs = sizes(),
    group = "Grid render",
    input_name = "rows x cols",
    sample_size = 10
)]
fn grid_render(size: &Shape, cx: &mut BenchAppContext) {
    let Shape { rows, cols } = *size;

    cx.update(|cx| Theme::set_global(Theme::of(Appearance::Dark), cx));

    let data = grid::bench::synthetic(rows, cols);
    let mut window = cx.add_empty_window();
    let view =
        window.update(|window, cx| window.replace_root(cx, |_window, cx| Grid::new(data, cx)));

    // Scroll every frame. A grid repainting the same rows forever hits the
    // line-layout cache on every cell and reports a number that has nothing to
    // do with scrolling a million rows.
    let mut tick = 0u32;
    cx.bench_renderer(view, move |grid, _window, cx| {
        tick = tick.wrapping_add(1);
        grid.scroll_for_bench(tick, cx);
        cx.notify();
    });
}

gpui::bench_group!(benches, grid_render);
gpui::bench_main!(benches);
