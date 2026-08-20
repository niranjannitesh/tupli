//! A label that keeps the end of a name when the row is too narrow for it.
//!
//! The schema tree is full of names written to a convention:
//! `conversation_annotations` beside `conversation_interactions` beside
//! `conversation_messages`. Elided at the end they all become
//! `conversation_a…`, and a tree that shows the same row four times is not
//! showing four tables. The text system's own middle truncation spends two
//! thirds of the room on the head, which for these names is two thirds spent on
//! the part that says nothing.
//!
//! So this is an [`Element`] rather than a styled `div`: the decision needs the
//! width the row actually got, which nothing knows until layout has run.
//! `prepaint` measures, picks the text, and shapes it; `paint` draws that one
//! line. The rule is:
//!
//! * it fits — draw the name, no ellipsis anywhere;
//! * it does not — keep the whole last word, spend what is left on the start of
//!   the name, and put one `…` between them: `conver…notations`;
//! * the last word alone does not fit — drop the head entirely and elide the
//!   word's own start: `…notations`.

use gpui::{
    px, App, Bounds, Element, ElementId, GlobalElementId, Font, InspectorElementId, IntoElement,
    LayoutId, Length, Pixels, Refineable, SharedString, Style, StyleRefinement, Styled, TextAlign,
    TextRun, Window,
};

use crate::{ActiveTheme, IconColor, LabelSize};

/// One `…`. Written out rather than `...`, which is three glyphs and looks it.
const ELLIPSIS: &str = "…";

#[derive(Clone)]
pub struct ElidedLabel {
    text: SharedString,
    size: LabelSize,
    color: IconColor,
    mono: bool,
    style: StyleRefinement,
}

impl ElidedLabel {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            size: LabelSize::default(),
            color: IconColor::Default,
            mono: false,
            style: StyleRefinement::default(),
        }
    }

    pub fn size(mut self, size: LabelSize) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: IconColor) -> Self {
        self.color = color;
        self
    }

    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    pub fn when_mono(self, mono: bool) -> Self {
        match mono {
            true => self.mono(),
            false => self,
        }
    }

    /// Font, size and line height for this label's step of the type ramp.
    /// Same lookup `Label` does, so the two render identically.
    fn ramp(&self, cx: &App) -> (Font, Pixels, Pixels) {
        let ty = cx.typography();
        let (size, line_height) = match self.size {
            LabelSize::Small => (ty.ui_size_sm, ty.ui_line_height_sm),
            LabelSize::Default => (ty.ui_size, ty.ui_line_height),
            LabelSize::Large => (ty.ui_size_lg, ty.ui_line_height_lg),
            LabelSize::Code => (ty.mono_size, ty.mono_line_height),
        };
        let font = match self.mono {
            true => ty.mono_font(),
            false => gpui::font(ty.ui_family.clone()),
        };
        (font, size, line_height)
    }
}

impl Styled for ElidedLabel {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// Where the name is cut when it has to be cut: after the last underscore,
/// because the convention that wrote these names put the distinguishing word
/// there. A name with no underscore is cut in the middle, which is the best
/// guess left.
fn cut(text: &str) -> usize {
    text.rfind('_')
        .map(|ix| ix + 1)
        .filter(|ix| *ix < text.len())
        .unwrap_or_else(|| {
            let mid = text.len() / 2;
            (mid..text.len())
                .find(|ix| text.is_char_boundary(*ix))
                .unwrap_or(text.len())
        })
}

/// The text to actually draw in `room` pixels.
///
/// `width` measures a substring the way the text system would. Kept separate
/// from the element so the rule can be tested without a window, and so reading
/// it does not mean reading three `Element` methods first.
fn fit(text: &str, room: f32, width: &dyn Fn(&str) -> f32) -> String {
    if width(text) <= room {
        return text.to_string();
    }
    let (head, tail) = text.split_at(cut(text));
    let ellipsis = width(ELLIPSIS);
    if width(tail) + ellipsis >= room {
        // No room for the last word either, so nothing of the head would
        // survive anyway. What is left of the word is worth more than the same
        // number of letters from the front of the name.
        let from = tail
            .char_indices()
            .map(|(ix, _)| ix)
            .find(|ix| width(&tail[*ix..]) + ellipsis <= room)
            .unwrap_or(tail.len());
        return format!("{ELLIPSIS}{}", &tail[from..]);
    }
    // Whatever the last word leaves over, spent on the start of the name.
    let budget = room - width(tail) - ellipsis;
    let end = head
        .char_indices()
        .map(|(ix, _)| ix)
        .chain(std::iter::once(head.len()))
        .take_while(|ix| width(&head[..*ix]) <= budget)
        .last()
        .unwrap_or(0);
    // The ellipsis stands in for the separator as well as for the letters:
    // `schema…migrations` rather than `schema_…migrations`, which reads as a
    // word with a hole in it rather than as a word cut in half.
    format!("{}{ELLIPSIS}{tail}", head[..end].trim_end_matches('_'))
}

impl IntoElement for ElidedLabel {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct Prepainted {
    line: gpui::ShapedLine,
    line_height: Pixels,
}

impl Element for ElidedLabel {
    type RequestLayoutState = ();
    type PrepaintState = Option<Prepainted>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let (_, _, line_height) = self.ramp(cx);
        // Takes the row's leftovers and never asks for more: the name is what
        // adapts to the panel, not the other way round.
        let mut style = Style {
            flex_grow: 1.,
            flex_shrink: 1.,
            flex_basis: Length::Definite(px(0.).into()),
            ..Default::default()
        };
        style.size.height = line_height.into();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Prepainted> {
        let (font, font_size, line_height) = self.ramp(cx);
        let color = self.color.resolve(cx).unwrap_or(cx.colors().text);
        let run = |len: usize| TextRun {
            len,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // `layout_line` caches by (text, font, size), so measuring a dozen
        // prefixes of one name costs a dozen hash lookups after the first
        // frame rather than a dozen shaping passes.
        let text_system = window.text_system().clone();
        let width_of = |text: &str| {
            text_system
                .layout_line(text, font_size, &[run(text.len())], None)
                .width
        };

        let text: SharedString = fit(
            &self.text,
            bounds.size.width.into(),
            &|part| width_of(part).into(),
        )
        .into();

        let line = window
            .text_system()
            .shape_line(text.clone(), font_size, &[run(text.len())], None);
        Some(Prepainted { line, line_height })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut (),
        prepaint: &mut Option<Prepainted>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(state) = prepaint else { return };
        // Centred in the row the same way a `div`'s line box centres its text,
        // so a name lines up with the icon beside it.
        let origin = gpui::point(
            bounds.origin.x,
            bounds.origin.y + (bounds.size.height - state.line_height) / 2.,
        );
        let _ = state
            .line
            .paint(origin, state.line_height, TextAlign::Left, None, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monospace stand-in: every character one unit wide, so a test can say
    /// "eighteen characters of room" and mean it.
    fn width(text: &str) -> f32 {
        text.chars().count() as f32
    }

    fn fit_in(text: &str, room: usize) -> String {
        fit(text, room as f32, &width)
    }

    #[test]
    fn a_name_that_fits_is_left_alone() {
        assert_eq!(fit_in("conversations", 20), "conversations");
        // Exactly the room it needs is still fitting.
        assert_eq!(fit_in("conversations", 13), "conversations");
    }

    #[test]
    fn the_last_word_is_what_survives_a_narrow_panel() {
        // One ellipsis, the whole distinguishing word, and as much of the
        // head as the leftovers pay for.
        assert_eq!(fit_in("conversation_annotations", 18), "conver…annotations");
        // `kysely_` is where the budget ran out, and the underscore it ends
        // on goes: the ellipsis is already saying a separator was here.
        assert_eq!(fit_in("kysely_migration_lock", 12), "kysely…lock");
    }

    #[test]
    fn sibling_names_stay_apart() {
        // The whole point: four tables written to one convention have to read
        // as four rows.
        let room = 18;
        let names = [
            "conversation_annotations",
            "conversation_interactions",
            "conversation_messages",
            "conversation_sessions",
        ];
        let fitted: Vec<_> = names.iter().map(|n| fit_in(n, room)).collect();
        let unique: std::collections::HashSet<_> = fitted.iter().collect();
        assert_eq!(unique.len(), names.len(), "{fitted:?}");
    }

    #[test]
    fn a_last_word_too_long_for_the_row_elides_its_own_start() {
        // No head can survive, so spending an ellipsis on the join would only
        // cost another letter of the word.
        assert_eq!(fit_in("public_transactions", 8), "…actions");
        assert_eq!(fit_in("public_transactions", 1), "…");
    }

    #[test]
    fn a_name_without_underscores_gives_up_its_middle() {
        // Halved, then the head spends what the tail left it.
        assert_eq!(fit_in("abcdefghijkl", 8), "a…ghijkl");
    }

    #[test]
    fn no_room_at_all_is_not_a_panic() {
        assert_eq!(fit_in("conversation_annotations", 0), "…");
        assert_eq!(fit_in("", 0), "");
    }

    #[test]
    fn a_trailing_underscore_is_not_an_empty_last_word() {
        // `cut` refuses a cut at the very end, so this falls back to halving
        // and still has something to keep.
        let fitted = fit_in("some_table_name_", 10);
        assert!(fitted.contains('…'), "{fitted}");
        assert!(fitted.ends_with("name_"), "{fitted}");
    }

    #[test]
    fn multibyte_names_are_cut_on_character_boundaries() {
        // The halving path indexes by byte; landing mid-character would panic.
        let fitted = fit_in("ααααβββββ", 5);
        assert!(fitted.contains('…'), "{fitted}");
    }
}
