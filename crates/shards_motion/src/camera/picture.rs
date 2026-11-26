//! The picture a camera draws ([`PictureTuning`], the `picture` of the `camera` content): its tone
//! mapping, its bloom and its screen-space ambient occlusion. All are left out by default, and then a
//! camera keeps what the game spawned it with: a game without a `picture` sees no change.
//!
//! Bloom needs the camera to draw in HDR (Bevy's `Hdr`), which moves the tone mapping from each
//! material's shader to a pass over the whole picture: the colours the materials compute are the
//! same, but a colour brighter than white (an effect's `intensity` above 1) now reaches the tone
//! mapping and the bloom instead of stopping at full. While the content gives a camera a bloom it
//! draws in HDR; taking the bloom out of the content gives the camera back what it had.
//!
//! SSAO ([`SsaoTuning`], Bevy's `ScreenSpaceAmbientOcclusion`) darkens the light that comes from no one
//! direction (an ambient light, an irradiance volume, an environment map) in creases and where things
//! meet, from the depth and normals of what is on screen; it never darkens a point, spot or directional
//! light, nor a lightmap's light (which has its occlusion baked in). It needs a depth and a normal prepass
//! and a camera without MSAA: while the content gives a camera SSAO, the camera gets the prepasses it
//! lacks (again, if another module takes one away) and `Msaa::Off`, and taking SSAO out of the content
//! gives it back its own MSAA and drops the prepasses it did not have. The view model's camera
//! (`shards_action`) copies the world camera's MSAA, so the two still share one main texture; it gets no
//! SSAO of its own (the arms are drawn over the occluded world).
//!
//! Only the picture changes: the `camera` kind is not simulation content, and nothing here is in a
//! snapshot or a replay.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::grading::Grading;

/// How a camera turns the light it draws into the picture's colours: Bevy's tone mapping operators,
/// by name. `TonyMcMapface` is Bevy's default.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToneMapping {
    /// None at all: no colour grading, exposure or dither either.
    None,
    /// The identity curve: exposure, colour grading and dither still apply.
    Linear,
    Reinhard,
    ReinhardLuminance,
    AcesFitted,
    AgX,
    SomewhatBoringDisplayTransform,
    TonyMcMapface,
    BlenderFilmic,
    KhronosPbrNeutral,
}

/// How a bloom's glow joins the picture.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BloomComposite {
    /// The glow takes its share of the light from the picture (Bevy's natural bloom).
    #[default]
    EnergyConserving,
    /// The glow is added on top: with a `threshold`, only what passes it glows and the rest is left as it was.
    Additive,
}

/// A bloom: light brighter than `threshold` spreads into a glow around it (Bevy's `Bloom`, whose
/// defaults these are: its natural bloom).
///
/// ```ron
/// (intensity: 0.3, threshold: 1.2, composite: Additive)
/// ```
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default, deny_unknown_fields)]
pub struct BloomTuning {
    /// How much of the light scatters (energy-conserving: 0 none to 1 all) or how much glow is added (additive).
    pub intensity: f32,
    /// How much more the widest glow takes (a low shelf): 0 to 1.
    pub low_frequency_boost: f32,
    /// How far that boost reaches into the narrower glows: 0 to 1.
    pub low_frequency_boost_curvature: f32,
    /// Above 0 and at most 1: lower cuts the narrowest glows (1 keeps them all).
    pub high_pass_frequency: f32,
    /// Light below this (linear, the brightest channel) does not glow: 0 everything glows a little; above 1 only what
    /// is brighter than white (an effect's `intensity`, an emissive surface).
    pub threshold: f32,
    /// 0 a hard edge at the threshold, up to 1 a soft one.
    pub threshold_softness: f32,
    pub composite: BloomComposite,
}

impl Default for BloomTuning {
    fn default() -> Self {
        Self {
            intensity: 0.15,
            low_frequency_boost: 0.7,
            low_frequency_boost_curvature: 0.95,
            high_pass_frequency: 1.0,
            threshold: 0.0,
            threshold_softness: 0.0,
            composite: BloomComposite::EnergyConserving,
        }
    }
}

/// How many samples SSAO takes for a pixel (Bevy's quality levels): more is smoother and dearer.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SsaoQuality {
    /// 4 samples a pixel.
    Low,
    /// 8.
    Medium,
    /// 18 (Bevy's default).
    #[default]
    High,
    /// 54.
    Ultra,
}

/// Screen-space ambient occlusion (Bevy's `ScreenSpaceAmbientOcclusion`, whose defaults these are).
///
/// ```ron
/// (quality: Medium, radius: 0.5, thickness: 0.25)
/// ```
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default, deny_unknown_fields)]
pub struct SsaoTuning {
    pub quality: SsaoQuality,
    /// How far around a pixel the occluders are looked for (m, above 0): wider darkens broader corners, with more
    /// haloes and noise.
    pub radius: f32,
    /// How thick everything on screen is taken to be (m, above 0): a ray that passes behind something closer than
    /// this is occluded by it.
    pub thickness: f32,
}

impl Default for SsaoTuning {
    fn default() -> Self {
        Self { quality: SsaoQuality::High, radius: 0.5 * 1.457, thickness: 0.25 }
    }
}

/// The picture a camera draws: each part left out leaves the camera as the game made it.
///
/// ```ron
/// { "default": (picture: (tone_mapping: Some(AgX), bloom: Some((intensity: 0.3, threshold: 1.2, composite: Additive)))) }
/// ```
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[reflect(Default)]
#[serde(default, deny_unknown_fields)]
pub struct PictureTuning {
    /// The tone mapping; `None` keeps the camera's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone_mapping: Option<ToneMapping>,
    /// A bloom, drawn in HDR; `None` keeps the camera's own (none, unless the game gave it one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bloom: Option<BloomTuning>,
    /// A colour grade of the whole picture (F13, [`Grading`]); `None` keeps the camera's own. A trigger volume with a
    /// `grading` blends the picture away from it and back (`GradingVolume`). Bevy grades just before tone mapping, so a
    /// camera whose `tone_mapping` is `None` shows none of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grading: Option<Grading>,
    /// Screen-space ambient occlusion ([`SsaoTuning`]); `None` keeps the camera's own (none, unless the game gave it
    /// one). It turns the camera's MSAA off while it is on (Bevy's SSAO needs that) and adds a depth and a normal
    /// prepass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssao: Option<SsaoTuning>,
}

impl PictureTuning {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// What is wrong with these numbers, one line each; empty when they are fine.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(grading) = &self.grading {
            out.extend(grading.problems("picture.grading"));
        }
        if let Some(ssao) = &self.ssao {
            for (name, v) in [("radius", ssao.radius), ("thickness", ssao.thickness)] {
                if !(v > 0.0 && v.is_finite()) {
                    out.push(format!("picture.ssao.{name}: above 0, not {v}"));
                }
            }
        }
        let Some(bloom) = &self.bloom else { return out };
        let at_least_zero = [
            ("intensity", bloom.intensity),
            ("low_frequency_boost", bloom.low_frequency_boost),
            ("low_frequency_boost_curvature", bloom.low_frequency_boost_curvature),
            ("threshold", bloom.threshold),
        ];
        for (name, v) in at_least_zero {
            if !(v >= 0.0 && v.is_finite()) {
                out.push(format!("picture.bloom.{name}: 0 or more, not {v}"));
            }
        }
        if !(bloom.high_pass_frequency > 0.0 && bloom.high_pass_frequency <= 1.0) {
            out.push(format!("picture.bloom.high_pass_frequency: above 0 and at most 1, not {}", bloom.high_pass_frequency));
        }
        if !(0.0..=1.0).contains(&bloom.threshold_softness) {
            out.push(format!("picture.bloom.threshold_softness: 0 to 1, not {}", bloom.threshold_softness));
        }
        out
    }
}

#[cfg(feature = "render")]
pub(super) use render::apply_picture;

#[cfg(feature = "render")]
mod render {
    use bevy::camera::Hdr;
    use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
    use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel};
    use bevy::post_process::bloom::{Bloom, BloomCompositeMode, BloomPrefilter};
    use bevy::prelude::*;
    use bevy::render::view::{Msaa, Tonemapping};

    use super::{BloomComposite, BloomTuning, SsaoQuality, SsaoTuning, ToneMapping};
    use crate::camera::{CameraTuning, FirstPersonCamera, ThirdPersonCamera};

    impl ToneMapping {
        pub fn bevy(self) -> Tonemapping {
            match self {
                ToneMapping::None => Tonemapping::None,
                ToneMapping::Linear => Tonemapping::Linear,
                ToneMapping::Reinhard => Tonemapping::Reinhard,
                ToneMapping::ReinhardLuminance => Tonemapping::ReinhardLuminance,
                ToneMapping::AcesFitted => Tonemapping::AcesFitted,
                ToneMapping::AgX => Tonemapping::AgX,
                ToneMapping::SomewhatBoringDisplayTransform => Tonemapping::SomewhatBoringDisplayTransform,
                ToneMapping::TonyMcMapface => Tonemapping::TonyMcMapface,
                ToneMapping::BlenderFilmic => Tonemapping::BlenderFilmic,
                ToneMapping::KhronosPbrNeutral => Tonemapping::KhronosPbrNeutral,
            }
        }
    }

    impl BloomTuning {
        pub fn bevy(&self) -> Bloom {
            Bloom {
                intensity: self.intensity,
                low_frequency_boost: self.low_frequency_boost,
                low_frequency_boost_curvature: self.low_frequency_boost_curvature,
                high_pass_frequency: self.high_pass_frequency,
                prefilter: BloomPrefilter { threshold: self.threshold, threshold_softness: self.threshold_softness },
                composite_mode: match self.composite {
                    BloomComposite::EnergyConserving => BloomCompositeMode::EnergyConserving,
                    BloomComposite::Additive => BloomCompositeMode::Additive,
                },
                ..Bloom::NATURAL
            }
        }
    }

    impl SsaoTuning {
        pub fn bevy(&self) -> ScreenSpaceAmbientOcclusion {
            ScreenSpaceAmbientOcclusion {
                quality_level: match self.quality {
                    SsaoQuality::Low => ScreenSpaceAmbientOcclusionQualityLevel::Low,
                    SsaoQuality::Medium => ScreenSpaceAmbientOcclusionQualityLevel::Medium,
                    SsaoQuality::High => ScreenSpaceAmbientOcclusionQualityLevel::High,
                    SsaoQuality::Ultra => ScreenSpaceAmbientOcclusionQualityLevel::Ultra,
                },
                radius: self.radius,
                constant_object_thickness: self.thickness,
            }
        }
    }

    /// What the content changed on a camera, kept so that taking it out of the content gives the
    /// camera back what it had.
    #[derive(Component, Clone, Default)]
    pub struct PictureSet {
        /// The camera's own tone mapping while the content's is in force.
        tone_mapping: Option<Tonemapping>,
        /// The camera's own bloom (or none) while the content's is in force.
        bloom: Option<Option<Bloom>>,
        /// Whether the content's bloom made the camera draw in HDR (it did not before).
        hdr_added: bool,
        /// The camera's own SSAO (or none) and MSAA (or none) while the content's SSAO is in force.
        ssao: Option<SsaoOwn>,
    }

    /// What a camera had before the content gave it SSAO.
    #[derive(Clone, Default)]
    struct SsaoOwn {
        ssao: Option<ScreenSpaceAmbientOcclusion>,
        msaa: Option<Msaa>,
        /// The prepasses the content's SSAO added (the camera did not have them).
        depth_added: bool,
        normal_added: bool,
    }

    /// The cameras that appeared since the last frame.
    type NewCameras<'w, 's> = Query<'w, 's, (), Or<(Added<ThirdPersonCamera>, Added<FirstPersonCamera>)>>;

    /// What a camera's SSAO needs: its prepasses and no MSAA.
    type SsaoNeeds = (Option<&'static ScreenSpaceAmbientOcclusion>, Option<&'static Msaa>, Has<DepthPrepass>, Has<NormalPrepass>);

    /// The cameras a picture applies to, with what they have now.
    type Cameras<'w, 's> = Query<
        'w,
        's,
        (Entity, Option<&'static Tonemapping>, Option<&'static Bloom>, Has<Hdr>, Option<&'static PictureSet>, SsaoNeeds),
        (With<Camera3d>, Or<(With<ThirdPersonCamera>, With<FirstPersonCamera>)>),
    >;

    /// The cameras the content gave SSAO that lost what it needs since (another module took a prepass away, or
    /// something turned MSAA back on).
    type SsaoBroken<'w, 's> = Query<
        'w,
        's,
        (Option<&'static Msaa>, Has<DepthPrepass>, Has<NormalPrepass>, &'static PictureSet),
        (With<Camera3d>, Or<(With<ThirdPersonCamera>, With<FirstPersonCamera>)>),
    >;

    fn ssao_broken(cameras: &SsaoBroken) -> bool {
        cameras.iter().any(|(msaa, depth, normal, set)| set.ssao.is_some() && (!depth || !normal || msaa.is_some_and(|m| *m != Msaa::Off)))
    }

    /// `Update`: the `picture` of the camera tuning onto every 3D camera with a
    /// [`ThirdPersonCamera`] or [`FirstPersonCamera`], when the tuning changes and when such a camera appears.
    pub fn apply_picture(
        tuning: Res<CameraTuning>,
        added: NewCameras,
        cameras: Cameras,
        broken: SsaoBroken,
        mut commands: Commands,
    ) {
        if !tuning.is_changed() && added.is_empty() && !ssao_broken(&broken) {
            return;
        }
        let picture = &tuning.picture;
        for (entity, tone_mapping, bloom, hdr, set, (ssao, msaa, depth, normal)) in &cameras {
            let mut set = set.cloned().unwrap_or_default();
            let mut camera = commands.entity(entity);
            match (picture.tone_mapping, set.tone_mapping) {
                (Some(want), _) => {
                    set.tone_mapping.get_or_insert(tone_mapping.copied().unwrap_or_default());
                    if tone_mapping != Some(&want.bevy()) {
                        camera.insert(want.bevy());
                    }
                }
                (None, Some(own)) => {
                    camera.insert(own);
                    set.tone_mapping = None;
                }
                (None, None) => {}
            }
            match (&picture.bloom, set.bloom.take()) {
                (Some(want), own) => {
                    if own.is_none() {
                        set.hdr_added = !hdr;
                    }
                    set.bloom = Some(own.unwrap_or_else(|| bloom.cloned()));
                    camera.insert(want.bevy());
                }
                (None, Some(own)) => {
                    match own {
                        Some(own) => {
                            camera.insert(own);
                        }
                        None => {
                            camera.remove::<Bloom>();
                        }
                    }
                    if set.hdr_added {
                        camera.remove::<Hdr>();
                    }
                    set.hdr_added = false;
                }
                (None, None) => {}
            }
            match (&picture.ssao, set.ssao.take()) {
                (Some(want), own) => {
                    let mut own = own.unwrap_or_else(|| SsaoOwn { ssao: ssao.cloned(), msaa: msaa.copied(), ..default() });
                    // The prepasses go in before the SSAO, whose own required ones would hide what the camera lacked.
                    if !depth {
                        camera.insert(DepthPrepass);
                        own.depth_added = true;
                    }
                    if !normal {
                        camera.insert(NormalPrepass);
                        own.normal_added = true;
                    }
                    if msaa != Some(&Msaa::Off) {
                        camera.insert(Msaa::Off);
                    }
                    let want = want.bevy();
                    if ssao != Some(&want) {
                        camera.insert(want);
                    }
                    set.ssao = Some(own);
                }
                (None, Some(own)) => {
                    match own.ssao {
                        Some(theirs) => {
                            camera.insert(theirs);
                        }
                        None => {
                            camera.remove::<ScreenSpaceAmbientOcclusion>();
                        }
                    }
                    if own.depth_added {
                        camera.remove::<DepthPrepass>();
                    }
                    if own.normal_added {
                        camera.remove::<NormalPrepass>();
                    }
                    match own.msaa {
                        Some(msaa) => {
                            camera.insert(msaa);
                        }
                        None => {
                            camera.remove::<Msaa>();
                        }
                    }
                }
                (None, None) => {}
            }
            if set.tone_mapping.is_none() && set.bloom.is_none() && set.ssao.is_none() {
                camera.remove::<PictureSet>();
            } else {
                camera.insert(set);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::camera::PictureTuning;

        fn app() -> (App, Entity) {
            let mut app = App::new();
            app.init_resource::<CameraTuning>().add_systems(Update, apply_picture);
            let camera = app.world_mut().spawn((Camera3d::default(), ThirdPersonCamera::default(), Tonemapping::Reinhard)).id();
            app.update();
            (app, camera)
        }

        fn set(app: &mut App, picture: PictureTuning) {
            app.world_mut().resource_mut::<CameraTuning>().picture = picture;
            app.update();
        }

        #[test]
        fn a_camera_without_a_picture_keeps_what_the_game_gave_it() {
            let (app, camera) = app();
            let world = app.world();
            assert_eq!(world.get::<Tonemapping>(camera), Some(&Tonemapping::Reinhard));
            assert!(world.get::<Bloom>(camera).is_none() && world.get::<Hdr>(camera).is_none() && world.get::<PictureSet>(camera).is_none());
        }

        #[test]
        fn the_contents_bloom_draws_in_hdr_and_taking_it_out_gives_the_camera_back() {
            let (mut app, camera) = app();
            let bloom = BloomTuning { intensity: 0.4, threshold: 1.2, threshold_softness: 0.1, composite: BloomComposite::Additive, ..default() };
            set(&mut app, PictureTuning { tone_mapping: Some(ToneMapping::AgX), bloom: Some(bloom), ..default() });
            let world = app.world();
            let drawn = world.get::<Bloom>(camera).expect("a bloom");
            assert_eq!((drawn.intensity, drawn.prefilter.threshold, drawn.prefilter.threshold_softness), (0.4, 1.2, 0.1));
            assert_eq!((drawn.composite_mode, drawn.low_frequency_boost, drawn.high_pass_frequency), (BloomCompositeMode::Additive, 0.7, 1.0));
            assert!(world.get::<Hdr>(camera).is_some(), "a bloom draws in HDR");
            assert_eq!(world.get::<Tonemapping>(camera), Some(&Tonemapping::AgX));

            set(&mut app, PictureTuning::default());
            let world = app.world();
            assert_eq!(world.get::<Tonemapping>(camera), Some(&Tonemapping::Reinhard), "its own tone mapping back");
            assert!(world.get::<Bloom>(camera).is_none() && world.get::<Hdr>(camera).is_none(), "no bloom, and no HDR it did not have");
            assert!(world.get::<PictureSet>(camera).is_none());
        }

        #[test]
        fn a_camera_the_game_drew_in_hdr_with_its_own_bloom_gets_them_back() {
            let (mut app, camera) = app();
            let own = Bloom { intensity: 0.02, ..Bloom::OLD_SCHOOL };
            app.world_mut().entity_mut(camera).insert(own.clone());
            app.update();
            set(&mut app, PictureTuning { bloom: Some(BloomTuning::default()), ..default() });
            assert_eq!(app.world().get::<Bloom>(camera).unwrap().intensity, 0.15, "the content's bloom in force");
            set(&mut app, PictureTuning { bloom: Some(BloomTuning { intensity: 0.5, ..default() }), ..default() });
            assert_eq!(app.world().get::<Bloom>(camera).unwrap().intensity, 0.5, "a changed bloom replaces it");
            set(&mut app, PictureTuning::default());
            let world = app.world();
            assert_eq!(world.get::<Bloom>(camera).map(|b| b.intensity), Some(0.02), "the game's own bloom back");
            assert!(world.get::<Hdr>(camera).is_some(), "and the HDR its bloom needs");
            assert_eq!(world.get::<Tonemapping>(camera), Some(&Tonemapping::Reinhard));
        }

        #[test]
        fn the_contents_ssao_turns_msaa_off_adds_prepasses_and_gives_them_back() {
            let (mut app, camera) = app();
            app.world_mut().entity_mut(camera).insert(Msaa::Sample4);
            app.update();
            let ssao = SsaoTuning { quality: SsaoQuality::Medium, radius: 0.4, thickness: 0.1 };
            set(&mut app, PictureTuning { ssao: Some(ssao.clone()), ..default() });
            let world = app.world();
            let drawn = world.get::<ScreenSpaceAmbientOcclusion>(camera).expect("SSAO");
            assert_eq!((drawn.quality_level, drawn.radius, drawn.constant_object_thickness), (ScreenSpaceAmbientOcclusionQualityLevel::Medium, 0.4, 0.1));
            assert_eq!(world.get::<Msaa>(camera), Some(&Msaa::Off), "Bevy's SSAO needs MSAA off");
            assert!(world.get::<DepthPrepass>(camera).is_some() && world.get::<NormalPrepass>(camera).is_some());

            // Another module (the decals' depth prepass) takes a prepass away: it comes back the next frame.
            app.world_mut().entity_mut(camera).remove::<DepthPrepass>();
            app.world_mut().entity_mut(camera).insert(Msaa::Sample8);
            app.update();
            assert!(app.world().get::<DepthPrepass>(camera).is_some(), "SSAO without its prepass would draw nothing");
            assert_eq!(app.world().get::<Msaa>(camera), Some(&Msaa::Off));

            set(&mut app, PictureTuning::default());
            let world = app.world();
            assert!(world.get::<ScreenSpaceAmbientOcclusion>(camera).is_none());
            assert_eq!(world.get::<Msaa>(camera), Some(&Msaa::Sample4), "its own MSAA back");
            assert!(world.get::<DepthPrepass>(camera).is_none() && world.get::<NormalPrepass>(camera).is_none(), "no prepass it did not have");
            assert!(world.get::<PictureSet>(camera).is_none());
        }

        #[test]
        fn a_camera_with_prepasses_and_no_msaa_of_its_own_keeps_them_after_ssao() {
            let (mut app, camera) = app();
            app.world_mut().entity_mut(camera).insert(DepthPrepass);
            app.update();
            set(&mut app, PictureTuning { ssao: Some(SsaoTuning::default()), ..default() });
            assert_eq!(app.world().get::<Msaa>(camera), Some(&Msaa::Off));
            set(&mut app, PictureTuning::default());
            let world = app.world();
            assert!(world.get::<DepthPrepass>(camera).is_some(), "the camera's own prepass stays");
            assert!(world.get::<NormalPrepass>(camera).is_none(), "the one SSAO added goes");
            assert!(world.get::<Msaa>(camera).is_none(), "no MSAA component before, none after");
        }

        #[test]
        fn a_camera_that_appears_later_gets_the_picture_too() {
            let (mut app, _) = app();
            set(&mut app, PictureTuning { tone_mapping: Some(ToneMapping::BlenderFilmic), ..default() });
            let late = app.world_mut().spawn((Camera3d::default(), ThirdPersonCamera::default())).id();
            app.update();
            assert_eq!(app.world().get::<Tonemapping>(late), Some(&Tonemapping::BlenderFilmic));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_left_out_is_not_written_and_bad_numbers_are_named() {
        assert!(PictureTuning::default().is_default());
        assert_eq!(ron::to_string(&PictureTuning::default()).unwrap(), "()");
        let read: PictureTuning = ron::from_str("(bloom: Some((threshold: 1.15, composite: Additive)))").unwrap();
        assert_eq!(read.bloom.as_ref().map(|b| (b.intensity, b.threshold, b.composite)), Some((0.15, 1.15, BloomComposite::Additive)));
        assert!(read.problems().is_empty());
        let read: PictureTuning = ron::from_str("(ssao: Some((quality: Low)))").unwrap();
        assert_eq!(read.ssao, Some(SsaoTuning { quality: SsaoQuality::Low, ..default() }));
        let bad = PictureTuning { ssao: Some(SsaoTuning { radius: 0.0, thickness: f32::INFINITY, ..default() }), ..default() };
        assert_eq!(bad.problems(), ["picture.ssao.radius: above 0, not 0", "picture.ssao.thickness: above 0, not inf"]);
        let bad = PictureTuning {
            bloom: Some(BloomTuning { intensity: -1.0, high_pass_frequency: 0.0, threshold_softness: 2.0, threshold: f32::NAN, ..default() }),
            ..default()
        };
        assert_eq!(
            bad.problems(),
            [
                "picture.bloom.intensity: 0 or more, not -1",
                "picture.bloom.threshold: 0 or more, not NaN",
                "picture.bloom.high_pass_frequency: above 0 and at most 1, not 0",
                "picture.bloom.threshold_softness: 0 to 1, not 2",
            ]
        );
    }
}
