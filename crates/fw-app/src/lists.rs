//! The list panels: the media the document uses.

use egui::{Align2, CornerRadius, Rect, Sense, Ui, pos2, vec2};
use fw_engine::MediaSummary;

use crate::icons;
use crate::style::{medium, sans, tokens as t};
use crate::viewer::timecode;

const MEDIA_ROW_HEIGHT: f32 = 48.0;

/// TODO: thumbnails, importing, and editing copies' progress.
pub fn media(ui: &mut Ui, media: &[MediaSummary]) {
    if media.is_empty() {
        empty(ui, "The document uses no media yet");
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(4.0);
            for item in media {
                let (rect, response) = ui.allocate_exact_size(
                    vec2(ui.available_width(), MEDIA_ROW_HEIGHT),
                    Sense::hover(),
                );
                let painter = ui.painter_at(rect);
                if response.hovered() {
                    painter.rect_filled(rect, CornerRadius::ZERO, t::RAISED);
                }
                let thumb = Rect::from_min_size(
                    pos2(rect.left() + 12.0, rect.top() + 9.0),
                    vec2(52.0, 30.0),
                );
                painter.rect_filled(thumb, CornerRadius::same(2), t::WELL);
                let icon = if item.picture.as_ref().is_some_and(|p| p.frames.is_some()) {
                    icons::FILM
                } else {
                    icons::FOLDER
                };
                icons::paint(
                    &painter,
                    Rect::from_center_size(thumb.center(), vec2(14.0, 14.0)),
                    icon,
                    t::TEXT3,
                    1.6,
                    false,
                );
                let name = std::path::Path::new(&item.path)
                    .file_name()
                    .map_or(item.path.clone(), |n| n.to_string_lossy().into_owned());
                let text_left = thumb.right() + 10.0;
                painter.text(
                    pos2(text_left, rect.top() + 17.0),
                    Align2::LEFT_CENTER,
                    name,
                    medium(12.0),
                    t::TEXT,
                );
                let (meta, color) = match (&item.picture, &item.error) {
                    (_, Some(error)) => (error.clone(), t::ACCENT),
                    (Some(p), None) => (describe(item, p), t::TEXT3),
                    (None, None) => ("Sound only".into(), t::TEXT3),
                };
                painter.text(
                    pos2(text_left, rect.bottom() - 15.0),
                    Align2::LEFT_CENTER,
                    meta,
                    sans(11.0),
                    color,
                );
                response.on_hover_text(&item.path);
            }
        });
}

/// `1920×1080 · 23.976 fps · 00:00:10:00 · sound · 2 uses`.
fn describe(item: &MediaSummary, p: &fw_engine::Picture) -> String {
    let mut parts = vec![format!("{}×{}", p.width, p.height)];
    match (p.frames, p.rate) {
        (Some(frames), Some(rate)) => {
            let fps = rate.as_f64();
            parts.push(if fps.fract() == 0.0 {
                format!("{fps} fps")
            } else {
                format!("{fps:.3} fps")
            });
            parts.push(timecode(frames as i64, rate));
        }
        (Some(frames), None) => parts.push(format!("{frames} frames, variable rate")),
        (None, _) => parts.push("Still".into()),
    }
    if item.has_audio {
        parts.push("sound".into());
    }
    if item.users > 1 {
        parts.push(format!("{} uses", item.users));
    }
    parts.join(" · ")
}

fn empty(ui: &mut Ui, text: &str) {
    let rect = ui.max_rect();
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        sans(12.0),
        t::TEXT3,
    );
}
