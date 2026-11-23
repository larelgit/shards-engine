//! Replays end to end: record a session, play it back in a fresh world, and get the
//! same game — or a report of the first tick where it stops being the same.

use sandbox::gameplay::Player;
use sandbox::SandboxPlugin;
use shards::ai_core::replay::{self, Replay, ReplayEvent};
use shards::ai_core::{call_tool, AiJournal, AiReplay, CheckDetail, EntityChange, JournalKind, Scope};
use shards::bevy::input::keyboard::Key;
use shards::prelude::*;
use shards::serde_json::{self, json, Value};

use crate::common::{ok, state, BOSS};

/// What the keyboard does: straight into the player's `Intent`, between ticks.
fn press(app: &mut App, dir: Vec3) {
    let world = app.world_mut();
    let mut q = world.query_filtered::<&mut Intent, With<Player>>();
    let mut intent = q.single_mut(world).unwrap();
    intent.orders.clear();
    intent.move_dir = dir;
}

/// Gameplay events only: agent, sim and log lines differ between a recording and
/// its playback (the playback reports itself).
fn story(app: &App) -> Vec<String> {
    let journal = app.world().resource::<AiJournal>();
    journal.after(0).0.filter(|e| e.kind == JournalKind::Event).map(ToString::to_string).collect()
}

/// A session with every kind of input a replay has to capture: dev edits, player
/// tools, keyboard, a spawn, possession, and a call after the last tick.
fn session(app: &mut App) {
    ok(app, "set", json!({"id": BOSS, "component": "Health", "path": "current", "value": 310}));
    ok(app, "attack", json!({"target": BOSS, "repeat": true}));
    ok(app, "step", json!({"ticks": 100}));
    press(app, Vec3::X);
    ok(app, "step", json!({"ticks": 20}));
    press(app, Vec3::ZERO);
    ok(app, "spawn_prefab", json!({"id": "skeleton", "x": 4, "z": 6}));
    ok(app, "possess", json!({"id": "e7"}));
    ok(app, "set", json!({"id": "e7", "component": "Health", "path": "current", "value": 5}));
    ok(app, "heal", json!({})); // heals "you": e7
    ok(app, "possess", json!({}));
    ok(app, "dodge", json!({"away_from": BOSS}));
    ok(app, "step", json!({"ticks": 200}));
    ok(app, "teleport", json!({"id": "e15", "x": -3, "z": 0}));
    ok(app, "step", json!({"ticks": 37}));
    ok(app, "move", json!({"dir": "W", "meters": 2}));
}

fn recorded(seed: u64) -> (App, Replay) {
    let mut app = sandbox::test_app(seed);
    session(&mut app);
    let replay = replay::save(app.world());
    // Through JSON, like a file.
    let replay = shards::serde_json::from_str(&shards::serde_json::to_string(&replay).unwrap()).unwrap();
    (app, replay)
}

fn report(app: &App) -> shards::ai_core::ReplayReport {
    app.world().resource::<AiReplay>().report().cloned().expect("playback has not finished")
}

/// Plays `replay` to its end in a fresh game, like `--verify`.
fn played(replay: Replay) -> App {
    let mut copy = sandbox::test_app(replay.seed);
    let end = replay.end.tick;
    replay::play(copy.world_mut(), replay).unwrap();
    ok(&mut copy, "step", json!({"ticks": end}));
    copy.update();
    copy
}

/// The first recorded call's arguments, to change as a newer build would.
fn first_call(replay: &mut Replay) -> &mut Value {
    let first = replay.events.iter_mut().find_map(|e| match e {
        ReplayEvent::Call { args, .. } => Some(args),
        _ => None,
    });
    first.expect("a recorded call")
}

fn journal(app: &App, kind: JournalKind) -> Vec<String> {
    app.world().resource::<AiJournal>().after(0).0.filter(|e| e.kind == kind).map(ToString::to_string).collect()
}

#[test]
fn playback_reproduces_the_session() {
    let (mut original, replay) = recorded(42);
    assert_eq!(replay.end.tick, 357);
    assert!(
        replay.events.iter().any(|e| matches!(e, ReplayEvent::Input { at: 100, .. })),
        "keyboard input not recorded"
    );
    assert!(
        !replay
            .events
            .iter()
            .any(|e| matches!(e, ReplayEvent::Call { tool, .. } if tool == "step" || tool == "possess")),
        "control tools must not be recorded"
    );

    let mut copy = sandbox::test_app(42);
    replay::play(copy.world_mut(), replay.clone()).unwrap();
    ok(&mut copy, "step", json!({"ticks": 50}));
    // Keys pressed while a replay plays are ignored.
    press(&mut copy, Vec3::NEG_X);
    ok(&mut copy, "step", json!({"ticks": 307}));
    copy.update(); // the trailing `move` call happens between ticks, like in the original

    let report = report(&copy);
    assert!(report.identical(), "{report}");
    assert_eq!(report.matched as usize, replay.checks.len() + 1);
    assert_eq!(state(&mut original), state(&mut copy));
    assert_eq!(story(&original), story(&copy));
    // The skeleton spawned mid-session exists in the copy too, and fights.
    assert!(story(&copy).iter().any(|l| l.contains("e15 -> e5 claw")), "{:#?}", story(&copy));
}

#[test]
fn a_changed_game_reports_the_first_diverging_tick() {
    let (_, mut replay) = recorded(42);
    // As if the boss had spawned with less health in a newer build.
    let ReplayEvent::Call { args, .. } = &mut replay.events[0] else { panic!("first event is the set call") };
    args["value"] = json!(299);

    let mut copy = sandbox::test_app(42);
    replay::play(copy.world_mut(), replay).unwrap();
    ok(&mut copy, "step", json!({"ticks": 357}));
    copy.update();

    let report = report(&copy);
    assert_eq!(report.diverged_at, Some(60), "{report}");
    assert!(report.to_string().contains("DIVERGED at tick 60"), "{report}");
    let errors = journal(&copy, JournalKind::Error);
    assert!(errors.iter().any(|e| e.contains("replay diverged at tick 60")), "{errors:?}");
}

#[test]
fn a_divergence_names_the_entity_the_component_and_its_value() {
    let (_, mut replay) = recorded(42);
    // Still above half health, so the boss stays in phase 1 and only its health differs.
    first_call(&mut replay)["value"] = json!(320);
    let copy = played(replay);

    let report = report(&copy);
    assert_eq!(report.headline(), "replay finished at tick 357: DIVERGED at tick 60: e6 Boss: Health differs");
    assert_eq!(report.value_lines(), ["e6 now: Health { current: 320.0, max: 600.0 }"]);
    let errors = journal(&copy, JournalKind::Error);
    assert!(errors.iter().any(|e| e.ends_with("replay diverged at tick 60: e6 Boss: Health differs")), "{errors:?}");
    let values = journal(&copy, JournalKind::Sim);
    assert!(values.iter().any(|e| e.ends_with("replay: e6 now: Health { current: 320.0, max: 600.0 }")), "{values:?}");
    // Only the first divergence is explained: later checks follow from it.
    assert_eq!(errors.iter().filter(|e| e.contains("replay diverged")).count(), 1, "{errors:?}");
}

#[test]
fn a_moved_pillar_is_the_only_entity_named() {
    let mut original = sandbox::test_app(42);
    ok(&mut original, "teleport", json!({"id": "e1", "x": -19, "z": -19}));
    ok(&mut original, "attack", json!({"target": BOSS, "repeat": true}));
    ok(&mut original, "step", json!({"ticks": 180}));
    let mut replay = replay::save(original.world());
    // The same corner, a meter off.
    first_call(&mut replay)["x"] = json!(-18);
    let copy = played(replay);

    let report = report(&copy);
    let found = report.divergence.as_ref().unwrap();
    assert_eq!(found.tick, 60, "{report}");
    assert_eq!(found.entities.len(), 1, "{report}");
    assert_eq!(found.entities[0].id, AiId(1));
    assert!(
        matches!(&found.entities[0].change, EntityChange::Differs { components, .. } if components == &["Transform"]),
        "{report}"
    );
    assert!(report.headline().ends_with("DIVERGED at tick 60: e1 Pillar: Transform differs"), "{report}");
    let [value] = report.value_lines().try_into().unwrap();
    assert!(value.starts_with("e1 now: Transform { translation: Vec3(-18.0, 0.0, -19.0)"), "{value}");
}

#[test]
fn checks_every_tick_name_the_exact_tick() {
    let record = |every: u64| {
        let mut app = sandbox::test_app_with(|config| config.check_every = every);
        ok(&mut app, "step", json!({"ticks": 37}));
        ok(&mut app, "set", json!({"id": BOSS, "component": "Health", "path": "current", "value": 400}));
        ok(&mut app, "step", json!({"ticks": 83}));
        let replay = replay::save(app.world());
        let mut replay: Replay = serde_json::from_str(&serde_json::to_string(&replay).unwrap()).unwrap();
        first_call(&mut replay)["value"] = json!(390);
        replay
    };
    let every_tick = record(1);
    assert_eq!(every_tick.check_every, 1);
    assert_eq!(every_tick.checks.len(), 120, "one at every tick from 1 to 120");
    // The call came after tick 37: the state differs from tick 38 on.
    let report_1 = report(&played(every_tick));
    assert_eq!(report_1.diverged_at, Some(38), "{report_1}");
    assert!(report_1.headline().ends_with("DIVERGED at tick 38: e6 Boss: Health differs"), "{report_1}");
    let report_60 = report(&played(record(60)));
    assert_eq!(report_60.diverged_at, Some(60), "{report_60}");
}

#[test]
fn the_check_detail_decides_what_the_report_names() {
    let reported = |detail: CheckDetail| {
        let mut app = sandbox::test_app_with(|config| config.check_detail = detail);
        session(&mut app);
        let mut replay = replay::save(app.world());
        assert_eq!(replay.check_detail, detail);
        first_call(&mut replay)["value"] = json!(320);
        report(&played(replay))
    };
    let entities = reported(CheckDetail::Entities);
    assert!(entities.headline().ends_with("DIVERGED at tick 60: e6 Boss differs"), "{entities}");
    assert!(entities.value_lines().is_empty(), "{entities}");
    let none = reported(CheckDetail::None);
    assert!(
        none.headline().ends_with("DIVERGED at tick 60: no details: the recording keeps hashes only (check detail none)"),
        "{none}"
    );
    // Hashes alone keep the file small; the hash itself is the same at every detail.
    let (_, full) = recorded(42);
    let mut bare = sandbox::test_app_with(|config| config.check_detail = CheckDetail::None);
    session(&mut bare);
    let bare = replay::save(bare.world());
    assert_eq!(full.checks.iter().map(|c| c.hash).collect::<Vec<_>>(), bare.checks.iter().map(|c| c.hash).collect::<Vec<_>>());
    assert!(bare.checks.iter().all(|c| c.parts.is_none()));
    assert!(serde_json::to_string(&bare).unwrap().len() * 4 < serde_json::to_string(&full).unwrap().len());
}

#[test]
fn missing_and_extra_entities_are_named_and_the_list_is_capped() {
    let mut original = sandbox::test_app(42);
    for i in 0..12 {
        ok(&mut original, "spawn_prefab", json!({"id": "skeleton", "x": -18 + 3 * i, "z": 18}));
    }
    ok(&mut original, "step", json!({"ticks": 60}));
    let replay = replay::save(original.world());

    // A newer build where those spawns never happen: e15..e26 are missing.
    let mut fewer = replay.clone();
    fewer.events.retain(|e| !matches!(e, ReplayEvent::Call { tool, .. } if tool == "spawn_prefab"));
    let gone = report(&played(fewer));
    let found = gone.divergence.as_ref().unwrap();
    assert_eq!(found.entities.len(), 10, "{gone}");
    assert_eq!(found.more, 2, "{gone}");
    assert!(found.entities.iter().all(|e| matches!(e.change, EntityChange::Missing { .. })), "{gone}");
    let listed: Vec<String> = (15..25).map(|id| format!("e{id} missing")).collect();
    // What the spawned skeletons read is missing too: their claw, and the director they share.
    let unread = "replay check content differs: action/claw missing, director/default missing";
    assert!(
        gone.headline().ends_with(&format!("DIVERGED at tick 60: {}; … and 2 more; {unread}", listed.join("; "))),
        "{gone}"
    );

    // And one where a thirteenth one joins them: e27 is extra.
    let mut more = replay;
    more.events.push(ReplayEvent::Call {
        at: 0,
        agent: "dev".into(),
        scope: Scope::Dev,
        subject: None,
        tool: "spawn_prefab".into(),
        args: json!({"id": "skeleton", "x": 18, "z": 15}),
    });
    let joined = report(&played(more));
    assert!(joined.headline().ends_with("DIVERGED at tick 60: e27 extra (Skeleton)"), "{joined}");
}

#[test]
fn a_despawned_entity_is_missing_with_what_became_of_it() {
    let mut original = sandbox::test_app(42);
    ok(&mut original, "step", json!({"ticks": 60}));
    let mut replay = replay::save(original.world());
    // A newer build removes the skeleton at tick 30.
    replay.events.push(ReplayEvent::Call {
        at: 30,
        agent: "dev".into(),
        scope: Scope::Dev,
        subject: None,
        tool: "despawn".into(),
        args: json!({"id": "e7"}),
    });
    let report = report(&played(replay));
    assert!(report.headline().ends_with("DIVERGED at tick 60: e7 missing (Skeleton despawned at tick 30)"), "{report}");
}

#[test]
fn a_version_2_replay_verifies_by_its_hashes() {
    let (_, replay) = recorded(42);
    // What a build before details wrote: no interval, no detail, no breakdowns.
    let mut v2 = serde_json::to_value(&replay).unwrap();
    v2["format"] = json!("shards-replay/2");
    let file = v2.as_object_mut().unwrap();
    file.remove("check_every");
    file.remove("check_detail");
    file["end"].as_object_mut().unwrap().remove("parts");
    for check in file["checks"].as_array_mut().unwrap() {
        check.as_object_mut().unwrap().remove("parts");
    }
    // The content parts stay: this recording's hashes are of them (a file without them is
    // checked with every definition hashed, as the builds before them did).
    assert!(v2.get("check_parts").is_some());
    assert!(v2["checks"].as_array().unwrap().iter().chain([&v2["end"]]).all(|c| c.get("parts").is_none()));
    let v2: Replay = serde_json::from_value(v2).unwrap();

    let same = report(&played(v2.clone()));
    assert!(same.identical(), "{same}");
    assert_eq!(same.matched as usize, replay.checks.len() + 1);

    let mut changed = v2;
    first_call(&mut changed)["value"] = json!(320);
    let copy = played(changed);
    let report = report(&copy);
    assert_eq!(report.headline(), "replay finished at tick 357: DIVERGED at tick 60: no details: replay format 2");
    assert!(report.value_lines().is_empty());
    let errors = journal(&copy, JournalKind::Error);
    assert!(errors.iter().any(|e| e.ends_with("replay diverged at tick 60: no details: replay format 2")), "{errors:?}");
}

#[test]
fn an_action_during_playback_takes_over() {
    let (_, replay) = recorded(42);
    let mut copy = sandbox::test_app(42);
    replay::play(copy.world_mut(), replay).unwrap();
    ok(&mut copy, "step", json!({"ticks": 130}));
    ok(&mut copy, "teleport", json!({"id": "e5", "x": 0, "z": 0}));

    let report = report(&copy);
    assert_eq!(report.interrupted.as_deref(), Some("dev took over (teleport)"), "{report}");
    assert_eq!(report.tick, 130);
    assert!(!copy.world().resource::<AiReplay>().is_playing());
    // From here on the game is live.
    ok(&mut copy, "step", json!({"ticks": 10}));
    assert_eq!(shards::ai_core::sim::tick(copy.world()), 140);
}

#[test]
fn a_branch_saves_as_one_replay() {
    let (_, replay) = recorded(42);
    let mut branch = sandbox::test_app(42);
    replay::play(branch.world_mut(), replay).unwrap();
    ok(&mut branch, "step", json!({"ticks": 357}));
    branch.update();
    assert!(report(&branch).identical());
    // Live again: play on differently.
    ok(&mut branch, "attack", json!({"target": "e8", "repeat": true}));
    ok(&mut branch, "step", json!({"ticks": 90}));
    let whole = replay::save(branch.world());
    assert_eq!(whole.end.tick, 447);

    let mut again = sandbox::test_app(42);
    replay::play(again.world_mut(), whole).unwrap();
    ok(&mut again, "step", json!({"ticks": 447}));
    again.update();
    assert!(report(&again).identical(), "{}", report(&again));
    assert_eq!(state(&mut branch), state(&mut again));
}

#[test]
fn calls_made_inside_a_tool_are_recorded_once() {
    /// A game tool built from other tools: spawns a pair of skeletons.
    fn pair(In(call): In<Call<NoArgs>>, world: &mut World) -> ToolResult {
        for x in [-2, 2] {
            let out = call_tool(world, &call.caller.agent, "spawn_prefab", json!({"id": "skeleton", "x": x, "z": 8}));
            if !out.ok {
                return Err(out.result.to_string());
            }
        }
        Ok("two skeletons".into())
    }
    let app_with_pair = || {
        let mut app = sandbox::test_app(42);
        app.add_ai_tool("pair", Scope::Dev, pair);
        app
    };

    let mut original = app_with_pair();
    ok(&mut original, "pair", json!({}));
    ok(&mut original, "step", json!({"ticks": 90}));
    let replay = replay::save(original.world());
    let tools: Vec<&str> = replay
        .events
        .iter()
        .filter_map(|e| match e {
            ReplayEvent::Call { tool, .. } => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tools, ["pair"]);

    let mut copy = app_with_pair();
    replay::play(copy.world_mut(), replay).unwrap();
    ok(&mut copy, "step", json!({"ticks": 90}));
    copy.update();
    assert!(report(&copy).identical(), "{}", report(&copy));
    assert_eq!(state(&mut original), state(&mut copy), "the inner spawns ran twice or not at all");
}

#[test]
fn playback_needs_a_fresh_world_with_the_same_seed() {
    let (_, replay) = recorded(42);

    let mut other_seed = sandbox::test_app(7);
    let err = replay::play(other_seed.world_mut(), replay.clone()).unwrap_err();
    assert!(err.contains("recorded with seed 42; this world has seed 7"), "{err}");

    let mut started = sandbox::test_app(42);
    ok(&mut started, "step", json!({"ticks": 10}));
    let err = replay::play(started.world_mut(), replay.clone()).unwrap_err();
    assert!(err.contains("fresh world at tick 0; this one is at tick 10"), "{err}");

    let mut edited = sandbox::test_app(42);
    ok(&mut edited, "heal", json!({}));
    let err = replay::play(edited.world_mut(), replay).unwrap_err();
    assert!(err.contains("already recorded 1 events"), "{err}");
}

#[test]
fn replay_save_writes_a_file_that_loads_back() {
    let (mut app, replay) = recorded(42);
    let name = format!("replays/test/save-{}.json", std::process::id());
    let out = ok(&mut app, "replay_save", json!({"path": name}));
    let text = out.result.as_str().unwrap();
    assert!(text.contains("357 ticks") && text.contains("seed 42"), "{text}");
    let path = std::env::current_dir().unwrap().join(&name);
    let loaded = replay::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(loaded, replay);
}

#[test]
fn replay_save_writes_only_inside_replays() {
    let mut app = sandbox::test_app(42);
    let outside = std::env::temp_dir().join(format!("sandbox_replay_{}.json", std::process::id()));
    for path in [outside.display().to_string(), "replays/../escaped.json".into(), "boss.json".into()] {
        let out = call_tool(app.world_mut(), "dev", "replay_save", json!({"path": path}));
        assert!(!out.ok, "{path} must be refused");
        let err = out.result.as_str().unwrap();
        assert!(err.starts_with("path must be inside replays/, e.g. replays/boss-fight.json"), "{err}");
    }
    assert!(
        !outside.exists()
            && !std::path::Path::new("escaped.json").exists()
            && !std::path::Path::new("boss.json").exists()
    );
}

#[test]
fn checks_hash_the_game_not_the_text_the_ai_reads() {
    let (_, replay) = recorded(42);
    // A newer build that describes the same game differently.
    let mut copy = sandbox::test_app(42);
    copy.register_ai_describer("mood", |e| e.contains::<Health>().then(|| "grim".to_owned()));
    let seen = ok(&mut copy, "observe", json!({}));
    assert!(seen.result.as_str().unwrap().contains("grim"), "the extra describer is in use");
    replay::play(copy.world_mut(), replay).unwrap();
    ok(&mut copy, "step", json!({"ticks": 357}));
    copy.update();
    assert!(report(&copy).identical(), "{}", report(&copy));
}

#[test]
fn a_version_1_replay_is_refused_with_the_reason() {
    let (_, mut replay) = recorded(42);
    replay.format = "shards-replay/1".into();
    let mut copy = sandbox::test_app(42);
    let err = replay::play(copy.world_mut(), replay).unwrap_err();
    assert!(err.contains("recorded by an older build") && err.contains("record it again"), "{err}");
}

/// Cosmetic state: may differ between a recording and its playback.
#[derive(Component, Reflect, Default)]
#[reflect(Component, Default)]
struct Sparkle(f32);

/// Game state outside AI-visible entities.
#[derive(Resource, Default)]
struct Weather(u64);

/// The game with a sparkle on the player and a weather check; what the checks cover is
/// part of the build, the same for the recording and its playback.
fn sparkling_app(ignore_sparkle: bool) -> App {
    let mut app = sandbox::test_app(42);
    app.register_type::<Sparkle>()
        .init_resource::<Weather>()
        .add_replay_check("weather", |w| w.resource::<Weather>().0);
    if ignore_sparkle {
        app.replay_ignore::<Sparkle>();
    }
    let player = app.world().resource::<shards::ai_core::AiIndex>().get(AiId(5)).unwrap();
    app.world_mut().entity_mut(player).insert(Sparkle(0.0));
    app
}

/// Records the session, then plays it back changing the sparkle and/or the weather
/// directly in the world after 100 ticks.
fn meddle(ignore_sparkle: bool, sparkle: bool, weather: bool) -> shards::ai_core::ReplayReport {
    let mut original = sparkling_app(ignore_sparkle);
    session(&mut original);
    let replay = replay::save(original.world());

    let mut app = sparkling_app(ignore_sparkle);
    replay::play(app.world_mut(), replay).unwrap();
    ok(&mut app, "step", json!({"ticks": 100}));
    if sparkle {
        let player = app.world().resource::<shards::ai_core::AiIndex>().get(AiId(5)).unwrap();
        app.world_mut().get_mut::<Sparkle>(player).unwrap().0 = 1.0;
    }
    if weather {
        app.world_mut().resource_mut::<Weather>().0 = 7;
    }
    ok(&mut app, "step", json!({"ticks": 257}));
    app.update();
    report(&app)
}

#[test]
fn games_choose_what_the_checks_cover() {
    assert!(meddle(true, false, false).identical());
    let ignored = meddle(true, true, false);
    assert!(ignored.identical(), "replay_ignore::<Sparkle>: {ignored}");
    let counted = meddle(false, true, false);
    assert_eq!(counted.diverged_at, Some(120), "without replay_ignore the sparkle counts: {counted}");
    let weather = meddle(true, false, true);
    assert_eq!(weather.diverged_at, Some(120), "add_replay_check covers the resource: {weather}");
}

/// A gameplay component that points at another entity.
#[derive(Component, Reflect)]
#[reflect(Component)]
struct Grudge(Entity);

#[test]
fn a_window_does_not_change_the_hash() {
    let mut app = sandbox::test_app(42);
    let before = replay::state_hash(app.world());
    // What the window's visuals add to gameplay entities: all engine types.
    let ids: Vec<Entity> = {
        let index = app.world().resource::<shards::ai_core::AiIndex>();
        index.ids().into_iter().filter_map(|id| index.get(id)).collect()
    };
    for entity in ids {
        app.world_mut().entity_mut(entity).insert((Visibility::default(), GlobalTransform::from_xyz(1.0, 2.0, 3.0)));
        app.world_mut().spawn((Transform::default(), ChildOf(entity)));
    }
    assert_eq!(replay::state_hash(app.world()), before, "visual components are not game data");
    ok(&mut app, "set", json!({"id": "e6", "component": "Health", "path": "current", "value": 10}));
    assert_ne!(replay::state_hash(app.world()), before, "game data is");
}

#[test]
fn an_entity_in_a_component_hashes_as_its_id() {
    // Render entities shift `Entity` indices between a window and headless; ids stay.
    let grudge = |app: &mut App| {
        app.register_type::<Grudge>();
        let index = app.world().resource::<shards::ai_core::AiIndex>();
        let (player, boss) = (index.get(AiId(5)).unwrap(), index.get(AiId(6)).unwrap());
        app.world_mut().entity_mut(player).insert(Grudge(boss));
        boss
    };
    let mut a = sandbox::test_app(42);
    // In b five more entities come before the world: the same boss, another `Entity`.
    let mut b = shards::testing::test_app(
        |config| config.seed = 42,
        |app| {
            app.add_plugins(SandboxPlugin).add_systems(Startup, |mut commands: Commands| {
                commands.spawn_batch((0..5).map(|_| Transform::default()));
            });
        },
    );
    let (boss_a, boss_b) = (grudge(&mut a), grudge(&mut b));
    assert_ne!(boss_a, boss_b);
    assert_eq!(replay::state_hash(a.world()), replay::state_hash(b.world()));
}

/// Reflects, but cannot be serialized: opaque without `ReflectSerialize`.
#[derive(Reflect, Clone, Default)]
#[reflect(opaque)]
#[allow(dead_code)] // only reflection looks inside
struct Secret(u8);

#[derive(Component, Reflect, Default)]
#[reflect(Component, Default)]
struct Locked {
    secret: Secret,
}

#[test]
fn a_component_that_does_not_serialize_is_left_out_once() {
    let mut app = sandbox::test_app(42);
    app.register_type::<Locked>();
    let player = app.world().resource::<shards::ai_core::AiIndex>().get(AiId(5)).unwrap();
    app.world_mut().entity_mut(player).insert(Locked::default());
    let before = replay::state_hash(app.world());
    app.world_mut().get_mut::<Locked>(player).unwrap().secret = Secret(9);
    assert_eq!(replay::state_hash(app.world()), before, "left out of the hash");
    ok(&mut app, "step", json!({"ticks": 180}));
    let warnings: Vec<String> = app
        .world()
        .resource::<AiJournal>()
        .after(0)
        .0
        .filter(|e| e.kind == JournalKind::Warn && e.text.contains("leave out"))
        .map(ToString::to_string)
        .collect();
    assert_eq!(warnings.len(), 1, "once, not at every check: {warnings:?}");
    assert!(warnings[0].contains("replays::Locked: it does not serialize"), "{warnings:?}");
}

#[test]
fn verify_prints_what_diverged_and_its_value_now() {
    let (_, mut replay) = recorded(42);
    first_call(&mut replay)["value"] = json!(320);
    let path = replay::write(&replay, &format!("replays/test/diverged-{}.json", std::process::id())).unwrap();
    let verify = std::process::Command::new(env!("CARGO_BIN_EXE_sandbox")).arg("--replay").arg(&path).arg("--verify").output().unwrap();
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert_eq!(verify.status.code(), Some(1), "{stdout}{}", String::from_utf8_lossy(&verify.stderr));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].starts_with("replay finished at tick 357: DIVERGED at tick 60: e6 Boss: Health differs [357 ticks in "), "{stdout}");
    assert_eq!(lines[1], "  e6 now: Health { current: 320.0, max: 600.0 }");
}

#[test]
fn check_flags_are_for_recording() {
    // Every call names a replay that does not exist: a flag let through by mistake ends
    // in an error, never in a game that runs until the test times out.
    let refused = |flags: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_sandbox"))
            .args(["--replay", "replays/test/none.json"])
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{flags:?}");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let zero = refused(&["--check-every", "0"]);
    assert!(zero.contains("invalid value '0' for '--check-every <TICKS>'"), "{zero}");
    let detail = refused(&["--check-detail", "all"]);
    assert!(detail.contains("unknown check detail `all`: use components, entities or none"), "{detail}");
    // A replay checks at its own ticks.
    let both = refused(&["--check-every", "1"]);
    assert!(both.contains("the argument '--replay <FILE>' cannot be used with '--check-every <TICKS>'"), "{both}");
}

/// A game that draws, as far as the interpolation can tell (`EngineConfig::draws`), without a GPU:
/// every character body gets a visual child, like the one a window's `dress` gives the model.
fn drawing_app(seed: u64, interpolation: bool) -> App {
    let mut app = sandbox::test_app(seed);
    app.world_mut().resource_mut::<shards::EngineConfig>().offscreen = true;
    app.insert_resource(shards::settings::GraphicsSettings { interpolation, ..default() });
    let bodies: Vec<Entity> = {
        let world = app.world_mut();
        world.query_filtered::<Entity, With<shards::motion::CharacterController>>().iter(world).collect()
    };
    for body in bodies {
        app.world_mut().spawn((Transform::from_xyz(0.0, 0.5, 0.0), ChildOf(body)));
    }
    app
}

/// Plays `replay` with a frame every ten ticks, so the picture's systems run between ticks.
fn played_with_frames(mut app: App, replay: Replay) -> App {
    let end = replay.end.tick;
    replay::play(app.world_mut(), replay).unwrap();
    app.update();
    for _ in 0..end / 10 {
        ok(&mut app, "step", json!({"ticks": 10}));
        app.update();
    }
    ok(&mut app, "step", json!({"ticks": end % 10}));
    app.update();
    app
}

/// How many visual children are blended right now (`Interpolated::shown`), and how many have a history
/// in which the body moved (the previous tick's pose differs from the last).
fn blended(app: &mut App) -> (usize, usize) {
    let world = app.world_mut();
    let mut q = world.query::<(&shards::motion::Interpolated, &Transform)>();
    let all: Vec<_> = q.iter(world).collect();
    let shown = all.iter().filter(|(i, _)| i.shown().is_some()).count();
    let moved = all.iter().filter(|(i, _)| i.previous() != i.current()).count();
    (shown, moved)
}

#[test]
fn render_interpolation_changes_no_game_state() {
    let (original, replay) = recorded(42);
    let want = replay::state_hash(original.world());

    // Headless: the interpolation does nothing at all, and the replay verifies.
    let mut headless = played_with_frames(sandbox::test_app(42), replay.clone());
    assert!(report(&headless).identical(), "{}", report(&headless));
    assert_eq!(replay::state_hash(headless.world()), want);
    assert_eq!(blended(&mut headless), (0, 0));

    // Drawing, interpolation on: bodies are blended between ticks, and the game is the same.
    let mut on = played_with_frames(drawing_app(42, true), replay.clone());
    assert!(report(&on).identical(), "{}", report(&on));
    assert_eq!(replay::state_hash(on.world()), want);
    let (shown, moved) = blended(&mut on);
    assert!(shown > 0, "no body was blended: the test would prove nothing");
    assert!(moved > 0, "no body moved between its last two ticks");

    // Drawing, interpolation off: the same game, the children where the game left them.
    let mut off = played_with_frames(drawing_app(42, false), replay);
    assert!(report(&off).identical(), "{}", report(&off));
    assert_eq!(replay::state_hash(off.world()), want);
    assert_eq!(blended(&mut off), (0, 0));
}

/// The player's setting that turns the attack assist off (`controls.attack_assist`) makes the
/// controls hold `assist_off` on the local player. A recording made with the assist on must
/// still play back identically on a machine whose setting is off: playback undoes input changes
/// nobody recorded (`inputs_between_ticks`), the way it undoes keys pressed while watching.
#[test]
fn a_replay_with_the_assist_on_verifies_on_a_machine_with_it_off() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/replays/timeline.json");
    let recording = replay::load(&path).expect("the timeline reference replay");
    assert!(
        !recording.events.iter().any(|e| matches!(e, ReplayEvent::Input { value, .. } if value.to_string().contains("assist_off"))),
        "the recording was made with the assist on"
    );
    let end = recording.end.tick;
    let mut app = sandbox::test_app(recording.seed);
    app.world_mut().resource_mut::<shards::settings::ControlSettings>().attack_assist = false;
    replay::play(app.world_mut(), recording).unwrap();
    let mut held_while_playing = false;
    for _ in 0..end {
        ok(&mut app, "step", json!({"ticks": 1}));
        let world = app.world_mut();
        let mut q = world.query_filtered::<&Intent, With<Player>>();
        held_while_playing |= q.single(world).is_ok_and(|i| i.is_held("assist_off"));
    }
    app.update();
    let report = report(&app);
    assert!(report.identical(), "{report}");
    assert!(!held_while_playing, "playback let the setting's input reach the replayed body");
}

/// Gives every fighter the animset and the humanoid's model as the sandbox's `dress` does where
/// something is drawn, bodies that spawn during the replay included.
fn dress_fighters(
    mut commands: Commands,
    bodies: Query<Entity, (With<Fighter>, Without<shards::action::Animated>)>,
    assets: Res<AssetServer>,
) {
    for body in &bodies {
        let model = shards::action::render::GltfModel::load(&assets, "models/humanoid.glb");
        commands
            .entity(body)
            .insert(shards::action::Animated::new("humanoid"))
            .with_child(model);
    }
}

/// The picture never feeds the simulation (animation design 10.2). The headless sandbox draws
/// nothing, so no body has `Animated`; this one dresses every fighter with the humanoid, as a
/// window would, and plays the combat prototype's reference replay with frames between the ticks.
/// The bridge binds the players, spawns the sword under a hand bone, and the replay still verifies
/// identical and the game's state hash is the one of the bare run. The `anim` status shows the
/// bridge did work, so the test cannot pass by doing nothing.
#[test]
fn a_replay_verifies_with_animated_bodies_and_the_picture_does_work() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/replays/combat_prototype.json");
    let replay: Replay = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let bare = played(replay.clone());
    assert!(report(&bare).identical(), "{}", report(&bare));
    let want = replay::state_hash(bare.world());

    let mut app = sandbox::test_app(replay.seed);
    app.add_systems(Update, dress_fighters);
    // The model loads on the asset server's threads: wait for the player to be bound first.
    let started = std::time::Instant::now();
    while shards::ai_core::status::status(app.world())["anim"]["bound"].as_u64() == Some(0) {
        assert!(started.elapsed() < std::time::Duration::from_secs(60), "no body was bound to a model");
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let end = replay.end.tick;
    replay::play(app.world_mut(), replay).unwrap();
    app.update();
    let (mut props, mut bound, mut on_hip) = (0, 0, false);
    let mut sample = |app: &mut App| {
        let world = app.world_mut();
        let mut q = world.query::<&shards::action::render::AnimationBound>();
        on_hip |= q.iter(world).any(|b| b.place("sword") == Some(&shards::action::Place::Socket("hip".into())));
        let app = &*app;
        let anim = shards::ai_core::status::status(app.world())["anim"].clone();
        props = props.max(anim["props"].as_u64().unwrap());
        bound = bound.max(anim["bound"].as_u64().unwrap());
    };
    for _ in 0..end / 10 {
        ok(&mut app, "step", json!({"ticks": 10}));
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(2));
        sample(&mut app);
    }
    ok(&mut app, "step", json!({"ticks": end % 10}));
    app.update();
    sample(&mut app);
    assert!(report(&app).identical(), "{}", report(&app));
    assert_eq!(replay::state_hash(app.world()), want, "the picture changed the game");
    assert!(bound > 0, "no body was bound");
    assert!(props > 0, "no prop was drawn: the test proves nothing");
    assert!(on_hip, "no sword was ever on the hip: the drink's mount never ran");
}

/// What the bridge drew while a replay was played with animated bodies and a frame after every tick.
#[derive(Default, Debug)]
struct Swim {
    swam: bool,
    sword_on_back: bool,
    sword_in_hand_afloat: bool,
    dropped: bool,
    left_events: usize,
}

/// Plays the reference replay `file` with every fighter dressed (as `a_replay_verifies_with_animated_bodies...`),
/// checks it verifies identical with the bare run's state hash, and reports what was drawn of the motion modes.
fn played_swimming(file: &str) -> Swim {
    #[derive(Resource, Default)]
    struct Lefts(usize);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/replays").join(file);
    let replay: Replay = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let bare = played(replay.clone());
    assert!(report(&bare).identical(), "{}", report(&bare));
    let want = replay::state_hash(bare.world());

    let mut app = sandbox::test_app(replay.seed);
    app.init_resource::<Lefts>();
    app.add_observer(|_: On<shards::action::ModeLeft>, mut lefts: ResMut<Lefts>| lefts.0 += 1);
    app.add_systems(Update, dress_fighters);
    let started = std::time::Instant::now();
    while shards::ai_core::status::status(app.world())["anim"]["bound"].as_u64() == Some(0) {
        assert!(started.elapsed() < std::time::Duration::from_secs(60), "no body was bound to a model");
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let end = replay.end.tick;
    replay::play(app.world_mut(), replay).unwrap();
    app.update();
    let mut swim = Swim::default();
    for _ in 0..end {
        ok(&mut app, "step", json!({"ticks": 1}));
        app.update();
        let world = app.world_mut();
        let mut q = world.query::<&shards::action::render::AnimationBound>();
        for b in q.iter(world) {
            let water = b.mode() == Some(shards::motion::MotionMode::Water);
            swim.swam |= water;
            swim.sword_on_back |= water && b.place("sword") == Some(&shards::action::Place::Socket("back".into()));
            swim.sword_in_hand_afloat |= water && b.drop_depth() > 0.0 && b.place("sword") == Some(&shards::action::Place::Socket("hand_r".into()));
            swim.dropped |= b.drop_depth() > 0.0;
        }
    }
    swim.left_events = app.world().resource::<Lefts>().0;
    assert!(report(&app).identical(), "{}", report(&app));
    assert_eq!(replay::state_hash(app.world()), want, "the picture changed the game");
    swim
}

/// The same for the motion modes (animation design 10.2, D1.2). The course replay swims across the
/// pool and climbs out: the bridge draws the sword on the back in the water and fires `ModeLeft` for the
/// water once the body is out; the body-motion replay swims a stroke, which keeps the sword in the hand
/// and lowers the model. Neither changes the replay.
#[test]
fn a_replay_with_a_swim_verifies_with_animated_bodies_in_the_water() {
    let course = played_swimming("obstacle_course.json");
    assert!(course.swam, "no body was drawn swimming: the test proves nothing: {course:?}");
    assert!(course.sword_on_back, "no sword rode the back afloat: the mode's mount never ran: {course:?}");
    assert!(course.left_events > 0, "the draw after the water never began: {course:?}");
    let strokes = played_swimming("body_motion.json");
    assert!(strokes.swam && strokes.sword_in_hand_afloat, "the stroke never made the rules step aside: {strokes:?}");
    assert!(strokes.dropped, "the model was never lowered: {strokes:?}");
}

/// A frame of a screen twice as fast as the ticks (120 Hz over 60 ticks a second): a tick runs every
/// other frame.
const FAST_FRAME: std::time::Duration = std::time::Duration::from_nanos(8_333_333);

/// The sandbox played in real time on such a screen.
fn on_a_fast_screen(seed: u64) -> App {
    let mut app = sandbox::test_app(seed);
    app.insert_resource(shards::bevy::time::TimeUpdateStrategy::ManualDuration(FAST_FRAME));
    app
}

fn frames(app: &mut App, n: u32) {
    for _ in 0..n {
        app.update();
    }
}

/// `code` goes down for a few frames and comes up, as a window tells Bevy.
fn tap(app: &mut App, code: KeyCode, logical: Key) {
    use shards::bevy::input::keyboard::KeyboardInput;
    use shards::bevy::input::ButtonState;
    for state in [ButtonState::Pressed, ButtonState::Released] {
        let key = KeyboardInput { key_code: code, logical_key: logical.clone(), state, text: None, repeat: false, window: Entity::PLACEHOLDER };
        app.world_mut().write_message(key);
        frames(app, 3);
    }
}

/// Whether the next frame runs a tick: its time and what the last left over make one.
fn ticks_next_frame(app: &App) -> bool {
    let fixed = app.world().resource::<Time<Fixed>>();
    fixed.overstep() + FAST_FRAME >= fixed.timestep()
}

/// What the menus did, and at which tick.
fn menus(app: &App) -> Vec<(u64, String)> {
    let journal = app.world().resource::<AiJournal>();
    journal.after(0).0.filter(|e| e.kind == JournalKind::Local && e.text.starts_with("ui: ")).map(|e| (e.tick, e.text.clone())).collect()
}

/// A pause menu takes effect after its frame's time was counted (in `First`; the key opens it in
/// `PreUpdate`), so the session runs that frame's tick under it, and the calls made in it stand a tick
/// after the call that opened it. Played back, the opening call comes inside a tick: the replay must not
/// stop the clock there, or it never reaches the tick of the calls that close the menu (playtest-1's
/// stalled session).
#[test]
fn a_replay_with_a_pause_menu_in_it_plays_through_on_a_fast_screen() {
    use shards::ai_core::sim;
    use shards::core::PauseState;
    let mut original = on_a_fast_screen(42);
    original.update();
    sim::resume(original.world_mut());
    frames(&mut original, 31);
    while !ticks_next_frame(&original) {
        original.update();
    }
    let opened_at = sim::tick(original.world());
    tap(&mut original, KeyCode::Escape, Key::Escape);
    assert_eq!(sim::tick(original.world()), opened_at + 1, "the frame that opened the menu ran its tick");
    assert!(sim::is_paused(original.world()), "the menu paused the game");
    // In the menu: down to Settings, into it, back out of it and out of the pause.
    tap(&mut original, KeyCode::ArrowDown, Key::ArrowDown);
    tap(&mut original, KeyCode::Enter, Key::Enter);
    tap(&mut original, KeyCode::Escape, Key::Escape);
    tap(&mut original, KeyCode::Escape, Key::Escape);
    frames(&mut original, 40);
    assert!(!sim::is_paused(original.world()) && sim::tick(original.world()) > opened_at + 10, "the game runs again");
    sim::pause(original.world_mut());
    let replay: Replay = serde_json::from_str(&serde_json::to_string(&replay::save(original.world())).unwrap()).unwrap();
    let calls: Vec<(u64, String)> = replay
        .events
        .iter()
        .filter_map(|e| match e {
            ReplayEvent::Call { at, tool, args, .. } => Some((*at, format!("{tool} {args}"))),
            _ => None,
        })
        .collect();
    let (open, inside) = (opened_at, opened_at + 1);
    let want: Vec<(u64, String)> = [
        (open, r#"ui_open {"screen":"pause"}"#),
        (inside, r#"ui_navigate {"dir":"down"}"#),
        (inside, r#"ui_navigate {"dir":"confirm"}"#),
        (inside, r#"ui_navigate {"dir":"back"}"#),
        (inside, r#"ui_navigate {"dir":"back"}"#),
    ]
    .into_iter()
    .map(|(at, call)| (at, call.to_owned()))
    .collect();
    assert_eq!(calls, want, "the menu's calls stand a tick after the one that opened it");
    let seen = menus(&original);
    assert!(seen.iter().any(|(_, line)| line == "ui: opened settings"), "the test went into Settings: {seen:#?}");

    // Played back on the same screen: through the menu and on to the end.
    let mut window = on_a_fast_screen(42);
    replay::play(window.world_mut(), replay.clone()).unwrap();
    window.update();
    sim::resume(window.world_mut());
    for _ in 0..4 * replay.end.tick + 20 {
        if window.world().resource::<AiReplay>().report().is_some() {
            break;
        }
        window.update();
    }
    let Some(report) = window.world().resource::<AiReplay>().report().cloned() else {
        panic!("the playback stopped at tick {} of {}", sim::tick(window.world()), replay.end.tick);
    };
    assert!(report.identical(), "{report}");
    assert_eq!(menus(&window), seen, "the menus opened and closed at the same ticks");
    assert_eq!(*window.world().resource::<State<PauseState>>().get(), PauseState::Running);
    assert!(!sim::is_paused(window.world()), "nothing left the game paused");
}

/// A session that ends under the pause menu (an agent stepped the game under it) plays on under the menu
/// to its end, and then the game pauses under it, as the session was.
#[test]
fn a_replay_that_ends_under_the_pause_menu_leaves_the_game_paused() {
    use shards::ai_core::sim;
    use shards::core::PauseState;
    let mut original = on_a_fast_screen(42);
    original.update();
    sim::resume(original.world_mut());
    frames(&mut original, 30);
    tap(&mut original, KeyCode::Escape, Key::Escape);
    assert!(sim::is_paused(original.world()), "the menu paused the game");
    ok(&mut original, "step", json!({"ticks": 30}));
    let replay: Replay = serde_json::from_str(&serde_json::to_string(&replay::save(original.world())).unwrap()).unwrap();

    let mut window = on_a_fast_screen(42);
    replay::play(window.world_mut(), replay.clone()).unwrap();
    window.update();
    sim::resume(window.world_mut());
    for _ in 0..4 * replay.end.tick + 20 {
        if window.world().resource::<AiReplay>().report().is_some() {
            break;
        }
        window.update();
    }
    let Some(report) = window.world().resource::<AiReplay>().report().cloned() else {
        panic!("the playback stopped at tick {} of {}", sim::tick(window.world()), replay.end.tick);
    };
    assert!(report.identical(), "{report}");
    frames(&mut window, 4);
    assert_eq!(*window.world().resource::<State<PauseState>>().get(), PauseState::Paused, "the menu is up");
    assert!(sim::is_paused(window.world()), "and the game paused under it once the replay was over");
    let at = sim::tick(window.world());
    frames(&mut window, 10);
    assert_eq!(sim::tick(window.world()), at);
}
