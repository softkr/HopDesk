//! Connection detail view: route diagram doubling as progress indicator, primary action, details.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Sense, Stroke, Ui, Vec2};

use super::sessions::{FailureInfo, State};
use super::theme::{Bold as _, HEADING_SIZE, Palette, TITLE_SIZE, bold_family, palette};
use super::widgets::{self, Icon, card};
use crate::profile::{DisplayMode, Profile};

pub enum Action {
    None,
    Connect,
    Cancel,
    Dismiss,
    Edit,
    Delete,
    ForgetKeys,
}

pub fn ui(ui: &mut Ui, profile: &Profile, state: Option<&State>, log: &[String]) -> Action {
    let pal = palette(ui);
    let mut action = Action::None;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(20.0);
        header(ui, &pal, profile, state, &mut action);
        ui.add_space(16.0);

        card(ui, |ui| {
            route(ui, &pal, profile, state);
            ui.add_space(8.0);
            status_area(ui, &pal, state, &mut action);
        });
        ui.add_space(12.0);

        card(ui, |ui| {
            widgets::section_title(ui, "연결 정보", None);
            egui::Grid::new("details").num_columns(2).min_row_height(20.0).spacing([28.0, 8.0]).show(ui, |ui| {
                let j = &profile.jump;
                if profile.use_jump {
                    widgets::info_row(ui, &pal, "점프 호스트", &format!("{}@{}:{}", j.user, j.host, j.port));
                    let auth = match &j.key_path {
                        Some(path) => format!("개인 키 · {}", tilde(path)),
                        None => "비밀번호".to_owned(),
                    };
                    widgets::info_row(ui, &pal, "SSH 인증", &auth);
                } else {
                    widgets::info_row(ui, &pal, "점프 호스트", "사용 안 함 (직접 접속)");
                }
                let t = &profile.target;
                widgets::info_row(ui, &pal, "원격 PC", &format!("{}:{}", t.host, t.port));
                widgets::info_row(ui, &pal, "Windows 계정", &account(profile));
                let resolution = match profile.display {
                    DisplayMode::Fixed => format!("{} × {}", profile.width, profile.height),
                    mode => mode.label().to_owned(),
                };
                widgets::info_row(ui, &pal, "해상도", &resolution);
            });
        });

        if !log.is_empty() {
            ui.add_space(12.0);
            card(ui, |ui| {
                egui::CollapsingHeader::new(RichText::new("연결 로그").bold())
                    .default_open(matches!(state, Some(State::Failed(_))))
                    .show(ui, |ui| {
                        let text = log.join("\n");
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .stick_to_bottom(true)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.label(RichText::new(&text).monospace().color(pal.text_weak));
                            });
                        if ui.button("로그 복사").clicked() {
                            ui.ctx().copy_text(text);
                        }
                    });
            });
        }
        ui.add_space(20.0);
    });

    action
}

fn header(ui: &mut Ui, pal: &Palette, profile: &Profile, state: Option<&State>, action: &mut Action) {
    ui.horizontal(|ui| {
        widgets::avatar(ui, 44.0, &profile.name);
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(egui::Label::new(RichText::new(&profile.name).size(HEADING_SIZE).bold()).truncate());
            ui.label(RichText::new(format!("{} · {}", account(profile), profile.target.host)).color(pal.text_weak));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let busy = state.is_some_and(State::is_active);
            ui.menu_button(RichText::new("···").size(16.0).bold(), |ui| {
                ui.set_min_width(180.0);
                if ui.button("호스트 키 초기화...").clicked() {
                    *action = Action::ForgetKeys;
                    ui.close();
                }
                ui.separator();
                let delete = egui::Button::new(RichText::new("연결 삭제...").color(pal.danger));
                if ui
                    .add_enabled(!busy, delete)
                    .on_disabled_hover_text("연결 중에는 삭제할 수 없습니다")
                    .clicked()
                {
                    *action = Action::Delete;
                    ui.close();
                }
            })
            .response
            .on_hover_text("더 보기");
            if ui.button("편집").on_hover_text(super::platform::shortcut("E")).clicked() {
                *action = Action::Edit;
            }
        });
    });
}

#[derive(Clone, Copy, PartialEq)]
enum Node {
    Idle,
    Done,
    Active,
    Error,
}

fn route(ui: &mut Ui, pal: &Palette, profile: &Profile, state: Option<&State>) {
    use Node::*;
    let (mut nodes, mut links) = match state {
        None => ([Idle, Idle, Idle], [Idle, Idle]),
        Some(State::Starting | State::Ssh) => ([Done, Active, Idle], [Active, Idle]),
        Some(State::Rdp) => ([Done, Done, Active], [Done, Active]),
        Some(State::Connected) => ([Done, Done, Done], [Done, Done]),
        Some(State::Reconnecting) => ([Done, Done, Active], [Done, Active]),
        Some(State::Failed(f)) if f.at_remote => ([Done, Done, Error], [Done, Error]),
        Some(State::Failed(_)) => ([Done, Error, Idle], [Error, Idle]),
    };

    let jump_auth = if profile.jump.key_path.is_some() { "개인 키" } else { "비밀번호" };
    let mut labels = vec![
        (Icon::Laptop, super::platform::THIS_DEVICE.to_owned(), "로컬".to_owned()),
        (Icon::Gateway, "점프 호스트".to_owned(), format!("{} · {jump_auth}", profile.jump.host)),
        (Icon::Monitor, "원격 PC".to_owned(), format!("{}:{}", profile.target.host, profile.target.port)),
    ];
    let mut positions = vec![0.16, 0.5, 0.84];
    if !profile.use_jump {
        // Direct connection: this device -> remote PC. A failure before RDP is the remote PC's.
        labels.remove(1);
        positions = vec![0.25, 0.75];
        let remote = match nodes[2] {
            Idle if nodes[1] == Error => Error,
            Idle if nodes[1] == Active => Active,
            other => other,
        };
        let link = if links[0] == Error || links[1] == Error {
            Error
        } else if links[1] == Idle {
            links[0]
        } else {
            links[1]
        };
        nodes = [nodes[0], remote, Idle];
        links = [link, Idle];
    }
    let node_count = labels.len();

    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 88.0), Sense::hover());
    let painter = ui.painter_at(rect.expand(4.0));
    let r = 22.0;
    let cy = rect.top() + r + 4.0;
    let xs: Vec<f32> = positions.iter().map(|f| rect.left() + rect.width() * f).collect();
    let color = |n: Node| match n {
        Idle => pal.border,
        Done => pal.success,
        Active => pal.accent,
        Error => pal.danger,
    };
    let time = ui.input(|i| i.time);
    let mut animating = false;

    for (i, link) in links.iter().take(node_count - 1).enumerate() {
        let a = Pos2::new(xs[i] + r + 6.0, cy);
        let b = Pos2::new(xs[i + 1] - r - 6.0, cy);
        let c = color(*link);
        match link {
            Active => {
                // Dashes flowing toward the destination.
                animating = true;
                let dash = 8.0;
                let gap = 6.0;
                let offset = ((time * 40.0) as f32) % (dash + gap);
                let mut x = a.x - (dash + gap) + offset;
                while x < b.x {
                    let x0 = x.max(a.x);
                    let x1 = (x + dash).min(b.x);
                    if x1 > x0 {
                        painter.line_segment([Pos2::new(x0, cy), Pos2::new(x1, cy)], Stroke::new(2.5, c));
                    }
                    x += dash + gap;
                }
            }
            Idle => {
                painter.extend(egui::Shape::dashed_line(&[a, b], Stroke::new(2.0, c), 4.0, 5.0));
            }
            _ => {
                painter.line_segment([a, b], Stroke::new(2.5, c));
            }
        }
        // Arrow head.
        painter.line_segment([b, b + Vec2::new(-6.0, -5.0)], Stroke::new(2.0, c));
        painter.line_segment([b, b + Vec2::new(-6.0, 5.0)], Stroke::new(2.0, c));
    }

    for (i, node) in nodes.iter().take(node_count).enumerate() {
        let center = Pos2::new(xs[i], cy);
        let c = color(*node);
        let (fill, ring) = match node {
            Idle => (pal.subtle, pal.border),
            _ => (c.gamma_multiply(0.15), c),
        };
        if *node == Active {
            animating = true;
            let pulse = ((time * 3.0).sin() as f32 + 1.0) / 2.0;
            painter.circle_stroke(center, r + 3.0 + pulse * 4.0, Stroke::new(2.0, c.gamma_multiply(0.35 * (1.0 - pulse))));
        }
        painter.circle(center, r, fill, Stroke::new(2.0, ring));
        let (icon, title, sub) = &labels[i];
        let icon_color = if *node == Idle { pal.text_weak } else { c };
        widgets::paint_icon(&painter, *icon, center, 26.0, icon_color);
        if matches!(node, Done | Error) {
            let bc = center + Vec2::new(r * 0.72, -r * 0.72);
            painter.circle(bc, 8.0, c, Stroke::new(2.0, pal.surface));
            if *node == Done {
                widgets::paint_icon(&painter, Icon::Check, bc, 11.0, Color32::WHITE);
            } else {
                painter.text(bc, Align2::CENTER_CENTER, "!", FontId::new(11.0, bold_family()), Color32::WHITE);
            }
        }
        let ty = cy + r + 14.0;
        painter.text(Pos2::new(center.x, ty), Align2::CENTER_CENTER, title, FontId::new(13.0, bold_family()), pal.text);
        let max_w = rect.width() * 0.3;
        let galley = painter.layout(sub.clone(), FontId::proportional(13.0), pal.text_weak, max_w);
        let pos = Pos2::new(center.x - galley.size().x / 2.0, ty + 10.0);
        painter.galley(pos, galley, pal.text_weak);
    }

    if animating {
        ui.ctx().request_repaint();
    }
}

/// Idle, connecting and connected share one height so the cards below do not jump between states.
const STATUS_HEIGHT: f32 = 84.0;

fn status_area(ui: &mut Ui, pal: &Palette, state: Option<&State>, action: &mut Action) {
    if let Some(State::Failed(info)) = state {
        failure(ui, pal, info, action);
        return;
    }
    let size = Vec2::new(ui.available_width(), STATUS_HEIGHT);
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
        ui.set_min_height(STATUS_HEIGHT);
        match state {
            None => {
                ui.add_space((STATUS_HEIGHT - 44.0) / 2.0);
                if widgets::large_primary_button(ui, "연결", 220.0).on_hover_text("Enter").clicked() {
                    *action = Action::Connect;
                }
            }
            Some(State::Connected) => {
                ui.add_space(2.0);
                widgets::status_pill(ui, pal.success, "연결됨");
                ui.label(RichText::new("원격 데스크톱 창이 열려 있습니다.").color(pal.text_weak));
                if ui.button("연결 끊기").clicked() {
                    *action = Action::Cancel;
                }
            }
            _ => {
                let text = match state {
                    Some(State::Rdp) => "원격 PC에 로그인하는 중...",
                    Some(State::Reconnecting) => "연결이 끊겨 다시 연결하는 중...",
                    _ => "점프 호스트에 연결하는 중...",
                };
                ui.add_space(8.0);
                progress_row(ui, pal, text, "취소", action);
            }
        }
    });
}

fn progress_row(ui: &mut Ui, pal: &Palette, text: &str, cancel: &str, action: &mut Action) {
    ui.horizontal(|ui| {
        // Center the spinner + label group.
        let font = FontId::proportional(TITLE_SIZE);
        let w = 26.0 + ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, pal.text).size().x);
        ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
        ui.add(egui::Spinner::new().size(18.0).color(pal.accent));
        ui.label(RichText::new(text).size(TITLE_SIZE).color(pal.text));
    });
    if ui.button(cancel).clicked() {
        *action = Action::Cancel;
    }
}

fn failure(ui: &mut Ui, pal: &Palette, info: &FailureInfo, action: &mut Action) {
    widgets::banner(ui, pal.danger_soft, pal.danger.gamma_multiply(0.4), |ui| {
        ui.label(RichText::new(&info.title).bold().size(TITLE_SIZE).color(pal.danger));
        ui.label(RichText::new(&info.hint).color(pal.text));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            // The primary button is the step that can actually fix this failure.
            if info.key_changed {
                if widgets::danger_button(ui, "호스트 키 초기화...").clicked() {
                    *action = Action::ForgetKeys;
                }
            } else if info.fix_in_settings {
                if widgets::primary_button(ui, "설정 편집").clicked() {
                    *action = Action::Edit;
                }
                if ui.button("다시 시도").clicked() {
                    *action = Action::Connect;
                }
            } else {
                if widgets::primary_button(ui, "다시 시도").clicked() {
                    *action = Action::Connect;
                }
                if ui.button("설정 편집").clicked() {
                    *action = Action::Edit;
                }
            }
            if widgets::ghost_button(ui, "닫기").clicked() {
                *action = Action::Dismiss;
            }
        });
        if !info.detail.is_empty() {
            egui::CollapsingHeader::new(RichText::new("오류 원문").small().color(pal.text_weak))
                .id_salt("failure_detail")
                .show(ui, |ui| {
                    ui.label(RichText::new(&info.detail).monospace().color(pal.text_weak));
                });
        }
    });
}

fn account(profile: &Profile) -> String {
    match &profile.target.domain {
        Some(domain) => format!("{domain}\\{}", profile.target.user),
        None => profile.target.user.clone(),
    }
}

fn tilde(path: &std::path::Path) -> String {
    directories::BaseDirs::new()
        .and_then(|d| path.strip_prefix(d.home_dir()).ok().map(|rest| format!("~/{}", rest.display())))
        .unwrap_or_else(|| path.display().to_string())
}
