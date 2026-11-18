//! Motion tokens and helpers. Every transition in the launcher uses these durations and curves, so
//! the app moves consistently. `Style::animation_time == 0` turns all of it off (screenshot tests);
//! the OS "reduce motion" setting keeps fades but drops movement and scaling.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{Color32, Context, Id, Ui, emath::easing};

/// Hover and press feedback.
pub const FAST: f32 = 0.12;
/// State changes: selection, status colors, content swaps.
pub const BASE: f32 = 0.2;
/// Things appearing: toasts, badges.
pub const SLOW: f32 = 0.3;

/// How far content slides while it fades in.
const ENTER_OFFSET: f32 = 8.0;

static REDUCED: AtomicBool = AtomicBool::new(false);

/// Follows the OS "reduce motion" setting; read once at startup.
pub fn follow_system() {
    REDUCED.store(super::platform::reduce_motion(), Ordering::Relaxed);
}

/// Scale for movement and size changes: 0 when the user asked for reduced motion.
pub fn travel() -> f32 {
    if REDUCED.load(Ordering::Relaxed) { 0.0 } else { 1.0 }
}

/// Whether motion is on at all (off in screenshot tests).
pub fn enabled(ctx: &Context) -> bool {
    ctx.global_style().animation_time > 0.0
}

fn duration(ctx: &Context, secs: f32) -> f32 {
    if enabled(ctx) { secs } else { 0.0 }
}

/// Duration of a movement: none under reduced motion.
pub fn movement(secs: f32) -> f32 {
    secs * travel()
}

/// 0..1 towards `on`, with a fast start and a soft landing in both directions.
pub fn toggle(ctx: &Context, id: Id, on: bool, secs: f32) -> f32 {
    ctx.animate_bool_with_time_and_easing(id, on, duration(ctx, secs), easing::cubic_out)
}

/// Like [`toggle`], but overshoots slightly before settling: for marks that pop in.
pub fn pop(ctx: &Context, id: Id, on: bool, secs: f32) -> f32 {
    let secs = duration(ctx, secs);
    if travel() == 0.0 {
        return ctx.animate_bool_with_time_and_easing(id, on, secs, easing::cubic_out);
    }
    ctx.animate_bool_with_time_and_easing(id, on, secs, easing::back_out).max(0.0)
}

/// Horizontal offset of a short "no" shake that plays once when `on` turns true.
pub fn shake(ctx: &Context, id: Id, on: bool) -> f32 {
    const AMPLITUDE: f32 = 4.0;
    const SWINGS: f32 = 3.0;
    let t = ctx.animate_bool_with_time(id, on, duration(ctx, SLOW));
    if !on || t >= 1.0 {
        return 0.0;
    }
    (t * std::f32::consts::TAU * SWINGS).sin() * AMPLITUDE * (1.0 - t) * travel()
}

/// Eased 0..1 progress of an animation that started at `start` (seconds, egui time).
pub fn since(ctx: &Context, start: f64, secs: f32) -> f32 {
    easing::cubic_out(linear_since(ctx, start, secs))
}

/// Linear 0..1 progress of an animation that started at `start`, for callers with their own curve.
pub fn linear_since(ctx: &Context, start: f64, secs: f32) -> f32 {
    let secs = duration(ctx, secs);
    let now = ctx.input(|i| i.time);
    let t = progress(now, start, secs);
    if t < 1.0 {
        ctx.request_repaint();
    }
    t
}

#[derive(Clone, Copy)]
struct Seen {
    key: Id,
    since: f64,
    pass: u64,
}

/// When `key` (for the thing `id` names) last changed, if that happened while it was on screen;
/// `NEG_INFINITY` when it is new or changed out of view. A change that finished while the user was
/// elsewhere, or a state found at launch, is shown as is instead of replaying its transition.
/// Call it once per frame while the thing is drawn.
pub fn changed_at(ctx: &Context, id: Id, key: impl std::hash::Hash + std::fmt::Debug) -> f64 {
    let key = Id::unique(key);
    let now = ctx.input(|i| i.time);
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let seen = d.get_temp_mut_or_insert_with(id, || Seen { key, since: f64::NEG_INFINITY, pass });
        if seen.key != key {
            let watched = seen.pass + 1 >= pass;
            seen.key = key;
            seen.since = if watched { now } else { f64::NEG_INFINITY };
        }
        seen.pass = pass;
        seen.since
    })
}

fn progress(now: f64, start: f64, secs: f32) -> f32 {
    if secs <= 0.0 { 1.0 } else { ((now - start) as f32 / secs).clamp(0.0, 1.0) }
}

/// Eases from the shown value to `target` whenever the target changes.
pub fn value(ctx: &Context, id: Id, target: f32, secs: f32) -> f32 {
    tween(ctx, id, target, secs, |a, b, t| a + (b - a) * t)
}

/// Eases from the shown color to `target` whenever the target changes.
pub fn color(ctx: &Context, id: Id, target: Color32, secs: f32) -> Color32 {
    tween(ctx, id, target, secs, |a, b, t| a.lerp_to_gamma(b, t))
}

#[derive(Clone, Copy)]
struct Tween<T> {
    from: T,
    to: T,
    start: f64,
}

fn tween<T>(ctx: &Context, id: Id, target: T, secs: f32, lerp: fn(T, T, f32) -> T) -> T
where
    T: Copy + PartialEq + Send + Sync + 'static,
{
    let now = ctx.input(|i| i.time);
    let secs = duration(ctx, secs);
    let state = ctx.data_mut(|d| {
        let t = d.get_temp_mut_or_insert_with(id, || Tween { from: target, to: target, start: f64::NEG_INFINITY });
        if t.to != target {
            // Start from what is on screen, so a change mid-animation does not jump.
            let shown = lerp(t.from, t.to, easing::cubic_out(progress(now, t.start, secs)));
            *t = Tween { from: shown, to: target, start: now };
        }
        *t
    });
    let t = progress(now, state.start, secs);
    if t < 1.0 {
        ctx.request_repaint();
    }
    lerp(state.from, state.to, easing::cubic_out(t))
}

/// Shows `add` fading in (and sliding up into place) whenever `key` changes on screen (see
/// [`changed_at`]), so nothing animates at launch.
pub fn fade_in<R>(ui: &mut Ui, id: Id, key: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui) -> R) -> R {
    let start = changed_at(ui.ctx(), id, key);
    let t = since(ui.ctx(), start, BASE);
    ui.scope(|ui| {
        ui.multiply_opacity(t);
        ui.add_space((1.0 - t) * ENTER_OFFSET * travel());
        add(ui)
    })
    .inner
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    use super::*;

    /// Runs one frame at `time` seconds and returns what `f` computed.
    fn frame<R>(ctx: &Context, time: f64, f: impl FnOnce(&Context) -> R) -> R {
        let input = egui::RawInput { time: Some(time), ..Default::default() };
        let mut out = None;
        let mut f = Some(f);
        let mut output = ctx.run_ui(input, |ui| {
            if let Some(f) = f.take() {
                out = Some(f(ui.ctx()));
            }
        });
        // Nothing renders here, so the font atlas upload is dropped on purpose.
        output.textures_delta.clear();
        out.expect("frame ran")
    }

    #[test]
    fn a_value_starts_where_it_is_and_eases_to_a_new_target() {
        let ctx = Context::default();
        let id = Id::unique("v");
        assert_eq!(frame(&ctx, 0.0, |c| value(c, id, 10.0, BASE)), 10.0);
        let early = frame(&ctx, 1.0, |c| value(c, id, 20.0, BASE));
        assert_eq!(early, 10.0, "the change starts from the shown value");
        let mid = frame(&ctx, 1.0 + f64::from(BASE) / 2.0, |c| value(c, id, 20.0, BASE));
        assert!(mid > 15.0 && mid < 20.0, "ease-out is past halfway at half time: {mid}");
        assert_eq!(frame(&ctx, 2.0, |c| value(c, id, 20.0, BASE)), 20.0);
    }

    #[test]
    fn a_retarget_mid_animation_does_not_jump() {
        let ctx = Context::default();
        let id = Id::unique("v");
        frame(&ctx, 0.0, |c| value(c, id, 0.0, BASE));
        frame(&ctx, 1.0, |c| value(c, id, 100.0, BASE));
        let before = frame(&ctx, 1.1, |c| value(c, id, 100.0, BASE));
        let after = frame(&ctx, 1.1, |c| value(c, id, 0.0, BASE));
        assert!((before - after).abs() < 0.01, "{before} vs {after}");
    }

    #[test]
    fn without_animation_time_everything_lands_at_once() {
        let ctx = Context::default();
        ctx.all_styles_mut(|s| s.animation_time = 0.0);
        let id = Id::unique("v");
        frame(&ctx, 0.0, |c| value(c, id, 0.0, BASE));
        assert_eq!(frame(&ctx, 0.0, |c| value(c, id, 1.0, BASE)), 1.0);
        assert_eq!(frame(&ctx, 0.0, |c| since(c, 0.0, SLOW)), 1.0);
        frame(&ctx, 0.0, |c| toggle(c, Id::unique("b"), false, BASE));
        assert_eq!(frame(&ctx, 0.0, |c| toggle(c, Id::unique("b"), true, BASE)), 1.0);
        assert_eq!(frame(&ctx, 0.0, |c| shake(c, Id::unique("s"), true)), 0.0);
    }

    #[test]
    fn a_change_on_screen_is_timed_and_one_out_of_view_is_not() {
        let ctx = Context::default();
        let id = Id::unique("state");
        assert_eq!(frame(&ctx, 0.0, |c| changed_at(c, id, 1)), f64::NEG_INFINITY, "found at launch");
        assert_eq!(frame(&ctx, 0.5, |c| changed_at(c, id, 2)), 0.5, "changed while drawn");
        assert_eq!(frame(&ctx, 0.6, |c| changed_at(c, id, 2)), 0.5);
        frame(&ctx, 0.7, |_| ()); // drawn elsewhere for a frame
        assert_eq!(frame(&ctx, 0.8, |c| changed_at(c, id, 3)), f64::NEG_INFINITY, "changed out of view");
    }

    #[test]
    fn an_entrance_runs_from_zero_to_one() {
        let ctx = Context::default();
        assert_eq!(frame(&ctx, 1.0, |c| since(c, 1.0, SLOW)), 0.0);
        let mid = frame(&ctx, 1.0 + f64::from(SLOW) / 2.0, |c| since(c, 1.0, SLOW));
        assert!(mid > 0.5 && mid < 1.0, "{mid}");
        assert_eq!(frame(&ctx, 2.0, |c| since(c, 1.0, SLOW)), 1.0);
    }
}
