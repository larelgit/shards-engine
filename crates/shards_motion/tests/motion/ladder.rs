//! Ladders: a `VolumeKind::Ladder` volume and the controller's `MotionMode::Ladder`. Taking hold at the
//! foot, from a fall, from the water and off the top's edge; the climb up, down and sideways; the step
//! off onto the floor above; the jump that lets go; and who never climbs.

use bevy::prelude::*;
use shards_core::{Gaze, Intent};
use shards_motion::*;

use crate::common::*;

/// The floor the ladder leads to: the top of the wall, in metres over the ground.
const TOP: f32 = 3.0;
/// The feet rest this far over a floor: the default skin.
const SKIN: f32 = 0.02;

/// The default body, climbing at 3.5 m/s.
fn climber() -> CharacterParams {
    CharacterParams { ladder_speed: 3.5, ..default() }
}

/// The ground, a wall `top` metres high west of x = 0 whose top is a floor, and a ladder up its east
/// face: a volume 0.3 m thick and 0.8 m wide from the ground to half a metre over the wall's top, climbed
/// from the east (+X).
fn wall_with_ladder(world: &mut World, top: f32) -> Entity {
    floor(world);
    block(world, Vec3::new(-6.0, 0.0, -3.0), Vec3::new(0.0, top, 3.0));
    ladder_to(world, top + 0.5)
}

fn ladder_to(world: &mut World, top: f32) -> Entity {
    volume(world, Vec3::new(0.0, 0.0, -0.4), Vec3::new(0.3, top, 0.4), VolumeKind::Ladder { face: Vec3::X })
}

/// A climber standing at `feet`, settled, with the events of its arrival forgotten.
fn climber_at(app: &mut App, feet: Vec3, params: CharacterParams) -> Entity {
    let e = hero_with(app.world_mut(), feet, params);
    step(app, 3);
    *app.world_mut().resource_mut::<Seen>() = Seen::default();
    e
}

/// The mode changes of `e` so far, as `(from, to)`.
fn modes(app: &App, e: Entity) -> Vec<(MotionMode, MotionMode)> {
    seen(app).modes.iter().filter(|m| m.entity == e).map(|m| (m.from, m.to)).collect()
}

/// Walks west into the ladder until the body is on it with its feet at least `height` up; the body is
/// left pushing in.
fn climb_to(app: &mut App, e: Entity, height: f32) {
    walk(app.world_mut(), e, Vec3::NEG_X);
    let ticks = drive(app, 240, |w| {
        let (s, y) = (w.get::<MotionState>(e).unwrap(), w.get::<Transform>(e).unwrap().translation.y);
        !(s.mode == MotionMode::Ladder && y >= height)
    });
    assert!(ticks < 240, "never got {height} m up the ladder: {:?} at {}", state(app, e).mode, feet(app, e));
}

#[test]
fn a_climb_of_three_metres_ends_standing_on_the_floor_above() {
    let mut app = app();
    wall_with_ladder(app.world_mut(), TOP);
    let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.0), climber());
    walk(app.world_mut(), e, Vec3::NEG_X);
    let (mut highest, mut climbed) = (0.0f32, 0);
    let ticks = drive(&mut app, 300, |w| {
        let (s, at) = (w.get::<MotionState>(e).unwrap(), w.get::<Transform>(e).unwrap().translation);
        highest = highest.max(at.y);
        // Between the foot and the top the climb is straight up at the ladder's speed, with no gravity.
        if s.mode == MotionMode::Ladder && (0.5..2.5).contains(&at.y) {
            assert_eq!(s.velocity, Vec3::Y * 3.5, "at {at}");
            assert!(!s.grounded && s.fall_from_y.is_none(), "{s:?}");
            climbed += 1;
        }
        !(s.mode == MotionMode::Ground && at.x < -1.0)
    });
    assert!(ticks < 300, "never walked onto the top: {:?} at {}", state(&app, e).mode, feet(&app, e));
    let at = feet(&app, e);
    assert!((at.y - (TOP + SKIN)).abs() < 1e-3, "stands on the wall's top: {at}");
    assert!(climbed > 20, "about 2 m at 3.5 m/s is 34 ticks: {climbed}");
    // On at the foot, off at the top: never in the air, so nothing fell and nothing landed.
    assert_eq!(modes(&app, e), [(MotionMode::Ground, MotionMode::Ladder), (MotionMode::Ladder, MotionMode::Ground)]);
    assert!(seen(&app).landed.is_empty() && seen(&app).jumped.is_empty(), "{:?}", seen(&app).landed);
    // A volume half a metre over the top is enough: the feet never rose to its top.
    assert!(highest < TOP + 0.2, "the body walks off level with the floor, it does not pop up: {highest}");
    assert_clear(&app, e);
}

#[test]
fn walking_off_the_top_takes_hold_and_climbs_down_to_the_ground() {
    let mut app = app();
    wall_with_ladder(app.world_mut(), TOP);
    let e = climber_at(&mut app, Vec3::new(-2.0, TOP + SKIN, 0.0), climber());
    // East: off the wall's edge, through the ladder's volume, and on away from the wall.
    walk(app.world_mut(), e, Vec3::X);
    let mut fastest_down = 0.0f32;
    let ticks = drive(&mut app, 300, |w| {
        let (s, at) = (w.get::<MotionState>(e).unwrap(), w.get::<Transform>(e).unwrap().translation);
        if s.mode == MotionMode::Ladder {
            fastest_down = fastest_down.min(s.velocity.y);
        }
        !(s.mode == MotionMode::Ground && at.x > 2.0)
    });
    assert!(ticks < 300, "never walked away from the foot: {:?} at {}", state(&app, e).mode, feet(&app, e));
    assert!((feet(&app, e).y - SKIN).abs() < 1e-3, "{}", feet(&app, e));
    assert_eq!(fastest_down, -3.5, "down at the ladder's speed, never faster");
    assert_eq!(modes(&app, e), [(MotionMode::Ground, MotionMode::Ladder), (MotionMode::Ladder, MotionMode::Ground)]);
    assert!(seen(&app).landed.is_empty(), "a climb down is no fall: {:?}", seen(&app).landed);
}

#[test]
fn a_body_that_stands_still_on_a_ladder_hangs_there() {
    let mut app = app();
    wall_with_ladder(app.world_mut(), TOP);
    let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.0), climber());
    climb_to(&mut app, e, 1.5);
    walk(app.world_mut(), e, Vec3::ZERO);
    step(&mut app, 1);
    let held = feet(&app, e);
    step(&mut app, 120);
    assert_eq!(feet(&app, e), held, "two seconds with no input: not a millimetre");
    let s = state(&app, e);
    assert_eq!((s.mode, s.velocity), (MotionMode::Ladder, Vec3::ZERO));
    // An action that owns the body holds it too: its input does not climb.
    set_input(app.world_mut(), e, LocomotionInput { move_dir: Vec3::NEG_X, locked: true, ..default() });
    step(&mut app, 30);
    assert_eq!((feet(&app, e), state(&app, e).mode), (held, MotionMode::Ladder));
}

fn let_go(app: &App, e: Entity) -> bool {
    app.world().get::<LadderLetGo>(e).is_some()
}

#[test]
fn a_jump_lets_go_with_a_push_out_of_the_ladder() {
    let mut app = app();
    wall_with_ladder(app.world_mut(), TOP);
    let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.0), climber());
    climb_to(&mut app, e, 1.5);
    let from = feet(&app, e);
    // Still pushing into the ladder, as a player who climbs does.
    press_jump(app.world_mut(), e);
    step(&mut app, 1);
    let s = state(&app, e);
    assert_eq!(s.mode, MotionMode::Air);
    assert!(s.velocity.x > 2.0 && s.velocity.x <= 2.5, "pushed east, out of the face, at ladder_jump_speed: {}", s.velocity);
    assert!(s.velocity.y <= 0.0, "a push, not a hop: {}", s.velocity);
    assert_eq!(seen(&app).jumped.len(), 1);
    assert!(feet(&app, e).x > from.x && let_go(&app, e), "{} from {from}", feet(&app, e));
    // Let go of the stick too: the marker of the jump goes, and the body falls to the ground in front of the ladder.
    walk(app.world_mut(), e, Vec3::ZERO);
    step(&mut app, 1);
    assert!(!let_go(&app, e));
    let ticks = drive(&mut app, 120, |w| w.get::<MotionState>(e).unwrap().mode != MotionMode::Ground);
    assert!(ticks < 120);
    assert_eq!(seen(&app).landed.len(), 1, "it fell: {:?}", seen(&app).landed);
    assert!(seen(&app).landed[0].fall_height > 1.0 && feet(&app, e).x > from.x + 0.2, "{:?} at {}", seen(&app).landed, feet(&app, e));
}

#[test]
fn a_jump_lets_go_for_good_while_the_body_goes_on_pushing_in() {
    // Both ways of steering the air: the stepped one drifts back to the wall in a third of a second, Half-Life's
    // push takes the jump's 2.5 m/s back in two ticks. Neither takes hold again.
    for params in [climber(), runner()] {
        let mut app = app();
        wall_with_ladder(app.world_mut(), 8.0);
        let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.0), params);
        climb_to(&mut app, e, 5.0);
        press_jump(app.world_mut(), e);
        let ticks = drive(&mut app, 240, |w| w.get::<MotionState>(e).unwrap().mode != MotionMode::Ground);
        assert!(ticks < 240, "{:?} at {}", state(&app, e).mode, feet(&app, e));
        let changes = modes(&app, e);
        assert_eq!(changes[changes.len() - 2..], [(MotionMode::Ladder, MotionMode::Air), (MotionMode::Air, MotionMode::Ground)], "{changes:?}");
        assert!(seen(&app).landed.len() == 1 && seen(&app).landed[0].fall_height > 4.5, "five metres down: {:?}", seen(&app).landed);
        // On the ground the jump is over: the same push climbs again.
        step(&mut app, 3);
        assert!(!let_go(&app, e) && state(&app, e).mode == MotionMode::Ladder, "{:?}", state(&app, e).mode);
    }
}

#[test]
fn letting_the_push_go_and_pushing_in_again_takes_hold_in_the_fall() {
    let mut app = app();
    wall_with_ladder(app.world_mut(), 8.0);
    let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.0), runner());
    climb_to(&mut app, e, 6.0);
    press_jump(app.world_mut(), e);
    step(&mut app, 6);
    assert!(state(&app, e).mode == MotionMode::Air && let_go(&app, e));
    walk(app.world_mut(), e, Vec3::ZERO);
    step(&mut app, 2);
    walk(app.world_mut(), e, Vec3::NEG_X);
    let ticks = drive(&mut app, 60, |w| w.get::<MotionState>(e).unwrap().mode == MotionMode::Air);
    assert!(ticks < 60);
    let (s, at) = (state(&app, e), feet(&app, e));
    assert!(s.mode == MotionMode::Ladder && at.y > 2.0 && !let_go(&app, e), "caught again at {at}: {:?}", s.mode);
    assert!(seen(&app).landed.is_empty());
}

/// The feet, the velocity and the mode of `e` over `ticks` ticks, bit for bit.
fn trace(app: &mut App, e: Entity, ticks: u64) -> Vec<([u32; 6], MotionMode)> {
    let mut trace = Vec::new();
    drive(app, ticks, |w| {
        let (at, s) = (w.get::<Transform>(e).unwrap().translation, w.get::<MotionState>(e).unwrap());
        trace.push(([at.x, at.y, at.z, s.velocity.x, s.velocity.y, s.velocity.z].map(f32::to_bits), s.mode));
        true
    });
    trace
}

#[test]
fn a_climb_and_a_jump_off_go_on_the_same_from_what_a_save_holds() {
    for jumped in [false, true] {
        let mut first = app();
        wall_with_ladder(first.world_mut(), 8.0);
        let e = climber_at(&mut first, Vec3::new(2.0, SKIN, 0.0), runner());
        climb_to(&mut first, e, 5.0);
        if jumped {
            press_jump(first.world_mut(), e);
            step(&mut first, 3);
            assert!(state(&first, e).mode == MotionMode::Air && let_go(&first, e));
        }
        // What a snapshot keeps of the body: its place, its motion, its input, and the marker of the jump.
        let w = first.world();
        let saved = (*w.get::<Transform>(e).unwrap(), w.get::<MotionState>(e).unwrap().clone(), w.get::<LocomotionInput>(e).unwrap().clone());
        let marked = let_go(&first, e);
        // A fresh world, the body put back from those alone.
        let mut second = app();
        wall_with_ladder(second.world_mut(), 8.0);
        let twin = second.world_mut().spawn((Name::new("Hero"), CharacterController::default(), runner(), saved)).id();
        if marked {
            second.world_mut().entity_mut(twin).insert(LadderLetGo);
        }
        let (went_on, resumed) = (trace(&mut first, e, 90), trace(&mut second, twin, 90));
        assert_eq!(went_on, resumed, "jumped: {jumped}");
        // The climb goes on climbing; the jump goes on falling, and is not caught on the way down.
        let first = if jumped { MotionMode::Air } else { MotionMode::Ladder };
        assert!(went_on[..20].iter().all(|(_, mode)| *mode == first), "{:?}", &went_on[..20]);
    }
}

#[test]
fn a_fall_past_a_ladder_is_caught_by_pushing_into_it() {
    let mut app = app();
    wall_with_ladder(app.world_mut(), 8.0);
    // In the air a metre off the wall, six metres up.
    let e = hero_with(app.world_mut(), Vec3::new(1.4, 6.0, 0.0), climber());
    step(&mut app, 3);
    assert_eq!(state(&app, e).mode, MotionMode::Air);
    walk(app.world_mut(), e, Vec3::NEG_X);
    let ticks = drive(&mut app, 120, |w| w.get::<MotionState>(e).unwrap().mode == MotionMode::Air);
    assert!(ticks < 120);
    let (s, at) = (state(&app, e), feet(&app, e));
    assert_eq!(s.mode, MotionMode::Ladder, "caught, at {at}");
    assert!(at.y > 1.0, "well above the ground: {at}");
    assert!(s.fall_from_y.is_none() && s.velocity.y >= 0.0, "the fall is over and forgotten: {s:?}");
    // And it climbs from there, back above where it was caught.
    step(&mut app, 30);
    assert!(feet(&app, e).y > at.y + 1.0 && seen(&app).landed.is_empty(), "{}", feet(&app, e));
}

#[test]
fn strafing_slides_along_the_ladder_and_off_its_side_is_a_fall() {
    let mut app = app();
    wall_with_ladder(app.world_mut(), TOP);
    let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.0), climber());
    climb_to(&mut app, e, 1.5);
    walk(app.world_mut(), e, Vec3::Z);
    step(&mut app, 1);
    let from = feet(&app, e);
    step(&mut app, 4);
    let (s, at) = (state(&app, e), feet(&app, e));
    assert_eq!((s.mode, s.velocity), (MotionMode::Ladder, Vec3::Z * 3.5));
    assert!(at.y == from.y && at.x == from.x && at.z > from.z + 0.2, "sideways only: {from} to {at}");
    // The ladder is 0.8 m wide: past its side there is nothing to hold.
    let ticks = drive(&mut app, 120, |w| w.get::<MotionState>(e).unwrap().mode != MotionMode::Ground);
    assert!(ticks < 120);
    assert!(modes(&app, e).contains(&(MotionMode::Ladder, MotionMode::Air)), "{:?}", modes(&app, e));
    assert_eq!(seen(&app).landed.len(), 1);
}

#[test]
fn a_ladder_that_leads_nowhere_stops_the_climb_at_its_top() {
    let mut app = app();
    // The wall goes on up to 8 m; the ladder ends at 3.5 m.
    floor(app.world_mut());
    block(app.world_mut(), Vec3::new(-6.0, 0.0, -3.0), Vec3::new(0.0, 8.0, 3.0));
    ladder_to(app.world_mut(), TOP + 0.5);
    let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.0), climber());
    walk(app.world_mut(), e, Vec3::NEG_X);
    step(&mut app, 240);
    let (s, at) = (state(&app, e), feet(&app, e));
    assert_eq!((s.mode, s.velocity), (MotionMode::Ladder, Vec3::ZERO), "at {at}");
    assert!(at.y > TOP && at.y <= TOP + 0.5, "it hangs at the top, its feet still in the volume: {at}");
    assert_eq!(modes(&app, e), [(MotionMode::Ground, MotionMode::Ladder)], "it never left the ladder to fall back onto it");
}

#[test]
fn a_ladder_with_nothing_solid_behind_it_is_not_taken_hold_of_from_the_ground() {
    let mut app = app();
    floor(app.world_mut());
    ladder_to(app.world_mut(), TOP + 0.5);
    let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.0), climber());
    walk(app.world_mut(), e, Vec3::NEG_X);
    step(&mut app, 90);
    assert!(feet(&app, e).x < -1.0 && modes(&app, e).is_empty(), "walked through: {} {:?}", feet(&app, e), modes(&app, e));
}

/// The feet and the velocity of a body with `params` that walks west into the wall for `ticks` ticks, in a
/// world with or without the ladder, bit for bit.
fn walk_into_the_wall(params: CharacterParams, ladder: bool, ticks: u64) -> (Vec<[u32; 6]>, Vec<(MotionMode, MotionMode)>) {
    let mut app = app();
    floor(app.world_mut());
    block(app.world_mut(), Vec3::new(-6.0, 0.0, -3.0), Vec3::new(0.0, TOP, 3.0));
    if ladder {
        ladder_to(app.world_mut(), TOP + 0.5);
    }
    let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.0), params);
    walk(app.world_mut(), e, Vec3::NEG_X);
    let mut trace = Vec::new();
    drive(&mut app, ticks, |w| {
        let (at, v) = (w.get::<Transform>(e).unwrap().translation, w.get::<MotionState>(e).unwrap().velocity);
        trace.push([at.x, at.y, at.z, v.x, v.y, v.z].map(f32::to_bits));
        true
    });
    (trace, modes(&app, e))
}

#[test]
fn a_body_with_no_ladder_speed_is_not_affected_by_a_ladder() {
    assert_eq!(CharacterParams::default().ladder_speed, 0.0, "nobody climbs unless its locomotion says");
    let (with, changes) = walk_into_the_wall(CharacterParams::default(), true, 120);
    let (without, _) = walk_into_the_wall(CharacterParams::default(), false, 120);
    assert!(changes.is_empty(), "it stays on the ground: {changes:?}");
    assert_eq!(with, without, "and moves as if the ladder were not there, to the bit");
    // The same world with a body that climbs is another story.
    let (climbing, changes) = walk_into_the_wall(climber(), true, 120);
    assert!(climbing != without && changes.first() == Some(&(MotionMode::Ground, MotionMode::Ladder)));
}

/// A session on the ladder: up and over the top, back down off the edge, half way up again, a jump off.
fn session() -> Vec<([u32; 6], MotionMode)> {
    let mut app = app();
    wall_with_ladder(app.world_mut(), TOP);
    let e = climber_at(&mut app, Vec3::new(2.0, SKIN, 0.3), climber());
    let mut trace = Vec::new();
    let mut run = |app: &mut App, input: LocomotionInput, ticks: u64| {
        set_input(app.world_mut(), e, input);
        drive(app, ticks, |w| {
            let (at, s) = (w.get::<Transform>(e).unwrap().translation, w.get::<MotionState>(e).unwrap());
            trace.push(([at.x, at.y, at.z, s.velocity.x, s.velocity.y, s.velocity.z].map(f32::to_bits), s.mode));
            true
        });
    };
    let toward = |dir: Vec3| LocomotionInput { move_dir: dir, ..default() };
    run(&mut app, toward(Vec3::NEG_X), 110);
    run(&mut app, toward(Vec3::new(1.0, 0.0, -0.2)), 130);
    run(&mut app, toward(Vec3::new(-1.0, 0.0, 0.1)), 60);
    run(&mut app, LocomotionInput { move_dir: Vec3::NEG_X, jump: true, ..default() }, 1);
    run(&mut app, toward(Vec3::ZERO), 60);
    trace
}

#[test]
fn the_same_inputs_give_the_same_ticks_twice() {
    let (first, second) = (session(), session());
    assert_eq!(first, second);
    let on_ladder = first.iter().filter(|(_, mode)| *mode == MotionMode::Ladder).count();
    assert!(on_ladder > 80, "the session is mostly climbing: {on_ladder} of {}", first.len());
    assert!(first.iter().any(|(_, mode)| *mode == MotionMode::Air), "and ends with the jump off");
}

/// A body that faces its look, driven through `Intent` as a player is.
fn looking_climber(app: &mut App, by_look: bool) -> Entity {
    let params = CharacterParams { face: Facing::Look, ladder_by_look: by_look, ..climber() };
    let at = Transform::from_xyz(2.0, SKIN, 0.0);
    let e = app.world_mut().spawn((Name::new("Hero"), CharacterController::default(), params, Intent::default(), at)).id();
    step(app, 3);
    e
}

/// West, into the ladder: a yaw of a quarter turn to the left of -Z.
const WEST: f32 = std::f32::consts::FRAC_PI_2;

fn look(app: &mut App, e: Entity, pitch_deg: f32) {
    let mut intent = app.world_mut().get_mut::<Intent>(e).unwrap();
    intent.look = Some(Gaze::new(WEST, pitch_deg.to_radians()));
    intent.move_dir = Vec3::NEG_X;
}

#[test]
fn by_look_the_forward_input_climbs_along_the_pitch_as_in_half_life() {
    let mut app = app();
    wall_with_ladder(app.world_mut(), 8.0);
    let e = looking_climber(&mut app, true);
    look(&mut app, e, 0.0);
    let ticks = drive(&mut app, 120, |w| {
        !(w.get::<MotionState>(e).unwrap().mode == MotionMode::Ladder && w.get::<Transform>(e).unwrap().translation.y > 1.5)
    });
    assert!(ticks < 120);
    assert_eq!(state(&app, e).velocity, Vec3::Y * 3.5, "a level look climbs at the ladder's speed");
    // 45 degrees up: the pitch adds its own rise, cos + sin = 1.41 times the speed.
    look(&mut app, e, 45.0);
    step(&mut app, 2);
    assert!((state(&app, e).velocity.y - 3.5 * std::f32::consts::SQRT_2).abs() < 1e-3, "{}", state(&app, e).velocity);
    // 60 degrees down and still pressing forward: down, at cos - sin = -0.37 of the speed.
    look(&mut app, e, -60.0);
    step(&mut app, 2);
    let down = 3.5 * (0.5 - 3f32.sqrt() * 0.5);
    assert!((state(&app, e).velocity.y - down).abs() < 1e-3, "{} vs {down}", state(&app, e).velocity);
    assert_eq!(state(&app, e).mode, MotionMode::Ladder);

    // Without the rule the pitch does not count: looking down and pushing in still climbs.
    let mut app = crate::common::app();
    wall_with_ladder(app.world_mut(), 8.0);
    let e = looking_climber(&mut app, false);
    look(&mut app, e, -60.0);
    step(&mut app, 60);
    let s = state(&app, e);
    assert_eq!((s.mode, s.velocity), (MotionMode::Ladder, Vec3::Y * 3.5));
}

#[test]
fn a_swimmer_takes_hold_of_a_ladder_out_of_the_water() {
    let mut app = app();
    let world = app.world_mut();
    // A tank 3 m deep, its water level with y = 0, and a deck a metre over the water west of x = 0.
    block(world, Vec3::new(-6.0, -4.0, -3.0), Vec3::new(10.0, -3.0, 3.0));
    block(world, Vec3::new(-6.0, -3.0, -3.0), Vec3::new(0.0, 1.0, 3.0));
    volume(world, Vec3::new(0.0, -3.0, -3.0), Vec3::new(10.0, 0.0, 3.0), VolumeKind::Water);
    volume(world, Vec3::new(0.0, -2.0, -0.4), Vec3::new(0.3, 1.5, 0.4), VolumeKind::Ladder { face: Vec3::X });
    let e = hero_with(world, Vec3::new(3.0, -1.1, 0.0), climber());
    step(&mut app, 30);
    assert_eq!(state(&app, e).mode, MotionMode::Water);
    *app.world_mut().resource_mut::<Seen>() = Seen::default();
    walk(app.world_mut(), e, Vec3::NEG_X);
    let ticks = drive(&mut app, 300, |w| {
        !(w.get::<MotionState>(e).unwrap().mode == MotionMode::Ground && w.get::<Transform>(e).unwrap().translation.x < -1.0)
    });
    assert!(ticks < 300, "never got onto the deck: {:?} at {}", state(&app, e).mode, feet(&app, e));
    assert!((feet(&app, e).y - (1.0 + SKIN)).abs() < 1e-3, "{}", feet(&app, e));
    assert_eq!(modes(&app, e), [(MotionMode::Water, MotionMode::Ladder), (MotionMode::Ladder, MotionMode::Ground)]);
    assert!(seen(&app).mantled.is_empty(), "by the ladder, not by a climb-out");
}

#[test]
fn the_ladder_numbers_are_checked_and_cost_a_default_body_nothing_in_its_serialized_form() {
    assert!(climber().check().is_ok() && climber().can_climb_ladders() && !CharacterParams::default().can_climb_ladders());
    let err = CharacterParams { ladder_speed: -1.0, ..default() }.check().unwrap_err();
    assert!(err.contains("ladder_speed must be >= 0"), "{err}");
    let err = CharacterParams { ladder_jump_speed: f32::NAN, ..default() }.check().unwrap_err();
    assert!(err.contains("ladder_jump_speed must be >= 0"), "{err}");
    // A definition that says nothing about ladders serializes as it did before they existed: the content
    // hash of every replay recorded before stays what it was.
    let plain = serde_json::to_string(&CharacterParams::default()).unwrap();
    assert!(!plain.contains("ladder"), "{plain}");
    let climbing = serde_json::to_string(&CharacterParams { ladder_by_look: true, ladder_jump_speed: 3.0, ..climber() }).unwrap();
    for field in ["\"ladder_speed\":3.5", "\"ladder_jump_speed\":3.0", "\"ladder_by_look\":true"] {
        assert!(climbing.contains(field), "{field} in {climbing}");
    }
}

/// A first-person runner: slimmer, 8.13 m/s on the ground under *Half-Life*'s friction and air control, a
/// stronger gravity, no climb of its own onto ledges, climbing ladders at *Half-Life*'s 5.08 m/s.
fn runner() -> CharacterParams {
    CharacterParams {
        radius: 0.35,
        walk_speed: 2.4,
        run_speed: 8.13,
        sprint_speed: 8.13,
        jump_height: 1.14,
        gravity: 20.32,
        mantle_height: 0.35,
        accel: AccelModel::HALF_LIFE,
        ladder_speed: 5.08,
        ..default()
    }
}

#[test]
fn a_fast_runner_goes_up_over_the_top_and_back_down_the_same_ladder() {
    let mut app = app();
    wall_with_ladder(app.world_mut(), TOP);
    let e = climber_at(&mut app, Vec3::new(4.0, SKIN, 0.0), runner());
    // At a full run into the ladder, up it and three metres onto the floor above.
    walk(app.world_mut(), e, Vec3::NEG_X);
    let mut highest = 0.0f32;
    let ticks = drive(&mut app, 300, |w| {
        let (s, at) = (w.get::<MotionState>(e).unwrap(), w.get::<Transform>(e).unwrap().translation);
        highest = highest.max(at.y);
        !(s.mode == MotionMode::Ground && at.x < -3.0)
    });
    assert!(ticks < 300, "never ran onto the top: {:?} at {}", state(&app, e).mode, feet(&app, e));
    assert!(highest < TOP + 0.2, "{highest}");
    // Back at a full run, off the edge: the ladder catches it, and it climbs down and runs on.
    walk(app.world_mut(), e, Vec3::X);
    let ticks = drive(&mut app, 300, |w| {
        !(w.get::<MotionState>(e).unwrap().mode == MotionMode::Ground && w.get::<Transform>(e).unwrap().translation.x > 3.0)
    });
    assert!(ticks < 300, "never ran away from the foot: {:?} at {}", state(&app, e).mode, feet(&app, e));
    let ladder = (MotionMode::Ground, MotionMode::Ladder);
    let ground = (MotionMode::Ladder, MotionMode::Ground);
    assert_eq!(modes(&app, e), [ladder, ground, ladder, ground]);
    assert!(seen(&app).landed.is_empty(), "{:?}", seen(&app).landed);
}
