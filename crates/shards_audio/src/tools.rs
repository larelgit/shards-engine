//! Tools: `audio` (what plays and why), `sound_play`, `music_set`, `emitter_play` and `emitter_stop` (dev: try a
//! sound, hold a layer, start or stop a placed sound). Sound is this machine's, so all of them run where they
//! are called, a co-op client too.

use ai_core::tools::{register_tool, ToolKind};
use ai_core::{Call, Id, Scope, ToolResult};
use bevy::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use shards_content::ContentDb;

use crate::def::{Bus, MusicDef};
use crate::emitter::{self, emitter_play, emitter_stop};
use crate::mixer::{self, bus_volume, play, voice_limit, Mixer, Played, SoundAt};
use crate::music::{check_layer, force_layer, set_track, summary, Music};
use crate::subtitles::{self, Subtitles};

/// How many voices the `audio` tool lists before it counts the rest.
const VOICES_SHOWN: usize = 12;

pub(crate) fn register(world: &mut World) {
    register_tool(world, "audio", Scope::Player, ToolKind::Query, audio);
    register_tool(world, "sound_play", Scope::Dev, ToolKind::Control, sound_play);
    register_tool(world, "music_set", Scope::Dev, ToolKind::Control, music_set);
    register_tool(world, "emitter_play", Scope::Dev, ToolKind::Control, emitter_play_tool);
    register_tool(world, "emitter_stop", Scope::Dev, ToolKind::Control, emitter_stop_tool);
}

/// What this machine hears: the music (track, layer, stems), the buses' volumes, the voices
/// playing (sound, source, distance, how loud, priority, and what stands in the way of a muffled one), the
/// emitters (where each is, heard or out of earshot or stopped), the sounds dropped lately and the
/// subtitles showing. Change volumes with `settings_set group=audio key=<bus> value=0.5`.
#[derive(Deserialize, JsonSchema, Default)]
pub struct AudioArgs {}

fn audio(In(_): In<Call<AudioArgs>>, world: &World) -> ToolResult {
    let mixer = world.get_resource::<Mixer>().ok_or("this game has no audio module")?;
    let mut out = format!("music {}\n", summary(world));
    if let Some(music) = world.get_resource::<Music>() {
        for stem in &music.stems {
            let aim = if (stem.gain - stem.target).abs() > 1e-3 { format!(" → {:.2}", stem.target) } else { String::new() };
            out.push_str(&format!("  {} [{}] {:.2}{aim}\n", stem.file, stem.track, stem.gain));
        }
    }
    let buses: Vec<String> = Bus::ALL.iter().map(|b| format!("{} {:.2}", b.name(), bus_volume(world, *b))).collect();
    out.push_str(&format!("buses (with master): {}\n", buses.join(" · ")));
    let voices = mixer.voices();
    out.push_str(&format!("voices {}/{} (started {}, dropped {})\n", voices.len(), voice_limit(world), mixer.started, mixer.dropped));
    let mut loudest: Vec<_> = voices.iter().collect();
    loudest.sort_by(|a, b| b.gain.total_cmp(&a.gain).then(a.id.cmp(&b.id)));
    for v in loudest.iter().take(VOICES_SHOWN) {
        let from = match (v.source_id, v.emitter) {
            (Some(id), _) => format!(" {id}"),
            (None, Some(emitter)) => format!(" {}", crate::occlusion::name_of(world, emitter)),
            (None, None) => String::new(),
        };
        let at = v.distance.map(|d| format!(" {d:.1}m")).unwrap_or_default();
        let length = if v.looped { "loop".to_owned() } else { format!("{:.2}s", v.length) };
        let walls = if v.occlusion.by.is_empty() { String::new() } else { format!(" (occluded: {})", v.occlusion.describe()) };
        let stopping = if v.stopping.is_some() { " (stopping)" } else { "" };
        out.push_str(&format!("  #{} {} {}{from}{at} gain {:.2} prio {} ({length}){walls}{stopping}\n", v.id, v.sound, v.bus.name(), v.gain, v.priority));
    }
    if voices.len() > VOICES_SHOWN {
        out.push_str(&format!("  and {} quieter\n", voices.len() - VOICES_SHOWN));
    }
    for drop in mixer.drops() {
        out.push_str(&format!("  dropped {} at {:.1}s: {}\n", drop.sound, drop.at, drop.reason));
    }
    let seen = emitter::collect(world);
    if !seen.is_empty() {
        let assessed: Vec<_> = seen.iter().map(|s| emitter::assess(world, s, mixer.listener)).collect();
        let heard = seen.iter().filter(|s| voices.iter().any(|v| v.emitter == Some(s.entity))).count();
        out.push_str(&format!("emitters {} ({heard} heard)\n", seen.len()));
        for (s, a) in seen.iter().zip(&assessed) {
            out.push_str(&emitter::line(world, s, a));
            out.push('\n');
        }
    }
    if let Some(lines) = world.get_resource::<Subtitles>().map(Subtitles::lines).filter(|l| !l.is_empty()) {
        let state = if subtitles::enabled(world) { "shown" } else { "not shown: accessibility.subtitles is off" };
        let (sim, real) = mixer.clocks();
        let lines: Vec<String> =
            lines.iter().map(|l| format!("\"{}\" {:.1}s more", subtitles::line_text(world, l), l.left(sim, real))).collect();
        out.push_str(&format!("subtitles ({state}): {}\n", lines.join(" · ")));
    }
    match mixer.listener {
        Some(ear) => {
            let who = ear.body.and_then(|b| world.get::<ai_core::AiId>(b)).map(|id| format!("{id} ")).unwrap_or_default();
            let p = ear.position;
            out.push_str(&format!("listener {who}at ({:.1}, {:.1}, {:.1})\n", p.x, p.y, p.z));
        }
        None => out.push_str("listener: none (no body to hear with): sounds are heard whole\n"),
    }
    Ok(out)
}

/// Play a sound now, as the game would: by its priority, the voice limit and the distance from
/// the listener. Without `at` or `point`, it is heard the same everywhere.
#[derive(Deserialize, JsonSchema)]
pub struct SoundPlayArgs {
    /// The sound (`*.sound.ron` id): `warden_slam`.
    pub sound: String,
    /// From this entity, following it: `e6`.
    pub at: Option<Id>,
    /// From this point: `[4.0, 0.0, -2.0]`.
    pub point: Option<[f32; 3]>,
    /// Play it this many times at once (1 to 64): what the voice limit does with a crowd.
    pub times: Option<u32>,
}

/// How many times `sound_play` plays a sound at most.
const TIMES_MAX: u32 = 64;

fn sound_play(In(call): In<Call<SoundPlayArgs>>, world: &mut World) -> ToolResult {
    let SoundPlayArgs { sound, at, point, times } = call.args;
    let at = match (at, point) {
        (Some(_), Some(_)) => return Err("give `at` (an entity) or `point` (x, y, z), not both".into()),
        (Some(id), None) => SoundAt::Entity(id.entity(world)?),
        (None, Some([x, y, z])) => SoundAt::Point { position: Vec3::new(x, y, z), source: None },
        (None, None) => SoundAt::Everywhere,
    };
    let times = times.unwrap_or(1);
    if !(1..=TIMES_MAX).contains(&times) {
        return Err(format!("times: 1 to {TIMES_MAX}, not {times}"));
    }
    if times > 1 {
        let (mut playing, mut stole, mut dropped) = (0, 0, Vec::new());
        for _ in 0..times {
            match play(world, &sound, at)? {
                Played::Started { stole: Some(_), .. } => {
                    playing += 1;
                    stole += 1;
                }
                Played::Started { .. } => playing += 1,
                Played::Dropped(reason) => dropped.push(reason),
            }
        }
        let mut out = format!("{sound} ×{times}: {playing} playing");
        if stole > 0 {
            out.push_str(&format!(" ({stole} took another's voice)"));
        }
        if let Some(reason) = dropped.first() {
            out.push_str(&format!(", {} not heard: {reason}", dropped.len()));
        }
        return Ok(out);
    }
    Ok(match play(world, &sound, at)? {
        Played::Started { id, stole } => {
            let voice = world.resource::<Mixer>().voices().iter().find(|v| v.id == id).cloned();
            let heard = voice.map(|v| {
                let at = v.distance.map(|d| format!(" at {d:.1} m")).unwrap_or_default();
                let walls = if v.occlusion.by.is_empty() { String::new() } else { format!(" (occluded: {})", v.occlusion.describe()) };
                format!("{}{at}, gain {:.2}, {:.2}s{walls}", v.file, v.gain, v.length)
            });
            let stole = stole.map(|s| format!("; took the voice of {s}")).unwrap_or_default();
            format!("playing {sound} #{id}: {}{stole}", heard.unwrap_or_default())
        }
        Played::Dropped(reason) => format!("not heard: {reason}"),
    })
}

/// Change the music: start another track (`track=boss`, `track=none` for silence) or keep the
/// current one to a layer whatever the game does (`layer=combat`; `layer=auto` follows the game
/// again).
#[derive(Deserialize, JsonSchema, Default)]
pub struct MusicSetArgs {
    /// A `*.music.ron` id, or `none`.
    pub track: Option<String>,
    /// A layer of the track, or `auto`.
    pub layer: Option<String>,
}

fn music_set(In(call): In<Call<MusicSetArgs>>, world: &mut World) -> ToolResult {
    let MusicSetArgs { track, layer } = call.args;
    if track.is_none() && layer.is_none() {
        return Err(format!("give `track` (a track, or none) or `layer` (a layer, or auto); now: {}", summary(world)));
    }
    // The layer is checked against the track that will play before anything changes: a call
    // that fails leaves the music as it was.
    if let Some(layer) = layer.as_deref().filter(|l| *l != "auto") {
        let after = match track.as_deref() {
            Some("none") => return Err(format!("`layer={layer}` needs music, and `track=none` plays none")),
            Some(track) => Some(track.to_owned()),
            None => world.resource::<Music>().track.clone(),
        };
        // An unknown track, or none playing: `set_track` and `force_layer` below say so.
        if let Some(after) = after
            && let Some(def) = world.resource::<ContentDb>().get::<MusicDef>(&after)
        {
            check_layer(&after, def, layer)?;
        }
    }
    // Silence holds no layer: `track=none` lets go of it, and `layer=auto` has nothing left to do.
    let silenced = track.as_deref() == Some("none");
    let mut said = Vec::new();
    if let Some(track) = track {
        said.push(set_track(world, (track != "none").then_some(track.as_str()))?);
    }
    if let Some(layer) = layer
        && !(silenced && layer == "auto")
    {
        said.push(force_layer(world, (layer != "auto").then_some(layer.as_str()))?);
    }
    Ok(said.join("; "))
}

/// Start or stop a sound emitter: a looped sound placed in the world (`SoundEmitter`). Local, like all
/// sound; the emitter takes a voice while its sound is in earshot (`audio` lists them).
#[derive(Deserialize, JsonSchema)]
pub struct EmitterArgs {
    /// The emitter: its name (a level node's, `Vent.North`), its id (`e20`) if it has one, or its
    /// sound when only one emitter plays it.
    pub emitter: String,
}

fn emitter_play_tool(In(call): In<Call<EmitterArgs>>, world: &mut World) -> ToolResult {
    set_emitter(world, &call.args.emitter, emitter_play)
}

fn emitter_stop_tool(In(call): In<Call<EmitterArgs>>, world: &mut World) -> ToolResult {
    set_emitter(world, &call.args.emitter, emitter_stop)
}

fn set_emitter(world: &mut World, name: &str, set: fn(&mut World, Entity) -> Result<(), String>) -> ToolResult {
    let entity = emitter::find(world, name)?;
    set(world, entity)?;
    // At once, so the answer (and the `audio` after it) shows what the emitter does now.
    mixer::refresh(world);
    Ok(emitter::describe(world, entity).trim().to_owned())
}
