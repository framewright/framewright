//! The one control vocabulary every panel uses (design §3.4): a label, a
//! slider with a thin handle, a monospaced value field (drag to scrub,
//! click to type) and a keyframe diamond.

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use fw_plugin::Unit;

use crate::style::{mono, sans, tokens as t};

pub const ROW_HEIGHT: f32 = 26.0;
const LABEL_WIDTH: f32 = 92.0;
const FIELD_WIDTH: f32 = 64.0;
/// Dragging a value field across this many points covers the slider's range.
const SCRUB_POINTS: f64 = 300.0;

/// Numbers the user drags or types, one gesture per drag so a drag undoes
/// in one step.
#[derive(Default)]
pub struct Gestures {
    next: u64,
    active: Option<(egui::Id, u64)>,
}

impl Gestures {
    /// The gesture a drag of `widget` belongs to.
    pub fn drag(&mut self, widget: egui::Id) -> u64 {
        match self.active {
            Some((id, gesture)) if id == widget => gesture,
            _ => {
                self.next += 1;
                self.active = Some((widget, self.next));
                self.next
            }
        }
    }

    /// The drag of `widget` ended.
    pub fn end(&mut self, widget: egui::Id) {
        if self.active.map(|a| a.0) == Some(widget) {
            self.active = None;
        }
    }
}

/// The value field being typed into, if any.
#[derive(Default)]
pub struct Typing {
    field: Option<(egui::Id, String)>,
    focus: bool,
}

/// How a number is shown and limited.
#[derive(Debug, Clone, Copy)]
pub struct Number {
    pub unit: Unit,
    /// What the slider covers; no slider without it.
    pub slider: Option<(f64, f64)>,
    /// Values are clamped to this.
    pub limits: Option<(f64, f64)>,
}

impl Number {
    fn decimals(&self) -> usize {
        match self.unit {
            Unit::Percent | Unit::Decibels | Unit::Degrees | Unit::Pixels => 1,
            Unit::Stops => 2,
            Unit::Seconds | Unit::Ratio | Unit::None => 3,
        }
    }

    fn suffix(&self) -> &'static str {
        match self.unit {
            Unit::Percent => "%",
            Unit::Decibels => " dB",
            Unit::Degrees => "°",
            Unit::Pixels => " px",
            Unit::Stops => " EV",
            Unit::Seconds => " s",
            Unit::Ratio => "×",
            Unit::None => "",
        }
    }

    pub fn show(&self, v: f64) -> String {
        format!("{:.*}{}", self.decimals(), v, self.suffix())
    }

    fn parse(&self, text: &str) -> Option<f64> {
        let text = text.trim().trim_end_matches(self.suffix().trim()).trim();
        text.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| self.clamp(v))
    }

    fn clamp(&self, v: f64) -> f64 {
        match self.limits {
            Some((min, max)) => v.clamp(min, max),
            None => v,
        }
    }
}

/// What the user did to a row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    /// A new value, part of `gesture` while dragging.
    Set { value: f64, gesture: Option<u64> },
    /// Back to the default (double-clicking the label).
    Reset,
}

/// A number row: label, slider, value field and keyframe diamond.
#[allow(clippy::too_many_arguments)]
pub fn number_row(
    ui: &mut Ui,
    id: egui::Id,
    label: &str,
    value: f64,
    number: Number,
    enabled: bool,
    typing: &mut Typing,
    gestures: &mut Gestures,
) -> Option<Change> {
    let (rect, inner, mut change) = row_frame(ui, id, label);
    let cy = rect.center().y;
    let diamond = keyframe_diamond(ui, id, pos2(inner.right() - 4.0, cy));
    let field = Rect::from_min_size(
        pos2(diamond.left() - 8.0 - FIELD_WIDTH, cy - 10.0),
        vec2(FIELD_WIDTH, 20.0),
    );
    let painter = ui.painter_at(rect);

    if let Some((min, max)) = number.slider.filter(|(a, b)| b > a) {
        let track = Rect::from_min_max(
            pos2(inner.left() + LABEL_WIDTH + 8.0, cy - 1.5),
            pos2(field.left() - 10.0, cy + 1.5),
        );
        let sense = if enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        };
        let slider_id = id.with("slider");
        let response = ui.interact(track.expand2(vec2(5.0, 8.0)), slider_id, sense);
        if (response.dragged() || response.clicked())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let f = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64;
            let gesture = response.dragged().then(|| gestures.drag(slider_id));
            change = Some(Change::Set {
                value: number.clamp(min + f * (max - min)),
                gesture,
            });
        }
        if response.drag_stopped() {
            gestures.end(slider_id);
        }
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, enabled, label));
        let fraction = ((value - min) / (max - min)).clamp(0.0, 1.0) as f32;
        let x = track.left() + track.width() * fraction;
        painter.rect_filled(track, CornerRadius::same(2), t::TRACK);
        painter.rect_filled(
            Rect::from_min_max(track.min, pos2(x, track.max.y)),
            CornerRadius::same(2),
            if enabled { t::TRACK_FILL } else { t::TRACK },
        );
        let handle = if !enabled {
            t::TRACK_FILL
        } else if response.hovered() || response.dragged() {
            t::TEXT_BRIGHT
        } else {
            t::HANDLE
        };
        painter.rect_filled(
            Rect::from_min_size(pos2(x - 1.5, cy - 5.5), vec2(3.0, 11.0)),
            CornerRadius::same(1),
            handle,
        );
    }

    let field_id = id.with("field");
    if let Some((_, text)) = typing.field.as_mut().filter(|(f, _)| *f == field_id) {
        let edit = egui::TextEdit::singleline(text)
            .font(mono(11.0))
            .align(egui::Align2::RIGHT_CENTER)
            .margin(egui::Margin::symmetric(6, 2))
            .frame(
                egui::Frame::NONE
                    .fill(t::FIELD)
                    .stroke(Stroke::new(1.0, t::ACCENT))
                    .corner_radius(3),
            );
        let response = ui.put(field, edit);
        if typing.focus {
            response.request_focus();
            // Everything selected, so typing replaces the value.
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), response.id) {
                let end = egui::text::CCursor::new(text.chars().count());
                let all = egui::text_selection::CCursorRange::two(egui::text::CCursor::new(0), end);
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), response.id);
            }
            typing.focus = false;
        }
        let (enter, escape) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if response.lost_focus() || enter || escape {
            if !escape && let Some(v) = number.parse(text) {
                change = Some(Change::Set {
                    value: v,
                    gesture: None,
                });
            }
            typing.field = None;
        }
    } else {
        let sense = if enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        };
        let response = ui.interact(field, field_id, sense);
        if response.dragged() {
            let (min, max) = number.slider.unwrap_or((value - 1.0, value + 1.0));
            let fine = if ui.input(|i| i.modifiers.shift) {
                0.1
            } else {
                1.0
            };
            let delta = response.drag_delta().x as f64 * (max - min) / SCRUB_POINTS * fine;
            if delta != 0.0 {
                change = Some(Change::Set {
                    value: number.clamp(value + delta),
                    gesture: Some(gestures.drag(field_id)),
                });
            }
        }
        if response.drag_stopped() {
            gestures.end(field_id);
        }
        if response.clicked() {
            typing.field = Some((field_id, format!("{:.*}", number.decimals(), value)));
            typing.focus = true;
        }
        let border = if enabled && (response.hovered() || response.dragged()) {
            t::LINE2
        } else {
            t::FIELD_BORDER
        };
        painter.rect(
            field,
            CornerRadius::same(3),
            t::FIELD,
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let colour = if enabled { t::TEXT } else { t::TEXT3 };
        painter.text(
            pos2(field.right() - 6.0, cy),
            Align2::RIGHT_CENTER,
            number.show(value),
            mono(11.0),
            colour,
        );
        let shown = format!("{label} {}", number.show(value));
        let response = if enabled {
            response.on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
        } else {
            response
        };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, enabled, &shown));
    }
    if !enabled && matches!(change, Some(Change::Set { .. })) {
        change = None;
    }
    change
}

/// A row whose control is whatever `control` draws in the space right of
/// the label. Returns `Change::Reset` when the label is double-clicked.
pub fn custom_row(
    ui: &mut Ui,
    id: egui::Id,
    label: &str,
    control: impl FnOnce(&mut Ui),
) -> Option<Change> {
    let (rect, inner, change) = row_frame(ui, id, label);
    keyframe_diamond(ui, id, pos2(inner.right() - 4.0, rect.center().y));
    let area = Rect::from_min_max(
        pos2(inner.left() + LABEL_WIDTH + 8.0, rect.top() + 2.0),
        pos2(inner.right() - 20.0, rect.bottom() - 2.0),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(area)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    control(&mut child);
    change
}

/// Allocates a row and draws its label. Returns the row, its padded inside,
/// and `Change::Reset` if the label was double-clicked.
fn row_frame(ui: &mut Ui, id: egui::Id, label: &str) -> (Rect, Rect, Option<Change>) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());
    let inner = rect.shrink2(vec2(12.0, 0.0));
    let label_rect = Rect::from_min_size(
        pos2(inner.left(), rect.top()),
        vec2(LABEL_WIDTH, ROW_HEIGHT),
    );
    let response = ui
        .interact(label_rect, id.with("label"), Sense::click())
        .on_hover_text("Double-click to reset");
    let colour = if response.hovered() {
        t::TEXT
    } else {
        t::TEXT2
    };
    ui.painter_at(label_rect).text(
        pos2(inner.left(), rect.center().y),
        Align2::LEFT_CENTER,
        label,
        sans(12.0),
        colour,
    );
    let change = response.double_clicked().then_some(Change::Reset);
    (rect, inner, change)
}

/// TODO: keyframing from the inspector. Drawn so the row reads as designed.
fn keyframe_diamond(ui: &mut Ui, id: egui::Id, centre: egui::Pos2) -> Rect {
    let rect = Rect::from_center_size(centre, vec2(14.0, 14.0));
    let response = ui.interact(rect, id.with("keyframe"), Sense::hover());
    response.on_hover_text("Keyframing from here isn't built yet");
    let r = 3.6;
    let points = vec![
        centre + vec2(0.0, -r),
        centre + vec2(r, 0.0),
        centre + vec2(0.0, r),
        centre + vec2(-r, 0.0),
    ];
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        Color32::TRANSPARENT,
        Stroke::new(1.0, t::TRACK_FILL),
    ));
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_show_and_parse_with_units() {
        let opacity = Number {
            unit: Unit::Percent,
            slider: Some((0.0, 100.0)),
            limits: Some((0.0, 100.0)),
        };
        assert_eq!(opacity.show(62.5), "62.5%");
        assert_eq!(opacity.parse("40%"), Some(40.0));
        assert_eq!(opacity.parse(" 140 "), Some(100.0), "clamped to the limits");
        assert_eq!(opacity.parse("lots"), None);
        let gain = Number {
            unit: Unit::Decibels,
            slider: None,
            limits: None,
        };
        assert_eq!(gain.show(-6.0), "-6.0 dB");
        assert_eq!(gain.parse("-3 dB"), Some(-3.0));
    }
}
