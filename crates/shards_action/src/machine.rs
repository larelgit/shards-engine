//! The action machine: one per fighter, for players, enemies and bosses alike.
//!
//! Presses of the fighter's moveset inputs move from `Intent` into its
//! [`InputBuffer`] and wait there up to [`CombatTuning::input_buffer`] seconds for
//! their action to become possible; one whose time runs out is rejected with the
//! reason ([`ActionRejected`], journaled). Held inputs (a block) start their action
//! whenever it is possible and loop it while held. [`can_start`] is the one rule for
//! "does this input start in the next tick, and if not, why and when": the machine
//! and the agent tools both ask it, so they never disagree.
//!
//! Timing: everything is decided between ticks. An action started in tick `N` runs
//! during tick `N` (its time goes from 0 to `dt`); it ends in the tick its time
//! reaches its length (that tick still counts: hits and defensive windows see it, the
//! state clears at the end of the tick), and the next action can start in the tick
//! after. A window is open in a tick when it touches the time that tick covered.

use ai_core::{AiId, AiIndex, SimTick};
use bevy::ecs::query::QueryData;
use bevy::prelude::*;
use shards_content::ContentDb;
use shards_core::input_names::{ASSIST_OFF, SPRINT};
use shards_core::{flat, Intent, NetRole, PlayerId};
use shards_motion::{
    CharacterController, CharacterParams, Collider, Ghost, LayerMask, Launch, LocomotionInput, MotionMode, MotionState, MoveModifiers,
    PhysicsWorld, QueryFilter,
};
use shards_rules::{ActiveRules, RuleClass, RulesView};

use std::ops::DerefMut;

use crate::combat::Downed;
use crate::context::{fresh_press, keeps_victim, past_revive, ContextInputs, GuardArm, PendingPair, PendingPairs, Pairing, RunHalted};
use crate::body::grant_barrier;
use crate::cue::ActionSound;
use crate::def::{touches, ActionDef, BufferMode, CancelTo, CombatTuning, Effect, Face, MovesetDef, StaminaMode, Steer, EPS};
use crate::fighter::{report_once, sprint_rule, Charges, Cooldowns, Dead, Fighter, Health, Stamina};
use crate::leap::{plan_leap, radius_of, LeapSite};
use crate::status::add_status_buildup;
use crate::zone::{action_zones, WorldShape};

/// What a fighter is doing.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
pub struct ActionState {
    /// `None`: free to act.
    pub current: Option<Running>,
    /// Actions started so far: numbers each one, so an action hits a target once.
    pub started: u32,
}

impl ActionState {
    /// Whether action `id` is running.
    pub fn is(&self, id: &str) -> bool {
        self.current.as_ref().is_some_and(|r| r.action == id)
    }
}

/// An action in progress. Display fields (`phase`, `windows`, `zone`) are kept up to
/// date by the machine so observations need no content lookup.
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct Running {
    pub action: String,
    /// Which start of an action this is ([`ActionState::started`]).
    pub instance: u32,
    /// Seconds into the action (loops inside a held span).
    pub t: f32,
    /// `t` before the last tick: the tick covered `(prev, t]`.
    pub prev: f32,
    pub length: f32,
    /// Where the action points, on the ground plane.
    pub facing: Vec3,
    /// The press's direction (a dodge's).
    pub dir: Option<Vec3>,
    /// The input that started it; `None` when the engine forced it (stagger, death).
    pub input: Option<String>,
    pub forced: bool,
    /// `(group, target)` pairs this action already hit.
    pub hit: Vec<(u8, Entity)>,
    pub phase: String,
    pub phase_end: f32,
    pub windows: Windows,
    /// `circle 4.5m`: its zone, while a hit window is still to come or open.
    pub zone: Option<String>,
    pub zone_until: f32,
    pub anim: Option<String>,
    /// Seconds a `hold` loop has kept this run raised (a block held by a loop); 0 when it is not
    /// looping. Display only, like the fields above: `observe` says `held 1.20s` instead of the
    /// loop phase's remaining time. Hidden from reflection (so from snapshots, replay checks and
    /// the network): it is counted again from the first tick after a load, and nothing reads it.
    #[reflect(ignore)]
    pub held: f32,
}

/// The defensive windows open in the last tick.
#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Windows {
    pub iframes: bool,
    pub guard: bool,
    pub perfect: bool,
    pub hyper_armor: bool,
}

/// What a run needs beyond [`Running`] when its action charges, turns by spans, assists or has
/// miss recovery ([`ActionDef::needs_extras`]). Inserted when such an action starts, removed when
/// its run ends; a fighter whose actions need none of it never has one.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component, Default)]
pub struct RunExtras {
    /// `Running::instance` of the run these belong to.
    pub instance: u32,
    /// The starting input has been held on every tick of the run so far (a charge turns on it).
    pub held: bool,
    /// One of this run's hits landed (a hit, a block, a parry or a guard break).
    pub landed: bool,
    /// The miss check has been made.
    pub checked: bool,
    /// Seconds added to the run's length (miss recovery, [`extend_recovery`]).
    pub extra: f32,
    /// The flat direction from the body to its lock-on target when the run started (the run's
    /// own facing without a target); `assist` keeps the facing within `assist.angle` degrees of it
    /// while there is a lock-on target.
    pub start_facing: Vec3,
    /// The run's facing when it started: the closing cap (`assist.close`) measures the approach
    /// its turning adds against moving along this.
    pub from: Vec3,
    /// Metres of approach to the target the run's turning has added so far (`assist.close` caps
    /// it).
    pub closed: f32,
    /// When the held input turns this run into its charged action (`charge.after`), while it
    /// still may; `None` once it no longer can (let go, past `until`, turned). For observers.
    pub charge_at: Option<f32>,
}

/// What a run of a `leap` action needs beyond [`Running`]: the scale its `displacement` is stretched
/// by (the chosen distance over the authored one) and whether it has launched. Inserted when such an
/// action starts, removed when its run ends; a fighter whose actions do not leap never has one.
/// (Apart from [`RunExtras`], so that a fighter that never leaps keeps the state it always had.)
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component, Default)]
pub struct LeapRun {
    /// `Running::instance` of the run this belongs to.
    pub instance: u32,
    /// `displacement` is scaled by this; 1: as authored.
    pub scale: f32,
    /// The launch has been asked for ([`Launch`]).
    pub launched: bool,
}

impl Default for LeapRun {
    fn default() -> Self {
        Self { instance: 0, scale: 1.0, launched: false }
    }
}

impl Default for RunExtras {
    fn default() -> Self {
        Self {
            instance: 0,
            held: false,
            landed: false,
            checked: false,
            extra: 0.0,
            start_facing: Vec3::NEG_Z,
            from: Vec3::NEG_Z,
            closed: 0.0,
            charge_at: None,
        }
    }
}

impl RunExtras {
    /// The extras a run of `def` starts with: `held` while `intent` holds its input,
    /// `start_facing` toward `target` (else the run's facing).
    pub(crate) fn start(def: &ActionDef, run: &Running, intent: &Intent, here: Vec3, target: Option<Vec3>) -> Self {
        let held = run.input.as_deref().is_some_and(|i| intent.is_held(i));
        Self {
            instance: run.instance,
            held,
            start_facing: target.and_then(|p| flat(p - here).try_normalize()).unwrap_or(run.facing),
            from: run.facing,
            charge_at: def.charge.as_ref().filter(|_| held).map(|c| c.after),
            ..default()
        }
    }

    /// Forgets `charge_at` once the run of `def` can no longer turn: its input was let go, it is
    /// past `until`, or it turned (`def` is the charged action then).
    pub(crate) fn settle_charge(&mut self, run: &Running, def: &ActionDef) {
        let may = def.charge.as_ref().is_some_and(|c| run.t <= c.until.unwrap_or_else(|| def.windup_end()) + EPS);
        if self.charge_at.is_some() && !(self.held && may) {
            self.charge_at = None;
        }
    }

    /// The seconds the run lasts beyond its definition, if `extras` belongs to it.
    pub(crate) fn extra_of(extras: Option<&RunExtras>) -> f32 {
        extras.map_or(0.0, |x| x.extra)
    }
}

impl Running {
    pub(crate) fn new(
        id: &str,
        def: &ActionDef,
        instance: u32,
        facing: Vec3,
        dir: Option<Vec3>,
        input: Option<String>,
    ) -> Self {
        let mut run = Running {
            action: id.to_owned(),
            instance,
            t: 0.0,
            prev: 0.0,
            length: def.length(),
            facing: flat(facing).try_normalize().unwrap_or(Vec3::NEG_Z),
            dir,
            forced: input.is_none(),
            input,
            hit: Vec::new(),
            phase: String::new(),
            phase_end: 0.0,
            windows: Windows::default(),
            zone: def.telegraph_zone().map(|(shape, _)| shape.to_string()),
            zone_until: zone_until(def),
            anim: def.anim.clone(),
            held: 0.0,
        };
        run.refresh(def, 0.0);
        run
    }

    /// Recomputes the phase and the open windows after `t` moved, and follows a hot reload
    /// of the definition: its length, zone and clip. They are compared first, so a tick that
    /// changes nothing allocates nothing (this runs every tick for every acting fighter).
    /// `extra`: seconds the run lasts beyond the definition ([`RunExtras::extra`]; 0 without).
    pub(crate) fn refresh(&mut self, def: &ActionDef, extra: f32) {
        self.length = def.length() + extra;
        self.zone_until = zone_until(def);
        let zone = def.telegraph_zone().map(|(shape, _)| shape);
        let same_zone = match (&self.zone, &zone) {
            (Some(shown), Some(shape)) => displays_as(shape, shown),
            (None, None) => true,
            _ => false,
        };
        if !same_zone {
            self.zone = zone.map(|shape| shape.to_string());
        }
        if self.anim != def.anim {
            self.anim.clone_from(&def.anim);
        }
        if let Some((phase, end)) = def.phase_at(self.t) {
            if self.phase != phase {
                self.phase = phase.to_owned();
            }
            self.phase_end = end;
        }
        let (prev, t) = (self.prev, self.t);
        let open =
            |span: Option<&crate::def::Span>| span.and_then(|s| def.span(s)).is_some_and(|w| touches(w, prev, t));
        self.windows = Windows {
            iframes: def.any_open(&def.iframes, prev, t),
            guard: open(def.guard.as_ref().map(|g| &g.span)),
            perfect: open(def.guard.as_ref().and_then(|g| g.perfect.as_ref())),
            hyper_armor: def.any_open(&def.hyper_armor, prev, t),
        };
    }
}

/// When the last hit window of `def` closes: the zone is shown until then.
fn zone_until(def: &ActionDef) -> f32 {
    def.hits.iter().filter_map(|h| def.span(&h.span)).map(|w| w.1).fold(0.0, f32::max)
}

/// Whether `value` formats as exactly `text`, without building the string (so it allocates
/// only if `value`'s own `Display` does; `HitShape`'s does not).
fn displays_as(value: &impl std::fmt::Display, text: &str) -> bool {
    use std::fmt::Write;
    /// A writer that only checks that what is written continues `rest`.
    struct Compare<'a> {
        rest: &'a str,
        same: bool,
    }
    impl Write for Compare<'_> {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            match self.rest.strip_prefix(s) {
                Some(rest) => self.rest = rest,
                None => self.same = false,
            }
            Ok(())
        }
    }
    let mut compare = Compare { rest: text, same: true };
    write!(compare, "{value}").is_ok() && compare.same && compare.rest.is_empty()
}

/// `windup heavy_slam 0.32s (zone circle 4.5m)`, `active roll 0.10s i-frames`; empty
/// when free.
pub fn describe_action(s: &ActionState) -> String {
    let Some(r) = &s.current else { return String::new() };
    // A run held by a loop never reaches the end of its phase: say that, and for how long.
    let mut out = match r.held > 0.0 {
        true => format!("{} {} held {:.2}s", r.phase, r.action, r.held),
        false => format!("{} {} {:.2}s", r.phase, r.action, (r.phase_end - r.t).max(0.0)),
    };
    if let Some(zone) = &r.zone
        && r.t < r.zone_until - EPS
    {
        out.push_str(&format!(" (zone {zone})"));
    }
    let w = r.windows;
    for (on, mark) in [
        (w.iframes, "i-frames"),
        (w.perfect, "perfect guard"),
        (w.guard && !w.perfect, "guard"),
        (w.hyper_armor, "hyper-armor"),
    ] {
        if on {
            out.push(' ');
            out.push_str(mark);
        }
    }
    out
}

/// The animation bridge's view: the clip of the running action and how far through
/// it is (0..1). Gameplay leads; the animation only follows.
pub fn action_progress(s: &ActionState) -> Option<(&str, f32)> {
    let r = s.current.as_ref()?;
    let clip = r.anim.as_deref()?;
    Some((clip, if r.length > 0.0 { (r.t / r.length).clamp(0.0, 1.0) } else { 1.0 }))
}

/// Presses waiting for their action to become possible.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
pub struct InputBuffer {
    /// Oldest first; one entry per input (a newer press replaces it).
    pub queued: Vec<Queued>,
}

#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct Queued {
    pub input: String,
    pub dir: Option<Vec3>,
    /// Seconds it may still wait.
    pub left: f32,
}

/// `queued dodge 0.10s`.
pub fn describe_buffer(b: &InputBuffer) -> String {
    b.queued.iter().map(|q| format!("queued {} {:.2}s", q.input, q.left.max(0.0))).collect::<Vec<_>>().join("  ")
}

// --- the shared start rule -----------------------------------------------------------

/// What [`can_start`] reads about a fighter: from a system's query or from `&World`. Build it with
/// [`FighterView::new`] and set what the caller has; what it leaves out is not judged.
#[derive(Clone, Copy)]
pub struct FighterView<'a> {
    pub db: &'a ContentDb,
    pub moveset: &'a str,
    pub state: &'a ActionState,
    pub stamina: Option<&'a Stamina>,
    pub cooldowns: Option<&'a Cooldowns>,
    pub charges: Option<&'a Charges>,
    /// `Intent::held`: decides whether a hold loop keeps going.
    pub held: &'a [String],
    pub dead: bool,
    /// The fighter is down ([`Downed`]): it starts nothing ([`Blocker::Downed`]).
    pub downed: bool,
    /// This tick's context ([`ContextInputs`]): a bound input starts its bound action.
    pub context: Option<&'a ContextInputs>,
    /// How the body moves now: the mode gate ([`MovesetDef::only_in`]) judges it. `None`: not judged.
    pub mode: Option<MotionMode>,
    /// The body is somewhere the controller drops a launch ([`off_ground`]): a leap would launch
    /// nothing, so it does not start. `None`: on the ground, within coyote time, or not judged.
    pub off_ground: Option<MotionMode>,
    /// Where the body stands, for the landing a leap needs. `None`: a leap's landing is not judged.
    pub leap: Option<LeapSite<'a>>,
    /// The rules in play as this fighter sees them (`shards_rules`): they scale what an action costs
    /// ([`stamina_cost`]). `None`: no rules, or a body without a class.
    pub rules: Option<RulesView<'a>>,
}

impl<'a> FighterView<'a> {
    /// A view of a fighter with the moveset `moveset`, in `state`, holding `held`, under `rules` (an argument,
    /// so no site that judges a start forgets what the rules make an action cost); every resource absent,
    /// the mode and the leap site not judged. Set the fields the caller has.
    pub fn new(
        db: &'a ContentDb,
        moveset: &'a str,
        state: &'a ActionState,
        held: &'a [String],
        dead: bool,
        rules: Option<RulesView<'a>>,
    ) -> Self {
        Self {
            db,
            moveset,
            state,
            stamina: None,
            cooldowns: None,
            charges: None,
            held,
            dead,
            downed: false,
            context: None,
            mode: None,
            off_ground: None,
            leap: None,
            rules,
        }
    }

    /// The fighter `entity` as the world has it now; `None` if it is no fighter.
    pub fn of(world: &'a World, entity: Entity) -> Option<Self> {
        let e = world.get_entity(entity).ok()?;
        Some(Self {
            db: world.get_resource::<ContentDb>()?,
            moveset: &e.get::<Fighter>()?.moveset,
            state: e.get::<ActionState>()?,
            stamina: e.get::<Stamina>(),
            cooldowns: e.get::<Cooldowns>(),
            charges: e.get::<Charges>(),
            held: e.get::<Intent>().map_or(&[], |i| i.held.as_slice()),
            dead: e.contains::<Dead>(),
            downed: e.contains::<Downed>(),
            context: e.get::<ContextInputs>(),
            mode: e.get::<MotionState>().map(|m| m.mode),
            off_ground: off_ground(e.get::<MotionState>()),
            leap: LeapSite::of(world, entity),
            rules: RulesView::of_entity(world, entity),
        })
    }

    pub fn moveset_def(&self) -> Option<&'a MovesetDef> {
        self.db.get::<MovesetDef>(self.moveset)
    }

    /// The action `input` starts now: the action its context binds it to, else the moveset's.
    pub fn action_for(&self, input: &str) -> Option<&'a str> {
        self.bound(input).or_else(|| self.moveset_def()?.action(input))
    }

    /// The action this tick's context binds `input` to, if any. A binding whose action the moveset's `only_in`
    /// refuses in the body's motion mode does not bind (`context::resolve` leaves it out too; this covers a
    /// context resolved before the mode changed, or a client's copy of its host's).
    pub fn bound(&self, input: &str) -> Option<&'a str> {
        let action = self.context.and_then(|c| c.bound(input)).map(|b| b.action.as_str())?;
        let refused = match (self.mode, self.moveset_def()) {
            (Some(mode), Some(moveset)) if moveset.gates(mode) => self.db.get::<ActionDef>(action).is_some_and(|def| !moveset.allows(mode, def)),
            _ => false,
        };
        (!refused).then_some(action)
    }
}

/// An input that starts in the next tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Start {
    pub action: String,
    /// It cuts the running action short through a cancel window.
    pub cancel: bool,
}

/// Why an input does not start in the next tick.
#[derive(Clone, Debug, PartialEq)]
pub enum Blocker {
    Dead,
    /// The fighter is down ([`Downed`]): nothing starts until it is revived.
    Downed,
    /// The fighter's moveset is not in content.
    NoMoveset {
        moveset: String,
    },
    /// The moveset has nothing for this input.
    NoInput {
        input: String,
        moveset: String,
        known: Vec<String>,
    },
    /// The moveset names an action content does not have.
    NoAction {
        action: String,
    },
    /// In an action that does not let this input through yet. `ready_in`: seconds until
    /// it ends or a cancel window lets the input through (`by_cancel`); `None` while a
    /// held loop keeps it going (`release` names the input to let go of).
    Busy {
        action: String,
        phase: String,
        phase_left: f32,
        ready_in: Option<f32>,
        by_cancel: bool,
        release: Option<String>,
    },
    /// `ready_in`: seconds until regeneration covers it; `None` if it never will.
    Stamina {
        have: f32,
        need: f32,
        ready_in: Option<f32>,
    },
    Cooldown {
        action: String,
        ready_in: f32,
    },
    /// The action's `cost.charge` takes `need` of a charge the fighter holds `have` of.
    NoCharges {
        charge: String,
        have: u32,
        /// How many the action takes ([`Cost::charge_count`](crate::Cost::charge_count)).
        need: u32,
        max: u32,
    },
    /// Like `NoCharges`, for a moveset that can refill the charge (an action of it has a `Refill`
    /// into it): the magazine is empty, `reload` is the input that fills it. Waiting does not help.
    Dry {
        charge: String,
        have: u32,
        need: u32,
        max: u32,
        /// The moveset input of the action that refills it (`reload`); `None` when no input maps to it
        /// (only `auto_reload` starts it).
        reload: Option<String>,
        /// The charge that action draws from, when that holds nothing: a reload would move nothing.
        spent: Option<String>,
    },
    /// An action that only refills charges (`Refill`) would move nothing now: its charge is full
    /// (`spent` is `None`), or the charge it draws from (`spent`) holds nothing.
    NothingToRefill {
        charge: String,
        have: u32,
        max: u32,
        spent: Option<String>,
    },
    /// The moveset lets only some actions start in the motion mode the body is in
    /// ([`MovesetDef::only_in`]), and this is not one of them. Waiting will not do.
    Mode {
        mode: MotionMode,
        action: String,
        /// The tags that would let an action start.
        tags: Vec<String>,
    },
    /// A leap (`leap.search`) has no landing between its `min` and `max`. Waiting will not do;
    /// moving may.
    NoLanding {
        min: f32,
        max: f32,
    },
    /// A leap needs the ground: the body is in `mode` (the air past its coyote time, water, a glide
    /// or a climb), where the controller drops the launch. Waiting does not help by the clock
    /// (`ready_in` is `None`); getting onto the ground does.
    OffGround { mode: MotionMode },
}

impl Blocker {
    /// Seconds until the input could start if nothing else happens; `None`: waiting
    /// will not do.
    pub fn ready_in(&self) -> Option<f32> {
        match self {
            Blocker::Busy { ready_in, .. } | Blocker::Stamina { ready_in, .. } => *ready_in,
            Blocker::Cooldown { ready_in, .. } => Some(*ready_in),
            _ => None,
        }
    }

    /// The short reason for the journal: `stamina 10/25`, `busy (windup heavy 0.80s)`.
    pub fn reason(&self) -> String {
        match self {
            Blocker::Dead => "dead".into(),
            Blocker::Downed => "down".into(),
            Blocker::NoMoveset { moveset } => format!("no moveset `{moveset}`"),
            Blocker::NoInput { input, .. } => format!("no action for `{input}`"),
            Blocker::NoAction { action } => format!("no action `{action}` in content"),
            Blocker::Busy { action, phase, phase_left, .. } => format!("busy ({phase} {action} {phase_left:.2}s)"),
            Blocker::Stamina { have, need, .. } => format!("stamina {have:.0}/{need:.0}"),
            Blocker::Cooldown { action, ready_in } => format!("cooldown {action} {ready_in:.2}s"),
            Blocker::NoCharges { charge, have, need, max } if *need > 1 => format!("needs {need} {charge} charges ({have}/{max})"),
            Blocker::NoCharges { charge, have, max, .. } => format!("no {charge} charges ({have}/{max})"),
            Blocker::Dry { charge, have, need, spent: Some(from), .. } => {
                format!("{charge} {}: no {from} left", short_of(*have, *need))
            }
            Blocker::Dry { charge, have, need, reload: Some(input), .. } => format!("{charge} {}: {input}", short_of(*have, *need)),
            Blocker::Dry { charge, have, need, .. } => format!("{charge} {}", short_of(*have, *need)),
            Blocker::NothingToRefill { charge, have, max, spent: None } => format!("{charge} full ({have}/{max})"),
            Blocker::NothingToRefill { charge, spent: Some(from), .. } => format!("no {from} left to refill {charge}"),
            Blocker::Mode { mode, tags, .. } => format!("not while in {}: only actions tagged {}", mode_name(*mode), tags.join(" or ")),
            Blocker::NoLanding { min, max } => format!("no landing spot within {}-{}m", fmt_m(*min), fmt_m(*max)),
            Blocker::OffGround { mode } => format!("in {}; a leap starts from the ground", mode_name(*mode)),
        }
    }

    /// The error a tool returns: what is wrong, when it would work, what to do.
    pub fn message(&self) -> String {
        match self {
            Blocker::Dead => "you are dead; a dev agent can `heal` you".into(),
            Blocker::Downed => "you are down: a partner can revive you (a dev agent can `heal` you)".into(),
            Blocker::NoMoveset { moveset } => {
                format!("your moveset `{moveset}` is not in content; `content_list kind=moveset` lists them")
            }
            Blocker::NoInput { input, moveset, known } => {
                format!("your moveset `{moveset}` has no `{input}`; it has: {}", known.join(", "))
            }
            Blocker::NoAction { action } => {
                format!("action `{action}` is missing from content; `content_check` explains")
            }
            Blocker::Busy { action, phase, phase_left, ready_in, by_cancel, release } => match (ready_in, release) {
                (Some(t), _) if *by_cancel => {
                    format!("busy: {phase} {action} {phase_left:.2}s (ready in {t:.2}s, when a cancel window opens)")
                }
                (Some(t), _) => format!("busy: {phase} {action} {phase_left:.2}s (ready in {t:.2}s)"),
                (None, Some(input)) => format!("busy: {phase} {action} while `{input}` is held; release it first"),
                (None, None) => format!("busy: {phase} {action}"),
            },
            Blocker::Stamina { have, need, ready_in: Some(t) } => {
                format!("not enough stamina: {have:.0}/{need:.0} (ready in {t:.1}s)")
            }
            Blocker::Stamina { have, need, ready_in: None } => format!("not enough stamina: {have:.0}/{need:.0}"),
            Blocker::Cooldown { action, ready_in } => format!("{action} is on cooldown (ready in {ready_in:.2}s)"),
            Blocker::NoCharges { charge, have, need, max } if *need > 1 => {
                format!("needs {need} {charge} charges, you have {have}/{max}")
            }
            Blocker::NoCharges { charge, have, max, .. } => format!("no {charge} charges left ({have}/{max})"),
            Blocker::Dry { charge, have, need, max, spent: Some(from), .. } => {
                format!("{charge} {} ({have}/{max}) and no {from} left to reload with; find more", short_of(*have, *need))
            }
            Blocker::Dry { charge, have, need, max, reload: Some(input), .. } => {
                format!("{charge} {} ({have}/{max}): {input}", short_of(*have, *need))
            }
            Blocker::Dry { charge, have, need, max, .. } => format!("{charge} {} ({have}/{max})", short_of(*have, *need)),
            Blocker::NothingToRefill { charge, have, max, spent: None } => format!("{charge} is already full ({have}/{max})"),
            Blocker::NothingToRefill { charge, spent: Some(from), .. } => {
                format!("no {from} left to refill {charge} with; find more")
            }
            Blocker::Mode { mode, action, tags } => format!(
                "{action} cannot start while in {}: only actions tagged {} do",
                mode_name(*mode),
                tags.join(" or ")
            ),
            Blocker::NoLanding { min, max } => {
                format!(
                    "no landing spot within {}-{}m; leap toward open, level ground, and not at a target nearer than {}m",
                    fmt_m(*min),
                    fmt_m(*max),
                    fmt_m(*min)
                )
            }
            Blocker::OffGround { mode } => {
                let then = if *mode == MotionMode::Air { "land first" } else { "get onto the ground first" };
                format!("you are in {}; a leap starts from the ground, so {then}", mode_name(*mode))
            }
        }
    }
}

/// The mode the body is in when the controller would drop a [`Launch`]: anything but the ground
/// and the air within coyote time of it (the controller's own rule). `None`: a launch would be
/// taken, or there is no body to judge.
pub fn off_ground(motion: Option<&MotionState>) -> Option<MotionMode> {
    let m = motion?;
    match m.mode {
        MotionMode::Ground => None,
        MotionMode::Air if m.coyote_left > 0.0 => None,
        mode => Some(mode),
    }
}

/// How a motion mode reads in a message: `water`, `the air`.
fn mode_name(mode: MotionMode) -> &'static str {
    match mode {
        MotionMode::Ground => "the ground",
        MotionMode::Air => "the air",
        MotionMode::Water => "water",
        MotionMode::Glide => "a glide",
        MotionMode::Mantle => "a climb",
        MotionMode::Ladder => "a climb on a ladder",
    }
}

/// A distance in metres without trailing zeros (`1.5`, `6`).
fn fmt_m(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// `empty` for a charge with none left, else what is short (`short (1 of 2 needed)`).
fn short_of(have: u32, need: u32) -> String {
    if have == 0 { "empty".to_owned() } else { format!("short ({have} of {need} needed)") }
}

/// Ticks until `secs` have passed (0 when they already have).
pub fn ticks_until(secs: f32, dt: f32) -> u32 {
    if secs <= EPS || dt <= 0.0 {
        0
    } else {
        ((secs - EPS) / dt).ceil() as u32
    }
}

/// Whether an input that becomes possible in `ready_in` seconds starts before its
/// buffer time `left` runs out.
pub fn fits_buffer(ready_in: f32, left: f32) -> bool {
    ready_in <= left + EPS
}

/// The engine's rule for `input`: does it start in the next tick (of length `dt`),
/// and as which action; if not, what blocks it, and for how long. When several things
/// block it, the one that lasts longest is reported.
pub fn can_start(v: &FighterView, input: &str, dt: f32) -> Result<Start, Blocker> {
    can_start_toward(v, input, dt, None)
}

/// [`can_start`] for a press toward `dir` (a queued press keeps its direction): a leap's landing is
/// searched along it.
pub(crate) fn can_start_toward(v: &FighterView, input: &str, dt: f32, dir: Option<Vec3>) -> Result<Start, Blocker> {
    if v.dead {
        return Err(Blocker::Dead);
    }
    if v.downed {
        return Err(Blocker::Downed);
    }
    let moveset = v.moveset_def().ok_or_else(|| Blocker::NoMoveset { moveset: v.moveset.to_owned() })?;
    let mut blockers: Vec<Blocker> = Vec::new();
    let mut cancel = false;
    // A bound input starts its bound action, through any window, a `Combo` one included.
    let bound = v.bound(input);
    let mut action = bound.or_else(|| moveset.action(input)).map(str::to_owned);
    if let Some(run) = &v.state.current
        && let Some(def) = v.db.get::<ActionDef>(&run.action)
    {
        match def.cancel_into(input, run.prev, run.t) {
            Some(to) => {
                cancel = true;
                if let CancelTo::Action(id) = to
                    && bound.is_none()
                {
                    action = Some(id);
                }
            }
            None => {
                let held = def
                    .hold
                    .as_ref()
                    .filter(|h| v.held.contains(&h.input) && def.span(&h.span).is_some_and(|w| run.t < w.1 - EPS));
                let ends_in = held.is_none().then(|| ticks_until(run.length - run.t, dt) as f32 * dt);
                let next = def.next_cancel(input, run.t);
                let cancel_in = next.as_ref().map(|(from, _)| ticks_until(from - run.t, dt) as f32 * dt);
                let by_cancel = match (cancel_in, ends_in) {
                    (Some(c), Some(e)) => c < e,
                    (Some(_), None) => true,
                    _ => false,
                };
                if by_cancel
                    && bound.is_none()
                    && let Some((_, CancelTo::Action(id))) = next
                {
                    action = Some(id);
                }
                blockers.push(Blocker::Busy {
                    action: run.action.clone(),
                    phase: run.phase.clone(),
                    phase_left: (run.phase_end - run.t).max(0.0),
                    ready_in: if by_cancel { cancel_in } else { ends_in },
                    by_cancel,
                    release: held.map(|h| h.input.clone()),
                });
            }
        }
    }
    let Some(action) = action else {
        return Err(Blocker::NoInput {
            input: input.to_owned(),
            moveset: v.moveset.to_owned(),
            known: moveset.inputs.keys().cloned().collect(),
        });
    };
    let mut action = action;
    let mut def = v.db.get::<ActionDef>(&action).ok_or_else(|| Blocker::NoAction { action: action.clone() })?;
    // An action that cannot pay its charges, in a moveset with an `auto_reload` that would help: the
    // press starts the reload instead, judged from here on as the reload (its own mode gate, cooldown, cost).
    if let Some((reload, reload_def)) = auto_reload(v, moveset, &action, def) {
        (action, def) = (reload, reload_def);
    }
    // The mode gate: in a mode `only_in` names, only the actions with a listed tag start.
    if let Some(mode) = v.mode
        && !moveset.allows(mode, def)
    {
        let tags = moveset.only_in.iter().filter(|(m, _)| *m == mode).flat_map(|(_, tags)| tags.iter().cloned()).collect();
        blockers.push(Blocker::Mode { mode, action: action.clone(), tags });
    }
    // A leap launches from the ground (or within coyote time of it): anywhere else it would pay and slide with no arc.
    if let Some(mode) = v.off_ground
        && def.leap.is_some()
    {
        blockers.push(Blocker::OffGround { mode });
    }
    // A leap with a search starts only where it has a landing.
    if let (Some(leap), Some(site)) = (def.leap.as_ref(), v.leap.as_ref())
        && let Some(search) = leap.search.as_ref()
        && plan_leap(def, search, site, dir).is_none()
    {
        blockers.push(Blocker::NoLanding { min: search.min, max: search.max });
    }
    let key = def.cooldown_key(&action);
    if let Some(left) = v.cooldowns.map(|c| c.left(key)).filter(|l| *l > EPS) {
        blockers.push(Blocker::Cooldown { action: key.to_owned(), ready_in: ticks_until(left, dt) as f32 * dt });
    }
    if let Some(blocker) = charges_short(v, moveset, def) {
        blockers.push(blocker);
    }
    // A reload that would move nothing (the magazine is full, the reserve is empty) does not start.
    if let Some(blocker) = nothing_to_refill(v, def) {
        blockers.push(blocker);
    }
    let need = stamina_cost(def, &action, v.rules);
    if let Some(s) = v.stamina {
        match def.cost.stamina_mode {
            StaminaMode::Full => {
                if s.current + EPS < need {
                    blockers.push(Blocker::Stamina { have: s.current, need, ready_in: stamina_ready_in(s, need, dt) });
                }
            }
            // Any stamina above 0 starts it; `pay` drains to 0 at most. The wait is for any at all.
            StaminaMode::Any => {
                if need > 0.0 && s.current <= EPS {
                    blockers.push(Blocker::Stamina { have: s.current, need, ready_in: stamina_ready_in(s, 2.0 * EPS, dt) });
                }
            }
        }
    }
    // Longest wait wins; `None` (never) is longest of all; earlier in the list on ties.
    let worst = blockers.into_iter().reduce(|a, b| match (a.ready_in(), b.ready_in()) {
        (None, _) => a,
        (_, None) => b,
        (Some(x), Some(y)) if y > x + EPS => b,
        _ => a,
    });
    match worst {
        Some(blocker) => Err(blocker),
        None => Ok(Start { action, cancel }),
    }
}

/// The charge `def` takes more of than the fighter holds: `(charge, have, need, max)`. A fighter
/// without the charge holds none.
fn short_charge<'d>(v: &FighterView, def: &'d ActionDef) -> Option<(&'d str, u32, u32, u32)> {
    let charge = def.cost.charge.as_deref()?;
    let (have, max) = v.charges.and_then(|c| c.get(charge)).map_or((0, 0), |c| (c.current, c.max));
    let need = def.cost.charge_count.max(1);
    (have < need).then_some((charge, have, need, max))
}

/// The reload that starts instead of `def` (the action `action`): the moveset's `auto_reload`, when
/// `def` is short of a charge and the reload would move something into it (room in the charge, a charge
/// in the reserve). Without one the press is refused ([`charges_short`]).
fn auto_reload<'a>(v: &FighterView<'a>, moveset: &MovesetDef, action: &str, def: &ActionDef) -> Option<(String, &'a ActionDef)> {
    let (charge, ..) = short_charge(v, def)?;
    let id = moveset.auto_reload.as_deref().filter(|id| *id != action)?;
    let reload = v.db.get::<ActionDef>(id)?;
    reload.refills_now(charge, v.charges).then(|| (id.to_owned(), reload))
}

/// Why `def` cannot pay its charges, if it cannot: `NoCharges`, or `Dry` when the moveset has an action
/// that refills the charge (the hint says which input, or that the reserve is empty too).
fn charges_short(v: &FighterView, moveset: &MovesetDef, def: &ActionDef) -> Option<Blocker> {
    let (charge, have, need, max) = short_charge(v, def)?;
    let charge = charge.to_owned();
    let Some((reload, from)) = refill_input(v.db, moveset, &charge) else {
        return Some(Blocker::NoCharges { charge, have, need, max });
    };
    let source_empty = v.charges.and_then(|c| c.get(from)).is_none_or(|c| c.current == 0);
    Some(Blocker::Dry {
        charge,
        have,
        need,
        max,
        reload: reload.map(str::to_owned),
        spent: source_empty.then(|| from.to_owned()),
    })
}

/// The moveset input of an action that refills `charge`, and the charge that action draws from: found in
/// the moveset's `inputs` (in input order), else in its `auto_reload` (no input then). Reads with `peek`:
/// the hint is words, not something the simulation's outcome depends on.
fn refill_input<'a>(db: &'a ContentDb, moveset: &'a MovesetDef, charge: &str) -> Option<(Option<&'a str>, &'a str)> {
    let draws = |action: &str| db.peek::<ActionDef>(action)?.refills().find(|(c, ..)| *c == charge).map(|(_, from, _)| from);
    moveset
        .inputs
        .iter()
        .find_map(|(input, action)| Some((Some(input.as_str()), draws(action)?)))
        .or_else(|| Some((None, draws(moveset.auto_reload.as_deref()?)?)))
}

/// `NothingToRefill` when `def` is a reload (it has `Refill` effects and nothing else but sounds) and none
/// of them would move anything now: every target is full, or every source is empty. An action that also
/// heals, hits or spawns starts whatever its refills do.
fn nothing_to_refill(v: &FighterView, def: &ActionDef) -> Option<Blocker> {
    let only_refills = def.hits.is_empty() && def.effects.iter().all(|e| matches!(e.effect, Effect::Refill { .. } | Effect::Sound(_)));
    if !only_refills {
        return None;
    }
    let mut refills = def.refills().peekable();
    refills.peek()?;
    let charges = v.charges;
    let (mut first, mut moves) = (None, false);
    for (charge, from, _) in refills {
        let (target, source) = (charges.and_then(|c| c.get(charge)), charges.and_then(|c| c.get(from)));
        let (room, left) = (target.is_some_and(|t| t.current < t.max), source.is_some_and(|s| s.current > 0));
        moves |= room && left;
        if first.is_none() {
            first = Some((charge, from, target.map_or((0, 0), |t| (t.current, t.max)), room));
        }
    }
    let (charge, from, (have, max), room) = first?;
    (!moves).then(|| Blocker::NothingToRefill {
        charge: charge.to_owned(),
        have,
        max,
        spent: room.then(|| from.to_owned()),
    })
}

/// What starting `def` (the action `id`) costs a fighter under `rules`: `cost.stamina` times the rules'
/// `stamina_cost` for its class (`shards_rules`, C3.4a). Without rules, or for an action that costs
/// nothing, it is `cost.stamina` exactly.
pub fn stamina_cost(def: &ActionDef, id: &str, rules: Option<RulesView>) -> f32 {
    let base = def.cost.stamina;
    match rules {
        Some(rules) if base > 0.0 => base * rules.stamina_factor(&def.tags, id),
        _ => base,
    }
}

/// Seconds until regeneration lifts `s` to `need`, by the same steps
/// [`regen_stamina`](crate::fighter::regen_stamina) takes (at the end of each tick).
fn stamina_ready_in(s: &Stamina, need: f32, dt: f32) -> Option<f32> {
    if s.regen <= 0.0 || need > s.max + EPS || dt <= 0.0 {
        return None;
    }
    let first = 1 + ticks_until(s.delay - s.since_use, dt);
    let steps = (((need - s.current) / (s.regen * dt)) - 1e-3).ceil().max(1.0) as u32;
    Some((first + steps - 1) as f32 * dt)
}

// --- events --------------------------------------------------------------------------

/// An action started (`input`: `None` when the engine forced it).
#[derive(Event, Reflect, Clone, Debug, PartialEq)]
pub struct ActionStarted {
    pub entity: Entity,
    pub action: String,
    pub input: Option<String>,
    /// A client started it for its own body ahead of the host
    /// ([`PredictedAction`](crate::PredictedAction)); the host's own `ActionStarted` for it
    /// comes later (forwarded, `false`), so presentation of a predicted body plays the first.
    pub predicted: bool,
}

/// An action stopped running. Triggered before the [`ActionStarted`] of what replaces it. Not a
/// net event: a client has no use for every end of every action. `finish_actions` triggers it
/// through `Commands` (applied at the next sync point), while [`force_action`], [`kill`](crate::kill)
/// and [`revive`](crate::revive) trigger it at once; an observer must not depend on the order of
/// the two paths (the test `action_ended_order_is_pinned` records what the engine produces).
#[derive(Event, Reflect, Clone, Debug, PartialEq)]
pub struct ActionEnded {
    pub entity: Entity,
    pub action: String,
    /// The run's `Running::instance`.
    pub instance: u32,
    pub end: ActionEnd,
}

impl ActionEnded {
    /// The event for `run` ending in `end`; one function for every place a run stops, which
    /// later modules extend.
    pub(crate) fn of(entity: Entity, run: &Running, end: ActionEnd) -> Self {
        Self { entity, action: run.action.clone(), instance: run.instance, end }
    }
}

/// How an action stopped ([`ActionEnded`]).
#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionEnd {
    /// Its time ran out.
    Finished,
    /// Another action started through one of its cancel windows.
    Cancelled,
    /// The engine forced another action, killed or revived the fighter.
    Interrupted,
    /// Its definition left content (a hot reload).
    Removed,
    /// It turned into its charge action (same instance, same time); the [`ActionStarted`] of
    /// the charged action follows.
    Branched,
    /// Its `while_held` input was let go.
    Released,
}

/// A buffered press never got to start.
#[derive(Event, Reflect, Clone, Debug, PartialEq)]
pub struct ActionRejected {
    pub entity: Entity,
    pub input: String,
    /// `stamina 10/25`, `busy (windup heavy 0.80s)`.
    pub reason: String,
}

/// An action with `miss_recovery` passed its last hit window and none of its hits landed: its
/// run is `extra` seconds longer. Host only; not a net event. An observer may lengthen it again
/// with [`extend_recovery`].
#[derive(Event, Reflect, Clone, Debug, PartialEq)]
pub struct ActionMissed {
    pub entity: Entity,
    pub action: String,
    /// Seconds added.
    pub extra: f32,
}

/// An attack's warning: where it lands and when, with the symbol and sound cue from
/// its content.
#[derive(Event, Reflect, Clone, Debug, PartialEq)]
pub struct Telegraph {
    pub attacker: Entity,
    pub action: String,
    /// Seconds until the first hit window opens.
    pub lands_in: f32,
    pub zone: Option<WorldShape>,
    pub symbol: String,
    pub sound: Option<String>,
}

/// Health came back (an action's `Heal` effect, the `heal` tool).
#[derive(Event, Reflect, Clone, Debug, PartialEq)]
pub struct Healed {
    pub entity: Entity,
    pub amount: f32,
    pub hp: f32,
}

/// Charges moved from one charge into another by a `Refill` effect (a reload): `taken` of `from` went
/// into `charge`. Host only; not a net event (the charges themselves are replicated).
#[derive(Event, Reflect, Clone, Debug, PartialEq)]
pub struct Refilled {
    pub entity: Entity,
    pub charge: String,
    pub from: String,
    /// How many moved (at least 1).
    pub taken: u32,
    /// What `charge` holds now, and its maximum.
    pub current: u32,
    pub max: u32,
    /// What `from` holds now.
    pub left: u32,
}

// --- the machine ---------------------------------------------------------------------

/// Effects actions reached this tick, applied in order right after the machine.
#[derive(Resource, Default)]
pub struct PendingEffects(pub Vec<(Entity, String, Effect)>);

/// A game's code for an action's `Custom(name)` effect.
pub type ActionEffect = fn(&mut World, Entity);

/// Game hooks for `Custom` effects, by name.
#[derive(Resource, Default, Clone)]
pub struct ActionEffects(pub(crate) Vec<(String, ActionEffect)>);

#[derive(QueryData)]
#[query_data(mutable)]
pub(crate) struct FighterData {
    entity: Entity,
    id: Option<&'static AiId>,
    fighter: &'static Fighter,
    intent: &'static mut Intent,
    state: &'static mut ActionState,
    buffer: &'static mut InputBuffer,
    stamina: Option<&'static mut Stamina>,
    cooldowns: &'static mut Cooldowns,
    charges: Option<&'static mut Charges>,
    transform: &'static Transform,
    loco: Option<&'static mut LocomotionInput>,
    motion: Option<&'static MotionState>,
    modifiers: Option<&'static MoveModifiers>,
    body: Option<(&'static CharacterController, Option<&'static CharacterParams>)>,
    extras: Option<&'static mut RunExtras>,
    leap: Option<&'static mut LeapRun>,
    ghost: Has<Ghost>,
    dead: Has<Dead>,
    downed: Has<Downed>,
    context: Option<&'static ContextInputs>,
    arm: Option<&'static mut GuardArm>,
    halted: Option<&'static RunHalted>,
    pairing: Option<&'static Pairing>,
    /// The body's rule class, and whether it is a player's (`shards_rules::RulesView::of`).
    class: Option<&'static RuleClass>,
    player: Has<PlayerId>,
}

/// What the machine decided for one fighter this tick.
pub(crate) struct Choice {
    pub start: Start,
    pub input: String,
    pub dir: Option<Vec3>,
    /// Index in the buffer; `None` for a held input.
    pub queued: Option<usize>,
}

/// Where a leap at the lock-on target `target` lands by, and how wide that body is: a lock point's
/// (a part's, R3) body, else the target itself. The host's machine and a client's prediction both.
pub(crate) fn leap_target(
    target: Option<AiId>,
    index: &AiIndex,
    parts: &Query<&crate::parts::Part>,
    positions: &Query<(&Transform, Option<&CharacterParams>, Option<&Collider>)>,
) -> Option<(Vec3, f32)> {
    let id = target?;
    let body = index.get(id).and_then(|e| parts.get(e).ok()).map_or(id, |p| p.body);
    let (tf, _, collider) = positions.get(index.get(body)?).ok()?;
    Some((tf.translation, radius_of(collider)))
}

/// `SimSet::Act`: every fighter's machine, in `AiId` order: presses into the buffer,
/// at most one action started, the running action advanced (effects, telegraphs),
/// expired presses rejected, and `LocomotionInput` told what the action wants.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_machines(
    time: Res<Time>,
    db: Res<ContentDb>,
    index: Res<AiIndex>,
    physics: Res<PhysicsWorld>,
    tick: Res<SimTick>,
    mut pending: ResMut<PendingEffects>,
    mut pairs: ResMut<PendingPairs>,
    mut commands: Commands,
    positions: Query<(&Transform, Option<&CharacterParams>, Option<&Collider>)>,
    parts: Query<&crate::parts::Part>,
    fallen: Query<(Has<Dead>, Has<Downed>)>,
    rules: Option<Res<ActiveRules>>,
    mut fighters: Query<FighterData>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let rules = rules.as_deref();
    let now = tick.0;
    let tuning = CombatTuning::of(&db);
    let (ttl, sight) = (tuning.input_buffer, tuning.sight_height);
    let mut order: Vec<(u32, Entity)> = fighters.iter().map(|f| (f.id.map_or(u32::MAX, |i| i.0), f.entity)).collect();
    order.sort();
    for (_, entity) in order {
        let Ok(mut f) = fighters.get_mut(entity) else { continue };
        // The rules in play as this fighter sees them: what its starts and branches cost (C3.4a).
        let ruled = RulesView::of(&db, rules, f.class, f.player);
        let moveset = db.get::<MovesetDef>(&f.fighter.moveset);
        // The mode gate: a run whose action lacks the tag the body's motion mode now asks for is cut
        // (a dodge or a guard when swimming begins), with no cooldown share. Forced runs (a stagger,
        // a death) are never cut.
        if let (Some(in_mode), Some(allowed), Some(run)) = (f.motion.map(|m| m.mode), moveset, f.state.current.as_ref())
            && !run.forced
            && db.get::<ActionDef>(&run.action).is_some_and(|d| !allowed.allows(in_mode, d))
            && let Some(old) = f.state.current.take()
        {
            cap_buffer(&mut f.buffer, ttl);
            commands.trigger(ActionEnded::of(entity, &old, ActionEnd::Interrupted));
        }
        let mode = moveset.map_or(&BufferMode::PerInput, |m| &m.buffer);
        let running_def = f.state.current.as_ref().and_then(|r| db.get::<ActionDef>(&r.action));
        if f.dead || f.downed {
            if !f.buffer.queued.is_empty() {
                f.buffer.queued.clear();
            }
        } else {
            let mut reject = |input, reason| commands.trigger(ActionRejected { entity, input, reason });
            let run = f.state.current.as_ref();
            take_presses(&mut f.intent, &mut f.buffer, mode, moveset, run, running_def, ttl, &mut reject);
        }

        let here = f.transform.translation;
        let forward = *f.transform.forward();
        // Where a body that faces its look aims its actions (F1a); none for every other body.
        let look = looked_at(f.body, &db, &f.intent);
        let locked = f.intent.target.and_then(|id| index.get(id)).and_then(|e| positions.get(e).ok().map(|p| (e, p)));
        // The lock-on target as a leap sees it: where it stands and how wide its body is (a lock point's
        // body: a leap at a hand or a horn lands by the body, not under the point).
        let target_body = leap_target(f.intent.target, &index, &parts, &positions);
        // A start judges a leap's landing from where the body stands, on the host as on a client.
        let choice = choose(&f, &db, ruled, leap_site(&f, &physics, &db, (here, forward), target_body), dt);
        // A paired action aims at its victim, lock-on or not: the one it starts on, the one it holds while it runs.
        let victim = match &choice {
            Some(c) => db
                .get::<ActionDef>(&c.start.action)
                .filter(|d| d.pair.is_some())
                .and_then(|_| f.context.and_then(|ctx| ctx.bound(&c.input)).filter(|b| b.action == c.start.action).and_then(|b| b.victim)),
            None => pair_victim_of(&db, f.pairing, f.state.current.as_ref(), |id| {
                index.get(id).and_then(|e| fallen.get(e).ok()).unwrap_or_default()
            }),
        };
        let locked = victim.and_then(|id| index.get(id)).and_then(|e| positions.get(e).ok().map(|p| (e, p))).or(locked);
        let target = locked.map(|(_, (t, _, _))| t.translation);
        // Whether the aimed-at body is in sight of the fighter, for an assist that needs it (asked only then).
        let mine = body_height(f.body.and_then(|(_, p)| p));
        let sees = || {
            let Some((them, (there, params, _))) = locked else { return true };
            in_sight(&physics, (entity, here, mine), (them, there.translation, body_height(params)), sight)
        };
        // This run's extras, if it has any (a local copy: a run started this tick has none in the world yet).
        let mut extras: Option<RunExtras> =
            f.extras.as_deref().copied().filter(|x| f.state.current.as_ref().is_some_and(|r| r.instance == x.instance));
        let mut leap: Option<LeapRun> =
            f.leap.as_deref().copied().filter(|x| f.state.current.as_ref().is_some_and(|r| r.instance == x.instance));

        if let Some(choice) = &choice {
            if let Some(i) = choice.queued {
                f.buffer.queued.remove(i);
            }
            // A leap's landing, searched once more with the press's direction: the same answer
            // `can_start` gave, unless the world changed in between (then the press is rejected, unpaid).
            if let Some(def) = db.get::<ActionDef>(&choice.start.action)
                && let Some((plan_dir, scale)) =
                    leap_plan(def, &f, &db, &physics, (here, forward), target_body, choice.dir, &mut |reason| {
                        commands.trigger(ActionRejected { entity, input: choice.input.clone(), reason });
                    })
            {
                // A windup cut short keeps its cooldown share before the new action pays its own: on a
                // shared key the new action's full cooldown is the one that stays.
                let cut = f.state.current.as_ref().and_then(|old| {
                    let old_def = db.get::<ActionDef>(&old.action)?;
                    let combo = matches!(old_def.cancel_into(&choice.input, old.prev, old.t), Some(CancelTo::Action(_)));
                    cut_share(old_def, old, ActionEnd::Cancelled, combo).map(|(key, secs)| (key.to_owned(), secs))
                });
                if let Some((key, secs)) = cut {
                    f.cooldowns.cut_to(&key, secs);
                }
                pay(&mut f, &choice.start.action, def, stamina_cost(def, &choice.start.action, ruled));
                let facing = plan_dir.unwrap_or_else(|| start_facing(def, choice.dir, &f.intent, look, here, forward, target));
                let instance = f.state.started;
                f.state.started += 1;
                let mut run =
                    Running::new(&choice.start.action, def, instance, facing, choice.dir, Some(choice.input.clone()));
                // A guard that needs a fresh press takes the press now: its perfect window opens
                // only if the press was fresh and recent, and the press arms no later guard.
                if let Some(press) = def.guard.as_ref().and_then(|g| g.perfect_needs_press.as_ref()) {
                    let fresh = fresh_press(f.arm.as_deref(), press, now, dt);
                    if let Some(arm) = f.arm.as_mut() {
                        arm.armed = false;
                        arm.run = fresh.then_some(instance);
                    }
                }
                gate_perfect(&mut run, def, f.arm.as_deref());
                if def.pair.is_some() {
                    let victim = f.context.and_then(|c| c.bound(&choice.input)).filter(|b| b.action == run.action).and_then(|b| b.victim);
                    pairs.0.push(PendingPair { attacker: entity, action: run.action.clone(), instance, victim });
                }
                if let Some(old) = f.state.current.take() {
                    cap_buffer(&mut f.buffer, ttl);
                    commands.trigger(ActionEnded::of(entity, &old, ActionEnd::Cancelled));
                }
                extras = def.needs_extras().then(|| RunExtras::start(def, &run, &f.intent, here, target));
                leap = def.leap.as_ref().map(|_| LeapRun { instance, scale, launched: false });
                begin(entity, def, &run, here, &mut pending.0, &mut |e| commands.trigger(e));
                commands.trigger(ActionStarted {
                    entity,
                    action: run.action.clone(),
                    input: run.input.clone(),
                    predicted: false,
                });
                f.state.current = Some(run);
            }
        }

        // The running action (perhaps the one just started) takes this tick.
        let mut external = Vec3::ZERO;
        let mut gone = false;
        let mut released = false;
        if let Some(run) = f.state.current.as_mut() {
            match db.get::<ActionDef>(&run.action) {
                Some(mut def) => {
                    if extras.is_none() && def.needs_extras() {
                        // A run whose action came to need extras on the way (a hot reload).
                        extras = Some(RunExtras {
                            instance: run.instance,
                            start_facing: run.facing,
                            from: run.facing,
                            ..default()
                        });
                    }
                    if def.leap.is_none() {
                        // A run whose action stopped leaping on the way (a hot reload) keeps no leap state.
                        leap = None;
                    } else if leap.is_none() {
                        // A run whose action came to leap on the way (a hot reload).
                        leap = Some(LeapRun { instance: run.instance, ..default() });
                    }
                    let spans;
                    let aimed = tracked(def, &f.intent, target, sees);
                    let scale = leap.map_or(1.0, |l| l.scale);
                    (spans, external) = step_run(run, def, extras.as_mut(), scale, &f.intent, look, here, aimed, dt);
                    if f.halted.is_some_and(|h| h.instance == run.instance) {
                        external = Vec3::ZERO;
                    }
                    // The launch: once, in the tick that crosses `leap.at` (a body with no controller has
                    // nothing to launch).
                    if let (Some(l), Some(state)) = (def.leap.as_ref(), leap.as_mut())
                        && !state.launched
                        && spans.iter().any(|&(lo, hi)| launch_due(l.at, lo, hi))
                    {
                        state.launched = true;
                        if f.body.is_some() {
                            commands.entity(entity).try_insert(Launch { height: l.height });
                        }
                    }
                    crossed(
                        def,
                        &spans,
                        |e| pending.0.push((entity, run.action.clone(), e.clone())),
                        || commands.trigger(telegraph(entity, def, run, here)),
                    );
                    run.refresh(def, RunExtras::extra_of(extras.as_ref()));
                    gate_perfect(run, def, f.arm.as_deref());
                    // Held long enough: it turns into its charged action, if the stamina is there.
                    if let Some(into) = charge_due(run, def, extras.as_ref()) {
                        match db.get::<ActionDef>(into) {
                            Some(into_def) => {
                                let from_cost = stamina_cost(def, &run.action, ruled);
                                let into_cost = stamina_cost(into_def, into, ruled);
                                let (from, into) = ((run.action.as_str(), def, from_cost), (into, into_def, into_cost));
                                if pay_branch(f.stamina.as_deref_mut(), &mut f.cooldowns, f.charges.as_deref_mut(), from, into) {
                                    let into = into.0;
                                    let ended = ActionEnded::of(entity, run, ActionEnd::Branched);
                                    run.action = into.to_owned();
                                    run.refresh(into_def, RunExtras::extra_of(extras.as_ref()));
                                    gate_perfect(run, into_def, f.arm.as_deref());
                                    commands.trigger(ended);
                                    commands.trigger(ActionStarted {
                                        entity,
                                        action: run.action.clone(),
                                        input: run.input.clone(),
                                        predicted: false,
                                    });
                                    def = into_def;
                                }
                            }
                            // The charged action left content (a hot reload): the run stops.
                            None => gone = true,
                        }
                    }
                    if let Some(x) = extras.as_mut() {
                        x.settle_charge(run, def);
                    }
                    released = def.while_held.as_ref().is_some_and(|input| !f.intent.is_held(input));
                }
                // Its definition left content (a hot reload): it stops.
                None => gone = true,
            }
        }
        if gone && let Some(old) = f.state.current.take() {
            cap_buffer(&mut f.buffer, ttl);
            commands.trigger(ActionEnded::of(entity, &old, ActionEnd::Removed));
        } else if released && let Some(old) = f.state.current.take() {
            cap_buffer(&mut f.buffer, ttl);
            if let Some((key, secs)) =
                db.get::<ActionDef>(&old.action).and_then(|d| cut_share(d, &old, ActionEnd::Released, false))
            {
                f.cooldowns.cut_to(key, secs);
            }
            commands.trigger(ActionEnded::of(entity, &old, ActionEnd::Released));
        }
        // The extras follow the run: updated in place, put in, or taken out.
        let extras = extras.filter(|x| f.state.current.as_ref().is_some_and(|r| r.instance == x.instance));
        match (extras, f.extras.as_mut()) {
            (Some(x), Some(there)) => {
                if **there != x {
                    **there = x;
                }
            }
            (Some(x), None) => {
                commands.queue(settle_extras(entity, Some(x)));
            }
            (None, Some(_)) => {
                commands.queue(settle_extras(entity, None));
            }
            (None, None) => {}
        }
        // The leap's state the same way.
        let leap = leap.filter(|x| {
            f.state.current.as_ref().is_some_and(|r| {
                r.instance == x.instance && db.get::<ActionDef>(&r.action).is_some_and(|d| d.leap.is_some())
            })
        });
        match (leap, f.leap.as_mut()) {
            (Some(x), Some(there)) => {
                if **there != x {
                    **there = x;
                }
            }
            (Some(x), None) => {
                commands.queue(settle_leap(entity, Some(x)));
            }
            (None, Some(_)) => {
                commands.queue(settle_leap(entity, None));
            }
            (None, None) => {}
        }
        // The body passes through characters while the run's `ghost` window is open, and not otherwise.
        let ghost = f
            .state
            .current
            .as_ref()
            .is_some_and(|r| db.get::<ActionDef>(&r.action).is_some_and(|d| d.ghost_in(r.prev, r.t)));
        if ghost != f.ghost {
            if ghost {
                commands.entity(entity).try_insert(Ghost);
            } else {
                commands.entity(entity).try_remove::<Ghost>();
            }
        }

        // Presses that did not start age; those out of time are rejected.
        if !f.buffer.queued.is_empty() {
            let mut expired = Vec::new();
            for q in &mut f.buffer.queued {
                q.left -= dt;
                if q.left < -EPS {
                    expired.push(q.input.clone());
                }
            }
            if !expired.is_empty() {
                f.buffer.queued.retain(|q| q.left >= -EPS);
                for input in expired {
                    let view = view(&f, &db, ruled, leap_site(&f, &physics, &db, (here, forward), target_body));
                    let reason = match can_start(&view, &input, dt) {
                        Err(blocker) => blocker.reason(),
                        Ok(_) => "another action started first".to_owned(),
                    };
                    commands.trigger(ActionRejected { entity, input, reason });
                }
            }
        }
        steer_body(&mut f, &db, external);
    }
}

/// The body of `f` as a leap's landing search sees it; `None` for a fighter with no controller.
fn leap_site<'a>(
    f: &'a FighterDataItem,
    physics: &'a PhysicsWorld,
    db: &ContentDb,
    at: (Vec3, Vec3),
    target: Option<(Vec3, f32)>,
) -> Option<LeapSite<'a>> {
    let (controller, own) = f.body?;
    Some(LeapSite::body(physics, f.entity, at, controller.resolve(own, Some(db)), &f.intent, target))
}

fn view<'a>(f: &'a FighterDataItem, db: &'a ContentDb, rules: Option<RulesView<'a>>, leap: Option<LeapSite<'a>>) -> FighterView<'a> {
    FighterView {
        leap,
        stamina: f.stamina.as_deref(),
        cooldowns: Some(&f.cooldowns),
        charges: f.charges.as_deref(),
        mode: f.motion.map(|m| m.mode),
        off_ground: off_ground(f.motion),
        context: f.context,
        downed: f.downed,
        ..FighterView::new(db, &f.fighter.moveset, &f.state, &f.intent.held, f.dead, rules)
    }
}

/// Where a leap of `def` goes for the fighter `f`, and how far the run's `displacement` stretches:
/// `Some((direction, scale))` (no direction and a scale of 1 for an action that does not search),
/// or `None` after saying why through `refuse` when there is no landing. A fighter with no body
/// searches nothing (it launches nothing either).
#[allow(clippy::too_many_arguments, reason = "the machine's state for one fighter, passed apart for the borrows")]
fn leap_plan(
    def: &ActionDef,
    f: &FighterDataItem,
    db: &ContentDb,
    physics: &PhysicsWorld,
    at: (Vec3, Vec3),
    target: Option<(Vec3, f32)>,
    press: Option<Vec3>,
    refuse: &mut impl FnMut(String),
) -> Option<(Option<Vec3>, f32)> {
    let Some(search) = def.leap.as_ref().and_then(|l| l.search.as_ref()) else { return Some((None, 1.0)) };
    let Some((controller, own)) = f.body else { return Some((None, 1.0)) };
    let site = LeapSite::body(physics, f.entity, at, controller.resolve(own, Some(db)), &f.intent, target);
    match plan_leap(def, search, &site, press) {
        Some(plan) => {
            let total = def.displacement_at(def.length());
            Some((Some(plan.dir), if total > EPS { plan.distance / total } else { 1.0 }))
        }
        None => {
            refuse(Blocker::NoLanding { min: search.min, max: search.max }.reason());
            None
        }
    }
}

/// Whether a leap that launches `at` seconds in launches in the tick `(lo, hi]` (one that launches
/// at 0 in the tick that starts the action).
pub(crate) fn launch_due(at: f32, lo: f32, hi: f32) -> bool {
    crosses(at, lo, hi) || (at <= EPS && lo <= EPS)
}

/// The first buffered press that can start, else a held input that can.
fn choose<'a>(
    f: &'a FighterDataItem,
    db: &'a ContentDb,
    rules: Option<RulesView<'a>>,
    leap: Option<LeapSite<'a>>,
    dt: f32,
) -> Option<Choice> {
    if f.dead || f.downed {
        return None;
    }
    choose_in(&view(f, db, rules, leap), &f.buffer.queued, dt)
}

/// [`choose`] over a view: the first of `queued` that can start, else a held input (the
/// view's `held`) whose action starts held and is not running yet.
pub(crate) fn choose_in(view: &FighterView, queued: &[Queued], dt: f32) -> Option<Choice> {
    for (i, q) in queued.iter().enumerate() {
        if let Ok(start) = can_start_toward(view, &q.input, dt, q.dir) {
            return Some(Choice { start, input: q.input.clone(), dir: q.dir, queued: Some(i) });
        }
    }
    view.moveset_def()?;
    for held in view.held {
        let Some(def) = view.action_for(held).and_then(|a| view.db.get::<ActionDef>(a)) else { continue };
        let starts_held = def.hold.as_ref().is_some_and(|h| h.input == *held);
        if !starts_held || view.action_for(held).is_some_and(|a| view.state.is(a)) {
            continue;
        }
        if let Ok(start) = can_start(view, held, dt) {
            return Some(Choice { start, input: held.clone(), dir: None, queued: None });
        }
    }
    None
}

/// Moves this tick's presses of inputs the fighter understands into its buffer by the moveset's
/// [`BufferMode`] (each living [`press_life`]); `reject` hears a press the buffer drops and why.
/// Others stay for other systems.
#[allow(clippy::too_many_arguments, reason = "the machine's state for one fighter, passed apart for the borrows")]
fn take_presses(
    intent: &mut Mut<Intent>,
    buffer: &mut Mut<InputBuffer>,
    mode: &BufferMode,
    moveset: Option<&MovesetDef>,
    run: Option<&Running>,
    running: Option<&ActionDef>,
    ttl: f32,
    reject: &mut impl FnMut(String, String),
) {
    let known = |input: &str| understands(input, moveset, running);
    if !intent.pressed.iter().any(|p| known(&p.input)) {
        return;
    }
    let mut i = 0;
    while i < intent.pressed.len() {
        if known(&intent.pressed[i].input) {
            let press = intent.pressed.remove(i);
            let left = press_life(running, run, &press.input, ttl);
            if let Some((input, reason)) = buffer_press(&mut buffer.queued, mode, Queued { input: press.input, dir: press.dir, left })
            {
                reject(input, reason);
            }
        } else {
            i += 1;
        }
    }
}

/// How long a press of `input` may wait for its action, made while `run` (an action of `def`)
/// is running: until the window a `queue` span holds it for opens, plus `ttl`; else `ttl`
/// ([`CancelDef::queue`](crate::def::CancelDef::queue)). The machine, a client's prediction and
/// the tools' answers all ask it.
pub fn press_life(def: Option<&ActionDef>, run: Option<&Running>, input: &str, ttl: f32) -> f32 {
    match (def, run) {
        (Some(def), Some(run)) => def.queue_until(input, run.prev, run.t).map_or(ttl, |open| open - run.t + ttl),
        _ => ttl,
    }
}

/// Puts `press` into `queued` by the buffer's rules. Returns a press that is dropped (the new
/// one, or the one it pushed out of a single slot) and why.
pub(crate) fn buffer_press(queued: &mut Vec<Queued>, mode: &BufferMode, mut press: Queued) -> Option<(String, String)> {
    match mode {
        BufferMode::PerInput => {
            queued.retain(|q| q.input != press.input);
            queued.push(press);
            None
        }
        BufferMode::SingleSlot { .. } => {
            let rank = |input: &str| mode.rank(input);
            let Some(waiting) = queued.first() else {
                queued.push(press);
                return None;
            };
            if rank(&press.input) < rank(&waiting.input) {
                let reason = format!("{} is waiting and comes first", waiting.input);
                return Some((press.input, reason));
            }
            let dropped = if waiting.input == press.input {
                press.left = press.left.max(waiting.left);
                None
            } else {
                Some((waiting.input.clone(), format!("{} took its place", press.input)))
            };
            queued.clear();
            queued.push(press);
            dropped
        }
    }
}

/// Every end of a run but a charge's turn (`Branched`): presses that waited for one of its
/// windows lose that wait (`left` at most `ttl`).
pub(crate) fn cap_buffer(buffer: &mut impl DerefMut<Target = InputBuffer>, ttl: f32) {
    if buffer.queued.iter().any(|q| q.left > ttl) {
        cap_queue(&mut buffer.queued, ttl);
    }
}

/// [`cap_buffer`] over a list of presses (a client's prediction keeps its own).
pub(crate) fn cap_queue(queued: &mut [Queued], ttl: f32) {
    for q in queued {
        q.left = q.left.min(ttl);
    }
}

/// The cooldown a run of `def` keeps when it ends in `end` (`combo`: it was cancelled into a
/// `Combo` continuation): `cost.cooldown_if_interrupted` of it, on the slot's key, when it was cut
/// (`Interrupted`, `Released`, or `Cancelled` into anything but a `Combo`) before its windup was over.
/// `None`: the cooldown stays as it is. Apply it with `Cooldowns::cut_to`: a slot already below the
/// share is left alone.
pub(crate) fn cut_share<'a>(def: &'a ActionDef, run: &'a Running, end: ActionEnd, combo: bool) -> Option<(&'a str, f32)> {
    let share = def.cost.cooldown_if_interrupted?;
    let cut = match end {
        ActionEnd::Interrupted | ActionEnd::Released => true,
        ActionEnd::Cancelled => !combo,
        ActionEnd::Finished | ActionEnd::Removed | ActionEnd::Branched => false,
    };
    (cut && run.t < def.windup_end() - EPS).then(|| (def.cooldown_key(&run.action), def.cost.cooldown * share))
}

/// What the engine's cut of `old` (`Interrupted`: a forced action, a death, a revive) leaves
/// behind besides its event: its extras go, the buffer's queue waits end ([`cap_buffer`]) and a
/// windup cut short keeps only its cooldown share ([`cut_share`]).
pub(crate) fn interrupted(world: &mut World, entity: Entity, old: &Running) {
    let db = world.resource::<ContentDb>();
    let ttl = CombatTuning::of(db).input_buffer;
    let share = db
        .get::<ActionDef>(&old.action)
        .and_then(|d| cut_share(d, old, ActionEnd::Interrupted, false))
        .map(|(key, secs)| (key.to_owned(), secs));
    if let Some(mut buffer) = world.get_mut::<InputBuffer>(entity) {
        cap_buffer(&mut buffer, ttl);
    }
    if let Some((key, secs)) = share
        && let Some(mut cooldowns) = world.get_mut::<Cooldowns>(entity)
    {
        cooldowns.cut_to(&key, secs);
    }
    if world.get::<RunExtras>(entity).is_some_and(|x| x.instance == old.instance) {
        world.entity_mut(entity).remove::<RunExtras>();
    }
    if world.get::<LeapRun>(entity).is_some_and(|x| x.instance == old.instance) {
        world.entity_mut(entity).remove::<LeapRun>();
    }
    // The body is solid again at once (the machine's next tick would also see to it).
    if world.get::<Ghost>(entity).is_some() {
        world.entity_mut(entity).remove::<Ghost>();
    }
}

/// A command that makes `entity`'s [`RunExtras`] agree with its run when it applies: `extras`
/// goes in if it belongs to the run running then, and extras of a run no longer running come out
/// (an observer of this tick's events may have started another run in between).
fn settle_extras(entity: Entity, extras: Option<RunExtras>) -> impl FnOnce(&mut World) {
    move |world: &mut World| {
        let Ok(mut e) = world.get_entity_mut(entity) else { return };
        let running = e.get::<ActionState>().and_then(|s| s.current.as_ref()).map(|r| r.instance);
        match extras {
            Some(x) if running == Some(x.instance) => {
                e.insert(x);
            }
            _ => {
                if e.get::<RunExtras>().is_some_and(|x| running != Some(x.instance)) {
                    e.remove::<RunExtras>();
                }
            }
        }
    }
}

/// [`settle_extras`] for the [`LeapRun`].
fn settle_leap(entity: Entity, leap: Option<LeapRun>) -> impl FnOnce(&mut World) {
    move |world: &mut World| {
        let run = world.get::<ActionState>(entity).and_then(|s| s.current.as_ref()).map(|r| (r.instance, r.action.clone()));
        // Whether the running action leaps now: a hot reload can take the leap out of it.
        let leaps = run.as_ref().is_some_and(|(_, action)| {
            world.get_resource::<ContentDb>().and_then(|db| db.get::<ActionDef>(action)).is_some_and(|d| d.leap.is_some())
        });
        let running = run.map(|(instance, _)| instance);
        let Ok(mut e) = world.get_entity_mut(entity) else { return };
        match leap {
            Some(x) if running == Some(x.instance) => {
                e.insert(x);
            }
            _ => {
                if e.get::<LeapRun>().is_some_and(|x| running != Some(x.instance) || !leaps) {
                    e.remove::<LeapRun>();
                }
            }
        }
    }
}

/// The charged action `run` turns into now, if it is due: its `charge` input has been held on
/// every tick so far, and `t` lies in `after..=until`.
pub(crate) fn charge_due<'d>(run: &Running, def: &'d ActionDef, extras: Option<&RunExtras>) -> Option<&'d str> {
    let charge = def.charge.as_ref()?;
    let held = extras.is_some_and(|x| x.held && x.instance == run.instance);
    let until = charge.until.unwrap_or_else(|| def.windup_end());
    (held && run.t >= charge.after - EPS && run.t <= until + EPS).then_some(charge.into.as_str())
}

/// What turning the run of `from` into `into` costs, paid only if all of it is there: the stamina
/// difference (each side's cost as the fighter's rules make it, [`stamina_cost`]; checked under `into`'s
/// stamina mode), one of `into`'s charges when it uses another charge than `from` (the start paid
/// `from`'s), and `into`'s cooldown, whose slot must be ready unless it is `from`'s own (this run started
/// it). `false`, and nothing is paid, when something is missing: the machine tries again on the next
/// tick, while the charge may still turn.
fn pay_branch(
    stamina: Option<&mut Stamina>,
    cooldowns: &mut Cooldowns,
    charges: Option<&mut Charges>,
    (from_id, from, from_cost): (&str, &ActionDef, f32),
    (into_id, into, into_cost): (&str, &ActionDef, f32),
) -> bool {
    let key = into.cooldown_key(into_id);
    if key != from.cooldown_key(from_id) && cooldowns.left(key) > EPS {
        return false;
    }
    // The charges the turn takes on top of what the start took: all of `into`'s, or the difference when it
    // takes the same charge.
    let (charge, count) = match &into.cost.charge {
        Some(c) if from.cost.charge.as_ref() == Some(c) => (Some(c), into.cost.charge_count.saturating_sub(from.cost.charge_count)),
        Some(c) => (Some(c), into.cost.charge_count),
        None => (None, 0),
    };
    if let Some(name) = charge
        && count > 0
        && !charges.as_deref().and_then(|c| c.get(name)).is_some_and(|c| c.current >= count)
    {
        return false;
    }
    let extra = into_cost - from_cost;
    if extra > 0.0
        && let Some(s) = stamina.as_deref()
    {
        let affordable = match into.cost.stamina_mode {
            StaminaMode::Full => s.current + EPS >= extra,
            StaminaMode::Any => s.current > EPS,
        };
        if !affordable {
            return false;
        }
    }
    if extra > 0.0
        && let Some(s) = stamina
    {
        s.spend(extra);
    }
    if let Some(name) = charge
        && count > 0
        && let Some(c) = charges.and_then(|c| c.get_mut(name))
    {
        c.current = c.current.saturating_sub(count);
    }
    cooldowns.start(key, into.cost.cooldown);
    true
}

/// Whether a fighter with `moveset`, running `running`, has a use for a press of `input`:
/// its moveset maps it or binds it in its `context`, or the running action combos into
/// something with it.
pub(crate) fn understands(input: &str, moveset: Option<&MovesetDef>, running: Option<&ActionDef>) -> bool {
    moveset.is_some_and(|m| m.inputs.contains_key(input) || m.context.iter().any(|b| b.input == input))
        || running.is_some_and(|d| {
            d.cancels
                .iter()
                .any(|c| c.into.iter().any(|t| matches!(t, crate::def::Transition::Combo(i, _) if i == input)))
        })
}

/// The perfect window of a guard that needs a fresh press (`perfect_needs_press`) stays shut
/// unless such a press started this very run ([`GuardArm::run`]). The machine, a forced start and a
/// client's prediction all call it after the windows are worked out, so the pipeline, `observe`
/// and the prediction read the same window.
pub(crate) fn gate_perfect(run: &mut Running, def: &ActionDef, arm: Option<&GuardArm>) {
    if run.windows.perfect
        && def.guard.as_ref().is_some_and(|g| g.perfect_needs_press.is_some())
        && arm.is_none_or(|a| a.run != Some(run.instance))
    {
        run.windows.perfect = false;
    }
}

/// Takes what starting `action` costs: `stamina`, its stamina cost as the fighter's rules make it
/// ([`stamina_cost`]), its cooldown and its charge.
fn pay(f: &mut FighterDataItem, action: &str, def: &ActionDef, stamina: f32) {
    if stamina > 0.0
        && let Some(s) = f.stamina.as_mut()
    {
        s.spend(stamina);
    }
    f.cooldowns.start(def.cooldown_key(action), def.cost.cooldown);
    if let Some(charge) = &def.cost.charge
        && let Some(c) = f.charges.as_mut().and_then(|c| c.get_mut(charge))
    {
        c.current = c.current.saturating_sub(def.cost.charge_count.max(1));
    }
}

/// The ground-plane direction a fighter's actions aim along because it looks that way: the yaw of
/// its `Intent::look` when its body has `face: Look` ([`CharacterParams::bound_look`]), else
/// `None` and its actions steer as they always did. A fighter without a character controller has none.
pub(crate) fn looked_at(
    body: Option<(&CharacterController, Option<&CharacterParams>)>,
    db: &ContentDb,
    intent: &Intent,
) -> Option<Vec3> {
    let (controller, own) = body?;
    controller.resolve(own, Some(db)).bound_look(intent).map(|gaze| gaze.flat_dir())
}

/// Where an action points when it starts. `look`: the direction the fighter looks along, if its
/// body faces its look ([`looked_at`]); an aiming action goes there unless a press names a
/// direction, before the lock-on, the aim point and the movement input.
pub(crate) fn steer(
    def: &ActionDef,
    dir: Option<Vec3>,
    intent: &Intent,
    look: Option<Vec3>,
    here: Vec3,
    forward: Vec3,
    target: Option<Vec3>,
) -> Vec3 {
    let toward = |p: Vec3| flat(p - here).try_normalize();
    let pressed = dir.and_then(|d| flat(d).try_normalize());
    let moving = flat(intent.move_dir).try_normalize();
    let pick = match def.steer {
        Steer::Aim => pressed.or(look).or_else(|| target.and_then(toward)).or_else(|| intent.aim.and_then(toward)).or(moving),
        Steer::Move | Steer::MoveOrBack => pressed.or(moving),
    };
    // A body that faces its look faces it now; one that does not, as it faced after its last tick.
    let ahead = look.or_else(|| flat(forward).try_normalize()).unwrap_or(Vec3::NEG_Z);
    // Nothing pressed and nothing held: a backstep leaves the way the body does not face.
    pick.unwrap_or(if def.steer == Steer::MoveOrBack { -ahead } else { ahead })
}

/// The victim of the pair the current run belongs to (`Pairing::instance` is the run's): what a paired action's aim follows
/// instead of the lock-on while it runs. Only while the pair still holds it ([`keeps_victim`]: a dead body never, a downed one
/// only by a pair that takes the downed, a revive, which holds no other): one that fell or stood up after `tend_pairs`' pass
/// (a status, a bleed-out, a tool) is not followed until that pass lets it go; the aim falls back to the lock-on, as it does
/// once the pair is lost. `fallen` says of a body whether it is dead and whether it is down.
pub(crate) fn pair_victim_of(
    db: &ContentDb,
    pairing: Option<&Pairing>,
    run: Option<&Running>,
    fallen: impl Fn(AiId) -> (bool, bool),
) -> Option<AiId> {
    let run = run?;
    let pairing = pairing.filter(|p| p.instance == run.instance)?;
    let def = db.get::<ActionDef>(&pairing.action)?;
    let state = def.pair.as_ref()?.select.state;
    let (dead, down) = fallen(pairing.victim);
    keeps_victim(state, dead, down, past_revive(def, run.t)).then_some(pairing.victim)
}

/// While an action tracks, it turns toward the look (of a body that faces it), the target or the aim point.
fn track(def: &ActionDef, intent: &Intent, look: Option<Vec3>, here: Vec3, target: Option<Vec3>) -> Option<Vec3> {
    if matches!(def.steer, Steer::Move | Steer::MoveOrBack) {
        return None;
    }
    let toward = |p: Vec3| flat(p - here).try_normalize();
    look.or_else(|| target.and_then(toward)).or_else(|| intent.aim.and_then(toward))
}

/// Where a run of `def` starts pointing: [`steer`]; an action with `turn` spans starts from the
/// body's facing, with no snap, and turns from there at the spans' rates.
pub(crate) fn start_facing(
    def: &ActionDef,
    dir: Option<Vec3>,
    intent: &Intent,
    look: Option<Vec3>,
    here: Vec3,
    forward: Vec3,
    target: Option<Vec3>,
) -> Vec3 {
    if def.turn.is_empty() {
        steer(def, dir, intent, look, here, forward, target)
    } else {
        look.or_else(|| flat(forward).try_normalize()).unwrap_or(Vec3::NEG_Z)
    }
}

/// One tick of a running action: its time moves on (looping a held span while its input
/// is held), it turns while it tracks, and it moves the body. Returns the stretches of the
/// timeline the tick covered and the displacement it makes (the machine's and a client's
/// prediction's). `extras`: the run's [`RunExtras`], when it has them (the hold for a charge,
/// turn spans, the assist and its closing cap). `scale`: what a leap stretches `displacement` by
/// ([`LeapRun::scale`]; 1 for any other run).
#[allow(clippy::too_many_arguments, reason = "one tick of a run: the run, its definition, what it reads and what moves it")]
pub(crate) fn step_run(
    run: &mut Running,
    def: &ActionDef,
    extras: Option<&mut RunExtras>,
    scale: f32,
    intent: &Intent,
    look: Option<Vec3>,
    here: Vec3,
    target: Option<Vec3>,
    dt: f32,
) -> (Vec<(f32, f32)>, Vec3) {
    let holding = def.hold.as_ref().is_some_and(|h| intent.is_held(&h.input));
    let spans = advance(run, def, dt, holding);
    let moved =
        |spans: &[(f32, f32)]| spans.iter().map(|&(a, b)| def.displacement_at(b) - def.displacement_at(a)).sum::<f32>() * scale;
    let external = match extras {
        None => {
            if run.t <= def.track_until + EPS
                && let Some(dir) = track(def, intent, look, here, target)
            {
                run.facing = dir;
            }
            run.facing * moved(&spans)
        }
        Some(x) => {
            x.held &= run.input.as_deref().is_some_and(|input| intent.is_held(input));
            turn_run(run, def, x, intent, look, here, target, dt);
            let along = moved(&spans);
            run.facing * along - over_close(def, x, run.facing, along, here, target)
        }
    };
    (spans, external)
}

/// What of a tick's displacement (`along` metres along `facing`) goes past the closing cap
/// (`assist.close`), as a vector toward the target to take off it: the approach to `target` that
/// moving along the turned `facing` gives beyond moving along the run's first facing
/// ([`RunExtras::from`]), past what is left of the cap. What it lets through adds to
/// [`RunExtras::closed`]; a tick whose turning gains nothing changes nothing.
fn over_close(def: &ActionDef, x: &mut RunExtras, facing: Vec3, along: f32, here: Vec3, target: Option<Vec3>) -> Vec3 {
    let (Some(cap), Some(there)) = (def.assist.as_ref().and_then(|a| a.close), target) else { return Vec3::ZERO };
    let Some(to) = flat(there - here).try_normalize() else { return Vec3::ZERO };
    let gain = along * (facing.dot(to) - x.from.dot(to));
    if gain <= 0.0 {
        return Vec3::ZERO;
    }
    let through = gain.min((cap - x.closed).max(0.0));
    x.closed += through;
    to * (gain - through)
}

/// The height of a body for a line of sight: its `CharacterParams::height`, else 1.7 m.
pub(crate) fn body_height(params: Option<&CharacterParams>) -> f32 {
    params.map_or(1.7, |p| p.height)
}

/// Whether no `LayerMask::WORLD` collider lies between the chests of two bodies, each given as
/// `(entity, feet, height)`, at `sight` of each one's height; their own colliders never count. The
/// same ray a `LineOfSight` hit casts; the assist's `sight` asks it.
pub(crate) fn in_sight(physics: &PhysicsWorld, me: (Entity, Vec3, f32), them: (Entity, Vec3, f32), sight: f32) -> bool {
    let (from, to) = (me.1 + Vec3::Y * (sight * me.2), them.1 + Vec3::Y * (sight * them.2));
    let along = to - from;
    let Ok(dir) = Dir3::new(along) else { return true };
    let filter = QueryFilter::new(LayerMask::WORLD).excluding(me.0).excluding(them.0);
    physics.raycast(from, dir, along.length(), &filter).is_none()
}

/// The lock-on target a run of `def` tracks: none while its assist is off, because the fighter
/// holds `assist_off` or because the assist needs `sight` and `sees` (asked only then) says the
/// target is out of it. An action without `assist` tracks its target as it always did.
pub(crate) fn tracked(def: &ActionDef, intent: &Intent, target: Option<Vec3>, sees: impl FnOnce() -> bool) -> Option<Vec3> {
    let assist = def.assist.as_ref().filter(|_| target.is_some());
    let off = assist.is_some_and(|a| intent.is_held(ASSIST_OFF) || (a.sight && !sees()));
    target.filter(|_| !off)
}

/// How a run with extras turns this tick: toward the target (or the aim), kept within
/// `assist.angle` degrees of [`RunExtras::start_facing`] while there is a lock-on target; snapping
/// while `t <= track_until` (no `turn` spans: the old rule), else at most the open spans' rate,
/// toward the target, the aim, else the movement input (a player steering the swing).
#[allow(clippy::too_many_arguments, reason = "one tick of a run: the run, its definition, what it reads and what moves it")]
fn turn_run(
    run: &mut Running,
    def: &ActionDef,
    x: &RunExtras,
    intent: &Intent,
    look: Option<Vec3>,
    here: Vec3,
    target: Option<Vec3>,
    dt: f32,
) {
    let desired = if def.turn.is_empty() {
        track(def, intent, look, here, target)
    } else {
        track(def, intent, look, here, target).or_else(|| flat(intent.move_dir).try_normalize())
    };
    let Some(desired) = desired else { return };
    let desired = match (&def.assist, target) {
        (Some(assist), Some(_)) => clamp_flat(desired, x.start_facing, assist.angle),
        _ => desired,
    };
    if def.turn.is_empty() {
        if run.t <= def.track_until + EPS {
            run.facing = desired;
        }
    } else if let Some(rate) = def.turn_rate(run.prev, run.t) {
        run.facing = rotate_flat(run.facing, desired, rate * dt);
    }
}

/// The yaw of a flat direction, as the character controller turns bodies (`-Z` is 0).
fn yaw(d: Vec3) -> f32 {
    ops::atan2(-d.x, -d.z)
}

/// The flat direction of yaw `y`.
fn from_yaw(y: f32) -> Vec3 {
    let (s, c) = ops::sin_cos(y);
    Vec3::new(-s, 0.0, -c)
}

/// An angle wrapped into `[-PI, PI)`.
fn wrap(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// `from` turned toward `to` about Y by at most `max_deg` degrees (both flat directions).
pub(crate) fn rotate_flat(from: Vec3, to: Vec3, max_deg: f32) -> Vec3 {
    if max_deg <= 0.0 {
        return from;
    }
    let current = yaw(from);
    let delta = wrap(yaw(to) - current);
    let max = max_deg.to_radians();
    if delta.abs() <= max {
        return to;
    }
    from_yaw(current + max.copysign(delta))
}

/// `dir` kept within `max_deg` degrees of `center` (both flat directions).
fn clamp_flat(dir: Vec3, center: Vec3, max_deg: f32) -> Vec3 {
    let c = yaw(center);
    let delta = wrap(yaw(dir) - c);
    let max = max_deg.to_radians();
    if delta.abs() <= max {
        dir
    } else {
        from_yaw(c + max.copysign(delta))
    }
}

/// Whether instant `at` lies in the tick span `(lo, hi]`.
fn crosses(at: f32, lo: f32, hi: f32) -> bool {
    at > lo + EPS && at <= hi + EPS
}

/// What the stretches `spans` of a tick crossed of `def`'s timeline, in order: each effect goes to
/// `effect`, the telegraph to `telegraph`. The machine's runs and the combat objects' share it.
pub(crate) fn crossed(def: &ActionDef, spans: &[(f32, f32)], mut effect: impl FnMut(&Effect), mut telegraph: impl FnMut()) {
    for &(lo, hi) in spans {
        for e in def.effects.iter().filter(|e| crosses(e.at, lo, hi)) {
            effect(&e.effect);
        }
        if def.telegraph.as_ref().is_some_and(|tg| crosses(tg.at, lo, hi)) {
            telegraph();
        }
    }
}

/// Moves `run` on by `dt`, looping inside the hold span while `holding`. Returns the
/// stretches of the timeline the tick covered (two when it looped).
pub(crate) fn advance(run: &mut Running, def: &ActionDef, dt: f32, holding: bool) -> Vec<(f32, f32)> {
    let prev = run.t;
    let next = prev + dt;
    let loop_span = def.hold.as_ref().filter(|_| holding).and_then(|h| def.span(&h.span)).filter(|(a, b)| b - a > EPS);
    // Held: the loop's input is down and the run has reached its span.
    match loop_span {
        Some((a, b)) if prev >= a - EPS && prev < b + EPS => run.held += dt,
        Some(_) => {}
        None => run.held = 0.0,
    }
    match loop_span {
        Some((a, b)) if prev < b - EPS && next >= b - EPS => {
            // `next` may sit a hair short of `b` (sums of tick lengths): a negative
            // remainder would wrap to the end of the span and the loop would drop out.
            let t = a + (next - b).max(0.0).rem_euclid(b - a);
            run.prev = a;
            run.t = t;
            vec![(prev, b), (a, t)]
        }
        _ => {
            run.prev = prev;
            run.t = next;
            vec![(prev, next.min(run.length))]
        }
    }
}

/// What starting an action does at once: effects and a telegraph at time 0.
fn begin(
    entity: Entity,
    def: &ActionDef,
    run: &Running,
    feet: Vec3,
    effects: &mut Vec<(Entity, String, Effect)>,
    trigger: &mut impl FnMut(Telegraph),
) {
    for e in def.effects.iter().filter(|e| e.at <= EPS) {
        effects.push((entity, run.action.clone(), e.effect.clone()));
    }
    if def.telegraph.as_ref().is_some_and(|tg| tg.at <= EPS) {
        trigger(telegraph(entity, def, run, feet));
    }
}

pub(crate) fn telegraph(attacker: Entity, def: &ActionDef, run: &Running, feet: Vec3) -> Telegraph {
    let zones = action_zones(def, run.t, feet, run.facing);
    let first = zones.first();
    let tg = def.telegraph.clone().unwrap_or_else(|| crate::def::TelegraphDef {
        at: 0.0,
        symbol: "!".into(),
        sound: None,
        zone: None,
        zone_at: default(),
    });
    Telegraph {
        attacker,
        action: run.action.clone(),
        lands_in: first.map_or(0.0, |z| z.lands_in),
        zone: first.map(|z| z.shape),
        symbol: tg.symbol,
        sound: tg.sound,
    }
}

/// Tells the controller what the action wants: its displacement this tick, whether it
/// owns the body, where to face; no sprinting without stamina, nor after sprinting it
/// dry until the button is let go.
fn steer_body(f: &mut FighterDataItem, db: &ContentDb, external: Vec3) {
    let rule = sprint_rule(db, f.motion, f.body);
    let held = f.intent.is_held(SPRINT);
    if let Some(s) = f.stamina.as_mut() {
        s.settle_exhaustion(rule, held);
    }
    let out_of_stamina =
        !free_sprint(f.modifiers, f.motion) && f.stamina.as_ref().is_some_and(|s| s.sprint_blocked(rule, held));
    let Some(loco) = f.loco.as_mut() else { return };
    steer_locomotion(loco, f.state.current.as_ref(), db, f.dead || f.downed, out_of_stamina, external);
}

/// Whether a sprint costs no stamina, so an empty one does not stop it: the body's
/// `MoveModifiers::sprint_cost` is 0 and it is not in water (a fast swim always costs).
/// The machine and a client's prediction ask the same question.
pub(crate) fn free_sprint(modifiers: Option<&MoveModifiers>, motion: Option<&MotionState>) -> bool {
    modifiers.is_some_and(|m| m.sprint_cost == 0.0) && !motion.is_some_and(|m| m.mode == MotionMode::Water)
}

/// What `run` (the body's action, if any) wants of the controller this tick: the
/// displacement `external`, whether it owns the body (no walking), where to face, a slower
/// walk, no sprint. The machine's [`steer_body`] and a client's prediction both use it.
pub(crate) fn steer_locomotion(
    loco: &mut LocomotionInput,
    run: Option<&Running>,
    db: &ContentDb,
    dead: bool,
    out_of_stamina: bool,
    external: Vec3,
) {
    let def = run.and_then(|r| db.get::<ActionDef>(&r.action));
    // A walk span open: the body walks at its share of the walking speed, the action's own displacement on top.
    let walk = run.zip(def).and_then(|(r, d)| d.walk_factor(r.prev, r.t));
    let owns = dead || (run.is_some() && walk.is_none() && def.is_none_or(|d| d.move_speed.is_none()));
    loco.external = external;
    loco.locked = owns;
    if let Some(r) = run
        && def.is_none_or(|d| d.face == Face::Run)
    {
        loco.face = Some(r.facing);
    }
    if let Some(m) = walk.or_else(|| def.filter(|_| run.is_some()).and_then(|d| d.move_speed)) {
        loco.move_dir *= m;
    }
    if owns || walk.is_some() || out_of_stamina {
        loco.sprint = false;
    }
}

/// `SimSet::Resolve`, after hits: actions whose time ran out in this tick end now. The
/// machine leaves them in place so the tick that ends an action still hits and
/// defends (hit windows, i-frames, guards), as the timing rules say.
#[allow(clippy::type_complexity, reason = "a Bevy query")]
pub(crate) fn finish_actions(
    db: Res<ContentDb>,
    mut fighters: Query<(Entity, Option<&AiId>, &mut ActionState, Option<&mut InputBuffer>, Has<RunExtras>, Has<LeapRun>, Has<Ghost>)>,
    mut commands: Commands,
) {
    // The query follows `Entity` order, which a window shifts: end them in `AiId` order.
    let mut ending: Vec<(u32, Entity)> = Vec::new();
    for (entity, id, state, ..) in &fighters {
        if state.current.as_ref().is_some_and(|r| r.t >= r.length - EPS) {
            ending.push((id.map_or(u32::MAX, |i| i.0), entity));
        }
    }
    if ending.is_empty() {
        return;
    }
    ending.sort();
    let ttl = CombatTuning::of(&db).input_buffer;
    for (_, entity) in ending {
        let Ok((_, _, mut state, buffer, has_extras, has_leap, has_ghost)) = fighters.get_mut(entity) else { continue };
        if let Some(run) = state.current.take() {
            if let Some(mut buffer) = buffer {
                cap_buffer(&mut buffer, ttl);
            }
            if has_extras {
                commands.queue(settle_extras(entity, None));
            }
            if has_leap {
                commands.queue(settle_leap(entity, None));
            }
            if has_ghost {
                commands.entity(entity).try_remove::<Ghost>();
            }
            let then = db.get::<ActionDef>(&run.action).and_then(|d| d.then.clone());
            commands.trigger(ActionEnded::of(entity, &run, ActionEnd::Finished));
            // `then` starts in this tick, after the end's observers; one of them that started
            // something else (a reaction) keeps it.
            if let Some(then) = then {
                commands.queue(move |world: &mut World| {
                    if world.get::<ActionState>(entity).is_some_and(|s| s.current.is_none()) {
                        force_action(world, entity, &then);
                    }
                });
            }
        }
    }
}

/// `SimSet::Resolve`, in [`ActionSystems::Hits`](crate::ActionSystems::Hits) after the hits are
/// resolved: a run with `miss_recovery` whose time reached the end of its last hit window is
/// checked once; if none of its hits landed by then (the checking tick's own hits count), it lasts
/// `miss_recovery` longer ([`ActionMissed`]). Before `finish_actions`, so a run whose time runs
/// out in the checking tick goes on into its extra recovery. In `AiId` order.
#[allow(clippy::type_complexity, reason = "a Bevy query")]
pub(crate) fn check_misses(
    db: Res<ContentDb>,
    mut fighters: Query<(Entity, Option<&AiId>, &mut ActionState, &mut RunExtras)>,
    mut commands: Commands,
) {
    let mut due: Vec<(u32, Entity)> = Vec::new();
    for (entity, id, state, x) in &fighters {
        let Some(run) = state.current.as_ref().filter(|r| r.instance == x.instance && !x.checked) else { continue };
        let Some(def) = db.get::<ActionDef>(&run.action).filter(|d| d.miss_recovery > 0.0) else { continue };
        if run.t >= def.last_hit_end() - EPS {
            due.push((id.map_or(u32::MAX, |i| i.0), entity));
        }
    }
    due.sort();
    for (_, entity) in due {
        let Ok((_, _, mut state, mut x)) = fighters.get_mut(entity) else { continue };
        let Some(run) = state.current.as_mut() else { continue };
        let Some(def) = db.get::<ActionDef>(&run.action) else { continue };
        x.checked = true;
        if !x.landed {
            x.extra += def.miss_recovery;
            run.length = def.length() + x.extra;
            commands.trigger(ActionMissed { entity, action: run.action.clone(), extra: def.miss_recovery });
        }
    }
}

/// Makes `entity`'s running action `seconds` longer (an [`ActionMissed`] observer's bonus, a
/// game rule). `false`, and nothing changes, when it runs nothing, `seconds` is not a number
/// `>= 0`, or this machine does not simulate (a co-op client: the host's state brings the longer
/// run, so an observer that calls it on every machine counts it once). Its cancel windows stay
/// where they are (they are times of the run).
pub fn extend_recovery(world: &mut World, entity: Entity, seconds: f32) -> bool {
    let simulates = world.get_resource::<NetRole>().is_none_or(|r| r.simulates());
    if !simulates || !(seconds >= 0.0 && seconds.is_finite()) {
        return false;
    }
    let Some(run) = world.get::<ActionState>(entity).and_then(|s| s.current.as_ref()) else { return false };
    let (instance, action, facing) = (run.instance, run.action.clone(), run.facing);
    let Some(length) = world.resource::<ContentDb>().get::<ActionDef>(&action).map(ActionDef::length) else {
        return false;
    };
    let mut x = world.get::<RunExtras>(entity).copied().filter(|x| x.instance == instance).unwrap_or(RunExtras {
        instance,
        start_facing: facing,
        from: facing,
        ..default()
    });
    x.extra += seconds;
    world.entity_mut(entity).insert(x);
    if let Some(run) = world.get_mut::<ActionState>(entity).and_then(|s| s.into_inner().current.as_mut()) {
        run.length = length + x.extra;
    }
    true
}

/// Whether `entity` is intangible now: its running action has an `intangible` span open in the tick
/// (a burrowed body). Hits find nothing of it (no hit, no ledger mark), a pair does not pick it as a
/// victim, and perception does not see it. Computed from the action, never read from how it looks.
pub fn intangible(world: &World, entity: Entity) -> bool {
    let (Some(db), Some(state)) = (world.get_resource::<ContentDb>(), world.get::<ActionState>(entity)) else { return false };
    state_intangible(db, state)
}

/// [`intangible`] over a state a system already has.
pub fn state_intangible(db: &ContentDb, state: &ActionState) -> bool {
    let Some(run) = state.current.as_ref() else { return false };
    db.get::<ActionDef>(&run.action).is_some_and(|d| d.intangible_in(run.prev, run.t))
}

/// `SimSet::Act`, after the machine: the effects actions reached, in order.
pub(crate) fn apply_effects(world: &mut World) {
    let pending = std::mem::take(&mut world.resource_mut::<PendingEffects>().0);
    for (entity, action, effect) in pending {
        if world.get_entity(entity).is_ok() {
            apply_effect(world, entity, &action, &effect);
        }
    }
}

/// Applies one effect of `action` to `entity` now.
pub fn apply_effect(world: &mut World, entity: Entity, action: &str, effect: &Effect) {
    match effect {
        Effect::Heal(amount) => {
            // A downed body stands up only by a revive.
            if world.get::<Dead>(entity).is_some() || world.get::<Downed>(entity).is_some() {
                return;
            }
            let Some(mut health) = world.get_mut::<Health>(entity) else { return };
            let before = health.current;
            health.current = (health.current + amount).min(health.max);
            let (gained, hp) = (health.current - before, health.current);
            world.trigger(Healed { entity, amount: gained, hp });
        }
        Effect::RestoreStamina(amount) => {
            if let Some(mut s) = world.get_mut::<Stamina>(entity) {
                s.current = (s.current + amount).min(s.max);
            }
        }
        Effect::Status(status, amount) => add_status_buildup(world, entity, status, *amount),
        Effect::Barrier { amount, seconds, of_max } => {
            let amount = if *of_max {
                let Some(max) = world.get::<Health>(entity).map(|h| h.max) else { return };
                amount * max
            } else {
                *amount
            };
            grant_barrier(world, entity, amount, *seconds);
        }
        Effect::Sound(sound) => world.trigger(ActionSound { entity, action: action.to_owned(), sound: sound.clone() }),
        Effect::Spawn(spawn) => {
            crate::objects::spawn_objects(world, entity, action, spawn, None);
        }
        Effect::Fire(fire) => crate::shot::fire(world, entity, action, fire),
        Effect::Custom(name) => {
            let hook = world.resource::<ActionEffects>().0.iter().find(|(n, _)| n == name).map(|(_, f)| *f);
            match hook {
                Some(hook) => hook(world, entity),
                None => {
                    let text =
                        format!("action `{action}`: no effect hook `{name}`; register it with app.add_action_effect");
                    report_once(world, format!("effect {name}"), text);
                }
            }
        }
        Effect::Refill { charge, from, per_round } => {
            refill(world, entity, charge, from, *per_round);
        }
        Effect::Revive { health, grace } => {
            // The victim of the pair this action holds, if it is still down.
            let victim = world
                .get::<crate::context::Pairing>(entity)
                .filter(|p| p.action == action)
                .and_then(|p| world.resource::<AiIndex>().get(p.victim));
            if let Some(victim) = victim {
                crate::combat::revive_downed(world, victim, *health, *grace, Some(entity));
            }
        }
    }
}

/// Moves charges from the charge `from` of `entity` into its charge `charge`: up to the room `charge`
/// has and what `from` holds, one when `per_round`. Returns how many moved (0: a charge the body lacks,
/// no room, nothing to take) and says so with [`Refilled`] when it is above 0. The `Refill` effect
/// calls it at its moment; a game may call it directly.
pub fn refill(world: &mut World, entity: Entity, charge: &str, from: &str, per_round: bool) -> u32 {
    if charge == from {
        return 0;
    }
    let Some(mut charges) = world.get_mut::<Charges>(entity) else { return 0 };
    let (Some(into), Some(source)) = (charges.get(charge), charges.get(from)) else { return 0 };
    let room = into.max.saturating_sub(into.current);
    let taken = room.min(source.current).min(if per_round { 1 } else { u32::MAX });
    if taken == 0 {
        return 0;
    }
    if let Some(c) = charges.get_mut(charge) {
        c.current += taken;
    }
    let Some(source) = charges.get_mut(from) else { return 0 };
    source.current -= taken;
    let left = source.current;
    let (current, max) = charges.get(charge).map_or((0, 0), |c| (c.current, c.max));
    world.trigger(Refilled { entity, charge: charge.to_owned(), from: from.to_owned(), taken, current, max, left });
    taken
}

/// Puts `entity` into `action` now, cutting the running one short (staggers, deaths,
/// flinches, the `stagger` tool). `false` if the action is not in content.
pub fn force_action(world: &mut World, entity: Entity, action: &str) -> bool {
    let Some(def) = world.resource::<ContentDb>().get::<ActionDef>(action).cloned() else {
        report_once(world, format!("action {action}"), format!("action `{action}` is not in content; nothing forced"));
        return false;
    };
    let Some(feet) = world.get::<Transform>(entity).map(|t| (t.translation, *t.forward())) else { return false };
    let Some(mut state) = world.get_mut::<ActionState>(entity) else { return false };
    let facing = state.current.as_ref().map_or(feet.1, |r| r.facing);
    let instance = state.started;
    state.started += 1;
    let mut run = Running::new(action, &def, instance, facing, None, None);
    // A forced guard that needs a fresh press had none: no perfect window.
    gate_perfect(&mut run, &def, None);
    let mut effects = Vec::new();
    let mut telegraphs = Vec::new();
    begin(entity, &def, &run, feet.0, &mut effects, &mut |t| telegraphs.push(t));
    // `from` is the closing cap's baseline: the run's own facing, as `RunExtras::start` sets it.
    let extras =
        def.needs_extras().then(|| RunExtras { instance, start_facing: run.facing, from: run.facing, ..default() });
    let leap = def.leap.is_some().then(|| LeapRun { instance, ..default() });
    let replaced = state.current.replace(run);
    if let Some(old) = &replaced {
        interrupted(world, entity, old);
    }
    if let Some(x) = extras {
        world.entity_mut(entity).insert(x);
    }
    if let Some(x) = leap {
        world.entity_mut(entity).insert(x);
    }
    if let Some(old) = replaced {
        world.trigger(ActionEnded::of(entity, &old, ActionEnd::Interrupted));
    }
    world.trigger(ActionStarted { entity, action: action.to_owned(), input: None, predicted: false });
    for t in telegraphs {
        world.trigger(t);
    }
    for (e, action, effect) in effects {
        apply_effect(world, e, &action, &effect);
    }
    true
}

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    use super::*;
    use crate::def::HitShape;
    use shards_content::{ContentDb, ContentKinds};

    thread_local! {
        /// Heap allocations this thread has made (tests run in parallel, one count each).
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    /// Counts allocations, to hold the per-tick paths to none.
    struct Counting;

    // SAFETY: every call goes to the system allocator unchanged; the counter only observes.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static COUNTING: Counting = Counting;

    /// How many times `f` allocated on this thread.
    fn allocations_in(f: impl FnOnce()) -> usize {
        let before = ALLOCATIONS.with(Cell::get);
        f();
        ALLOCATIONS.with(Cell::get) - before
    }

    fn parse_def(text: &str) -> ActionDef {
        ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
            .from_str(text)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    fn db() -> ContentDb {
        let mut kinds = ContentKinds::default();
        kinds.register::<ActionDef>();
        kinds.register::<MovesetDef>();
        kinds.register::<crate::status::StatusDef>();
        kinds.register::<CombatTuning>();
        let actions = r#"{
            "light_1": (net_id: 1, phases: [("windup", 0.2), ("active", 0.1), ("recovery", 0.3)], cost: (stamina: 10),
                cancels: [(span: Secs(0.3, 0.6), into: [Combo("light", "light_2"), Input("dodge")])]),
            "light_2": (net_id: 2, phases: [("windup", 0.2), ("recovery", 0.3)], cost: (stamina: 10)),
            "heavy": (net_id: 3, phases: [("windup", 0.5), ("recovery", 0.5)], cost: (stamina: 30, cooldown: 2)),
            "roll": (net_id: 4, phases: [("roll", 0.4)], cost: (stamina: 20), steer: Move),
            "drink": (net_id: 5, phases: [("drink", 1.0)], cost: (charge: "flask")),
            "block": (net_id: 6, phases: [("raise", 0.1), ("hold", 0.2), ("lower", 0.1)],
                hold: (input: "block", span: Phase("hold")), guard: (span: Phases("raise", "hold"))),
        }"#;
        let movesets = r#"{ "hero": (inputs: {"light": "light_1", "heavy": "heavy", "dodge": "roll",
            "use_item.flask": "drink", "block": "block"}) }"#;
        let sources = [
            shards_content::db::Source { path: "content/a.action.ron".into(), text: actions.into() },
            shards_content::db::Source { path: "content/m.moveset.ron".into(), text: movesets.into() },
        ];
        let db = ContentDb::build(&kinds, &sources);
        assert_eq!(db.errors().count(), 0, "{:?}", db.errors().map(ToString::to_string).collect::<Vec<_>>());
        db
    }

    const DT: f32 = 1.0 / 60.0;

    fn running(db: &ContentDb, id: &str, t: f32) -> ActionState {
        let def = db.get::<ActionDef>(id).unwrap();
        let mut run = Running::new(id, def, 0, Vec3::NEG_Z, None, Some("x".into()));
        run.t = t;
        run.prev = (t - DT).max(0.0);
        run.refresh(def, 0.0);
        ActionState { current: Some(run), started: 1 }
    }

    fn view<'a>(
        db: &'a ContentDb,
        state: &'a ActionState,
        stamina: Option<&'a Stamina>,
        held: &'a [String],
    ) -> FighterView<'a> {
        FighterView { stamina, ..FighterView::new(db, "hero", state, held, false, None) }
    }

    #[test]
    fn a_running_action_follows_a_reloaded_definition() {
        let parse = parse_def;
        let first = parse(
            r#"(net_id: 1, anim: "a", phases: [("windup", 0.2), ("active", 0.1), ("recovery", 0.3)],
                hits: [(span: Phase("active"), shape: Cone(range: 2.0, angle: 90), damage: 20)])"#,
        );
        let second = parse(
            r#"(net_id: 1, anim: "b", phases: [("windup", 0.5), ("active", 0.1), ("recovery", 0.3)],
                hits: [(span: Phase("active"), shape: Circle(radius: 4.0), damage: 20)])"#,
        );
        let mut run = Running::new("swing", &first, 0, Vec3::NEG_Z, None, Some("light".into()));
        assert_eq!((run.zone.as_deref(), run.anim.as_deref()), (Some("cone 2m 90°"), Some("a")));
        assert!((run.zone_until - 0.3).abs() < 1e-6, "{}", run.zone_until);

        // The content was reloaded under the running action: the next tick's refresh follows.
        run.refresh(&second, 0.0);
        assert_eq!((run.zone.as_deref(), run.anim.as_deref()), (Some("circle 4m"), Some("b")));
        assert!((run.zone_until - 0.6).abs() < 1e-6, "the later hit span: {}", run.zone_until);
        let settled = run.clone();
        run.refresh(&second, 0.0);
        assert_eq!(run, settled, "a tick that changes nothing changes nothing");

        // And a definition that lost them.
        let bare = parse(r#"(net_id: 1, phases: [("windup", 0.2), ("recovery", 0.3)])"#);
        run.refresh(&bare, 0.0);
        assert_eq!((run.zone.as_deref(), run.anim.as_deref(), run.zone_until), (None, None, 0.0));
    }

    #[test]
    fn a_tick_that_changes_nothing_allocates_nothing() {
        // `refresh` runs every tick for every fighter with a running action: whatever the
        // zone's shape, following the definition must not build a string while it is unchanged.
        let shapes = [
            "Sphere(radius: 1.5)",
            "Capsule(radius: 0.5, length: 2.0)",
            "Cone(range: 3.0, angle: 90)",
            "Box(width: 1.0, height: 2.0, depth: 2.5)",
            "Circle(radius: 4.5)",
            "Line(length: 8.0, width: 1.6)",
        ];
        for shape in shapes {
            let def = parse_def(&format!(
                r#"(net_id: 1, anim: "swing", phases: [("windup", 0.2), ("active", 0.1), ("recovery", 0.3)],
                    hits: [(span: Phase("active"), shape: {shape}, damage: 20)])"#
            ));
            let mut run = Running::new("swing", &def, 0, Vec3::NEG_Z, None, Some("light".into()));
            run.t = 0.1;
            run.prev = 0.1 - DT;
            run.refresh(&def, 0.0);
            assert!(run.zone.is_some(), "{shape}");
            let allocated = allocations_in(|| run.refresh(&def, 0.0));
            assert_eq!(allocated, 0, "{shape}: an unchanged tick allocated");
        }
        // The counter does see a string being built, so the zeros above mean something.
        let shape = HitShape::Cone { range: 3.0, angle: 90.0, height: 2.0 };
        assert!(allocations_in(|| drop(shape.to_string())) > 0);
        assert!(displays_as(&shape, "cone 3m 90°"));
        assert!(!displays_as(&shape, "cone 3m 90"));
        assert!(!displays_as(&shape, "cone 3m 90°!"));
        assert!(!displays_as(&shape, "cone 4m 90°"));
    }

    #[test]
    fn idle_fighters_start_at_once() {
        let db = db();
        let state = ActionState::default();
        let s = Stamina::new(100.0, 20.0, 0.5);
        assert_eq!(
            can_start(&view(&db, &state, Some(&s), &[]), "light", DT),
            Ok(Start { action: "light_1".into(), cancel: false })
        );
        let err = can_start(&view(&db, &state, Some(&s), &[]), "kick", DT).unwrap_err();
        assert_eq!(
            err.message(),
            "your moveset `hero` has no `kick`; it has: block, dodge, heavy, light, use_item.flask"
        );
    }

    #[test]
    fn combos_go_through_cancel_windows_only() {
        let db = db();
        let early = running(&db, "light_1", 0.1);
        let err = can_start(&view(&db, &early, None, &[]), "light", DT).unwrap_err();
        let Blocker::Busy { ready_in: Some(t), by_cancel: true, .. } = &err else { panic!("{err:?}") };
        assert!((t - 0.2).abs() < 1e-4, "{t}");
        assert_eq!(err.message(), "busy: windup light_1 0.10s (ready in 0.20s, when a cancel window opens)");
        assert_eq!(err.reason(), "busy (windup light_1 0.10s)");
        let open = running(&db, "light_1", 0.35);
        let start = can_start(&view(&db, &open, None, &[]), "light", DT).unwrap();
        assert_eq!(start, Start { action: "light_2".into(), cancel: true });
        let heavy = can_start(&view(&db, &open, None, &[]), "heavy", DT).unwrap_err();
        let Blocker::Busy { ready_in: Some(t), by_cancel: false, .. } = heavy else { panic!() };
        assert!((t - 0.25).abs() < 1e-4, "waits for the end: {t}");
    }

    #[test]
    fn stamina_reports_when_regeneration_covers_it() {
        let db = db();
        let state = ActionState::default();
        let mut s = Stamina::new(100.0, 60.0, 0.5);
        s.spend(95.0);
        let err = can_start(&view(&db, &state, Some(&s), &[]), "heavy", DT).unwrap_err();
        // A 0.5 s delay (30 ticks), then 1 stamina per tick: the 25th regeneration ends
        // tick 55, and the press starts in the tick after, 55 ticks from now.
        let Blocker::Stamina { ready_in: Some(t), .. } = err else { panic!("{err:?}") };
        assert!((t - 55.0 * DT).abs() < 1e-4, "{t}");
        assert_eq!(err.message(), format!("not enough stamina: 5/30 (ready in {:.1}s)", 55.0 * DT));
        // Simulate it: regeneration at the end of each tick, the try at the start.
        let mut ticks = 0;
        while s.current + EPS < 30.0 {
            crate::fighter::regen_stamina(&mut s, DT);
            ticks += 1;
        }
        assert_eq!(ticks, 55, "the next tick's try succeeds");
    }

    #[test]
    fn charges_cooldowns_and_the_longest_wait() {
        let db = db();
        let state = ActionState::default();
        let none = Charges::new([("flask", 3)]);
        let mut empty = none.clone();
        empty.get_mut("flask").unwrap().current = 0;
        let mut v = view(&db, &state, None, &[]);
        v.charges = Some(&none);
        assert!(can_start(&v, "use_item.flask", DT).is_ok());
        v.charges = Some(&empty);
        assert_eq!(can_start(&v, "use_item.flask", DT).unwrap_err().message(), "no flask charges left (0/3)");
        let mut cd = Cooldowns::default();
        cd.start("heavy", 1.0);
        let busy = running(&db, "light_1", 0.5);
        let mut v = view(&db, &busy, None, &[]);
        v.cooldowns = Some(&cd);
        let err = can_start(&v, "heavy", DT).unwrap_err();
        assert!(matches!(err, Blocker::Cooldown { .. }), "the cooldown outlasts the busy action: {err:?}");
    }

    #[test]
    fn a_held_block_loops_until_released() {
        let db = db();
        let def = db.get::<ActionDef>("block").unwrap();
        let mut run = Running::new("block", def, 0, Vec3::NEG_Z, None, Some("block".into()));
        for _ in 0..60 {
            advance(&mut run, def, DT, true);
            run.refresh(def, 0.0);
        }
        assert!(run.t > 0.1 - EPS && run.t < 0.3 + EPS, "still in the hold phase: {}", run.t);
        let state = ActionState { current: Some(run.clone()), started: 1 };
        let held = ["block".to_owned()];
        let err = can_start(&view(&db, &state, None, &held), "light", DT).unwrap_err();
        assert_eq!(err.message(), "busy: hold block while `block` is held; release it first");
        let mut ticks = 0;
        while run.t < run.length - EPS {
            advance(&mut run, def, DT, false);
            ticks += 1;
        }
        assert!(ticks <= 18, "released, it runs out: {ticks}");
    }

    #[test]
    fn a_held_loop_never_drops_out_whatever_the_tick_rate() {
        // Sums of tick lengths land a hair before the end of the hold span; the wrap
        // must not read that as "a whole span over".
        for (hz, raise, hold, lower) in
            [(60, 0.1, 0.6, 0.1), (30, 0.1, 0.3, 0.1), (64, 0.1, 0.3, 0.1), (120, 0.1, 0.3, 0.1), (50, 0.1, 0.2, 0.1)]
        {
            let def: ActionDef = ron::Options::default()
                .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
                .from_str(&format!(
                    r#"(net_id: 1, phases: [("raise", {raise}), ("hold", {hold}), ("lower", {lower})],
                        hold: (input: "block", span: Phase("hold")), guard: (span: Phases("raise", "hold")))"#
                ))
                .unwrap();
            let (a, b) = def.span(&crate::def::Span::Phase("hold".into())).unwrap();
            let dt = std::time::Duration::from_secs_f64(1.0 / f64::from(hz)).as_secs_f32();
            let mut run = Running::new("block", &def, 0, Vec3::NEG_Z, None, Some("block".into()));
            let mut inside = false;
            for tick in 1..=hz * 8 {
                let spans = advance(&mut run, &def, dt, true);
                run.refresh(&def, 0.0);
                inside |= run.t >= a - EPS;
                assert!(run.t < b - EPS, "{hz} Hz tick {tick}: t {} ran past the hold {a}..{b}", run.t);
                assert!(!inside || run.t >= a - EPS, "{hz} Hz tick {tick}: t {} fell out of the hold {a}..{b}", run.t);
                assert!(spans.len() <= 2 && spans.iter().all(|s| s.1 - s.0 <= dt + EPS), "{hz} Hz tick {tick}: {spans:?}");
            }
        }
    }

    #[test]
    fn buffer_fit_matches_tick_counts() {
        assert_eq!(ticks_until(0.0, DT), 0);
        assert_eq!(ticks_until(DT, DT), 1);
        assert_eq!(ticks_until(DT * 1.001, DT), 1);
        assert_eq!(ticks_until(DT * 1.1, DT), 2);
        assert!(fits_buffer(0.15, 0.15) && !fits_buffer(0.16, 0.15));
    }

    #[test]
    fn a_backstep_goes_back_unless_something_names_a_direction() {
        let step = |text: &str, dir: Option<Vec3>, held: Vec3| {
            let def = parse_def(text);
            let intent = Intent { move_dir: held, ..default() };
            steer(&def, dir, &intent, None, Vec3::ZERO, Vec3::X, None)
        };
        let back = r#"(net_id: 1, phases: [("a", 0.4)], steer: MoveOrBack)"#;
        let plain = r#"(net_id: 1, phases: [("a", 0.4)], steer: Move)"#;
        assert_eq!(step(back, None, Vec3::ZERO), -Vec3::X, "nothing pressed: away from the facing");
        assert_eq!(step(plain, None, Vec3::ZERO), Vec3::X, "the plain move keeps the facing");
        assert_eq!(step(back, Some(Vec3::Z), Vec3::ZERO), Vec3::Z, "a press names the way");
        assert_eq!(step(back, None, -Vec3::Z), -Vec3::Z, "so does the stick");
        let def = parse_def(back);
        assert!(track(&def, &Intent { aim: Some(Vec3::Z), ..default() }, None, Vec3::ZERO, Some(Vec3::Z)).is_none(), "no tracking");
    }

    #[test]
    fn an_aiming_action_of_a_body_that_faces_its_look_goes_where_it_looks() {
        let aim = parse_def(r#"(net_id: 1, phases: [("a", 0.4)], steer: Aim)"#);
        let back = parse_def(r#"(net_id: 1, phases: [("a", 0.4)], steer: MoveOrBack)"#);
        let west = Some(Vec3::NEG_X);
        // Walking north, aiming at a point to the east, locked on a body to the south, looking west.
        let intent = Intent { move_dir: Vec3::NEG_Z, aim: Some(Vec3::new(5.0, 0.0, 0.0)), ..default() };
        let there = Some(Vec3::new(0.0, 0.0, 5.0));
        assert_eq!(steer(&aim, None, &intent, west, Vec3::ZERO, Vec3::X, there), Vec3::NEG_X, "the look comes before the lock-on, the aim and the stick");
        assert_eq!(steer(&aim, Some(Vec3::Z), &intent, west, Vec3::ZERO, Vec3::X, there), Vec3::Z, "a press that names a direction still wins");
        assert_eq!(steer(&aim, None, &intent, None, Vec3::ZERO, Vec3::X, there), Vec3::Z, "no look: the lock-on, as before");
        assert_eq!(steer(&aim, None, &Intent::default(), west, Vec3::ZERO, Vec3::X, None), Vec3::NEG_X);
        // A backstep of a body that looks west goes back from the look, not from where it faced a tick ago.
        assert_eq!(steer(&back, None, &Intent::default(), west, Vec3::ZERO, Vec3::X, None), Vec3::X);
        // What the run turns toward while it tracks.
        assert_eq!(track(&aim, &intent, west, Vec3::ZERO, there), Some(Vec3::NEG_X));
        assert_eq!(track(&aim, &intent, None, Vec3::ZERO, there), Some(Vec3::Z));
        assert_eq!(track(&back, &intent, west, Vec3::ZERO, there), None, "an action that does not aim does not track");
        // With turn spans a run starts from the body's facing, which for such a body is the look.
        let turning = parse_def(r#"(net_id: 1, phases: [("a", 0.4)], steer: Aim, turn: [(Phase("a"), 90)])"#);
        assert_eq!(start_facing(&turning, None, &intent, west, Vec3::ZERO, Vec3::X, there), Vec3::NEG_X);
        assert_eq!(start_facing(&turning, None, &intent, None, Vec3::ZERO, Vec3::X, there), Vec3::X);
    }

    #[test]
    fn the_turn_and_hold_options_parse_and_stay_out_of_json_when_off() {
        let def = parse_def(r#"(net_id: 1, phases: [("a", 0.4)], turn_speed_deg: 458, regen_hold: 1.3)"#);
        assert_eq!((def.turn_speed_deg, def.regen_hold), (Some(458.0), Some(1.3)));
        let plain = parse_def(r#"(net_id: 1, phases: [("a", 0.4)])"#);
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("turn_speed_deg").is_none() && json.get("regen_hold").is_none(), "not serialized when off");
        assert!(serde_json::to_value(&def).unwrap().get("regen_hold").is_some());
    }

    #[test]
    fn progress_for_the_animation_bridge() {
        let db = db();
        let mut state = running(&db, "light_1", 0.3);
        assert_eq!(action_progress(&state), None, "no clip");
        state.current.as_mut().unwrap().anim = Some("swing".into());
        let (clip, t) = action_progress(&state).unwrap();
        assert_eq!(clip, "swing");
        assert!((t - 0.5).abs() < 1e-4);
        assert_eq!(describe_action(&state), "recovery light_1 0.30s");
    }
}
