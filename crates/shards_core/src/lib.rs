//! # shards_core — what every Shards module and game shares
//!
//! - [`EngineConfig`]: window or headless, seed, port, tick rate, assets; one value
//!   builds the whole engine (`shards::EnginePlugins::new(config)`).
//! - [`Intent`]: the only input of a character, written by keyboard, agents, network
//!   and replays, read by gameplay. One-shot [`Press`]es are consumed during a tick;
//!   [`Order`]s are carried out over many ([`follow_orders`] walks).
//! - [`EngineState`] / [`PauseState`]: loading, menu, game, pause. A game with a main menu
//!   ([`menu`]) shows it only in runs a person may be at ([`MenuLaunch`]); time stands there.
//! - [`LoadingGate`]: the simulation starts only once everything is loaded, so loading
//!   never shifts a tick.
//! - [`EngineModules`]: which engine modules run, shown in `ai/status` under `engine`.
//!
//! Gameplay runs in `FixedUpdate`, in `SimSet` order: the engine's own systems are in
//! [`CoreSystems`] so a game can order its systems against them.

pub mod exit;
pub mod intent;
pub mod loading;
pub mod menu;
pub mod seed;
pub mod state;

use ai_core::{AiAppExt, AiCorePlugin, SimConfig, SimSet};
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::state::state::StateTransitionSystems;
use serde::Serialize;
use serde_json::{json, Value};

pub use exit::{run_exit_hooks, ExitHook, ExitHooks};
pub use intent::{flat, follow_orders, input_names, Gaze, Intent, Order, Press, Target};
pub use loading::{statuses as loading_statuses, LoadCheck, LoadStatus, LoadingGate};
pub use menu::{can_start_game, start_game};
pub use seed::{adopt_seed, hold_seed, seed_ready, SeedLock};
pub use state::{
    player_body, predicts, simulates, EngineState, LocalPlayer, NetPrediction, NetRole, NetSet, PauseState, PlayerId, Predicted,
    PredictedMotion,
};

/// Version of the engine crates (they share one).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The types a game module uses most.
pub mod prelude {
    pub use crate::{
        input_names, CoreSystems, EngineAppExt, EngineConfig, EngineState, Intent, LoadStatus, LocalPlayer, MenuLaunch, NetRole, NetSet,
        Order, PauseState, PlayerId, Predicted, Press, Target,
    };
}

/// Everything that decides how the engine is built and run. `shards::run` fills it
/// from the command line; tests use `shards::testing::config()`.
#[derive(Resource, Clone, Debug)]
pub struct EngineConfig {
    /// Window title.
    pub title: String,
    /// The game's version (`Some(env!("CARGO_PKG_VERSION").into())` in the game's crate), named in
    /// crash reports.
    pub version: Option<String>,
    /// No window, no GPU, no audio: CI, servers, parallel playtests. Gameplay is the
    /// same plugins, so a game plays the same with a window or without.
    pub headless: bool,
    /// No window, but the GPU draws: the cameras render into an image of [`window`](Self::window)
    /// size, which `shot` captures (`--offscreen`). For CI and agents on a machine with a GPU (or a
    /// software one). Not headless: the game adds its looks ([`draws`](Self::draws)).
    pub offscreen: bool,
    /// World seed: same seed + same inputs = same game.
    pub seed: u64,
    /// BRP port for `shards` (`remote` feature). `None`: no HTTP server.
    pub port: Option<u16>,
    /// Start with the simulation paused; advance it with `shards step`.
    pub paused: bool,
    /// Simulation ticks per second.
    pub tick_hz: f64,
    /// A gameplay failure saves `replays/crash-tick<N>.json` to reproduce it with and a crash
    /// report (`crashes/…`; a panic nobody handled too), a violated invariant
    /// `replays/invariant-<name>-tick<N>.json`. `false` switches off what the game writes by itself,
    /// not the `crash_report` tool: a bug report an agent asks for is written whatever this says.
    pub crash_replays: bool,
    /// Ticks between replay checks (`--check-every`), stored in every replay saved.
    pub check_every: u64,
    /// What each replay check keeps to name what diverged (`--check-detail`).
    pub check_detail: ai_core::CheckDetail,
    /// A tick that takes longer is over budget (`--tick-budget-ms`); `None`: the tick's
    /// own duration.
    pub tick_budget: Option<std::time::Duration>,
    /// A frame that takes longer is over budget (`--frame-budget-ms`); `None`: 1/60 s. Only a game
    /// that draws (a window, `--offscreen`) has frames.
    pub frame_budget: Option<std::time::Duration>,
    /// Tick timing in `step` replies and `perf` journal lines; off in tests, whose
    /// journals must not depend on the machine.
    pub report_perf: bool,
    /// Window size in logical pixels.
    pub window: (u32, u32),
    /// A window draws on a thread of its own, side by side with the game's update (Bevy's pipelined rendering): a
    /// frame costs the slower of the two instead of their sum, and the picture is one frame later behind the mouse.
    /// `false` (the default) draws each frame right after its update, on one thread: the least latency, at a frame
    /// rate of update plus render. A game whose render thread is its limit sets it for the frame rate; at 144 frames
    /// a second the frame it adds is 7 ms. `--offscreen` always draws side by side.
    pub pipelined_rendering: bool,
    /// Where `EngineState::Loading` goes once everything is loaded: `InGame`, or `MainMenu` for a
    /// game with a main menu. Whether a run shows that menu is [`menu`](Self::menu)'s to say
    /// ([`start_state`](Self::start_state)).
    pub after_loading: EngineState,
    /// Whether a game whose [`after_loading`](Self::after_loading) is `MainMenu` shows it in this run
    /// (`--menu`, `--no-menu`): by default only when a person may be there to use it.
    pub menu: MenuLaunch,
    /// A picture under the asset folder (`ui/loading.png`) shown full screen while the game loads
    /// (`EngineState::Loading`) and while a region loads across frames (`RegionLoading`), when
    /// something is drawn. Content is not loaded yet when it is first needed, so the game names it
    /// here, not in content. `None`: nothing is shown.
    pub loading_image: Option<String>,
    /// Apply content files as they change on disk. Needs the `hot_reload` cargo feature
    /// (Bevy's file watcher); on by default, off in tests and `--verify`.
    pub hot_reload: bool,
    /// The asset folder: relative to the game's folder (`CARGO_MANIFEST_DIR` under
    /// cargo, else the executable's folder; `BEVY_ASSET_ROOT` overrides both), or
    /// absolute. Content lives in `<asset_dir>/content/`.
    pub asset_dir: String,
    /// A test app: headless, no logger (a process has only one), no Ctrl+C handler and
    /// no run loop; tests drive it with `app.update()` and tools.
    pub testing: bool,
    /// A game beside another in one process (a stand's clients): like [`testing`](Self::testing)
    /// (headless, no logger, no Ctrl+C handler, no run loop: a driver updates it), but with
    /// its own BRP port, and its log lines go to its own journal.
    pub embedded: bool,
    /// Taking part in a networked game (`--host`, `--join`, `--stand`).
    pub net: NetLaunch,
    /// The name other players see (`--name`).
    pub player_name: String,
    /// Network conditions this game's links simulate (`--net-sim latency=150,loss=0.02`).
    pub net_sim: Option<String>,
    /// Where saves are written, relative to the game's folder (`--save-dir`).
    pub save_dir: String,
    /// Where player settings are kept (`--settings-dir`, `--no-settings`).
    pub settings: SettingsStore,
    /// The character to play (`--character SLOT`): a save slot.
    pub character_slot: Option<String>,
    /// The world to load (`--world SLOT`): a save slot.
    pub world_slot: Option<String>,
    /// A client exits when its session ends, instead of going back to the menu
    /// (`--exit-with-host`): with 0 when the host ended it, 1 when it was refused, kicked, lost
    /// the host or could not take the host's world. Exit 2 also applies when the session cannot
    /// be opened: an address that does not resolve, or the port of a host that is taken. A
    /// host does not exit when it ends the session.
    pub exit_with_host: bool,
    /// Steam (the `steam` feature of `shards`): whether to talk to the Steam client, the game's app
    /// id, a session hosted in a Steam lobby or as a Steam game server. Without the feature nothing
    /// reads it.
    pub steam: SteamLaunch,
    /// Engine modules this game leaves out, by the names `ai/status` lists under `engine.modules`
    /// (`audio`, `scatter`; `--without audio,scatter`): their plugins are not added. Which ones can
    /// go, and what each needs, is `shards::modules`.
    pub without: Vec<String>,
    /// Heavy mode (`--heavy N`, `--heavy-span K`, `--heavy-offset O`): every N ticks a fresh game is
    /// loaded from a snapshot of this one and must continue like it ([`ai_core::heavy`]). `None` (the
    /// default): off. It needs the game's builder, which `shards::run` and
    /// `shards::testing::heavy_app` give it.
    pub heavy: Option<ai_core::HeavyConfig>,
    /// Check the engine's own invariant rules after every tick (`ai.index_consistent`, `core.intent_sane`,
    /// `motion.finite`, `action.resources_in_range`, `action.running_sane`, `npc.state_known`,
    /// `npc.tokens_within_limit`): a broken one halts the game like a failed gameplay system.
    /// On by default; off in a build with the `dist` feature (a shipped game must not pause on a
    /// rule), and by `--no-engine-invariants`. Test apps, `--verify`, fuzz and soak keep it on.
    /// Read when the plugins are built, so set it before.
    pub engine_invariants: bool,
    /// The engine's rules that are off by default, turned on by name (`--engine-invariant NAME`,
    /// once a rule), only while [`engine_invariants`](Self::engine_invariants) is on: none today.
    /// `voxel.body_in_solid` (shards_motion), the first, is on by default since its fuzz runs on
    /// voxel terrain, like the other engine rules (off only with `--no-engine-invariants`). Empty by
    /// default.
    pub opt_in_invariants: Vec<String>,
    /// State coverage (`--coverage`): the `coverage` tool lists the states the session reached and what
    /// content declares that it never did ([`ai_core::coverage`]). Off by default; it only reads the
    /// world, so a game plays the same either way.
    pub coverage: bool,
}

/// What a game asks of Steam at launch (the `steam` feature of `shards`: the `shards_steam` module).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SteamLaunch {
    /// Talk to the Steam client (a build with the feature `steam_sdk`): a game a player plays does
    /// (`shards::run` sets it). Off, as in tests, `--check-content`, `--smoke`, `--verify`, `--perf` and
    /// the in-process stand, the module runs on a fake Steam of its own and never reaches the real one.
    pub connect: bool,
    /// The game's Steam app id (`--steam-app 480`). `None`: the one Steam started the game with, or
    /// the `SteamAppId` environment variable (`steam_appid.txt` is never needed, and never shipped).
    pub app_id: Option<u32>,
    /// Host the session in a Steam lobby (`--steam-host`; [`net`](EngineConfig::net) is then
    /// [`NetLaunch::Host`]): clients join it through the Steam relay (`--join steam:<lobby>`, an
    /// invite), and no UDP port is opened.
    pub host: bool,
    /// Host as an anonymous Steam game server (`--steam-server`; [`net`](EngineConfig::net) is
    /// [`NetLaunch::Host`]): no lobby and no Steam user; clients join it by its Steam id
    /// (`--join steam:<id>`) through the relay.
    pub server: bool,
    /// Only the Steam relay carries the session (`--steam-relay-only`): no direct route between the
    /// machines (ICE off), so a test proves the relay works.
    pub relay_only: bool,
    /// Who sees the lobby a `--steam-host` game makes (`--steam-lobby`; friends only by default).
    pub lobby: SteamLobby,
}

/// Who sees a Steam lobby and how players come in (`--steam-lobby friends|private|invisible|public`),
/// as Steam's lobby types say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SteamLobby {
    /// Friends see it and join it; whoever is in invites their own friends.
    #[default]
    Friends,
    /// Nobody sees it: only an invite brings a player in.
    Private,
    /// Not shown to friends (a lobby search finds it).
    Invisible,
    /// Friends see it, and so does Steam's lobby list.
    Public,
}

impl SteamLobby {
    /// Every type, as `--steam-lobby` names them.
    pub const ALL: [SteamLobby; 4] = [SteamLobby::Friends, SteamLobby::Private, SteamLobby::Invisible, SteamLobby::Public];

    /// `friends`, `private`, `invisible`, `public`.
    pub fn as_str(self) -> &'static str {
        match self {
            SteamLobby::Friends => "friends",
            SteamLobby::Private => "private",
            SteamLobby::Invisible => "invisible",
            SteamLobby::Public => "public",
        }
    }

    /// Whether a lobby search lists a lobby of this type: Steam's search finds `public` and
    /// `invisible` lobbies, and never a `friends` or a `private` one.
    pub fn in_search(self) -> bool {
        matches!(self, SteamLobby::Invisible | SteamLobby::Public)
    }

    /// What the type means, for `steam`: `friends only`, `private: invite only`.
    pub fn describe(self) -> &'static str {
        match self {
            SteamLobby::Friends => "friends only",
            SteamLobby::Private => "private: invite only",
            SteamLobby::Invisible => "invisible: friends do not see it",
            SteamLobby::Public => "public: in Steam's lobby list too",
        }
    }
}

impl std::str::FromStr for SteamLobby {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|t| t.as_str() == text).ok_or_else(|| {
            let names: Vec<&str> = Self::ALL.iter().map(|t| t.as_str()).collect();
            format!("`{text}` is no lobby type: {}", names.join(", "))
        })
    }
}

impl std::fmt::Display for SteamLobby {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The UDP port of a hosted game unless `--host PORT` says otherwise.
pub const DEFAULT_NET_PORT: u16 = 7777;

/// How a game takes part in a networked session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NetLaunch {
    /// Single player: no network.
    #[default]
    Off,
    /// Hosts a session on this UDP port (`--host [PORT]`).
    Host { port: u16 },
    /// Joins the host at `addr` (`--join 127.0.0.1:7777`).
    Join { addr: String },
    /// A host and `clients` clients from one command (`--stand N`): in this process, or
    /// child processes over UDP loopback (`udp`, `--stand-udp`).
    Stand { clients: u8, udp: bool },
}

/// Whether a run shows the game's main menu (a game whose [`EngineConfig::after_loading`] is
/// `MainMenu`), or goes straight into the game as one without a menu does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuLaunch {
    /// Only when a person may be there to use it: something is drawn (a window, `--offscreen`),
    /// and the launch says nothing of what to play (no session: `--host`, `--join`, `--stand`; no
    /// save slot: `--world`, `--character`), and the game runs the `ui` module that draws the menu
    /// (not left out with [`EngineConfig::without`]). Headless runs, tests, replays and checks go
    /// straight in.
    #[default]
    Auto,
    /// Always (`--menu`): an agent goes through it headless, with `ui_*` tools.
    Show,
    /// Never (`--no-menu`): straight into the game.
    Skip,
}

/// Where player settings (graphics, audio, controls) are kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SettingsStore {
    /// Defaults, no files (`--no-settings`, tests, a stand's clients).
    Off,
    /// The platform's settings folder for this game.
    #[default]
    Default,
    /// This folder (`--settings-dir DIR`).
    Dir(String),
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            title: "Shards".into(),
            version: None,
            headless: false,
            offscreen: false,
            seed: 42,
            port: Some(ai_core::DEFAULT_PORT),
            paused: false,
            tick_hz: 60.0,
            crash_replays: true,
            check_every: ai_core::replay::CHECK_EVERY,
            check_detail: ai_core::CheckDetail::default(),
            tick_budget: None,
            frame_budget: None,
            report_perf: true,
            window: (1280, 720),
            pipelined_rendering: false,
            after_loading: EngineState::InGame,
            menu: MenuLaunch::Auto,
            loading_image: None,
            hot_reload: true,
            asset_dir: "assets".into(),
            testing: false,
            embedded: false,
            net: NetLaunch::Off,
            player_name: "player".into(),
            net_sim: None,
            save_dir: "saves".into(),
            settings: SettingsStore::Default,
            character_slot: None,
            world_slot: None,
            exit_with_host: false,
            steam: SteamLaunch::default(),
            without: Vec::new(),
            heavy: None,
            engine_invariants: !cfg!(feature = "dist"),
            opt_in_invariants: Vec::new(),
            coverage: false,
        }
    }
}

impl EngineConfig {
    /// Something is drawn: a window, or an offscreen image. A game adds its looks (meshes, lights,
    /// cameras) only then.
    pub fn draws(&self) -> bool {
        self.offscreen || !(self.headless || self.testing || self.embedded)
    }

    /// Where `Loading` goes in this run: [`after_loading`](Self::after_loading), except that a
    /// main menu is passed over (`InGame`) when [`menu`](Self::menu) says this run does not show it,
    /// so a game that goes straight in builds exactly the world it builds without a menu.
    pub fn start_state(&self) -> EngineState {
        if self.after_loading != EngineState::MainMenu {
            return self.after_loading;
        }
        let show = match self.menu {
            MenuLaunch::Show => true,
            MenuLaunch::Skip => false,
            MenuLaunch::Auto => {
                self.draws()
                    && self.net == NetLaunch::Off
                    && self.world_slot.is_none()
                    && self.character_slot.is_none()
                    && !self.without.iter().any(|module| module.trim() == "ui")
            }
        };
        if show { EngineState::MainMenu } else { EngineState::InGame }
    }

    /// The AI layer as this config wants it.
    pub fn ai_core_plugin(&self) -> AiCorePlugin {
        AiCorePlugin {
            sim: SimConfig { tick_hz: self.tick_hz, seed: self.seed, start_paused: self.paused },
            remote_port: if self.testing { None } else { self.port },
            crash_replays: self.crash_replays,
            check_every: self.check_every,
            check_detail: self.check_detail,
            tick_budget: self.tick_budget,
            report_perf: self.report_perf,
            game: ai_core::crash::GameInfo { title: self.title.clone(), version: self.version.clone() },
            heavy: self.heavy,
            coverage: self.coverage,
            // Test apps write no files.
            heavy_files: !self.testing,
            engine_invariants: self.engine_invariants,
            ..default()
        }
    }
}

/// The engine's own gameplay systems, for ordering a game's systems against them.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoreSystems {
    /// `SimSet::Input`: [`follow_orders`] steers `Order::MoveTo`.
    FollowOrders,
    /// `SimSet::Cleanup`: unconsumed presses are dropped ([`intent::clear_presses`]).
    ClearPresses,
}

/// An engine module as `ai/status` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EngineModule {
    /// `core`, `content`, `motion`, ...
    pub name: String,
    /// Crate version.
    pub version: String,
}

/// The engine modules this game runs, in the order they were added.
#[derive(Resource, Default, Clone, Debug)]
pub struct EngineModules(Vec<EngineModule>);

impl EngineModules {
    /// Adds a module; adding one with the same name again updates its version.
    pub fn add(&mut self, name: &str, version: &str) {
        match self.0.iter_mut().find(|m| m.name == name) {
            Some(module) => module.version = version.to_owned(),
            None => self.0.push(EngineModule { name: name.to_owned(), version: version.to_owned() }),
        }
    }

    /// Modules in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = &EngineModule> {
        self.0.iter()
    }

    /// Whether module `name` runs.
    pub fn contains(&self, name: &str) -> bool {
        self.0.iter().any(|m| m.name == name)
    }
}

/// Registration API for engine modules and games.
pub trait EngineAppExt {
    /// Lists an engine module in `ai/status` (`engine.modules`).
    fn add_engine_module(&mut self, name: &str, version: &str) -> &mut Self;

    /// Holds the simulation until `check` reports [`LoadStatus::Ready`] (see
    /// [`LoadingGate`]).
    fn require_loaded(&mut self, label: &str, check: LoadCheck) -> &mut Self;

    /// Runs `hook` when the game ends: once, after the hooks added before it, when an
    /// `AppExit` is written or a termination signal arrives (see [`exit`]).
    fn add_exit_hook(&mut self, name: &str, hook: ExitHook) -> &mut Self;

    /// Registers one of the engine's own invariant rules (`<module>.<rule>`, checked after every
    /// tick like a game rule, listed as `engine rule`) unless [`EngineConfig::engine_invariants`] is
    /// off. An app without an `EngineConfig` (a module's test rig) gets the rule. For engine
    /// modules; a game adds its own with `add_ai_invariant`. Panics on a bad name.
    fn add_engine_invariant(&mut self, name: &str, rule: ai_core::invariants::Invariant) -> &mut Self;

    /// Registers one of the engine's rules that are off by default, as
    /// [`add_engine_invariant`](Self::add_engine_invariant) does, only when
    /// [`EngineConfig::opt_in_invariants`] names it and the engine's rules are on. An app without
    /// an `EngineConfig` does not get it. For a rule that may still fire on legitimate states (it
    /// waits for fuzz runs); its doc comment says what turns it on by default.
    fn add_opt_in_engine_invariant(&mut self, name: &str, rule: ai_core::invariants::Invariant) -> &mut Self;

    /// Whether the engine's invariant rules are on for this app ([`EngineConfig::engine_invariants`]; on
    /// when there is no `EngineConfig`). For a module whose rule needs a system of its own.
    fn engine_invariants_on(&self) -> bool;
}

impl EngineAppExt for App {
    fn add_engine_module(&mut self, name: &str, version: &str) -> &mut Self {
        self.world_mut().get_resource_or_init::<EngineModules>().add(name, version);
        self
    }

    fn require_loaded(&mut self, label: &str, check: LoadCheck) -> &mut Self {
        self.world_mut().get_resource_or_init::<LoadingGate>().require(label, check);
        self
    }

    fn add_exit_hook(&mut self, name: &str, hook: ExitHook) -> &mut Self {
        self.world_mut().get_resource_or_init::<ExitHooks>().add(name, hook);
        self
    }

    fn engine_invariants_on(&self) -> bool {
        self.world().get_resource::<EngineConfig>().is_none_or(|c| c.engine_invariants)
    }

    fn add_engine_invariant(&mut self, name: &str, rule: ai_core::invariants::Invariant) -> &mut Self {
        if self.engine_invariants_on()
            && let Err(e) = self.world_mut().get_resource_or_init::<ai_core::AiInvariants>().add_engine_rule(name, rule)
        {
            panic!("add_engine_invariant: {e}");
        }
        self
    }

    fn add_opt_in_engine_invariant(&mut self, name: &str, rule: ai_core::invariants::Invariant) -> &mut Self {
        let wanted = self
            .world()
            .get_resource::<EngineConfig>()
            .is_some_and(|c| c.engine_invariants && c.opt_in_invariants.iter().any(|n| n == name));
        if wanted {
            self.add_engine_invariant(name, rule);
        }
        self
    }
}

/// States, the loading gate, `Intent` (a replay input) and its orders, engine modules
/// and the `engine` field of `ai/status`. Needs `AiCorePlugin` first; `EnginePlugins`
/// adds both.
pub struct CorePlugin {
    /// Inserted as a resource; `after_loading` decides where loading goes.
    pub config: EngineConfig,
}

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        assert!(
            app.is_plugin_added::<AiCorePlugin>(),
            "CorePlugin needs AiCorePlugin added first (shards::EnginePlugins adds both)"
        );
        if !app.is_plugin_added::<StatesPlugin>() {
            app.add_plugins(StatesPlugin);
        }
        // Explicitly, for apps built without `reflect_auto_register`: replay checks and
        // `inspect` read components through the type registry.
        app.register_type::<Intent>()
            .register_type::<Gaze>()
            .register_type::<PlayerId>()
            .register_type::<LocalPlayer>()
            .register_type::<Predicted>()
            .register_type::<NetRole>();
        // `ai/session {player: N}`, `possess player=N`: the body with `PlayerId(N)`.
        app.world_mut().get_resource_or_init::<ai_core::AgentSessions>().set_player_resolver(state::player_body);
        exit::register(app.world_mut());
        menu::register(app.world_mut());
        app.insert_resource(self.config.clone())
            .init_resource::<NetRole>()
            .init_resource::<LoadingGate>()
            .init_resource::<state::MenuPaused>()
            .init_resource::<state::RegionTrip>()
            .init_resource::<menu::MainMenuClock>()
            .init_resource::<ExitHooks>()
            .init_resource::<SeedLock>()
            .init_state::<EngineState>()
            .add_sub_state::<PauseState>()
            .replay_input::<Intent>()
            // "My body" is each machine's own.
            .net_ignore::<LocalPlayer>()
            .net_ignore::<Predicted>()
            .replay_ignore::<Predicted>()
            // A joining client learns the seed from its host.
            .require_loaded("seed", seed::seed_status)
            .add_engine_module("core", VERSION)
            .add_engine_invariant("core.intent_sane", intent::intent_sane)
            .add_ai_status("engine", engine_status)
            // A client is fed the world by its host: it runs no gameplay, only its prediction.
            .configure_sets(
                FixedUpdate,
                (SimSet::Input, SimSet::Think, SimSet::Act, SimSet::Move, SimSet::Resolve, SimSet::Cleanup)
                    .distributive_run_if(simulates),
            )
            .init_resource::<NetPrediction>()
            .configure_sets(
                FixedUpdate,
                (NetSet::PredictInput, NetSet::Predict, NetSet::PredictEnd)
                    .chain()
                    .before(SimSet::Input)
                    .distributive_run_if(predicts),
            )
            // Invariants check the simulation where it runs: a client's world is the host's.
            .configure_sets(FixedLast, ai_core::InvariantSystems.run_if(simulates))
            .configure_sets(
                FixedUpdate,
                (CoreSystems::FollowOrders.in_set(SimSet::Input), CoreSystems::ClearPresses.in_set(SimSet::Cleanup)),
            )
            .add_systems(
                FixedUpdate,
                (
                    follow_orders::<()>.in_set(CoreSystems::FollowOrders),
                    intent::clear_presses.in_set(CoreSystems::ClearPresses),
                ),
            )
            .add_systems(First, loading::update_loading.before(ai_core::sim::apply_gate))
            .add_systems(PreUpdate, state::close_pause_when_resumed)
            .add_systems(First, exit::quit_when_due)
            .add_systems(Last, run_exit_hooks.run_if(exit::exiting))
            .add_systems(
                StateTransition,
                (
                    last_transition::<EngineState>
                        .pipe(state::hide_game_schedules)
                        .in_set(StateTransitionSystems::ExitSchedules)
                        .before(ExitSchedules::<EngineState>::default()),
                    state::restore_game_schedules
                        .in_set(StateTransitionSystems::EnterSchedules)
                        .after(EnterSchedules::<EngineState>::default()),
                    last_transition::<EngineState>
                        .pipe(state::open_transition_scope)
                        .in_set(StateTransitionSystems::ExitSchedules)
                        .before(ExitSchedules::<EngineState>::default()),
                    state::close_transition_scope
                        .in_set(StateTransitionSystems::EnterSchedules)
                        .after(EnterSchedules::<EngineState>::default()),
                ),
            )
            .add_systems(OnEnter(EngineState::MainMenu), menu::enter_menu)
            .add_systems(OnExit(EngineState::MainMenu), menu::leave_menu)
            .add_systems(RunFixedMainLoop, menu::restart_when_due.in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop))
            .add_systems(OnEnter(PauseState::Paused), state::pause_simulation)
            .add_systems(OnExit(PauseState::Paused), state::resume_simulation)
            .add_systems(Update, (state::journal_transitions::<EngineState>, state::journal_transitions::<PauseState>));
    }

    fn finish(&self, app: &mut App) {
        // After the game's describers: where an entity looks, then its orders, which end its line.
        app.register_ai_describer("look", intent::describe_look);
        app.register_ai_describer("orders", intent::describe_orders);
    }
}

/// `ai/status` → `engine: {version, modules, state, pause}`.
fn engine_status(world: &World) -> Value {
    let modules: Vec<&EngineModule> = world.get_resource::<EngineModules>().map(|m| m.iter().collect()).unwrap_or_default();
    let state = world.get_resource::<State<EngineState>>().map(|s| format!("{:?}", s.get()));
    let pause = world.get_resource::<State<PauseState>>().map(|s| format!("{:?}", s.get()));
    json!({"version": VERSION, "modules": modules, "state": state, "pause": pause})
}
