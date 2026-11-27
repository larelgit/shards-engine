//! Frame timing: how long the frames take that the game draws, in a window and `--offscreen`,
//! against a frame budget ([`EngineConfig::frame_budget`](shards_core::EngineConfig::frame_budget),
//! 1/60 s by default), and what the GPU spent on them and drew: `frames` in `ai/status`.
//!
//! A frame is the main world's update (`First` … `Last`: input, the ticks it ran, the game's
//! systems) and the render world's frame for it (the `Render` schedule), which Bevy runs side by
//! side on two threads: a frame costs the slower of the two. Extraction, between them, is not
//! counted, nor is the wait for the display (the swapchain image under vsync); offscreen every
//! frame waits for the GPU to draw it, and that wait is counted. A frame in which a call ran
//! ticks on a paused game (`step`) is no frame a player sees: it is left out.
//!
//! What the GPU spent and drew comes from Bevy's render diagnostics (not in a `dist` build): the
//! time of every pass and the triangles that reached the rasterizer, where the GPU measures them
//! (timestamp and pipeline-statistics queries: Vulkan and DX12). They arrive a few frames late.
//!
//! The draw commands per frame are counted in the render world, from the batched render phases ([`crate::draws`]
//! says exactly what is counted); they are frame-exact, so they travel with the frame's render time and
//! need no waiting for a report.
//!
//! Timing is wall-clock and differs from run to run: it is in no replay check, no replay and no
//! snapshot, and it is not journaled.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::diagnostic::DiagnosticsStore;
use bevy::prelude::*;
use bevy::render::view::prepare_windows;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};
use serde_json::{json, Value};

/// Frames kept for `ai/status`.
pub const RECENT: usize = 600;

/// The frame budget unless the game names one: 60 frames a second.
pub const DEFAULT_BUDGET: Duration = Duration::from_nanos(16_666_667);

/// Frames whose render time is still out: a render world more frames behind than this has
/// dropped the frame, which then counts with its main-world time alone.
const IN_FLIGHT: u64 = 8;

/// Frame timing and what the GPU reports.
pub(crate) struct FramesPlugin {
    /// [`EngineConfig::frame_budget`](shards_core::EngineConfig::frame_budget).
    pub budget: Option<Duration>,
}

impl Plugin for FramesPlugin {
    fn build(&self, app: &mut App) {
        let renders = Renders::default();
        app.insert_resource(FrameClock::new(self.budget.unwrap_or(DEFAULT_BUDGET)))
            .insert_resource(renders.clone())
            .add_systems(First, frame_started)
            .add_systems(Last, frame_ended);
        // GPU time and triangles of every pass (timestamp and pipeline-statistics queries). A
        // build for players does without them.
        #[cfg(not(feature = "dist"))]
        if !app.is_plugin_added::<bevy::render::diagnostic::RenderDiagnosticsPlugin>() {
            // (With `trace_tracy` Bevy's render plugin has added it already, and a second one is a panic.)
            app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin);
        }
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .insert_resource(renders)
                .init_resource::<RenderClock>()
                .add_systems(ExtractSchedule, extract_frame)
                .add_systems(
                    Render,
                    (
                        render_started.before(RenderSystems::ExtractCommands),
                        (display_wait_started.before(prepare_windows), display_wait_ended.after(prepare_windows))
                            .in_set(RenderSystems::PrepareViews),
                        // After Bevy batched the phases (the UI sets its batches in `PrepareBindGroups`),
                        // before the render graph draws them.
                        count_draws.after(RenderSystems::PrepareBindGroups).before(RenderSystems::Render),
                        render_ended.after(RenderSystems::PostCleanup),
                    ),
                );
        }
    }
}

/// One frame: its number, the main world's time and the render world's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub frame: u64,
    /// When its main-world update began.
    pub at: Instant,
    pub main: Duration,
    /// `None`: the render world dropped it.
    pub render: Option<Duration>,
    /// The draw commands the render phases issued for it ([`crate::draws`]); `None`: the render world
    /// dropped it, or Bevy keeps this GPU's batches to itself.
    pub draws: Option<u64>,
}

impl Frame {
    /// The slower of the two worlds: they run side by side.
    pub fn cost(&self) -> Duration {
        self.main.max(self.render.unwrap_or_default())
    }
}

/// How a run of frames went.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    pub frames: u64,
    pub total: Duration,
    /// The slowest frame, and which one it was.
    pub max: Duration,
    pub worst_frame: u64,
    pub over_budget: u64,
    /// Main-world and render-world time, summed.
    pub main: Duration,
    pub render: Duration,
    /// When the first and the last frame began: frames per second.
    pub first: Option<Instant>,
    pub last: Option<Instant>,
    /// What the GPU reported: frames measured, their time (ms) summed and the slowest, the
    /// triangles of the frames that counted them, summed and the most.
    pub gpu_frames: u64,
    pub gpu_ms: f64,
    pub gpu_max_ms: f64,
    pub triangle_frames: u64,
    pub triangles: f64,
    pub triangles_max: f64,
    /// The frames that counted their draws, the draws summed and the most in one frame.
    pub draw_frames: u64,
    pub draws: u64,
    pub draws_max: u64,
}

impl FrameStats {
    fn add(&mut self, frame: &Frame, budget: Duration) {
        let cost = frame.cost();
        self.frames += 1;
        self.total += cost;
        if self.frames == 1 || cost > self.max {
            self.max = cost;
            self.worst_frame = frame.frame;
        }
        self.over_budget += u64::from(cost > budget);
        self.main += frame.main;
        self.render += frame.render.unwrap_or_default();
        if let Some(draws) = frame.draws {
            self.draw_frames += 1;
            self.draws += draws;
            self.draws_max = self.draws_max.max(draws);
        }
        self.first.get_or_insert(frame.at);
        self.last = Some(frame.at);
    }

    fn add_gpu(&mut self, gpu: &Gpu) {
        if let Some(ms) = gpu.ms {
            self.gpu_frames += 1;
            self.gpu_ms += ms;
            self.gpu_max_ms = self.gpu_max_ms.max(ms);
        }
        if let Some(triangles) = gpu.triangles {
            self.triangle_frames += 1;
            self.triangles += triangles;
            self.triangles_max = self.triangles_max.max(triangles);
        }
    }

    fn per_frame(&self, total: Duration) -> Duration {
        match self.frames {
            0 => Duration::ZERO,
            n => Duration::from_nanos((total.as_nanos() / u128::from(n)) as u64),
        }
    }

    pub fn avg(&self) -> Duration {
        self.per_frame(self.total)
    }

    pub fn avg_main(&self) -> Duration {
        self.per_frame(self.main)
    }

    pub fn avg_render(&self) -> Duration {
        self.per_frame(self.render)
    }

    /// Frames begun per second of real time, between the first and the last.
    pub fn fps(&self) -> Option<f64> {
        let span = self.last?.duration_since(self.first?).as_secs_f64();
        (self.frames > 1 && span > 0.0).then(|| (self.frames - 1) as f64 / span)
    }

    /// GPU time per frame (ms), where the GPU measured it.
    pub fn gpu_avg_ms(&self) -> Option<f64> {
        (self.gpu_frames > 0).then(|| self.gpu_ms / self.gpu_frames as f64)
    }

    /// Draw commands per frame, where they were counted ([`crate::draws`]).
    pub fn draws_avg(&self) -> Option<f64> {
        (self.draw_frames > 0).then(|| self.draws as f64 / self.draw_frames as f64)
    }

    /// Triangles per frame, where the GPU counted them.
    pub fn triangles_avg(&self) -> Option<f64> {
        (self.triangle_frames > 0).then(|| self.triangles / self.triangle_frames as f64)
    }
}

/// What the GPU reported of one frame: the time of its passes (ms) and the triangles they drew.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gpu {
    pub ms: Option<f64>,
    pub triangles: Option<f64>,
}

/// The main world's side: the frame being drawn, the frames waiting for their render time, the
/// last [`RECENT`] frames and the span since [`begin_span`].
#[derive(Resource, Debug)]
pub struct FrameClock {
    /// A frame that takes longer is over budget.
    pub budget: Duration,
    frame: u64,
    started: Option<(Instant, u64)>,
    waiting: VecDeque<(u64, Instant, Duration)>,
    recent: VecDeque<Frame>,
    gpu: VecDeque<Gpu>,
    /// When the newest GPU report came in: the next one is a later frame's.
    gpu_seen: Option<Instant>,
    span: FrameStats,
    /// The frames the span counts: from the first after [`begin_span`] to the last before
    /// [`end_span`] (`u64::MAX` while it is open).
    span_frames: (u64, u64),
}

impl FrameClock {
    pub fn new(budget: Duration) -> Self {
        Self {
            budget,
            frame: 0,
            started: None,
            waiting: VecDeque::new(),
            recent: VecDeque::with_capacity(RECENT),
            gpu: VecDeque::with_capacity(RECENT),
            gpu_seen: None,
            span: FrameStats::default(),
            span_frames: (1, u64::MAX),
        }
    }

    /// Frame `frame + 1` begins at `at`, on tick `tick`.
    fn start(&mut self, at: Instant, tick: u64) {
        self.frame += 1;
        self.started = Some((at, tick));
    }

    /// The main world is done with the frame at `at`; the game is `paused` on tick `tick`. A
    /// frame that ran ticks on a paused game is left out.
    fn end(&mut self, at: Instant, tick: u64, paused: bool) {
        let Some((started, from_tick)) = self.started.take() else {
            return;
        };
        if paused && tick != from_tick {
            return;
        }
        self.waiting.push_back((self.frame, started, at.saturating_duration_since(started)));
    }

    /// The render world drew frame `frame` in `took`. Frames before it that it never reported
    /// count without it; a frame that was left out has no main-world time and is not counted.
    fn rendered(&mut self, frame: u64, took: Duration, draws: Option<u64>) {
        while let Some(&(waiting, at, main)) = self.waiting.front() {
            if waiting > frame {
                break;
            }
            self.waiting.pop_front();
            let render = (waiting == frame).then_some(took);
            self.complete(Frame { frame: waiting, at, main, render, draws: if waiting == frame { draws } else { None } });
        }
    }

    /// Frames whose render time is long overdue count without it.
    fn give_up_on_old(&mut self) {
        while let Some(&(waiting, at, main)) = self.waiting.front() {
            if waiting + IN_FLIGHT > self.frame {
                break;
            }
            self.waiting.pop_front();
            self.complete(Frame { frame: waiting, at, main, render: None, draws: None });
        }
    }

    fn complete(&mut self, frame: Frame) {
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(frame);
        let (first, last) = self.span_frames;
        if (first..=last).contains(&frame.frame) {
            self.span.add(&frame, self.budget);
        }
    }

    /// The GPU reported a frame. A report names no frame: it comes in at least one and at most
    /// [`IN_FLIGHT`] frames after the one it measures, so the span takes the reports that come in
    /// from its first frame + [`IN_FLIGHT`] to its last, which are surely of its own frames.
    fn gpu_report(&mut self, gpu: Gpu) {
        if self.gpu.len() == RECENT {
            self.gpu.pop_front();
        }
        self.gpu.push_back(gpu);
        let (first, last) = self.span_frames;
        if (first.saturating_add(IN_FLIGHT)..=last).contains(&self.frame) {
            self.span.add_gpu(&gpu);
        }
    }

    /// No frame of the span still waits for its render time.
    fn span_settled(&self) -> bool {
        self.waiting.front().is_none_or(|&(frame, ..)| frame > self.span_frames.1)
    }

    /// The last [`RECENT`] frames, oldest first.
    pub fn recent(&self) -> impl Iterator<Item = &Frame> {
        self.recent.iter()
    }

    /// The last [`RECENT`] frames, and the last [`RECENT`] GPU reports.
    pub fn recent_stats(&self) -> FrameStats {
        let mut stats = FrameStats::default();
        for frame in &self.recent {
            stats.add(frame, self.budget);
        }
        for gpu in &self.gpu {
            stats.add_gpu(gpu);
        }
        stats
    }
}

/// Starts counting frames afresh (the perf autotest after its warmup), from the next frame on:
/// the frames before it whose render times come in later are not counted.
pub fn begin_span(world: &mut World) {
    if let Some(mut clock) = world.get_resource_mut::<FrameClock>() {
        clock.span = FrameStats::default();
        clock.span_frames = (clock.frame + 1, u64::MAX);
    }
}

/// Ends the span at the last frame begun, and draws frames until the render times of the span's
/// frames have come in (or are given up on): the game is to be paused, so the frames drawn to wait
/// for them run no ticks, and they are not counted.
pub fn end_span(app: &mut App) {
    match app.world_mut().get_resource_mut::<FrameClock>() {
        Some(mut clock) => clock.span_frames.1 = clock.frame,
        None => return,
    }
    for _ in 0..=IN_FLIGHT {
        if app.world().resource::<FrameClock>().span_settled() {
            break;
        }
        app.update();
    }
}

/// The frames between [`begin_span`] and [`end_span`] (the ones so far, while it is open); `None`
/// when nothing is drawn. GPU reports carry no frame: the span has those that came in late enough
/// and early enough to be surely of its frames (all but its first [`IN_FLIGHT`] or so).
pub fn span(world: &World) -> Option<FrameStats> {
    world.get_resource::<FrameClock>().map(|c| c.span)
}

/// Render times (and draws) on their way from the render world to the main world.
#[derive(Resource, Clone, Default)]
struct Renders(Arc<Mutex<Vec<Rendered>>>);

/// What the render world reports of one frame it drew.
struct Rendered {
    frame: u64,
    took: Duration,
    draws: Option<u64>,
}

/// The render world's side: the frame it draws, when it began, and the time it waited for the
/// display.
#[derive(Resource, Default)]
struct RenderClock {
    frame: u64,
    started: Option<Instant>,
    display_wait_started: Option<Instant>,
    display_wait: Duration,
    /// The draws counted for this frame ([`crate::draws`]).
    draws: Option<u64>,
}

fn frame_started(mut clock: ResMut<FrameClock>, tick: Res<ai_core::sim::SimTick>) {
    clock.start(Instant::now(), tick.0);
}

/// The main world's part of the frame is done: it waits for its render time; the render times
/// and the GPU reports that came in are taken.
fn frame_ended(world: &mut World) {
    let at = Instant::now();
    let (tick, paused) = (ai_core::sim::tick(world), ai_core::sim::is_paused(world));
    let renders = std::mem::take(&mut *world.resource::<Renders>().0.lock().unwrap_or_else(|e| e.into_inner()));
    let seen = world.resource::<FrameClock>().gpu_seen;
    let report = world.get_resource::<DiagnosticsStore>().and_then(|store| gpu_report(store, seen));
    let mut clock = world.resource_mut::<FrameClock>();
    clock.end(at, tick, paused);
    for Rendered { frame, took, draws } in renders {
        clock.rendered(frame, took, draws);
    }
    clock.give_up_on_old();
    if let Some((time, gpu)) = report {
        clock.gpu_seen = Some(time);
        clock.gpu_report(gpu);
    }
}

/// The GPU report that came in after `seen`: Bevy's render diagnostics of one frame arrive
/// together, under one time. Its GPU time is that of the outermost passes (`render/<pass>/…`;
/// the inner ones are inside them), its triangles those of every render pass (passes do not
/// nest).
fn gpu_report(store: &DiagnosticsStore, seen: Option<Instant>) -> Option<(Instant, Gpu)> {
    let newest = store
        .iter()
        .filter(|d| d.path().components().next() == Some("render"))
        .filter_map(|d| d.measurement().map(|m| m.time))
        .max()?;
    if seen.is_some_and(|seen| newest <= seen) {
        return None;
    }
    let mut gpu = Gpu::default();
    for diagnostic in store.iter() {
        let Some(measurement) = diagnostic.measurement().filter(|m| m.time == newest) else {
            continue;
        };
        let path: Vec<&str> = diagnostic.path().components().collect();
        match path.as_slice() {
            ["render", _, "elapsed_gpu"] => *gpu.ms.get_or_insert(0.0) += measurement.value,
            ["render", .., "clipper_primitives_out"] => *gpu.triangles.get_or_insert(0.0) += measurement.value,
            _ => {}
        }
    }
    Some((newest, gpu))
}

fn extract_frame(mut clock: ResMut<RenderClock>, main: Extract<Res<FrameClock>>) {
    clock.frame = main.frame;
}

fn render_started(mut clock: ResMut<RenderClock>) {
    clock.started = Some(Instant::now());
    clock.display_wait = Duration::ZERO;
}

fn display_wait_started(mut clock: ResMut<RenderClock>) {
    clock.display_wait_started = Some(Instant::now());
}

/// `prepare_windows` takes the window's next swapchain image, and waits for the display to free
/// one (vsync): time the frame did not spend working.
fn display_wait_ended(mut clock: ResMut<RenderClock>) {
    if let Some(started) = clock.display_wait_started.take() {
        clock.display_wait += started.elapsed();
    }
}

/// The draws the render phases hold now, batched and about to be drawn.
fn count_draws(world: &mut World) {
    let draws = crate::draws::count(world);
    world.resource_mut::<RenderClock>().draws = draws;
}

fn render_ended(mut clock: ResMut<RenderClock>, renders: Res<Renders>) {
    let Some(started) = clock.started.take() else {
        return;
    };
    let took = started.elapsed().saturating_sub(clock.display_wait);
    renders.0.lock().unwrap_or_else(|e| e.into_inner()).push(Rendered { frame: clock.frame, took, draws: clock.draws.take() });
}

fn ms_json(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e6).round() / 1e3
}

fn round3(x: f64) -> f64 {
    (x * 1e3).round() / 1e3
}

/// `frames` in `ai/status`: the last [`RECENT`] frames against the budget, the main and the render
/// world's share, the GPU's time and triangles per frame (`null` where it measures none), and the
/// draw commands the render phases issue per frame, on average and at most (`null` where Bevy keeps
/// its batches to itself: [`crate::draws`]). `null`
/// when nothing is drawn (headless).
pub fn status(world: &World) -> Value {
    let Some(clock) = world.get_resource::<FrameClock>() else {
        return Value::Null;
    };
    let stats = clock.recent_stats();
    json!({
        "frames": stats.frames,
        "fps": stats.fps().map(|f| (f * 10.0).round() / 10.0),
        "avg_ms": ms_json(stats.avg()),
        "max_ms": ms_json(stats.max),
        "worst_frame": (stats.frames > 0).then_some(stats.worst_frame),
        "over_budget": stats.over_budget,
        "budget_ms": ms_json(clock.budget),
        "main_ms": ms_json(stats.avg_main()),
        "render_ms": ms_json(stats.avg_render()),
        "gpu_ms": stats.gpu_avg_ms().map(round3),
        "triangles": stats.triangles_avg().map(f64::round),
        "draws": stats.draws_avg().map(f64::round),
        "draws_max": (stats.draw_frames > 0).then_some(stats.draws_max),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Frame after frame from `at`, back to back: `(main ms, ticks it ran, paused)`. Returns when
    /// the last one ended.
    fn frames(clock: &mut FrameClock, mut at: Instant, frames: &[(u64, u64, bool)]) -> Instant {
        let mut tick = 0;
        for &(main, ticks, paused) in frames {
            clock.start(at, tick);
            tick += ticks;
            at += ms(main);
            clock.end(at, tick, paused);
        }
        at
    }

    #[test]
    fn a_frame_costs_the_slower_world_and_waits_for_its_render_time() {
        let mut clock = FrameClock::new(ms(16));
        let at = frames(&mut clock, Instant::now(), &[(4, 1, false), (20, 0, false), (3, 1, false)]);
        assert_eq!(clock.recent().count(), 0, "no render time yet");
        clock.rendered(1, ms(10), None);
        clock.rendered(2, ms(5), None);
        let stats = clock.recent_stats();
        assert_eq!((stats.frames, stats.worst_frame, stats.over_budget), (2, 2, 1));
        assert_eq!((stats.max, stats.avg()), (ms(20), ms(15)), "10 ms of render, then 20 ms of main");
        assert_eq!((stats.avg_main(), stats.avg_render()), (ms(12), Duration::from_micros(7500)));
        // Frame 3's render time never comes: once the render world is far ahead, it counts alone.
        frames(&mut clock, at, &[(1, 0, false); IN_FLIGHT as usize]);
        clock.give_up_on_old();
        let third = *clock.recent().last().unwrap();
        assert_eq!((third.frame, third.render, third.cost()), (3, None, ms(3)));
    }

    #[test]
    fn a_frame_that_stepped_a_paused_game_is_left_out() {
        let mut clock = FrameClock::new(ms(16));
        // A paused frame without ticks counts; one that ran a `step` of 600 does not; play does.
        frames(&mut clock, Instant::now(), &[(2, 0, true), (900, 600, true), (5, 1, false)]);
        for frame in 1..=3 {
            clock.rendered(frame, ms(1), None);
        }
        let counted: Vec<u64> = clock.recent().map(|f| f.frame).collect();
        assert_eq!(counted, [1, 3]);
        assert_eq!(clock.recent_stats().over_budget, 0);
    }

    #[test]
    fn only_the_last_frames_are_kept_and_a_span_starts_afresh() {
        let mut clock = FrameClock::new(ms(16));
        let last = RECENT as u64 + 5;
        let mut at = Instant::now();
        for frame in 1..=last {
            at = frames(&mut clock, at, &[(1, 1, false)]);
            clock.rendered(frame, ms(frame % 20), None);
        }
        assert_eq!(clock.recent().next().map(|f| f.frame), Some(6), "the oldest five are gone");
        assert_eq!(clock.recent_stats().frames, RECENT as u64);
        assert_eq!(clock.span.frames, last, "a span counts every frame, not only the kept ones");
        let fps = clock.recent_stats().fps().unwrap();
        assert!((fps - 1000.0).abs() < 1.0, "a frame every millisecond: {fps}");
        let mut world = World::new();
        world.insert_resource(clock);
        begin_span(&mut world);
        assert_eq!(span(&world).map(|s| s.frames), Some(0));
        assert_eq!(world.resource::<FrameClock>().recent().count(), RECENT, "the kept frames stay");
    }

    #[test]
    fn a_span_counts_its_own_frames_whenever_their_render_times_come_in() {
        let mut world = World::new();
        world.insert_resource(FrameClock::new(ms(16)));
        let mut clock = world.resource_mut::<FrameClock>();
        // A warmup of two frames; the second is still drawn when the span begins.
        let at = frames(&mut clock, Instant::now(), &[(30, 1, false), (2, 1, false)]);
        clock.rendered(1, ms(1), None);
        begin_span(&mut world);
        let mut clock = world.resource_mut::<FrameClock>();
        let at = frames(&mut clock, at, &[(30, 1, false), (2, 1, false)]);
        clock.rendered(2, ms(1), None);
        clock.rendered(3, ms(1), None);
        clock.span_frames.1 = clock.frame;
        assert!(!clock.span_settled(), "frame 4 waits for its render time");
        // A frame on the paused game while the last one is drawn: it is not the span's.
        frames(&mut clock, at, &[(40, 0, true)]);
        clock.rendered(4, ms(1), None);
        clock.rendered(5, ms(1), None);
        assert!(clock.span_settled());
        let span = span(&world).unwrap();
        assert_eq!((span.frames, span.over_budget, span.worst_frame), (2, 1, 3), "frames 3 and 4 only");
        assert_eq!(world.resource::<FrameClock>().recent().count(), 5, "every frame is still recent");
    }

    #[test]
    fn a_span_takes_only_the_gpu_reports_that_are_surely_of_its_frames() {
        let mut world = World::new();
        world.insert_resource(FrameClock::new(ms(16)));
        let report = |clock: &mut FrameClock, gpu_ms: f64| clock.gpu_report(Gpu { ms: Some(gpu_ms), triangles: None });
        let mut clock = world.resource_mut::<FrameClock>();
        let mut at = frames(&mut clock, Instant::now(), &[(1, 1, false); 3]);
        begin_span(&mut world);
        let mut clock = world.resource_mut::<FrameClock>();
        // A report that comes in on the span's first IN_FLIGHT frames may be a warmup frame's.
        for _ in 0..IN_FLIGHT {
            at = frames(&mut clock, at, &[(1, 1, false)]);
            report(&mut clock, 100.0);
        }
        for _ in 0..4 {
            at = frames(&mut clock, at, &[(1, 1, false)]);
            report(&mut clock, 2.0);
        }
        clock.span_frames.1 = clock.frame;
        // After the span's last frame, a report may be of the frames drawn to wait for it.
        frames(&mut clock, at, &[(1, 0, true)]);
        report(&mut clock, 100.0);
        let span = span(&world).unwrap();
        assert_eq!((span.gpu_frames, span.gpu_avg_ms()), (4, Some(2.0)));
        assert_eq!(world.resource::<FrameClock>().recent_stats().gpu_frames, 13, "every report is recent");
    }

    #[test]
    fn draws_travel_with_their_frame_and_average_over_the_frames_that_counted() {
        let mut clock = FrameClock::new(ms(16));
        frames(&mut clock, Instant::now(), &[(1, 1, false); 4]);
        clock.rendered(1, ms(1), Some(100));
        clock.rendered(2, ms(1), None);
        // Frame 3's render time never comes: its draws are lost with it, not given to frame 4.
        clock.rendered(4, ms(1), Some(300));
        let counted: Vec<Option<u64>> = clock.recent().map(|f| f.draws).collect();
        assert_eq!(counted, [Some(100), None, None, Some(300)]);
        let stats = clock.recent_stats();
        assert_eq!((stats.draw_frames, stats.draws_avg(), stats.draws_max), (2, Some(200.0), 300));
        let mut world = World::new();
        world.insert_resource(clock);
        let status = status(&world);
        assert_eq!((&status["draws"], &status["draws_max"]), (&json!(200.0), &json!(300)));
    }

    #[test]
    fn a_gpu_whose_batches_are_private_reports_no_draws() {
        let mut clock = FrameClock::new(ms(16));
        frames(&mut clock, Instant::now(), &[(1, 1, false); 2]);
        clock.rendered(2, ms(1), None);
        assert_eq!(clock.recent_stats().draws_avg(), None);
        let mut world = World::new();
        world.insert_resource(clock);
        let status = status(&world);
        assert_eq!((&status["draws"], &status["draws_max"]), (&Value::Null, &Value::Null));
    }

    #[test]
    fn the_gpu_report_sums_the_outer_passes_and_every_pass_s_triangles() {
        use bevy::diagnostic::{Diagnostic, DiagnosticMeasurement, DiagnosticPath};
        let mut store = DiagnosticsStore::default();
        let time = Instant::now();
        for (path, value) in [
            ("render/main_opaque_pass_3d/elapsed_gpu", 2.0),
            ("render/main_opaque_pass_3d/clipper_primitives_out", 1000.0),
            ("render/shadows/elapsed_gpu", 1.5),
            ("render/shadows/light_0/elapsed_gpu", 1.0),
            ("render/shadows/light_0/clipper_primitives_out", 400.0),
            ("render/ui/elapsed_cpu", 9.0),
        ] {
            let path = DiagnosticPath::new(path);
            store.add(Diagnostic::new(path.clone()));
            store.get_mut(&path).unwrap().add_measurement(DiagnosticMeasurement { time, value });
        }
        let (at, gpu) = gpu_report(&store, None).unwrap();
        assert_eq!((at, gpu), (time, Gpu { ms: Some(3.5), triangles: Some(1400.0) }));
        assert_eq!(gpu_report(&store, Some(time)), None, "a report is taken once");
    }
}
