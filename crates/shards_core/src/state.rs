//! Engine states and who plays.
//!
//! ```text
//! Loading ──(loading gate open)──► EngineConfig::start_state: InGame (default) or MainMenu
//! MainMenu ──(new_game, continue_game, the game's own)──► InGame
//! InGame ─► PauseState: Running ⇄ Paused        RegionLoading ──(gate open)──► InGame
//! ```
//!
//! The simulation never runs while the engine is `Loading` or `RegionLoading` (the
//! [loading gate](crate::loading) holds it). A trip `InGame → RegionLoading → InGame` (a
//! region change) keeps the game: `OnExit(InGame)` and `OnEnter(InGame)` do not run for
//! it, so the players a game spawned on entering `InGame` stay (the region module moves
//! them); `OnEnter`/`OnExit(RegionLoading)` mark the trip. `PauseState::Paused` is the player's pause
//! (a menu): in single player it pauses the simulation, and an agent's `resume` closes
//! it again; while a replay plays, the recording's ticks run on under a menu it opened, and
//! its end pauses if that menu is still up. Pausing the simulation itself (`shards pause`,
//! `--paused`) is a dev tool and leaves `PauseState` alone, so stepping a game never pops a
//! menu up.

use ai_core::journal::{AiJournal, JournalKind};
use ai_core::state::ComponentValues;
use ai_core::{sim, AiId, AiReplay, SimTick};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Where the whole application is. Games spawn their world in
/// `OnEnter(EngineState::InGame)`: content exists only after `Loading`.
#[derive(States, Reflect, Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum EngineState {
    /// Waiting for the loading gate (content, ...). No ticks run.
    #[default]
    Loading,
    /// The game's main menu (see [`menu`](crate::menu)): time stands; `new_game`, `continue_game`
    /// or the game itself goes on to `InGame`.
    MainMenu,
    /// Playing: games spawn their world on entering it.
    InGame,
    /// A region is being replaced (`shards_world`): no ticks run, and the engine returns
    /// to `InGame` once the loading gate is open again. Coming from `InGame`, the trip
    /// keeps the game: `OnExit(InGame)` and `OnEnter(InGame)` do not run for it.
    RegionLoading,
}

/// The player's pause, inside `InGame`.
#[derive(SubStates, Reflect, Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[source(EngineState = EngineState::InGame)]
pub enum PauseState {
    /// Playing.
    #[default]
    Running,
    /// In single player the simulation is paused too.
    Paused,
}

/// This machine's part in a session. The network model is host-authoritative with
/// tick-stamped inputs; networking itself comes later.
#[derive(Resource, Reflect, Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[reflect(Resource, Default)]
pub enum NetRole {
    /// One machine, no network.
    #[default]
    Single,
    /// Runs the simulation for everyone.
    Host,
    /// Sends inputs, shows the host's world.
    Client,
}

impl NetRole {
    /// Whether this machine runs the simulation: everyone but a client, which is fed the
    /// world by its host. Every `SimSet` runs only if it does ([`simulates`]).
    pub fn simulates(self) -> bool {
        self != NetRole::Client
    }

    /// Whether this machine predicts its own body between the host's states: a client.
    pub fn predicts(self) -> bool {
        self == NetRole::Client
    }
}

/// Run condition of every `SimSet`: this machine simulates ([`NetRole::simulates`]; no
/// `NetRole` counts as single player).
pub fn simulates(role: Option<Res<NetRole>>) -> bool {
    role.is_none_or(|r| r.simulates())
}

/// Run condition of [`NetSet::Predict`]: this machine is a client ([`NetRole::predicts`]).
pub fn predicts(role: Option<Res<NetRole>>) -> bool {
    role.is_some_and(|r| r.predicts())
}

/// `FixedUpdate` sets of the network layer, chained in this order before `SimSet::Input`
/// and run only where [`predicts`]. They are outside every `SimSet`: a client runs no
/// `SimSet`.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetSet {
    /// The network layer puts the inputs this machine sent for the tick into the own
    /// body's `Intent` (the presses the capture took), as the host will have them.
    PredictInput,
    /// A client moves its own body ahead of the host's state ([`Predicted`]): the same
    /// systems the simulation runs, filtered to the body, and the start of its actions.
    Predict,
    /// The network layer puts back what [`NetSet::PredictInput`] set aside and records
    /// what was predicted for the tick (to correct it when the host's state comes).
    PredictEnd,
}

/// What a client's prediction knows of its host: written by the network layer, read by
/// the modules that predict (the action machine's start of actions). A client only; a
/// host or single player never reads it.
#[derive(Resource, Clone, Debug, Default)]
pub struct NetPrediction {
    /// The host tick of the newest state applied.
    pub host_tick: u64,
    /// Ticks this machine runs ahead of the host.
    pub lead: u64,
    /// The host tick of the forwarded event being shown now (set while the network layer
    /// triggers it again).
    pub event_tick: u64,
    /// Actions started ahead of the host.
    pub predicted: u64,
    /// ... the host started too.
    pub confirmed: u64,
    /// ... the host refused, or did not start in time.
    pub mispredicted: u64,
    /// The own body takes the host's state at the next correction (a death): why.
    pub snap: Option<String>,
    /// Times this machine took the host's world (a join, a rebase): what was predicted in
    /// an older one is dropped.
    pub epoch: u32,
}

/// How a client's correction reads and moves its predicted body, from the module that moves
/// bodies (`shards_motion` inserts it): the network layer knows no motion types.
#[derive(Resource, Clone, Copy)]
pub struct PredictedMotion {
    /// The body's motion mode now, as a number to compare (never to interpret).
    pub mode: fn(&World, Entity) -> Option<u8>,
    /// The same number in the host's state of the body (its predicted components' values).
    pub host_mode: fn(&World, &ComponentValues) -> Option<u8>,
    /// Moves the body by `by` as if the controller had put it there (no fresh start).
    pub shift: fn(&mut World, Entity, Vec3),
    /// Takes the host's motion state and facing for the body (`values`, as `host_mode`
    /// reads them), its feet put at `feet`.
    pub adopt: fn(&mut World, Entity, &ComponentValues, Vec3),
}

/// The body a client predicts: its own. Never on a host or in single player; each machine
/// marks it itself, so it is neither replicated nor checked by replays.
#[derive(Component, Reflect, Clone, Copy, Debug, Default)]
#[reflect(Component, Default)]
pub struct Predicted;

/// A player-controlled character: player 0, 1, ... in a session.
#[derive(Component, Reflect, Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub struct PlayerId(pub u8);

/// The body of player `n` now: the entity with `PlayerId(n)` and an id (the newest, if a
/// corpse kept its `PlayerId`). Agent sessions bound to a player find their body with it.
pub fn player_body(world: &World, n: u8) -> Option<AiId> {
    let mut bodies = world.try_query::<(&PlayerId, &AiId)>()?;
    bodies.iter(world).filter(|(player, _)| player.0 == n).map(|(_, id)| *id).max()
}

/// The character this machine's keyboard and camera drive.
#[derive(Component, Reflect, Clone, Copy, Debug, Default)]
#[reflect(Component, Default)]
pub struct LocalPlayer;

/// What `PauseState::Paused` did to the simulation's clock.
#[derive(Resource, Default)]
pub(crate) struct MenuPaused {
    /// It paused the clock, so leaving it resumes.
    paused: bool,
    /// It came while a replay played, and the clock ran on under the menu: the recording holds
    /// which ticks ran while it was up, and the end of the replay pauses, if the menu is still up.
    under_replay: bool,
}

/// `OnEnter(PauseState::Paused)`: single player, a menu pauses the simulation.
///
/// Not while a replay plays: the recording decides which ticks run. The pause menu took effect after
/// that frame's time was counted (Bevy counts it in `First`, a menu opens in `PreUpdate`), so the
/// session may have run one more tick under it, and the call that closed it then stands one tick
/// after the call that opened it. In playback the call that opens it comes inside a tick, and a
/// paused clock would never reach the tick of the one that closes it.
pub(crate) fn pause_simulation(world: &mut World) {
    if *world.resource::<NetRole>() != NetRole::Single || sim::is_paused(world) {
        return;
    }
    if world.get_resource::<AiReplay>().is_some_and(AiReplay::is_playing) {
        world.resource_mut::<MenuPaused>().under_replay = true;
        return;
    }
    sim::pause(world);
    world.resource_mut::<MenuPaused>().paused = true;
}

pub(crate) fn resume_simulation(world: &mut World) {
    let menu = std::mem::take(&mut *world.resource_mut::<MenuPaused>());
    if menu.paused && sim::is_paused(world) && sim::gate_message(world).is_none() {
        sim::resume(world);
    }
}

/// `PreUpdate`: the simulation runs again (an agent's `resume`) while the pause menu is
/// up in single player: the menu closes. A menu a replay opened stays up while the replay plays on
/// under it, and pauses the game once the replay is over (it ended, or someone took over).
pub(crate) fn close_pause_when_resumed(
    state: Option<Res<State<PauseState>>>,
    role: Res<NetRole>,
    replay: Option<Res<AiReplay>>,
    mut time: ResMut<Time<Virtual>>,
    mut menu: ResMut<MenuPaused>,
    mut next: ResMut<NextState<PauseState>>,
) {
    if !(state.is_some_and(|s| *s.get() == PauseState::Paused) && *role == NetRole::Single && !time.is_paused()) {
        return;
    }
    if menu.under_replay {
        if !replay.is_some_and(|r| r.is_playing()) {
            *menu = MenuPaused { paused: true, under_replay: false };
            time.pause();
        }
        return;
    }
    menu.paused = false;
    next.set(PauseState::Running);
}

/// The simulation's scope while a change of `EngineState` runs: its `OnExit`/`OnEnter` systems
/// build and take down the simulation's world (`OnEnter(InGame)` spawns it), so what they read
/// counts as the simulation's ([`ai_core::SimScope`]), in a playback and with a window as in the
/// session that was recorded.
#[derive(Resource, Default)]
pub(crate) struct TransitionScope(Option<ai_core::ScopeOpen>);

/// `StateTransition`, before the exit schedules of `EngineState`: opens the scope when the state
/// changes now.
pub(crate) fn open_transition_scope(In(transition): In<Option<StateTransitionEvent<EngineState>>>, world: &mut World) {
    if transition.is_some() {
        let open = ai_core::scope::open(world);
        world.get_resource_or_init::<TransitionScope>().0 = open;
    }
}

/// `StateTransition`, after the enter schedules of `EngineState`: closes it.
pub(crate) fn close_transition_scope(world: &mut World) {
    if let Some(mut scope) = world.get_resource_mut::<TransitionScope>() {
        scope.0 = None;
    }
}

/// Whether a region trip keeps the game, and the schedules hidden while its transition runs.
#[derive(Resource, Default)]
pub(crate) struct RegionTrip {
    /// `RegionLoading` was entered from `InGame`: coming back continues that game.
    from_game: bool,
    /// Taken out of [`Schedules`] for the transition running now.
    hidden: Vec<Schedule>,
}

/// `StateTransition`, before the exit schedules of `EngineState`: a region trip out of or
/// back into `InGame` hides `OnExit(InGame)` / `OnEnter(InGame)` from this transition, so
/// the game's world (its players above all) is neither torn down nor spawned twice.
pub(crate) fn hide_game_schedules(In(transition): In<Option<StateTransitionEvent<EngineState>>>, world: &mut World) {
    let Some(StateTransitionEvent { exited: Some(from), entered: Some(to), .. }) = transition else {
        return;
    };
    let mut trip = world.get_resource_or_init::<RegionTrip>();
    let hidden = match (from, to) {
        (EngineState::InGame, EngineState::RegionLoading) => {
            trip.from_game = true;
            world.resource_mut::<Schedules>().remove(OnExit(EngineState::InGame))
        }
        (EngineState::RegionLoading, EngineState::InGame) if trip.from_game => {
            trip.from_game = false;
            world.resource_mut::<Schedules>().remove(OnEnter(EngineState::InGame))
        }
        (EngineState::RegionLoading, _) => {
            trip.from_game = false;
            None
        }
        _ => None,
    };
    if let Some(schedule) = hidden {
        world.resource_mut::<RegionTrip>().hidden.push(schedule);
    }
}

/// `StateTransition`, after the enter schedules of `EngineState`: puts back what
/// [`hide_game_schedules`] took out.
pub(crate) fn restore_game_schedules(world: &mut World) {
    let hidden = std::mem::take(&mut world.get_resource_or_init::<RegionTrip>().hidden);
    let mut schedules = world.resource_mut::<Schedules>();
    for schedule in hidden {
        schedules.insert(schedule);
    }
}

/// `state Loading -> InGame` in the journal.
pub(crate) fn journal_transitions<S: States>(
    mut transitions: MessageReader<StateTransitionEvent<S>>,
    tick: Res<SimTick>,
    mut journal: ResMut<AiJournal>,
) {
    for t in transitions.read() {
        if let (Some(from), Some(to)) = (&t.exited, &t.entered)
            && from != to
        {
            journal.push(tick.0, JournalKind::Sim, format!("state {from:?} -> {to:?}"));
        }
    }
}
