//! # shards_audio — sound and music (E9.5)
//!
//! What a game sounds like is content: `content/**/*.sound.ron` names sounds (files, bus, volume,
//! priority, how they fade with distance), `*.music.ron` tracks in layers, and one `*.audio.ron`
//! the mixer (how many voices, the start track, the sounds of the engine's events). The engine
//! decides what plays, from where and how loud, and tells agents; with a window and an audio
//! device (feature `playback`), Bevy's audio plays it.
//!
//! - **Buses** ([`Bus`]): music, effects (`Sfx`), menus (`Ui`) and speech (`Voice`), each at the
//!   player's volume (`audio.<bus>` times `audio.master`, the settings group `audio`). A volume
//!   changed in the settings menu applies to what is playing.
//! - **Sounds** ([`SoundDef`]): an attack's telegraph plays the sound it names where the attack
//!   will land; an action's `Sound` effect plays one from its actor at its time (a swing's
//!   whoosh); a clip's cue plays a sound of that name from the body (from where the cue's bone is
//!   when it names one, at a volume scaled by the cue's `strength`: [`play_scaled`]), but only where a
//!   model is drawn and plays the clip; the mixer gives the engine's events (`damaged`, `blocked`, `died`,
//!   `ui_pressed`, …) the sounds it names; a game plays its own with [`play`] or the [`PlaySound`]
//!   event.
//! - **Voices** ([`Mixer`]): at most the mixer's `voices` play at once. One more takes the voice of
//!   the weakest (lowest priority, then quietest, then oldest) when it is stronger, else it is not
//!   heard; a sound with `max_instances` takes its own oldest voice. A spatial sound is heard from
//!   where it happens, fading from `near` to `far` from the listener (the body this machine plays,
//!   facing with the camera) and panned; past `far` it is not started at all. A game cuts a voice short
//!   with [`stop`] (the id [`play`] gave it): it fades to silence over the seconds it asks and its line goes
//!   at once (an interrupted line, a radio call cut off).
//! - **Placed sounds** ([`SoundEmitter`], `looped: true` in a [`SoundDef`]): a looped sound plays only from
//!   an emitter, a component a level node, a prefab or code puts at a `Transform`. The mixer holds one
//!   voice for it while the listener is within `far` (under the normal priority rules: a one-shot of a
//!   lower priority never takes a loop's voice), lets it go beyond and takes one again on the way back.
//! - **Walls** ([`OcclusionDef`], [`SoundTransmission`]): off unless the mixer asks (`occlusion`). A ray from
//!   the listener to each spatial voice through the physics world's `WORLD` colliders takes a share of the
//!   voice's gain for every collider on the way (its `SoundTransmission`, else the mixer's `gain`).
//! - **Music** ([`Music`], [`MusicDef`]): a track's stems loop together, and its layers (calm to
//!   intense: `explore`, `tension`, `combat`) each turn some of them up. The most intense layer
//!   whose condition holds is played (conditions by name, as `step --until named:…` takes them:
//!   the NPC module's `npc_near` and `npc_engaged`, or the game's), at once when it is more
//!   intense, after the track's `hold` when it is calmer; stems fade in and out.
//! - **Subtitles** ([`subtitles`]): a sound with a `subtitle` (a voice's words, text or `@id`, and
//!   a `speaker`) shows its line while it plays and a moment after; the game's UI binds the source
//!   `subtitles` (the newest lines, in the language played), and `accessibility.subtitles` turns
//!   them off. The journal says `[local] subtitle: Warden: The gate holds.`
//! - **Agents** read it all with `audio`, and `observe` says the layer (`MUSIC  arena: combat for
//!   6.2s`); the journal says `[local] music: arena explore → combat`. `sound_play` plays a
//!   sound by the mixer's rules and `music_set` changes the track or holds a layer (dev).
//!   `ai/status` has `audio: {track, layer, voices, limit}`.
//! - **This machine's, not the simulation's**: the mixer decides on the simulation's clock (a
//!   `step` runs it, a pause stops it, menu sounds keep real time), the same headless and in a
//!   window, from events and conditions that only read the world (a clip's cue is the one
//!   exception: it needs the model that plays the clip, so give the sounds the game must hear on
//!   every machine to the action's `Sound` effect), so a co-op client hears its own copy of the
//!   fight (a host forwards the effect's event as it does a hit's). Nothing of it is in the replay
//!   hash, its journal lines are `local` (a host does not send them), and the music's state is in
//!   snapshots.
//! - **Checks**: `--check-content` finds files that are missing or are no WAV or Ogg Vorbis,
//!   stems of different lengths, layers no stem plays, conditions missing, and attacks that warn
//!   with a sound, or actions that play one, that no `*.sound.ron` defines.

pub mod def;
pub mod emitter;
pub mod length;
pub mod mixer;
pub mod music;
pub mod occlusion;
pub mod subtitles;
#[cfg(feature = "playback")]
mod playback;
mod tools;

use ai_core::AiAppExt;
use bevy::prelude::*;
use serde_json::{json, Value};
use shards_action::{
    ActionSound, AnimationCue, Blocked, Damaged, Died, Evaded, GuardBroken, Healed, Parried, SoundCue, Staggered,
};
use shards_content::{ContentAppExt, ContentDb};
use shards_core::EngineAppExt;
use shards_ui::{Layer, ScreenDef, UiAppExt, UiChanged, UiFocusMoved, UiPressed, UiScreenChanged};

pub use def::{AudioDef, Bus, Falloff, LayerDef, MusicDef, OcclusionDef, SoundDef, StemDef, EVENTS};
pub use emitter::{check_sound, emitter_play, emitter_stop, SoundEmitter};
pub use mixer::{
    bus_volume, listener, play, play_scaled, refresh, stop, voice_limit, AudioListener, Dropped, Fading, Listener, Mixer,
    PlaySound, Played, SoundAt, Voice,
};
pub use music::{force_layer, set_track, Music, Stem};
pub use occlusion::{Blocker, Occluded, SoundTransmission};
pub use subtitles::{LineVoice, SubtitleLine, Subtitles};
pub use tools::{AudioArgs, EmitterArgs, MusicSetArgs, SoundPlayArgs};

/// Version of the engine crates (they share one).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The types a game module uses most.
pub mod prelude {
    pub use crate::{AudioListener, Bus, Music, PlaySound, SoundAt, SoundEmitter};
}

/// The audio systems other modules order against.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioSystems {
    /// `FixedPostUpdate` (after every tick: the music director, the voices on the simulation's
    /// clock) and `Update` (menu voices on real time): what plays and how loud.
    Mix,
    /// `Update`, after `Mix`, with feature `playback` and an audio device: Bevy's audio plays it.
    Play,
}

/// The audio module: sounds, music and the mixer as content, the mixer and the music director,
/// the tools, the `MUSIC` line of `observe`; playback with feature `playback`. Needs the engine
/// core, content, actions and the UI first; `shards::EnginePlugins` adds it after them.
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        assert!(
            app.is_plugin_added::<ai_core::AiCorePlugin>(),
            "AudioPlugin needs AiCorePlugin added first (shards::EnginePlugins adds both)"
        );
        app.add_content::<SoundDef>()
            .add_content::<MusicDef>()
            .add_content::<AudioDef>()
            .add_content_check::<shards_content::PrefabDef>("sound emitters", emitter::check_prefab)
            .init_resource::<Mixer>()
            .init_resource::<Music>()
            .init_resource::<music::UnknownConditions>()
            .init_resource::<Subtitles>()
            .add_ui_source("subtitles", subtitles::source)
            .register_type::<Music>()
            .register_type::<SoundEmitter>()
            .register_type::<SoundTransmission>()
            .add_snapshot_resource::<Music>()
            // Placed sound is this machine's: not in the replay hash, wherever an emitter or a muffling collider
            // stands (a level's have no `AiId`; a prefab body's would).
            .replay_ignore::<SoundEmitter>()
            .replay_ignore::<SoundTransmission>()
            .add_engine_module("audio", VERSION)
            .add_ai_status("audio", status)
            .register_ai_observe_section(observe_section)
            .add_systems(FixedPostUpdate, (mixer::tick_clock, music::direct, refresh).chain().in_set(AudioSystems::Mix))
            .add_systems(Update, (mixer::real_clock, refresh).chain().in_set(AudioSystems::Mix))
            .configure_sets(Update, AudioSystems::Play.after(AudioSystems::Mix))
            .add_observer(on_play_sound)
            .add_observer(on_sound_cue)
            .add_observer(on_action_sound)
            .add_observer(on_animation_cue)
            .add_observer(|e: On<Damaged>, c: Commands| event_sound(c, "damaged", e.target))
            .add_observer(|e: On<Blocked>, c: Commands| event_sound(c, "blocked", e.defender))
            .add_observer(|e: On<Parried>, c: Commands| event_sound(c, "parried", e.defender))
            .add_observer(|e: On<Evaded>, c: Commands| event_sound(c, "evaded", e.defender))
            .add_observer(|e: On<GuardBroken>, c: Commands| event_sound(c, "guard_broken", e.defender))
            .add_observer(|e: On<Staggered>, c: Commands| event_sound(c, "staggered", e.entity))
            .add_observer(|e: On<Died>, c: Commands| event_sound(c, "died", e.entity))
            .add_observer(|e: On<Healed>, c: Commands| event_sound(c, "healed", e.entity))
            .add_observer(|_: On<UiFocusMoved>, c: Commands| menu_sound(c, "ui_moved"))
            .add_observer(|_: On<UiPressed>, c: Commands| menu_sound(c, "ui_pressed"))
            .add_observer(|_: On<UiChanged>, c: Commands| menu_sound(c, "ui_changed"))
            .add_observer(on_screen_changed);
        tools::register(app.world_mut());
        #[cfg(feature = "playback")]
        playback::plugin(app);
    }
}

/// `ai/status` → `audio: {track, layer, voices, limit}`.
fn status(world: &World) -> Value {
    let music = world.get_resource::<Music>();
    let voices = world.get_resource::<Mixer>().map_or(0, |m| m.voices().len());
    json!({
        "track": music.and_then(|m| m.track.clone()),
        "layer": music.and_then(|m| m.layer.clone()),
        "voices": voices,
        "limit": voice_limit(world),
    })
}

/// `MUSIC  arena: combat for 6.2s`, while a track plays. The voices are left to `audio`: they are
/// this machine's moment, which a snapshot does not bring back.
fn observe_section(world: &World) -> Vec<String> {
    if !world.get_resource::<Music>().is_some_and(|m| m.track.is_some()) {
        return Vec::new();
    }
    vec![format!("MUSIC  {}", music::summary(world))]
}

fn on_play_sound(event: On<PlaySound>, mut commands: Commands) {
    let PlaySound { sound, at } = event.event().clone();
    commands.queue(move |world: &mut World| mixer::play_quietly(world, &sound, at, "PlaySound"));
}

/// An attack's warning: its sound, where the attack lands.
fn on_sound_cue(cue: On<SoundCue>, mut commands: Commands) {
    let SoundCue { entity, cue, position } = cue.event().clone();
    commands.queue(move |world: &mut World| {
        let why = format!("{} warns", ai_core::label(world, entity));
        mixer::play_quietly(world, &cue, SoundAt::Point { position, source: Some(entity) }, &why);
    });
}

/// An action's `Sound` effect: that sound from the body doing it. The effect runs on the
/// simulation's clock (a client gets the host's event), so this is the same headless, in a window
/// and on a client.
fn on_action_sound(sound: On<ActionSound>, mut commands: Commands) {
    let ActionSound { entity, action, sound } = sound.event().clone();
    commands.queue(move |world: &mut World| {
        mixer::play_quietly(world, &sound, SoundAt::Entity(entity), &format!("action `{action}` plays it"));
    });
}

/// A clip's cue: the sound of that name, if there is one (cues are for effects too). From the body,
/// or from where the cue's bone is when it names one (a point: it stays where the foot fell, it does
/// not follow the body), at the cue's strength as its gain.
fn on_animation_cue(cue: On<AnimationCue>, mut commands: Commands) {
    let AnimationCue { entity, cue, at, strength, .. } = cue.event().clone();
    commands.queue(move |world: &mut World| {
        if world.resource::<ContentDb>().get::<SoundDef>(&cue).is_some() {
            let place = at.map_or(SoundAt::Entity(entity), |position| SoundAt::Point { position, source: Some(entity) });
            mixer::play_quietly_scaled(world, &cue, place, strength, "a clip's cue");
        }
    });
}

/// The sound the mixer gives engine event `event`, from `entity`.
fn event_sound(mut commands: Commands, event: &'static str, entity: Entity) {
    commands.queue(move |world: &mut World| {
        if let Some(sound) = mixer::audio_def(world).and_then(|d| d.sounds.get(event).cloned()) {
            mixer::play_quietly(world, &sound, SoundAt::Entity(entity), &format!("the mixer's `{event}`"));
        }
    });
}

fn menu_sound(mut commands: Commands, event: &'static str) {
    commands.queue(move |world: &mut World| {
        if let Some(sound) = mixer::audio_def(world).and_then(|d| d.sounds.get(event).cloned()) {
            mixer::play_quietly(world, &sound, SoundAt::Everywhere, &format!("the mixer's `{event}`"));
        }
    });
}

/// A menu opened or closed (a HUD makes no sound).
fn on_screen_changed(changed: On<UiScreenChanged>, mut commands: Commands) {
    let UiScreenChanged { screen, open } = changed.event().clone();
    commands.queue(move |world: &mut World| {
        let hud = world.resource::<ContentDb>().peek::<ScreenDef>(&screen).is_some_and(|s| s.layer == Layer::Hud);
        if !hud {
            let event = if open { "ui_opened" } else { "ui_closed" };
            if let Some(sound) = mixer::audio_def(world).and_then(|d| d.sounds.get(event).cloned()) {
                mixer::play_quietly(world, &sound, SoundAt::Everywhere, &format!("the mixer's `{event}`"));
            }
        }
    });
}
