//! `EnginePlugins`: the whole engine from one [`EngineConfig`].

use std::path::Path;
use std::time::Duration;

use bevy::app::{PluginGroupBuilder, ScheduleRunnerPlugin, TerminalCtrlCHandlerPlugin};
use bevy::log::LogPlugin;
use bevy::prelude::*;
use bevy::window::ExitCondition;
use shards_action::ActionPlugin;
use shards_audio::AudioPlugin as ShardsAudioPlugin;
use shards_content::ContentPlugin;
use shards_core::{CorePlugin, EngineConfig};
use shards_dev::DevPlugin;
use shards_encounter::EncounterPlugin;
use shards_fx::FxPlugin;
use shards_hitstop::HitstopPlugin;
use shards_level::LevelPlugin;
use shards_light::LightPlugin;
use shards_look::LookPlugin;
use shards_motion::MotionPlugin;
use shards_net::NetPlugin;
use shards_npc::NpcPlugin;
use shards_rules::{RulesPlugin, RulesSystems};
use shards_scatter::ScatterPlugin;
use shards_save::SavePlugin;
use shards_locale::LocalePlugin;
use shards_settings::ShardsSettingsPlugin;
use shards_ui::UiPlugin as ShardsUiPlugin;
use shards_voxel::VoxelPlugin;
use shards_world::{WorldPlugin, WorldSystems};

use crate::audio_wiring::EngineAudioPlugin;
use crate::coop::CoopPlugin;
use crate::encounter_wiring::EngineEncounterPlugin;
use crate::net_wiring::EngineNetPlugin;
use crate::save_sections::EngineSavePlugin;
use crate::ui_wiring::EngineUiPlugin;
use crate::TileLevelsPlugin;

/// Bevy's `DefaultPlugins` (whatever the cargo features compile in), the log capture
/// that feeds `warn!`/`error!` into the journal, the AI layer, the engine core, content
/// and motion (physics, characters, input, camera), all configured from one [`EngineConfig`].
///
/// One plugin set for every mode: headless is the same plugins minus window, GPU and
/// audio (`WindowPlugin` without a window, no winit, `RenderPlugin` without a backend so
/// render plugins stay registered, a 60 Hz run loop instead of the event loop).
/// Transforms, assets and states work the same, so gameplay is the same.
/// [`EngineConfig::offscreen`] keeps the GPU and drops only the window: the cameras draw into
/// an image that `shot` captures.
/// [`EngineConfig::testing`] also drops the logger (one per process) and the Ctrl+C
/// handler, and runs no loop: tests drive the app with `update()`.
/// [`EngineConfig::embedded`] does the same for a game beside another in one process, but
/// keeps its BRP port, and its log lines go to its own journal.
///
/// A plugin group: a game can still `.set(...)` or `.disable::<...>()` single plugins.
pub struct EnginePlugins {
    config: EngineConfig,
}

impl EnginePlugins {
    /// The engine as `config` describes it.
    pub fn new(config: EngineConfig) -> Self {
        Self { config }
    }
}

impl PluginGroup for EnginePlugins {
    fn build(self) -> PluginGroupBuilder {
        let mut config = self.config;
        // Resolved once, before anything reads it: the asset plugin, the `EngineConfig`
        // resource that `asset_root` (content loading, `--check-content`) reads, all see it.
        if let Ok(cwd) = std::env::current_dir() {
            config.asset_dir = resolve_asset_path(&config.asset_dir, std::env::var("BEVY_ASSET_ROOT").ok().as_deref(), &cwd);
        }
        // What the game leaves out; `shards::run` said what is wrong with it already.
        let without = crate::modules::check_without(&config.without).unwrap_or_else(|e| panic!("EngineConfig::without: {e}"));
        // Driven from outside (a test, a stand's driver), beside other apps in the process.
        let driven = config.testing || config.embedded;
        let headless = config.headless || driven;
        let assets = AssetPlugin {
            file_path: config.asset_dir.clone(),
            watch_for_changes_override: Some(config.hot_reload && !driven && cfg!(feature = "hot_reload")),
            ..default()
        };
        let mut group = DefaultPlugins.build().set(assets);
        group = if driven {
            group.disable::<LogPlugin>().disable::<TerminalCtrlCHandlerPlugin>()
        } else {
            let mut filter = bevy::log::DEFAULT_FILTER.to_owned();
            if headless && !config.offscreen && cfg!(feature = "render") {
                // Render plugins without a render app: Bevy logs one error about the missing
                // sub-app it extracts into. Expected headless, and noise in every journal.
                filter.push_str(",bevy_extract::extract_resource=off");
            }
            group.set(LogPlugin { filter, custom_layer: ai_core::capture_log_layer, ..default() })
        };
        group = if config.offscreen && cfg!(feature = "render") {
            offscreen(group, &config, driven)
        } else if headless {
            without_display(group, driven)
        } else {
            with_window(group, &config)
        };
        if config.embedded {
            group = group.add(EmbeddedLogCapture);
        }
        // A played game on the Steam client starts it before the renderer exists: Steam's overlay
        // hooks the graphics device as it is made (the module itself comes with the others below).
        #[cfg(feature = "steam")]
        if config.steam.connect {
            group = group.add_after::<bevy::app::TaskPoolPlugin>(shards_steam::SteamClientPlugin::new(&config));
        }
        // The engine's modules, in dependency order. A new module is one more line.
        let group = group
            .add(config.ai_core_plugin())
            .add(CorePlugin { config: config.clone() })
            .add(ContentPlugin)
            // Player settings: the groups the modules below apply (controls, camera, warnings).
            .add(ShardsSettingsPlugin)
            .add(MotionPlugin)
            // Rules: the rule set a world plays (by class, by session size), before the modules that read it.
            .add(RulesPlugin)
            .add(RulesOrder)
            .add(ActionPlugin)
            .add(LevelPlugin)
            .add(WorldPlugin)
            // Level files as the geometry of tiles: joins the two above.
            .add(TileLevelsPlugin)
            .add(NpcPlugin)
            // Waves of enemies at a place: anchors, runs, wipes and resets (needs the NPC and world modules).
            .add(EncounterPlugin)
            // Co-op: roles, join, inputs, replication; then what it needs of the modules above.
            .add(NetPlugin)
            .add(EngineNetPlugin)
            // Saves: the registry and tools, then what the engine itself saves.
            .add(SavePlugin)
            .add(EngineSavePlugin)
            // Where the two meet: characters travel with their players, the host keeps the world.
            .add(CoopPlugin)
            // Languages and their text, which screens are drawn in.
            .add(LocalePlugin)
            // Screens and menus, then the HUD's values and the controls in menus.
            .add(ShardsUiPlugin)
            .add(EngineUiPlugin)
            // Sound and music: what the fight and the menus sound like, heard from the body.
            .add(ShardsAudioPlugin)
            .add(EngineAudioPlugin)
            // Looks: how meshes are drawn (toon light, flat faces, outlines), from content.
            .add(LookPlugin)
            // Scatter: foliage and small props strewn over the tiles, drawn in batches, by distance.
            .add(ScatterPlugin)
            // Voxel terrain (core): the palette content kind and the registry of generation steps.
            .add(VoxelPlugin)
            // Effects: particles from content in pools, the telegraphs on the ground as decals.
            .add(FxPlugin)
            // Where encounters meet the net, effects, sound, messages and saves.
            .add(EngineEncounterPlugin)
            // Hit stop: a brief hold of the picture (and of the real-time clock when alone) when a blow lands.
            .add(HitstopPlugin)
            // Lights from content: types, flicker and the shadow budget (the colour grade is the camera's, in motion).
            .add(LightPlugin);
        // Steam (feature `steam`): lobbies, the relay as a transport, achievements, cloud saves.
        #[cfg(feature = "steam")]
        let group = group.add(shards_steam::SteamPlugin);
        let group = group
            // Performance: the `perf` content kind (`--perf`), `counts` and `frames` in `ai/status`.
            .add(crate::bench::PerfPlugin)
            // The developer menu and console, last: every module above has added its entries.
            .add(DevPlugin)
            // A build for players: none of the developer's keys.
            .add(PlayerKeys);
        leave_out(group, &without)
    }
}

/// `group` without the plugins of the modules in `without` (checked by
/// [`check_without`](crate::modules::check_without)), and without what joins one of them to
/// another (level files as tiles, co-op saves).
fn leave_out(mut group: PluginGroupBuilder, without: &[&str]) -> PluginGroupBuilder {
    fn off<P: Plugin>(group: PluginGroupBuilder) -> PluginGroupBuilder {
        if group.contains::<P>() { group.disable::<P>() } else { group }
    }
    for module in without {
        group = match *module {
            "level" => off::<TileLevelsPlugin>(off::<LevelPlugin>(group)),
            "world" => off::<TileLevelsPlugin>(off::<WorldPlugin>(group)),
            "npc" => off::<NpcPlugin>(group),
            "encounter" => off::<EngineEncounterPlugin>(off::<EncounterPlugin>(group)),
            "net" => off::<EngineNetPlugin>(off::<NetPlugin>(group)),
            "save" => off::<EngineSavePlugin>(off::<SavePlugin>(group)),
            "coop" => off::<CoopPlugin>(group),
            "locale" => off::<LocalePlugin>(group),
            "ui" => off::<EngineUiPlugin>(off::<ShardsUiPlugin>(group)),
            "audio" => off::<EngineAudioPlugin>(off::<ShardsAudioPlugin>(group)),
            "look" => off::<LookPlugin>(group),
            "scatter" => off::<ScatterPlugin>(group),
            "voxel" => off::<VoxelPlugin>(group),
            "fx" => off::<FxPlugin>(group),
            "hitstop" => off::<HitstopPlugin>(group),
            "light" => off::<LightPlugin>(group),
            #[cfg(feature = "steam")]
            "steam" => off::<shards_steam::SteamClientPlugin>(off::<shards_steam::SteamPlugin>(group)),
            "dev" => off::<DevPlugin>(group),
            _ => group,
        };
    }
    group
}

/// The rules first in a tick: `RulesSystems::Apply` before the world's zones, the other set of
/// `SimSet::Input` that runs before `CoreSystems::FollowOrders` (the umbrella knows both crates).
struct RulesOrder;

impl Plugin for RulesOrder {
    fn build(&self, app: &mut App) {
        app.configure_sets(FixedUpdate, RulesSystems::Apply.before(WorldSystems::Zones));
    }
}

/// A build for players (feature `dist`): the developer's keys do nothing, neither the console and
/// the menu (` and F10) nor the overlays (F2, F6, F7). Their tools stay, and such a build serves
/// them to nobody (it is built without `remote`). Without `dist` this does nothing.
struct PlayerKeys;

impl Plugin for PlayerKeys {
    fn build(&self, _: &mut App) {}

    #[cfg_attr(not(all(feature = "dist", feature = "render")), allow(unused_variables))]
    fn finish(&self, app: &mut App) {
        #[cfg(all(feature = "dist", feature = "render"))]
        {
            let world = app.world_mut();
            world.insert_resource(shards_dev::DevKeys { console: None, menu: None });
            if let Some(mut overlay) = world.get_resource_mut::<shards_action::render::CombatOverlay>() {
                overlay.key = None;
            }
            if let Some(mut overlay) = world.get_resource_mut::<shards_net::NetOverlay>() {
                overlay.key = None;
            }
            if let Some(mut overlay) = world.get_resource_mut::<shards_npc::NpcOverlay>() {
                overlay.key = None;
            }
        }
    }
}

/// The asset folder as the engine hands it to Bevy. Bevy joins it onto `BEVY_ASSET_ROOT`
/// (else the manifest dir, else the executable's folder) and starts a file watcher on the
/// result, which fails for a relative path (`PathNotFound`). So a relative `asset_dir`
/// under a relative root becomes absolute, against `cwd`; `Path::join` with an absolute
/// path keeps it as it is. Anything else is returned unchanged.
fn resolve_asset_path(asset_dir: &str, asset_root: Option<&str>, cwd: &Path) -> String {
    let dir = Path::new(asset_dir);
    match asset_root.map(Path::new) {
        Some(root) if dir.is_relative() && root.is_relative() => cwd.join(root).join(dir).display().to_string(),
        _ => asset_dir.to_owned(),
    }
}

/// An embedded game's log lines: the process's capture layer (installed by the first game's
/// `LogPlugin`) writes them into this game's journal while it is the active one.
struct EmbeddedLogCapture;

impl Plugin for EmbeddedLogCapture {
    fn build(&self, app: &mut App) {
        ai_core::journal::register_capture(app);
    }
}

fn with_window(group: PluginGroupBuilder, config: &EngineConfig) -> PluginGroupBuilder {
    let (width, height) = config.window;
    // The least latency a hand on the mouse can feel, at some of the frame rate: one frame queued on the GPU, not
    // Bevy's two, and no pipelined rendering (with it the main world works on the next frame while this one is drawn,
    // which is one frame more between a mouse move and the picture). With vsync and both, a look turned 3 to 4 frames
    // after the hand moved, which a player felt as smoothing on the mouse.
    let window = Window {
        title: config.title.clone(),
        resolution: (width, height).into(),
        desired_maximum_frame_latency: std::num::NonZeroU32::new(1),
        ..default()
    };
    #[cfg_attr(not(feature = "render"), allow(unused_mut))]
    let mut group = group.set(WindowPlugin { primary_window: Some(window), ..default() });
    #[cfg(feature = "render")]
    if group.contains::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>() {
        group = group.disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>();
    }
    // `<assets>/icon.png`, if the game has one, in the title bar and the taskbar; how long frames take.
    #[cfg(feature = "render")]
    let group = group
        .add(crate::window_icon::WindowIconPlugin)
        .add(crate::frames::FramesPlugin { budget: config.frame_budget })
        .add(crate::render_scale::RenderScalePlugin);
    group
}

/// What a GPU would have told the image loaders: no compressed formats (Bevy asks for it
/// when `RenderPlugin` runs without a backend).
#[cfg(feature = "render")]
struct NoGpu;

#[cfg(feature = "render")]
impl Plugin for NoGpu {
    fn build(&self, app: &mut App) {
        use bevy::image::{CompressedImageFormatSupport, CompressedImageFormats};
        app.insert_resource(CompressedImageFormatSupport(CompressedImageFormats::NONE));
    }
}

/// Headless: no window and no GPU.
fn without_display(group: PluginGroupBuilder, driven: bool) -> PluginGroupBuilder {
    #[cfg_attr(not(feature = "render"), allow(unused_mut))]
    let mut group = without_window(group, driven);
    #[cfg(feature = "render")]
    {
        use bevy::render::settings::WgpuSettings;
        use bevy::render::RenderPlugin;
        group = group
            // Drawing gizmos needs a GPU; gizmos themselves (`GizmoPlugin`) stay.
            .disable::<bevy::gizmos_render::GizmoRenderPlugin>()
            .set(RenderPlugin { render_creation: WgpuSettings { backends: None, ..default() }.into(), ..default() })
            .add_before::<RenderPlugin>(NoGpu);
    }
    group
}

/// Offscreen: no window, but the GPU draws, gizmos too, and the cameras draw into an image.
#[cfg_attr(not(feature = "render"), allow(unused_variables))]
fn offscreen(group: PluginGroupBuilder, config: &EngineConfig, driven: bool) -> PluginGroupBuilder {
    let group = without_window(group, driven);
    #[cfg(feature = "render")]
    let group = group
        .add(crate::offscreen::OffscreenPlugin { size: config.window })
        .add(crate::frames::FramesPlugin { budget: config.frame_budget })
        .add(crate::render_scale::RenderScalePlugin);
    group
}

/// No window, no audio device, no gamepads, and a run loop of its own (none when `driven`).
fn without_window(mut group: PluginGroupBuilder, driven: bool) -> PluginGroupBuilder {
    group = group.set(WindowPlugin {
        primary_window: None,
        exit_condition: ExitCondition::DontExit,
        close_when_requested: false,
        ..default()
    });
    #[cfg(feature = "render")]
    {
        group = group.disable::<bevy::winit::WinitPlugin>();
    }
    #[cfg(feature = "audio")]
    {
        group = group.disable::<bevy::audio::AudioPlugin>();
    }
    #[cfg(feature = "gamepad")]
    {
        group = group.disable::<bevy::gilrs::GilrsPlugin>();
    }
    let runner = if driven {
        ScheduleRunnerPlugin::run_once()
    } else {
        ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 60.0))
    };
    group.add(runner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_root_makes_the_asset_folder_absolute() {
        let cwd = std::env::current_dir().unwrap();
        let resolved = resolve_asset_path("assets", Some("crates/sandbox"), &cwd);
        assert_eq!(resolved, cwd.join("crates/sandbox").join("assets").display().to_string());
        // What Bevy does next: it joins its base onto the path, and the absolute one wins.
        assert_eq!(Path::new("crates/sandbox").join(&resolved), Path::new(&resolved));
    }

    #[test]
    fn an_absolute_root_or_no_root_keeps_the_asset_folder() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(resolve_asset_path("assets", cwd.to_str(), &cwd), "assets");
        assert_eq!(resolve_asset_path("assets", None, &cwd), "assets");
    }

    #[test]
    fn an_absolute_asset_folder_is_kept_whatever_the_root() {
        let cwd = std::env::current_dir().unwrap();
        let absolute = cwd.join("elsewhere").display().to_string();
        assert_eq!(resolve_asset_path(&absolute, Some("crates/sandbox"), &cwd), absolute);
        assert_eq!(resolve_asset_path(&absolute, None, &cwd), absolute);
    }

    /// A game played on Steam (`connect`) starts it right after the task pools, before the assets
    /// and the renderer after them, and the module takes that Steam. Without the SDK (never run
    /// with it: a test does not talk to Steam) there is none, and the game says why.
    #[cfg(all(feature = "steam", not(feature = "steam_sdk")))]
    #[test]
    fn a_game_played_on_steam_starts_it_before_the_renderer() {
        #[derive(Resource)]
        struct BuiltBefore {
            task_pools: bool,
            assets: bool,
        }
        struct Probe;
        impl Plugin for Probe {
            fn build(&self, app: &mut App) {
                let task_pools = app.is_plugin_added::<bevy::app::TaskPoolPlugin>();
                let assets = app.is_plugin_added::<AssetPlugin>();
                app.insert_resource(BuiltBefore { task_pools, assets });
            }
        }
        let config = EngineConfig { steam: shards_core::SteamLaunch { connect: true, ..default() }, ..crate::testing::config() };
        let mut app = App::new();
        app.add_plugins(EnginePlugins::new(config).build().add_after::<shards_steam::SteamClientPlugin>(Probe));
        let seen = app.world().resource::<BuiltBefore>();
        assert!(seen.task_pools && !seen.assets, "after the task pools, before the assets (and the renderer after them)");
        app.finish();
        app.cleanup();
        app.update();
        let steam = app.world().resource::<shards_steam::Steam>();
        assert_eq!((steam.kind(), steam.unavailable().as_deref()), ("none", Some(shards_steam::NO_SDK)));
    }
}
