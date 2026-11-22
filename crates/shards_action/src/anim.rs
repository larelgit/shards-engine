//! Animation, as data and decisions: the `animset` content kind and the choice of the
//! clip a character should show.
//!
//! **Gameplay leads, animation catches up.** The action machine decides what happens and
//! when; an [`AnimSet`] only says which clip of a glTF file shows it. While an action runs,
//! its clip is *seeked* to the action's progress (gameplay time scaled to the clip's
//! length), so a hit window opens at the same moment of the animation on every machine and
//! at every frame rate. Between actions, locomotion clips (`idle`, `walk`, `run`, ...) loop,
//! at a speed that follows the character's velocity. [`select_clip`] is that decision as one
//! pure function; the systems that apply it to a Bevy `AnimationPlayer` live behind the
//! `render` feature, and nothing here needs a window.
//!
//! ## Content
//!
//! ```ron
//! // content/anim/humanoid.animset.ron
//! {
//!     "humanoid": (
//!         // One glTF holds the clips. Characters of other models share them when their
//!         // skeletons have the same node names.
//!         source: "models/humanoid.glb",
//!         // Library: our name -> the animation in the file (default: the same name).
//!         clips: {
//!             "idle": (),
//!             "walk": (),
//!             "run": (speed: 1.0),
//!             "fall": (name: "Armature|Falling"),
//!             "swing_1": (name: "Armature|Slash1", cues: [(at: 0.28, cue: "swoosh")]),
//!             "roll": (blend: 0.04),
//!             "hit": (),
//!             "die": (),
//!             // A clip from another file made for the same skeleton.
//!             "bow": (source: "models/humanoid_gestures.glb"),
//!         },
//!         // Locomotion state -> clip. Missing ones fall back (sprint -> run -> walk -> idle).
//!         locomotion: {"idle": "idle", "walk": "walk", "run": "run", "air": "fall",
//!                      "stagger": "hit", "dead": "die"},
//!         // Action id -> clip; or give the action `anim: "swing_1"`, or map its tags. (A clip
//!         // can play on a layer of the body: see "Layers".)
//!         actions: {"light_1": "swing_1", "roll": "roll"},
//!         tags: {},
//!         blend: 0.2,         // seconds between locomotion clips
//!         action_blend: 0.06, // seconds into an action's clip
//!         // What speed (m/s) the walk, run and sprint clips were made for.
//!         speeds: (walk: 2.0, run: 5.0, sprint: 8.0),
//!         // The root bone: its move along the ground is root motion (see "Root motion").
//!         root_motion: "root",
//!     ),
//! }
//! ```
//!
//! A character is [`Animated`] with the set's id and has the glTF scene as a child:
//! `render::GltfModel::load(&asset_server, "models/hero.glb")` loads the file whole, so the bridge,
//! which loads it for its clips too, shares its one read.
//!
//! ## Layers
//!
//! An action can play on one part of the body while locomotion goes on in the rest: a flask
//! drunk while walking, a guard raised while strafing. A layer is a bone and everything below
//! it; an action's mapping says which layer its clip plays on:
//!
//! ```ron
//! {
//!     "humanoid": (
//!         source: "models/humanoid.glb",
//!         clips: {"idle": (), "walk": (), "drink": (source: "models/humanoid_gestures.glb")},
//!         locomotion: {"idle": "idle", "walk": "walk"},
//!         // The layer `upper` is the spine with the head and the arms; the legs are the rest.
//!         layers: {"upper": (root: "spine")},
//!         // The plain form (`"drink": "drink"`) plays on the whole body; this one on the layer.
//!         // Tags map the same way.
//!         actions: {"drink": (clip: "drink", layer: "upper")},
//!     ),
//! }
//! ```
//!
//! While the action runs, the layer's bones follow its clip, seeked to the action's progress
//! like any other, and every other bone keeps its locomotion clip. The blends are the usual
//! ones: into the action's clip over that clip's `blend` (or `action_blend`), back to
//! locomotion over the locomotion clip's, or over the mapping's own `blend_out` (`(clip:
//! "drink", layer: "upper", blend_out: 0.075)`). [`select_layer`] says what plays on a layer;
//! [`select_clip_for`] chooses as if the action did not run. Picture only, like the rest of
//! the set.
//!
//! ## Event overlays
//!
//! An engine event that names a body ([`OVERLAY_EVENTS`]: a hit it blocked or took, a parry, a
//! guard broken, a stagger, a part broken, a landing) can play a clip on a layer for a while, over
//! whatever the layer plays:
//!
//! ```ron
//! //     overlays: {"damaged": (clip: "flinch", layer: "upper", fade_out: (0.2, 0.35), strength: 0.7)},
//! ```
//!
//! The clip plays from its start on the simulation's clock, comes in over `fade_in` (0 by default),
//! fades out between the two ages of `fade_out` and ends at the second; it takes at most `strength`
//! of the layer's bones ([`OverlayDef::share`]), and the action's own clip there goes on under it.
//! The same event again starts it over.
//!
//! ## Root motion
//!
//! An attack's lunge and a roll's distance are in its clip: the root bone moves forward. The
//! body moves by the action's `displacement` (gameplay leads), so the curve is baked from the
//! clip: the action names it (`bake: (animset: "humanoid", clip: "roll")`) and `--bake-motion`
//! (or the `motion_bake` tool) writes the root bone's move along the ground into the action
//! ([`crate::bake`]). The picture then keeps that bone in place: the model does not walk off
//! its body. A content check with the asset folder reads the files: a clip the file lacks, a
//! root bone it lacks, a baked curve that no longer matches its clip are errors.
//!
//! ## Phases on the clip
//!
//! An action's clip is seeked evenly over the run by default: 40 % of the run shows 40 % of the
//! clip. A clip made for one action and played by another with other timings (an enemy's slow
//! swing on the hero's quick one) then shows its strike before the strike lands. A clip that says
//! which phases it was timed to (`phases`, in clip seconds, as an action's are written) is seeked
//! phase by phase instead: each phase of an action with the same phases (the same names in the
//! same order) shows its own stretch of the clip, however long the action's phase lasts.
//!
//! ```ron
//! //     clips: {"light_1": (phases: [("windup", 0.2), ("active", 0.1), ("recovery", 0.3)])},
//! // an action with phases [("windup", 0.6), ("active", 0.15), ("recovery", 0.8)] shows the
//! // clip's first 0.2 s over its 0.6 s windup, and the strike when its active phase starts.
//! ```
//!
//! A phase of open length is no stretch: name it in `loops` (`loops: ["down"]`) and its stretch of
//! the clip repeats at the authored speed for as long as the action's phase lasts (a toppled body
//! breathing for 4 s on a 1.5 s breath); the next phase starts at its own start.
//!
//! A phase that outlasts its motion is no stretch either: name it in `holds` (`holds: ["dead"]`) and
//! its stretch of the clip plays at the authored speed, then holds its last pose for as long as the
//! action's phase lasts (a body that falls in 1.3 s and lies for the rest of a 3.5 s death, rather
//! than falling in slow motion for 3.5 s). An action's phase shorter than the clip's ends where it
//! ends, as a looped one does. A phase is in `loops` or in `holds`, not both.
//!
//! Time a run gains past its phases (a miss's longer recovery) stretches its last phase. An
//! action whose phases are not the clip's plays it evenly, as one without `phases` does;
//! the content check refuses one that shares some of the clip's phase names but not all, in
//! order (a typo or a phase left out). With the asset folder, a clip's `phases` must match the
//! file's: its `phases` extra when it has one, else its length. [`phase_seek`] is the mapping.
//!
//! ## Facing
//!
//! A body that faces a target (a lock-on, an aim point) can move away from where it faces. Its
//! gait then shows the clip of its heading from its facing ([`Heading`]: four quarters, with
//! [`HEADING_SLACK`] at their edges), and standing still it shows a combat idle:
//!
//! ```ron
//! //     locomotion: {"walk": "walk", "walk.back": "backstep", "walk.left": "strafe_l",
//! //                  "walk.right": "strafe_r", "idle": "idle", "idle.facing": "guard_idle"},
//! ```
//!
//! With no clip for a heading, `back` plays the gait's clip backward and `left` and `right` play it
//! as it is. A switch from one gait's clip to another starts the new one at the old one's place in
//! the stride ([`carry_phase`]). A whole-body mapping can turn the model to the heading of a run
//! that keeps the body's facing (`face: Keep`): `actions: {"dodge": (clip: "roll", turn: 0.08)}`
//! ([`ActionClip::turn`]; written on the `root_motion` bone).
//!
//! ## Sockets and props
//!
//! A character can carry things: a sword in the hand, a flask on the hip. A *socket* is a place on
//! a bone; a *prop* (content kind `prop`) is the thing; `carry` says what a moveset carries and in
//! which socket, `mounts` where it goes while an action runs (or just after a weapon switch).
//!
//! ```ron
//! // content/anim/props.prop.ron
//! { "sword": (size: (0.05, 0.9, 0.02), grip: (0.0, 0.3, 0.0)) }
//! // inside an animset:
//! //     sockets: {"hand_r": (bone: "hand.R"), "hip": (bone: "hips", offset: (0.1, 0.0, 0.0))},
//! //     carry: {"player": [(prop: "sword", socket: "hand_r")]},
//! //     mounts: {"drink": (moves: [(prop: "sword", to: Some("hip"), span: Secs(0.1, 0.9))],
//! //                        on_cut: {"sword": 0.05})},
//! ```
//!
//! Where a prop sits is a state, read every frame from the action's time (a [`Span`], as an
//! action's windows are), never an edge: it is right after a join in the middle of an action, a
//! snapshot load or a hold. `on_cut`: a whole-body action that cuts a run leaves the props where
//! they were for a moment. `mounts["@moveset"]` holds the rules for the seconds after the body's
//! moveset changed (`previous` shows the old moveset's props meanwhile, `ended_by` lists the events
//! that end it). `by_moveset` renames clips per moveset (a clip family). [`mount_places`] is the
//! pure decision; the bridge spawns the props under the bone, without `AiId`.
//!
//! ## Motion modes
//!
//! What a body shows can depend on the motion mode it is drawn in (`MotionState::mode`: `ground`,
//! `air`, `water`, `glide`, `mantle`, `ladder`; [`MODES`]). `mounts["@mode:water"]` holds the rules while the
//! body swims (`yield_after`: they step aside while an action runs in the mode and for that many
//! seconds after it, a blade swung afloat stays in the hand); `mounts["@left:water"]` holds the rules
//! after the body left it (`wait`: the moves are read at time 0 for up to that many seconds, or until
//! the body's fade out of its clip is done; then they run, and the bridge fires [`ModeLeft`]).
//! `LayerDef::off_in` switches a layer off in a mode, and `drops` lowers the model for the seconds an
//! action runs in a mode (a swing afloat). [`mount_places`] decides the props (the order: a cut hold,
//! the running action, `@mode`, `@left`, `@moveset`, the carry socket); the clocks are the
//! simulation's, kept per body by the bridge.
//!
//! ## Cues
//!
//! A clip can carry timed [`CueDef`]s; when the animation passes one, an [`AnimationCue`]
//! event fires for the entity. Cues are for effects and sound (a footstep, a whoosh, a
//! spark), never for gameplay: hits, i-frames and cancels come from the action's own
//! timeline.
//!
//! A cue may name a `bone` of the model (`(at: 0.0, cue: "footstep", bone: "foot.L")`): the event
//! then carries the bone's world position at the moment it fires, and the effect and the sound
//! play there instead of at the body's feet. `strength` (default 1, left out of the file at its
//! default) scales the effect's particles, in number and in size, and the sound's volume; a cue
//! of strength 0 plays nothing. Both are picture and sound only, like the cue itself.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use shards_content::{ContentDef, Refs};
use shards_motion::{MotionMode, MotionState};

use crate::clipfile::ClipFile;
use crate::def::{ActionDef, Face, MovesetDef, Phase, Span, EPS};

/// Marks a character the animation bridge drives: `animset` is the id of its
/// [`AnimSet`]. The glTF scene is a child entity. Presentation only: replays ignore it, so
/// a game may add it only when it has a window.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq)]
#[reflect(Component, Default)]
pub struct Animated {
    pub animset: String,
}

impl Animated {
    /// A character animated by `animset`.
    pub fn new(animset: impl Into<String>) -> Self {
        Self { animset: animset.into() }
    }
}

/// Which prop definition a body shows in place of a carried one: prop id -> the prop shown (a
/// variant of the same weapon). A picture marker like [`Animated`]: a game puts it on a body only
/// where something is drawn, from state every machine has, and replays and co-op ignore it.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq)]
#[reflect(Component, Default)]
pub struct PropSwaps(pub BTreeMap<String, String>);

/// The animation passed a cue of its clip: for effects and sound only (VFX, SFX). Never
/// drive gameplay from it.
#[derive(Event, Clone, Debug, PartialEq)]
pub struct AnimationCue {
    pub entity: Entity,
    /// The cue's name from content: `footstep`, `swoosh`.
    pub cue: String,
    /// The clip (our name for it) that fired it.
    pub clip: String,
    /// The bone the cue names ([`CueDef::bone`]), if any.
    pub bone: Option<String>,
    /// Where that bone was in the world when the cue fired (its `GlobalTransform`, as of the last
    /// frame that was drawn): `None` for a cue without a bone, or when the model has no bone of
    /// that name. Effects and sound play here instead of at the body's feet.
    pub at: Option<Vec3>,
    /// How strong the cue is ([`CueDef::strength`]): 1 plays as authored, less is a smaller burst
    /// and a quieter sound, 0 plays nothing.
    pub strength: f32,
}

impl AnimationCue {
    /// A cue of `clip` for `entity`, at the body, at full strength.
    pub fn new(entity: Entity, cue: impl Into<String>, clip: impl Into<String>) -> Self {
        Self { entity, cue: cue.into(), clip: clip.into(), bone: None, at: None, strength: 1.0 }
    }
}

/// A moment of a clip for effects or sound.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CueDef {
    /// Seconds into the clip, on the clip's own timeline.
    pub at: f32,
    pub cue: String,
    /// A bone of the model (a node name: `foot.L`): the cue then plays where that bone is when it
    /// fires ([`AnimationCue::at`]), not at the body's feet. Left out, the cue is the body's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    /// How strong the cue is, 0 or more: it scales the effect's particles (their number and size)
    /// and the sound's volume. 1 plays as authored, 0 plays nothing.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub strength: f32,
}

impl CueDef {
    /// A cue `cue` at `at` seconds, at the body, at full strength.
    pub fn new(at: f32, cue: impl Into<String>) -> Self {
        Self { at, cue: cue.into(), bone: None, strength: 1.0 }
    }

    /// The same cue at the bone `bone`.
    pub fn at_bone(mut self, bone: impl Into<String>) -> Self {
        self.bone = Some(bone.into());
        self
    }

    /// The same cue at `strength`.
    pub fn with_strength(mut self, strength: f32) -> Self {
        self.strength = strength;
        self
    }
}

fn one() -> f32 {
    1.0
}

/// One clip of an animset's library.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ClipDef {
    /// The animation's name in the glTF file; default: this clip's own name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The glTF file it comes from; default: the set's `source`. Another file's clips play on
    /// the set's characters when its skeleton has the same node names (clips shared by models).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Playback speed multiplier while it loops as locomotion (1: as authored).
    pub speed: f32,
    /// Seconds to blend into it; default: the set's `blend` (`action_blend` for actions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blend: Option<f32>,
    /// Effects and sound on its timeline.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<CueDef>,
    /// The phases the clip was timed to, in clip seconds (`[("windup", 0.2), ("active", 0.1),
    /// ("recovery", 0.3)]`): an action with the same phases is seeked phase by phase (see
    /// "Phases on the clip" in the [module](self)). Empty: every action plays it evenly.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<Phase>,
    /// Phases of `phases` whose stretch of the clip repeats at the authored speed while the action's
    /// phase lasts (a breath while a body lies toppled), instead of being stretched over it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<String>,
    /// Phases of `phases` whose stretch of the clip plays at the authored speed and then holds its
    /// last pose while the action's phase lasts on (a fall, then the body lying for the rest of a long
    /// death), instead of being stretched over it. Not also in `loops`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<String>,
}

impl Default for ClipDef {
    fn default() -> Self {
        Self {
            name: None,
            source: None,
            speed: 1.0,
            blend: None,
            cues: Vec::new(),
            phases: Vec::new(),
            loops: Vec::new(),
            holds: Vec::new(),
        }
    }
}

/// What speeds the locomotion clips were made for, and where a walk becomes a run.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GroundSpeeds {
    /// m/s the `walk` clip's feet match.
    pub walk: f32,
    /// m/s of the `run` clip.
    pub run: f32,
    /// m/s of the `sprint` clip.
    pub sprint: f32,
    /// Slower than this (m/s) the character stands: `idle`.
    pub idle_below: f32,
    /// The clip never plays slower than this multiple of its authored speed...
    pub min_scale: f32,
    /// ...nor faster than this one.
    pub max_scale: f32,
}

impl Default for GroundSpeeds {
    fn default() -> Self {
        Self { walk: 2.0, run: 5.0, sprint: 8.0, idle_below: 0.1, min_scale: 0.6, max_scale: 1.6 }
    }
}

/// How many layers a set can have: each is a mask group of Bevy's animation graph, a bit of a `u64`.
pub const MAX_LAYERS: usize = 64;

/// A part of the body (an animset's `layers`): a bone and everything below it. An action's clip
/// can play on a layer while the rest of the body keeps its locomotion clip.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LayerDef {
    /// The bone (a node name of the glTF) the layer starts at: that bone and all below it.
    pub root: String,
    /// Motion modes ([`mode_name`]) in which nothing plays on this layer: `["water"]`, where the
    /// strokes own both arms. A play already under way fades out over the locomotion clip's blend.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub off_in: Vec<String>,
}

impl LayerDef {
    /// A layer from `root` that is on in every mode.
    pub fn at(root: impl Into<String>) -> Self {
        Self { root: root.into(), off_in: Vec::new() }
    }
}

/// The name content gives a motion mode: `ground`, `air`, `water`, `glide`, `mantle`, `ladder`.
pub const MODES: [&str; 6] = ["ground", "air", "water", "glide", "mantle", "ladder"];

/// The content name of `mode` (see [`MODES`]).
pub fn mode_name(mode: MotionMode) -> &'static str {
    match mode {
        MotionMode::Ground => "ground",
        MotionMode::Air => "air",
        MotionMode::Water => "water",
        MotionMode::Glide => "glide",
        MotionMode::Mantle => "mantle",
        MotionMode::Ladder => "ladder",
    }
}

/// The motion mode content calls `name`.
pub fn parse_mode(name: &str) -> Option<MotionMode> {
    [MotionMode::Ground, MotionMode::Air, MotionMode::Water, MotionMode::Glide, MotionMode::Mantle, MotionMode::Ladder]
        .into_iter()
        .find(|m| mode_name(*m) == name)
}

/// The model goes `down` metres lower, at a constant rate that reaches it in `seconds`, while an
/// action runs in the mode this is the entry of (`drops: {"water": (down: 0.15, seconds: 0.22)}`),
/// and comes back the same way when the action ends or the mode changes.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DropDef {
    /// Metres, in the world.
    pub down: f32,
    /// Seconds to go all the way down (or up).
    pub seconds: f32,
}

impl DropDef {
    /// Metres a second.
    pub fn rate(&self) -> f32 {
        self.down / self.seconds
    }
}

/// The engine's events an animset's `overlays` can answer, and the body each one plays on.
pub const OVERLAY_EVENTS: [(&str, &str); 7] = [
    ("blocked", "a hit met the body's guard"),
    ("parried", "the body parried a hit"),
    ("damaged", "a hit landed on the body"),
    ("guard_broken", "the body's guard broke"),
    ("staggered", "the body staggered"),
    ("part_broken", "one of the body's parts broke"),
    ("landed", "the body came down from a fall or a jump"),
];

/// A clip played on one of the set's layers for a while when an engine event names the body (an
/// animset's `overlays`, by [`OVERLAY_EVENTS`]): `"damaged": (clip: "flinch", layer: "upper",
/// fade_out: (0.2, 0.35), strength: 0.7)`. It plays from the clip's start on the simulation's clock
/// (a pause holds it, a `step` moves it), over whatever the layer plays: the running action's own
/// clip there goes on under it and is back as it fades. The same event again starts it over.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OverlayDef {
    /// Our name of the clip.
    pub clip: String,
    /// The layer it plays on (a key of the set's `layers`).
    pub layer: String,
    /// Seconds it takes to come in (linearly); 0, the default: at once.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub fade_in: f32,
    /// Seconds after the event between which it fades out (smoothly); it ends at the second.
    pub fade_out: (f32, f32),
    /// The most of the layer's bones it takes: above 0, at most 1 (the default).
    #[serde(default = "full_strength", skip_serializing_if = "is_one")]
    pub strength: f32,
}

fn full_strength() -> f32 {
    1.0
}

impl OverlayDef {
    /// Its share of the layer's bones `age` seconds after the event: `strength`, faded in from
    /// `from` (0 to 1: 0, or the part of `strength` it had when the same event came again) and out.
    pub fn share(&self, age: f32, from: f32) -> f32 {
        let (out_from, out_to) = self.fade_out;
        if self.ended(age) {
            return 0.0;
        }
        let age = age.max(0.0);
        let rise = if self.fade_in > 0.0 { (from + age / self.fade_in).min(1.0) } else { 1.0 };
        let fall = if age <= out_from {
            1.0
        } else {
            let x = ((age - out_from) / (out_to - out_from)).min(1.0);
            1.0 - x * x * (3.0 - 2.0 * x)
        };
        rise * fall * self.strength
    }

    /// Whether it is over `age` seconds after the event.
    pub fn ended(&self, age: f32) -> bool {
        age >= self.fade_out.1
    }
}

/// What an action shows (a value of an animset's `actions` or `tags`): a clip of the library,
/// on the whole body or on one of the set's layers. Written `"swing_1"` for the whole body, or
/// `(clip: "drink", layer: "upper")`, or `(clip: "roll", turn: 0.08)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionClip {
    /// Our name of the clip.
    pub clip: String,
    /// The layer it plays on (a key of the set's `layers`); `None`: the whole body.
    pub layer: Option<String>,
    /// The run ends a weapon switch's equip (the props are the new moveset's, in their carry
    /// sockets). Default: true for a whole-body mapping, false for a layered one; written only
    /// when it differs from that default.
    pub ends_equip: bool,
    /// Seconds the model takes to turn to the run's heading, and back after it, for an action that
    /// keeps the body's facing (`face: Keep`): a roll to the side under lock-on rolls the model that
    /// way while the body still looks at its target. 0 (the default, not written): the model faces
    /// where the body faces. Whole body only; needs `root_motion` (the turn is written on that bone).
    pub turn: f32,
    /// Seconds its clip on a layer takes to leave when the action ends (or another action's clip takes
    /// the layer). `None` (the default, not written): the blend of what the body shows next. Layered
    /// mappings only.
    pub blend_out: Option<f32>,
}

impl ActionClip {
    /// `clip` on the whole body.
    pub fn body(clip: impl Into<String>) -> Self {
        Self { clip: clip.into(), layer: None, ends_equip: true, turn: 0.0, blend_out: None }
    }

    /// `clip` on `layer`.
    pub fn on(clip: impl Into<String>, layer: impl Into<String>) -> Self {
        Self { clip: clip.into(), layer: Some(layer.into()), ends_equip: false, turn: 0.0, blend_out: None }
    }
}

impl From<&str> for ActionClip {
    fn from(clip: &str) -> Self {
        Self::body(clip)
    }
}

impl Serialize for ActionClip {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let default_ends = self.layer.is_none();
        let turns = self.turn != 0.0;
        match &self.layer {
            None if self.ends_equip == default_ends && !turns && self.blend_out.is_none() => serializer.serialize_str(&self.clip),
            layer => {
                let extra = self.ends_equip != default_ends;
                let fields =
                    1 + usize::from(layer.is_some()) + usize::from(extra) + usize::from(turns) + usize::from(self.blend_out.is_some());
                let mut s = serializer.serialize_struct("ActionClip", fields)?;
                s.serialize_field("clip", &self.clip)?;
                if let Some(layer) = layer {
                    s.serialize_field("layer", layer)?;
                }
                if extra {
                    s.serialize_field("ends_equip", &self.ends_equip)?;
                }
                if turns {
                    s.serialize_field("turn", &self.turn)?;
                }
                if let Some(out) = self.blend_out {
                    s.serialize_field("blend_out", &out)?;
                }
                s.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ActionClip {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{Error, MapAccess, Visitor};

        struct Reader;
        impl<'de> Visitor<'de> for Reader {
            type Value = ActionClip;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a clip name (`\"drink\"`) or a clip on a layer (`(clip: \"drink\", layer: \"upper\")`)")
            }

            fn visit_str<E: Error>(self, clip: &str) -> Result<ActionClip, E> {
                Ok(ActionClip::body(clip))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ActionClip, A::Error> {
                let (mut clip, mut layer, mut ends_equip): (Option<String>, Option<String>, Option<bool>) = (None, None, None);
                let (mut turn, mut blend_out) = (0.0, None);
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "clip" => clip = Some(map.next_value()?),
                        "layer" => layer = map.next_value()?,
                        "ends_equip" => ends_equip = Some(map.next_value()?),
                        "turn" => turn = map.next_value()?,
                        "blend_out" => blend_out = map.next_value()?,
                        other => return Err(A::Error::unknown_field(other, &["clip", "layer", "ends_equip", "turn", "blend_out"])),
                    }
                }
                let ends_equip = ends_equip.unwrap_or(layer.is_none());
                let clip = clip.ok_or_else(|| A::Error::missing_field("clip"))?;
                Ok(ActionClip { clip, layer, ends_equip, turn, blend_out })
            }
        }
        deserializer.deserialize_any(Reader)
    }
}

/// An animation set (content kind `animset`): which clips of a glTF file show which
/// locomotion state and which action, how they blend, and their cues.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AnimSet {
    /// The glTF (`.gltf` or `.glb`) asset with the clips.
    pub source: String,
    /// The library: our clip name -> its definition.
    pub clips: BTreeMap<String, ClipDef>,
    /// Locomotion state ([`Locomotion::ALL`]) -> clip.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locomotion: BTreeMap<String, String>,
    /// Parts of the body an action's clip can play on while locomotion goes on in the rest:
    /// layer name -> where it starts. At most [`MAX_LAYERS`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layers: BTreeMap<String, LayerDef>,
    /// [`ActionDef`] id -> clip (and the layer it plays on, if not the whole body).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub actions: BTreeMap<String, ActionClip>,
    /// Action tag -> clip, for actions the set does not map by id (or `anim`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, ActionClip>,
    /// Seconds to blend between locomotion clips.
    #[serde(default = "default_blend")]
    pub blend: f32,
    /// Seconds to blend into an action's clip: short, the action is already under way.
    #[serde(default = "default_action_blend")]
    pub action_blend: f32,
    #[serde(default)]
    pub speeds: GroundSpeeds,
    /// The root bone (a node name of the glTF): its move along the ground is root motion. The
    /// picture keeps it in place, since the body moves by the action's `displacement`, which
    /// `--bake-motion` reads off this bone ([`crate::bake`]). `None`: the clips move nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_motion: Option<String>,
    /// Clip families: moveset id -> (clip -> the clip shown in its place). Applied to every pick:
    /// locomotion, action clips, layer plays. A moveset not listed shows the clips as named.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_moveset: BTreeMap<String, BTreeMap<String, String>>,
    /// Places on the skeleton a prop can sit: socket name -> bone and offset.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sockets: BTreeMap<String, SocketDef>,
    /// What a body carries, by its moveset (`Fighter::moveset`); `"*"`: any moveset not listed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub carry: BTreeMap<String, Vec<Carry>>,
    /// Where carried props go: by action key (an id, an `anim` name or a tag, looked up on their
    /// own, see [`AnimSet::mount_rules`]), `"@moveset"` (for the seconds after a weapon switch),
    /// `"@mode:<mode>"` (while the drawn motion mode is `<mode>`) and `"@left:<mode>"` (after the
    /// body left `<mode>`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mounts: BTreeMap<String, MountRules>,
    /// The model lowered while an action runs in a motion mode: mode name ([`MODES`]) -> drop (a
    /// swing afloat). Written on the `root_motion` bone, so the set needs one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub drops: BTreeMap<String, DropDef>,
    /// Event overlays: engine event ([`OVERLAY_EVENTS`]) -> the clip that plays on a layer for a
    /// while when the event names the body (a flinch on the upper body when it is hit).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overlays: BTreeMap<String, OverlayDef>,
}

fn is_zero(v: &Vec3) -> bool {
    *v == Vec3::ZERO
}

fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

/// A place on the skeleton (an animset's `sockets`): a bone, and where on it a prop is held.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SocketDef {
    /// The bone (a node name of the model) the socket rides.
    pub bone: String,
    /// Offset from the bone, in the bone's own axes and the rig's units at scale 1.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: Vec3,
    /// Turn from the bone, in degrees, applied as `EulerRot::XYZ`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub turn: Vec3,
}

impl SocketDef {
    /// The socket's place on its bone.
    pub fn transform(&self) -> Transform {
        Transform { translation: self.offset, rotation: euler_degrees(self.turn), scale: Vec3::ONE }
    }
}

/// Degrees about X, Y and Z, composed as Bevy's `EulerRot::XYZ`.
pub fn euler_degrees(turn: Vec3) -> Quat {
    Quat::from_euler(EulerRot::XYZ, turn.x.to_radians(), turn.y.to_radians(), turn.z.to_radians())
}

/// A prop a body carries in a socket: `(prop: "sword", socket: "hand_r")`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Carry {
    /// A `prop` id.
    pub prop: String,
    /// A key of the set's `sockets`.
    pub socket: String,
}

/// Where carried props go under one key of `mounts`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MountRules {
    /// Each move: while `span` is open, `prop` (if carried) sits in `to`; `to: None` hides it.
    /// The last open move of a prop wins.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub moves: Vec<MountMove>,
    /// While this span is open, the body shows the props of the moveset it carried before its
    /// moveset last changed (a weapon swap shows the old weapon until the hand reaches the back).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<Span>,
    /// `"@moveset"` only: engine events that end the equip at once (Godot: a blocked hit).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ended_by: Vec<String>,
    /// An action key only: when a whole-body action cuts this run, each listed prop that sat away
    /// from its carry socket at the cut stays where it was for these seconds of the new action
    /// (the drink: the bottle 0.025, the blade 0.055). Applies to reactions, staggers and death too.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub on_cut: BTreeMap<String, f32>,
    /// `"@mode:<mode>"` only: the rules step aside while an action runs in the mode and for this many
    /// seconds after it ends (the blade stays in hand 1.2 s after a swing afloat). 0: they never do.
    #[serde(skip_serializing_if = "is_zero_f32")]
    pub yield_after: f32,
    /// `"@left:<mode>"` only: the moves' clock (and the `left:<mode>` event, [`ModeLeft`]) waits up to
    /// this many seconds after the body left the mode, less once the body's fade out of the mode's
    /// clip is done. While it waits the moves are read at time 0.
    #[serde(skip_serializing_if = "is_zero_f32")]
    pub wait: f32,
}

/// One move of a prop: while `span` is open it sits in socket `to`, or is hidden (`to: None`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MountMove {
    pub prop: String,
    #[serde(default)]
    pub to: Option<String>,
    /// On the action's own timeline: `Secs(0.055, 0.745)`, `Phase("drink")`, `Phases("raise",
    /// "hold")`. Under `"@moveset"`, only `Secs`, counted from the weapon switch.
    pub span: Span,
}

/// A thing a body can carry (content kind `prop`, `*.prop.ron`): a glTF model or a plain box, and
/// how it sits in a socket. Presentation only: the engine spawns it under the socket's bone where
/// something is drawn.
///
/// ```ron
/// { "sword": (size: (0.05, 0.9, 0.02), grip: (0.0, 0.3, 0.0)) }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PropDef {
    /// A glTF scene (`models/sword.glb`; its first scene). Exactly one of `model` and `size`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// A box of this size (rig units at scale 1), drawn as a mesh with a `StandardMaterial`, so
    /// `shards_look` styles it like any mesh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<Vec3>,
    /// Its look (`shards_look`); default: the look of what it hangs from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look: Option<String>,
    /// Where it sits from its socket: moved by this, in the socket's axes and the rig's units.
    #[serde(skip_serializing_if = "is_zero")]
    pub grip: Vec3,
    /// Turned by this from the socket, in degrees (`EulerRot::XYZ`).
    #[serde(skip_serializing_if = "is_zero")]
    pub turn: Vec3,
    #[serde(skip_serializing_if = "is_one")]
    pub scale: f32,
}

impl Default for PropDef {
    fn default() -> Self {
        Self { model: None, size: None, look: None, grip: Vec3::ZERO, turn: Vec3::ZERO, scale: 1.0 }
    }
}

impl PropDef {
    /// Where the prop sits in a socket: the socket's place, then the prop's (`grip`, `turn`,
    /// `scale`).
    pub fn in_socket(&self, socket: &SocketDef) -> Transform {
        socket.transform()
            * Transform { translation: self.grip, rotation: euler_degrees(self.turn), scale: Vec3::splat(self.scale) }
    }
}

fn default_blend() -> f32 {
    0.2
}

fn default_action_blend() -> f32 {
    0.06
}

/// What a character's body is doing between actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Locomotion {
    Idle,
    Walk,
    Run,
    Sprint,
    /// Jumping or falling.
    Air,
    Swim,
    Glide,
    /// Climbing onto a ledge.
    Mantle,
    /// On a ladder, climbing or hanging.
    Ladder,
    /// Reeling from a hit (the moveset's `stagger` action).
    Stagger,
    Dead,
}

impl Locomotion {
    pub const ALL: [Locomotion; 11] = [
        Locomotion::Idle,
        Locomotion::Walk,
        Locomotion::Run,
        Locomotion::Sprint,
        Locomotion::Air,
        Locomotion::Swim,
        Locomotion::Glide,
        Locomotion::Mantle,
        Locomotion::Ladder,
        Locomotion::Stagger,
        Locomotion::Dead,
    ];

    /// The key in `locomotion:`.
    pub fn name(self) -> &'static str {
        match self {
            Locomotion::Idle => "idle",
            Locomotion::Walk => "walk",
            Locomotion::Run => "run",
            Locomotion::Sprint => "sprint",
            Locomotion::Air => "air",
            Locomotion::Swim => "swim",
            Locomotion::Glide => "glide",
            Locomotion::Mantle => "mantle",
            Locomotion::Ladder => "ladder",
            Locomotion::Stagger => "stagger",
            Locomotion::Dead => "dead",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// What to show when a set has no clip for this state, nearest first.
    pub fn fallbacks(self) -> &'static [Locomotion] {
        use Locomotion::*;
        match self {
            Idle | Stagger | Dead => &[],
            Walk => &[Run, Idle],
            Run => &[Walk, Idle],
            Sprint => &[Run, Walk, Idle],
            Air => &[Idle],
            Swim => &[Walk, Idle],
            Glide => &[Air, Idle],
            Mantle => &[Air, Idle],
            Ladder => &[Idle],
        }
    }

    /// A gait on the ground: walk, run or sprint (the states that have headings and carry their
    /// phase from one clip to the next).
    pub fn is_gait(self) -> bool {
        matches!(self, Locomotion::Walk | Locomotion::Run | Locomotion::Sprint)
    }
}

/// Which way a body that faces a target moves, seen from where it faces ([`BodyStatus::facing`]):
/// four quarters of 90° around its forward.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Heading {
    #[default]
    Forward,
    Back,
    /// To the body's left.
    Left,
    /// To the body's right.
    Right,
}

/// Degrees a body may move past the edge of its heading's quarter before it takes the next one: a
/// diagonal does not flicker between two clips.
pub const HEADING_SLACK: f32 = 10.0;

impl Heading {
    pub const ALL: [Heading; 4] = [Heading::Forward, Heading::Back, Heading::Left, Heading::Right];

    pub fn name(self) -> &'static str {
        match self {
            Heading::Forward => "forward",
            Heading::Back => "back",
            Heading::Left => "left",
            Heading::Right => "right",
        }
    }

    /// The middle of the quarter, in degrees from forward, clockwise seen from above (right +90).
    fn centre(self) -> f32 {
        match self {
            Heading::Forward => 0.0,
            Heading::Right => 90.0,
            Heading::Back => 180.0,
            Heading::Left => -90.0,
        }
    }

    /// The heading of `velocity` for a body whose forward is `forward` (both read on the ground
    /// plane): the quarter it falls in, except that `prev` is kept up to [`HEADING_SLACK`] degrees
    /// past its edge. A body that barely moves (or faces nowhere) keeps `prev`.
    pub fn of(forward: Vec3, velocity: Vec3, prev: Heading) -> Heading {
        let forward = Vec3::new(forward.x, 0.0, forward.z);
        let v = Vec3::new(velocity.x, 0.0, velocity.z);
        if forward.length_squared() < EPS || v.length_squared() < EPS {
            return prev;
        }
        // Bevy's right is +x when forward is -z: (-forward.z, 0, forward.x).
        let right = Vec3::new(-forward.z, 0.0, forward.x);
        let degrees = ops::atan2(v.dot(right), v.dot(forward)).to_degrees();
        let off = |h: Heading| ((degrees - h.centre() + 180.0).rem_euclid(360.0) - 180.0).abs();
        if off(prev) <= 45.0 + HEADING_SLACK {
            return prev;
        }
        Heading::ALL.into_iter().min_by(|a, b| off(*a).total_cmp(&off(*b))).unwrap_or(prev)
    }
}

/// The keys of `locomotion` a body that faces a target looks up first: a gait's heading (none
/// authored: `back` plays the gait's own clip backward, `left` and `right` play it as it is) and
/// the combat idle (none: `idle`).
pub const FACING_KEYS: [&str; 10] = [
    "idle.facing",
    "walk.back",
    "walk.left",
    "walk.right",
    "run.back",
    "run.left",
    "run.right",
    "sprint.back",
    "sprint.left",
    "sprint.right",
];

/// The key of `locomotion` for `state` seen from a body that faces a target and moves toward
/// `heading`: `walk.back`, `run.left`, `idle.facing`. `None`: forward, or a state without one.
pub fn facing_key(state: Locomotion, heading: Heading) -> Option<&'static str> {
    use Heading::*;
    use Locomotion::*;
    Some(match (state, heading) {
        (Idle, _) => "idle.facing",
        (Walk, Back) => "walk.back",
        (Walk, Left) => "walk.left",
        (Walk, Right) => "walk.right",
        (Run, Back) => "run.back",
        (Run, Left) => "run.left",
        (Run, Right) => "run.right",
        (Sprint, Back) => "sprint.back",
        (Sprint, Left) => "sprint.left",
        (Sprint, Right) => "sprint.right",
        _ => return None,
    })
}

impl AnimSet {
    /// The clip's definition.
    pub fn clip(&self, clip: &str) -> Option<&ClipDef> {
        self.clips.get(clip)
    }

    /// The animation's name in the glTF file for our clip name.
    pub fn file_name<'a>(&'a self, clip: &'a str) -> &'a str {
        self.clips.get(clip).and_then(|c| c.name.as_deref()).unwrap_or(clip)
    }

    /// The glTF file clip `clip` comes from: its own `source`, or the set's.
    pub fn source_of<'a>(&'a self, clip: &str) -> &'a str {
        self.clips.get(clip).and_then(|c| c.source.as_deref()).unwrap_or(&self.source)
    }

    /// Every glTF file the set reads, its own `source` first.
    pub fn sources(&self) -> Vec<&str> {
        let mut files = vec![self.source.as_str()];
        for clip in self.clips.values() {
            if let Some(source) = clip.source.as_deref()
                && !files.contains(&source)
            {
                files.push(source);
            }
        }
        files
    }

    /// Our clip for locomotion `state`, or the nearest fallback the set has, with the state
    /// it stands in for.
    pub fn locomotion_clip(&self, state: Locomotion) -> Option<(Locomotion, &str)> {
        std::iter::once(state).chain(state.fallbacks().iter().copied()).find_map(|s| {
            let clip = self.locomotion.get(s.name())?;
            self.clips.contains_key(clip).then_some((s, clip.as_str()))
        })
    }

    /// The clip for exactly this state, no fallbacks.
    fn exact(&self, state: Locomotion) -> Option<&str> {
        self.locomotion.get(state.name()).and_then(|clip| self.known(clip))
    }

    /// Our clip for `state` (as [`locomotion_clip`](Self::locomotion_clip) resolved it) on a body
    /// that faces a target and moves toward `heading`: the set's `walk.back`, `run.left`,
    /// `idle.facing` ([`facing_key`]), if it has one.
    pub fn facing_clip(&self, state: Locomotion, heading: Heading) -> Option<&str> {
        self.locomotion.get(facing_key(state, heading)?).and_then(|clip| self.known(clip))
    }

    /// The mapping an action looked up by `key` shows, found the way [`action_shown`](Self::action_shown)
    /// finds it; `None` for a key that is a clip's own name (an action's `anim`), which has none.
    pub fn mapping(&self, key: &str) -> Option<&ActionClip> {
        let known = |m: &&ActionClip| self.clips.contains_key(&m.clip);
        match self.actions.get(key).filter(known) {
            Some(m) => Some(m),
            None if self.clips.contains_key(key) => None,
            None => self.tags.get(key).filter(known),
        }
    }

    /// Seconds the model takes to turn to the heading of a run shown by `key` ([`ActionClip::turn`]);
    /// 0 when it does not turn (no mapping, or one on a layer).
    pub fn turn_of(&self, key: &str) -> f32 {
        self.mapping(key).filter(|m| m.layer.is_none()).map_or(0.0, |m| m.turn)
    }

    /// `clip` if the library has it.
    fn known<'a>(&self, clip: &'a str) -> Option<&'a str> {
        self.clips.contains_key(clip).then_some(clip)
    }

    /// Our clip for an action looked up by `key`: an id the set maps, a clip's own name (an
    /// action's `anim`), or a mapped tag.
    pub fn action_clip(&self, key: &str) -> Option<&str> {
        self.action_shown(key).map(|(clip, _)| clip)
    }

    /// What an action looked up by `key` shows, as [`action_clip`](Self::action_clip) finds it:
    /// our clip, and the layer it plays on (`None`: the whole body). A mapping that names a
    /// layer the set lacks plays on the whole body (the content check reports it).
    pub fn action_shown(&self, key: &str) -> Option<(&str, Option<&str>)> {
        self.mapped(&self.actions, key)
            .or_else(|| self.clips.get_key_value(key).map(|(k, _)| (k.as_str(), None)))
            .or_else(|| self.mapped(&self.tags, key))
    }

    /// What `map` (`actions` or `tags`) says `key` shows, if its clip is in the library.
    fn mapped<'a>(&'a self, map: &'a BTreeMap<String, ActionClip>, key: &str) -> Option<(&'a str, Option<&'a str>)> {
        let shown = map.get(key).filter(|a| self.clips.contains_key(&a.clip))?;
        let layer = shown.layer.as_deref().filter(|l| self.layers.contains_key(*l));
        Some((shown.clip.as_str(), layer))
    }

    /// Whether the action looked up by `key` plays on a layer, not on the whole body.
    pub fn is_layered(&self, key: &str) -> bool {
        self.action_shown(key).is_some_and(|(_, layer)| layer.is_some())
    }

    /// The position of layer `name` among the set's layers (sorted by name): its mask group in
    /// the animation graph.
    pub fn layer_index(&self, name: &str) -> Option<usize> {
        self.layers.keys().position(|k| k == name)
    }

    /// The key to look an action up under: its id if the set maps it, else its `anim` name
    /// if that is a clip of the set, else its first mapped tag; the id when nothing matches.
    pub fn action_key<'a>(&self, id: &'a str, anim: Option<&'a str>, tags: &'a [String]) -> &'a str {
        if self.actions.get(id).is_some_and(|c| self.clips.contains_key(&c.clip)) {
            return id;
        }
        if let Some(anim) = anim.filter(|a| self.clips.contains_key(*a)) {
            return anim;
        }
        tags.iter().map(String::as_str).find(|t| self.action_clip(t).is_some()).unwrap_or(id)
    }

    /// Seconds to blend into `clip`.
    pub fn blend_into(&self, clip: &str, action: bool) -> f32 {
        let own = self.clips.get(clip).and_then(|c| c.blend);
        own.unwrap_or(if action { self.action_blend } else { self.blend }).max(0.0)
    }

    /// The speed multiplier `clip` carries.
    fn clip_speed(&self, clip: &str) -> f32 {
        self.clips.get(clip).map_or(1.0, |c| c.speed)
    }

    /// The clip shown in place of `clip` for a body whose moveset is `moveset`: the set's
    /// `by_moveset` table for that moveset, when it renames `clip` to a clip of the library;
    /// `clip` itself otherwise.
    pub fn family<'a>(&'a self, moveset: Option<&str>, clip: &'a str) -> &'a str {
        moveset
            .and_then(|m| self.by_moveset.get(m))
            .and_then(|table| table.get(clip))
            .and_then(|renamed| self.known(renamed))
            .unwrap_or(clip)
    }

    /// Whether the run of the action looked up by `key` ends a weapon switch's equip: its
    /// mapping's `ends_equip`; an action the set maps to nothing is a whole-body action and ends it.
    pub fn ends_equip(&self, key: &str) -> bool {
        self.actions
            .get(key)
            .or_else(|| self.tags.get(key))
            .map_or_else(|| !self.is_layered(key), |mapping| mapping.ends_equip)
    }

    /// The mount rules of an action: `mounts[id]`, else `mounts[anim]`, else the first of the
    /// action's tags that has rules. (Not through [`action_key`](Self::action_key): that returns a
    /// tag only when `tags` maps it to a clip.)
    pub fn mount_rules(&self, id: &str, anim: Option<&str>, tags: &[String]) -> Option<&MountRules> {
        self.mount_key(id, anim, tags).and_then(|key| self.mounts.get(key))
    }

    /// The key of `mounts` whose rules an action has: see [`mount_rules`](Self::mount_rules).
    pub fn mount_key(&self, id: &str, anim: Option<&str>, tags: &[String]) -> Option<&str> {
        let has = |key: &str| self.mounts.get_key_value(key).map(|(k, _)| k.as_str());
        has(id).or_else(|| anim.and_then(has)).or_else(|| tags.iter().find_map(|t| has(t)))
    }

    /// The rules for the seconds after a weapon switch (`mounts["@moveset"]`).
    pub fn equip_rules(&self) -> Option<&MountRules> {
        self.mounts.get(EQUIP_KEY)
    }

    /// The rules for the motion mode `mode` (`mounts["@mode:water"]`).
    pub fn mode_rules(&self, mode: MotionMode) -> Option<&MountRules> {
        self.mounts.get(&format!("{MODE_KEY}{}", mode_name(mode)))
    }

    /// The rules for after the body left `mode` (`mounts["@left:water"]`).
    pub fn left_rules(&self, mode: MotionMode) -> Option<&MountRules> {
        self.mounts.get(&format!("{LEFT_KEY}{}", mode_name(mode)))
    }

    /// Whether layer `layer` plays nothing while the drawn motion mode is `mode` (`off_in`).
    pub fn layer_off(&self, layer: &str, mode: MotionMode) -> bool {
        self.layers.get(layer).is_some_and(|l| l.off_in.iter().any(|m| m == mode_name(mode)))
    }

    /// What a body of moveset `moveset` carries: the set's `carry` for it, else for `"*"`.
    pub fn carried(&self, moveset: Option<&str>) -> &[Carry] {
        moveset
            .and_then(|m| self.carry.get(m))
            .or_else(|| self.carry.get("*"))
            .map_or(&[], Vec::as_slice)
    }
}

/// The key of `mounts` whose rules hold for the seconds after a weapon switch.
pub const EQUIP_KEY: &str = "@moveset";

/// The start of the key of `mounts` whose rules hold while the drawn motion mode is the one that
/// follows (`"@mode:water"`).
pub const MODE_KEY: &str = "@mode:";

/// The start of the key of `mounts` whose rules hold after the body left the mode that follows
/// (`"@left:water"`).
pub const LEFT_KEY: &str = "@left:";

/// A presentation event: the bridge fires it for an animated body when the wait of its
/// `"@left:<mode>"` rules ends (the body left `<mode>`, and has stood out of it long enough, or its
/// fade out of the mode's clip is done). The event the overlay `left:<mode>` is named after; a game
/// can observe it. Not gameplay: it is not journaled, forwarded or replayed.
#[derive(Event, Clone, Debug, PartialEq)]
pub struct ModeLeft {
    pub entity: Entity,
    /// The mode left: `water` (see [`MODES`]).
    pub mode: String,
}

impl ModeLeft {
    /// The overlay event's name: `left:water`.
    pub fn name(&self) -> String {
        format!("left:{}", self.mode)
    }
}

/// The engine events whose arrival can end an equip (`MountRules::ended_by`): the names of the
/// combat and pair events.
pub const EQUIP_EVENTS: [&str; 11] = [
    "blocked",
    "parried",
    "damaged",
    "reacted",
    "guard_broken",
    "staggered",
    "evaded",
    "perfect_evaded",
    "pair_started",
    "pair_ended",
    "landed",
];

/// Where a carried prop sits now.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    /// In this socket (a key of the set's `sockets`).
    Socket(String),
    /// Not drawn.
    Hidden,
}

impl Place {
    /// The place a move asks for: its `to`.
    fn of(to: Option<&String>) -> Self {
        to.map_or(Place::Hidden, |socket| Place::Socket(socket.clone()))
    }
}

/// What the mounts are told of the body, per frame.
#[derive(Clone, Copy, Debug)]
pub struct MountView<'a> {
    /// What the body carries (prop, socket), and what it carried before its moveset changed.
    pub carried: &'a [(String, String)],
    pub previous: &'a [(String, String)],
    /// The running action: its rules, its definition (for phase names) and its time.
    pub run: Option<(&'a MountRules, &'a ActionDef, f32)>,
    /// The weapon switch's rules and the seconds since it; `None` after the equip has ended.
    pub equip: Option<(&'a MountRules, f32)>,
    /// The `"@mode:<mode>"` rules of the motion mode the body is in, while they apply (not while they
    /// yield), and the seconds since they began to apply.
    pub mode: Option<(&'a MountRules, f32)>,
    /// The `"@left:<mode>"` rules of the mode the body left, and the seconds on their clock: 0 while
    /// they wait.
    pub left: Option<(&'a MountRules, f32)>,
    /// Props a cut run holds where they were: (prop, where).
    pub cut: &'a [(String, Place)],
}

/// The span `(from, to)` in seconds of an equip's move (`Secs` only; any other is ignored).
fn secs_of(span: &Span) -> Option<(f32, f32)> {
    match span {
        Span::Secs(from, to) => Some((*from, *to)),
        _ => None,
    }
}

/// Whether `t` is in `(from, to)` of `span` on the timeline of `def` (half open, `[from, to)`).
fn open_in(span: &Span, def: Option<&ActionDef>, t: f32) -> bool {
    let range = match (span, def) {
        (Span::Secs(from, to), _) => Some((*from, *to)),
        (_, Some(def)) => def.span(span),
        _ => None,
    };
    range.is_some_and(|(from, to)| t >= from - EPS && t < to - EPS)
}

/// The props the body shows (the previous moveset's while `previous` is open, else the carried
/// ones) and where each sits now; the others of `carried` and `previous` are `Hidden`. In the
/// order of 8.7 of the design, first that applies: a cut hold, the running action's rules, the
/// motion mode's, the rules after leaving a mode, the weapon switch's rules, the carry socket.
pub fn mount_places(view: &MountView) -> Vec<(String, Place)> {
    // The action's `previous` span when its rules have one, else the weapon switch's.
    let previous_open = match view.run.and_then(|(rules, def, t)| rules.previous.as_ref().map(|s| open_in(s, Some(def), t))) {
        Some(open) => open,
        None => view.equip.and_then(|(rules, since)| rules.previous.as_ref().map(|s| open_in(s, None, since))).unwrap_or(false),
    };
    let shown = if previous_open { view.previous } else { view.carried };
    let mut places: BTreeMap<&str, Place> = BTreeMap::new();
    for (prop, _) in view.carried.iter().chain(view.previous) {
        places.insert(prop, Place::Hidden);
    }
    for (prop, socket) in shown {
        let place = view
            .cut
            .iter()
            .find(|(p, _)| p == prop)
            .map(|(_, place)| place.clone())
            .or_else(|| {
                let (rules, def, t) = view.run?;
                last_open(rules, prop, |span| open_in(span, Some(def), t))
            })
            .or_else(|| {
                let (rules, since) = view.mode?;
                last_open(rules, prop, |span| open_in(span, None, since))
            })
            .or_else(|| {
                let (rules, since) = view.left?;
                last_open(rules, prop, |span| open_in(span, None, since))
            })
            .or_else(|| {
                let (rules, since) = view.equip?;
                last_open(rules, prop, |span| open_in(span, None, since))
            })
            .unwrap_or_else(|| Place::Socket(socket.clone()));
        places.insert(prop, place);
    }
    places.into_iter().map(|(prop, place)| (prop.to_owned(), place)).collect()
}

/// Where the last of `rules`' moves for `prop` whose span is open puts it.
fn last_open(rules: &MountRules, prop: &str, open: impl Fn(&Span) -> bool) -> Option<Place> {
    rules.moves.iter().rfind(|m| m.prop == prop && open(&m.span)).map(|m| Place::of(m.to.as_ref()))
}

/// Whether the equip of a weapon switch still has something to show `since` seconds after it: a
/// move or the `previous` span that has not ended.
pub fn equip_open(rules: &MountRules, since: f32) -> bool {
    let last = rules
        .moves
        .iter()
        .filter_map(|m| secs_of(&m.span))
        .chain(rules.previous.as_ref().and_then(secs_of))
        .map(|(_, to)| to)
        .fold(0.0f32, f32::max);
    since < last
}

/// The props a cut holds: when a run with `rules` is cut by a whole-body action, each prop in its
/// `on_cut` that sat away from its carry socket (`places` against `carried`) stays there, for the
/// seconds `on_cut` says.
pub fn cut_holds(
    rules: &MountRules,
    carried: &[(String, String)],
    places: &BTreeMap<String, Place>,
) -> Vec<(String, Place, f32)> {
    rules
        .on_cut
        .iter()
        .filter_map(|(prop, secs)| {
            let socket = carried.iter().find(|(p, _)| p == prop).map(|(_, s)| s)?;
            let place = places.get(prop)?;
            (*place != Place::Socket(socket.clone())).then(|| (prop.clone(), place.clone(), *secs))
        })
        .collect()
}

/// What to play now. `clip` is our name for it; the bridge maps it to a node of the graph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipChoice<'a> {
    /// `None`: the set has nothing for this moment; whatever is playing stays as it is.
    pub clip: Option<&'a str>,
    /// Gameplay's position in the clip, 0..1 of its length (an action's progress, a
    /// mantle's). `None`: the clip plays by itself.
    pub seek_to: Option<f32>,
    /// Playback speed; 0 while the clip is seeked (gameplay moves it).
    pub speed: f32,
    /// Seconds to blend from what played before.
    pub blend: f32,
    /// Loops (locomotion) or plays once and holds its last pose.
    pub looping: bool,
    /// A gait's clip (walk, run, sprint, any heading): a switch from one to another starts the new
    /// clip where the old one was in its stride.
    pub gait: bool,
    /// The stretch of a seeked clip that a looping phase repeats (`loops`), 0..1 of its length,
    /// while the seek is in it: going back inside it starts the stretch over, and the cues say so.
    pub repeats: Option<(f32, f32)>,
}

impl<'a> ClipChoice<'a> {
    fn none(set: &AnimSet) -> Self {
        Self { clip: None, seek_to: None, speed: 1.0, blend: set.blend.max(0.0), looping: false, gait: false, repeats: None }
    }

    fn seeked(set: &AnimSet, clip: &'a str, (progress, repeats): (f32, Option<(f32, f32)>), action: bool) -> Self {
        Self {
            clip: Some(clip),
            seek_to: Some(progress),
            speed: 0.0,
            blend: set.blend_into(clip, action),
            looping: false,
            gait: false,
            repeats,
        }
    }

    /// The stretch going back inside which is a wrap for the cues, in seconds of a clip `duration`
    /// long: a looping phase's, or the whole of a looping clip.
    pub fn wraps(&self, duration: f32) -> Option<(f32, f32)> {
        if duration <= 0.0 {
            return None;
        }
        match self.repeats {
            Some((start, end)) => Some((start * duration, end * duration)),
            None => self.looping.then_some((0.0, duration)),
        }
    }
}

/// What the body is, besides what [`MotionState`] says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyStatus {
    /// The fighter has the `Dead` marker.
    pub dead: bool,
    /// The running action is the moveset's forced `stagger`.
    pub staggered: bool,
    /// The factor the body's speeds are scaled by (`MoveModifiers::pace`): a slowed run is
    /// still a run, played slower, not a walk. 1: no modifier.
    pub pace: f32,
    /// The body faces a target (a lock-on, an aim point: `LocomotionInput::face`) and moves this
    /// way from where it faces ([`Heading::of`]): a gait shows its heading's clip (`walk.back`;
    /// none: `back` plays the gait's clip backward, `left` and `right` play it as it is) and
    /// standing still shows `idle.facing` (none: `idle`). `None`: it faces where it goes.
    pub facing: Option<Heading>,
}

impl Default for BodyStatus {
    fn default() -> Self {
        Self { dead: false, staggered: false, pace: 1.0, facing: None }
    }
}

/// The locomotion state of a character: by motion mode, then by how fast it moves on the
/// ground (`idle` below `idle_below`, `walk` up to halfway to the run clip's speed, then
/// `run`, or `sprint` while the controller sprints).
pub fn locomotion_state(motion: &MotionState, speeds: &GroundSpeeds) -> Locomotion {
    locomotion_state_at(motion, speeds, 1.0)
}

/// [`locomotion_state`] for a body whose speeds are scaled by `pace` (a status slow, wading):
/// the ground speed is compared with the clips' speeds as it would be without the scale, so a
/// slowed run still shows the `run` clip (at a lower playback speed). Standing still stays
/// standing still.
pub fn locomotion_state_at(motion: &MotionState, speeds: &GroundSpeeds, pace: f32) -> Locomotion {
    match motion.mode {
        MotionMode::Water => Locomotion::Swim,
        MotionMode::Glide => Locomotion::Glide,
        MotionMode::Mantle => Locomotion::Mantle,
        MotionMode::Ladder => Locomotion::Ladder,
        MotionMode::Air => Locomotion::Air,
        MotionMode::Ground => {
            let v = Vec2::new(motion.velocity.x, motion.velocity.z).length();
            let walk_to_run = (speeds.walk + speeds.run) * 0.5;
            if v < speeds.idle_below {
                return Locomotion::Idle;
            }
            let v = if pace > EPS && pace != 1.0 { v / pace } else { v };
            if motion.sprinting && v > walk_to_run {
                Locomotion::Sprint
            } else if v < walk_to_run {
                Locomotion::Walk
            } else {
                Locomotion::Run
            }
        }
    }
}

/// How fast the clip for `state` plays so the feet match the ground speed.
fn ground_scale(state: Locomotion, motion: &MotionState, speeds: &GroundSpeeds) -> f32 {
    let authored = match state {
        Locomotion::Walk => speeds.walk,
        Locomotion::Run => speeds.run,
        Locomotion::Sprint => speeds.sprint,
        _ => return 1.0,
    };
    let v = Vec2::new(motion.velocity.x, motion.velocity.z).length();
    if authored <= EPS {
        return 1.0;
    }
    (v / authored).clamp(speeds.min_scale, speeds.max_scale.max(speeds.min_scale))
}

/// A running action as the animation reads it: the key [`AnimSet::action_key`] chose, seconds into
/// the run, the run's length (a miss's longer recovery included) and the action's phases.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionTime<'a> {
    pub key: &'a str,
    pub t: f32,
    pub length: f32,
    /// The action's phases; empty: every clip plays it evenly.
    pub phases: &'a [Phase],
}

impl<'a> ActionTime<'a> {
    pub fn new(key: &'a str, t: f32, length: f32, phases: &'a [Phase]) -> Self {
        Self { key, t, length, phases }
    }

    /// An action whose clips play evenly (`t / length`): no phases to match.
    pub fn even(key: &'a str, t: f32, length: f32) -> Self {
        Self::new(key, t, length, &[])
    }
}

/// `t / length`, 0 to 1; 1 for a run of no length.
fn even_progress(t: f32, length: f32) -> f32 {
    if length > EPS { (t / length).clamp(0.0, 1.0) } else { 1.0 }
}

/// Whether an action timed to `action` and a clip timed to `clip` have the same phases by name, in
/// the same order (their seconds may differ).
fn timed_alike(action: &[Phase], clip: &[Phase]) -> bool {
    !action.is_empty() && action.len() == clip.len() && action.iter().zip(clip).all(|(a, c)| a.0 == c.0)
}

/// The phases as `windup 0.2, active 0.1, recovery 0.3`.
fn phases_text(phases: &[Phase]) -> String {
    phases.iter().map(|p| format!("{} {}", p.0, p.1)).collect::<Vec<_>>().join(", ")
}

/// Where a clip timed to `clip` shows an action timed to `action`, `t` seconds into a run of
/// `length` seconds, as a fraction of the clip (0 to 1): the action's phase that `t` falls in is
/// found, and the same fraction of the clip's phase of that name is the place. A phase named in
/// `loops` repeats its stretch of the clip at the authored speed instead (`t - start` modulo its clip
/// seconds); one named in `holds` plays its stretch at the authored speed and then holds its last
/// pose (`t - start`, at most its clip seconds). Time the run has past the action's phases (a miss's
/// longer recovery) stretches the last phase, or holds it. `None` when the two are not timed alike
/// (other names, another order, a clip of no length): the clip plays evenly then. See "Phases on the
/// clip" in the [module](self).
pub fn phase_seek(action: &[Phase], clip: &[Phase], loops: &[String], holds: &[String], t: f32, length: f32) -> Option<f32> {
    phase_place(action, clip, loops, holds, t, length).map(|(at, _)| at)
}

/// [`phase_seek`] with the stretch of the clip that repeats: `Some((start, end))`, fractions of the
/// clip, while `t` is in a phase named in `loops`. Going back inside it is the stretch starting
/// over, not the clip ([`crossed_cues_in`]).
pub fn phase_place(
    action: &[Phase],
    clip: &[Phase],
    loops: &[String],
    holds: &[String],
    t: f32,
    length: f32,
) -> Option<(f32, Option<(f32, f32)>)> {
    if !timed_alike(action, clip) {
        return None;
    }
    let clip_total: f32 = clip.iter().map(|p| p.1).sum();
    if clip_total <= EPS {
        return None;
    }
    let action_total: f32 = action.iter().map(|p| p.1).sum();
    let extra = (length - action_total).max(0.0);
    let t = t.clamp(0.0, action_total + extra);
    let last = action.len() - 1;
    let (mut from, mut clip_from) = (0.0, 0.0);
    for (i, (a, c)) in action.iter().zip(clip).enumerate() {
        let lasts = if i == last { a.1 + extra } else { a.1 };
        let to = from + lasts;
        if t <= to || i == last {
            let into = (t - from).max(0.0);
            let looped = c.1 > EPS && loops.contains(&c.0);
            let on_clip = if looped {
                into % c.1
            } else if holds.contains(&c.0) {
                into.min(c.1)
            } else if lasts > EPS {
                (into / lasts).min(1.0) * c.1
            } else {
                c.1
            };
            let stretch = looped.then(|| (clip_from / clip_total, ((clip_from + c.1) / clip_total).min(1.0)));
            return Some((((clip_from + on_clip) / clip_total).clamp(0.0, 1.0), stretch));
        }
        from = to;
        clip_from += c.1;
    }
    None
}

impl AnimSet {
    /// Where on `clip` the running `action` is, 0 to 1: phase by phase when the clip is timed to the
    /// action's phases ([`phase_seek`]), else evenly over the run.
    pub fn progress(&self, clip: &str, action: &ActionTime) -> f32 {
        self.place(clip, action).0
    }

    /// [`AnimSet::progress`] with the stretch of the clip a looping phase repeats while the action
    /// is in it, fractions of the clip ([`phase_place`]).
    pub fn place(&self, clip: &str, action: &ActionTime) -> (f32, Option<(f32, f32)>) {
        let even = || (even_progress(action.t, action.length), None);
        let Some(def) = self.clips.get(clip) else { return even() };
        phase_place(action.phases, &def.phases, &def.loops, &def.holds, action.t, action.length).unwrap_or_else(even)
    }
}

/// The clip a character should show, as a pure function of the action machine and the
/// controller. `action` is the running action as `(key, t, length)` in seconds, the key
/// being what [`AnimSet::action_key`] chose. Same as [`select_clip_for`] for a body that is
/// alive and not staggered.
///
/// - An action with a clip in the set is **seeked** to `t / length`: gameplay leads.
/// - Otherwise the body's state decides: ledge climbs are seeked to their progress, every
///   other locomotion clip loops with its speed following the velocity.
pub fn select_clip<'a>(action: Option<(&str, f32, f32)>, motion: &MotionState, set: &'a AnimSet) -> ClipChoice<'a> {
    select_clip_for(action, motion, BodyStatus::default(), set)
}

/// [`select_clip`] with the body's status: a dead body shows the `dead` clip (seeked while
/// its death action runs, then held on its last pose; no clip: the pose it has stays), a
/// staggered one the `stagger` clip seeked to the stagger action's progress, and one that faces a
/// target its heading's clips ([`BodyStatus::facing`]).
///
/// An action whose clip plays on a layer is not the body's: this chooses as if it did not run
/// ([`select_layer`] says what plays on the layer).
pub fn select_clip_for<'a>(
    action: Option<(&str, f32, f32)>,
    motion: &MotionState,
    status: BodyStatus,
    set: &'a AnimSet,
) -> ClipChoice<'a> {
    select_clip_in(action, motion, status, set, None)
}

/// [`select_clip_for`] for a body whose moveset is `moveset`: every clip it picks goes through the
/// set's `by_moveset` table for that moveset (the clip family), blend and speed included.
pub fn select_clip_in<'a>(
    action: Option<(&str, f32, f32)>,
    motion: &MotionState,
    status: BodyStatus,
    set: &'a AnimSet,
    moveset: Option<&str>,
) -> ClipChoice<'a> {
    select_clip_timed(action.map(|(key, t, length)| ActionTime::even(key, t, length)), motion, status, set, moveset)
}

/// [`select_clip_in`] with the action's phases: a clip timed to them is seeked phase by phase
/// ([`AnimSet::progress`]), the action's clip as well as the dead and stagger clips.
pub fn select_clip_timed<'a>(
    action: Option<ActionTime>,
    motion: &MotionState,
    status: BodyStatus,
    set: &'a AnimSet,
    moveset: Option<&str>,
) -> ClipChoice<'a> {
    let action = action.filter(|a| !set.is_layered(a.key));
    let seeked = |clip: &'a str, a: &ActionTime| {
        let clip = set.family(moveset, clip);
        ClipChoice::seeked(set, clip, set.place(clip, a), true)
    };
    if let Some(a) = &action
        && let Some(clip) = set.action_clip(a.key)
    {
        return seeked(clip, a);
    }
    if status.dead {
        return match (set.exact(Locomotion::Dead), &action) {
            (Some(clip), Some(a)) => seeked(clip, a),
            (Some(clip), None) => {
                let clip = set.family(moveset, clip);
                ClipChoice {
                    clip: Some(clip),
                    seek_to: None,
                    speed: set.clip_speed(clip),
                    blend: set.blend_into(clip, false),
                    looping: false,
                    gait: false,
                    repeats: None,
                }
            }
            (None, _) => ClipChoice::none(set),
        };
    }
    if status.staggered
        && let (Some(clip), Some(a)) = (set.exact(Locomotion::Stagger), &action)
    {
        return seeked(clip, a);
    }
    let state = locomotion_state_at(motion, &set.speeds, status.pace);
    let Some((resolved, clip)) = set.locomotion_clip(state) else { return ClipChoice::none(set) };
    if resolved == Locomotion::Mantle
        && let Some(mantle) = &motion.mantle
    {
        return ClipChoice::seeked(set, set.family(moveset, clip), (mantle.t.clamp(0.0, 1.0), None), false);
    }
    // Facing a target: the heading's clip, or the gait's own (backward for `back`).
    let (clip, backward) = match status.facing.map(|h| (h, set.facing_clip(resolved, h))) {
        Some((_, Some(faced))) => (faced, false),
        Some((heading, None)) => (clip, heading == Heading::Back && resolved.is_gait()),
        None => (clip, false),
    };
    let clip = set.family(moveset, clip);
    let speed = ground_scale(resolved, motion, &set.speeds) * set.clip_speed(clip);
    ClipChoice {
        clip: Some(clip),
        seek_to: None,
        speed: if backward { -speed } else { speed },
        blend: set.blend_into(clip, false),
        looping: true,
        gait: resolved.is_gait(),
        repeats: None,
    }
}

/// What plays on a layer: the clip, and the layer's name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerChoice<'a> {
    /// The layer (a key of the set's `layers`).
    pub layer: &'a str,
    /// The clip on it, always seeked to the action's progress: gameplay leads here too.
    pub choice: ClipChoice<'a>,
    /// Seconds the clip takes to leave the layer ([`ActionClip::blend_out`]), if the mapping says.
    pub blend_out: Option<f32>,
}

/// The clip that plays on a layer now, if the running action is mapped to one. `action` is the
/// running action as for [`select_clip_for`]. A dead body or a staggering one shows nothing on
/// a layer: its whole body is the clip's.
pub fn select_layer<'a>(action: Option<(&str, f32, f32)>, status: BodyStatus, set: &'a AnimSet) -> Option<LayerChoice<'a>> {
    select_layer_in(action, status, set, None)
}

/// [`select_layer`] for a body whose moveset is `moveset` (the clip goes through `by_moveset`).
pub fn select_layer_in<'a>(
    action: Option<(&str, f32, f32)>,
    status: BodyStatus,
    set: &'a AnimSet,
    moveset: Option<&str>,
) -> Option<LayerChoice<'a>> {
    select_layer_timed(action.map(|(key, t, length)| ActionTime::even(key, t, length)), status, set, moveset)
}

/// [`select_layer_in`] with the action's phases: a clip timed to them is seeked phase by phase.
pub fn select_layer_timed<'a>(
    action: Option<ActionTime>,
    status: BodyStatus,
    set: &'a AnimSet,
    moveset: Option<&str>,
) -> Option<LayerChoice<'a>> {
    if status.dead || status.staggered {
        return None;
    }
    let a = action?;
    let (clip, Some(layer)) = set.action_shown(a.key)? else { return None };
    let blend_out = set.mapping(a.key).and_then(|m| m.blend_out);
    let clip = set.family(moveset, clip);
    Some(LayerChoice { layer, choice: ClipChoice::seeked(set, clip, set.place(clip, &a), true), blend_out })
}

/// The cues the animation passed going from `prev` to `now` (seconds on the clip's
/// timeline), in the order they were passed: after a wrap, the rest of the last pass comes
/// before the start of the new one.
///
/// - `prev` `None`: the clip just started; every cue at or before `now` fires.
/// - `now >= prev`: cues in `(prev, now]`.
/// - `now < prev`: it went back. A looping clip (`wrap_at`: its length) wrapped around: the
///   rest of the last pass and the start of the new one. Anything else restarted: cues up
///   to `now` fire again.
pub fn crossed_cues(cues: &[CueDef], prev: Option<f32>, now: f32, wrap_at: Option<f32>) -> Vec<&CueDef> {
    crossed_cues_in(cues, prev, now, wrap_at.map(|length| (0.0, length)))
}

/// [`crossed_cues`] for a clip that repeats the stretch `wraps` (`(start, end)` in seconds: a
/// looping phase's, [`ClipChoice::wraps`]; a looping clip's is `(0, length)`). Going back from inside
/// the stretch to inside it is a wrap: the rest of the stretch, then its start up to `now`; the cues of
/// earlier phases do not fire again. Going back from anywhere else is a restart.
pub fn crossed_cues_in(cues: &[CueDef], prev: Option<f32>, now: f32, wraps: Option<(f32, f32)>) -> Vec<&CueDef> {
    let inside = |at: f32, (start, end): (f32, f32)| at >= start - EPS && at <= end + EPS;
    let wrap = wraps.filter(|&stretch| prev.is_some_and(|p| now < p && inside(p, stretch) && inside(now, stretch)));
    let mut found: Vec<&CueDef> = cues
        .iter()
        .filter(|c| match (prev, wrap) {
            (None, _) => c.at <= now + EPS,
            (Some(p), _) if now >= p => c.at > p + EPS && c.at <= now + EPS,
            (Some(p), Some((start, end))) => (c.at > p + EPS && c.at <= end + EPS) || (c.at >= start - EPS && c.at <= now + EPS),
            (Some(_), None) => c.at <= now + EPS,
        })
        .collect();
    // After a wrap, the cues at or before `now` are the new pass's head: they come last.
    let head = |c: &CueDef| wrap.is_some() && c.at <= now + EPS;
    found.sort_by(|a, b| head(a).cmp(&head(b)).then(a.at.total_cmp(&b.at)));
    found
}

/// Where a gait's new clip of `new_len` seconds starts when the one before it, `old_len` seconds
/// long, was `at` seconds in: the same share of the stride, so the feet do not skip. `None` when
/// either clip has no length.
pub fn carry_phase(at: f32, old_len: f32, new_len: f32) -> Option<f32> {
    (old_len > EPS && new_len > EPS && at.is_finite()).then(|| (at / old_len).rem_euclid(1.0) * new_len)
}

/// [`crossed_cues`] for a clip played backward (a gait's clip on a body backing away, at a negative
/// speed): the cues passed going down from `prev` to `now`, in the order they were passed.
///
/// - `prev` `None`: the clip just started; it has passed nothing yet.
/// - `now <= prev`: cues in `[now, prev)`.
/// - `now > prev`: a looping clip (`wrap_at`: its length) wrapped below its start: the cues in
///   `[0, prev)`, then those in `[now, length]`. Anything else restarted: nothing fires.
pub fn crossed_cues_back(cues: &[CueDef], prev: Option<f32>, now: f32, wrap_at: Option<f32>) -> Vec<&CueDef> {
    let Some(prev) = prev else { return Vec::new() };
    let wrapped = now > prev && wrap_at.is_some();
    let mut found: Vec<&CueDef> = cues
        .iter()
        .filter(|c| match wrap_at {
            _ if now <= prev => c.at >= now - EPS && c.at < prev - EPS,
            Some(len) => c.at < prev - EPS || (c.at >= now - EPS && c.at <= len + EPS),
            None => false,
        })
        .collect();
    // After a wrap, the cues at or after `now` are the new pass's head: they come last.
    let head = |c: &CueDef| wrapped && c.at >= now - EPS;
    found.sort_by(|a, b| head(a).cmp(&head(b)).then(b.at.total_cmp(&a.at)));
    found
}

fn is_gltf(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    matches!(ext.as_deref(), Some("gltf" | "glb"))
}

impl AnimSet {
    /// With an asset folder: every file the set names can be read, has the animations its
    /// clips name, the bones their cues name and, with `root_motion`, that bone, unturned by its
    /// parents.
    fn check_files(&self, refs: &mut Refs) {
        let Some(root) = refs.asset_root() else { return };
        for source in self.sources() {
            if source.is_empty() || !is_gltf(source) {
                continue;
            }
            let field = if source == self.source { "source" } else { "a clip's source" };
            let file = match ClipFile::read(&root.join(source)) {
                Ok(file) => file,
                Err(e) => {
                    refs.error(format!("{field} `{source}` {e}; check the path under the assets folder"));
                    continue;
                }
            };
            let have = file.animation_names();
            let have = if have.is_empty() { "none".to_owned() } else { have.join(", ") };
            for name in self.clips.keys().filter(|c| self.source_of(c) == source) {
                let wanted = self.file_name(name);
                if !file.has_animation(wanted) {
                    refs.error(format!("clips.{name} -> `{source}` has no animation `{wanted}`; it has: {have}"));
                } else {
                    check_phases_in_file(refs, name, source, wanted, &file, &self.clips[name].phases);
                }
            }
            if source != self.source {
                continue;
            }
            // A cue's bone is found under the model, whichever file its clip comes from.
            for (name, clip) in &self.clips {
                for (i, cue) in clip.cues.iter().enumerate() {
                    if let Some(bone) = cue.bone.as_deref().filter(|b| !b.is_empty())
                        && !file.has_node(bone)
                    {
                        refs.error(format!(
                            "clips.{name}.cues[{i}].bone -> `{source}` has no node `{bone}`; name a bone of the model"
                        ));
                    }
                }
            }
            if let Some(bone) = self.root_motion.as_deref().filter(|b| !b.is_empty()) {
                if !file.has_node(bone) {
                    refs.error(format!("root_motion -> `{source}` has no node `{bone}`; name its root bone"));
                } else if let Some(parent) = file.turned_parent(bone) {
                    refs.error(format!(
                        "root_motion `{bone}` sits under node `{parent}` of `{source}`, which turns it or scales it unevenly: \
                         its moves are not in the model's axes; export the armature unturned"
                    ));
                }
            }
            for (name, socket) in &self.sockets {
                if !socket.bone.is_empty() && !file.has_node(&socket.bone) {
                    refs.error(format!(
                        "sockets.{name}.bone -> `{source}` has no node `{}`; name a bone of the model",
                        socket.bone
                    ));
                }
            }
            for (name, layer) in &self.layers {
                if !layer.root.is_empty() && !file.has_node(&layer.root) {
                    refs.error(format!(
                        "layers.{name}.root -> `{source}` has no node `{}`; name the bone the layer starts at",
                        layer.root
                    ));
                }
            }
        }
    }

    /// `field.turn` of an action's mapping: a number of seconds, on the whole body, with a bone to
    /// write it on.
    fn check_turn(&self, refs: &mut Refs, field: &str, mapping: &ActionClip) {
        let turn = mapping.turn;
        if !(turn >= 0.0 && turn.is_finite()) {
            refs.error(format!("{field}.turn must be >= 0 seconds (is {turn})"));
            return;
        }
        if turn == 0.0 {
            return;
        }
        if let Some(layer) = &mapping.layer {
            refs.error(format!(
                "{field}.turn turns the whole model, and the clip plays on layer `{layer}`; leave `turn` out or play \
                 the clip on the whole body"
            ));
        }
        if self.root_motion.is_none() {
            refs.error(format!(
                "{field}.turn needs `root_motion`: the turn is written on that bone; name the root bone, e.g. \
                 `root_motion: \"root\"`"
            ));
        }
    }

    /// `field.blend_out` of an action's mapping: a number of seconds, for a clip on a layer.
    fn check_blend_out(refs: &mut Refs, field: &str, mapping: &ActionClip) {
        let Some(out) = mapping.blend_out else { return };
        if !(out >= 0.0 && out.is_finite()) {
            refs.error(format!("{field}.blend_out must be >= 0 seconds (is {out})"));
        }
        if mapping.layer.is_none() {
            refs.error(format!(
                "{field}.blend_out is how a clip leaves its layer, and the clip plays on the whole body (the blend of \
                 what the body shows next takes it over); leave `blend_out` out or give the mapping a `layer`"
            ));
        }
    }

    /// `overlays`: known events, clips and layers, an envelope that ends, a strength that shows.
    fn check_overlays(&self, refs: &mut Refs, names: &[&str]) {
        let events: Vec<&str> = OVERLAY_EVENTS.iter().map(|(event, _)| *event).collect();
        for (event, overlay) in &self.overlays {
            let field = format!("overlays.{event}");
            if !events.contains(&event.as_str()) {
                let hint =
                    shards_content::db::closest(event, events.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
                refs.error(format!("overlays has an unknown event `{event}`{hint}; events: {}", events.join(", ")));
            }
            if !self.clips.contains_key(&overlay.clip) {
                unknown_clip(refs, names, &field, &overlay.clip);
            }
            self.check_layer_name(refs, &field, &overlay.layer);
            let fade_in = overlay.fade_in;
            if !(fade_in >= 0.0 && fade_in.is_finite()) {
                refs.error(format!("{field}.fade_in must be >= 0 seconds (is {fade_in})"));
            }
            let (from, to) = overlay.fade_out;
            if !(from >= 0.0 && to > 0.0 && from <= to && to.is_finite()) {
                refs.error(format!(
                    "{field}.fade_out must be (start, end) in seconds after the event, 0 <= start <= end and end > 0 \
                     (is ({from}, {to})); the overlay fades out between them and ends at `end`"
                ));
            }
            let strength = overlay.strength;
            if !(strength > 0.0 && strength <= 1.0) {
                refs.error(format!("{field}.strength must be above 0 and at most 1 (is {strength})"));
            }
        }
    }

    /// `field.layer` of an action's mapping names one of the set's layers.
    fn check_layer(&self, refs: &mut Refs, field: &str, mapping: &ActionClip) {
        let Some(layer) = mapping.layer.as_deref() else { return };
        self.check_layer_name(refs, field, layer);
    }

    /// `field.layer` names one of the set's layers.
    fn check_layer_name(&self, refs: &mut Refs, field: &str, layer: &str) {
        if self.layers.contains_key(layer) {
            return;
        }
        let names: Vec<&str> = self.layers.keys().map(String::as_str).collect();
        if names.is_empty() {
            refs.error(format!(
                "{field}.layer -> unknown layer `{layer}`; the set has no layers: name one in `layers`, \
                 e.g. `layers: {{\"upper\": (root: \"spine\")}}`"
            ));
            return;
        }
        let hint = shards_content::db::closest(layer, names.iter().copied())
            .map(|c| format!(" (did you mean `{c}`?)"))
            .unwrap_or_default();
        refs.error(format!("{field}.layer -> unknown layer `{layer}`{hint}; layers: {}", names.join(", ")));
    }
}

/// `clips.NAME.phases`: each named, once, lasting >= 0 s, and some length in all; `loops` and
/// `holds` name some of them, never the same one.
fn check_clip_phases(refs: &mut Refs, name: &str, clip: &ClipDef) {
    let phases = &clip.phases;
    let names: Vec<&str> = phases.iter().map(|p| p.0.as_str()).collect();
    for (field, open) in [("loops", &clip.loops), ("holds", &clip.holds)] {
        for phase in open {
            if names.is_empty() {
                refs.error(format!(
                    "clips.{name}.{field} names `{phase}`, but the clip has no `phases`; give the phases it was timed to, \
                     e.g. `phases: [(\"{phase}\", 1.0)]`"
                ));
                break;
            }
            if !names.contains(&phase.as_str()) {
                let hint = shards_content::db::closest(phase, names.iter().copied())
                    .map(|c| format!(" (did you mean `{c}`?)"))
                    .unwrap_or_default();
                refs.error(format!("clips.{name}.{field} names an unknown phase `{phase}`{hint}; phases: {}", names.join(", ")));
            }
        }
    }
    for both in clip.holds.iter().filter(|h| clip.loops.contains(h)) {
        refs.error(format!(
            "clips.{name}: phase `{both}` is in both `loops` and `holds`; a phase repeats its stretch or holds its last pose, \
             not both"
        ));
    }
    if phases.is_empty() {
        return;
    }
    let mut seen: Vec<&str> = Vec::new();
    for Phase(phase, secs) in phases {
        if phase.is_empty() {
            refs.error(format!("clips.{name}.phases has a phase with no name"));
        } else if seen.contains(&phase.as_str()) {
            refs.error(format!("clips.{name}.phases has `{phase}` twice; an action's phase finds the clip's by its name"));
        }
        seen.push(phase);
        if !(*secs >= 0.0 && secs.is_finite()) {
            refs.error(format!("clips.{name}.phases: `{phase}` must last >= 0 s (is {secs})"));
        }
    }
    let valid = phases.iter().all(|p| p.1 >= 0.0 && p.1.is_finite());
    if valid && phases.iter().map(|p| p.1).sum::<f32>() <= EPS {
        refs.error(format!("clips.{name}.phases last 0 s in all; give the seconds the clip was timed to"));
    }
}

/// `clips.NAME.phases` against animation `wanted` of `file` (shown as `source`): the phases its
/// `phases` extra names, to half a tick of [`SAMPLE_HZ`](crate::bake::SAMPLE_HZ) (as a bake reads
/// them), or without one its length.
fn check_phases_in_file(refs: &mut Refs, name: &str, source: &str, wanted: &str, file: &ClipFile, phases: &[Phase]) {
    if phases.is_empty() {
        return;
    }
    let half_tick = 0.5 / crate::bake::SAMPLE_HZ;
    match file.phases(wanted) {
        Some(Err(e)) => refs.error(format!("clips.{name}.phases: animation `{wanted}` of `{source}`: {e}")),
        Some(Ok(theirs)) => {
            let same = theirs.len() == phases.len()
                && theirs.iter().zip(phases).all(|(f, p)| f.0 == p.0 && (f.1 - p.1).abs() <= half_tick);
            if !same {
                let theirs: Vec<Phase> = theirs.into_iter().map(|(n, s)| Phase(n, s)).collect();
                refs.error(format!(
                    "clips.{name}.phases ({}) are not the phases animation `{wanted}` of `{source}` was timed to ({}); \
                     write the file's",
                    phases_text(phases),
                    phases_text(&theirs)
                ));
            }
        }
        None => {
            let total: f32 = phases.iter().map(|p| p.1).sum();
            if let Some(lasts) = file.duration(wanted)
                && (lasts - total).abs() > half_tick
            {
                refs.error(format!(
                    "clips.{name}.phases last {} s in all, animation `{wanted}` of `{source}` {} s; make them add up \
                     to the clip's length",
                    crate::bake::number(total, 4),
                    crate::bake::number(lasts, 4)
                ));
            }
        }
    }
}

impl AnimSet {
    /// Every action the set shows on a clip timed to phases (its clip, and that clip's family in each
    /// `by_moveset`; for a moveset's stagger and death actions without a clip of their own, the
    /// `stagger` and `dead` clips) has the same phases in the same order, or none of their names (the
    /// clip plays evenly then). One that shares some of the names has a typo or a phase left out.
    fn check_timing(&self, refs: &mut Refs) {
        let actions: Vec<(&str, &ActionDef)> = refs.iter::<ActionDef>().collect();
        // Each clip an action shows, once: its own mapping and the families of it, then for a
        // moveset's stagger and death actions that map no clip of their own, the set's `stagger` and
        // `dead` clips (in that moveset's family), which are seeked by those actions' phases too.
        let mut shown: Vec<(&str, &str, &ActionDef)> = Vec::new();
        for &(id, def) in &actions {
            let key = self.action_key(id, def.anim.as_deref(), &def.tags);
            if def.face == Face::Run && self.turn_of(key) > 0.0 {
                let field = if self.actions.contains_key(key) { "actions" } else { "tags" };
                refs.error(format!(
                    "{field}.{key}.turn turns the model to the heading of a run that keeps the body's facing, and \
                     action `{id}` turns the body itself (`face: Run`); give it `face: Keep` or leave `turn` out"
                ));
            }
            let Some((clip, _)) = self.action_shown(key) else { continue };
            self.show_timed(&mut shown, clip, id, def);
            for renamed in self.by_moveset.values().filter_map(|table| table.get(clip)) {
                self.show_timed(&mut shown, renamed, id, def);
            }
        }
        for (moveset, m) in refs.iter::<MovesetDef>() {
            for (action, state) in [(&m.stagger, Locomotion::Stagger), (&m.death, Locomotion::Dead)] {
                let (Some(action), Some(clip)) = (action.as_deref(), self.exact(state)) else { continue };
                let Some(&(id, def)) = actions.iter().find(|(id, _)| *id == action) else { continue };
                if self.action_shown(self.action_key(id, def.anim.as_deref(), &def.tags)).is_none() {
                    self.show_timed(&mut shown, self.family(Some(moveset), clip), id, def);
                }
            }
        }
        for (clip, id, def) in shown {
            let timed = &self.clips[clip].phases;
            let shared: Vec<&str> = timed.iter().map(|c| c.0.as_str()).filter(|n| def.phases.iter().any(|a| a.0 == *n)).collect();
            if shared.is_empty() || timed_alike(&def.phases, timed) {
                continue;
            }
            refs.error(format!(
                "clips.{clip}.phases ({}) and action `{id}`, which plays it ({}), share {} but are not the same phases \
                 in the same order; give the clip the action's phase names (or none of them, and it plays evenly)",
                phases_text(timed),
                phases_text(&def.phases),
                shared.join(", ")
            ));
        }
    }

    /// `clip` (when the set has it) shown by action `id`, added to `shown` once.
    fn show_timed<'s>(&self, shown: &mut Vec<(&'s str, &'s str, &'s ActionDef)>, clip: &'s str, id: &'s str, def: &'s ActionDef) {
        if let Some(clip) = self.known(clip)
            && !shown.iter().any(|(c, i, _)| *c == clip && *i == id)
        {
            shown.push((clip, id, def));
        }
    }
}

/// ` (did you mean `water`?)` for a misspelt motion mode, or nothing.
fn mode_hint(mode: &str) -> String {
    shards_content::db::closest(mode, MODES.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default()
}

/// `field -> unknown clip `x` (did you mean `y`?); clips: a, b`.
fn unknown_clip(refs: &mut Refs, names: &[&str], field: &str, clip: &str) {
    let hint = shards_content::db::closest(clip, names.iter().copied())
        .map(|c| format!(" (did you mean `{c}`?)"))
        .unwrap_or_default();
    refs.error(format!("{field} -> unknown clip `{clip}`{hint}; clips: {}", names.join(", ")));
}

/// The content check of an action's `anim`: in a game with animsets, one of them plays it (a
/// clip of that name, or one its `actions` or `tags` give the action). A game with none shows
/// its actions another way, and `anim` is then only a name for it.
pub(crate) fn check_action_clip(id: &str, anim: &str, tags: &[String], refs: &mut Refs) {
    let sets: Vec<&AnimSet> = refs.iter::<AnimSet>().map(|(_, set)| set).collect();
    if sets.is_empty() || sets.iter().any(|set| set.action_clip(set.action_key(id, Some(anim), tags)).is_some()) {
        return;
    }
    let mut clips: Vec<&str> = sets.iter().flat_map(|set| set.clips.keys().map(String::as_str)).collect();
    clips.sort_unstable();
    clips.dedup();
    let hint = shards_content::db::closest(anim, clips.iter().copied())
        .map(|c| format!(" (did you mean `{c}`?)"))
        .unwrap_or_default();
    let message = format!(
        "anim `{anim}` is no clip of any animset{hint}; clips: {}; name one of them, or leave `anim` out and the \
         character keeps its locomotion clip meanwhile",
        clips.join(", ")
    );
    refs.error(message);
}

impl ContentDef for AnimSet {
    const KIND: &'static str = "animset";
    // Only what a machine shows or plays, or what lies outside a session: never in the replay check.
    const SIMULATION: bool = false;

    fn validate(&self, _id: &str, refs: &mut Refs) {
        if self.source.is_empty() {
            refs.error("source is empty; name the glTF with the clips, e.g. `source: \"models/hero.glb\"`");
        } else if !is_gltf(&self.source) {
            refs.error(format!("source `{}` is not a glTF file (.gltf or .glb)", self.source));
        }
        if self.clips.is_empty() {
            refs.error("clips is empty; list the animations to use, e.g. `clips: {\"idle\": ()}`");
        }
        let names: Vec<&str> = self.clips.keys().map(String::as_str).collect();
        let known = |clip: &str| self.clips.contains_key(clip);
        for (name, clip) in &self.clips {
            if name.is_empty() {
                refs.error("a clip has no name");
            }
            if clip.name.as_deref().is_some_and(str::is_empty) {
                refs.error(format!("clips.{name}.name is empty; leave it out to use `{name}`"));
            }
            if let Some(source) = &clip.source
                && !is_gltf(source)
            {
                refs.error(format!("clips.{name}.source `{source}` is not a glTF file (.gltf or .glb)"));
            }
            if !(clip.speed > 0.0 && clip.speed.is_finite()) {
                refs.error(format!("clips.{name}.speed must be > 0 (is {})", clip.speed));
            }
            if let Some(b) = clip.blend
                && !(b >= 0.0 && b.is_finite())
            {
                refs.error(format!("clips.{name}.blend must be >= 0 (is {b})"));
            }
            for (i, cue) in clip.cues.iter().enumerate() {
                if !(cue.at >= 0.0 && cue.at.is_finite()) {
                    refs.error(format!("clips.{name}.cues[{i}].at must be >= 0 (is {})", cue.at));
                }
                if cue.cue.is_empty() {
                    refs.error(format!("clips.{name}.cues[{i}].cue has no name"));
                }
                if cue.bone.as_deref().is_some_and(str::is_empty) {
                    refs.error(format!(
                        "clips.{name}.cues[{i}].bone is empty; name a bone of the model (`bone: \"foot.L\"`) or leave it out"
                    ));
                }
                if !(cue.strength >= 0.0 && cue.strength.is_finite()) {
                    refs.error(format!("clips.{name}.cues[{i}].strength must be a number, 0 or more (is {})", cue.strength));
                }
            }
            check_clip_phases(refs, name, clip);
        }
        let states: Vec<&str> = Locomotion::ALL.iter().map(|s| s.name()).collect();
        for (state, clip) in &self.locomotion {
            if Locomotion::parse(state).is_none() && !FACING_KEYS.contains(&state.as_str()) {
                let hint = shards_content::db::closest(state, states.iter().chain(FACING_KEYS.iter()).copied())
                    .map(|c| format!(" (did you mean `{c}`?)"))
                    .unwrap_or_default();
                refs.error(format!(
                    "locomotion has an unknown state `{state}`{hint}; states: {}; facing a target: a gait's heading \
                     (`walk.back`, `run.left`, `sprint.right`) or `idle.facing`",
                    states.join(", ")
                ));
            }
            if !known(clip) {
                unknown_clip(refs, &names, &format!("locomotion.{state}"), clip);
            }
        }
        if self.layers.len() > MAX_LAYERS {
            refs.error(format!("layers has {} layers; at most {MAX_LAYERS}", self.layers.len()));
        }
        for (name, layer) in &self.layers {
            if name.is_empty() {
                refs.error("a layer has no name");
            }
            if layer.root.is_empty() {
                refs.error(format!(
                    "layers.{name}.root is empty; name the bone the layer starts at, e.g. `layers: {{\"{name}\": (root: \"spine\")}}`"
                ));
            }
            for mode in &layer.off_in {
                if parse_mode(mode).is_none() {
                    refs.error(format!("layers.{name}.off_in names an unknown motion mode `{mode}`{}; modes: {}", mode_hint(mode), MODES.join(", ")));
                }
            }
        }
        for (action, mapping) in &self.actions {
            refs.require::<ActionDef>(action, "actions");
            if !known(&mapping.clip) {
                unknown_clip(refs, &names, &format!("actions.{action}"), &mapping.clip);
            }
            self.check_layer(refs, &format!("actions.{action}"), mapping);
            self.check_turn(refs, &format!("actions.{action}"), mapping);
            Self::check_blend_out(refs, &format!("actions.{action}"), mapping);
        }
        for (tag, mapping) in &self.tags {
            if tag.is_empty() {
                refs.error("tags has an empty tag");
            }
            if !known(&mapping.clip) {
                unknown_clip(refs, &names, &format!("tags.{tag}"), &mapping.clip);
            }
            self.check_layer(refs, &format!("tags.{tag}"), mapping);
            self.check_turn(refs, &format!("tags.{tag}"), mapping);
            Self::check_blend_out(refs, &format!("tags.{tag}"), mapping);
        }
        self.check_overlays(refs, &names);
        self.check_timing(refs);
        for (field, v) in [("blend", self.blend), ("action_blend", self.action_blend)] {
            if !(v >= 0.0 && v.is_finite()) {
                refs.error(format!("{field} must be >= 0 (is {v})"));
            }
        }
        if self.root_motion.as_deref().is_some_and(str::is_empty) {
            refs.error("root_motion is empty; name the root bone (`root_motion: \"root\"`) or leave it out");
        }
        self.check_mounting(refs);
        self.check_files(refs);
        let s = &self.speeds;
        for (field, v) in [("walk", s.walk), ("run", s.run), ("sprint", s.sprint)] {
            if !(v > 0.0 && v.is_finite()) {
                refs.error(format!("speeds.{field} must be > 0 (is {v})"));
            }
        }
        if !(s.idle_below >= 0.0 && s.idle_below.is_finite()) {
            refs.error(format!("speeds.idle_below must be >= 0 (is {})", s.idle_below));
        }
        if !(s.min_scale > 0.0 && s.max_scale >= s.min_scale && s.max_scale.is_finite()) {
            refs.error(format!(
                "speeds.min_scale and max_scale must satisfy 0 < min_scale <= max_scale (are {} and {})",
                s.min_scale, s.max_scale
            ));
        }
    }
}

impl AnimSet {
    /// `by_moveset`, `sockets`, `carry` and `mounts`: what they name exists, and the numbers make
    /// sense.
    fn check_mounting(&self, refs: &mut Refs) {
        for (mode, drop) in &self.drops {
            if parse_mode(mode).is_none() {
                refs.error(format!("drops has an unknown motion mode `{mode}`{}; modes: {}", mode_hint(mode), MODES.join(", ")));
            }
            for (field, v) in [("down", drop.down), ("seconds", drop.seconds)] {
                if !(v > 0.0 && v.is_finite()) {
                    refs.error(format!("drops.{mode}.{field} must be > 0 (is {v})"));
                }
            }
        }
        if !self.drops.is_empty() && self.root_motion.is_none() {
            refs.error(
                "`drops` needs `root_motion`: the drop is written on that bone; name the root bone, e.g. `root_motion: \"root\"`",
            );
        }
        let clip_names: Vec<&str> = self.clips.keys().map(String::as_str).collect();
        for (moveset, table) in &self.by_moveset {
            refs.require::<MovesetDef>(moveset, "by_moveset");
            for (from, to) in table {
                if !self.clips.contains_key(from) {
                    unknown_clip(refs, &clip_names, &format!("by_moveset.{moveset}"), from);
                }
                if !self.clips.contains_key(to) {
                    unknown_clip(refs, &clip_names, &format!("by_moveset.{moveset}.{from}"), to);
                }
            }
        }
        for (name, socket) in &self.sockets {
            if socket.bone.is_empty() {
                refs.error(format!(
                    "sockets.{name}.bone is empty; name the bone the socket rides, e.g. `sockets: {{\"{name}\": (bone: \"hand.R\")}}`"
                ));
            }
            for (field, v) in [("offset", socket.offset), ("turn", socket.turn)] {
                if !v.is_finite() {
                    refs.error(format!("sockets.{name}.{field} must be finite (is {v})"));
                }
            }
        }
        let socket_names: Vec<&str> = self.sockets.keys().map(String::as_str).collect();
        let mut carried_props: BTreeSet<&str> = BTreeSet::new();
        for (moveset, list) in &self.carry {
            if moveset != "*" {
                refs.require::<MovesetDef>(moveset, "carry");
            }
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for (i, carry) in list.iter().enumerate() {
                let field = format!("carry.{moveset}[{i}]");
                refs.require::<PropDef>(&carry.prop, &format!("{field}.prop"));
                self.check_socket(refs, &socket_names, &format!("{field}.socket"), &carry.socket);
                if !seen.insert(&carry.prop) {
                    refs.error(format!(
                        "{field}.prop `{}` is carried twice by `{moveset}`; a prop sits in one socket",
                        carry.prop
                    ));
                }
                carried_props.insert(&carry.prop);
            }
        }
        for (key, rules) in &self.mounts {
            self.check_rules(refs, key, rules, &socket_names, &carried_props);
        }
    }

    /// `field -> unknown socket `x` (did you mean `y`?); sockets: a, b`.
    fn check_socket(&self, refs: &mut Refs, names: &[&str], field: &str, socket: &str) {
        if self.sockets.contains_key(socket) {
            return;
        }
        if names.is_empty() {
            refs.error(format!(
                "{field} -> unknown socket `{socket}`; the set has no sockets: name one in `sockets`, \
                 e.g. `sockets: {{\"{socket}\": (bone: \"hand.R\")}}`"
            ));
            return;
        }
        let hint = shards_content::db::closest(socket, names.iter().copied())
            .map(|c| format!(" (did you mean `{c}`?)"))
            .unwrap_or_default();
        refs.error(format!("{field} -> unknown socket `{socket}`{hint}; sockets: {}", names.join(", ")));
    }

    fn check_rules(&self, refs: &mut Refs, key: &str, rules: &MountRules, sockets: &[&str], carried: &BTreeSet<&str>) {
        let equip = key == EQUIP_KEY;
        let (in_mode, in_left) = (key.starts_with(MODE_KEY), key.starts_with(LEFT_KEY));
        let special = key.starts_with('@');
        if special && !(equip || in_mode || in_left) {
            refs.error(format!(
                "mounts.{key}: the only rules keys that start with `@` are `{EQUIP_KEY}`, `{MODE_KEY}<mode>` and \
                 `{LEFT_KEY}<mode>`; name an action, an `anim` or a tag"
            ));
            return;
        }
        if in_mode || in_left {
            let mode = &key[if in_mode { MODE_KEY.len() } else { LEFT_KEY.len() }..];
            if parse_mode(mode).is_none() {
                refs.error(format!("mounts.{key} names an unknown motion mode `{mode}`{}; modes: {}", mode_hint(mode), MODES.join(", ")));
            }
        }
        // The actions this key looks up: by id, by `anim` or by tag.
        let actions: Vec<&ActionDef> = refs
            .iter::<ActionDef>()
            .filter(|(id, def)| *id == key || def.anim.as_deref() == Some(key) || def.tags.iter().any(|t| t == key))
            .map(|(_, def)| def)
            .collect();
        if !special && actions.is_empty() && refs.iter::<ActionDef>().next().is_some() {
            refs.error(format!(
                "mounts.{key} names no action, `anim` or tag; the rules are looked up by an action's id, its `anim` or \
                 one of its tags"
            ));
        }
        let check_span = |refs: &mut Refs, field: &str, span: &Span| {
            let bad = |v: f32| !(v >= 0.0 && v.is_finite());
            let lacks = |p: &str| !actions.is_empty() && !actions.iter().any(|a| a.phase_span(p).is_some());
            match span {
                Span::Secs(a, b) => {
                    if bad(*a) || bad(*b) || b < a {
                        refs.error(format!("{field} `Secs({a}, {b})` must satisfy 0 <= from <= to"));
                    }
                }
                _ if special => {
                    let from = if equip {
                        "the weapon switch"
                    } else if in_mode {
                        "the moment the rules begin to apply"
                    } else {
                        "the moment the wait ends"
                    };
                    refs.error(format!("{field}: under `{key}` a span is `Secs(from, to)`, counted from {from} (it has no phases)"));
                }
                Span::Phase(p) if lacks(p) => {
                    refs.error(format!("{field} names phase `{p}`, which no action looked up by `{key}` has"));
                }
                Span::Phases(a, b) => {
                    for p in [a, b] {
                        if lacks(p) {
                            refs.error(format!("{field} names phase `{p}`, which no action looked up by `{key}` has"));
                        }
                    }
                }
                Span::Phase(_) => {}
            }
        };
        for (i, mv) in rules.moves.iter().enumerate() {
            let field = format!("mounts.{key}.moves[{i}]");
            if !carried.contains(mv.prop.as_str()) {
                refs.error(format!(
                    "{field}.prop `{}` is never carried (no `carry` entry has it); the move does nothing",
                    mv.prop
                ));
            }
            if let Some(to) = &mv.to {
                self.check_socket(refs, sockets, &format!("{field}.to"), to);
            }
            check_span(refs, &format!("{field}.span"), &mv.span);
        }
        if let Some(span) = &rules.previous {
            check_span(refs, &format!("mounts.{key}.previous"), span);
        }
        if !rules.ended_by.is_empty() && !equip {
            refs.error(format!(
                "mounts.{key}.ended_by belongs under `{EQUIP_KEY}`: only a weapon switch's equip ends by an event"
            ));
        }
        for event in &rules.ended_by {
            if equip && !EQUIP_EVENTS.contains(&event.as_str()) {
                refs.error(format!(
                    "mounts.{key}.ended_by has an unknown event `{event}`; events: {}",
                    EQUIP_EVENTS.join(", ")
                ));
            }
        }
        if !rules.on_cut.is_empty() && special {
            refs.error(format!("mounts.{key}.on_cut belongs under an action's key: it is what a cut run holds"));
        }
        if rules.previous.is_some() && (in_mode || in_left) {
            refs.error(format!(
                "mounts.{key}.previous belongs under an action's key or `{EQUIP_KEY}`: a mode's rules show the props the body carries"
            ));
        }
        if rules.yield_after != 0.0 && !in_mode {
            refs.error(format!(
                "mounts.{key}.yield_after belongs under `{MODE_KEY}<mode>`: it is how long the rules step aside after an action in the mode"
            ));
        }
        if rules.wait != 0.0 && !in_left {
            refs.error(format!(
                "mounts.{key}.wait belongs under `{LEFT_KEY}<mode>`: it is how long the moves wait after the body left the mode"
            ));
        }
        for (field, v) in [("yield_after", rules.yield_after), ("wait", rules.wait)] {
            if !(v >= 0.0 && v.is_finite()) {
                refs.error(format!("mounts.{key}.{field} must be >= 0 (is {v})"));
            }
        }
        for (prop, secs) in &rules.on_cut {
            if !carried.contains(prop.as_str()) {
                refs.error(format!(
                    "mounts.{key}.on_cut.{prop} names a prop that is never carried (no `carry` entry has it)"
                ));
            }
            if !(*secs >= 0.0 && secs.is_finite()) {
                refs.error(format!("mounts.{key}.on_cut.{prop} must be >= 0 (is {secs})"));
            }
        }
    }
}

impl ContentDef for PropDef {
    const KIND: &'static str = "prop";
    // A picture: never in the replay check.
    const SIMULATION: bool = false;

    fn validate(&self, _id: &str, refs: &mut Refs) {
        match (&self.model, &self.size) {
            (None, None) => {
                refs.error("a prop needs a `model` (a glTF) or a `size` (a box), e.g. `size: (0.05, 0.9, 0.02)`");
            }
            (Some(_), Some(_)) => refs.error("a prop has a `model` or a `size`, not both; leave one out"),
            _ => {}
        }
        if let Some(model) = &self.model {
            if std::path::Path::new(model).components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
                refs.error(format!("model `{model}` must be a path inside the assets folder, without `..` or a leading `/`"));
            } else if !is_gltf(model) {
                refs.error(format!("model `{model}` is not a glTF file (.gltf or .glb)"));
            } else if let Some(root) = refs.asset_root()
                && !root.join(model).is_file()
            {
                refs.error(format!("model `{model}` does not exist under the assets folder; check the path"));
            }
        }
        if let Some(size) = self.size
            && !(size.x > 0.0 && size.y > 0.0 && size.z > 0.0 && size.is_finite())
        {
            refs.error(format!("size must be positive on every axis (is {size})"));
        }
        if !(self.scale > 0.0 && self.scale.is_finite()) {
            refs.error(format!("scale must be > 0 (is {})", self.scale));
        }
        for (field, v) in [("grip", self.grip), ("turn", self.turn)] {
            if !v.is_finite() {
                refs.error(format!("{field} must be finite (is {v})"));
            }
        }
        if let Some(look) = &self.look {
            refs.require::<shards_look::LookDef>(look, "look");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Running;
    use shards_content::{ContentDb, ContentKinds};

    fn set() -> AnimSet {
        ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
            .from_str(
                r#"(
                source: "models/humanoid.glb",
                clips: {
                    "idle": (), "walk": (), "run": (speed: 1.1), "fall": (name: "Armature|Falling"),
                    "swim": (), "climb": (), "hit": (), "die": (),
                    "swing_1": (name: "Armature|Slash1", cues: [(at: 0.4, cue: "swoosh"), (at: 0.1, cue: "draw")]),
                    "roll": (blend: 0.04), "block": (), "generic_attack": (),
                },
                locomotion: {"idle": "idle", "walk": "walk", "run": "run", "air": "fall", "swim": "swim",
                             "mantle": "climb", "stagger": "hit", "dead": "die"},
                actions: {"light_1": "swing_1", "roll": "roll"},
                tags: {"attack": "generic_attack"},
            )"#,
            )
            .unwrap_or_else(|e| panic!("{e}"))
    }

    fn moving(v: f32) -> MotionState {
        MotionState { velocity: Vec3::new(0.0, 0.0, -v), grounded: true, ..default() }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn standing_still_is_idle() {
        let set = set();
        let c = select_clip(None, &moving(0.0), &set);
        assert_eq!(c.clip, Some("idle"));
        assert_eq!(c.seek_to, None);
        assert!(c.looping && close(c.speed, 1.0) && close(c.blend, 0.2), "{c:?}");
        assert_eq!(select_clip(None, &moving(0.05), &set).clip, Some("idle"), "below idle_below");
    }

    #[test]
    fn ground_speed_picks_walk_or_run_and_scales_the_clip() {
        let set = set();
        let walk = select_clip(None, &moving(1.0), &set);
        assert_eq!(walk.clip, Some("walk"));
        assert!(close(walk.speed, 0.6), "1 m/s on a 2 m/s clip is clamped to min_scale: {}", walk.speed);
        let walk = select_clip(None, &moving(2.0), &set);
        assert!(close(walk.speed, 1.0), "{}", walk.speed);
        let jog = select_clip(None, &moving(3.4), &set);
        assert_eq!(jog.clip, Some("walk"), "below the midpoint 3.5 m/s it is still a walk");
        assert!(close(jog.speed, 1.6), "clamped to max_scale: {}", jog.speed);
        let run = select_clip(None, &moving(5.0), &set);
        assert_eq!(run.clip, Some("run"));
        assert!(close(run.speed, 1.1), "the clip's own speed 1.1 times 5/5: {}", run.speed);
        let fast = select_clip(None, &moving(6.0), &set);
        assert!(close(fast.speed, 1.2 * 1.1), "{}", fast.speed);
    }

    #[test]
    fn sprinting_falls_back_to_run_without_a_sprint_clip() {
        let mut set = set();
        let sprinting = MotionState { sprinting: true, ..moving(8.0) };
        let c = select_clip(None, &sprinting, &set);
        assert_eq!(c.clip, Some("run"), "no sprint clip: the run clip, sped up");
        assert!(close(c.speed, 1.6 * 1.1), "authored for sprint speed 8 m/s -> run clip scaled by 8/5, capped: {c:?}");
        set.clips.insert("sprint".into(), ClipDef::default());
        set.locomotion.insert("sprint".into(), "sprint".into());
        let c = select_clip(None, &sprinting, &set);
        assert_eq!(c.clip, Some("sprint"));
        assert!(close(c.speed, 1.0), "{c:?}");
        // The sprint flag at walking pace is still a walk.
        let slow = MotionState { sprinting: true, ..moving(1.5) };
        assert_eq!(select_clip(None, &slow, &set).clip, Some("walk"));
    }

    #[test]
    fn other_modes_have_their_own_clips() {
        let set = set();
        let mode = |mode| MotionState { mode, velocity: Vec3::new(1.0, -3.0, 0.0), ..default() };
        assert_eq!(select_clip(None, &mode(MotionMode::Air), &set).clip, Some("fall"));
        assert_eq!(select_clip(None, &mode(MotionMode::Water), &set).clip, Some("swim"));
        // No glide clip: the air clip stands in.
        assert_eq!(select_clip(None, &mode(MotionMode::Glide), &set).clip, Some("fall"));
    }

    #[test]
    fn a_ledge_climb_is_seeked_to_its_progress() {
        let set = set();
        let mantle = shards_motion::Mantle { from: Vec3::ZERO, corner: Vec3::Y, to: Vec3::ONE, t: 0.4, duration: 0.8 };
        let motion = MotionState { mode: MotionMode::Mantle, mantle: Some(mantle), ..default() };
        let c = select_clip(None, &motion, &set);
        assert_eq!(c.clip, Some("climb"));
        assert_eq!(c.seek_to, Some(0.4));
        assert!(close(c.speed, 0.0) && !c.looping, "{c:?}");
    }

    #[test]
    fn a_mapped_action_is_seeked_to_gameplay_time() {
        let set = set();
        let c = select_clip(Some(("light_1", 0.15, 0.6)), &moving(5.0), &set);
        assert_eq!(c.clip, Some("swing_1"), "the action wins over locomotion");
        assert!(close(c.seek_to.unwrap(), 0.25), "{c:?}");
        assert!(close(c.speed, 0.0) && !c.looping, "gameplay moves it, the player does not");
        assert!(close(c.blend, 0.06), "action blend: {}", c.blend);
        let end = select_clip(Some(("light_1", 0.7, 0.6)), &moving(0.0), &set);
        assert_eq!(end.seek_to, Some(1.0), "past the end it holds the last pose");
        let roll = select_clip(Some(("roll", 0.0, 0.4)), &moving(0.0), &set);
        assert!(close(roll.blend, 0.04), "the clip's own blend: {}", roll.blend);
        assert_eq!(select_clip(Some(("roll", 0.1, 0.0)), &moving(0.0), &set).seek_to, Some(1.0), "zero length");
    }

    #[test]
    fn an_unmapped_action_leaves_locomotion_to_play() {
        let set = set();
        let c = select_clip(Some(("mystery", 0.1, 0.5)), &moving(2.0), &set);
        assert_eq!(c.clip, Some("walk"));
        assert_eq!(c.seek_to, None);
    }

    #[test]
    fn actions_are_found_by_id_clip_name_or_tag() {
        let set = set();
        assert_eq!(set.action_clip("light_1"), Some("swing_1"), "by id");
        assert_eq!(set.action_clip("swing_1"), Some("swing_1"), "by the clip's own name (an action's `anim`)");
        assert_eq!(set.action_clip("attack"), Some("generic_attack"), "by tag");
        assert_eq!(set.action_clip("nothing"), None);
        let tags = ["fire".to_owned(), "attack".to_owned()];
        assert_eq!(set.action_key("heavy", None, &tags), "attack", "the first tag the set maps");
        assert_eq!(set.action_key("heavy", Some("block"), &tags), "block", "the action's anim beats its tags");
        assert_eq!(set.action_key("light_1", Some("block"), &tags), "light_1", "the id beats everything");
        assert_eq!(set.action_key("heavy", Some("not_a_clip"), &[]), "heavy", "nothing matches: the id");
        let c = select_clip(Some((set.action_key("heavy", None, &tags), 0.1, 0.5)), &moving(0.0), &set);
        assert_eq!(c.clip, Some("generic_attack"));
    }

    #[test]
    fn dead_bodies_show_the_dead_clip() {
        let set = set();
        let dead = BodyStatus { dead: true, ..default() };
        // While the death action runs (unmapped), the dead clip is seeked to its progress.
        let dying = select_clip_for(Some(("death", 0.5, 2.0)), &moving(0.0), dead, &set);
        assert_eq!((dying.clip, dying.seek_to), (Some("die"), Some(0.25)));
        // Afterwards it plays once and holds the last pose.
        let after = select_clip_for(None, &moving(0.0), dead, &set);
        assert_eq!(after.clip, Some("die"));
        assert!(after.seek_to.is_none() && !after.looping, "{after:?}");
        // Without a dead clip nothing changes: the last pose stays.
        let mut bare = set.clone();
        bare.locomotion.remove("dead");
        assert_eq!(select_clip_for(None, &moving(3.0), dead, &bare).clip, None);
        // A mapped death action still wins while it runs.
        let mut mapped = set;
        mapped.actions.insert("death".into(), "roll".into());
        assert_eq!(select_clip_for(Some(("death", 0.1, 1.0)), &moving(0.0), dead, &mapped).clip, Some("roll"));
    }

    #[test]
    fn staggering_seeks_the_stagger_clip() {
        let set = set();
        let status = BodyStatus { staggered: true, ..default() };
        let c = select_clip_for(Some(("stagger", 0.3, 0.6)), &moving(0.0), status, &set);
        assert_eq!((c.clip, c.seek_to), (Some("hit"), Some(0.5)));
        // Not staggered: the same unmapped action leaves locomotion alone.
        assert_eq!(select_clip(Some(("stagger", 0.3, 0.6)), &moving(0.0), &set).clip, Some("idle"));
        // A staggered body without a stagger action running has nothing to follow.
        assert_eq!(select_clip_for(None, &moving(0.0), status, &set).clip, Some("idle"));
    }

    fn phases(list: &[(&str, f32)]) -> Vec<Phase> {
        list.iter().map(|(n, s)| Phase((*n).to_owned(), *s)).collect()
    }

    #[test]
    fn a_clip_timed_to_the_actions_phases_is_seeked_phase_by_phase() {
        let quick = phases(&[("windup", 0.2), ("active", 0.1), ("recovery", 0.3)]);
        let slow = phases(&[("windup", 0.6), ("active", 0.15), ("recovery", 0.8)]);
        let at = |t, length| phase_seek(&slow, &quick, &[], &[], t, length).unwrap();
        // The slow windup shows the quick one's 0.2 s, and the strike comes as its active phase starts.
        assert!(close(at(0.3, 1.55), 0.1 / 0.6), "{}", at(0.3, 1.55));
        assert!(close(at(0.6, 1.55), 0.2 / 0.6), "{}", at(0.6, 1.55));
        assert!(close(at(0.675, 1.55), 0.25 / 0.6), "halfway through the active phase: {}", at(0.675, 1.55));
        assert!(close(at(1.55, 1.55), 1.0) && close(at(9.0, 1.55), 1.0) && close(at(-1.0, 1.55), 0.0));
        // Evenly, 0.6 s in is 39 % of the clip, past the strike that starts at 33 %: it shows early.
        assert!(even_progress(0.6, 1.55) > at(0.6, 1.55));
        // A run longer than the phases (a miss's longer recovery) stretches the last phase.
        assert!(close(at(0.75 + 0.65, 2.05), (0.3 + 0.15) / 0.6), "{}", at(1.4, 2.05));
        // The same timings seek as evenly.
        assert!(close(phase_seek(&quick, &quick, &[], &[], 0.15, 0.6).unwrap(), even_progress(0.15, 0.6)));
        // Not timed alike: other names, another order, fewer phases, none, a clip of no length.
        assert_eq!(phase_seek(&phases(&[("stroke", 0.5)]), &quick, &[], &[], 0.1, 0.5), None);
        let swapped = phases(&[("active", 0.1), ("windup", 0.2), ("recovery", 0.3)]);
        assert_eq!(phase_seek(&swapped, &quick, &[], &[], 0.1, 0.6), None);
        assert_eq!(phase_seek(&quick[..2], &quick, &[], &[], 0.1, 0.3), None);
        assert_eq!(phase_seek(&[], &quick, &[], &[], 0.1, 0.6), None);
        assert_eq!(phase_seek(&quick, &phases(&[("windup", 0.0), ("active", 0.0), ("recovery", 0.0)]), &[], &[], 0.1, 0.6), None);
    }

    #[test]
    fn a_looped_phase_repeats_its_stretch_of_the_clip() {
        let clip = phases(&[("fall", 0.5), ("down", 1.5), ("rise", 1.0)]);
        let action = phases(&[("fall", 0.5), ("down", 4.0), ("rise", 1.0)]);
        let loops = ["down".to_owned()];
        let at = |t| phase_seek(&action, &clip, &loops, &[], t, 5.5).unwrap();
        // The breath repeats at its own speed: 2 s into the phase is 0.5 s into its second pass.
        assert!(close(at(2.5), (0.5 + 0.5) / 3.0), "{}", at(2.5));
        assert!(close(at(0.5 + 1.5 * 2.0 + 0.25), (0.5 + 0.25) / 3.0));
        // The rise starts at its own start and is stretched as usual.
        assert!(close(at(4.75), 2.25 / 3.0) && close(at(5.5), 1.0));
        // Without `loops` the breath is stretched over the 4 s.
        assert!(close(phase_seek(&action, &clip, &[], &[], 2.5, 5.5).unwrap(), (0.5 + 0.75) / 3.0));
    }

    #[test]
    fn a_held_phase_plays_at_its_speed_then_holds_its_last_pose() {
        // A 1.3 s fall on a death that lasts 3.5 s: the fall at its own speed, then the body lies.
        let clip = phases(&[("dead", 1.3)]);
        let action = phases(&[("dead", 3.5)]);
        let holds = ["dead".to_owned()];
        let at = |t| phase_seek(&action, &clip, &[], &holds, t, 3.5).unwrap();
        assert!(close(at(0.0), 0.0) && close(at(0.65), 0.5), "{}", at(0.65));
        assert!(close(at(1.3), 1.0) && close(at(2.0), 1.0) && close(at(3.5), 1.0), "{}", at(2.0));
        // Without `holds` the fall is stretched over the 3.5 s.
        assert!(close(phase_seek(&action, &clip, &[], &[], 0.65, 3.5).unwrap(), 0.65 / 3.5));
        // A held phase in the middle: the next phase starts at its own start, and is stretched as usual.
        let clip = phases(&[("fall", 0.5), ("down", 1.0), ("rise", 0.5)]);
        let action = phases(&[("fall", 0.5), ("down", 3.0), ("rise", 1.0)]);
        let holds = ["down".to_owned()];
        let place = |t| phase_place(&action, &clip, &[], &holds, t, 4.5).unwrap();
        assert!(close(place(1.0).0, 1.0 / 2.0) && close(place(3.0).0, 1.5 / 2.0), "{:?}", place(1.0));
        assert_eq!(place(3.0).1, None, "a held phase repeats nothing");
        assert!(close(place(4.0).0, 1.75 / 2.0), "{:?}", place(4.0));
        // An action's held phase shorter than the clip's ends where it ends.
        let short = phases(&[("fall", 0.5), ("down", 0.4), ("rise", 1.0)]);
        assert!(close(phase_seek(&short, &clip, &[], &holds, 0.9, 1.9).unwrap(), 0.9 / 2.0));
        assert!(close(phase_seek(&short, &clip, &[], &holds, 0.95, 1.9).unwrap(), (1.5 + 0.025) / 2.0));
        // Time past the action's phases holds a held last phase too.
        let clip = phases(&[("dead", 1.3)]);
        let action = phases(&[("dead", 1.0)]);
        let holds = ["dead".to_owned()];
        assert!(close(phase_seek(&action, &clip, &[], &holds, 0.65, 2.0).unwrap(), 0.5));
        assert!(close(phase_seek(&action, &clip, &[], &holds, 2.0, 2.0).unwrap(), 1.0));
    }

    #[test]
    fn a_dead_clip_that_holds_its_phase_falls_at_its_speed_and_lies_still() {
        let mut set = set();
        let dead = BodyStatus { dead: true, ..default() };
        let death = phases(&[("dead", 3.5)]);
        let idle = moving(0.0);
        // Untimed, the dead clip is stretched over the whole death.
        let c = select_clip_timed(Some(ActionTime::new("death", 0.65, 3.5, &death)), &idle, dead, &set, None);
        assert!(close(c.seek_to.unwrap(), 0.65 / 3.5), "{c:?}");
        let die = set.clips.get_mut("die").unwrap();
        die.phases = phases(&[("dead", 1.3)]);
        die.holds = vec!["dead".into()];
        let c = select_clip_timed(Some(ActionTime::new("death", 0.65, 3.5, &death)), &idle, dead, &set, None);
        assert_eq!(c.clip, Some("die"));
        assert!(close(c.seek_to.unwrap(), 0.5) && c.speed == 0.0, "gameplay still leads: {c:?}");
        let c = select_clip_timed(Some(ActionTime::new("death", 2.4, 3.5, &death)), &idle, dead, &set, None);
        assert!(close(c.seek_to.unwrap(), 1.0), "the last pose, held: {c:?}");
    }

    #[test]
    fn a_looped_phase_fires_its_own_cues_on_each_pass_and_no_earlier_ones() {
        let clip = phases(&[("fall", 0.5), ("down", 1.5), ("rise", 1.0)]);
        let action = phases(&[("fall", 0.5), ("down", 4.0), ("rise", 1.0)]);
        let loops = ["down".to_owned()];
        let place = |t| phase_place(&action, &clip, &loops, &[], t, 5.5).unwrap();
        // The stretch is said while the action is in the looped phase, and only then.
        assert_eq!(place(0.25).1, None);
        let (start, end) = place(2.5).1.unwrap();
        assert!(close(start, 0.5 / 3.0) && close(end, 2.0 / 3.0), "{start} {end}");
        assert_eq!(place(4.75).1, None);
        assert_eq!(phase_place(&action, &clip, &[], &[], 2.5, 5.5).unwrap().1, None, "no `loops`, no stretch");

        // On a 3 s clip: a thud as it falls, a breath in the down stretch (0.5 s to 2 s).
        let cues = [CueDef::new(0.3, "thud"), CueDef::new(1.2, "breath")];
        let fired = |prev, now, wraps| -> Vec<&str> { crossed_cues_in(&cues, prev, now, wraps).iter().map(|c| c.cue.as_str()).collect() };
        let down = Some((0.5, 2.0));
        assert!(fired(Some(1.9), 0.7, down).is_empty(), "the stretch starts over: no thud again");
        assert_eq!(fired(Some(1.9), 1.3, down), ["breath"], "the breath once on the new pass");
        assert_eq!(fired(Some(1.0), 0.6, down), ["breath"], "the rest of the last pass");
        assert_eq!(fired(Some(1.9), 0.7, None), ["thud"], "the old rule: a restart");
        // Going back from outside the stretch (a new run of the action) is a restart.
        assert_eq!(fired(Some(2.5), 1.3, down), ["thud", "breath"]);
        // The whole clip looping is the stretch from 0: as `crossed_cues` with `wrap_at`.
        assert_eq!(fired(Some(2.9), 0.4, Some((0.0, 3.0))), ["thud"]);
    }

    #[test]
    fn the_played_clip_decides_how_an_action_is_seeked() {
        let mut set = set();
        let quick = phases(&[("windup", 0.2), ("active", 0.1), ("recovery", 0.3)]);
        let slow = phases(&[("windup", 0.6), ("active", 0.15), ("recovery", 0.8)]);
        let slow_at = |t| Some(ActionTime::new("light_1", t, 1.55, &slow));
        let idle = moving(0.0);
        let alive = BodyStatus::default();
        // Without phases on the clip, the timed choice is the even one, bit for bit.
        for t in [0.0, 0.3, 0.6, 1.0, 1.55, 2.0] {
            let timed = select_clip_timed(slow_at(t), &idle, alive, &set, None);
            assert_eq!(timed, select_clip_in(Some(("light_1", t, 1.55)), &idle, alive, &set, None), "t {t}");
        }
        set.clips.get_mut("swing_1").unwrap().phases = quick.clone();
        let c = select_clip_timed(slow_at(0.6), &idle, alive, &set, None);
        assert_eq!(c.clip, Some("swing_1"));
        assert!(close(c.seek_to.unwrap(), 0.2 / 0.6), "{c:?}");
        // An action with no phases (the old call) still plays it evenly.
        let even = select_clip_in(Some(("light_1", 0.6, 1.55)), &idle, alive, &set, None);
        assert!(close(even.seek_to.unwrap(), 0.6 / 1.55), "{even:?}");
        // A moveset's family clip is seeked by its own phases: `block` has none, so evenly.
        set.by_moveset.insert("brute".into(), [("swing_1".to_owned(), "block".to_owned())].into());
        let family = select_clip_timed(slow_at(0.6), &idle, alive, &set, Some("brute"));
        assert_eq!(family.clip, Some("block"));
        assert!(close(family.seek_to.unwrap(), 0.6 / 1.55), "{family:?}");
        // The stagger and dead clips follow the running action's phases too.
        set.clips.get_mut("hit").unwrap().phases = phases(&[("reel", 0.2), ("recover", 0.4)]);
        let reel = phases(&[("reel", 0.4), ("recover", 0.4)]);
        let staggered = BodyStatus { staggered: true, ..default() };
        let c = select_clip_timed(Some(ActionTime::new("stagger", 0.4, 0.8, &reel)), &idle, staggered, &set, None);
        assert_eq!(c.clip, Some("hit"));
        assert!(close(c.seek_to.unwrap(), 0.2 / 0.6), "the reel ends where the clip's does: {c:?}");
        // And so does a clip on a layer.
        set.layers.insert("upper".into(), LayerDef::at("spine"));
        set.actions.insert("drink".into(), ActionClip::on("swing_1", "upper"));
        let drink = Some(ActionTime::new("drink", 0.6, 1.55, &slow));
        let layer = select_layer_timed(drink, alive, &set, None).unwrap();
        assert_eq!((layer.layer, layer.choice.clip), ("upper", Some("swing_1")));
        assert!(close(layer.choice.seek_to.unwrap(), 0.2 / 0.6), "{layer:?}");
        assert!(close(select_layer_in(Some(("drink", 0.6, 1.55)), alive, &set, None).unwrap().choice.seek_to.unwrap(), 0.6 / 1.55));
    }

    #[test]
    fn an_empty_set_chooses_nothing() {
        let mut set = set();
        set.locomotion.clear();
        assert_eq!(select_clip(None, &moving(3.0), &set).clip, None);
        // A mapping to a clip the library lacks is ignored too (validation reports it).
        set.locomotion.insert("idle".into(), "nope".into());
        assert_eq!(select_clip(None, &moving(0.0), &set).clip, None);
    }

    #[test]
    fn locomotion_fallbacks_chain() {
        let mut set = set();
        set.locomotion.retain(|k, _| k == "idle" || k == "walk");
        assert_eq!(set.locomotion_clip(Locomotion::Sprint), Some((Locomotion::Walk, "walk")));
        assert_eq!(set.locomotion_clip(Locomotion::Run), Some((Locomotion::Walk, "walk")));
        assert_eq!(set.locomotion_clip(Locomotion::Swim), Some((Locomotion::Walk, "walk")));
        assert_eq!(set.locomotion_clip(Locomotion::Glide), Some((Locomotion::Idle, "idle")));
        assert_eq!(set.locomotion_clip(Locomotion::Dead), None, "dead never falls back");
        assert_eq!(Locomotion::parse("sprint"), Some(Locomotion::Sprint));
        assert_eq!(Locomotion::parse("jog"), None);
        for s in Locomotion::ALL {
            assert_eq!(Locomotion::parse(s.name()), Some(s));
        }
    }

    #[test]
    fn cues_fire_once_as_time_passes() {
        let cues = set().clips["swing_1"].cues.clone();
        let at: Vec<f32> = crossed_cues(&cues, None, 0.5, None).iter().map(|c| c.at).collect();
        assert_eq!(at, [0.1, 0.4], "a fresh clip fires what is already behind it, in time order");
        assert!(crossed_cues(&cues, Some(0.5), 0.6, None).is_empty());
        let step: Vec<&str> = crossed_cues(&cues, Some(0.05), 0.15, None).iter().map(|c| c.cue.as_str()).collect();
        assert_eq!(step, ["draw"]);
        assert!(crossed_cues(&cues, Some(0.1), 0.1, None).is_empty(), "standing still fires nothing");
        assert_eq!(crossed_cues(&cues, Some(0.3), 0.4, None).len(), 1, "a cue exactly at `now` fires");
        assert!(crossed_cues(&cues, Some(0.4), 0.5, None).is_empty(), "and not again");
    }

    #[test]
    fn cues_survive_loops_and_restarts() {
        let cues = set().clips["swing_1"].cues.clone();
        // A loop of length 1.0 wrapped from 0.9 to 0.2: the tail has nothing, the head has `draw`.
        let wrapped: Vec<&str> = crossed_cues(&cues, Some(0.9), 0.2, Some(1.0)).iter().map(|c| c.cue.as_str()).collect();
        assert_eq!(wrapped, ["draw"]);
        // From 0.3 round to 0.05: the rest of the pass has `swoosh`.
        let wrapped: Vec<&str> = crossed_cues(&cues, Some(0.3), 0.05, Some(1.0)).iter().map(|c| c.cue.as_str()).collect();
        assert_eq!(wrapped, ["swoosh"]);
        // An action clip that restarts (same clip, new swing) fires from its start.
        let restarted: Vec<&str> = crossed_cues(&cues, Some(0.55), 0.12, None).iter().map(|c| c.cue.as_str()).collect();
        assert_eq!(restarted, ["draw"]);
    }

    #[test]
    fn a_wrapped_loop_fires_the_rest_of_the_last_pass_before_the_new_one() {
        let cues = [CueDef::new(0.1, "head"), CueDef::new(0.95, "tail")];
        let at = |prev, now, wrap_at| -> Vec<f32> { crossed_cues(&cues, prev, now, wrap_at).iter().map(|c| c.at).collect() };
        assert_eq!(at(Some(0.9), 0.2, Some(1.0)), [0.95, 0.1], "the order they were passed in");
        // Everything else stays in time order.
        assert_eq!(at(None, 1.0, None), [0.1, 0.95]);
        assert_eq!(at(Some(0.05), 0.99, Some(1.0)), [0.1, 0.95]);
        assert!(at(Some(0.5), 0.05, None).is_empty(), "a restart fires what is behind `now` again");
        assert_eq!(at(Some(0.99), 0.12, None), [0.1]);
    }

    #[test]
    fn file_names_default_to_our_own() {
        let set = set();
        assert_eq!(set.file_name("fall"), "Armature|Falling");
        assert_eq!(set.file_name("idle"), "idle");
        assert_eq!(set.file_name("unlisted"), "unlisted");
    }

    fn build(animsets: &str) -> Vec<String> {
        let actions = r#"{ "light_1": (net_id: 1, phases: [("active", 0.3)]), "roll": (net_id: 2, phases: [("roll", 0.4)]) }"#;
        build_with(actions, animsets)
    }

    fn build_with(actions: &str, animsets: &str) -> Vec<String> {
        let mut kinds = ContentKinds::default();
        kinds.register::<ActionDef>();
        kinds.register::<AnimSet>();
        kinds.register::<crate::def::MovesetDef>();
        kinds.register::<crate::status::StatusDef>();
        let sources = [
            shards_content::db::Source { path: "content/a.action.ron".into(), text: actions.into() },
            shards_content::db::Source { path: "content/h.animset.ron".into(), text: animsets.into() },
        ];
        ContentDb::build(&kinds, &sources).errors().map(ToString::to_string).collect()
    }

    #[test]
    fn an_action_clip_no_animset_has_is_an_error_but_a_mapped_one_is_not() {
        let set = r#"{ "h": (source: "models/h.glb", clips: {"idle": (), "slash": (name: "Slash")},
            actions: {"light_1": "slash"}, tags: {"fire": "slash"}) }"#;
        let actions = r#"{
            "light_1": (net_id: 1, phases: [("active", 0.3)], anim: "nothing"),
            "a": (net_id: 2, phases: [("a", 0.3)], anim: "idle"),
            "b": (net_id: 3, phases: [("a", 0.3)], anim: "swing", tags: ["fire"]),
            "c": (net_id: 4, phases: [("a", 0.3)], anim: "slsh"),
            "d": (net_id: 5, phases: [("a", 0.3)]),
        }"#;
        assert_eq!(
            build_with(actions, set),
            [
                "content/a.action.ron: action `c`: anim `slsh` is no clip of any animset (did you mean `slash`?); clips: \
                 idle, slash; name one of them, or leave `anim` out and the character keeps its locomotion clip meanwhile"
            ],
            "a clip, a mapped id and a tag are played; a typo is not; no `anim` asks for nothing"
        );
        // A game with no animset shows its actions another way: `anim` is only a name there.
        let alone = shards_content::db::Source { path: "content/a.action.ron".into(), text: actions.into() };
        let mut kinds = ContentKinds::default();
        kinds.register::<ActionDef>();
        let errors: Vec<String> = ContentDb::build(&kinds, &[alone]).errors().map(ToString::to_string).collect();
        assert!(errors.is_empty(), "{errors:#?}");
    }

    #[test]
    fn a_clips_phases_are_checked_and_an_action_must_share_all_of_them_or_none() {
        let set = r#"{ "h": (source: "models/h.glb", clips: {
                "swing": (phases: [("windup", 0.2), ("active", 0.1), ("recovery", 0.3)]),
                "heavy": (phases: [("windup", 0.5), ("strike", 0.1), ("recovery", 0.3)]),
                "bad": (phases: [("", 0.1), ("a", -1.0), ("a", 0.0)]),
                "bare": (loops: ["x"]),
                "breath": (phases: [("in", 1.0)], loops: ["inn"]),
                "held": (holds: ["x"]),
                "lying": (phases: [("down", 1.0)], loops: ["down"], holds: ["down", "dwn"]),
                "still": (phases: [("still", 0.0)]),
            },
            actions: {"spar": "swing"}, by_moveset: {"brute": {"swing": "heavy"}}) }"#;
        let actions = r#"{
            "light": (net_id: 1, phases: [("windup", 0.6), ("active", 0.15), ("recovery", 0.8)], anim: "swing"),
            "spar": (net_id: 2, phases: [("stroke", 0.5)]),
            "typo": (net_id: 3, phases: [("windup", 0.2), ("actve", 0.1), ("recovery", 0.3)], anim: "swing"),
        }"#;
        let mut kinds = ContentKinds::default();
        kinds.register::<ActionDef>();
        kinds.register::<AnimSet>();
        kinds.register::<crate::def::MovesetDef>();
        kinds.register::<crate::status::StatusDef>();
        let sources = [
            shards_content::db::Source { path: "content/a.action.ron".into(), text: actions.into() },
            shards_content::db::Source { path: "content/h.animset.ron".into(), text: set.into() },
            shards_content::db::Source { path: "content/m.moveset.ron".into(), text: r#"{ "brute": (inputs: {}) }"#.into() },
        ];
        let errors: Vec<String> = ContentDb::build(&kinds, &sources).errors().map(ToString::to_string).collect();
        let h = "content/h.animset.ron: animset `h`: ";
        assert_eq!(
            errors,
            [
                format!("{h}clips.bad.phases has a phase with no name"),
                format!("{h}clips.bad.phases: `a` must last >= 0 s (is -1)"),
                format!("{h}clips.bad.phases has `a` twice; an action's phase finds the clip's by its name"),
                format!(
                    "{h}clips.bare.loops names `x`, but the clip has no `phases`; give the phases it was timed to, \
                     e.g. `phases: [(\"x\", 1.0)]`"
                ),
                format!("{h}clips.breath.loops names an unknown phase `inn` (did you mean `in`?); phases: in"),
                format!(
                    "{h}clips.held.holds names `x`, but the clip has no `phases`; give the phases it was timed to, \
                     e.g. `phases: [(\"x\", 1.0)]`"
                ),
                format!("{h}clips.lying.holds names an unknown phase `dwn` (did you mean `down`?); phases: down"),
                format!(
                    "{h}clips.lying: phase `down` is in both `loops` and `holds`; a phase repeats its stretch or holds its \
                     last pose, not both"
                ),
                format!("{h}clips.still.phases last 0 s in all; give the seconds the clip was timed to"),
                format!(
                    "{h}clips.heavy.phases (windup 0.5, strike 0.1, recovery 0.3) and action `light`, which plays it (windup 0.6, \
                     active 0.15, recovery 0.8), share windup, recovery but are not the same phases in the same order; give \
                     the clip the action's phase names (or none of them, and it plays evenly)"
                ),
                format!(
                    "{h}clips.swing.phases (windup 0.2, active 0.1, recovery 0.3) and action `typo`, which plays it (windup 0.2, \
                     actve 0.1, recovery 0.3), share windup, recovery but are not the same phases in the same order; give \
                     the clip the action's phase names (or none of them, and it plays evenly)"
                ),
                format!(
                    "{h}clips.heavy.phases (windup 0.5, strike 0.1, recovery 0.3) and action `typo`, which plays it (windup 0.2, \
                     actve 0.1, recovery 0.3), share windup, recovery but are not the same phases in the same order; give \
                     the clip the action's phase names (or none of them, and it plays evenly)"
                ),
            ],
            "`light` matches `swing`; the spar's stroke shares nothing and plays evenly"
        );
    }

    #[test]
    fn the_heading_is_the_quarter_the_body_moves_in_and_keeps_its_slack_at_the_edges() {
        let ahead = Vec3::NEG_Z;
        let at = |degrees: f32| {
            let (s, c) = ops::sin_cos(degrees.to_radians());
            // Clockwise from forward seen from above: +90 is the body's right (+x).
            Vec3::new(s, 0.0, -c)
        };
        assert_eq!(Heading::of(ahead, at(0.0), Heading::Back), Heading::Forward);
        assert_eq!(Heading::of(ahead, at(90.0), Heading::Forward), Heading::Right);
        assert_eq!(Heading::of(ahead, at(-90.0), Heading::Forward), Heading::Left);
        assert_eq!(Heading::of(ahead, at(180.0), Heading::Forward), Heading::Back);
        // A diagonal: the heading it had stays until 10 degrees past the edge.
        assert_eq!(Heading::of(ahead, at(50.0), Heading::Forward), Heading::Forward);
        assert_eq!(Heading::of(ahead, at(50.0), Heading::Right), Heading::Right);
        assert_eq!(Heading::of(ahead, at(40.0), Heading::Right), Heading::Right);
        assert_eq!(Heading::of(ahead, at(56.0), Heading::Forward), Heading::Right);
        assert_eq!(Heading::of(ahead, at(-140.0), Heading::Left), Heading::Left);
        assert_eq!(Heading::of(ahead, at(-150.0), Heading::Left), Heading::Back);
        // Turned bodies: the quarter is the body's, not the world's.
        assert_eq!(Heading::of(Vec3::X, Vec3::NEG_Z, Heading::Forward), Heading::Left);
        assert_eq!(Heading::of(Vec3::X, Vec3::NEG_X, Heading::Forward), Heading::Back);
        // Standing still keeps what it had.
        assert_eq!(Heading::of(ahead, Vec3::ZERO, Heading::Left), Heading::Left);
    }

    #[test]
    fn a_body_facing_a_target_shows_its_headings_clips_and_backs_away_on_its_gait_played_backward() {
        let mut set = set();
        for clip in ["walk_back", "strafe_left", "guard_idle"] {
            set.clips.insert(clip.into(), ClipDef::default());
        }
        set.locomotion.insert("walk.back".into(), "walk_back".into());
        set.locomotion.insert("walk.left".into(), "strafe_left".into());
        set.locomotion.insert("idle.facing".into(), "guard_idle".into());
        let facing = |h: Heading| BodyStatus { facing: Some(h), ..default() };
        let back = select_clip_for(None, &moving(2.0), facing(Heading::Back), &set);
        assert_eq!((back.clip, back.gait), (Some("walk_back"), true));
        assert!(close(back.speed, 1.0), "authored: plays forward at the walk's pace: {back:?}");
        assert_eq!(select_clip_for(None, &moving(2.0), facing(Heading::Left), &set).clip, Some("strafe_left"));
        // No `walk.right`: the walk as it is.
        let right = select_clip_for(None, &moving(2.0), facing(Heading::Right), &set);
        assert_eq!(right.clip, Some("walk"));
        assert!(close(right.speed, 1.0), "{right:?}");
        // No `run.back`: the run played backward, at the run's pace.
        let run_back = select_clip_for(None, &moving(5.0), facing(Heading::Back), &set);
        assert_eq!((run_back.clip, run_back.gait, run_back.looping), (Some("run"), true, true));
        assert!(close(run_back.speed, -1.1), "{run_back:?}");
        // Standing: the combat idle; facing no target, the plain one.
        let idle = select_clip_for(None, &moving(0.0), facing(Heading::Back), &set);
        assert_eq!((idle.clip, idle.gait), (Some("guard_idle"), false));
        assert_eq!(select_clip_for(None, &moving(0.0), BodyStatus::default(), &set).clip, Some("idle"));
        // Facing no target, a backward walk is the walk: the body turns to where it goes.
        assert_eq!(select_clip_for(None, &moving(2.0), BodyStatus::default(), &set).clip, Some("walk"));
        // An action still leads, and the air has no heading.
        let acting = select_clip_for(Some(("light_1", 0.1, 0.6)), &moving(2.0), facing(Heading::Back), &set);
        assert_eq!((acting.clip, acting.gait), (Some("swing_1"), false));
        let falling = MotionState { mode: MotionMode::Air, ..moving(2.0) };
        let air = select_clip_for(None, &falling, facing(Heading::Back), &set);
        assert!(air.clip == Some("fall") && air.speed > 0.0 && !air.gait, "{air:?}");
        // Families rename the heading's clip too.
        set.by_moveset.insert("brute".into(), BTreeMap::from([("walk_back".to_owned(), "strafe_left".to_owned())]));
        assert_eq!(select_clip_in(None, &moving(2.0), facing(Heading::Back), &set, Some("brute")).clip, Some("strafe_left"));
        assert_eq!(facing_key(Locomotion::Swim, Heading::Back), None);
        assert_eq!(facing_key(Locomotion::Walk, Heading::Forward), None);
        assert_eq!(FACING_KEYS.len(), 10);
    }

    #[test]
    fn a_clip_played_backward_passes_its_cues_going_down() {
        let cues: Vec<CueDef> = ron::from_str(r#"[(at: 0.1, cue: "a"), (at: 0.4, cue: "b"), (at: 0.7, cue: "c")]"#).unwrap();
        fn names(found: Vec<&CueDef>) -> Vec<&str> {
            found.into_iter().map(|c| c.cue.as_str()).collect()
        }
        assert_eq!(names(crossed_cues_back(&cues, None, 0.9, Some(1.0))), Vec::<&str>::new(), "just started: nothing passed");
        assert_eq!(names(crossed_cues_back(&cues, Some(0.8), 0.3, Some(1.0))), ["c", "b"]);
        assert_eq!(names(crossed_cues_back(&cues, Some(0.4), 0.35, Some(1.0))), Vec::<&str>::new(), "left the cue's moment, never reached it");
        assert_eq!(names(crossed_cues_back(&cues, Some(0.5), 0.4, Some(1.0))), ["b"], "reached it");
        // Wrapped below the start: the rest of this pass, then the end of the next.
        assert_eq!(names(crossed_cues_back(&cues, Some(0.2), 0.6, Some(1.0))), ["a", "c"]);
        assert_eq!(names(crossed_cues_back(&cues, Some(0.2), 0.6, None)), Vec::<&str>::new(), "not looping: a restart");
        // Forward play is unchanged: a backward step there reads as a wrap.
        assert_eq!(names(crossed_cues(&cues, Some(0.8), 0.3, Some(1.0))), ["a"]);
    }

    #[test]
    fn a_gaits_stride_is_carried_to_the_next_clip() {
        assert!(close(carry_phase(0.25, 1.0, 0.8).unwrap(), 0.2));
        assert!(close(carry_phase(1.5, 1.0, 2.0).unwrap(), 1.0), "a time past the end is its place in the stride");
        assert_eq!(carry_phase(0.25, 0.0, 0.8), None);
        assert_eq!(carry_phase(0.25, 1.0, 0.0), None);
    }

    #[test]
    fn the_facing_keys_and_the_turn_are_checked() {
        let set = r#"{
            "h": (source: "models/h.glb", clips: {"idle": (), "walk": (), "roll": (), "drink": ()},
                locomotion: {"idle": "idle", "walk.bak": "walk", "idle.facing": "idle", "run.left": "walk"},
                layers: {"upper": (root: "spine")},
                actions: {"dodge": (clip: "roll", turn: 0.1), "lunge": (clip: "roll", turn: -1.0),
                          "sip": (clip: "drink", layer: "upper", turn: 0.2)},
                tags: {"evade": (clip: "roll", turn: 0.1)}),
            "rooted": (source: "models/h.glb", clips: {"roll": ()}, actions: {"dodge": (clip: "roll", turn: 0.1)}, root_motion: "root"),
        }"#;
        let actions = r#"{
            "dodge": (net_id: 1, phases: [("roll", 0.4)], face: Keep),
            "lunge": (net_id: 2, phases: [("roll", 0.4)], face: Keep),
            "sip": (net_id: 3, phases: [("sip", 1.0)]),
            "spin": (net_id: 4, phases: [("roll", 0.4)], tags: ["evade"]),
        }"#;
        let mut kinds = ContentKinds::default();
        kinds.register::<ActionDef>();
        kinds.register::<AnimSet>();
        kinds.register::<crate::def::MovesetDef>();
        kinds.register::<crate::status::StatusDef>();
        let sources = [
            shards_content::db::Source { path: "content/a.action.ron".into(), text: actions.into() },
            shards_content::db::Source { path: "content/h.animset.ron".into(), text: set.into() },
        ];
        let errors: Vec<String> = ContentDb::build(&kinds, &sources).errors().map(ToString::to_string).collect();
        let h = "content/h.animset.ron: animset `h`: ";
        let needs_root = |field: &str| {
            format!(
                "{h}{field}.turn needs `root_motion`: the turn is written on that bone; name the root bone, e.g. \
                 `root_motion: \"root\"`"
            )
        };
        assert_eq!(
            errors,
            [
                format!(
                    "{h}locomotion has an unknown state `walk.bak` (did you mean `walk.back`?); states: idle, walk, run, sprint, \
                     air, swim, glide, mantle, ladder, stagger, dead; facing a target: a gait's heading (`walk.back`, `run.left`, \
                     `sprint.right`) or `idle.facing`"
                ),
                needs_root("actions.dodge"),
                format!("{h}actions.lunge.turn must be >= 0 seconds (is -1)"),
                format!(
                    "{h}actions.sip.turn turns the whole model, and the clip plays on layer `upper`; leave `turn` out or play \
                     the clip on the whole body"
                ),
                needs_root("actions.sip"),
                needs_root("tags.evade"),
                format!(
                    "{h}tags.evade.turn turns the model to the heading of a run that keeps the body's facing, and action \
                     `spin` turns the body itself (`face: Run`); give it `face: Keep` or leave `turn` out"
                ),
            ],
            "`rooted` is fine; `sip` plays on a layer, so it has no turn to refuse for `face: Run`"
        );
        // The turn is written only when there is one.
        let parse = |text: &str| -> ActionClip {
            ron::Options::default()
                .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
                .from_str(text)
                .unwrap_or_else(|e| panic!("{text}: {e}"))
        };
        let turned = parse(r#"(clip: "roll", turn: 0.08)"#);
        assert_eq!(serde_json::to_value(&turned).unwrap(), serde_json::json!({"clip": "roll", "turn": 0.08f32}));
        assert_eq!(serde_json::to_value(parse(r#"(clip: "roll", turn: 0.0)"#)).unwrap(), serde_json::json!("roll"));
    }

    #[test]
    fn an_overlay_comes_in_holds_its_strength_and_fades_out_smoothly() {
        let def = OverlayDef { clip: "hit".into(), layer: "upper".into(), fade_in: 0.1, fade_out: (0.3, 0.5), strength: 0.8 };
        let near = |a: f32, b: f32| (a - b).abs() < 1e-5;
        assert!(near(def.share(0.0, 0.0), 0.0));
        assert!(near(def.share(0.05, 0.0), 0.4), "half way in, linearly");
        assert!(near(def.share(0.2, 0.0), 0.8), "all in: its strength");
        // A quarter of the way out the smoothstep has taken 0.15625 of it, half way a half.
        assert!(near(def.share(0.35, 0.0), 0.8 * (1.0 - 0.15625)), "{}", def.share(0.35, 0.0));
        assert!(near(def.share(0.4, 0.0), 0.4));
        assert!(!def.ended(0.49) && def.ended(0.5));
        assert!(near(def.share(0.5, 0.0), 0.0), "over at the end of its fade out");
        assert!(near(def.share(0.0, 0.5), 0.4), "the same event again picks up from where it was");
        let at_once = OverlayDef { fade_in: 0.0, fade_out: (0.2, 0.2), ..def };
        assert!(near(at_once.share(0.0, 0.0), 0.8) && near(at_once.share(0.19, 0.0), 0.8));
        assert!(at_once.ended(0.2), "a fade out with no length ends at once");
    }

    #[test]
    fn overlays_and_blend_out_are_checked() {
        let set = r#"{
            "h": (source: "models/h.glb", clips: {"idle": (), "hit": (), "drink": ()},
                layers: {"upper": (root: "spine")},
                actions: {"sip": (clip: "drink", layer: "upper", blend_out: 0.075), "gulp": (clip: "drink", blend_out: -1.0)},
                overlays: {
                    "damaged": (clip: "hit", layer: "upper", fade_out: (0.2, 0.35), strength: 0.7),
                    "damage": (clip: "hitt", layer: "uper", fade_in: -0.1, fade_out: (0.3, 0.2), strength: 1.5),
                }),
        }"#;
        let actions = r#"{
            "sip": (net_id: 1, phases: [("sip", 1.0)]),
            "gulp": (net_id: 2, phases: [("gulp", 1.0)]),
        }"#;
        let mut kinds = ContentKinds::default();
        kinds.register::<ActionDef>();
        kinds.register::<AnimSet>();
        kinds.register::<crate::def::MovesetDef>();
        kinds.register::<crate::status::StatusDef>();
        let sources = [
            shards_content::db::Source { path: "content/a.action.ron".into(), text: actions.into() },
            shards_content::db::Source { path: "content/h.animset.ron".into(), text: set.into() },
        ];
        let db = ContentDb::build(&kinds, &sources);
        let errors: Vec<String> = db.errors().map(ToString::to_string).collect();
        let h = "content/h.animset.ron: animset `h`: ";
        assert_eq!(
            errors,
            [
                format!("{h}actions.gulp.blend_out must be >= 0 seconds (is -1)"),
                format!(
                    "{h}actions.gulp.blend_out is how a clip leaves its layer, and the clip plays on the whole body (the blend \
                     of what the body shows next takes it over); leave `blend_out` out or give the mapping a `layer`"
                ),
                format!(
                    "{h}overlays has an unknown event `damage` (did you mean `damaged`?); events: blocked, parried, damaged, \
                     guard_broken, staggered, part_broken, landed"
                ),
                format!("{h}overlays.damage -> unknown clip `hitt` (did you mean `hit`?); clips: drink, hit, idle"),
                format!("{h}overlays.damage.layer -> unknown layer `uper` (did you mean `upper`?); layers: upper"),
                format!("{h}overlays.damage.fade_in must be >= 0 seconds (is -0.1)"),
                format!(
                    "{h}overlays.damage.fade_out must be (start, end) in seconds after the event, 0 <= start <= end and end > 0 \
                     (is (0.3, 0.2)); the overlay fades out between them and ends at `end`"
                ),
                format!("{h}overlays.damage.strength must be above 0 and at most 1 (is 1.5)"),
            ],
            "`sip` and `damaged` are fine"
        );
        // Written only when they are not the defaults.
        let set = db.get::<AnimSet>("h").unwrap();
        assert_eq!(
            serde_json::to_value(&set.actions["sip"]).unwrap(),
            serde_json::json!({"clip": "drink", "layer": "upper", "blend_out": 0.075f32})
        );
        assert_eq!(
            serde_json::to_value(&set.overlays["damaged"]).unwrap(),
            serde_json::json!({"clip": "hit", "layer": "upper", "fade_out": [0.2f32, 0.35f32], "strength": 0.7f32})
        );
    }

    #[test]
    fn the_stagger_and_dead_clips_are_checked_against_the_movesets_stagger_and_death_actions() {
        // A moveset's stagger or death action that maps no clip of its own is seeked on the set's
        // `stagger` or `dead` clip (in that moveset's family) by its phases: a typo there is the same error.
        let set = r#"{ "h": (source: "models/h.glb", clips: {
                "idle": (),
                "hit": (phases: [("reel", 0.2), ("recover", 0.4)]),
                "big_hit": (phases: [("reel", 0.3), ("recover", 0.5)]),
                "death": (phases: [("fall", 0.5), ("down", 1.0)]),
                "kneel": (phases: [("fall", 0.5), ("down", 1.0)]),
            },
            locomotion: {"idle": "idle", "stagger": "hit", "dead": "death"},
            actions: {"kneel": "kneel"}, by_moveset: {"brute": {"hit": "big_hit"}}) }"#;
        let actions = r#"{
            "reel": (net_id: 1, phases: [("reel", 0.4), ("recoverr", 0.4)]),
            "die": (net_id: 2, phases: [("fall", 0.6), ("down", 2.0)]),
            "kneel": (net_id: 3, phases: [("fall", 0.5), ("dwn", 1.0)]),
        }"#;
        let movesets = r#"{
            "hero": (inputs: {}, stagger: "reel", death: "die"),
            "brute": (inputs: {}, stagger: "reel", death: "kneel"),
        }"#;
        let mut kinds = ContentKinds::default();
        kinds.register::<ActionDef>();
        kinds.register::<AnimSet>();
        kinds.register::<crate::def::MovesetDef>();
        kinds.register::<crate::status::StatusDef>();
        let sources = [
            shards_content::db::Source { path: "content/a.action.ron".into(), text: actions.into() },
            shards_content::db::Source { path: "content/h.animset.ron".into(), text: set.into() },
            shards_content::db::Source { path: "content/m.moveset.ron".into(), text: movesets.into() },
        ];
        let errors: Vec<String> = ContentDb::build(&kinds, &sources).errors().map(ToString::to_string).collect();
        let fix = "but are not the same phases in the same order; give the clip the action's phase names (or none of them, \
                   and it plays evenly)";
        let h = "content/h.animset.ron: animset `h`: ";
        assert_eq!(
            errors,
            [
                format!(
                    "{h}clips.kneel.phases (fall 0.5, down 1) and action `kneel`, which plays it (fall 0.5, dwn 1), share fall {fix}"
                ),
                format!(
                    "{h}clips.big_hit.phases (reel 0.3, recover 0.5) and action `reel`, which plays it (reel 0.4, recoverr 0.4), \
                     share reel {fix}"
                ),
                format!(
                    "{h}clips.hit.phases (reel 0.2, recover 0.4) and action `reel`, which plays it (reel 0.4, recoverr 0.4), \
                     share reel {fix}"
                ),
            ],
            "the brute's stagger shows its family's `big_hit`, the hero's the `hit`; the brute's death has a clip of its \
             own (checked once, as an action); the hero's `die` matches the `death` clip"
        );
    }

    #[test]
    fn the_example_in_the_docs_is_valid_content() {
        // The RON of the module docs, with the actions it names.
        let docs = include_str!("anim.rs");
        let start = docs.find("//!     \"humanoid\": (").expect("the example");
        let end = docs[start..].find("//! }\n").expect("its end") + start;
        let body: String = docs[start..end].lines().map(|l| l.trim_start_matches("//!").to_owned() + "\n").collect();
        let errors = build(&format!("{{\n{body}}}"));
        // The example names `light_1` and `roll`, which the helper's content has too.
        assert!(errors.is_empty(), "{errors:#?}");
    }

    #[test]
    fn a_good_animset_validates() {
        let errors = build(
            r#"{ "h": (source: "models/h.glb", clips: {"idle": (), "slash": (name: "Slash", cues: [(at: 0.2, cue: "x")])},
                locomotion: {"idle": "idle"}, actions: {"light_1": "slash"}, tags: {"fire": "slash"}) }"#,
        );
        assert!(errors.is_empty(), "{errors:#?}");
    }

    #[test]
    fn a_cue_without_a_bone_or_strength_keeps_its_json() {
        // The content hash feeds the co-op handshake: a cue that uses neither new field is the
        // JSON it was before D1.7.
        let plain = CueDef::new(0.5, "swoosh");
        assert_eq!(serde_json::to_value(&plain).unwrap(), serde_json::json!({"at": 0.5, "cue": "swoosh"}));
        // The old form parses to the defaults: at the body, at full strength.
        let read = read_set(r#"(source: "models/h.glb", clips: {"walk": (cues: [(at: 0.5, cue: "footstep")])})"#);
        assert_eq!(read.clips["walk"].cues, [CueDef { at: 0.5, cue: "footstep".into(), bone: None, strength: 1.0 }]);
        assert_eq!(serde_json::to_value(&read.clips["walk"]).unwrap()["cues"], serde_json::json!([{"at": 0.5, "cue": "footstep"}]));
        // A strength of exactly 1 written out is left out again; a bone and any other strength stay.
        let read = read_set(
            r#"(source: "models/h.glb", clips: {"walk": (cues: [
                (at: 0.0, cue: "footstep", bone: "foot.L", strength: 1.0),
                (at: 0.5, cue: "footstep", bone: "foot.R", strength: 0.5),
                (at: 0.75, cue: "thud", strength: 0),
            ])})"#,
        );
        let cues = &read.clips["walk"].cues;
        assert_eq!(cues[0], CueDef::new(0.0, "footstep").at_bone("foot.L"));
        assert_eq!(cues[1], CueDef::new(0.5, "footstep").at_bone("foot.R").with_strength(0.5));
        assert_eq!(cues[2], CueDef::new(0.75, "thud").with_strength(0.0), "a strength of 0 is written, not left out");
        assert_eq!(
            serde_json::to_value(cues).unwrap(),
            serde_json::json!([
                {"at": 0.0, "cue": "footstep", "bone": "foot.L"},
                {"at": 0.5, "cue": "footstep", "bone": "foot.R", "strength": 0.5},
                {"at": 0.75, "cue": "thud", "strength": 0.0},
            ])
        );
        // A set written out and read again is the same.
        assert_eq!(read_set(&ron::to_string(&read).unwrap()), read);
        // A misspelt field is a parse error, like any other field of an animset.
        let errors = build(r#"{ "h": (source: "models/h.glb", clips: {"walk": (cues: [(at: 0.5, cue: "x", bones: "foot.L")])}) }"#);
        assert!(errors.len() == 1 && errors[0].contains("bones") && errors[0].contains("`bone`"), "{errors:#?}");
    }

    #[test]
    fn a_cues_bone_and_strength_are_checked() {
        let errors = build(
            r#"{
            "h": (source: "models/h.glb", clips: {
                "idle": (),
                "walk": (cues: [
                    (at: 0.0, cue: "footstep", bone: "foot.L", strength: 0.5),
                    (at: 0.4, cue: "footstep", bone: ""),
                    (at: 0.5, cue: "footstep", strength: -1),
                    (at: 0.6, cue: "footstep", strength: inf),
                    (at: 0.7, cue: "footstep", bone: "", strength: -0.25),
                    (at: 0.8, cue: "silent", strength: 0),
                ]),
            }),
        }"#,
        );
        assert_eq!(
            errors,
            [
                "content/h.animset.ron: animset `h`: clips.walk.cues[1].bone is empty; name a bone of the model \
                 (`bone: \"foot.L\"`) or leave it out",
                "content/h.animset.ron: animset `h`: clips.walk.cues[2].strength must be a number, 0 or more (is -1)",
                "content/h.animset.ron: animset `h`: clips.walk.cues[3].strength must be a number, 0 or more (is inf)",
                "content/h.animset.ron: animset `h`: clips.walk.cues[4].bone is empty; name a bone of the model \
                 (`bone: \"foot.L\"`) or leave it out",
                "content/h.animset.ron: animset `h`: clips.walk.cues[4].strength must be a number, 0 or more (is -0.25)",
            ]
        );
    }

    #[test]
    fn validation_names_the_field_and_the_fix() {
        let errors = build(
            r#"{
            "bad": (source: "models/h.png", clips: {"idle": (speed: 0), "walk": (blend: -1, cues: [(at: -1, cue: "")])},
                locomotion: {"idel": "idle", "walk": "wlak", "run": "idle"},
                actions: {"light_2": "idle", "roll": "dance"}, tags: {"fire": "nope"},
                blend: -0.1, speeds: (walk: 0, min_scale: 2, max_scale: 1)),
            "empty": (source: "", clips: {}),
        }"#,
        );
        let expected = [
            "content/h.animset.ron: animset `bad`: source `models/h.png` is not a glTF file (.gltf or .glb)",
            "content/h.animset.ron: animset `bad`: clips.idle.speed must be > 0 (is 0)",
            "content/h.animset.ron: animset `bad`: clips.walk.blend must be >= 0 (is -1)",
            "content/h.animset.ron: animset `bad`: clips.walk.cues[0].at must be >= 0 (is -1)",
            "content/h.animset.ron: animset `bad`: clips.walk.cues[0].cue has no name",
            "content/h.animset.ron: animset `bad`: locomotion has an unknown state `idel` (did you mean `idle`?); states: idle, walk, run, sprint, air, swim, glide, mantle, ladder, stagger, dead; facing a target: a gait's heading (`walk.back`, `run.left`, `sprint.right`) or `idle.facing`",
            "content/h.animset.ron: animset `bad`: locomotion.walk -> unknown clip `wlak` (did you mean `walk`?); clips: idle, walk",
            "content/h.animset.ron: animset `bad`: actions -> unknown action `light_2` (did you mean `light_1`?)",
            "content/h.animset.ron: animset `bad`: actions.roll -> unknown clip `dance`; clips: idle, walk",
            "content/h.animset.ron: animset `bad`: tags.fire -> unknown clip `nope`; clips: idle, walk",
            "content/h.animset.ron: animset `bad`: blend must be >= 0 (is -0.1)",
            "content/h.animset.ron: animset `bad`: speeds.walk must be > 0 (is 0)",
            "content/h.animset.ron: animset `bad`: speeds.min_scale and max_scale must satisfy 0 < min_scale <= max_scale (are 2 and 1)",
            "content/h.animset.ron: animset `empty`: source is empty; name the glTF with the clips, e.g. `source: \"models/hero.glb\"`",
            "content/h.animset.ron: animset `empty`: clips is empty; list the animations to use, e.g. `clips: {\"idle\": ()}`",
        ];
        assert_eq!(errors, expected);
    }

    #[test]
    fn progress_comes_from_the_running_action() {
        // The bridge feeds `select_clip` from `Running`: t and length in seconds.
        let def: ActionDef = ron::Options::default()
            .from_str(r#"(net_id: 1, phases: [("windup", 0.2), ("active", 0.1), ("recovery", 0.3)])"#)
            .unwrap();
        let mut run = Running::new("light_1", &def, 0, Vec3::NEG_Z, None, Some("light".into()));
        run.t = 0.3;
        let set = set();
        let c = select_clip(Some((&run.action, run.t, run.length)), &moving(0.0), &set);
        assert!(close(c.seek_to.unwrap(), 0.5), "{c:?}");
    }

    /// The set of `set()` with a drinking clip that plays on the layer `upper` (the spine and
    /// what is below it), and a tag that does too.
    fn layered() -> AnimSet {
        let mut set = set();
        set.clips.insert("drink".into(), ClipDef { blend: Some(0.3), ..default() });
        set.layers.insert("upper".into(), LayerDef::at("spine"));
        set.actions.insert("drink".into(), ActionClip::on("drink", "upper"));
        set.tags.insert("heal".into(), ActionClip::on("generic_attack", "upper"));
        set
    }

    fn read_set(text: &str) -> AnimSet {
        ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
            .from_str(text)
            .unwrap_or_else(|e| panic!("{e}: {text}"))
    }

    #[test]
    fn an_action_is_mapped_by_the_plain_form_or_with_a_layer() {
        let set = read_set(
            r#"(
            source: "models/humanoid.glb",
            clips: {"idle": (), "swing_1": (), "drink": (), "guard": ()},
            layers: {"upper": (root: "spine")},
            actions: {"light_1": "swing_1", "drink": (clip: "drink", layer: "upper"), "block": (clip: "guard")},
            tags: {"heal": (layer: "upper", clip: "drink")},
        )"#,
        );
        assert_eq!(set.actions["light_1"], ActionClip::body("swing_1"), "the old form: the whole body");
        assert_eq!(set.actions["drink"], ActionClip::on("drink", "upper"));
        assert_eq!(set.actions["block"], ActionClip::body("guard"), "a clip with no layer: the whole body");
        assert_eq!(set.tags["heal"], ActionClip::on("drink", "upper"), "in any order");
        assert_eq!(set.layers["upper"].root, "spine");
        assert_eq!(set.action_shown("light_1"), Some(("swing_1", None)));
        assert_eq!(set.action_shown("drink"), Some(("drink", Some("upper"))));
        assert_eq!(set.action_shown("heal"), Some(("drink", Some("upper"))), "by tag");
        assert_eq!(set.action_clip("drink"), Some("drink"), "the clip alone, for callers that do not care");
        assert!(set.is_layered("drink") && !set.is_layered("light_1") && !set.is_layered("nothing"));
        assert_eq!((set.layer_index("upper"), set.layer_index("lower")), (Some(0), None));
        // Written out and read again it is the same: plain where plain, a struct where layered.
        let text = ron::to_string(&set).unwrap();
        assert!(text.contains(r#""light_1":"swing_1""#) && text.contains(r#"(clip:"drink",layer:"upper")"#), "{text}");
        assert_eq!(read_set(&text), set);
    }

    #[test]
    fn a_layered_action_leaves_the_body_to_locomotion_and_plays_on_its_layer() {
        let set = layered();
        let walking = moving(2.0);
        let drinking = Some(("drink", 0.5, 1.0));
        let body = select_clip_for(drinking, &walking, BodyStatus::default(), &set);
        assert_eq!(body.clip, Some("walk"), "the legs go on walking");
        assert!(body.looping && body.seek_to.is_none() && close(body.speed, 1.0), "{body:?}");
        let layer = select_layer(drinking, BodyStatus::default(), &set).expect("the layer has the drink");
        assert_eq!(layer.layer, "upper");
        assert_eq!(layer.choice.clip, Some("drink"));
        let choice = layer.choice;
        assert!(close(choice.seek_to.unwrap(), 0.5) && close(choice.speed, 0.0) && !choice.looping, "{choice:?}");
        assert!(close(choice.blend, 0.3), "the clip's own blend: {}", choice.blend);
        // Standing still: the idle clip is the body's.
        assert_eq!(select_clip_for(drinking, &moving(0.0), BodyStatus::default(), &set).clip, Some("idle"));
        // A tag maps the same way.
        let tags = ["heal".to_owned()];
        let key = set.action_key("potion", None, &tags);
        let healing = select_layer(Some((key, 0.1, 0.5)), BodyStatus::default(), &set);
        assert_eq!(healing.map(|l| l.choice.clip), Some(Some("generic_attack")));
        // Past the end it holds the last pose, like any seeked clip; a zero length is the end.
        let at = |t, len| select_layer(Some(("drink", t, len)), BodyStatus::default(), &set).unwrap().choice.seek_to;
        assert_eq!((at(2.0, 1.0), at(0.0, 0.0)), (Some(1.0), Some(1.0)));
    }

    #[test]
    fn nothing_plays_on_a_layer_unless_a_layered_action_runs_on_a_living_body() {
        let set = layered();
        assert!(select_layer(None, BodyStatus::default(), &set).is_none(), "no action");
        // A whole-body action is the body's clip, not a layer's.
        let swing = Some(("light_1", 0.15, 0.6));
        assert!(select_layer(swing, BodyStatus::default(), &set).is_none());
        assert_eq!(select_clip_for(swing, &moving(2.0), BodyStatus::default(), &set).clip, Some("swing_1"));
        assert!(select_layer(Some(("mystery", 0.1, 0.5)), BodyStatus::default(), &set).is_none(), "no clip for it");
        // The dead and the staggering show their own clips on the whole body.
        let drinking = Some(("drink", 0.5, 1.0));
        assert!(select_layer(drinking, BodyStatus { dead: true, ..default() }, &set).is_none());
        assert!(select_layer(drinking, BodyStatus { staggered: true, ..default() }, &set).is_none());
        // A mapping to a layer the set lacks plays on the whole body (the content check
        // reports it), so the picture shows something.
        let mut typo = set;
        typo.actions.insert("drink".into(), ActionClip::on("drink", "uper"));
        assert!(typo.action_shown("drink") == Some(("drink", None)) && !typo.is_layered("drink"));
        assert!(select_layer(drinking, BodyStatus::default(), &typo).is_none());
        assert_eq!(select_clip_for(drinking, &moving(2.0), BodyStatus::default(), &typo).clip, Some("drink"));
    }

    const TWO_ACTIONS: &str =
        r#"{ "light_1": (net_id: 1, phases: [("active", 0.3)]), "roll": (net_id: 2, phases: [("roll", 0.4)]) }"#;

    #[test]
    fn layers_are_checked_by_content() {
        let errors = build_with(
            TWO_ACTIONS,
            r#"{ "h": (source: "models/h.glb", clips: {"idle": (), "drink": ()},
                layers: {"upper": (root: "spine"), "arm": (root: ""), "head": (root: "head")},
                actions: {"light_1": (clip: "drink", layer: "uper"), "roll": (clip: "dring", layer: "upper")},
                tags: {"fire": (clip: "drink", layer: "head"), "ice": "drink"}) }"#,
        );
        assert_eq!(
            errors,
            [
                "content/h.animset.ron: animset `h`: layers.arm.root is empty; name the bone the layer starts at, \
                 e.g. `layers: {\"arm\": (root: \"spine\")}`",
                "content/h.animset.ron: animset `h`: actions.light_1.layer -> unknown layer `uper` (did you mean `upper`?); \
                 layers: arm, head, upper",
                "content/h.animset.ron: animset `h`: actions.roll -> unknown clip `dring` (did you mean `drink`?); \
                 clips: drink, idle",
            ]
        );
        // A set with no layers says how to make one.
        let errors = build_with(
            TWO_ACTIONS,
            r#"{ "h": (source: "models/h.glb", clips: {"idle": (), "drink": ()},
                actions: {"light_1": (clip: "drink", layer: "upper")}, tags: {"fire": (clip: "drink", layer: "")}) }"#,
        );
        assert_eq!(
            errors,
            [
                "content/h.animset.ron: animset `h`: actions.light_1.layer -> unknown layer `upper`; the set has no layers: \
                 name one in `layers`, e.g. `layers: {\"upper\": (root: \"spine\")}`",
                "content/h.animset.ron: animset `h`: tags.fire.layer -> unknown layer ``; the set has no layers: \
                 name one in `layers`, e.g. `layers: {\"upper\": (root: \"spine\")}`",
            ]
        );
    }

    #[test]
    fn a_misspelt_mapping_is_a_parse_error_that_says_what_it_wants() {
        let set = |mapping: &str| {
            format!(r#"{{ "h": (source: "models/h.glb", clips: {{"idle": ()}}, actions: {{"light_1": {mapping}}}) }}"#)
        };
        let errors = build_with(TWO_ACTIONS, &set(r#"(clip: "idle", lyer: "upper")"#));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert!(errors[0].contains("Unexpected field named `lyer`") && errors[0].contains("expected one of `clip`, `layer`, `ends_equip`, `turn`, or `blend_out`"), "{errors:#?}");
        let errors = build_with(TWO_ACTIONS, &set(r#"(layer: "upper")"#));
        assert!(errors.len() == 1 && errors[0].contains("missing field named `clip`"), "{errors:#?}");
        let errors = build_with(TWO_ACTIONS, &set("7"));
        assert!(errors.len() == 1 && errors[0].contains("a clip name"), "{errors:#?}");
        let errors = build_with(
            TWO_ACTIONS,
            r#"{ "h": (source: "models/h.glb", clips: {"idle": ()}, layers: {"upper": (root: "spine", blend: 1)}) }"#,
        );
        assert!(errors.len() == 1 && errors[0].contains("blend"), "a layer is only its root bone: {errors:#?}");
    }

    #[test]
    fn at_most_sixty_four_layers() {
        let layers: String = (0..=MAX_LAYERS).map(|i| format!(r#""l{i:02}": (root: "b{i}"), "#)).collect();
        let errors = build(&format!(r#"{{ "h": (source: "models/h.glb", clips: {{"idle": ()}}, layers: {{{layers}}}) }}"#));
        assert_eq!(errors, ["content/h.animset.ron: animset `h`: layers has 65 layers; at most 64"]);
    }

    #[test]
    fn the_layers_example_in_the_docs_is_valid_content() {
        let docs = include_str!("anim.rs");
        let start = docs.find("//! ## Layers").expect("the section");
        let start = docs[start..].find("//! {\n").expect("its example") + start;
        let end = docs[start..].find("//! }\n").expect("its end") + start;
        let body: String = docs[start..end + "//! }\n".len()].lines().map(|l| l.trim_start_matches("//!").to_owned() + "\n").collect();
        let errors = build_with(r#"{ "drink": (net_id: 1, phases: [("drink", 1.0)]) }"#, &body);
        assert!(errors.is_empty(), "{errors:#?}");
        let sets: BTreeMap<String, AnimSet> = ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
            .from_str(&body)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(sets["humanoid"].is_layered("drink"), "the example puts the drink on a layer");
    }
}

#[cfg(test)]
mod modes;
#[cfg(test)]
mod mounting;
