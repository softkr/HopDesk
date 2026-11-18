//! Connection detail view: route diagram doubling as progress indicator, primary action, details.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Id, Pos2, RichText, Sense, Stroke, Ui, Vec2, emath::easing};

use super::motion;
use super::sessions::{FailureInfo, State};
use super::theme::{Bold as _, HEADING_SIZE, Palette, TITLE_SIZE, bold_family, palette};
use super::widgets::{self, Icon, card};
use crate::profile::{DisplayMode, Profile, Protocol};

pub enum Action {
    None,
    Connect,
    Cancel,
    Dismiss,
    Edit,
    Delete,
    ForgetKeys,
    OpenSharedFolder,
    PickSharedFolder,
}

pub fn ui(
    ui: &mut Ui,
    profile: &Profile,
    state: Option<&State>,
    connected_since: Option<std::time::Instant>,
    log: &[String],
    shared_folder: &std::path::Path,
) -> Action {
    let pal = palette(ui);
    let mut action = Action::None;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(20.0);
        header(ui, &pal, profile, state, &mut action);
        ui.add_space(16.0);

        card(ui, |ui| {
            widgets::card_header(ui, Icon::Route, "세션 경로", |ui| {
                if let (Some(State::Connected), Some(since)) = (state, connected_since) {
                    uptime_label(ui, &pal, since);
                }
            });
            ui.add_space(14.0);
            route(ui, &pal, profile, state);
            ui.add_space(6.0);
            hairline(ui, &pal);
            ui.add_space(14.0);
            let connecting = if profile.vpn.is_some() {
                "VPN에 연결하는 중...".to_owned()
            } else if profile.use_jump {
                "점프 호스트에 연결하는 중...".to_owned()
            } else {
                format!("{}에 연결하는 중...", super::target_noun(profile.protocol))
            };
            // Each step of the session swaps in its controls with a fade.
            let phase = match state {
                None => 0,
                Some(State::Starting | State::Ssh) => 1,
                Some(State::Remote) => 2,
                Some(State::Reconnecting) => 3,
                Some(State::Connected) => 4,
                Some(State::Failed(_)) => 5,
            };
            motion::fade_in(ui, Id::unique(("status", profile.id)), phase, |ui| {
                status_area(ui, &pal, profile.protocol, &connecting, state, &mut action);
            });
        });
        ui.add_space(12.0);

        card(ui, |ui| {
            widgets::card_header(ui, Icon::List, "연결 정보", |_| {});
            facts(ui, &pal, profile, shared_folder, &mut action);
        });

        if profile.protocol == Protocol::Vnc && !profile.is_tunneled() {
            ui.add_space(12.0);
            widgets::banner(ui, pal.danger_soft, pal.danger.gamma_multiply(0.4), |ui| {
                ui.label(RichText::new("암호화되지 않은 연결").bold().color(pal.danger));
                ui.label(
                    RichText::new("점프 호스트나 VPN 없이 VNC로 접속하므로 화면과 키 입력이 네트워크에 그대로 보입니다.")
                        .color(pal.text),
                );
            });
        }

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
                        if ui.add(widgets::labeled_button(Icon::Copy, "로그 복사", pal.text)).clicked() {
                            ui.ctx().copy_text(text);
                        }
                    });
            });
        }
        ui.add_space(20.0);
    });

    action
}

/// The connection's settings in three groups: how it is reached, the target, and (RDP) the session.
fn facts(ui: &mut Ui, pal: &Palette, profile: &Profile, shared_folder: &std::path::Path, action: &mut Action) {
    let key_or_password = |key: &Option<std::path::PathBuf>| match key {
        Some(path) => format!("개인 키 · {}", tilde(path)),
        None => "비밀번호".to_owned(),
    };
    widgets::fact_group(ui, "접속 경로", |f| {
        if let Some(vpn) = &profile.vpn {
            f.row(Icon::Shield, "VPN (WireGuard)", &vpn.endpoint);
            let addresses = vpn.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
            f.row(Icon::Network, "VPN 주소", &addresses);
        } else if profile.use_jump {
            let j = &profile.jump;
            f.row(Icon::Server, "점프 호스트", &format!("{}@{}:{}", j.user, j.host, j.port));
            f.row(Icon::Key, "SSH 인증", &key_or_password(&j.key_path));
        } else {
            f.row(Icon::Route, "방식", "직접 접속");
        }
    });

    let t = &profile.target;
    let target_icon = if profile.protocol == Protocol::Ssh { Icon::Terminal } else { Icon::Monitor };
    widgets::fact_group(ui, super::target_noun(profile.protocol), |f| {
        f.row(target_icon, "주소", &format!("{}:{}", t.host, t.port));
        match profile.protocol {
            Protocol::Vnc => {
                let login = match t.user.as_str() {
                    "" => "VNC 비밀번호".to_owned(),
                    user => format!("macOS 계정 · {user}"),
                };
                f.row(Icon::User, "로그인", &login);
            }
            Protocol::Ssh => {
                f.row(Icon::User, "계정", &account(profile));
                f.row(Icon::Key, "인증", &key_or_password(&t.key_path));
            }
            Protocol::Rdp => f.row(Icon::User, "Windows 계정", &account(profile)),
        }
    });

    if profile.protocol != Protocol::Rdp {
        return;
    }
    widgets::fact_group(ui, "세션", |f| {
        let resolution = match profile.display {
            DisplayMode::Fixed => format!("{} × {}", profile.width, profile.height),
            mode => mode.label().to_owned(),
        };
        f.row(Icon::Ratio, "해상도", &resolution);
        f.row_with(Icon::Folder, "공유 폴더", &tilde(shared_folder), |ui| {
            // Quiet text buttons: the row is information first. Right to left: "열기" ends up first.
            let quiet = |text: &str| egui::Button::new(RichText::new(text).color(pal.accent)).frame_when_inactive(false).small();
            if ui.add(quiet("변경…")).clicked() {
                *action = Action::PickSharedFolder;
            }
            if ui.add(quiet("열기")).clicked() {
                *action = Action::OpenSharedFolder;
            }
        });
    });
}

/// Identity card: tile with the session's status dot, name with its state, protocol and route tags,
/// account and address; actions on the right.
fn header(ui: &mut Ui, pal: &Palette, profile: &Profile, state: Option<&State>, action: &mut Action) {
    const TILE: f32 = 64.0;
    /// Fits the three text rows; the tile and the actions are centered on it.
    const HEIGHT: f32 = 84.0;
    const PILL_ROOM: f32 = 170.0;
    let badge = state.map(|s| super::state_badge(pal, s));
    card(ui, |ui| {
        let size = Vec2::new(ui.available_width(), HEIGHT);
        ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let busy = state.is_some_and(State::is_active);
            let menu = widgets::icon_button(ui, Icon::Ellipsis, 32.0).on_hover_text("더 보기");
            egui::Popup::menu(&menu).show(|ui| {
                ui.set_min_width(180.0);
                if ui.add(widgets::labeled_button(Icon::Key, "호스트 키 초기화...", pal.text)).clicked() {
                    *action = Action::ForgetKeys;
                    ui.close();
                }
                ui.separator();
                let delete = widgets::labeled_button(Icon::Trash, "연결 삭제...", pal.danger);
                if ui
                    .add_enabled(!busy, delete)
                    .on_disabled_hover_text("연결 중에는 삭제할 수 없습니다")
                    .clicked()
                {
                    *action = Action::Delete;
                    ui.close();
                }
            });
            let edit = widgets::outline_button(pal, Icon::Pencil, "편집");
            if ui.add(edit).on_hover_text(super::platform::shortcut("E")).clicked() {
                *action = Action::Edit;
            }

            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                widgets::avatar_tile(ui, TILE, &profile.name, badge.map(|(color, _)| color));
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal(|ui| {
                        ui.scope(|ui| {
                            // Leave room for the state pill; a long name is cut short instead.
                            let room = if badge.is_some() { PILL_ROOM } else { 0.0 };
                            ui.set_max_width((ui.available_width() - room).max(80.0));
                            ui.add(egui::Label::new(RichText::new(&profile.name).size(HEADING_SIZE).bold()).truncate());
                        });
                        if let Some((color, label)) = badge {
                            widgets::status_pill(ui, color, label);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        widgets::tag(ui, super::protocol_name(profile.protocol), pal.text, pal.subtle);
                        let route = super::route_name(profile).unwrap_or("직접 접속");
                        widgets::tag(ui, route, pal.accent, pal.accent_soft);
                    });
                    meta_line(ui, pal, profile);
                });
            });
        });
    });
}

/// `account │ host:port` in a monospace face, like the address bar of a terminal.
fn meta_line(ui: &mut Ui, pal: &Palette, profile: &Profile) {
    let font = FontId::monospace(13.0);
    let address = format!("{}:{}", profile.target.host, profile.target.port);
    let account = account(profile);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if !account.is_empty() {
            ui.add(egui::Label::new(RichText::new(account).font(font.clone()).color(pal.text)).truncate());
            ui.label(RichText::new("│").font(font.clone()).color(pal.border));
        }
        ui.add(egui::Label::new(RichText::new(address).font(font).color(pal.text_weak)).truncate());
    });
}

#[derive(Clone, Copy, PartialEq)]
enum Node {
    Idle,
    Done,
    Active,
    Error,
}

/// `● 가동시간 01:02:03`: how long the session has been up, ticking once a second.
fn uptime_label(ui: &mut Ui, pal: &Palette, since: std::time::Instant) {
    let elapsed = since.elapsed();
    ui.label(RichText::new(format_uptime(elapsed)).font(FontId::monospace(13.0)).color(pal.text));
    ui.label(RichText::new("가동시간").small().color(pal.text_weak));
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, pal.success);
    let to_next_second = 1.0 - f64::from(elapsed.subsec_millis()) / 1000.0;
    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(to_next_second));
}

/// `HH:MM:SS`, hours growing past 99 for sessions that last days.
fn format_uptime(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// A 1 px divider across the card.
fn hairline(ui: &mut Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, pal.border));
}

/// One stop on the route: its icon, name, address and role.
struct Stop {
    icon: Icon,
    title: String,
    address: String,
    role: String,
}

/// The route diagram, doubling as the progress indicator: a tile per stop (this device, the jump
/// host or VPN, the target) joined by links labeled with what runs over them.
fn route(ui: &mut Ui, pal: &Palette, profile: &Profile, state: Option<&State>) {
    use Node::*;
    let (mut nodes, mut links) = match state {
        None => ([Idle, Idle, Idle], [Idle, Idle]),
        Some(State::Starting | State::Ssh) => ([Done, Active, Idle], [Active, Idle]),
        Some(State::Remote) => ([Done, Done, Active], [Done, Active]),
        Some(State::Connected) => ([Done, Done, Done], [Done, Done]),
        Some(State::Reconnecting) => ([Done, Done, Active], [Done, Active]),
        Some(State::Failed(f)) if f.at_remote => ([Done, Done, Error], [Done, Error]),
        Some(State::Failed(_)) => ([Done, Error, Idle], [Error, Idle]),
    };

    let protocol = super::protocol_name(profile.protocol);
    let target_link = format!("{protocol} :{}", profile.target.port);
    let local = Stop {
        icon: Icon::Laptop,
        title: super::platform::THIS_DEVICE.to_owned(),
        address: super::platform::host_name().to_owned(),
        role: super::platform::OS_NAME.to_owned(),
    };
    let target = Stop {
        icon: if profile.protocol == Protocol::Ssh { Icon::Terminal } else { Icon::Monitor },
        title: super::target_noun(profile.protocol).to_owned(),
        address: format!("{}:{}", profile.target.host, profile.target.port),
        role: protocol.to_owned(),
    };
    let (stops, link_labels, positions) = if let Some(vpn) = &profile.vpn {
        let middle = Stop { icon: Icon::Shield, title: "VPN".to_owned(), address: vpn.endpoint.clone(), role: "WireGuard".to_owned() };
        (vec![local, middle, target], vec!["WireGuard".to_owned(), target_link], vec![1.0 / 6.0, 0.5, 5.0 / 6.0])
    } else if profile.use_jump {
        let j = &profile.jump;
        let auth = if j.key_path.is_some() { "개인 키" } else { "비밀번호" };
        let middle = Stop { icon: Icon::Server, title: "점프 호스트".to_owned(), address: format!("{}:{}", j.host, j.port), role: auth.to_owned() };
        (vec![local, middle, target], vec!["SSH".to_owned(), target_link], vec![1.0 / 6.0, 0.5, 5.0 / 6.0])
    } else {
        // Direct connection: this device -> target. A failure before the session is the target's.
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
        (vec![local, target], vec![target_link], vec![0.25, 0.75])
    };
    let node_count = stops.len();
    let connected = matches!(state, Some(State::Connected));

    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, DIAGRAM_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect.expand(8.0));
    let cy = rect.top() + 8.0 + TILE / 2.0;
    let xs: Vec<f32> = positions.iter().map(|f| rect.left() + rect.width() * f).collect();
    let column = rect.width() / node_count as f32 - 16.0;
    let color = |n: Node| match n {
        Idle => pal.border,
        Done => pal.success,
        Active => pal.accent,
        Error => pal.danger,
    };
    let ctx = ui.ctx().clone();
    let time = ui.input(|i| i.time);
    let mut animating = false;
    // Colors ease from step to step. Keyed per connection, so switching connections does not animate.
    let anim = |part: &str, i: usize| Id::unique(("route", profile.id, part, i));
    let ease = |part: &str, i: usize, target: Color32| motion::color(&ctx, anim(part, i), target, motion::BASE);
    let mono = FontId::monospace(11.5);

    for (i, link) in links.iter().take(node_count - 1).enumerate() {
        let a = Pos2::new(xs[i] + TILE / 2.0 + LINK_GAP, cy);
        let b = Pos2::new(xs[i + 1] - TILE / 2.0 - LINK_GAP, cy);
        let c = ease("link", i, color(*link));
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
                        painter.line_segment([Pos2::new(x0, cy), Pos2::new(x1, cy)], Stroke::new(2.0, c));
                    }
                    x += dash + gap;
                }
            }
            Idle => {
                painter.extend(egui::Shape::dashed_line(&[a, b], Stroke::new(1.5, c), 4.0, 5.0));
            }
            _ => {
                painter.line_segment([a, b], Stroke::new(2.0, c));
            }
        }
        // Arrow head.
        painter.line_segment([b, b + Vec2::new(-6.0, -5.0)], Stroke::new(2.0, c));
        painter.line_segment([b, b + Vec2::new(-6.0, 5.0)], Stroke::new(2.0, c));

        // What runs over the link, on a chip below it.
        let (text_color, fill, stroke) = match link {
            Idle => (pal.text_weak, pal.bg, pal.border),
            Error => (pal.danger, pal.danger_soft, pal.danger.gamma_multiply(0.4)),
            _ => (c, c.gamma_multiply(0.1), c.gamma_multiply(0.35)),
        };
        let label = &link_labels[i];
        let size = widgets::tag_size(ui, label, &mono);
        let chip = egui::Rect::from_center_size(Pos2::new((a.x + b.x) / 2.0, cy + 20.0), size);
        widgets::paint_tag(ui, chip, label, mono.clone(), ease("link_text", i, text_color), ease("link_fill", i, fill), ease("link_stroke", i, stroke));
    }

    for (i, node) in nodes.iter().take(node_count).enumerate() {
        let stop = &stops[i];
        let arrived = connected && i == node_count - 1;
        // A step that fails shakes once, like a rejected password field.
        let shake = motion::shake(&ctx, anim("shake", i), *node == Error);
        let center = Pos2::new(xs[i] + shake, cy);
        let tile = egui::Rect::from_center_size(center, Vec2::splat(TILE));
        let c = color(*node);
        let (fill, border, icon_color) = match node {
            _ if arrived => (pal.surface.lerp_to_gamma(pal.success, 0.12), pal.success, pal.success),
            Idle => (pal.surface, pal.border, pal.text_weak),
            Done => (pal.surface, pal.border, pal.text),
            Active => (pal.accent_soft, pal.accent, pal.accent),
            Error => (pal.danger_soft, pal.danger, pal.danger),
        };
        let (fill, border, icon_color) = (ease("fill", i, fill), ease("ring", i, border), ease("icon", i, icon_color));
        let radius = CornerRadius::same(TILE_RADIUS);
        painter.add(egui::epaint::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(14) }.as_shape(tile, radius));
        if *node == Active {
            animating = true;
            let pulse = ((time * 3.0).sin() as f32 + 1.0) / 2.0;
            let halo = tile.expand(3.0 + pulse * 4.0);
            let halo_radius = CornerRadius::same(TILE_RADIUS + (3.0 + pulse * 4.0) as u8);
            painter.rect_stroke(halo, halo_radius, Stroke::new(2.0, c.gamma_multiply(0.35 * (1.0 - pulse))), egui::StrokeKind::Outside);
        }
        painter.rect(tile, radius, fill, Stroke::new(1.5, border), egui::StrokeKind::Inside);
        widgets::paint_icon(ui, stop.icon, center, NODE_ICON, icon_color);
        // The step's result pops in as a badge on the tile's corner.
        let badge = motion::pop(&ctx, anim("badge", i), matches!(node, Done | Error), motion::SLOW);
        if badge > 0.0 {
            let bc = tile.right_top() + Vec2::new(-3.0, 3.0);
            let badge_color = if *node == Error { pal.danger } else { pal.success };
            painter.circle(bc, BADGE_RADIUS * badge, badge_color, Stroke::new(2.0, pal.surface));
            match node {
                Done => widgets::paint_icon_scaled(ui, Icon::Check, bc, BADGE_ICON, badge, Color32::WHITE),
                Error => {
                    let font = FontId::new(11.0 * badge, bold_family());
                    painter.text(bc, Align2::CENTER_CENTER, "!", font, Color32::WHITE);
                }
                // Fading out after a dismiss: the disc alone.
                _ => {}
            }
        }

        let mut y = tile.bottom() + 16.0;
        painter.text(Pos2::new(center.x, y), Align2::CENTER_CENTER, &stop.title, FontId::new(13.5, bold_family()), pal.text);
        y += 19.0;
        let mut job = egui::text::LayoutJob::single_section(stop.address.clone(), egui::TextFormat::simple(mono.clone(), pal.text_weak));
        job.wrap = egui::text::TextWrapping::truncate_at_width(column);
        let address = painter.layout_job(job);
        painter.galley(Pos2::new(center.x - address.size().x / 2.0, y - address.size().y / 2.0), address, pal.text_weak);
        y += 22.0;
        // The destination's role chip turns into the result once the session is up.
        let (role, text_color, chip_fill, chip_stroke) = if arrived {
            ("연결됨", Color32::WHITE, pal.success, pal.success)
        } else {
            (stop.role.as_str(), pal.text_weak, pal.bg, pal.border)
        };
        let font = FontId::proportional(11.5);
        let size = widgets::tag_size(ui, role, &font);
        let chip = egui::Rect::from_center_size(Pos2::new(center.x, y), size);
        widgets::paint_tag(ui, chip, role, font, text_color, chip_fill, chip_stroke);
    }

    // A connection that succeeds on screen gets a light running the route once and a ring blooming
    // out of the destination; after that (or right away for a session found connected), packets of
    // light keep flowing along the links while the session is up.
    let connected_at = motion::changed_at(&ctx, anim("connected", 0), connected);
    if connected && motion::enabled(&ctx) && motion::travel() > 0.0 {
        let intro = SWEEP_SECS + BLOOM_SECS;
        let elapsed = motion::linear_since(&ctx, connected_at, intro) * intro;
        if elapsed < intro {
            let head = easing::sin_in_out((elapsed / SWEEP_SECS).min(1.0));
            let bloom = ((elapsed - SWEEP_SECS) / BLOOM_SECS).clamp(0.0, 1.0);
            let last = egui::Rect::from_center_size(Pos2::new(xs[node_count - 1], cy), Vec2::splat(TILE));
            paint_sweep(&painter, &sweep_path(&xs, cy, TILE / 2.0 + 8.0), head, bloom, last, pal.success);
        } else {
            let shown = ((time - connected_at - f64::from(intro)) as f32 / SIGNAL_FADE_IN).min(1.0);
            paint_signal(&painter, &xs, cy, TILE / 2.0 + LINK_GAP, time, shown, pal.success);
            // The only animation that runs for as long as a session: capped at a modest frame
            // rate, and stopped while the window is hidden, minimized or covered.
            if ctx.input(|i| i.viewport().visible()) != Some(false) {
                ctx.request_repaint_after(SIGNAL_FRAME);
            }
        }
    }

    if animating {
        ui.ctx().request_repaint();
    }
}

/// How long the "connected" light takes to run the route, and the ring blooming after it.
const SWEEP_SECS: f32 = 1.4;
const BLOOM_SECS: f32 = 0.45;
/// The signal flowing while connected: how it appears after the sweep, and its frame interval.
const SIGNAL_FADE_IN: f32 = 0.5;
const SIGNAL_FRAME: std::time::Duration = std::time::Duration::from_millis(33);

/// Packets of light flowing along each link toward the destination, fading in where a link leaves
/// a node and out at its arrow. `time` drives the flow; `opacity` fades the whole signal in.
fn paint_signal(painter: &egui::Painter, xs: &[f32], cy: f32, edge_gap: f32, time: f64, opacity: f32, color: Color32) {
    const SPEED: f64 = 60.0;
    const SPACING: f32 = 80.0;
    const EDGE: f32 = 20.0;
    const TAIL: f32 = 16.0;
    const TAIL_SAMPLES: usize = 8;
    // f64 keeps the phase exact in sessions that last for days.
    let phase = (time * SPEED % f64::from(SPACING)) as f32;
    let core = color.lerp_to_gamma(Color32::WHITE, 0.6);
    for pair in xs.windows(2) {
        // From where the link leaves a tile to just before the arrow head.
        let (start, end) = (pair[0] + edge_gap, pair[1] - edge_gap - 6.0);
        let len = end - start;
        let mut d = phase - SPACING;
        while d < len {
            let edge = (d.min(len - d) / EDGE).clamp(0.0, 1.0);
            if d > 0.0 && edge > 0.0 {
                let alpha = edge * opacity;
                let tip = Pos2::new(start + d, cy);
                for k in 0..TAIL_SAMPLES {
                    let f = k as f32 / TAIL_SAMPLES as f32;
                    let x0 = (tip.x - TAIL * (1.0 - f)).max(start);
                    let x1 = (tip.x - TAIL * (1.0 - (k + 1) as f32 / TAIL_SAMPLES as f32)).max(start);
                    painter.line_segment([Pos2::new(x0, cy), Pos2::new(x1, cy)], Stroke::new(2.5, core.gamma_multiply(f * f * alpha)));
                }
                paint_spark(painter, tip, 0.7, alpha, color);
            }
            d += SPACING;
        }
    }
}

/// The glowing head of a light: soft halos in `color` around a bright core. `scale` sizes it.
fn paint_spark(painter: &egui::Painter, tip: Pos2, scale: f32, alpha: f32, color: Color32) {
    for (radius, strength) in [(12.0, 0.12), (8.0, 0.22), (5.5, 0.45)] {
        painter.circle_filled(tip, radius * scale, color.gamma_multiply(strength * alpha));
    }
    painter.circle_filled(tip, 3.0 * scale, color.lerp_to_gamma(Color32::WHITE, 0.6).gamma_multiply(alpha));
}

/// The route traced by the "connected" light, on circles around the tiles: once around the first,
/// along each link, over the top of a middle one, and once around the last. Angles grow clockwise
/// on screen (y points down), starting at the right.
fn sweep_path(xs: &[f32], cy: f32, r: f32) -> Vec<Pos2> {
    const STEPS_PER_TURN: f32 = 48.0;
    let mut path = Vec::new();
    let last = xs.len() - 1;
    for (i, &x) in xs.iter().enumerate() {
        let (from, turns) = match i {
            0 => (0.0, 1.0),
            i if i == last => (std::f32::consts::PI, 1.0),
            _ => (std::f32::consts::PI, 0.5),
        };
        let steps = (STEPS_PER_TURN * turns) as usize;
        for k in 0..=steps {
            let a = from + std::f32::consts::TAU * turns * k as f32 / steps as f32;
            path.push(Pos2::new(x + r * a.cos(), cy + r * a.sin()));
        }
    }
    path
}

/// A comet of light at `head` (0..1 along `path`) with a fading tail. As it fades (`bloom` 0..1),
/// an outline grows out of the destination tile `last`.
fn paint_sweep(painter: &egui::Painter, path: &[Pos2], head: f32, bloom: f32, last: egui::Rect, color: Color32) {
    const TAIL: f32 = 70.0;
    const TAIL_SAMPLES: usize = 24;
    const BLOOM_GROWTH: f32 = 12.0;
    let lengths: Vec<f32> = std::iter::once(0.0)
        .chain(path.windows(2).scan(0.0, |total, w| {
            *total += w[0].distance(w[1]);
            Some(*total)
        }))
        .collect();
    let total = lengths[lengths.len() - 1];
    let at = |s: f32| {
        let s = s.clamp(0.0, total);
        let i = lengths.partition_point(|&l| l < s).clamp(1, path.len() - 1);
        let (l0, l1) = (lengths[i - 1], lengths[i]);
        let f = if l1 > l0 { (s - l0) / (l1 - l0) } else { 0.0 };
        path[i - 1].lerp(path[i], f)
    };

    // Two layers read as light on both themes: a soft halo wider than the line (visible against a
    // light card) and a bright streak on it (visible against a dark one).
    let fade = 1.0 - bloom;
    let core = color.lerp_to_gamma(Color32::WHITE, 0.6);
    let s = head * total;
    let mut prev = at(s - TAIL);
    for k in 1..=TAIL_SAMPLES {
        let f = k as f32 / TAIL_SAMPLES as f32;
        let p = at(s - TAIL * (1.0 - f));
        let strength = f * f * fade;
        painter.line_segment([prev, p], Stroke::new(2.0 + 7.0 * f, color.gamma_multiply(0.22 * strength)));
        painter.line_segment([prev, p], Stroke::new(1.0 + 1.5 * f, core.gamma_multiply(strength)));
        prev = p;
    }
    paint_spark(painter, at(s), 1.0, fade, color);
    if bloom > 0.0 {
        let grow = 2.0 + BLOOM_GROWTH * easing::cubic_out(bloom);
        let stroke = Stroke::new(2.0, color.gamma_multiply(0.6 * (1.0 - bloom)));
        painter.rect_stroke(last.expand(grow), CornerRadius::same(TILE_RADIUS + grow as u8), stroke, egui::StrokeKind::Outside);
    }
}

/// Route diagram geometry: the stop tiles, the gap between a tile and its links, the icon in a
/// tile and the result badge on its corner.
const DIAGRAM_HEIGHT: f32 = 152.0;
const TILE: f32 = 56.0;
const TILE_RADIUS: u8 = 16;
const LINK_GAP: f32 = 12.0;
const NODE_ICON: f32 = 26.0;
const BADGE_RADIUS: f32 = 9.0;
const BADGE_ICON: f32 = 12.0;

/// Idle, connecting and connected share one height so the cards below do not jump between states.
const STATUS_HEIGHT: f32 = 50.0;

/// The action under the route: connect (idle), the progress with a way to cancel (connecting), the
/// open window with a way to disconnect (connected), or what failed and how to fix it.
fn status_area(ui: &mut Ui, pal: &Palette, protocol: Protocol, connecting: &str, state: Option<&State>, action: &mut Action) {
    if let Some(State::Failed(info)) = state {
        failure(ui, pal, info, action);
        return;
    }
    let size = Vec2::new(ui.available_width(), STATUS_HEIGHT);
    let row = |ui: &mut Ui, status: &mut dyn FnMut(&mut Ui), end: &mut dyn FnMut(&mut Ui)| {
        ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            status(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| end(ui));
        });
    };
    match state {
        None => {
            ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
                if widgets::hero_button(ui, Icon::Plug, "연결", Some("↵ Enter"), 260.0).clicked() {
                    *action = Action::Connect;
                }
            });
        }
        Some(State::Connected) => {
            let window = match protocol {
                Protocol::Rdp => "원격 데스크톱",
                Protocol::Ssh => "터미널",
                Protocol::Vnc => "화면 공유",
            };
            row(
                ui,
                &mut |ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.0, pal.success);
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("{window} 창이 열려 있습니다")).color(pal.text));
                },
                &mut |ui| {
                    if ui.add(widgets::danger_outline_button(pal, Icon::Power, "연결 끊기")).clicked() {
                        *action = Action::Cancel;
                    }
                },
            );
        }
        _ => {
            let text = match state {
                Some(State::Remote) => format!("{}에 로그인하는 중...", super::target_noun(protocol)),
                Some(State::Reconnecting) => "연결이 끊겨 다시 연결하는 중...".to_owned(),
                _ => connecting.to_owned(),
            };
            row(
                ui,
                &mut |ui| {
                    ui.add(egui::Spinner::new().size(18.0).color(pal.accent));
                    ui.add_space(6.0);
                    ui.label(RichText::new(&text).color(pal.text));
                },
                &mut |ui| {
                    if ui.add(widgets::outline_button(pal, Icon::Close, "취소")).clicked() {
                        *action = Action::Cancel;
                    }
                },
            );
        }
    }
}

fn failure(ui: &mut Ui, pal: &Palette, info: &FailureInfo, action: &mut Action) {
    widgets::banner(ui, pal.danger_soft, pal.danger.gamma_multiply(0.4), |ui| {
        ui.horizontal(|ui| {
            ui.add(Icon::Alert.image(widgets::ICON_MD, pal.danger));
            ui.label(RichText::new(&info.title).bold().size(TITLE_SIZE).color(pal.danger));
        });
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

#[cfg(test)]
mod tests {
    use eframe::egui::Pos2;

    use super::sweep_path;

    fn near(a: Pos2, b: Pos2) -> bool {
        a.distance(b) < 0.01
    }

    #[test]
    fn the_sweep_circles_the_ends_and_crosses_over_the_middle() {
        let (xs, cy, r) = ([100.0, 300.0, 500.0], 50.0, 20.0);
        let path = sweep_path(&xs, cy, r);
        assert!(near(path[0], Pos2::new(120.0, 50.0)), "starts right of the first node");
        assert!(path.iter().any(|p| near(*p, Pos2::new(100.0, 70.0))), "goes around the first node");
        assert!(path.iter().any(|p| near(*p, Pos2::new(300.0, 30.0))), "crosses over the middle node");
        assert!(!path.iter().any(|p| near(*p, Pos2::new(300.0, 70.0))), "not under it");
        assert!(path.iter().any(|p| near(*p, Pos2::new(520.0, 50.0))), "goes around the last node");
        assert!(near(path[path.len() - 1], Pos2::new(480.0, 50.0)), "ends where it reached the last node");
        // The only long steps are the links between rings.
        let jumps = path.windows(2).filter(|w| w[0].distance(w[1]) > 5.0).count();
        assert_eq!(jumps, 2);
    }

    #[test]
    fn uptime_reads_as_a_clock() {
        use std::time::Duration;
        assert_eq!(super::format_uptime(Duration::from_secs(0)), "00:00:00");
        assert_eq!(super::format_uptime(Duration::from_millis(3_723_900)), "01:02:03");
        assert_eq!(super::format_uptime(Duration::from_secs(100 * 3600 + 59)), "100:00:59");
    }

    #[test]
    fn a_direct_route_circles_both_nodes() {
        let path = sweep_path(&[100.0, 500.0], 50.0, 20.0);
        assert!(path.iter().any(|p| near(*p, Pos2::new(520.0, 50.0))));
        assert_eq!(path.windows(2).filter(|w| w[0].distance(w[1]) > 5.0).count(), 1);
    }
}
