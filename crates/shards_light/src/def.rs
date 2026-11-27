//! The light kind: `*.light.ron`, a map of ids to [`LightDef`].

use bevy::math::ops;
use serde::{Deserialize, Serialize};
use shards_content::color::parse_color;
use shards_content::{ContentDef, Refs};

/// The shape of a light's beam.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub enum LightKind {
    /// Shines in every direction.
    #[default]
    Point,
    /// A cone along the light's own -Z (the way a glTF lamp points). The angles are the cone's whole opening in
    /// degrees, as Blender's *Spot Size* is: `inner_deg` is where the full light ends, `outer_deg` where it reaches
    /// nothing. `0 < inner_deg <= outer_deg < 180`.
    Spot { inner_deg: f32, outer_deg: f32 },
}

impl LightKind {
    /// `point` or `spot`.
    pub fn name(&self) -> &'static str {
        match self {
            LightKind::Point => "point",
            LightKind::Spot { .. } => "spot",
        }
    }
}

/// How a light's brightness moves.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum Flicker {
    /// Steady.
    #[default]
    None,
    /// The classic light styles: one letter per step, `a` dark, `m` as the light is written, `z` about twice that
    /// (`(letter - 'a') / 12` times the intensity), `rate` steps a second, looping. `"mmamammmmammamamaaamammma"`
    /// at 10 is a fluorescent tube that is failing.
    Pattern { pattern: String, rate: f32 },
    /// Smooth random flicker, from a seed of the light's own (its content id, node name and place when it first appeared, kept while it lives, so a moving light wanders the same way), never the simulation's
    /// random numbers: the brightness wanders between `1 - depth` and 1 times the intensity, `rate` changes a second.
    Noise { rate: f32, depth: f32 },
}

impl Flicker {
    /// `none`, `pattern` or `noise`.
    pub fn name(&self) -> &'static str {
        match self {
            Flicker::None => "none",
            Flicker::Pattern { .. } => "pattern",
            Flicker::Noise { .. } => "noise",
        }
    }
}

/// The most a content light's range can be (m).
pub const MAX_RANGE: f32 = 1000.0;

/// A light placed from content: a `ContentLight { id }` on an entity (a level node's `light=<id>`, a prefab's
/// component, game code) shines from where the entity stands. Pictures only: nothing in the simulation reads it.
///
/// ```ron
/// {
///     "sodium": (color: "#ffb347", intensity: 120000.0, range: 14.0, shadows: true,
///                flicker: Noise(rate: 6.0, depth: 0.25)),
///     "tube": (kelvin: 6500.0, intensity: 60000.0, flicker: Pattern(pattern: "mmamammmmammamamaaamammma", rate: 10.0)),
///     "torch": (kind: Spot(inner_deg: 20.0, outer_deg: 45.0), intensity: 300000.0, range: 25.0, shadows: true),
/// }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LightDef {
    /// Point or spot.
    pub kind: LightKind,
    /// The colour, `#rrggbb` (sRGB). Left out: white, or `kelvin`'s. Not both.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub color: String,
    /// The colour as a temperature, 1000 to 40000 K (1900 a candle, 2700 a bulb, 6500 daylight). 0, or left out, is not
    /// set. Not with `color`.
    #[serde(skip_serializing_if = "is_zero")]
    pub kelvin: f32,
    /// The light's power in lumens, as Bevy takes it (`PointLight::intensity`), 0 or more. At Bevy's default camera exposure
    /// 20000 is a dim bulb, 100000 a lamp that lights a room, 1000000 a floodlight. A pattern or a noise multiplies it.
    pub intensity: f32,
    /// How far the light reaches (m), above 0 and at most 1000: beyond it the light is nothing.
    pub range: f32,
    /// The size of the source (m), 0 or more: bigger softens the highlight it makes. 0 is a point.
    #[serde(skip_serializing_if = "is_zero")]
    pub radius: f32,
    /// Whether the light casts shadows, when the shadow budget (`graphics.shadow_lights`) lets it.
    #[serde(skip_serializing_if = "is_false")]
    pub shadows: bool,
    /// How the brightness moves.
    #[serde(skip_serializing_if = "is_steady")]
    pub flicker: Flicker,
    /// Whether its light is baked into the level's lightmaps (`baked_light`, `shards_level`): it then lights no
    /// lightmapped mesh's diffuse (Bevy's `affects_lightmapped_mesh_diffuse`), which has it already, and still lights
    /// what moves (bodies, props) and the shine of every surface. A flashlight, a muzzle flash and a light that
    /// changes from the bake (a flicker the bake did not see) leave it `false`.
    #[serde(skip_serializing_if = "is_false")]
    pub baked: bool,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_steady(f: &Flicker) -> bool {
    *f == Flicker::None
}

impl Default for LightDef {
    fn default() -> Self {
        Self { kind: LightKind::Point, color: String::new(), kelvin: 0.0, intensity: 100_000.0, range: 15.0, radius: 0.0, shadows: false, flicker: Flicker::None, baked: false }
    }
}

impl LightDef {
    /// The light's colour as sRGB, each channel 0 to 1: `color`, else `kelvin`'s, else white. A colour that does
    /// not read (a content error) is white.
    pub fn srgb(&self) -> [f32; 3] {
        if !self.color.is_empty() {
            return match parse_color(&self.color) {
                Ok([r, g, b, _]) => [r, g, b].map(|c| f32::from(c) / 255.0),
                Err(_) => [1.0; 3],
            };
        }
        if self.kelvin != 0.0 { kelvin_srgb(self.kelvin) } else { [1.0; 3] }
    }

    /// What is wrong with the definition, one line each naming the field; empty when it is fine.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !(self.intensity.is_finite() && self.intensity >= 0.0) {
            out.push(format!("intensity: 0 or more lumens, not {}", self.intensity));
        }
        if !(self.range.is_finite() && self.range > 0.0 && self.range <= MAX_RANGE) {
            out.push(format!("range: above 0 and at most {MAX_RANGE} metres, not {}", self.range));
        }
        if !(self.radius.is_finite() && self.radius >= 0.0) {
            out.push(format!("radius: 0 or more metres, not {}", self.radius));
        }
        if let LightKind::Spot { inner_deg, outer_deg } = self.kind
            && !(inner_deg.is_finite() && outer_deg.is_finite() && inner_deg > 0.0 && inner_deg <= outer_deg && outer_deg < 180.0)
        {
            out.push(format!("kind: a Spot needs 0 < inner_deg <= outer_deg < 180 (the cone's whole opening), not inner_deg {inner_deg}, outer_deg {outer_deg}"));
        }
        let (has_color, has_kelvin) = (!self.color.is_empty(), self.kelvin != 0.0);
        if has_color && has_kelvin {
            out.push("color and kelvin: a light has one colour, name one of them".to_owned());
        } else if has_color {
            if let Err(why) = parse_color(&self.color) {
                out.push(format!("color: {why}"));
            }
        } else if has_kelvin && !(self.kelvin.is_finite() && (1000.0..=40000.0).contains(&self.kelvin)) {
            out.push(format!("kelvin: from 1000 to 40000 (0 leaves it out), not {}", self.kelvin));
        }
        match &self.flicker {
            Flicker::None => {}
            Flicker::Pattern { pattern, rate } => {
                if pattern.is_empty() {
                    out.push("flicker.pattern: at least one letter from a to z".to_owned());
                } else if let Some((at, c)) = pattern.char_indices().find(|(_, c)| !c.is_ascii_lowercase()) {
                    out.push(format!("flicker.pattern: only the letters a to z, found `{c}` at {at}"));
                }
                if !(rate.is_finite() && *rate > 0.0 && *rate <= 240.0) {
                    out.push(format!("flicker.rate: steps a second, above 0 and at most 240, not {rate}"));
                }
            }
            Flicker::Noise { rate, depth } => {
                if !(rate.is_finite() && *rate > 0.0 && *rate <= 240.0) {
                    out.push(format!("flicker.rate: changes a second, above 0 and at most 240, not {rate}"));
                }
                if !(depth.is_finite() && (0.0..=1.0).contains(depth)) {
                    out.push(format!("flicker.depth: from 0 (steady) to 1 (down to dark), not {depth}"));
                }
            }
        }
        out
    }
}

impl ContentDef for LightDef {
    const KIND: &'static str = "light";
    // Only what a machine shows: a replay never reads it, and editing it never makes one diverge.
    const SIMULATION: bool = false;

    fn validate(&self, _id: &str, refs: &mut Refs) {
        for problem in self.problems() {
            refs.error(problem);
        }
    }
}

/// A colour temperature as sRGB, each channel 0 to 1 (Tanner Helland's fit to black-body light, good from about
/// 1000 K to 40000 K). The curve is for looks; the engine takes no measurement from it.
pub fn kelvin_srgb(kelvin: f32) -> [f32; 3] {
    let t = kelvin.clamp(1000.0, 40000.0) / 100.0;
    let red = if t <= 66.0 { 255.0 } else { 329.698_73 * ops::powf(t - 60.0, -0.133_204_76) };
    let green = if t <= 66.0 { 99.470_8 * ops::ln(t) - 161.119_57 } else { 288.122_17 * ops::powf(t - 60.0, -0.075_514_85) };
    let blue = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_73 * ops::ln(t - 10.0) - 305.044_8
    };
    [red, green, blue].map(|c: f32| c.clamp(0.0, 255.0) / 255.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_definition_reads_with_defaults_and_a_default_writes_nothing() {
        let read: LightDef = ron::from_str("(color: \"#ffb347\", range: 14.0, shadows: true, flicker: Noise(rate: 6.0, depth: 0.25))").unwrap();
        assert_eq!((read.intensity, read.range, read.shadows), (100_000.0, 14.0, true));
        assert_eq!(read.flicker, Flicker::Noise { rate: 6.0, depth: 0.25 });
        assert!(read.problems().is_empty());
        let text = ron::to_string(&LightDef::default()).unwrap();
        assert_eq!(text, "(kind:Point,intensity:100000.0,range:15.0)");
        let spot: LightDef = ron::from_str("(kind: Spot(inner_deg: 20.0, outer_deg: 45.0))").unwrap();
        assert_eq!(spot.kind, LightKind::Spot { inner_deg: 20.0, outer_deg: 45.0 });
        assert!(ron::from_str::<LightDef>("(colour: \"#fff\")").is_err(), "an unknown field is an error");
    }

    #[test]
    fn bad_numbers_are_named_by_field() {
        let bad = LightDef {
            intensity: -1.0,
            range: 0.0,
            radius: -0.5,
            kind: LightKind::Spot { inner_deg: 50.0, outer_deg: 40.0 },
            color: "red".into(),
            flicker: Flicker::Pattern { pattern: "maM".into(), rate: 0.0 },
            ..default_def()
        };
        let problems = bad.problems();
        let text = problems.join("\n");
        for want in [
            "intensity: 0 or more lumens, not -1",
            "range: above 0 and at most 1000 metres, not 0",
            "radius: 0 or more metres, not -0.5",
            "kind: a Spot needs 0 < inner_deg <= outer_deg < 180",
            "color: `red` is not a colour",
            "flicker.pattern: only the letters a to z, found `M` at 2",
            "flicker.rate: steps a second, above 0 and at most 240, not 0",
        ] {
            assert!(text.contains(want), "{want:?} missing from\n{text}");
        }
        for (def, want) in [
            (LightDef { color: "#fff".into(), kelvin: 3000.0, ..default_def() }, "color and kelvin"),
            (LightDef { kelvin: 500.0, ..default_def() }, "kelvin: from 1000 to 40000 (0 leaves it out), not 500"),
            (LightDef { kind: LightKind::Spot { inner_deg: 0.0, outer_deg: 30.0 }, ..default_def() }, "0 < inner_deg"),
            (LightDef { kind: LightKind::Spot { inner_deg: 10.0, outer_deg: 180.0 }, ..default_def() }, "outer_deg < 180"),
            (LightDef { flicker: Flicker::Pattern { pattern: String::new(), rate: 10.0 }, ..default_def() }, "at least one letter"),
            (LightDef { flicker: Flicker::Noise { rate: 5.0, depth: 1.5 }, ..default_def() }, "flicker.depth"),
        ] {
            assert!(def.problems().join("\n").contains(want), "{want}: {:?}", def.problems());
        }
        // The edges are allowed.
        let edges = LightDef { intensity: 0.0, kind: LightKind::Spot { inner_deg: 30.0, outer_deg: 30.0 }, ..default_def() };
        assert!(edges.problems().is_empty(), "{:?}", edges.problems());
    }

    fn default_def() -> LightDef {
        LightDef::default()
    }

    #[test]
    fn colours_come_from_hex_or_temperature() {
        let hex = LightDef { color: "#ff8000".into(), ..default_def() };
        assert_eq!(hex.srgb(), [1.0, 128.0 / 255.0, 0.0]);
        assert_eq!(default_def().srgb(), [1.0; 3]);
        let warm = kelvin_srgb(1900.0);
        let daylight = kelvin_srgb(6600.0);
        let blue = kelvin_srgb(12000.0);
        assert!(warm[0] > warm[1] && warm[1] > warm[2], "a candle is orange: {warm:?}");
        assert!(daylight.iter().all(|c| *c > 0.95), "daylight is near white: {daylight:?}");
        assert!(blue[2] > blue[0], "high temperatures are blue: {blue:?}");
        assert_eq!(LightDef { kelvin: 2700.0, ..default_def() }.srgb(), kelvin_srgb(2700.0));
    }
}
