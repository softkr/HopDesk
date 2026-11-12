//! Renders the launcher in representative states to PNG files for design review.
//!
//! `cargo test screenshots -- --ignored` (needs a GPU); images go to `$HOPDESK_SHOTS`
//! or `target/screenshots`.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use egui_kittest::Harness;
use uuid::Uuid;

use super::sessions::{Sessions, State};
use super::{Launcher, Nav, theme};
use crate::profile::{JumpHost, Profile, ProfileStore, RdpTarget};

fn profile(name: &str, jump: &str, target: &str, user: &str, key: bool) -> Profile {
    Profile {
        id: Uuid::new_v4(),
        name: name.into(),
        jump: JumpHost {
            host: jump.into(),
            port: 22,
            user: "ubuntu".into(),
            key_path: key.then(|| PathBuf::from("/Users/me/.ssh/id_ed25519")),
        },
        target: RdpTarget {
            host: target.into(),
            port: 3389,
            user: user.into(),
            domain: (user == "hong").then(|| "CORP".into()),
        },
        width: 1920,
        height: 1080,
        display: crate::profile::DisplayMode::Fixed,
    }
}

fn sample() -> Vec<Profile> {
    vec![
        profile("사무실 PC", "bastion.example.com", "10.0.0.5", "hong", true),
        profile("빌드 서버", "jump.dev.example.com", "10.20.1.14", "Administrator", false),
        profile("Lab Windows 11", "bastion.example.com", "192.168.40.21", "tester", true),
    ]
}

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("HOPDESK_SHOTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/screenshots"));
    std::fs::create_dir_all(&dir).expect("create screenshot dir");
    dir
}

fn harness(launcher: Launcher, dark: bool) -> Harness<'static, Launcher> {
    Harness::builder()
        .with_size(egui::Vec2::new(980.0, 680.0))
        .with_pixels_per_point(1.5)
        .with_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light })
        .with_max_steps(8)
        .wgpu()
        .with_context_setup(|ctx| {
            ctx.add_font_provider(Arc::new(egui_system_font_provider::SystemFontProvider::new()));
            theme::apply(ctx);
        })
        .build_ui_state(|ui, launcher: &mut Launcher| launcher.draw(ui), launcher)
}

fn shot(harness: &mut Harness<'_, Launcher>, name: &str) {
    for _ in 0..4 {
        harness.step();
    }
    let image = harness.render().expect("render");
    let path = out_dir().join(format!("{name}.png"));
    image.save(&path).expect("save png");
    eprintln!("wrote {}", path.display());
}

#[test]
#[ignore = "design review; needs a GPU"]
fn screenshots() {
    // 1. First run
    let mut h = harness(Launcher::with_store(ProfileStore::default(), Sessions::default()), false);
    shot(&mut h, "01-empty");

    // 2. Detail, idle
    let profiles = sample();
    let ids: Vec<Uuid> = profiles.iter().map(|p| p.id).collect();
    let sessions = Sessions::default();
    sessions.set_for_test(ids[2], State::Connected, vec![]);
    let mut h = harness(Launcher::with_store(ProfileStore::from_profiles(profiles.clone()), sessions.clone()), false);
    shot(&mut h, "02-detail-idle");

    // 3. Connecting (dark)
    sessions.set_for_test(ids[0], State::Rdp, vec!["INFO SSH jump host authenticated".into()]);
    let mut h = harness(Launcher::with_store(ProfileStore::from_profiles(profiles.clone()), sessions.clone()), true);
    shot(&mut h, "03-connecting-dark");

    // 4. Failure
    let info = Sessions::failure_for_test("jump host authentication failed", &State::Ssh);
    sessions.set_for_test(
        ids[0],
        State::Failed(info),
        vec![
            "INFO hopdesk::tunnel: Connecting to SSH jump host host=bastion.example.com port=22".into(),
            "Error: jump host authentication failed".into(),
        ],
    );
    let mut h = harness(Launcher::with_store(ProfileStore::from_profiles(profiles.clone()), sessions.clone()), false);
    shot(&mut h, "04-failed");

    // 5. Connected
    sessions.set_for_test(ids[0], State::Connected, vec![]);
    let mut h = harness(Launcher::with_store(ProfileStore::from_profiles(profiles.clone()), sessions.clone()), true);
    shot(&mut h, "05-connected-dark");

    // 6. New connection form after a save attempt with missing fields
    let mut h = harness(Launcher::with_store(ProfileStore::from_profiles(profiles.clone()), Sessions::default()), false);
    h.state_mut().apply_nav(Nav::New);
    h.step();
    let ctx = h.ctx.clone();
    h.state_mut().save(&ctx);
    shot(&mut h, "06-new-errors");

    // 7. Edit existing (dark)
    let mut h = harness(Launcher::with_store(ProfileStore::from_profiles(profiles), Sessions::default()), true);
    h.state_mut().apply_nav(Nav::Edit(ids[0]));
    shot(&mut h, "07-edit-dark");
}
