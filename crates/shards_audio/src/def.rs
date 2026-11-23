//! Sounds, music and the mixer as content: `*.sound.ron`, `*.music.ron` and `*.audio.ron`.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use shards_action::{ActionDef, Effect};
use shards_content::db::closest;
use shards_content::{ContentDef, Refs};

use crate::length::file_seconds;

/// Where a sound goes on its way out: each bus has the player's volume (`audio.<bus>`, times
/// `audio.master`).
#[derive(Serialize, Deserialize, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bus {
    /// Music tracks (`*.music.ron`); no sound plays on it.
    Music,
    /// Effects: attacks, hits, steps, the world.
    #[default]
    Sfx,
    /// Menus: focus, presses, screens. They play while the game is paused.
    Ui,
    /// Speech.
    Voice,
}

impl Bus {
    pub const ALL: [Bus; 4] = [Bus::Music, Bus::Sfx, Bus::Ui, Bus::Voice];

    /// `music`, `sfx`, `ui`, `voice`: its setting's key in the `audio` group.
    pub fn name(self) -> &'static str {
        match self {
            Bus::Music => "music",
            Bus::Sfx => "sfx",
            Bus::Ui => "ui",
            Bus::Voice => "voice",
        }
    }
}

/// How a sound fades with distance from the listener: whole within `near`, silent from `far`,
/// and in between `(1 - t)²` of the way (`t` from 0 at `near` to 1 at `far`).
#[derive(Serialize, Deserialize, Reflect, Clone, Copy, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Falloff {
    pub near: f32,
    pub far: f32,
}

impl Falloff {
    /// The share of its volume a sound keeps `distance` away.
    pub fn gain(&self, distance: f32) -> f32 {
        if distance <= self.near {
            return 1.0;
        }
        if distance >= self.far {
            return 0.0;
        }
        let t = (distance - self.near) / (self.far - self.near);
        (1.0 - t) * (1.0 - t)
    }
}

/// A sound (kind `sound`; its id is what a telegraph, a clip's cue or the game names).
///
/// ```ron
/// {
///     "warden_slam": (
///         files: ["sounds/slam_1.wav", "sounds/slam_2.wav"],
///         priority: 200,
///         spatial: (near: 3.0, far: 45.0),
///     ),
///     "ui_press": (files: ["sounds/ui_press.wav"], bus: Ui, volume: 0.6),
///     "warden_gate": (files: ["voice/warden_gate.ogg"], bus: Voice, subtitle: "@warden-gate", speaker: "@warden"),
///     "radio_beep": (files: ["sounds/beep.wav"], bus: Voice, subtitle: "@radio-orders", shown: 6.0),
///     "vent_hum": (files: ["sounds/vent_hum.wav"], priority: 40, spatial: (near: 2.0, far: 18.0), looped: true),
/// }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SoundDef {
    /// Files under the asset folder (`.wav`, `.ogg`): each play takes the next, so a step
    /// does not sound twice the same in a row.
    pub files: Vec<String>,
    #[serde(default)]
    pub bus: Bus,
    /// 0 to 1, before the bus.
    #[serde(default = "one")]
    pub volume: f32,
    /// Which sound keeps its voice when there are not enough: the higher (0 to 255). Between
    /// equals, the louder.
    #[serde(default = "default_priority")]
    pub priority: u8,
    /// Heard from where it happens, fading with distance and panned; without it, the same
    /// everywhere (menus, a stinger).
    #[serde(default)]
    pub spatial: Option<Falloff>,
    /// At most this many at once: one more takes the oldest one's voice.
    #[serde(default)]
    pub max_instances: Option<u32>,
    /// A line shown while it plays (a voice's words): text, or a message `@id` in the language
    /// played. Shown by a UI that binds the source `subtitles`, when `accessibility.subtitles` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// Who says it, before the line (`Warden: …`): text or `@id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Seconds its line shows at least, from the start: a long line on a short sound (a beep before a
    /// notice) is still read. Without it, a line shows for the sound's length and
    /// [`LINGER_S`](crate::subtitles::LINGER_S), at least [`MIN_SHOWN_S`](crate::subtitles::MIN_SHOWN_S).
    /// A sound without a `subtitle` may name it as the time of the lines the game says with it
    /// ([`say`](crate::subtitles::say) takes the seconds from the game, which reads them here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<f32>,
    /// Plays over and over until its [`SoundEmitter`](crate::SoundEmitter) lets it go (a hum, dripping
    /// water, a radio). Only an emitter plays a looped sound: an action, an event, a telegraph or the
    /// `sound_play` tool refuses it, and the content check finds a mixer event, a telegraph or a
    /// `Sound` effect that names one. With `spatial` it is heard while its emitter is within `far` and
    /// let go beyond; without, it is heard everywhere (rain). It takes no `subtitle` and no
    /// `max_instances`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub looped: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The priority of a sound that names none.
pub const DEFAULT_PRIORITY: u8 = 100;

fn one() -> f32 {
    1.0
}

fn default_priority() -> u8 {
    DEFAULT_PRIORITY
}

impl ContentDef for SoundDef {
    const KIND: &'static str = "sound";
    // Only what a machine shows or plays, or what lies outside a session: never in the replay check.
    const SIMULATION: bool = false;

    fn validate(&self, _: &str, refs: &mut Refs) {
        if self.files.is_empty() {
            refs.error("`files` is empty: name a .wav or .ogg file under the asset folder (`sounds/hit.wav`)");
        }
        if self.bus == Bus::Music {
            refs.error("bus `Music` is for tracks (`*.music.ron`): a sound plays on `Sfx`, `Ui` or `Voice`");
        }
        unit(refs, "volume", self.volume);
        if let Some(falloff) = self.spatial
            && !(falloff.near >= 0.0 && falloff.far > falloff.near && falloff.far.is_finite())
        {
            refs.error(format!("spatial: `near` 0 or more and `far` beyond it, not near {} far {}", falloff.near, falloff.far));
        }
        if self.max_instances == Some(0) {
            refs.error("max_instances: 0 would never play it; leave it out for no limit");
        }
        for file in &self.files {
            check_file(refs, "files", file);
        }
        if self.subtitle.as_deref().is_some_and(|s| s.trim().is_empty()) {
            refs.error("subtitle: empty; leave it out for a sound with no line");
        }
        if self.speaker.is_some() && self.subtitle.is_none() {
            refs.error("speaker without a subtitle: there is no line to put the name before");
        }
        if let Some(shown) = self.shown
            && !(shown > 0.0 && shown.is_finite())
        {
            refs.error(format!("shown: the seconds its line shows, more than 0, not {shown}; leave it out for its length"));
        }
        let texts = [(self.subtitle.as_deref(), "`subtitle`"), (self.speaker.as_deref(), "`speaker`")];
        shards_locale::check_messages(refs, texts.into_iter().filter_map(|(t, field)| Some((t?, field.to_owned()))));
        if self.looped {
            if self.subtitle.is_some() || self.speaker.is_some() || self.shown.is_some() {
                refs.error("looped: a sound that never ends has no line to show; leave out `subtitle`, `speaker` and `shown`");
            }
            if self.max_instances.is_some() {
                refs.error(
                    "looped: its voices belong to its emitters, and a limit would have them take each other's every tick; \
                     leave out `max_instances` (`priority` and the mixer's `voices` decide who is heard)",
                );
            }
        }
    }
}

/// A music track (kind `music`): stems that loop together, each heard in some layers. The layers
/// go from calm to intense; the last whose `when` holds is played, else the first.
///
/// ```ron
/// {
///     "arena": (
///         stems: [
///             (file: "music/arena_pad.wav", layers: ["explore", "tension", "combat"]),
///             (file: "music/arena_pulse.wav", layers: ["tension", "combat"]),
///             (file: "music/arena_drums.wav", layers: ["combat"]),
///         ],
///         layers: [
///             (name: "explore"),
///             (name: "tension", when: "npc_near"),
///             (name: "combat", when: "npc_engaged"),
///         ],
///     ),
/// }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MusicDef {
    /// Files of one length that start together and loop.
    pub stems: Vec<StemDef>,
    pub layers: Vec<LayerDef>,
    /// Seconds a stem takes to come in.
    #[serde(default = "default_fade_in")]
    pub fade_in: f32,
    /// Seconds a stem takes to go.
    #[serde(default = "default_fade_out")]
    pub fade_out: f32,
    /// Seconds the rules must want a calmer layer before it plays: a fight that pauses for
    /// breath keeps its music. A more intense layer comes at once.
    #[serde(default = "default_hold")]
    pub hold: f32,
    /// 0 to 1, before the music bus.
    #[serde(default = "one")]
    pub volume: f32,
}

fn default_fade_in() -> f32 {
    1.0
}

fn default_fade_out() -> f32 {
    2.5
}

fn default_hold() -> f32 {
    5.0
}

/// One file of a track and the layers it is heard in.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StemDef {
    pub file: String,
    pub layers: Vec<String>,
    /// 0 to 1, within the track.
    #[serde(default = "one")]
    pub volume: f32,
}

/// A layer of a track and when it plays.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LayerDef {
    pub name: String,
    /// A condition by name (`npc_engaged`; `!npcs_calm` for its opposite): the engine's, a
    /// module's or the game's (`add_ai_condition`), the ones `step --until named:…` takes. The
    /// first layer has none: it plays when no other holds.
    #[serde(default)]
    pub when: Option<String>,
}

/// A layer's `when` as the music reads it: whether it is negated (one `!`), and the
/// condition's name.
pub fn condition_of(when: &str) -> (bool, &str) {
    when.strip_prefix('!').map_or((false, when.trim()), |name| (true, name.trim()))
}

impl MusicDef {
    /// The layer called `name`, by its place from calm (0) to intense.
    pub fn layer_index(&self, name: &str) -> Option<usize> {
        self.layers.iter().position(|l| l.name == name)
    }

    /// Layer names, calm to intense.
    pub fn layer_names(&self) -> impl Iterator<Item = &str> {
        self.layers.iter().map(|l| l.name.as_str())
    }
}

impl ContentDef for MusicDef {
    const KIND: &'static str = "music";
    // Only what a machine shows or plays, or what lies outside a session: never in the replay check.
    const SIMULATION: bool = false;

    fn validate(&self, _: &str, refs: &mut Refs) {
        if self.stems.is_empty() {
            refs.error("`stems` is empty: name the files that play (`(file: \"music/theme.ogg\", layers: [\"explore\"])`)");
        }
        if self.layers.is_empty() {
            refs.error("`layers` is empty: name at least the calm one (`(name: \"explore\")`)");
        }
        let mut names = BTreeSet::new();
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.name.trim().is_empty() {
                refs.error(format!("layers[{i}]: `name` is empty"));
            } else if !names.insert(layer.name.as_str()) {
                refs.error(format!("layers[{i}]: two layers are called `{}`", layer.name));
            }
            match (i, &layer.when) {
                (0, Some(when)) => refs.error(format!(
                    "layer `{}` is the first, the one played when no other holds: it takes no `when` (`{when}`)",
                    layer.name
                )),
                (1.., None) => refs.error(format!("layer `{}` has no `when`: only the first layer plays without one", layer.name)),
                (_, Some(when)) if matches!(condition_of(when).1.chars().next(), None | Some('!')) => refs.error(format!(
                    "layer `{}`: `when` names no condition (`npc_engaged`, `!npcs_calm`; one `!` at most), not `{when}`",
                    layer.name
                )),
                _ => {}
            }
        }
        let mut heard = BTreeSet::new();
        for (i, stem) in self.stems.iter().enumerate() {
            unit(refs, &format!("stems[{i}].volume"), stem.volume);
            check_file(refs, &format!("stems[{i}].file"), &stem.file);
            if stem.layers.is_empty() {
                refs.error(format!("stems[{i}] (`{}`) is in no layer: name the layers it is heard in", stem.file));
            }
            for layer in &stem.layers {
                if names.contains(layer.as_str()) {
                    heard.insert(layer.as_str());
                } else {
                    let hint = closest(layer, names.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
                    refs.error(format!("stems[{i}]: no layer `{layer}`{hint}; layers: {}", self.layer_names().collect::<Vec<_>>().join(", ")));
                }
            }
        }
        for layer in &self.layers {
            if !heard.contains(layer.name.as_str()) && !layer.name.trim().is_empty() {
                refs.error(format!("layer `{}` plays no stem: add it to a stem's `layers`", layer.name));
            }
        }
        for (field, value) in [("fade_in", self.fade_in), ("fade_out", self.fade_out), ("hold", self.hold)] {
            if !(value >= 0.0 && value.is_finite()) {
                refs.error(format!("{field}: seconds, 0 or more, not {value}"));
            }
        }
        unit(refs, "volume", self.volume);
        // Stems loop together: one shorter than the others would drift a little more each time round.
        if let Some(root) = refs.asset_root() {
            let lengths: Vec<(&str, f64)> = self.stems.iter().filter_map(|s| Some((s.file.as_str(), file_seconds(&root.join(&s.file)).ok()?))).collect();
            if let Some((first, length)) = lengths.first()
                && let Some((other, theirs)) = lengths.iter().find(|(_, l)| (l - length).abs() > 0.01 * length.max(0.01))
            {
                refs.error(format!("stems loop together, so they need one length: `{first}` plays {length:.2} s, `{other}` {theirs:.2} s"));
            }
        }
    }
}

/// The engine's events a game can give a sound in its [`AudioDef`], and where each is heard.
pub const EVENTS: [(&str, &str); 13] = [
    ("damaged", "a hit landed, at the one hit"),
    ("blocked", "a hit met a guard, at the defender"),
    ("parried", "a hit was parried, at the defender"),
    ("evaded", "a hit missed a dodge, at the defender"),
    ("guard_broken", "a guard broke, at the defender"),
    ("staggered", "someone staggered"),
    ("died", "someone died"),
    ("healed", "someone was healed"),
    ("ui_moved", "the focus moved in a menu"),
    ("ui_pressed", "a button was pressed"),
    ("ui_changed", "a slider, toggle, choice or tab changed"),
    ("ui_opened", "a menu opened (not a HUD)"),
    ("ui_closed", "a menu closed (not a HUD)"),
];

/// The game's mixer (kind `audio`; one per game, any id): how many voices play at once, the
/// track that starts the game, and the sounds of the engine's events.
///
/// ```ron
/// {
///     "mixer": (
///         voices: 24,
///         music: "arena",
///         sounds: {"damaged": "hit", "blocked": "block", "ui_pressed": "ui_press"},
///     ),
/// }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AudioDef {
    /// Sounds playing at once, music aside: one more takes the weakest's voice or is not
    /// heard.
    #[serde(default = "default_voices")]
    pub voices: u32,
    /// The track playing from the start (`music_set` changes it).
    #[serde(default)]
    pub music: Option<String>,
    /// An engine event ([`EVENTS`]: `damaged`, `ui_pressed`, …) → the sound it plays.
    #[serde(default)]
    pub sounds: BTreeMap<String, String>,
    /// Walls between the listener and a sound muffle it (off when left out): a ray from the ears to
    /// each spatial voice, through the physics world's `WORLD` colliders, takes a share of its volume
    /// for every collider in the way ([`OcclusionDef`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion: Option<OcclusionDef>,
}

/// What a collider between the ears and a sound takes from it (the mixer's `occlusion`).
///
/// ```ron
/// { "mixer": (voices: 24, occlusion: Some((gain: 0.3))) }
/// ```
///
/// A collider that carries a [`SoundTransmission`](crate::SoundTransmission) lets that share through
/// instead (glass 0.6, a curtain 0.8, a slab 0.1). Colliders in a row multiply.
#[derive(Serialize, Deserialize, Reflect, Clone, Copy, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OcclusionDef {
    /// The share of a sound a collider with no `SoundTransmission` lets through: 0 (nothing) to 1
    /// (all, which turns the wall into air).
    #[serde(default = "default_through")]
    pub gain: f32,
}

/// What an ordinary collider lets through when the mixer names no `gain`.
pub const DEFAULT_THROUGH: f32 = 0.3;

fn default_through() -> f32 {
    DEFAULT_THROUGH
}

impl Default for OcclusionDef {
    fn default() -> Self {
        Self { gain: DEFAULT_THROUGH }
    }
}

/// Voices when the game has no [`AudioDef`].
pub const DEFAULT_VOICES: u32 = 24;

/// The most voices a mixer may have.
pub const MAX_VOICES: u32 = 256;

fn default_voices() -> u32 {
    DEFAULT_VOICES
}

impl Default for AudioDef {
    fn default() -> Self {
        Self { voices: DEFAULT_VOICES, music: None, sounds: BTreeMap::new(), occlusion: None }
    }
}

impl ContentDef for AudioDef {
    const KIND: &'static str = "audio";
    // Only what a machine shows or plays, or what lies outside a session: never in the replay check.
    const SIMULATION: bool = false;

    fn validate(&self, id: &str, refs: &mut Refs) {
        let others: Vec<&str> = refs.iter::<AudioDef>().map(|(other, _)| other).filter(|other| *other != id).collect();
        if !others.is_empty() {
            refs.error(format!("a game has one mixer, and this one has more: {}; keep one", others.join(", ")));
        }
        if !(1..=MAX_VOICES).contains(&self.voices) {
            refs.error(format!("voices: 1 to {MAX_VOICES}, not {}", self.voices));
        }
        if let Some(occlusion) = self.occlusion
            && !(0.0..=1.0).contains(&occlusion.gain)
        {
            refs.error(format!("occlusion.gain: 0 to 1, not {}", occlusion.gain));
        }
        if let Some(music) = &self.music {
            refs.require::<MusicDef>(music, "music");
        }
        for (event, sound) in &self.sounds {
            if EVENTS.iter().any(|(name, _)| name == event) {
                refs.require::<SoundDef>(sound, &format!("sounds.{event}"));
                if refs.iter::<SoundDef>().any(|(id, def)| id == sound && def.looped) {
                    refs.error(format!(
                        "sounds.{event}: `{sound}` loops, and an event plays a sound once; only a `SoundEmitter` plays a looped sound"
                    ));
                }
            } else {
                let names = EVENTS.iter().map(|(name, _)| *name);
                let hint = closest(event, names.clone()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
                refs.error(format!("sounds: no engine event `{event}`{hint}; events: {}", names.collect::<Vec<_>>().join(", ")));
            }
        }
        // What the game's attacks warn with and its actions play is heard only if it is a sound:
        // every telegraph's `sound` and every `Sound` effect is checked here, where the mixer is.
        let sounds: Vec<&str> = refs.iter::<SoundDef>().map(|(sound, _)| sound).collect();
        let looped: BTreeSet<&str> = refs.iter::<SoundDef>().filter(|(_, def)| def.looped).map(|(sound, _)| sound).collect();
        let hint = |sound: &str| {
            closest(sound, sounds.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default()
        };
        let mut missing = Vec::new();
        for (action, def) in refs.iter::<ActionDef>() {
            if let Some(sound) = def.telegraph.as_ref().and_then(|t| t.sound.as_deref()) {
                if !sounds.contains(&sound) {
                    missing.push(format!("action `{action}` warns with sound `{sound}`, which no `*.sound.ron` defines{}", hint(sound)));
                } else if looped.contains(sound) {
                    missing.push(format!("action `{action}` warns with sound `{sound}`, which loops; only a `SoundEmitter` plays a looped sound"));
                }
            }
            for (i, effect) in def.effects.iter().enumerate() {
                if let Effect::Sound(sound) = &effect.effect {
                    if !sounds.contains(&sound.as_str()) {
                        missing.push(format!(
                            "action `{action}` effects[{i}] plays sound `{sound}`, which no `*.sound.ron` defines{}",
                            hint(sound)
                        ));
                    } else if looped.contains(sound.as_str()) {
                        missing.push(format!(
                            "action `{action}` effects[{i}] plays sound `{sound}`, which loops; only a `SoundEmitter` plays a looped sound"
                        ));
                    }
                }
            }
        }
        for line in missing {
            refs.error(line);
        }
    }
}

fn unit(refs: &mut Refs, field: &str, value: f32) {
    if !(0.0..=1.0).contains(&value) {
        refs.error(format!("{field}: 0 to 1, not {value}"));
    }
}

/// The file is under the asset folder and plays (when the database knows the folder).
fn check_file(refs: &mut Refs, field: &str, file: &str) {
    let Some(root) = refs.asset_root() else { return };
    let path = root.join(file);
    if !path.is_file() {
        refs.error(format!("{field}: no file `{file}` under the asset folder"));
        return;
    }
    match file_seconds(&path) {
        Ok(seconds) if seconds > 0.0 => {}
        Ok(_) => refs.error(format!("{field}: `{file}` is empty (it plays 0 s)")),
        Err(e) => refs.error(format!("{field}: `{file}` {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_fades_from_near_to_far() {
        let falloff = Falloff { near: 2.0, far: 12.0 };
        assert_eq!(falloff.gain(1.0), 1.0);
        assert_eq!(falloff.gain(2.0), 1.0);
        assert_eq!(falloff.gain(7.0), 0.25);
        assert_eq!(falloff.gain(12.0), 0.0);
        assert_eq!(falloff.gain(40.0), 0.0);
    }

    #[test]
    fn every_bus_is_a_setting_of_the_audio_group() {
        let settings = shards_settings::AudioSettings { master: 0.5, music: 0.5, sfx: 1.0, voice: 0.2, ui: 0.8 };
        let heard: Vec<f32> = Bus::ALL.iter().map(|b| settings.volume(b.name())).collect();
        assert_eq!(heard, [0.25, 0.5, 0.4, 0.1]);
    }
}
