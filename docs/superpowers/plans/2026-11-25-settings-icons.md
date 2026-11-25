# Settings Icons Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Icons on the settings screens: tiles on section titles, icons in segmented choices, leading icons in inputs.

**Architecture:** Two widget changes in `src/gui/widgets.rs` (a tile on `section_title`, a `segmented_icons` beside `segmented`), then call-site changes in `form.rs` and `ai_settings.rs`. Input icons use the existing `Input::leading_icon`.

**Tech Stack:** Rust, egui/eframe 0.37, egui_kittest, Lucide SVGs (lucide-static 1.74.0).

**Spec:** `docs/superpowers/specs/2026-11-25-settings-icons-design.md`

## Global Constraints
- Icons are Lucide, lucide-static 1.74.0, white stroke (fetched by `scripts/fetch-icons.sh`).
- No icons on field labels or dialog buttons; provider choices stay text.
- Section tile colors: 연결 accent blue, 접속 경로 indigo `#5E5CE6`, 원격 PC orange `#FF9F0A`, 세션 grey `#8E8E93`, 제공자·모델 accent blue.
- Commits only when the person asks.

## Review Focus
- Edit form (not new): 종류 is a plain label, not a control — it stays text (no tile clash).
- Narrow window (760 pt): segmented rows with icons must still fit on one line (`SSH 점프 호스트` is the widest).
- Dark mode: tiles and segmented icons must read on dark surfaces (checked in screenshots 07, 13, 19).
- Errors under a field with a leading icon keep the red outline (existing `Input` behavior).
- VNC/SSH variants of the 원격 card use the same tile.

---

### Task 1: Icons
**Files:** `scripts/fetch-icons.sh`, `assets/icons/lucide/*.svg`, `src/gui/widgets.rs` (enum `Icon`, `Icon::source`)
**Produces:** `Icon::{Globe, Cpu, User, ScreenShare, ArrowRight, FileKey, Folder}`
- [ ] Add `globe cpu user screen-share arrow-right file-key folder` and the branch's `arrow-up hand lock mouse-pointer-2 sparkles square zap` to `ICONS`; fetch only the 7 new files with the script's URL and `sed`.
- [ ] Add the variants and `lucide!` arms; `cargo build` passes.

### Task 2: Widgets
**Files:** `src/gui/widgets.rs`
**Produces:** `section_title(ui, icon: Icon, color: Color32, title: &str, help: Option<&str>)` (28 pt tile left of the title, help under the title) and `segmented_icons<T: PartialEq + Copy>(ui, value: &mut T, options: &[(T, Icon, &str)]) -> bool` (same look as `segmented`, a 14 pt icon before each label in the label's color).
- [ ] Test `a_segment_with_an_icon_is_picked_by_its_label`: kittest harness over `segmented_icons` with `(1, Monitor, "RDP"), (2, Terminal, "SSH")`; clicking "SSH" makes the state `2`.
- [ ] Run it: fails (no function).
- [ ] Implement both; `segmented` and `segmented_icons` share one private body.
- [ ] Run it: passes.

### Task 3: Connection form
**Files:** `src/gui/form.rs`
- [ ] First card gets `section_title(Monitor, accent, "연결", None)`; 접속 경로 Route/indigo; 원격 card Globe/orange; 세션 Settings/grey.
- [ ] 종류 → `segmented_icons` RDP Monitor / SSH Terminal / VNC ScreenShare; 방식 → 직접 ArrowRight / SSH 점프 Server / VPN Shield; both 인증 → 비밀번호 Lock / 개인 키 Key.
- [ ] Leading icons: host inputs Globe, user inputs User, key-file inputs FileKey, shared folder Folder.

### Task 4: AI settings
**Files:** `src/gui/ai_settings.rs`
- [ ] Before the provider control: `section_title(Server, accent, "제공자·모델", None)`; the field label "AI 제공자" goes.
- [ ] Leading icons: 서버 주소 Globe, 모델 Cpu, API 키 Key.

### Task 5: Verify
- [ ] `cargo clippy --all-targets` clean, `cargo test` all pass.
- [ ] `HOPDESK_SHOTS=target/screenshots cargo test screenshots -- --ignored`; look at 06, 07, 09, 13, 15, 17, 18, 19.
