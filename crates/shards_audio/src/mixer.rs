//! The mixer: which sounds play now, from where, how loud. It decides on the simulation's
//! clock (a `step` runs it, a pause stops it; menu sounds keep real time), the same with a window
//! and without one; playback (feature `playback`) only carries out what it decided.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ai_core::journal::{self, JournalKind};
use ai_core::AiId;
use bevy::prelude::*;
use shards_content::db::closest;
use shards_content::ContentDb;
use shards_settings::AudioSettings;

use crate::def::{AudioDef, Bus, Falloff, SoundDef, DEFAULT_VOICES};
use crate::emitter;
use crate::length::file_seconds;
use crate::occlusion::{self, Occluded};
use crate::subtitles::{self, Subtitles};

/// Where a sound is heard from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SoundAt {
    /// The same everywhere (menus); a spatial sound played so is heard at full volume.
    Everywhere,
    /// From an entity, following it while it exists.
    Entity(Entity),
    /// From a point; `source` is whose sound it is (an attacker's warning lands elsewhere).
    Point { position: Vec3, source: Option<Entity> },
}

/// Plays sound `sound` (a [`SoundDef`] id) as soon as commands apply:
/// `commands.trigger(PlaySound::new("coin", SoundAt::Entity(hero)))`.
#[derive(Event, Clone, Debug, PartialEq)]
pub struct PlaySound {
    pub sound: String,
    pub at: SoundAt,
}

impl PlaySound {
    pub fn new(sound: impl Into<String>, at: SoundAt) -> Self {
        Self { sound: sound.into(), at }
    }
}

/// The ear: the body this machine plays (the HUD's [`viewer`](shards_ui::viewer)) hears, facing
/// the way an [`AudioListener`] (the camera) faces, else the way the body does. It hears from the body's
/// feet, except a body that is looked through in the first person (F1b): that one hears from its eye.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Listener {
    pub position: Vec3,
    pub rotation: Quat,
    pub body: Option<Entity>,
}

/// Marks what the ears face with: the game's camera. Without one, they face with the body.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct AudioListener;

/// A sound playing.
#[derive(Clone, Debug, PartialEq)]
pub struct Voice {
    /// Numbered from 1 in the order sounds started.
    pub id: u64,
    pub sound: String,
    /// The file playing, of the sound's `files`.
    pub file: String,
    pub bus: Bus,
    pub priority: u8,
    /// What it comes from (it follows that entity while it exists), and that entity's id.
    pub source: Option<Entity>,
    pub source_id: Option<AiId>,
    /// Where it is heard from; `None`: everywhere the same.
    pub position: Option<Vec3>,
    pub falloff: Option<Falloff>,
    /// The sound's own volume, times the gain it was played at ([`play_scaled`]: a clip cue's strength).
    pub volume: f32,
    /// When it started, on its clock (the simulation's; real time on the `Ui` bus).
    pub started: f64,
    /// Seconds it plays (a looped sound's file: how long one round takes).
    pub length: f64,
    /// How loud it is heard now: its volume × the bus's × distance × what gets through the colliders
    /// in the way (0 out of earshot).
    pub gain: f32,
    pub distance: Option<f32>,
    /// Where it comes from, seen from the listener (−z ahead, +x right): what playback pans by.
    pub direction: Option<Vec3>,
    /// A looped sound's voice: it never plays out, its emitter keeps it or lets it go.
    pub looped: bool,
    /// The [`SoundEmitter`](crate::SoundEmitter) that holds this voice; `None` for a one-shot.
    pub emitter: Option<Entity>,
    /// What stands between the ears and the source: clear unless the mixer has `occlusion` and the voice
    /// is spatial.
    pub occlusion: Occluded,
    /// Its fade to silence once [`stop`] cut it short; `None` while it plays as it started.
    pub stopping: Option<Fading>,
}

/// A voice fading to silence ([`stop`]): heard at the share of its gain that is left, and let go once it is
/// silent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fading {
    /// When it is silent, on the voice's clock.
    pub until: f64,
    /// Seconds the fade takes.
    pub seconds: f64,
}

impl Fading {
    /// The share of the voice's gain heard at `now`: 1 when the fade starts, 0 once it is over.
    pub fn share(&self, now: f64) -> f32 {
        ((self.until - now) / self.seconds).clamp(0.0, 1.0) as f32
    }
}

/// A sound that was not heard, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Dropped {
    pub sound: String,
    pub reason: String,
    /// Simulation seconds when it was dropped.
    pub at: f64,
}

/// What [`play`] did.
#[derive(Clone, Debug, PartialEq)]
pub enum Played {
    /// It plays as voice `id`.
    Started { id: u64, stole: Option<String> },
    /// It is not heard: out of earshot, or weaker than every voice when all are taken.
    Dropped(String),
}

/// How many dropped sounds the mixer remembers for the `audio` tool.
const DROPS_KEPT: usize = 16;

/// The mixer's state: the voices playing and the ear.
#[derive(Resource, Default)]
pub struct Mixer {
    pub(crate) voices: Vec<Voice>,
    next_id: u64,
    /// Which of a sound's files plays next.
    turns: BTreeMap<String, usize>,
    pub listener: Option<Listener>,
    drops: VecDeque<Dropped>,
    /// Sounds started and dropped since the game began.
    pub started: u64,
    pub dropped: u64,
    /// Seconds of simulation and of real time the voices are timed by.
    sim_seconds: f64,
    real_seconds: f64,
    /// Sound files' lengths, read once.
    lengths: BTreeMap<String, f64>,
    /// Sounds named but not defined, journaled once each (and the emitters' mistakes, with the
    /// emitter in the key).
    pub(crate) unknown: BTreeSet<String>,
}

impl Mixer {
    /// The voices playing, oldest first.
    pub fn voices(&self) -> &[Voice] {
        &self.voices
    }

    /// The sounds dropped lately, oldest first.
    pub fn drops(&self) -> impl Iterator<Item = &Dropped> {
        self.drops.iter()
    }

    pub(crate) fn now(&self, bus: Bus) -> f64 {
        if bus == Bus::Ui { self.real_seconds } else { self.sim_seconds }
    }

    /// The simulation's clock and real time, now.
    pub(crate) fn clocks(&self) -> (f64, f64) {
        (self.sim_seconds, self.real_seconds)
    }

    fn drop_sound(&mut self, sound: &str, reason: String) -> Played {
        self.dropped += 1;
        if self.drops.len() == DROPS_KEPT {
            self.drops.pop_front();
        }
        self.drops.push_back(Dropped { sound: sound.to_owned(), reason: reason.clone(), at: self.sim_seconds });
        Played::Dropped(reason)
    }
}

/// Voices the game may play at once (its [`AudioDef`]).
pub fn voice_limit(world: &World) -> u32 {
    audio_def(world).map_or(DEFAULT_VOICES, |d| d.voices)
}

/// The game's mixer definition, if it has one.
pub fn audio_def(world: &World) -> Option<&AudioDef> {
    let db = world.get_resource::<ContentDb>()?;
    db.iter::<AudioDef>().next().map(|(_, def)| def)
}

/// What a voice of `bus` is heard at before distance: the player's volume of the bus.
pub fn bus_volume(world: &World, bus: Bus) -> f32 {
    world.get_resource::<AudioSettings>().map_or(1.0, |s| s.volume(bus.name()))
}

/// Plays `sound` from `at`, now: a voice of its own, the oldest of its own when it has its
/// `max_instances`, the weakest voice when all are taken and it is stronger, or nothing (out
/// of earshot, or weaker than every voice). An unknown sound is an error naming the closest.
pub fn play(world: &mut World, sound: &str, at: SoundAt) -> Result<Played, String> {
    play_scaled(world, sound, at, 1.0)
}

/// [`play`] at `gain`, a linear factor on the sound's own volume (a clip cue's strength: 0.5 is
/// half as loud as authored, above 1 louder, before the bus and the distance). 1 is [`play`]; 0, or
/// a value that is not a number, starts no voice (`Played::Dropped`, not counted as a drop).
pub fn play_scaled(world: &mut World, sound: &str, at: SoundAt, gain: f32) -> Result<Played, String> {
    let gain = if gain.is_finite() { gain.max(0.0) } else { 0.0 };
    let def = world.get_resource::<ContentDb>().and_then(|db| db.get::<SoundDef>(sound)).cloned();
    let Some(def) = def else {
        let db = world.get_resource::<ContentDb>();
        let known: Vec<&str> = db.map(|db| db.iter::<SoundDef>().map(|(id, _)| id).collect()).unwrap_or_default();
        let hint = closest(sound, known.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
        return Err(format!("no sound `{sound}`{hint}: content `*.sound.ron` defines sounds; {} known", known.len()));
    };
    if def.files.is_empty() {
        return Err(format!("sound `{sound}` names no file"));
    }
    if def.looped {
        return Err(format!(
            "sound `{sound}` loops, so it plays only from a `SoundEmitter` (a level node's `SoundEmitter = {{\"sound\": \"{sound}\"}}`), not as a one-shot"
        ));
    }
    if gain == 0.0 {
        return Ok(Played::Dropped(format!("its gain is 0 (`{sound}` would be silent)")));
    }
    start(world, sound, &def, at, gain, None)
}

/// Gives sound `def` (called `sound`) a voice from `at`: [`play_scaled`]'s one-shots and the emitters'
/// loops (`emitter` holds the voice) both come here, under the same rules for the limit, `max_instances`
/// and priority. An emitter's refusals are not counted as drops: it asks again at every refresh, and
/// `audio` says why it waits.
pub(crate) fn start(
    world: &mut World,
    sound: &str,
    def: &SoundDef,
    at: SoundAt,
    gain: f32,
    emitter: Option<Entity>,
) -> Result<Played, String> {
    if def.files.is_empty() {
        return Err(format!("sound `{sound}` names no file"));
    }
    refresh_listener(world);
    let limit = voice_limit(world) as usize;
    let (source, position) = match at {
        SoundAt::Everywhere => (None, None),
        SoundAt::Entity(e) => (Some(e), world.get::<Transform>(e).map(|t| t.translation)),
        SoundAt::Point { position, source } => (source, Some(position)),
    };
    let source_id = source.and_then(|e| world.get::<AiId>(e).copied());
    let position = position.filter(|_| def.spatial.is_some());
    let mut voice = Voice {
        id: 0,
        sound: sound.to_owned(),
        file: String::new(),
        bus: def.bus,
        priority: def.priority,
        source: if matches!(at, SoundAt::Entity(_)) { source } else { None },
        source_id,
        position,
        falloff: def.spatial,
        volume: def.volume * gain,
        started: 0.0,
        length: 0.0,
        gain: 0.0,
        distance: None,
        direction: None,
        looped: def.looped,
        emitter,
        occlusion: Occluded::CLEAR,
        stopping: None,
    };
    let bus = bus_volume(world, def.bus);
    let listener = world.resource::<Mixer>().listener;
    hear(&mut voice, listener, bus);
    occlude(world, &mut voice, listener);
    let root = shards_content::load::asset_root(world);
    let mut mixer = world.resource_mut::<Mixer>();
    let drop = |mixer: &mut Mixer, reason: String| if emitter.is_some() { Played::Dropped(reason) } else { mixer.drop_sound(sound, reason) };
    if let (Some(distance), Some(falloff)) = (voice.distance, voice.falloff)
        && distance >= falloff.far
    {
        return Ok(drop(&mut mixer, format!("out of earshot: {distance:.1} m, heard to {:.0} m", falloff.far)));
    }
    let (mut stole, mut taken) = (None, None);
    if let Some(max) = def.max_instances {
        let own: Vec<usize> = mixer.voices.iter().enumerate().filter(|(_, v)| v.sound == sound).map(|(i, _)| i).collect();
        if own.len() >= max as usize {
            let oldest = mixer.voices.remove(own[0]);
            taken = Some(oldest.id);
            stole = Some(format!("its own #{} (max {max})", oldest.id));
        }
    }
    if stole.is_none() && mixer.voices.len() >= limit {
        // The weakest: lowest priority, then quietest, then oldest.
        let weakest = mixer
            .voices
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.priority.cmp(&b.priority).then(a.gain.total_cmp(&b.gain)).then(a.id.cmp(&b.id)))
            .map(|(i, v)| (i, v.priority, v.gain, v.sound.clone(), v.id));
        match weakest {
            Some((i, priority, gain, name, id)) if (voice.priority, voice.gain) > (priority, gain) => {
                mixer.voices.remove(i);
                taken = Some(id);
                stole = Some(format!("{name} #{id} (voice limit {limit})"));
            }
            _ => {
                return Ok(drop(&mut mixer, format!("all {limit} voices play sounds as strong or stronger")));
            }
        }
    }
    let turn = mixer.turns.entry(sound.to_owned()).or_default();
    voice.file = def.files[*turn % def.files.len()].clone();
    *turn = (*turn + 1) % def.files.len();
    let file = voice.file.clone();
    let length = match mixer.lengths.get(&file) {
        Some(length) => *length,
        // Content's check reported a file that does not play; it is over at once.
        None => *mixer.lengths.entry(file.clone()).or_insert_with(|| file_seconds(&root.join(&file)).unwrap_or(0.0)),
    };
    mixer.next_id += 1;
    mixer.started += 1;
    voice.id = mixer.next_id;
    voice.started = mixer.now(voice.bus);
    voice.length = length;
    let id = voice.id;
    mixer.voices.push(voice);
    // What the taken voice said is no longer heard; what this one says shows.
    if let Some(taken) = taken {
        subtitles::cut(world, taken);
    }
    subtitles::start(world, sound, def, id, length);
    Ok(Played::Started { id, stole })
}

/// What stands between the ears and `voice`, when the mixer has `occlusion` and the voice has a place:
/// the colliders on the way take their share of its gain.
fn occlude(world: &World, voice: &mut Voice, listener: Option<Listener>) {
    voice.occlusion = Occluded::CLEAR;
    let (Some(def), Some(ear), Some(at)) = (audio_def(world).and_then(|d| d.occlusion), listener, voice.position) else { return };
    if voice.gain <= 0.0 {
        return;
    }
    let skip: Vec<Entity> = ear.body.into_iter().chain(voice.source).collect();
    let occluded = occlusion::measure(world, ear.position, at, &skip, def.gain);
    voice.gain *= occluded.passes;
    voice.occlusion = occluded;
}

/// [`play`] for what the engine hears (a cue, an event): an unknown sound is journaled once and
/// otherwise ignored.
pub(crate) fn play_quietly(world: &mut World, sound: &str, at: SoundAt, why: &str) {
    play_quietly_scaled(world, sound, at, 1.0, why);
}

/// [`play_quietly`] at `gain`, as [`play_scaled`] takes it.
pub(crate) fn play_quietly_scaled(world: &mut World, sound: &str, at: SoundAt, gain: f32, why: &str) {
    if let Err(e) = play_scaled(world, sound, at, gain)
        && world.resource_mut::<Mixer>().unknown.insert(sound.to_owned())
    {
        journal::emit(world, JournalKind::Warn, format!("audio: {why}: {e}"));
    }
}

/// Where the ear is now.
pub fn listener(world: &World) -> Option<Listener> {
    let body = shards_ui::viewer(world);
    let mut ears = world.try_query_filtered::<&Transform, With<AudioListener>>();
    let ears = ears.as_mut().and_then(|q| q.iter(world).next().copied());
    let body_at = body.and_then(|b| world.get::<Transform>(b).copied());
    let position = body.and_then(|b| first_person_ears(world, b)).or_else(|| body_at.or(ears).map(|t| t.translation))?;
    let rotation = ears.or(body_at).map_or(Quat::IDENTITY, |t| t.rotation);
    Some(Listener { position, rotation, body })
}

/// Where a body that is looked through in the first person hears from: its eye (F1b). It is looked
/// through when it faces its look (`face: Look`) or when an active `FirstPersonCamera` looks through it (its `target`,
/// else the local player when it names none). `None` for any
/// other body (it hears from its feet) and for an entity that is no character. The eye is the
/// simulation's (`shards_motion::eye_of`), not the camera's, so a headless run and a window hear alike,
/// and a shaking or bobbing view does not move the ears.
fn first_person_ears(world: &World, body: Entity) -> Option<Vec3> {
    let params = shards_motion::params_of(world, body)?;
    let local = world.get::<shards_core::LocalPlayer>(body).is_some();
    let looked_through = params.face == shards_motion::Facing::Look
        || world.try_query_filtered::<&shards_motion::FirstPersonCamera, ()>().is_some_and(|mut cameras| {
            cameras.iter(world).any(|camera| camera.looks_through(body, local))
        });
    looked_through.then(|| shards_motion::eye_of(world, body)).flatten()
}

fn refresh_listener(world: &mut World) {
    let ear = listener(world);
    world.resource_mut::<Mixer>().listener = ear;
}

/// Distance, direction and gain of `voice` heard from `listener` through a bus at `bus`.
fn hear(voice: &mut Voice, listener: Option<Listener>, bus: f32) {
    let mut gain = voice.volume * bus;
    voice.distance = None;
    voice.direction = None;
    if let (Some(at), Some(ear)) = (voice.position, listener) {
        let offset = at - ear.position;
        let distance = offset.length();
        voice.distance = Some(distance);
        voice.direction = Some(if distance > 1e-3 { ear.rotation.inverse() * (offset / distance) } else { Vec3::NEG_Z });
        if let Some(falloff) = voice.falloff {
            gain *= falloff.gain(distance);
        }
    }
    voice.gain = gain;
}

/// `FixedPostUpdate`: one tick of simulation time passes for the voices (the step, not the fixed
/// clock's delta, which is the whole jump after a snapshot load).
pub(crate) fn tick_clock(mut mixer: ResMut<Mixer>, time: Res<Time<Fixed>>) {
    mixer.sim_seconds += time.timestep().as_secs_f64();
}

/// `Update`: real time passes for the menu voices.
pub(crate) fn real_clock(mut mixer: ResMut<Mixer>, time: Res<Time<Real>>) {
    mixer.real_seconds = time.elapsed_secs_f64();
}

/// Voices that played out are over (a looped one never plays out: its emitter lets it go), and so are
/// the stopped ones whose fade is over; the others follow their sources and are heard again from where
/// the ear is now, through the buses' volumes now and what stands in the way, a stopped one at what is
/// left of its fade. Emitters first take voices in earshot and let go of the ones out of it
/// ([`emitter::sync`]), so calling this again with nothing changed starts nothing.
pub fn refresh(world: &mut World) {
    refresh_listener(world);
    let (sim, real) = world.resource::<Mixer>().clocks();
    let now = |bus: Bus| if bus == Bus::Ui { real } else { sim };
    world.resource_mut::<Mixer>().voices.retain(|v| {
        let silent = v.stopping.is_some_and(|f| now(v.bus) >= f.until);
        !silent && (v.looped || now(v.bus) - v.started < v.length)
    });
    emitter::sync(world);
    let buses: BTreeMap<Bus, f32> = Bus::ALL.iter().map(|b| (*b, bus_volume(world, *b))).collect();
    // The voices are taken out while they are heard, so the world can be read for what they follow and
    // what stands in their way.
    let mut voices = std::mem::take(&mut world.resource_mut::<Mixer>().voices);
    let listener = world.resource::<Mixer>().listener;
    for voice in &mut voices {
        if voice.position.is_some()
            && let Some(at) = voice.source.and_then(|e| world.get::<Transform>(e))
        {
            voice.position = Some(at.translation);
        }
        hear(voice, listener, buses[&voice.bus]);
        occlude(world, voice, listener);
        if let Some(fading) = voice.stopping {
            voice.gain *= fading.share(now(voice.bus));
        }
    }
    world.resource_mut::<Mixer>().voices = voices;
    if let Some(mut lines) = world.get_resource_mut::<Subtitles>() {
        subtitles::expire(&mut lines, sim, real);
    }
}

/// Cuts voice `voice` short (the id [`play`] gave it): a line interrupted, a radio call cut off. It fades to
/// silence over `fade` seconds on its clock and is let go; `0` stops it at once, and a tenth of a second
/// spares the ears a click (playback follows a voice's gain within about 0.05 s). Its line goes at once,
/// since a line is what is heard. Stopping it again only ever shortens the fade. An emitter's voice is
/// not stopped so, since its emitter would take one again: switch the emitter off
/// ([`emitter_stop`](crate::emitter_stop)).
pub fn stop(world: &mut World, voice: u64, fade: f32) -> Result<(), String> {
    let fade = if fade.is_finite() { f64::from(fade.max(0.0)) } else { 0.0 };
    let mut mixer = world.resource_mut::<Mixer>();
    let Some(i) = mixer.voices.iter().position(|v| v.id == voice) else {
        return Err(format!("no voice #{voice} plays: it played out, a stronger sound took it, or it was stopped"));
    };
    if mixer.voices[i].emitter.is_some() {
        let sound = mixer.voices[i].sound.clone();
        return Err(format!("voice #{voice} ({sound}) is a sound emitter's: switch the emitter off (`emitter_stop`) instead"));
    }
    if fade == 0.0 {
        mixer.voices.remove(i);
    } else {
        let now = mixer.now(mixer.voices[i].bus);
        let fading = Fading { until: now + fade, seconds: fade };
        let it = &mut mixer.voices[i];
        if it.stopping.is_none_or(|f| fading.until < f.until) {
            it.stopping = Some(fading);
        }
    }
    subtitles::cut(world, voice);
    Ok(())
}

/// Stops every voice (a new region, a loaded save: what played belongs to the world that went).
pub fn stop_all(world: &mut World) {
    world.resource_mut::<Mixer>().voices.clear();
    subtitles::clear(world);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(position: Option<Vec3>) -> Voice {
        Voice {
            id: 1,
            sound: "slam".into(),
            file: "slam.wav".into(),
            bus: Bus::Sfx,
            priority: 100,
            source: None,
            source_id: None,
            position,
            falloff: Some(Falloff { near: 1.0, far: 11.0 }),
            volume: 0.8,
            started: 0.0,
            length: 1.0,
            gain: 0.0,
            distance: None,
            direction: None,
            looped: false,
            emitter: None,
            occlusion: Occluded::CLEAR,
            stopping: None,
        }
    }

    #[test]
    fn a_voice_is_heard_by_distance_and_from_the_ears_side() {
        // The ear faces −x: a sound at −x is straight ahead, one at +z to its left.
        let ear = Listener { position: Vec3::ZERO, rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2), body: None };
        let mut ahead = voice(Some(Vec3::new(-6.0, 0.0, 0.0)));
        hear(&mut ahead, Some(ear), 0.5);
        assert_eq!(ahead.distance, Some(6.0));
        assert!((ahead.gain - 0.8 * 0.5 * 0.25).abs() < 1e-6, "{}", ahead.gain);
        assert!(ahead.direction.unwrap().abs_diff_eq(Vec3::NEG_Z, 1e-5), "{:?}", ahead.direction);
        let mut left = voice(Some(Vec3::new(0.0, 0.0, 3.0)));
        hear(&mut left, Some(ear), 1.0);
        assert!(left.direction.unwrap().abs_diff_eq(Vec3::NEG_X, 1e-5), "{:?}", left.direction);
    }

    #[test]
    fn the_ears_of_a_first_person_body_are_at_its_eye_and_any_other_is_at_its_feet() {
        use shards_core::LocalPlayer;
        use shards_motion::{CharacterController, CharacterParams, Facing};

        let mut world = World::new();
        let feet = Transform::from_xyz(2.0, 1.0, -3.0).with_rotation(Quat::from_rotation_y(0.5));
        // A body that faces its movement hears where it stands, as before.
        let body = world.spawn((LocalPlayer, CharacterController::default(), feet)).id();
        let ear = listener(&world).unwrap();
        assert_eq!((ear.position, ear.body), (Vec3::new(2.0, 1.0, -3.0), Some(body)));
        // One that faces its look hears from its eye: its feet plus its eye height, turned like the body without a camera.
        world.entity_mut(body).insert(CharacterParams { face: Facing::Look, eye_height: Some(1.6), ..default() });
        let ear = listener(&world).unwrap();
        assert!(ear.position.distance(Vec3::new(2.0, 2.6, -3.0)) < 1e-6, "{}", ear.position);
        assert!(ear.rotation.abs_diff_eq(feet.rotation, 1e-6));
        // A first-person camera turns the ears with the view, and moves a body that faces its movement to its eye too.
        world.entity_mut(body).insert(CharacterParams { eye_height: Some(1.5), ..default() });
        assert_eq!(listener(&world).unwrap().position, Vec3::new(2.0, 1.0, -3.0), "no camera, no first person");
        let view = Transform::from_xyz(2.0, 2.5, -3.0).with_rotation(Quat::from_rotation_y(1.0));
        // A first-person camera that looks through another body (a spectator's) does not move this body's ears.
        let other = world.spawn_empty().id();
        let looking = world.spawn((AudioListener, shards_motion::FirstPersonCamera::looking_through(other), view)).id();
        assert_eq!(listener(&world).unwrap().position, Vec3::new(2.0, 1.0, -3.0), "a camera of another body");
        world.entity_mut(looking).insert(shards_motion::FirstPersonCamera { active: false, ..default() });
        assert_eq!(listener(&world).unwrap().position, Vec3::new(2.0, 1.0, -3.0), "a camera that is switched off");
        world.entity_mut(looking).insert(shards_motion::FirstPersonCamera::default());
        let ear = listener(&world).unwrap();
        assert!(ear.position.distance(Vec3::new(2.0, 2.5, -3.0)) < 1e-6, "{}", ear.position);
        assert!(ear.rotation.abs_diff_eq(view.rotation, 1e-6), "faces the way the camera faces");
    }

    #[test]
    fn a_voice_without_a_place_is_heard_whole() {

        let mut menu = voice(None);
        hear(&mut menu, None, 0.5);
        assert_eq!((menu.gain, menu.distance, menu.direction), (0.4, None, None));
    }
}
