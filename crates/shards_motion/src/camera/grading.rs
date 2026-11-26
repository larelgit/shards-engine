//! Colour grading (F13): the camera's palette as content, changed by trigger volumes.
//!
//! - [`Grading`] is a grade: exposure, white balance (`temperature`, `tint`), hue, saturation,
//!   contrast and lift/gamma/gain, applied to the whole picture. It becomes Bevy's `ColorGrading` on
//!   the camera (shadows, midtones and highlights alike). It is the camera's own picture content
//!   (`picture.grading` of the `camera` kind, [`PictureTuning`](super::PictureTuning)), or a named one
//!   in the `grading` kind ([`GradingDef`], `*.grading.ron`) that a volume carries.
//! - A trigger volume with a [`GradingVolume`] (a level node's `grading=<id>`, with `volume = trigger`)
//!   blends the picture toward its grading while the local player is inside it (the player's body
//!   touches it; a game with no local player uses the camera's place), over the definition's `blend_s`,
//!   and back to the camera's own grading over the same time when the player leaves. The state is
//!   [`GradingBlend`], a resource that lives on the simulation's clock (`Time<Fixed>`: a pause freezes
//!   a blend and `step` moves it), so a headless game and a drawn one are in the same place of it.
//! - Picture only: nothing here is in a snapshot or a replay. The volume is read, never written; the
//!   tracking runs in `FixedLast` so it sees each tick's volumes exactly, whatever the frame rate.
//!
//! **How much of a grade is drawn depends on the camera.** A camera that draws in HDR (the picture's `bloom`, or
//! the game's own `Hdr`) is graded by Bevy's tone mapping pass, which draws all of the grade. A camera that does not
//! draw in HDR is graded in each material's shader, and Bevy applies only the `exposure` there: `temperature`,
//! `tint`, `hue_deg`, `saturation`, `contrast`, `lift`, `gamma` and `gain` are left out ([`Grading::hdr_only`]
//! names them). Turning a bloom on therefore draws the rest of a grade for the first time, and a grade tuned without
//! HDR can look nothing like it did. The `lights` query says which it is (`drawn:`). A camera whose tone mapping is
//! `None` shows none of a grade (the picture's `tone_mapping` doc says so).
//!
//! In HDR Bevy grades in this order: the white balance, then saturation and contrast, then lift, gamma and gain,
//! and the `exposure` last. Contrast pivots on 0.5 of the linear picture before the exposure, so a dark scene that the
//! grade's exposure lifts loses its darks below zero: with `contrast` above 1 it goes black. Such a scene keeps
//! `contrast` at 1, or is lit (or exposed by the camera's `Exposure`) instead.
//!
//! A game's own `ColorGrading` on a camera is
//! kept while the content says nothing and put back when the content stops saying anything; a blend
//! starts from the camera's content grading (or the neutral one), not from the game's own.

use ai_core::AiAppExt;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use shards_content::{ContentAppExt, ContentDb, ContentDef, Refs};
use shards_core::LocalPlayer;

use super::CameraTuning;
use crate::controller::MotionState;

/// A grade of the picture. The neutral grade (the default) changes nothing. Every field but `exposure` is drawn only
/// by a camera in HDR (the module says why).
///
/// ```ron
/// (temperature: 0.03, saturation: 0.8, contrast: 1.1)
/// ```
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default, deny_unknown_fields)]
pub struct Grading {
    /// Exposure in stops (EV): 1 is twice the light. `-10` to `10`.
    pub exposure: f32,
    /// White balance toward red (positive) or blue (negative), no change in brightness. Bevy's units: what is taken
    /// from the *x* of the white point in CIE 1931 (D65's is 0.3127), so small numbers are strong: 0.02 to 0.05 is a
    /// clear warm or cool cast, and near 0.3 the white point leaves the colour diagram (the picture loses its green and
    /// blue). The check allows `-1` to `1`. HDR only.
    pub temperature: f32,
    /// White balance toward magenta (positive) or green (negative): what is added to the *y* of the white point (D65's
    /// is 0.3290); 0.01 to 0.03 is a clear cast. The check allows `-1` to `1`. HDR only.
    pub tint: f32,
    /// Turns every hue round the colour wheel, degrees: `-180` to `180`. HDR only.
    pub hue_deg: f32,
    /// 1 as drawn, 0 grey, above 1 stronger colours: `0` to `4`. HDR only.
    pub saturation: f32,
    /// 1 as drawn, below 1 toward grey, above 1 away from it: `0` to `4`. It pivots on 0.5 of the linear picture,
    /// before the `exposure`: above 1 it blackens a dark scene that the exposure lifts. HDR only.
    pub contrast: f32,
    /// A fixed lift of the dark end (the offset of the ASC CDL): `-1` to `1`, 0 none. HDR only.
    pub lift: f32,
    /// The curve of the light (the power of the ASC CDL): `0.05` to `4`, 1 none. HDR only.
    pub gamma: f32,
    /// A multiplier of the light (the slope of the ASC CDL): `0` to `8`, 1 none. HDR only.
    pub gain: f32,
}

impl Default for Grading {
    fn default() -> Self {
        Self { exposure: 0.0, temperature: 0.0, tint: 0.0, hue_deg: 0.0, saturation: 1.0, contrast: 1.0, lift: 0.0, gamma: 1.0, gain: 1.0 }
    }
}

impl Grading {
    /// Each field of the grade, with its range: the one place that knows them.
    fn fields(&self) -> [(&'static str, f32, f32, f32); 9] {
        [
            ("exposure", self.exposure, -10.0, 10.0),
            ("temperature", self.temperature, -1.0, 1.0),
            ("tint", self.tint, -1.0, 1.0),
            ("hue_deg", self.hue_deg, -180.0, 180.0),
            ("saturation", self.saturation, 0.0, 4.0),
            ("contrast", self.contrast, 0.0, 4.0),
            ("lift", self.lift, -1.0, 1.0),
            ("gamma", self.gamma, 0.05, 4.0),
            ("gain", self.gain, 0.0, 8.0),
        ]
    }

    /// What is wrong with these numbers, one line each, naming `field.<name>`; empty when they are fine.
    pub fn problems(&self, field: &str) -> Vec<String> {
        self.fields()
            .into_iter()
            .filter(|(_, v, lo, hi)| !(v.is_finite() && (lo..=hi).contains(&v)))
            .map(|(name, v, lo, hi)| format!("{field}.{name}: from {lo} to {hi}, not {v}"))
            .collect()
    }

    /// The fields of this grade that only a camera drawing in HDR shows (every one but `exposure` that is not
    /// neutral), by name. Without HDR, Bevy grades in each material's shader and applies the exposure alone.
    pub fn hdr_only(&self) -> Vec<&'static str> {
        self.fields()
            .into_iter()
            .zip(Self::default().fields())
            .filter(|((name, value, ..), (_, neutral, ..))| *name != "exposure" && value != neutral)
            .map(|((name, ..), _)| name)
            .collect()
    }

    /// Whether this grade changes nothing.
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }

    /// The grade `t` of the way from `self` to `to` (`t` within 0 to 1, field by field).
    pub fn lerp(&self, to: &Grading, t: f32) -> Grading {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Grading {
            exposure: mix(self.exposure, to.exposure),
            temperature: mix(self.temperature, to.temperature),
            tint: mix(self.tint, to.tint),
            hue_deg: mix(self.hue_deg, to.hue_deg),
            saturation: mix(self.saturation, to.saturation),
            contrast: mix(self.contrast, to.contrast),
            lift: mix(self.lift, to.lift),
            gamma: mix(self.gamma, to.gamma),
            gain: mix(self.gain, to.gain),
        }
    }
}

/// The longest a blend can take (s).
pub const MAX_BLEND_S: f32 = 60.0;

/// A named grade a volume blends the picture toward: content kind `grading` (`*.grading.ron`, a map of
/// ids to definitions).
///
/// ```ron
/// { "rust": (blend_s: 1.5, grading: (temperature: 0.03, saturation: 0.7)) }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GradingDef {
    /// Seconds the picture takes to reach this grade when the player enters its volume, and to leave it
    /// when the player goes out: `0` to `60`. 0 is a cut.
    pub blend_s: f32,
    /// The grade.
    pub grading: Grading,
}

impl Default for GradingDef {
    fn default() -> Self {
        Self { blend_s: 1.0, grading: Grading::default() }
    }
}

impl ContentDef for GradingDef {
    const KIND: &'static str = "grading";
    // The picture's: a replay never reads it, and editing it never makes one diverge.
    const SIMULATION: bool = false;

    fn validate(&self, _id: &str, refs: &mut Refs) {
        if !(self.blend_s.is_finite() && (0.0..=MAX_BLEND_S).contains(&self.blend_s)) {
            refs.error(format!("blend_s: from 0 to {MAX_BLEND_S} seconds, not {}", self.blend_s));
        }
        for problem in self.grading.problems("grading") {
            refs.error(problem);
        }
    }
}

/// A trigger volume that grades the picture: while the local player is inside it, the picture blends
/// toward the `grading` definition `id`. A level node makes one with `grading=<id>` on a `volume =
/// trigger` node; a prefab with `"GradingVolume": (id: "rust")` on an entity with a trigger `Volume`
/// does too. Where volumes overlap, the one placed last (the highest collider id) wins.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
pub struct GradingVolume {
    /// The `grading` definition it blends toward.
    pub id: String,
}

/// Where the picture's grade is between the camera's own and a volume's. A resource of the picture
/// (no `AiId`, not in a snapshot, not in a replay), moved on the simulation's clock.
#[derive(Resource, Clone, Debug, Default)]
pub struct GradingBlend {
    /// The `grading` definition the picture is blending toward or holding; `None`: the camera's own grade.
    volume: Option<String>,
    /// The grade the blend started from.
    from: Grading,
    /// The simulation clock (s) it started at, and how long it takes.
    started: f64,
    duration: f64,
    /// The `blend_s` of the volume held now, which it takes to leave it.
    leave_s: f64,
    /// How many times the target changed.
    changes: u64,
}

/// A way to find a `grading` definition by id.
pub type Resolve<'a> = &'a dyn Fn(&str) -> Option<&'a GradingDef>;

impl GradingBlend {
    /// The definition the picture is blending toward or holding.
    pub fn volume(&self) -> Option<&str> {
        self.volume.as_deref()
    }

    /// How many times the target has changed since the game began.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// How far the blend has come at clock `now` (s): 0 to 1 in a straight line. 1 when it is done or
    /// there was none.
    pub fn progress(&self, now: f64) -> f32 {
        if self.duration <= 0.0 { 1.0 } else { ((now - self.started) / self.duration).clamp(0.0, 1.0) as f32 }
    }

    /// Whether the blend is under way at clock `now`.
    pub fn is_blending(&self, now: f64) -> bool {
        self.progress(now) < 1.0
    }

    /// The grade the picture shows at clock `now`, or `None` when it shows the camera's own (`base`, the
    /// camera content's `picture.grading`): no volume holds it and no blend is left. The blend eases
    /// (smoothstep) between the grade it started from and the volume's, or `base` (neutral without one).
    pub fn current(&self, now: f64, base: Option<&Grading>, resolve: Resolve) -> Option<Grading> {
        let target = self.volume.as_deref().and_then(resolve).map(|d| &d.grading);
        let t = self.progress(now);
        if target.is_none() && t >= 1.0 {
            return None;
        }
        let neutral = Grading::default();
        let to = target.or(base).unwrap_or(&neutral);
        Some(if t >= 1.0 { to.clone() } else { self.from.lerp(to, t * t * (3.0 - 2.0 * t)) })
    }

    /// Points the blend at the definition `volume` (`None`: the camera's own grade) from clock `now`,
    /// starting from what is shown now so nothing jumps. Entering takes the new volume's `blend_s`,
    /// leaving the held one's. Does nothing when it is the target already.
    pub fn retarget(&mut self, volume: Option<&str>, now: f64, base: Option<&Grading>, resolve: Resolve) {
        if self.volume.as_deref() == volume {
            return;
        }
        let shown = self.current(now, base, resolve).or_else(|| base.cloned()).unwrap_or_default();
        self.duration = match volume.and_then(resolve) {
            Some(def) => f64::from(def.blend_s),
            None => self.leave_s,
        };
        self.leave_s = volume.and_then(resolve).map_or(self.leave_s, |def| f64::from(def.blend_s));
        self.from = shown;
        self.started = now;
        self.volume = volume.map(str::to_owned);
        self.changes += 1;
    }
}

/// The clock the blend lives on (s): the simulation's, between ticks too. A pause stops it and a
/// `step` moves it on by the ticks it runs.
pub fn picture_clock(fixed: &Time<Fixed>) -> f64 {
    fixed.elapsed_secs_f64() + fixed.overstep().as_secs_f64()
}

/// The cameras that look through the world, for a game with no local player.
type WorldCameras<'w, 's> = Query<'w, 's, &'static Transform, Or<(With<super::ThirdPersonCamera>, With<super::FirstPersonCamera>)>>;

/// The volume the local player (else the camera) is in that grades the picture: the last of the
/// volumes it touches, in collider id order.
fn graded_volume<'a>(touching: impl Iterator<Item = Entity>, volumes: &'a Query<&GradingVolume>) -> Option<&'a str> {
    touching.filter_map(|e| volumes.get(e).ok()).map(|v| v.id.as_str()).last()
}

/// `FixedLast`: which grading volume the picture should be in after this tick, from the volumes the
/// local player's body touches (`MotionState::volumes`, collider id order); with no local player, the
/// volumes the camera stands in. It only reads the simulation.
#[allow(clippy::too_many_arguments, reason = "a Bevy system: one parameter per thing it reads")]
fn track_volumes(
    fixed: Res<Time<Fixed>>,
    db: Option<Res<ContentDb>>,
    tuning: Res<CameraTuning>,
    players: Query<&MotionState, With<LocalPlayer>>,
    cameras: WorldCameras,
    world: Option<Res<crate::physics::PhysicsWorld>>,
    volumes: Query<&GradingVolume>,
    mut blend: ResMut<GradingBlend>,
) {
    let Some(db) = db else { return };
    let resolve = |id: &str| db.peek::<GradingDef>(id);
    let wanted = match (players.iter().next(), cameras.iter().next(), world) {
        (Some(state), ..) => graded_volume(state.volumes.iter().copied(), &volumes),
        (None, Some(camera), Some(world)) => {
            let at: Vec<Entity> = world.volumes_at(camera.translation, None).into_iter().map(|(e, _)| e).collect();
            graded_volume(at.into_iter(), &volumes)
        }
        _ => None,
    };
    // A volume whose definition is gone grades nothing.
    let wanted = wanted.filter(|id| resolve(id).is_some());
    if blend.volume() != wanted {
        let base = tuning.picture.grading.as_ref();
        blend.retarget(wanted, fixed.elapsed_secs_f64(), base, &resolve);
    }
}

pub(super) fn plugin(app: &mut App) {
    app.register_type::<Grading>()
        .register_type::<GradingVolume>()
        // The picture's: a trigger marker has an `AiId`, and this component on it is not part of a replay's hash.
        .replay_ignore::<GradingVolume>()
        .add_content::<GradingDef>()
        .init_resource::<GradingBlend>()
        .add_systems(FixedLast, track_volumes);
    #[cfg(feature = "render")]
    app.add_systems(Update, render::apply_grading);
}

/// Whether the world cameras (3D, with a [`ThirdPersonCamera`](super::ThirdPersonCamera) or a
/// [`FirstPersonCamera`](super::FirstPersonCamera)) draw a grade in full: `Some(true)` when every one draws in HDR,
/// `Some(false)` when one does not (it draws only the grade's `exposure`, [`Grading::hdr_only`]), `None` without such
/// a camera or without the picture (a headless build).
pub fn grade_drawn_in_full(world: &mut World) -> Option<bool> {
    #[cfg(feature = "render")]
    {
        use bevy::camera::{Camera3d, Hdr};
        let mut cameras =
            world.query_filtered::<Has<Hdr>, (With<Camera3d>, Or<(With<super::ThirdPersonCamera>, With<super::FirstPersonCamera>)>)>();
        cameras.iter(world).fold(None, |all, hdr| Some(all.unwrap_or(true) && hdr))
    }
    #[cfg(not(feature = "render"))]
    {
        let _ = world;
        None
    }
}

#[cfg(feature = "render")]
mod render {
    use bevy::camera::Camera3d;
    use bevy::prelude::*;
    use bevy::render::view::{ColorGrading, ColorGradingGlobal, ColorGradingSection};
    use shards_content::ContentDb;

    use super::{picture_clock, Grading, GradingBlend, GradingDef};
    use crate::camera::{CameraTuning, FirstPersonCamera, ThirdPersonCamera};

    impl Grading {
        /// Bevy's grade: the same section for the shadows, the midtones and the highlights.
        pub fn bevy(&self) -> ColorGrading {
            let global = ColorGradingGlobal {
                exposure: self.exposure,
                temperature: self.temperature,
                tint: self.tint,
                hue: self.hue_deg.to_radians(),
                ..default()
            };
            let section = ColorGradingSection { saturation: self.saturation, contrast: self.contrast, gamma: self.gamma, gain: self.gain, lift: self.lift };
            ColorGrading::with_identical_sections(global, section)
        }
    }

    /// The grade a camera had of its own while the content's is in force, so that it can be put back.
    #[derive(Component, Clone, Debug)]
    pub struct GradingSet {
        own: Option<ColorGrading>,
    }

    /// The cameras a grade applies to, with what they have now.
    type Cameras<'w, 's> = Query<
        'w,
        's,
        (Entity, Option<&'static ColorGrading>, Option<&'static GradingSet>),
        (With<Camera3d>, Or<(With<ThirdPersonCamera>, With<FirstPersonCamera>)>),
    >;

    /// `Update`: the grade in force (the volume's blend, else the camera content's `picture.grading`)
    /// onto every 3D camera with a [`ThirdPersonCamera`] or a [`FirstPersonCamera`]; a camera gets its own grade back when none is.
    pub fn apply_grading(
        fixed: Res<Time<Fixed>>,
        tuning: Res<CameraTuning>,
        blend: Res<GradingBlend>,
        db: Option<Res<ContentDb>>,
        cameras: Cameras,
        mut commands: Commands,
    ) {
        let resolve = |id: &str| db.as_deref().and_then(|db| db.peek::<GradingDef>(id));
        let base = tuning.picture.grading.as_ref();
        let want = blend.current(picture_clock(&fixed), base, &resolve).or_else(|| base.cloned()).map(|g| g.bevy());
        for (entity, grading, set) in &cameras {
            match (&want, set) {
                (Some(want), set) => {
                    if set.is_none() {
                        commands.entity(entity).insert(GradingSet { own: grading.cloned() });
                    }
                    if grading != Some(want) {
                        commands.entity(entity).insert(want.clone());
                    }
                }
                (None, Some(set)) => {
                    let mut camera = commands.entity(entity);
                    match &set.own {
                        Some(own) => camera.insert(own.clone()),
                        None => camera.remove::<ColorGrading>(),
                    };
                    camera.remove::<GradingSet>();
                }
                (None, None) => {}
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::time::Duration;

        use shards_content::db::{ContentKinds, Source};

        use super::*;
        use crate::camera::PictureTuning;

        fn app() -> App {
            let mut app = App::new();
            app.init_resource::<CameraTuning>().init_resource::<GradingBlend>().init_resource::<Time<Fixed>>().add_systems(Update, apply_grading);
            app
        }

        fn warm() -> Grading {
            Grading { temperature: 0.4, saturation: 0.5, ..default() }
        }

        fn set(app: &mut App, grading: Option<Grading>) {
            app.world_mut().resource_mut::<CameraTuning>().picture = PictureTuning { grading, ..default() };
            app.update();
            app.update();
        }

        #[test]
        fn a_camera_with_no_grade_in_the_content_keeps_the_game_s_own() {
            let mut app = app();
            let own = ColorGrading::with_identical_sections(ColorGradingGlobal { exposure: 0.5, ..default() }, ColorGradingSection::default());
            let camera = app.world_mut().spawn((Camera3d::default(), ThirdPersonCamera::default(), own.clone())).id();
            let plain = app.world_mut().spawn((Camera3d::default(), ThirdPersonCamera::default())).id();
            app.update();
            assert_eq!(app.world().get::<ColorGrading>(camera), Some(&own));
            assert!(app.world().get::<ColorGrading>(plain).is_none() && app.world().get::<GradingSet>(plain).is_none());
        }

        #[test]
        fn the_contents_grade_reaches_every_kind_of_world_camera_and_leaving_it_gives_each_its_own_back() {
            let mut app = app();
            let own = ColorGrading::with_identical_sections(ColorGradingGlobal { exposure: 0.5, ..default() }, ColorGradingSection::default());
            let orbit = app.world_mut().spawn((Camera3d::default(), ThirdPersonCamera::default(), own.clone())).id();
            let first = app.world_mut().spawn((Camera3d::default(), FirstPersonCamera::default())).id();
            let other = app.world_mut().spawn(Camera3d::default()).id();
            app.update();
            set(&mut app, Some(warm()));
            assert_eq!(app.world().get::<ColorGrading>(orbit), Some(&warm().bevy()), "a third-person camera");
            assert_eq!(app.world().get::<ColorGrading>(first), Some(&warm().bevy()), "a first-person camera");
            assert!(app.world().get::<ColorGrading>(other).is_none(), "a camera with neither is the game's own business");

            // Another grade replaces it, and none gives the cameras back what they had.
            set(&mut app, Some(Grading { exposure: 1.0, ..default() }));
            assert_eq!(app.world().get::<ColorGrading>(first).map(|g| g.global.exposure), Some(1.0));
            set(&mut app, None);
            assert_eq!(app.world().get::<ColorGrading>(orbit), Some(&own), "its own grade back");
            assert!(app.world().get::<ColorGrading>(first).is_none() && app.world().get::<GradingSet>(first).is_none(), "and none where it had none");
            // A camera that appears later gets it too.
            set(&mut app, Some(warm()));
            let late = app.world_mut().spawn((Camera3d::default(), FirstPersonCamera::default())).id();
            app.update();
            app.update();
            assert_eq!(app.world().get::<ColorGrading>(late), Some(&warm().bevy()));
        }

        #[test]
        fn a_blend_toward_a_volume_moves_the_cameras_grade_on_the_simulations_clock() {
            let mut app = app();
            let mut kinds = ContentKinds::default();
            kinds.register::<GradingDef>();
            let source = Source { path: "content/g.grading.ron".into(), text: r#"{"rust": (blend_s: 2.0, grading: (temperature: 0.4))}"#.into() };
            app.insert_resource(ContentDb::build(&kinds, &[source]));
            let camera = app.world_mut().spawn((Camera3d::default(), FirstPersonCamera::default())).id();
            app.update();
            assert!(app.world().get::<ColorGrading>(camera).is_none(), "no volume, no content grade: nothing is written");
            let db = app.world().resource::<ContentDb>().clone();
            app.world_mut().resource_mut::<GradingBlend>().retarget(Some("rust"), 0.0, None, &|id| db.peek::<GradingDef>(id));
            app.update();
            app.update();
            assert_eq!(app.world().get::<ColorGrading>(camera).map(|g| g.global.temperature), Some(0.0), "it starts where the picture was");
            app.world_mut().resource_mut::<Time<Fixed>>().advance_by(Duration::from_secs(1));
            app.update();
            app.update();
            assert!((app.world().get::<ColorGrading>(camera).unwrap().global.temperature - 0.2).abs() < 1e-5, "half way, eased");
            app.world_mut().resource_mut::<Time<Fixed>>().advance_by(Duration::from_secs(2));
            app.update();
            app.update();
            assert_eq!(app.world().get::<ColorGrading>(camera).map(|g| g.global.temperature), Some(0.4));
            // Out again: back to the camera's own, which is no grade at all.
            let db = app.world().resource::<ContentDb>().clone();
            let now = picture_clock(app.world().resource::<Time<Fixed>>());
            app.world_mut().resource_mut::<GradingBlend>().retarget(None, now, None, &|id| db.peek::<GradingDef>(id));
            app.world_mut().resource_mut::<Time<Fixed>>().advance_by(Duration::from_secs(3));
            app.update();
            app.update();
            assert!(app.world().get::<ColorGrading>(camera).is_none() && app.world().get::<GradingSet>(camera).is_none());
        }
    }
}

#[cfg(feature = "render")]
pub use render::GradingSet;

#[cfg(test)]
mod tests {
    use super::*;

    fn rust() -> GradingDef {
        GradingDef { blend_s: 2.0, grading: Grading { temperature: 0.4, saturation: 0.5, ..default() } }
    }

    fn night() -> GradingDef {
        GradingDef { blend_s: 4.0, grading: Grading { temperature: -0.4, exposure: -1.0, ..default() } }
    }

    fn defs(id: &str) -> Option<&'static GradingDef> {
        use std::sync::OnceLock;
        static DEFS: OnceLock<(GradingDef, GradingDef)> = OnceLock::new();
        let (rust_def, night_def) = DEFS.get_or_init(|| (rust(), night()));
        match id {
            "rust" => Some(rust_def),
            "night" => Some(night_def),
            _ => None,
        }
    }

    #[test]
    fn the_neutral_grade_is_the_default_and_bad_numbers_are_named() {
        assert!(Grading::default().is_neutral());
        assert!(Grading::default().problems("grading").is_empty());
        let bad = Grading { temperature: 3.0, gamma: 0.0, saturation: f32::NAN, ..default() };
        assert_eq!(
            bad.problems("picture.grading"),
            [
                "picture.grading.temperature: from -1 to 1, not 3",
                "picture.grading.saturation: from 0 to 4, not NaN",
                "picture.grading.gamma: from 0.05 to 4, not 0"
            ]
        );
        let read: GradingDef = ron::from_str("(blend_s: 1.5, grading: (temperature: 0.35, saturation: 0.7))").unwrap();
        assert_eq!((read.blend_s, read.grading.temperature, read.grading.contrast), (1.5, 0.35, 1.0), "a field left out is the neutral one");
        assert!(ron::from_str::<GradingDef>("(blend: 1.0)").is_err(), "an unknown field is an error");
    }

    #[test]
    fn every_field_but_the_exposure_is_drawn_only_in_hdr() {
        assert!(Grading::default().hdr_only().is_empty());
        assert!(Grading { exposure: 1.7, ..default() }.hdr_only().is_empty(), "an exposure is drawn by every camera");
        let grade = Grading { exposure: 1.7, temperature: 0.03, contrast: 1.2, gain: 1.1, ..default() };
        assert_eq!(grade.hdr_only(), ["temperature", "contrast", "gain"]);
    }

    #[test]
    fn a_blend_eases_between_the_grades_on_the_clock_and_a_cut_is_instant() {
        let mut blend = GradingBlend::default();
        assert_eq!(blend.current(0.0, None, &defs), None, "no volume: the camera's own grade");
        blend.retarget(Some("rust"), 10.0, None, &defs);
        assert_eq!((blend.volume(), blend.progress(10.0), blend.progress(11.0), blend.progress(12.0), blend.progress(99.0)), (Some("rust"), 0.0, 0.5, 1.0, 1.0));
        let start = blend.current(10.0, None, &defs).unwrap();
        assert!(start.is_neutral(), "it starts from the neutral grade: {start:?}");
        let half = blend.current(11.0, None, &defs).unwrap();
        assert!((half.temperature - 0.2).abs() < 1e-6 && (half.saturation - 0.75).abs() < 1e-6, "half way: {half:?}");
        assert_eq!(blend.current(13.0, None, &defs).unwrap(), rust().grading, "held");
        assert!(blend.is_blending(11.0) && !blend.is_blending(12.0));
        // Leaving takes the held volume's time, and goes back to nothing.
        blend.retarget(None, 20.0, None, &defs);
        assert!(blend.current(21.0, None, &defs).unwrap().temperature > 0.0, "still blending out");
        assert_eq!(blend.current(22.0, None, &defs), None, "and then the camera's own grade");
        // The same target again changes nothing.
        let changes = blend.changes();
        blend.retarget(None, 30.0, None, &defs);
        assert_eq!(blend.changes(), changes);
        // A zero blend is a cut.
        let cut = GradingDef { blend_s: 0.0, ..rust() };
        let resolve = |id: &str| (id == "cut").then_some(&cut);
        let mut blend = GradingBlend::default();
        blend.retarget(Some("cut"), 5.0, None, &resolve);
        assert_eq!(blend.current(5.0, None, &resolve).unwrap(), cut.grading);
    }

    #[test]
    fn a_blend_that_changes_course_starts_from_what_is_shown() {
        let mut blend = GradingBlend::default();
        blend.retarget(Some("rust"), 0.0, None, &defs);
        let mid = blend.current(1.0, None, &defs).unwrap();
        blend.retarget(Some("night"), 1.0, None, &defs);
        assert_eq!(blend.current(1.0, None, &defs).unwrap(), mid, "no jump when the target changes");
        assert_eq!(blend.progress(3.0), 0.5, "the new volume's own 4 s");
        // Out of a volume whose blend is 4 s takes 4 s, and a base grade is what it returns to.
        let base = Grading { exposure: 0.5, ..default() };
        blend.retarget(None, 10.0, Some(&base), &defs);
        let back = blend.current(12.0, Some(&base), &defs).unwrap();
        assert!((back.exposure + 0.25).abs() < 1e-6, "half way from the volume's -1 to the base's 0.5: {back:?}");
        assert_eq!(blend.current(14.0, Some(&base), &defs), None, "and then the camera's own, which is the base");
        // A definition that disappears from the content (a hot reload) lets the picture go back to the camera's own.
        let mut held = GradingBlend::default();
        held.retarget(Some("rust"), 0.0, None, &defs);
        assert!(held.current(5.0, None, &defs).is_some());
        let gone = |_: &str| None;
        assert_eq!(held.current(5.0, None, &gone), None);
    }

    #[test]
    fn the_players_volumes_pick_the_last_graded_one() {
        let mut app = App::new();
        app.init_resource::<GradingBlend>().init_resource::<CameraTuning>().init_resource::<Time<Fixed>>();
        app.add_systems(FixedLast, track_volumes);
        let kinds = {
            let mut kinds = shards_content::db::ContentKinds::default();
            kinds.register::<GradingDef>();
            kinds
        };
        let source = shards_content::db::Source {
            path: "content/world/g.grading.ron".into(),
            text: r#"{"rust": (blend_s: 2.0, grading: (temperature: 0.4)), "night": (blend_s: 1.0, grading: (exposure: -1.0))}"#.into(),
        };
        app.insert_resource(ContentDb::build(&kinds, &[source]));
        let rusty = app.world_mut().spawn(GradingVolume { id: "rust".into() }).id();
        let dark = app.world_mut().spawn(GradingVolume { id: "night".into() }).id();
        let plain = app.world_mut().spawn_empty().id();
        let missing = app.world_mut().spawn(GradingVolume { id: "gone".into() }).id();
        let player = app.world_mut().spawn((LocalPlayer, MotionState::default())).id();
        let tick = |app: &mut App, touching: Vec<Entity>| {
            app.world_mut().get_mut::<MotionState>(player).unwrap().volumes = touching;
            app.world_mut().resource_mut::<Time<Fixed>>().advance_by(std::time::Duration::from_millis(500));
            app.world_mut().run_schedule(FixedLast);
        };
        tick(&mut app, vec![plain]);
        assert_eq!(app.world().resource::<GradingBlend>().volume(), None, "a volume with no grading is nothing");
        tick(&mut app, vec![rusty, plain]);
        assert_eq!(app.world().resource::<GradingBlend>().volume(), Some("rust"));
        tick(&mut app, vec![rusty, dark, plain]);
        assert_eq!(app.world().resource::<GradingBlend>().volume(), Some("night"), "the one placed last wins");
        tick(&mut app, vec![missing]);
        assert_eq!(app.world().resource::<GradingBlend>().volume(), None, "a volume whose definition is gone grades nothing");
        tick(&mut app, vec![]);
        assert_eq!(app.world().resource::<GradingBlend>().volume(), None);
    }
}
