use crate::ui::StatusBanner;
use gnsh_proto::ConsoleState;

pub(crate) const CANVAS: egui::Color32 = egui::Color32::from_rgb(32, 39, 52);
pub(crate) const CHROME: egui::Color32 = egui::Color32::from_rgb(27, 33, 45);
pub(crate) const SURFACE: egui::Color32 = egui::Color32::from_rgb(41, 50, 68);
pub(crate) const SURFACE_RAISED: egui::Color32 = egui::Color32::from_rgb(54, 65, 90);
pub(crate) const SURFACE_HOVER: egui::Color32 = egui::Color32::from_rgb(66, 79, 109);
pub(crate) const HUD_GLASS: egui::Color32 = egui::Color32::from_rgba_premultiplied(29, 35, 47, 232);

pub(crate) const BORDER_SUBTLE: egui::Color32 = egui::Color32::from_rgb(54, 65, 90);
pub(crate) const BORDER_STRONG: egui::Color32 = egui::Color32::from_rgb(78, 93, 126);
pub(crate) const FOCUS_RING: egui::Color32 = egui::Color32::from_rgb(224, 248, 199);

pub(crate) const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(224, 248, 199);
pub(crate) const TEXT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(198, 216, 184);
pub(crate) const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(163, 180, 168);

pub(crate) const ACCENT: egui::Color32 = egui::Color32::from_rgb(183, 216, 148);
pub(crate) const ACCENT_ACTIVE: egui::Color32 = egui::Color32::from_rgb(224, 248, 199);
pub(crate) const ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(32, 39, 52);

pub(crate) const SUCCESS_BG: egui::Color32 = egui::Color32::from_rgb(40, 61, 50);
pub(crate) const SUCCESS_BORDER: egui::Color32 = egui::Color32::from_rgb(183, 216, 148);
pub(crate) const SUCCESS_FG: egui::Color32 = egui::Color32::from_rgb(224, 248, 199);

pub(crate) const WARN_BG: egui::Color32 = egui::Color32::from_rgb(74, 59, 36);
pub(crate) const WARN_FG: egui::Color32 = egui::Color32::from_rgb(246, 211, 139);

pub(crate) const DANGER_BG: egui::Color32 = egui::Color32::from_rgb(76, 37, 46);
pub(crate) const DANGER_BORDER: egui::Color32 = egui::Color32::from_rgb(201, 91, 103);
pub(crate) const DANGER_FG: egui::Color32 = egui::Color32::from_rgb(252, 168, 176);
pub(crate) const DANGER_SOLID_BG: egui::Color32 = egui::Color32::from_rgb(158, 43, 53);

pub(crate) const INFO_BG: egui::Color32 = egui::Color32::from_rgb(42, 54, 77);
pub(crate) const INFO_BORDER: egui::Color32 = egui::Color32::from_rgb(183, 216, 148);
pub(crate) const INFO_FG: egui::Color32 = egui::Color32::from_rgb(224, 248, 199);

pub(crate) const RADIUS_CARD: u8 = 12;
pub(crate) const RADIUS_CONTROL: u8 = 8;
pub(crate) const RADIUS_PILL: u8 = 10;

pub(crate) fn apply_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = CANVAS;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = CANVAS;
    visuals.faint_bg_color = SURFACE_RAISED;
    visuals.window_corner_radius = egui::CornerRadius::same(RADIUS_CARD);
    visuals.menu_corner_radius = egui::CornerRadius::same(RADIUS_CONTROL);
    visuals.window_stroke = egui::Stroke::new(1.0, BORDER_STRONG);
    visuals.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 18,
        spread: 0,
        color: egui::Color32::from_rgba_unmultiplied(12, 16, 24, 120),
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: egui::Color32::from_rgba_unmultiplied(12, 16, 24, 104),
    };

    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.5, ON_ACCENT);

    visuals.widgets.noninteractive.bg_fill = SURFACE;
    visuals.widgets.noninteractive.weak_bg_fill = SURFACE_RAISED;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(RADIUS_CONTROL);

    visuals.widgets.inactive.bg_fill = SURFACE_RAISED;
    visuals.widgets.inactive.weak_bg_fill = SURFACE_RAISED;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, BORDER_SUBTLE);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(RADIUS_CONTROL);

    visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
    visuals.widgets.hovered.weak_bg_fill = SURFACE_HOVER;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, BORDER_STRONG);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, ACCENT_ACTIVE);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(RADIUS_CONTROL);

    visuals.widgets.active.bg_fill = ACCENT_ACTIVE;
    visuals.widgets.active.weak_bg_fill = ACCENT;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.5, FOCUS_RING);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, ON_ACCENT);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(RADIUS_CONTROL);

    visuals.widgets.open.bg_fill = SURFACE_HOVER;
    visuals.widgets.open.weak_bg_fill = SURFACE_HOVER;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, FOCUS_RING);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.open.corner_radius = egui::CornerRadius::same(RADIUS_CONTROL);

    ctx.set_visuals(visuals);

    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.slider_width = 140.0;

        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(19.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(13.5, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::new(12.5, egui::FontFamily::Monospace),
        );
    });
}

pub(crate) fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(egui::CornerRadius::same(RADIUS_CARD))
        .inner_margin(egui::Margin::symmetric(16, 14))
}

pub(crate) fn nested_surface_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE_RAISED)
        .stroke(egui::Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL))
        .inner_margin(egui::Margin::symmetric(12, 8))
}

pub(crate) fn hud_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(HUD_GLASS)
        .stroke(egui::Stroke::new(1.0, BORDER_STRONG))
        .corner_radius(egui::CornerRadius::same(RADIUS_CARD))
        .inner_margin(egui::Margin::symmetric(12, 8))
}

pub(crate) fn primary_button(label: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).color(ON_ACCENT).strong())
        .fill(ACCENT)
        .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL))
}

pub(crate) fn emerald_button(label: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).color(SUCCESS_FG).strong())
        .fill(SUCCESS_BG)
        .stroke(egui::Stroke::new(1.0, SUCCESS_BORDER))
        .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL))
}

pub(crate) fn secondary_button(label: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).color(TEXT_PRIMARY))
        .fill(SURFACE_RAISED)
        .stroke(egui::Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL))
}

pub(crate) fn subtle_danger_button(label: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).color(DANGER_FG))
        .fill(DANGER_BG)
        .stroke(egui::Stroke::new(1.0, DANGER_BORDER))
        .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL))
}

pub(crate) fn small_pill_button(
    label: &str,
    bg: egui::Color32,
    fg: egui::Color32,
    stroke: Option<egui::Color32>,
) -> egui::Button<'static> {
    let mut btn = egui::Button::new(egui::RichText::new(label).small().strong().color(fg))
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL));
    if let Some(border) = stroke {
        btn = btn.stroke(egui::Stroke::new(1.0, border));
    }
    btn
}

pub(crate) fn status_pill(ui: &mut egui::Ui, bg: egui::Color32, fg: egui::Color32, text: &str) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).small().strong().color(fg));
        });
}

pub(crate) fn state_badge(ui: &mut egui::Ui, state: ConsoleState) {
    let (bg, fg, label) = match state {
        ConsoleState::Ready => (SUCCESS_BG, SUCCESS_FG, "Ready"),
        ConsoleState::Standby => (WARN_BG, WARN_FG, "Standby"),
        ConsoleState::Unknown => (SURFACE_RAISED, TEXT_SECONDARY, "Unknown"),
    };
    status_pill(ui, bg, fg, label);
}

pub(crate) fn render_status_banner(ui: &mut egui::Ui, banner: &StatusBanner) -> bool {
    let (bg, border, fg) = match banner.kind {
        super::BannerKind::Info => (INFO_BG, INFO_BORDER, INFO_FG),
        super::BannerKind::Success => (SUCCESS_BG, SUCCESS_BORDER, SUCCESS_FG),
        super::BannerKind::Error => (DANGER_BG, DANGER_BORDER, DANGER_FG),
    };
    let mut dismissed = false;
    egui::Frame::new()
        .fill(bg)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(RADIUS_CARD))
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&banner.text).color(fg));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("Dismiss").small().color(fg))
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0, border))
                                .corner_radius(egui::CornerRadius::same(RADIUS_CONTROL)),
                        )
                        .clicked()
                    {
                        dismissed = true;
                    }
                });
            });
        });
    dismissed
}

#[cfg(test)]
fn channel_lum(c: u8) -> f64 {
    let s = f64::from(c) / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
pub(crate) fn contrast_ratio(a: egui::Color32, b: egui::Color32) -> f64 {
    let l1 =
        0.2126 * channel_lum(a.r()) + 0.7152 * channel_lum(a.g()) + 0.0722 * channel_lum(a.b());
    let l2 =
        0.2126 * channel_lum(b.r()) + 0.7152 * channel_lum(b.g()) + 0.0722 * channel_lum(b.b());
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_tokens_meet_contrast_and_concentric_radius_rules() {
        assert!(contrast_ratio(TEXT_PRIMARY, SURFACE) >= 4.5);
        assert!(contrast_ratio(TEXT_PRIMARY, SURFACE_RAISED) >= 4.5);
        assert!(contrast_ratio(TEXT_PRIMARY, CANVAS) >= 4.5);
        assert!(contrast_ratio(TEXT_SECONDARY, SURFACE) >= 4.5);
        assert!(contrast_ratio(TEXT_MUTED, CANVAS) >= 4.5);
        assert!(contrast_ratio(TEXT_MUTED, SURFACE) >= 4.5);
        assert!(contrast_ratio(ON_ACCENT, ACCENT) >= 4.5);
        assert!(contrast_ratio(ON_ACCENT, ACCENT_ACTIVE) >= 4.5);
        assert!(contrast_ratio(SUCCESS_FG, SUCCESS_BG) >= 4.5);
        assert!(contrast_ratio(INFO_FG, INFO_BG) >= 4.5);
        assert!(contrast_ratio(WARN_FG, WARN_BG) >= 4.5);
        assert!(contrast_ratio(DANGER_FG, DANGER_BG) >= 4.5);
        const {
            assert!(RADIUS_CARD > RADIUS_CONTROL);
            assert!(RADIUS_CARD >= RADIUS_PILL);
        }
    }
}
