//! The viewer: the engine's frame drawn with no copy, a scrub bar and the
//! transport.

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use fw_core::Time;
use fw_render::{Display, DisplayView, GpuImage};

use crate::chrome::AppCommand;
use crate::icons;
use crate::style::{mono, mono_medium, sans, tokens as t};

const TRANSPORT_HEIGHT: f32 = 44.0;
const SCRUB_HEIGHT: f32 = 4.0;

/// The engine's frame on screen.
#[derive(Clone)]
pub struct Shown {
    pub image: GpuImage,
    pub serial: u64,
}

/// What the viewer shows besides the frame.
pub struct ViewerState<'a> {
    pub shown: Option<&'a Shown>,
    /// Why the frame due couldn't be rendered.
    pub failure: Option<&'a str>,
    /// The canvas's width over height, for before the first frame arrives.
    pub aspect: f32,
    pub playhead: Time,
    pub duration: Time,
    pub rate: Time,
    pub playing: bool,
}

pub fn viewer(ui: &mut Ui, state: &ViewerState, commands: &mut Vec<AppCommand>) {
    let full = ui.max_rect();
    let well = Rect::from_min_max(
        full.min,
        pos2(
            full.right(),
            full.bottom() - TRANSPORT_HEIGHT - SCRUB_HEIGHT,
        ),
    );
    ui.painter().rect_filled(well, CornerRadius::ZERO, t::WELL);
    let aspect = match state.shown {
        Some(s) => s.image.texture.width() as f32 / s.image.texture.height() as f32,
        None => state.aspect,
    };
    let image = fit(well.shrink(12.0), aspect);
    match state.shown {
        Some(shown) => {
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                image,
                FrameCallback {
                    image: shown.image.clone(),
                    serial: shown.serial,
                },
            ));
        }
        None => {
            ui.painter()
                .rect_filled(image, CornerRadius::ZERO, egui::Color32::BLACK);
        }
    }
    if let Some(failure) = state.failure {
        let text = ui.painter().layout(
            failure.to_owned(),
            sans(12.0),
            t::TEXT,
            (image.width() - 48.0).max(120.0),
        );
        let panel = Rect::from_center_size(image.center(), text.size() + vec2(24.0, 16.0));
        ui.painter()
            .rect_filled(panel, CornerRadius::same(4), t::PANEL.gamma_multiply(0.92));
        ui.painter().rect_stroke(
            panel,
            CornerRadius::same(4),
            Stroke::new(1.0, t::ACCENT),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .galley(panel.min + vec2(12.0, 8.0), text, t::TEXT);
    }

    // The scrub bar: the whole timeline across the viewer's width.
    let scrub = Rect::from_min_size(
        pos2(full.left(), well.bottom()),
        vec2(full.width(), SCRUB_HEIGHT),
    );
    ui.painter().rect_filled(scrub, CornerRadius::ZERO, t::LINE);
    let duration = state.duration.as_f64();
    if duration > 0.0 {
        let response = ui.interact(
            scrub.expand2(vec2(0.0, 5.0)),
            ui.scope_id().with("scrub"),
            Sense::click_and_drag(),
        );
        if let (true, Some(pos)) = (
            response.dragged() || response.clicked(),
            response.interact_pointer_pos(),
        ) {
            let fraction = ((pos.x - scrub.left()) / scrub.width()).clamp(0.0, 1.0) as f64;
            commands.push(AppCommand::Seek(frame_start(
                fraction * duration,
                state.rate,
            )));
        }
        let fraction = (state.playhead.as_f64() / duration).clamp(0.0, 1.0) as f32;
        ui.painter().rect_filled(
            Rect::from_min_size(scrub.min, vec2(scrub.width() * fraction, SCRUB_HEIGHT)),
            CornerRadius::ZERO,
            t::ACCENT.gamma_multiply(0.75),
        );
    }

    // The transport.
    let row = Rect::from_min_size(
        pos2(full.left(), scrub.bottom()),
        vec2(full.width(), TRANSPORT_HEIGHT),
    );
    let index = state.playhead.frame_index(state.rate).unwrap_or(0);
    let last = state.duration.frame_index(state.rate).unwrap_or(0);
    ui.painter().text(
        pos2(row.left() + 12.0, row.center().y),
        Align2::LEFT_CENTER,
        timecode(index, state.rate),
        mono_medium(15.0),
        t::ACCENT,
    );
    ui.painter().text(
        pos2(row.right() - 12.0, row.center().y),
        Align2::RIGHT_CENTER,
        timecode(last, state.rate),
        mono(12.0),
        t::TEXT3,
    );
    let (play, play_tip) = if state.playing {
        (icons::PAUSE, "Pause (Space)")
    } else {
        (icons::PLAY, "Play (Space)")
    };
    let buttons = [
        (icons::SKIP_BACK, "Go to start", Some(AppCommand::GoToStart)),
        (
            icons::STEP_BACK,
            "Previous frame",
            Some(AppCommand::StepBack),
        ),
        (play, play_tip, Some(AppCommand::PlayPause)),
        (icons::STEP_FWD, "Next frame", Some(AppCommand::StepForward)),
        (icons::SKIP_FWD, "Go to end", Some(AppCommand::GoToEnd)),
    ];
    let total = buttons.len() as f32 * 30.0;
    let mut x = row.center().x - total / 2.0;
    for (i, (icon, tip, command)) in buttons.into_iter().enumerate() {
        let b = Rect::from_min_size(pos2(x, row.center().y - 13.0), vec2(26.0, 26.0));
        let response = ui.interact(b, ui.scope_id().with(("transport", i)), Sense::click());
        let enabled = command.is_some();
        if enabled && response.hovered() {
            ui.painter()
                .rect_filled(b, CornerRadius::same(3), t::RAISED2);
        }
        let color = match (enabled, response.hovered()) {
            (false, _) => t::TEXT_DISABLED,
            (true, true) => t::TEXT_BRIGHT,
            (true, false) => t::TEXT2,
        };
        let big = icon == icons::PLAY || icon == icons::PAUSE;
        let size = if big { 17.0 } else { 15.0 };
        let icon_rect = Rect::from_center_size(b.center(), vec2(size, size));
        icons::paint(ui.painter(), icon_rect, icon, color, 1.6, big);
        let response = response.on_hover_text(tip);
        if let (true, Some(command)) = (response.clicked(), command) {
            commands.push(command);
        }
        x += 30.0;
    }
}

/// `aspect` (width over height) fitted inside `area`, centered.
fn fit(area: Rect, aspect: f32) -> Rect {
    if area.width() <= 0.0 || area.height() <= 0.0 || !aspect.is_finite() || aspect <= 0.0 {
        return Rect::from_center_size(area.center(), egui::Vec2::ZERO);
    }
    let size = if area.width() / area.height() > aspect {
        vec2(area.height() * aspect, area.height())
    } else {
        vec2(area.width(), area.width() / aspect)
    };
    Rect::from_center_size(area.center(), size).round()
}

/// The start of the frame `seconds` falls in.
pub fn frame_start(seconds: f64, rate: Time) -> Time {
    let index = (seconds * rate.as_f64()).floor() as i64;
    Time::of_frame(index.max(0), rate).unwrap_or(Time::ZERO)
}

/// `HH:MM:SS:FF` for frame `index`, counting whole frames per second
/// (23.976 counts 24). TODO: drop-frame timecode, and a start timecode.
pub fn timecode(index: i64, rate: Time) -> String {
    let per_second = rate.as_f64().round().max(1.0) as i64;
    let index = index.max(0);
    let (seconds, frames) = (index / per_second, index % per_second);
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        frames
    )
}

/// Draws the frame inside egui's render pass.
struct FrameCallback {
    image: GpuImage,
    serial: u64,
}

impl egui_wgpu::CallbackTrait for FrameCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(display) = resources.get_mut::<Display>() {
            display.prepare(
                device,
                queue,
                &self.image,
                self.serial,
                DisplayView::default(),
            );
        }
        vec![]
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(display) = resources.get::<Display>() {
            display.draw(pass);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecodes_count_whole_frames() {
        let rate: Time = "24000/1001".parse().unwrap();
        assert_eq!(timecode(0, rate), "00:00:00:00");
        assert_eq!(timecode(23, rate), "00:00:00:23");
        assert_eq!(timecode(24, rate), "00:00:01:00");
        assert_eq!(timecode(24 * 3661 + 5, rate), "01:01:01:05");
    }
}
