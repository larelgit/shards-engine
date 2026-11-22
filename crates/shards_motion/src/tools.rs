//! Movement tools for agents: `move_to`, `move`, `stop`, `jump`, `sprint`, `crouch`, `glide`.
//!
//! They act the way a player does, through `Intent` only (Player scope): orders for the
//! fixed tick to carry out, held buttons, one-shot presses. A refusal says why and what to do.

use ai_core::prelude::*;
use ai_core::spatial::{compass_vector, fmt_pos, relative};
use ai_core::tools::{register_tool, ToolKind};
use ai_core::AiIndex;
use bevy::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use shards_content::ContentDb;
use shards_core::input_names::{CROUCH, GLIDE, JUMP, SPRINT};
use shards_core::{Intent, Order, Target};

use crate::ai::MotionAi;
use crate::controller::{room_to_stand, CharacterController, CharacterParams, Crouch, LocomotionInput, MotionMode, MotionState};
use crate::modifiers::MoveModifiers;
use crate::intent::GlideUntilLanded;
use crate::perception::{self, reach_toward, Bodies, Hazard, PathPlan, StanceArg};
use crate::physics::{fresh, Collider, PhysicsWorld};

pub(crate) fn plugin(app: &mut App) {
    use ToolKind::Action;
    let world = app.world_mut();
    register_tool(world, "move_to", Scope::Player, Action, fresh::<MoveToArgs>.pipe(routed).pipe(move_to));
    register_tool(world, "move", Scope::Player, Action, move_dir);
    register_tool(world, "stop", Scope::Player, Action, stop);
    register_tool(world, "jump", Scope::Player, Action, fresh::<NoArgs>.pipe(jump));
    register_tool(world, "sprint", Scope::Player, Action, sprint);
    register_tool(world, "glide", Scope::Player, Action, glide);
    register_tool(world, "crouch", Scope::Player, Action, crouch);
    register_tool(world, "move_info", Scope::Player, ToolKind::Query, move_info);
}

/// Where to go: an entity id (42 or "e42") or a ground point {"x": 3, "z": -4}.
#[derive(Deserialize, JsonSchema, Clone, Copy, Debug)]
#[serde(untagged)]
pub enum TargetArg {
    Entity(Id),
    Point { x: f32, z: f32 },
}

/// The intent of the caller's body and where it stands.
fn my_intent<'a>(
    caller: &Caller,
    bodies: &'a mut Query<(&Transform, &mut Intent)>,
) -> Result<(Vec3, Mut<'a, Intent>), String> {
    let me = caller.require_subject()?;
    let (tf, intent) = bodies.get_mut(me).map_err(|_| "your body cannot be controlled (it has no Intent)".to_owned())?;
    Ok((tf.translation, intent))
}

/// Walk to an entity or a point. Replaces your current orders unless `queue` is set. The
/// game steers every tick; check progress with `observe` or `step`. By default it walks in
/// a straight line; `path` plans a route around walls and cliffs first (see `path_to`).
/// An order whose body gets no closer to the goal for 2 s (0.1 m) ends as `blocked` (journal, `observe`).
#[derive(Deserialize, JsonSchema)]
pub struct MoveToArgs {
    pub target: TargetArg,
    /// Stop this close to the target (meters). Default 1.5 for an entity, 0.3 for a point.
    pub within: Option<f32>,
    /// Append to the order queue instead of replacing it.
    #[serde(default)]
    pub queue: bool,
    /// Plan a route over what you can walk, swim and climb (like `path_to`) and follow it.
    /// The route starts where you stand, so with `queue` it needs no pending orders.
    #[serde(default)]
    pub path: bool,
}

/// What a [`RoutePlanner`](perception::RoutePlanner) (the navmesh) answers for `move_to path=true`; `None` when there
/// is none, it does not cover the walk, or the call is not one it can answer (the second half
/// then says what is wrong with it).
type Routed = Option<Result<PathPlan, String>>;

/// `move_to`'s first half: with `path`, ask the registered [`RoutePlanner`](perception::RoutePlanner).
fn routed(In(call): In<Call<MoveToArgs>>, world: &World) -> (Call<MoveToArgs>, Routed) {
    let answer = match (call.args.path, call.caller.subject) {
        (true, Some(me)) => {
            let default = if matches!(call.args.target, TargetArg::Entity(_)) { 1.5 } else { 0.3 };
            let within = call.args.within.unwrap_or(default).max(0.2);
            perception::planner_route(world, me, &call.args.target, Some(within), None, StanceArg::Current).map(|(_, answer)| answer)
        }
        _ => None,
    };
    (call, answer)
}

#[allow(clippy::too_many_arguments, reason = "a Bevy system: one parameter per thing it reads")]
fn move_to(
    In((call, routed)): In<(Call<MoveToArgs>, Routed)>,
    index: Res<AiIndex>,
    physics: Res<PhysicsWorld>,
    ai: Res<MotionAi>,
    db: Option<Res<ContentDb>>,
    positions: Query<&Transform>,
    perceiving: Bodies,
    mut bodies: Query<(&Transform, &mut Intent)>,
) -> ToolResult {
    let me = call.caller.require_subject()?;
    let MoveToArgs { target, within, queue, path } = call.args;
    let (goal, to, default_within) = match target {
        TargetArg::Entity(id) => {
            let entity = index.resolve(id.0)?;
            if entity == me {
                return Err(format!("{id} is you; give another entity or a point {{x, z}}"));
            }
            // An AI-visible entity need not be in the world (a director, a quest): the order
            // would be dropped by `follow_orders` on the next tick, which walks to a position.
            let Ok(there) = positions.get(entity) else {
                return Err(format!("{id} has no position, so move_to cannot walk to it; pass a point {{x, z}} or another entity"));
            };
            (there.translation, Target::Entity(id.0), 1.5)
        }
        TargetArg::Point { x, z } => {
            if !(x.is_finite() && z.is_finite()) {
                return Err("x and z must be finite numbers".into());
            }
            (Vec3::new(x, 0.0, z), Target::Point(Vec3::new(x, 0.0, z)), 0.3)
        }
    };
    let within = within.unwrap_or(default_within).max(0.2);
    let pending = bodies.as_readonly().get(me).map_or(0, |(_, intent)| intent.orders.len());
    if path && queue && pending > 0 {
        return Err(format!(
            "path=true plans a route from where you stand, which is not where your {pending} pending order(s) end: \
             `step` until they are done (or `stop`), then move_to with path=true and without `queue`; \
             or drop `path` to queue a straight line"
        ));
    }
    // Plan before touching the orders: a refused plan leaves them alone. The navmesh, when
    // it covers the walk; else the local grid.
    let plan = if path {
        let plan = match routed {
            Some(answer) => answer,
            None => perception::plan_for(me, &target, Some(within), None, StanceArg::Current, &index, &physics, &ai, db.as_deref(), &perceiving),
        };
        Some(plan.map_err(|why| format!("{why}. Without path=true move_to walks a straight line"))?)
    } else {
        None
    };
    let (here, mut intent) = my_intent(&call.caller, &mut bodies)?;
    if !queue {
        intent.orders.clear();
    }
    intent.aim = None;
    let Some(plan) = plan else {
        let order = Order::MoveTo { target: to, within };
        let desc = order.describe();
        intent.orders.push_back(order);
        return Ok(format!("ok: {desc}, {} away", relative(here, goal)));
    };
    // One order per leg end; passing a waypoint needs less precision than arriving. The
    // last leg already ends within `within` of the target: its order heads for the target
    // itself, so the walk stops `within` from the target, not `within` short of a point
    // that is itself that far short.
    let legs = plan.waypoints();
    if legs.is_empty() {
        // Nothing left to walk, and no order will zero the steering of the walk that was
        // under way (a plain `move_to` pops its order at once and does).
        if intent.orders.is_empty() {
            intent.move_dir = Vec3::ZERO;
        }
        return Ok(format!("ok: already within {within:.1}m"));
    }
    let last = legs.len() - 1;
    for (n, (at, _)) in legs.into_iter().enumerate() {
        let order = match n == last {
            true => Order::MoveTo { target: to, within },
            false => Order::MoveTo { target: Target::Point(at), within: WAYPOINT_WITHIN },
        };
        intent.orders.push_back(order);
    }
    Ok(format!("ok: following {}", plan.summary()))
}

/// How close a waypoint is passed (m): less than a grid cell, so the route stays on cells
/// the planner checked.
const WAYPOINT_WITHIN: f32 = 0.45;

/// Walk a distance in a compass direction (N = -Z, E = +X) in a straight line. An order that gets no closer for 2 s
/// (a wall) ends as `blocked`.
#[derive(Deserialize, JsonSchema)]
pub struct MoveArgs {
    /// N, NE, E, SE, S, SW, W or NW.
    pub dir: String,
    /// How far, at most 60.
    pub meters: f32,
}

fn move_dir(In(call): In<Call<MoveArgs>>, mut bodies: Query<(&Transform, &mut Intent)>) -> ToolResult {
    let dir = compass_vector(&call.args.dir)
        .ok_or_else(|| format!("unknown direction `{}`; use N, NE, E, SE, S, SW, W or NW", call.args.dir))?;
    if !call.args.meters.is_finite() || call.args.meters <= 0.0 {
        return Err("meters must be a positive number (at most 60)".into());
    }
    let (here, mut intent) = my_intent(&call.caller, &mut bodies)?;
    let goal = here + dir * call.args.meters.min(60.0);
    intent.orders.clear();
    intent.aim = None;
    intent.orders.push_back(Order::MoveTo { target: Target::Point(goal), within: 0.3 });
    Ok(format!("ok: moving to {}", fmt_pos(goal)))
}

/// Cancel all orders and stand still.
fn stop(In(call): In<Call<NoArgs>>, mut bodies: Query<(&Transform, &mut Intent)>) -> ToolResult {
    let (_, mut intent) = my_intent(&call.caller, &mut bodies)?;
    intent.orders.clear();
    intent.move_dir = Vec3::ZERO;
    intent.aim = None;
    Ok("ok: stopped".into())
}

/// The parts of a body the character tools look at.
type Characters<'w, 's> = Query<
    'w,
    's,
    (
        &'static Transform,
        &'static mut Intent,
        &'static MotionState,
        &'static LocomotionInput,
        Option<&'static CharacterController>,
        Option<&'static CharacterParams>,
    ),
>;

/// The caller's character: its intent to write, how it moves and what it moves by.
struct Me<'a> {
    entity: Entity,
    tf: Transform,
    intent: Mut<'a, Intent>,
    state: MotionState,
    input: LocomotionInput,
    params: CharacterParams,
}

fn my_character<'a>(caller: &Caller, bodies: &'a mut Characters, db: Option<&ContentDb>, what: &str) -> Result<Me<'a>, String> {
    let entity = caller.require_subject()?;
    let missing = || format!("your body cannot {what}: it has no character controller (CharacterController and Intent)");
    let params = {
        let (.., controller, own) = bodies.get(entity).map_err(|_| missing())?;
        controller.map_or_else(CharacterParams::default, |c| c.resolve(own, db).clone())
    };
    let (tf, intent, state, input, ..) = bodies.get_mut(entity).map_err(|_| missing())?;
    Ok(Me { entity, tf: *tf, intent, state: state.clone(), input: input.clone(), params })
}

/// Jump. A one-shot press, taken by the controller on the next tick. Works from the ground,
/// within a moment of walking off a ledge, and as a press just before landing; in water it
/// climbs out onto a ledge in front of you.
fn jump(
    In(call): In<Call<NoArgs>>,
    physics: Res<PhysicsWorld>,
    ai: Res<MotionAi>,
    db: Option<Res<ContentDb>>,
    mut bodies: Characters,
) -> ToolResult {
    let Me { entity, tf, mut intent, state, input, params } = my_character(&call.caller, &mut bodies, db.as_deref(), "jump")?;
    if input.locked {
        return Err("cannot jump now: an action holds your body (an attack or a dodge); wait for it to end".into());
    }
    let message = match state.mode {
        MotionMode::Mantle => return Err("cannot jump while climbing; it ends in a moment, then jump".into()),
        MotionMode::Glide => return Err("cannot jump while gliding; `glide on=false` first, or wait to land".into()),
        MotionMode::Ladder => "ok: jumping off the ladder".into(),
        MotionMode::Water => {
            // The controller climbs out when a ledge is within reach in front of the swimmer.
            let forward = tf.forward();
            let facing = Vec3::new(forward.x, 0.0, forward.z);
            let toward = if intent.move_dir.length_squared() > 1e-4 { intent.move_dir } else { facing };
            let reach = params.climb_reach();
            // Probe past the reach, so a ledge just out of it is named (and the reach itself is covered).
            let probe = (reach + 0.9).max(1.2);
            let (distance, hazard) =
                reach_toward(&physics, &params, tf.translation, state.water_surface, toward, probe, ai.safe_drop, Some(entity));
            // The one rule for what is climbed (a swimmer's rise counts its skin), the same the
            // controller, the planner and `clearance` ask.
            match hazard {
                Hazard::Ledge { rise } if params.can_jump_out(rise) && distance <= reach + 0.02 => {
                    format!("ok: jumping out of the water onto the ledge {distance:.1}m ahead (+{rise:.1}m)")
                }
                // The controller climbs out only from right at the ledge: an "ok" here would
                // press a jump that does nothing.
                Hazard::Ledge { rise } if params.can_jump_out(rise) => {
                    return Err(format!(
                        "the ledge is {distance:.1}m ahead (+{rise:.1}m), out of reach from the water: swim within \
                         {reach:.1}m of it and jump (a low enough ledge is also climbed by swimming into it)"
                    ));
                }
                _ => {
                    // The highest ledge top the jump climbs, over the water.
                    let over_water = params.climb_out_max_height.unwrap_or(params.mantle_height - params.float_depth);
                    return Err(format!(
                        "cannot jump while swimming: there is no ledge within 1m in front of you that you can climb \
                         (at most {over_water:.1}m above the water); swim to the shore or use move_to to climb out"
                    ));
                }
            }
        }
        MotionMode::Air if state.coyote_left <= 0.0 => format!(
            "ok: pressed jump in the air; it only works within {:.2}s of leaving the ground or right before landing",
            params.jump_buffer
        ),
        MotionMode::Air => format!("ok: jumping (coyote time left {:.2}s)", state.coyote_left),
        MotionMode::Ground => "ok: jumping".into(),
    };
    intent.press(JUMP);
    Ok(message)
}

/// Hold sprint (or let go with `on=false`): faster on the ground while you move at running
/// speed. Stays on until you turn it off.
#[derive(Deserialize, JsonSchema)]
pub struct OnArgs {
    /// Default true.
    pub on: Option<bool>,
}

fn sprint(In(call): In<Call<OnArgs>>, mut bodies: Query<(&Transform, &mut Intent)>) -> ToolResult {
    let on = call.args.on.unwrap_or(true);
    let (_, mut intent) = my_intent(&call.caller, &mut bodies)?;
    intent.set_held(SPRINT, on);
    Ok(if on {
        "ok: sprint on; it applies while you move at running speed on the ground (turn it off with on=false)".into()
    } else {
        "ok: sprint off".into()
    })
}

/// Crouch (or let go with `hold=false`): lower your body while the crouch is held, which makes you a
/// smaller target, lowers your eye and slows your walk, and passes under what a standing body cannot.
/// On the ground your feet stay and your head comes down; in the air your head stays and your feet are
/// pulled up, so a crouch in a jump clears a higher ledge. Standing up waits for room: under a low
/// ceiling you stay crouched until you move out. Your locomotion needs a `crouch_height`.
#[derive(Deserialize, JsonSchema)]
pub struct CrouchArgs {
    /// Hold the crouch (default true); false lets go and stands up as soon as there is room.
    pub hold: Option<bool>,
}

#[allow(clippy::too_many_arguments, reason = "a Bevy system: one parameter per thing it reads")]
fn crouch(
    In(call): In<Call<CrouchArgs>>,
    physics: Res<PhysicsWorld>,
    db: Option<Res<ContentDb>>,
    mut bodies: Characters,
    stances: Query<(Option<&Crouch>, Option<&MoveModifiers>, &Collider)>,
) -> ToolResult {
    let hold = call.args.hold.unwrap_or(true);
    let Me { entity, tf, mut intent, state, params, .. } = my_character(&call.caller, &mut bodies, db.as_deref(), "crouch")?;
    let (stance, modifiers, collider) = stances.get(entity).map_err(|_| "your body has no collider to crouch with".to_owned())?;
    let stance = stance.copied().unwrap_or_default();
    let Some(crouched) = params.crouch_height else {
        if hold {
            return Err("your body cannot crouch: its locomotion has no `crouch_height`".into());
        }
        intent.set_held(CROUCH, false);
        return Ok("ok: crouch off (your body cannot crouch anyway)".into());
    };
    if hold {
        if modifiers.is_some_and(|m| m.rooted) {
            return Err("cannot crouch now: your movement is taken away (rooted); wait for it to end".into());
        }
        if state.mode == MotionMode::Mantle {
            return Err("cannot crouch while climbing; it ends in a moment, then crouch".into());
        }
        intent.set_held(CROUCH, true);
        let secs = params.crouch_time;
        let how = match (secs > 0.0, matches!(state.mode, MotionMode::Air | MotionMode::Glide)) {
            (true, true) => format!("your feet are pulled up {:.1}m over {secs:.2}s", params.height - crouched),
            (false, true) => format!("your feet are pulled up {:.1}m", params.height - crouched),
            (true, false) => format!("you go down to {crouched:.1}m over {secs:.2}s"),
            (false, false) => format!("you go down to {crouched:.1}m"),
        };
        return Ok(if stance.depth >= 1.0 {
            "ok: crouch held; you are crouched already".into()
        } else {
            let cap = params.crouch_speed.map_or(String::new(), |v| format!(", walking at most {v:.1}m/s"));
            format!("ok: crouch held; {how}{cap}")
        });
    }
    intent.set_held(CROUCH, false);
    if stance.depth <= 0.0 {
        return Ok("ok: crouch off; you are standing".into());
    }
    // The same cast the controller makes each tick, so the answer is the one the next ticks give.
    let air = matches!(state.mode, MotionMode::Air | MotionMode::Glide);
    let (room, lacking) = room_to_stand(&physics, &params, (entity, collider.layers), air, tf.translation, stance.depth);
    if room + 1e-4 < lacking {
        let (side, fix) = if air { ("below your feet", "land") } else { ("above your head", "move out from under it") };
        return Ok(format!(
            "ok: crouch off, but there is no room to stand: {:.2}m more is needed {side}; you stay crouched until you {fix}",
            lacking - room
        ));
    }
    Ok(format!("ok: crouch off; you stand up over {:.2}s", params.crouch_time * stance.depth))
}

/// Glide (or stop with `on=false`): while you fall, drop slowly and steer, and ride
/// updrafts. Glide is held until you land or ask to stop; asking on the ground arms it
/// for your next fall.
#[derive(Deserialize, JsonSchema)]
pub struct GlideArgs {
    /// Default true.
    pub on: Option<bool>,
}

fn glide(In(call): In<Call<GlideArgs>>, mut commands: Commands, db: Option<Res<ContentDb>>, mut bodies: Characters) -> ToolResult {
    let on = call.args.on.unwrap_or(true);
    let Me { entity, mut intent, state, .. } = my_character(&call.caller, &mut bodies, db.as_deref(), "glide")?;
    if !on {
        intent.set_held(GLIDE, false);
        commands.entity(entity).remove::<GlideUntilLanded>();
        return Ok("ok: glide off".into());
    }
    let message = match state.mode {
        MotionMode::Water => return Err("cannot glide in water; swim out first".into()),
        MotionMode::Mantle => return Err("cannot glide while climbing; wait until you stand".into()),
        MotionMode::Ladder => return Err("cannot glide on a ladder; `jump` off it first".into()),
        MotionMode::Glide => "ok: gliding (ends when you land; stop with on=false)",
        MotionMode::Air if state.velocity.y >= 0.0 => "ok: glide set; it opens once you start to fall",
        MotionMode::Air => "ok: gliding (ends when you land; stop with on=false)",
        MotionMode::Ground => "ok: glide armed; jump or walk off an edge and it opens once you fall, then ends on landing",
    };
    intent.set_held(GLIDE, true);
    commands.entity(entity).insert(GlideUntilLanded { flown: matches!(state.mode, MotionMode::Air | MotionMode::Glide) });
    Ok(message.into())
}

/// What moves a character, and how fast it can go right now: its modifiers (slow, caps, root,
/// sprint cost), the walk, run and sprint speeds they leave (m/s), the swim speeds, the
/// wading factor, how the ground and the air steer the body (`accel`: the model and its numbers)
/// and its horizontal speed now (`speed`). Read-only.
#[derive(Deserialize, JsonSchema)]
pub struct MoveInfoArgs {
    /// The character: 42 or "e42". Default: you.
    pub id: Option<Id>,
}

/// How the ground and the air steer the body (F7), with the numbers the model reads: `step`
/// (`acceleration`, `deceleration`, `air_control`, `air_deceleration`) or `projected` (`ground_accel`, `air_accel`,
/// `air_speed_cap`, `friction`, `stop_speed`, `max_air_speed`: null for no cap).
fn accel_info(params: &CharacterParams) -> serde_json::Value {
    use crate::controller::AccelModel;
    match params.accel {
        AccelModel::Step => serde_json::json!({
            "model": params.accel.name(),
            "acceleration": params.acceleration,
            "deceleration": params.deceleration,
            "air_control": params.air_control,
            "air_deceleration": params.air_deceleration,
        }),
        AccelModel::Projected { ground_accel, air_accel, air_speed_cap, friction, stop_speed, max_air_speed } => serde_json::json!({
            "model": params.accel.name(),
            "ground_accel": ground_accel,
            "air_accel": air_accel,
            "air_speed_cap": air_speed_cap,
            "friction": friction,
            "stop_speed": stop_speed,
            "max_air_speed": max_air_speed,
        }),
    }
}

fn move_info(In(call): In<Call<MoveInfoArgs>>, world: &World) -> ToolResult<serde_json::Value> {
    let entity = match call.args.id {
        Some(id) => id.entity(world)?,
        None => call.caller.require_subject()?,
    };
    let e = world.entity(entity);
    let Some(controller) = e.get::<CharacterController>() else {
        return Err("it is not a character (no CharacterController)".to_owned());
    };
    let params = controller.resolve(e.get::<CharacterParams>(), world.get_resource::<ContentDb>());
    let m = e.get::<crate::MoveModifiers>().copied().unwrap_or_default();
    let round = |v: f32| (v * 100.0).round() / 100.0;
    let speed = |amount: f32, sprint: bool| round(m.ground_speed(params, amount, sprint).0);
    // The depth wading reads (water over the feet plus the params' offset) and the factor it makes.
    let depth = e
        .get::<MotionState>()
        .filter(|s| params.wades_in(s.mode))
        .and_then(|s| s.water_surface.map(|surface| (surface, s.mode)))
        .zip(e.get::<Transform>())
        .map(|((surface, mode), tf)| params.wade_depth_for(mode, surface, tf.translation.y));
    let wade = depth.map(|depth| params.wade_scale(depth));
    Ok(serde_json::json!({
        "modifiers": m.describe(),
        "factor": m.factor,
        "ground_factor": m.ground_factor,
        "cap": m.cap,
        "unsprinted_cap": m.unsprinted_cap,
        "no_sprint": m.no_sprint,
        "rooted": m.rooted,
        "sprint_cost": m.sprint_cost,
        "wade": m.wade,
        "stance": m.stance.map(|s| serde_json::json!({ "top": s.top, "by_input": s.by_input })),
        "acceleration": m.acceleration,
        "deceleration": m.deceleration,
        "walk": speed(0.5, false),
        "run": speed(1.0, false),
        "sprint": speed(1.0, true),
        "swim": round(m.swim_speed(params.swim_speed)),
        "swim_fast": (params.swim_fast_speed > 0.0).then(|| round(m.swim_speed(params.swim_fast_speed))),
        "wade_factor": wade.map(round),
        "wade_depth": depth.map(round),
        "wade_depth_offset": params.wade_depth_offset,
        "wade_air_depth_offset": params.wade_air_depth_offset,
        "climb_out_reach": params.climb_reach(),
        "turn_speed_deg": m.turn_speed_deg,
        "turn_free_deg": params.turn_speed_deg,
        "turn_locked_deg": (params.locked_turn_speed_deg > 0.0).then_some(params.locked_turn_speed_deg),
        "turn_swim_deg": (params.swim_turn_speed_deg > 0.0).then_some(params.swim_turn_speed_deg),
        "turn_toward_input": params.turn_toward_input,
        "entry_max_speed": (params.entry_max_speed > 0.0).then_some(params.entry_max_speed),
        "buoyancy_omega": (params.buoyancy_omega > 0.0).then_some(params.buoyancy_omega),
        "climb_out_time": params.climb_out_time.unwrap_or(params.mantle_time),
        "climb_out_min_facing": params.climb_out_min_facing,
        "crouch_height": params.crouch_height,
        "crouch_speed": params.crouch_speed,
        "crouch_time": params.can_crouch().then_some(params.crouch_time),
        "crouch_depth": e.get::<Crouch>().map(|c| round(c.depth)),
        "accel": accel_info(params),
        "speed": e.get::<MotionState>().map(|s| round(crate::controller::mover::flat(s.velocity).length())),
    }))
}
