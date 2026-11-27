//! The shadow budget: of the lights that want shadows, only the nearest to the viewer get them, as many as the
//! budget holds: so many lights (`graphics.shadow_lights`), or so many shadow views (`graphics.shadow_views`: a
//! point light's cube is 6, a spot is 1).
//!
//! A pure function of the lights and the viewer, so a picture and a headless `lights` query agree and a
//! shot taken twice from one place shows the same shadows. There is no hysteresis: a light that is
//! nearly as near as one that has a shadow takes it when it comes nearer, once.

use std::cmp::Ordering;

pub use shards_settings::ShadowBudget;

/// The shadow views a point light takes: its shadow is a cube map, six passes.
pub const POINT_VIEWS: usize = 6;
/// The shadow views a spot light takes: one pass.
pub const SPOT_VIEWS: usize = 1;

/// One light that is a candidate for a shadow.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate<'a> {
    /// How far it is from the viewer (m).
    pub distance: f32,
    /// A stable name for ties: the content id, the node and the place.
    pub key: &'a str,
    /// Whether its content asks for shadows.
    pub wants: bool,
    /// The shadow views its shadow takes ([`POINT_VIEWS`], [`SPOT_VIEWS`]); only a budget in views reads it.
    pub views: usize,
}

/// Distances are compared to the centimetre, so a viewer that stands still on a boundary does not flip a
/// pair of lights that are as near as each other by float noise; equals go by `key`.
fn rank(a: &Candidate, b: &Candidate) -> Ordering {
    let cm = |c: &Candidate| (c.distance * 100.0).round() as i64;
    cm(a).cmp(&cm(b)).then_with(|| a.key.cmp(b.key))
}

/// Which of `lights` cast a shadow, in the same order, nearest first: under [`ShadowBudget::Lights`] the first
/// that many of those that want one; under [`ShadowBudget::Views`] each in turn while its views fit in what is
/// left (a near point light that does not fit has none, and a farther spot that does fit has one). A light that
/// does not want shadows never has one.
pub fn casters(lights: &[Candidate], budget: ShadowBudget) -> Vec<bool> {
    let mut wanting: Vec<usize> = (0..lights.len()).filter(|i| lights[*i].wants && lights[*i].distance.is_finite()).collect();
    wanting.sort_by(|a, b| rank(&lights[*a], &lights[*b]).then(a.cmp(b)));
    let mut out = vec![false; lights.len()];
    match budget {
        ShadowBudget::Lights(n) => {
            for i in wanting.into_iter().take(n) {
                out[i] = true;
            }
        }
        ShadowBudget::Views(mut left) => {
            for i in wanting {
                let cost = lights[i].views.max(1);
                if cost <= left {
                    left -= cost;
                    out[i] = true;
                }
            }
        }
    }
    out
}

/// The shadow views the casters take, by [`casters`]' answer.
pub fn views_used(lights: &[Candidate], casts: &[bool]) -> usize {
    lights.iter().zip(casts).filter(|(_, c)| **c).map(|(l, _)| l.views.max(1)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lights<'a>(list: &[(f32, &'a str, bool)]) -> Vec<Candidate<'a>> {
        list.iter().map(|(distance, key, wants)| Candidate { distance: *distance, key, wants: *wants, views: POINT_VIEWS }).collect()
    }

    const fn n(lights: usize) -> ShadowBudget {
        ShadowBudget::Lights(lights)
    }

    #[test]
    fn the_nearest_that_want_a_shadow_get_one() {
        let all = lights(&[(9.0, "c", true), (2.0, "a", true), (5.0, "b", true), (1.0, "n", false), (30.0, "z", true)]);
        assert_eq!(casters(&all, n(2)), [false, true, true, false, false]);
        assert_eq!(casters(&all, n(0)), [false; 5], "a budget of 0 is no shadows");
        assert_eq!(casters(&all, n(99)), [true, true, true, false, true], "a light that does not want one never has one");
    }

    #[test]
    fn a_budget_in_views_lets_many_spots_shadow_where_one_point_would() {
        let mut all = lights(&[(1.0, "point", true), (2.0, "s1", true), (3.0, "s2", true), (4.0, "s3", true), (5.0, "far point", true)]);
        for spot in &mut all[1..4] {
            spot.views = SPOT_VIEWS;
        }
        let casts = casters(&all, ShadowBudget::Views(6));
        assert_eq!(casts, [true, false, false, false, false], "the nearest point takes the whole cube");
        let casts = casters(&all, ShadowBudget::Views(8));
        assert_eq!(casts, [true, true, true, false, false], "then two spots in the two views left");
        assert_eq!(views_used(&all, &casts), 8);
        let casts = casters(&all, ShadowBudget::Views(5));
        assert_eq!(casts, [false, true, true, true, false], "a point that does not fit has none; the spots behind it that fit do");
        assert_eq!(views_used(&all, &casts), 3);
        assert_eq!(casters(&all, ShadowBudget::Views(0)), [false; 5]);
        assert_eq!(casters(&all, ShadowBudget::Views(15)), [true, true, true, true, true]);
    }

    #[test]
    fn ties_go_by_key_and_the_order_of_the_list_does_not_matter() {
        let a = lights(&[(4.0, "lamp.b", true), (4.001, "lamp.a", true), (4.0, "lamp.c", true)]);
        // To the centimetre all three are 4 m: the key decides.
        assert_eq!(casters(&a, n(1)), [false, true, false]);
        let b = lights(&[(4.0, "lamp.c", true), (4.0, "lamp.a", true), (4.0, "lamp.b", true)]);
        assert_eq!(casters(&b, n(2)), [false, true, true]);
        let nan = lights(&[(f32::NAN, "x", true), (1.0, "y", true)]);
        assert_eq!(casters(&nan, n(2)), [false, true], "a light with no distance has no shadow");
    }
}
