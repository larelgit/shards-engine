//! Looped sounds placed in the world (emitters) and walls between the ears and a sound (occlusion):
//! the mixer's voices for emitters, the priority rules, the muffling by colliders, the tools, the
//! content check, and a level file that spawns an emitter. No output device is needed: the mixer
//! decides the same without one.

use serde_json::json;
use shards::ai_core::{JournalKind, replay};
use shards::audio::{emitter_play, emitter_stop, play, refresh, stop, Bus, Mixer, Played, PlaySound, SoundAt, SoundEmitter, SoundTransmission};
use shards::content::{check_dir, ContentKinds};
use shards::motion::{Collider, CollisionLayers, Shape};
use shards::prelude::*;

use crate::*;

/// Looped sounds: a hum heard to 20 m, a drip to 10, a rain heard everywhere at half volume; and a weak
/// one-shot.
const AMBIENT: &str = r#"{
    "hum": (files: ["sounds/hum.wav"], priority: 40, spatial: (near: 2.0, far: 20.0), looped: true),
    "drip": (files: ["sounds/hum.wav"], priority: 40, spatial: (near: 1.0, far: 10.0), looped: true),
    "rain": (files: ["sounds/hum.wav"], priority: 30, volume: 0.5, looped: true),
    "faint": (files: ["sounds/beep.wav"], priority: 10),
}"#;

/// The tests' mixer, four voices, with walls muffling a share of the sound: 0.3 behind an ordinary one.
const WALLS: &str = r#"{"mixer": (voices: 4, music: "theme", occlusion: Some((gain: 0.3)), sounds: {"healed": "beep"})}"#;

/// A game with the ambient sounds and the mixer `mixer`; the folder lives as long as the game.
fn ambient(name: &str, mixer: &str) -> (Folder, App) {
    let assets = Folder::new(name);
    assets.put("content/audio/ambient.sound.ron", AMBIENT);
    assets.put("content/audio/mixer.audio.ron", mixer);
    std::fs::write(assets.path().join("sounds/hum.wav"), tone(1.0, 100.0)).unwrap();
    let app = game(&assets);
    (assets, app)
}

fn emit(app: &mut App, name: &str, sound: &str, at: Vec3) -> Entity {
    app.world_mut().spawn((Name::new(name.to_owned()), SoundEmitter::new(sound), Transform::from_translation(at))).id()
}

/// A static box (a wall, a pane) with its own name, `size` across, centred on `at`.
fn solid(app: &mut App, name: &str, at: Vec3, size: Vec3) -> Entity {
    app.world_mut().spawn((Name::new(name.to_owned()), Collider::new(Shape::cuboid(size * 0.5)), Transform::from_translation(at))).id()
}

fn move_hero(app: &mut App, to: Vec3) {
    let hero = entity(app, HERO);
    app.world_mut().get_mut::<Transform>(hero).unwrap().translation = to;
}

/// The physics world catches up with colliders spawned or removed (it does on every frame).
fn settle(app: &mut App) {
    app.update();
}

fn started(app: &App) -> u64 {
    app.world().resource::<Mixer>().started
}

/// The voice of sound `sound`'s emitter `name`.
fn voice_of(app: &App, emitter: Entity) -> Option<shards::audio::Voice> {
    voices(app).into_iter().find(|v| v.emitter == Some(emitter))
}

#[test]
fn a_looped_emitter_holds_a_voice_in_earshot_lets_it_go_beyond_far_and_takes_one_again() {
    let (_assets, mut app) = ambient("loop", MIXER_PLAIN);
    let vent = emit(&mut app, "Vent", "hum", Vec3::new(0.0, 0.0, -5.0));
    step(&mut app, 1);
    let heard = voices(&app);
    assert_eq!(heard.len(), 1, "{heard:?}");
    let v = &heard[0];
    assert_eq!((v.sound.as_str(), v.looped, v.emitter, v.source, v.position), ("hum", true, Some(vent), Some(vent), Some(Vec3::new(0.0, 0.0, -5.0))));
    // 5 m with `near` 2 and `far` 20: (1 - 3/18)².
    near(v.gain, ops::powf(1.0 - 3.0 / 18.0, 2.0));
    assert_eq!(v.direction, Some(Vec3::NEG_Z));

    // A loop does not play out with its file (1 s).
    wait(&mut app, 3.0);
    assert_eq!(playing(&app), ["hum #1"]);

    // Out of earshot (35 m, heard to 20 m): the voice goes, and it is no drop.
    move_hero(&mut app, Vec3::new(0.0, 0.0, 30.0));
    step(&mut app, 1);
    assert!(playing(&app).is_empty(), "{:?}", playing(&app));
    assert_eq!(app.world().resource::<Mixer>().dropped, 0, "an emitter out of earshot is not a dropped sound");
    assert_eq!(journal(&app, JournalKind::Local, "emitter Vent (hum) out of earshot"), ["emitter Vent (hum) out of earshot (35.0 m, heard to 20 m)"]);
    wait(&mut app, 2.0);
    assert_eq!(started(&app), 1, "it does not start again and again out of earshot");

    // Back within earshot: a new voice, the loop from its start.
    move_hero(&mut app, Vec3::new(0.0, 0.0, -1.0));
    step(&mut app, 1);
    assert_eq!(playing(&app), ["hum #2"]);
    assert_eq!(journal(&app, JournalKind::Local, "emitter Vent (hum) in earshot"), ["emitter Vent (hum) in earshot: voice #1", "emitter Vent (hum) in earshot: voice #2"]);
}

/// The mixer the tests have unless a test asks for walls.
const MIXER_PLAIN: &str = r#"{"mixer": (voices: 4, music: "theme", sounds: {"healed": "beep"})}"#;

#[test]
fn refreshing_again_with_nothing_changed_starts_nothing() {
    let (_assets, mut app) = ambient("idempotent", MIXER_PLAIN);
    emit(&mut app, "Vent", "hum", Vec3::new(0.0, 0.0, -5.0));
    emit(&mut app, "Far", "hum", Vec3::new(0.0, 0.0, -50.0));
    step(&mut app, 1);
    let (count, next) = (started(&app), playing(&app));
    assert_eq!((count, next.len()), (1, 1));
    for _ in 0..3 {
        refresh(app.world_mut());
        app.update();
    }
    assert_eq!((started(&app), playing(&app)), (count, next), "no second voice, no new id");
}

#[test]
fn an_emitter_starts_and_stops_by_its_flag_the_api_and_the_tools_and_goes_with_its_entity() {
    let (_assets, mut app) = ambient("playing", MIXER_PLAIN);
    let vent = emit(&mut app, "Vent", "hum", Vec3::new(0.0, 0.0, -5.0));
    let stopped = app.world_mut().spawn((Name::new("Quiet"), SoundEmitter::new("drip").stopped(), Transform::from_xyz(0.0, 0.0, -3.0))).id();
    step(&mut app, 1);
    assert_eq!(playing(&app), ["hum #1"], "a stopped emitter holds no voice");
    // Its voice is not cut short as a one-shot's is: the emitter would take one again.
    let refusal = "voice #1 (hum) is a sound emitter's: switch the emitter off (`emitter_stop`) instead";
    assert_eq!(stop(app.world_mut(), 1, 0.1), Err(refusal.to_owned()));

    emitter_stop(app.world_mut(), vent).unwrap();
    step(&mut app, 1);
    assert!(playing(&app).is_empty());
    assert_eq!(journal(&app, JournalKind::Local, "emitter Vent (hum) stopped").len(), 1);
    emitter_play(app.world_mut(), stopped).unwrap();
    step(&mut app, 1);
    assert_eq!(playing(&app), ["drip #2"]);

    // The tools do the same, and say what the emitter does now.
    let said = ok(&mut app, "emitter_play", json!({"emitter": "Vent"}));
    assert!(said.starts_with("Vent hum 5.0m heard #3 gain 0.69"), "{said}");
    assert_eq!(ok(&mut app, "emitter_stop", json!({"emitter": "hum"})), "Vent hum 5.0m stopped", "by its sound, when only one plays it");
    let missing = refused(&mut app, "emitter_stop", json!({"emitter": "Vnet"}));
    assert_eq!(missing, "no emitter `Vnet`; emitters: Quiet (drip), Vent (hum)");
    let plain = Folder::new("no_emitters");
    let alone = refused(&mut game(&plain), "emitter_play", json!({"emitter": "Vent"}));
    assert!(alone.starts_with("no emitters here: put a `SoundEmitter` on an entity"), "{alone}");

    // The emitter's entity goes: so does its voice.
    app.world_mut().despawn(stopped);
    step(&mut app, 1);
    assert!(playing(&app).is_empty(), "{:?}", playing(&app));
    assert_eq!(emitter_play(app.world_mut(), stopped), Err(format!("{} has no SoundEmitter", ai_core_label(&app, stopped))));
}

fn ai_core_label(app: &App, entity: Entity) -> String {
    shards::ai_core::label(app.world(), entity)
}

#[test]
fn loops_keep_their_voices_against_weaker_one_shots_and_give_them_up_to_stronger_ones() {
    let (_assets, mut app) = ambient("priority", MIXER_PLAIN);
    let drips: Vec<Entity> = (0..4).map(|i| emit(&mut app, &format!("Drip{i}"), "drip", Vec3::new(i as f32, 0.0, -3.0))).collect();
    step(&mut app, 1);
    assert_eq!(playing(&app).len(), 4, "four voices, four emitters in earshot");
    let begun = started(&app);

    // A one-shot of a lower priority (10 against the drips' 40) never takes a loop's voice.
    let faint = play(app.world_mut(), "faint", SoundAt::Everywhere);
    assert_eq!(faint, Ok(Played::Dropped("all 4 voices play sounds as strong or stronger".into())));
    assert!(drips.iter().all(|d| voice_of(&app, *d).is_some()));

    // A stronger one (50) takes the weakest loop's: the quietest, the farthest drip.
    let beep = play(app.world_mut(), "beep", SoundAt::Everywhere).unwrap();
    assert_eq!(beep, Played::Started { id: begun + 1, stole: Some("drip #4 (voice limit 4)".into()) });
    assert!(voice_of(&app, drips[3]).is_none() && drips[..3].iter().all(|d| voice_of(&app, *d).is_some()));
    // The emitter waits for a voice, and says so; it does not count as a drop and does not ask for one it cannot have.
    step(&mut app, 3);
    assert_eq!(app.world().resource::<Mixer>().dropped, 1, "only the faint one-shot");
    let said = ok(&mut app, "audio", json!({}));
    assert!(said.contains("emitters 4 (3 heard)\n"), "{said}");
    assert!(said.contains("Drip3 drip 4.2m waiting for a voice: all 4 play sounds as strong or stronger"), "{said}");
    assert_eq!(started(&app), begun + 1, "waiting starts nothing");

    // When the one-shot has played (0.5 s), the emitter has its voice again.
    wait(&mut app, 0.6);
    assert_eq!(playing(&app).len(), 4, "{:?}", playing(&app));
    assert!(voice_of(&app, drips[3]).is_some());
    assert_eq!(started(&app), begun + 2);
}

#[test]
fn a_looped_sound_without_a_place_is_heard_everywhere() {
    let (_assets, mut app) = ambient("rain", MIXER_PLAIN);
    let rain = emit(&mut app, "Rain", "rain", Vec3::new(500.0, 0.0, 500.0));
    step(&mut app, 1);
    let v = voice_of(&app, rain).expect("rain has no `spatial`, so no distance silences it");
    assert_eq!((v.position, v.distance, v.gain), (None, None, 0.5), "its volume, whole");
    wait(&mut app, 2.0);
    assert_eq!(playing(&app).len(), 1);
}

#[test]
fn an_emitter_that_cannot_play_says_why_once() {
    let (_assets, mut app) = ambient("broken", MIXER_PLAIN);
    emit(&mut app, "Typo", "hom", Vec3::new(0.0, 0.0, -2.0));
    emit(&mut app, "Once", "beep", Vec3::new(0.0, 0.0, -2.0));
    emit(&mut app, "Blank", "", Vec3::new(0.0, 0.0, -2.0));
    wait(&mut app, 1.0);
    assert!(playing(&app).is_empty());
    let warned = journal(&app, JournalKind::Warn, "cannot play");
    assert_eq!(warned.len(), 3, "once each, not at every refresh: {warned:?}");
    assert!(warned.contains(&"audio: emitter Typo (hom) cannot play: no sound `hom` (did you mean `hum`?)".to_owned()), "{warned:?}");
    assert!(warned.contains(&"audio: emitter Once (beep) cannot play: `beep` does not loop: an emitter plays a sound with `looped: true`".to_owned()), "{warned:?}");
    assert!(warned.contains(&"audio: emitter Blank () cannot play: it names no sound".to_owned()), "{warned:?}");
    let said = ok(&mut app, "audio", json!({}));
    assert!(said.contains("Typo hom 2.0m cannot play: no sound `hom` (did you mean `hum`?)"), "{said}");
}

#[test]
fn a_looped_sound_is_refused_as_a_one_shot_everywhere_but_an_emitter() {
    let (_assets, mut app) = ambient("one_shot", MIXER_PLAIN);
    let refusal = play(app.world_mut(), "hum", SoundAt::Everywhere).unwrap_err();
    assert!(refusal.starts_with("sound `hum` loops, so it plays only from a `SoundEmitter`"), "{refusal}");
    let said = refused(&mut app, "sound_play", json!({"sound": "hum", "point": [0.0, 0.0, -3.0]}));
    assert!(said.contains("loops, so it plays only from a `SoundEmitter`"), "{said}");
    // The engine's own paths (an event, a cue) say so once in the journal, and start nothing.
    for _ in 0..2 {
        app.world_mut().trigger(PlaySound::new("hum", SoundAt::Everywhere));
        app.world_mut().flush();
    }
    assert!(playing(&app).is_empty());
    let warned = journal(&app, JournalKind::Warn, "loops");
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(warned[0].starts_with("audio: PlaySound: sound `hum` loops"), "{warned:?}");
}

#[test]
fn nothing_of_placed_sound_is_in_the_replay_hash() {
    let (_assets, mut app) = ambient("hash", MIXER_PLAIN);
    step(&mut app, 1);
    let foe = entity(&app, FOE);
    let before = replay::state_hash(app.world());
    // A body with an id carries an emitter and a muffling collider: still the same hash.
    app.world_mut().entity_mut(foe).insert((SoundEmitter::new("hum"), SoundTransmission(0.4)));
    refresh(app.world_mut());
    assert_eq!(replay::state_hash(app.world()), before);
    assert!(voice_of(&app, foe).is_some(), "and it sounds");
}

// --- occlusion ---------------------------------------------------------------------------------

/// A wall at z = -3 between the hero and a vent at z = -6, a pane at x = 3 between the hero and a
/// vent at x = 6, and a vent at x = -6 in the open.
fn walled(app: &mut App) -> (Entity, Entity, Entity) {
    solid(app, "Wall", Vec3::new(0.0, 1.0, -3.0), Vec3::new(8.0, 4.0, 0.4));
    let pane = solid(app, "Glass", Vec3::new(3.0, 1.0, 0.0), Vec3::new(0.2, 4.0, 8.0));
    app.world_mut().entity_mut(pane).insert(SoundTransmission(0.6));
    let behind_wall = emit(app, "Behind", "hum", Vec3::new(0.0, 0.0, -6.0));
    let behind_glass = emit(app, "Through", "hum", Vec3::new(6.0, 0.0, 0.0));
    let open = emit(app, "Open", "hum", Vec3::new(-6.0, 0.0, 0.0));
    settle(app);
    step(app, 1);
    (behind_wall, behind_glass, open)
}

#[test]
fn behind_a_wall_a_sound_is_quieter_behind_glass_it_is_between_and_walls_in_a_row_multiply() {
    let (_assets, mut app) = ambient("walls", WALLS);
    let (behind_wall, behind_glass, open) = walled(&mut app);
    let gain = |app: &App, e| voice_of(app, e).expect("heard").gain;
    let clear = gain(&app, open);
    // 6 m with `near` 2 and `far` 20.
    near(clear, ops::powf(1.0 - 4.0 / 18.0, 2.0));
    near(gain(&app, behind_wall), clear * 0.3);
    near(gain(&app, behind_glass), clear * 0.6);
    assert!(gain(&app, behind_wall) < gain(&app, behind_glass) && gain(&app, behind_glass) < clear);
    let wall = voice_of(&app, behind_wall).unwrap().occlusion;
    assert_eq!((wall.by.len(), wall.by[0].name.as_str(), wall.by[0].passes, wall.passes), (1, "Wall", 0.3, 0.3));
    assert_eq!(voice_of(&app, behind_glass).unwrap().occlusion.describe(), "Glass ×0.60");
    assert!(voice_of(&app, open).unwrap().occlusion.by.is_empty());

    let said = ok(&mut app, "audio", json!({}));
    assert!(said.contains("Behind hum 6.0m heard #1 gain 0.18 (occluded: Wall ×0.30)"), "{said}");
    assert!(said.contains("Through hum 6.0m heard #3 gain 0.36 (occluded: Glass ×0.60)"), "{said}");
    assert!(said.contains("#1 hum sfx Behind 6.0m gain 0.18 prio 40 (loop) (occluded: Wall ×0.30)"), "{said}");
    assert!(said.contains("#2 hum sfx Open 6.0m gain 0.60 prio 40 (loop)\n"), "{said}");

    // A second wall behind the first: the shares multiply, the nearest to the ears first.
    solid(&mut app, "Second", Vec3::new(0.0, 1.0, -4.5), Vec3::new(8.0, 4.0, 0.4));
    settle(&mut app);
    step(&mut app, 1);
    let both = voice_of(&app, behind_wall).unwrap();
    near(both.gain, clear * 0.09);
    assert_eq!(both.occlusion.describe(), "Wall ×0.30, Second ×0.30");

    // The wall opens (a door): the sound is whole again.
    let wall_entity = {
        let world = app.world_mut();
        let mut found = world.query::<(Entity, &Name)>();
        found.iter(world).find(|(_, n)| n.as_str() == "Wall").map(|(e, _)| e).unwrap()
    };
    app.world_mut().despawn(wall_entity);
    settle(&mut app);
    step(&mut app, 1);
    near(voice_of(&app, behind_wall).unwrap().gain, clear * 0.3);
}

#[test]
fn only_world_colliders_in_the_way_count_not_the_sources_or_the_listeners_own_nor_bodies_nor_sensors() {
    let (_assets, mut app) = ambient("only_walls", WALLS);
    let hero = entity(&app, HERO);
    // The vent stands on its own collider (a machine), and the hero in one.
    let vent = app.world_mut().spawn((Name::new("Machine"), SoundEmitter::new("hum"), Transform::from_xyz(0.0, 0.0, -6.0), Collider::standing_box(Vec3::splat(0.5)))).id();
    app.world_mut().entity_mut(hero).insert(Collider::standing_box(Vec3::splat(0.5)));
    // A body (a character layer) and a sensor between them.
    app.world_mut().spawn((Name::new("Guard"), Collider::standing_box(Vec3::new(1.0, 1.0, 0.3)).with_layers(CollisionLayers::CHARACTER), Transform::from_xyz(0.0, 0.0, -3.0)));
    app.world_mut().spawn((Name::new("Zone"), Collider::standing_box(Vec3::new(2.0, 2.0, 0.3)).with_layers(CollisionLayers::VOLUME), Transform::from_xyz(0.0, 0.0, -4.0)));
    // And the floor: the ears are on its surface, the vent too, and the ray runs along it.
    solid(&mut app, "Floor", Vec3::new(0.0, -0.25, 0.0), Vec3::new(40.0, 0.5, 40.0));
    settle(&mut app);
    step(&mut app, 1);
    let v = voice_of(&app, vent).expect("heard");
    assert!(v.occlusion.by.is_empty(), "{:?}", v.occlusion);
    near(v.gain, ops::powf(1.0 - 4.0 / 18.0, 2.0));
}

#[test]
fn a_mixer_without_occlusion_hears_every_sound_as_before_and_a_one_shot_is_muffled_too() {
    let (_assets, mut plain) = ambient("no_occlusion", MIXER_PLAIN);
    solid(&mut plain, "Wall", Vec3::new(0.0, 1.0, -3.0), Vec3::new(8.0, 4.0, 0.4));
    let vent = emit(&mut plain, "Behind", "hum", Vec3::new(0.0, 0.0, -6.0));
    settle(&mut plain);
    step(&mut plain, 1);
    let v = voice_of(&plain, vent).unwrap();
    near(v.gain, ops::powf(1.0 - 4.0 / 18.0, 2.0));
    assert!(v.occlusion.by.is_empty());

    // With the mixer asking, the same wall muffles a one-shot played from behind it too.
    let (_assets, mut app) = ambient("one_shot_walls", WALLS);
    solid(&mut app, "Wall", Vec3::new(0.0, 1.0, -3.0), Vec3::new(8.0, 4.0, 0.4));
    settle(&mut app);
    let said = ok(&mut app, "sound_play", json!({"sound": "beep", "point": [0.0, 0.0, -5.0]}));
    assert_eq!(said, "playing beep #1: sounds/beep.wav at 5.0 m, gain 0.12, 0.50s (occluded: Wall ×0.30)", "{said}");
    // The same one-shot in the open, and at the ears, has nothing in its way.
    let open = ok(&mut app, "sound_play", json!({"sound": "beep", "point": [-5.0, 0.0, 0.0]}));
    assert_eq!(open, "playing beep #2: sounds/beep.wav at 5.0 m, gain 0.39, 0.50s", "{open}");
    // A sound with no place is not behind anything.
    play(app.world_mut(), "click", SoundAt::Everywhere).unwrap();
    assert_eq!(voices(&app).last().map(|v| (v.bus, v.gain, v.occlusion.by.len())), Some((Bus::Ui, 1.0, 0)));
}

#[test]
fn a_transmission_on_a_collider_is_read_as_a_share_between_nothing_and_everything() {
    let (_assets, mut app) = ambient("transmission", WALLS);
    let pane = solid(&mut app, "Pane", Vec3::new(0.0, 1.0, -3.0), Vec3::new(8.0, 4.0, 0.4));
    let vent = emit(&mut app, "Behind", "hum", Vec3::new(0.0, 0.0, -6.0));
    let clear = ops::powf(1.0 - 4.0 / 18.0, 2.0);
    for (set, share) in [(2.0, 1.0), (-1.0, 0.0), (0.0, 0.0), (1.0, 1.0)] {
        app.world_mut().entity_mut(pane).insert(SoundTransmission(set));
        settle(&mut app);
        step(&mut app, 1);
        let gain = voice_of(&app, vent).map_or(0.0, |v| v.gain);
        near(gain, clear * share);
    }
}

// --- the content check -------------------------------------------------------------------------

#[test]
fn the_content_check_refuses_a_looped_sound_played_once_and_a_bad_loop() {
    let assets = Folder::new("loop_check");
    let app = game(&assets);
    let kinds = app.world().resource::<ContentKinds>().clone();
    std::fs::write(assets.path().join("sounds/hum.wav"), tone(1.0, 100.0)).unwrap();
    assets.put(
        "content/audio/loops.sound.ron",
        r#"{
            "hum": (files: ["sounds/hum.wav"], spatial: (near: 1.0, far: 9.0), looped: true),
            "words": (files: ["sounds/hum.wav"], looped: true, subtitle: "la la", speaker: "Radio"),
            "limited": (files: ["sounds/hum.wav"], looped: true, max_instances: 2),
        }"#,
    );
    assets.put("content/audio/mixer.audio.ron", r#"{"mixer": (voices: 4, music: "theme", sounds: {"healed": "hum", "died": "boom"}, occlusion: Some((gain: 1.5)))}"#);
    assets.put("content/bite.action.ron", r#"{"bite": (net_id: 1, phases: [("windup", 0.5), ("active", 0.1)], telegraph: (at: 0.1, sound: "hum"))}"#);
    assets.put(
        "content/swing.action.ron",
        r#"{"swing": (net_id: 2, phases: [("windup", 0.2), ("active", 0.1)], effects: [(at: 0.2, effect: Sound("hum")), (at: 0.2, effect: Sound("beep"))])}"#,
    );
    let errors: Vec<String> = check_dir(assets.path(), &kinds).unwrap_err().iter().map(ToString::to_string).collect();
    let has = |needle: &str| assert!(errors.iter().any(|e| e.contains(needle)), "no `{needle}` in:\n{}", errors.join("\n"));
    has("looped: a sound that never ends has no line to show; leave out `subtitle` and `speaker`");
    has("looped: its voices belong to its emitters");
    has("sounds.healed: `hum` loops, and an event plays a sound once; only a `SoundEmitter` plays a looped sound");
    has("occlusion.gain: 0 to 1, not 1.5");
    has("action `bite` warns with sound `hum`, which loops; only a `SoundEmitter` plays a looped sound");
    has("action `swing` effects[0] plays sound `hum`, which loops; only a `SoundEmitter` plays a looped sound");
    assert!(!errors.iter().any(|e| e.contains("effects[1]")), "a one-shot is fine: {errors:#?}");
    assert!(!errors.iter().any(|e| e.contains("sounds.died")), "{errors:#?}");

    // A well-formed loop and a mixer with walls pass.
    assets.put("content/audio/loops.sound.ron", r#"{"hum": (files: ["sounds/hum.wav"], spatial: (near: 1.0, far: 9.0), looped: true)}"#);
    assets.put("content/audio/mixer.audio.ron", r#"{"mixer": (voices: 4, music: "theme", occlusion: Some(()))}"#);
    std::fs::remove_file(assets.path().join("content/bite.action.ron")).unwrap();
    std::fs::remove_file(assets.path().join("content/swing.action.ron")).unwrap();
    let verdict = check_dir(assets.path(), &kinds).unwrap();
    assert!(verdict.to_string().starts_with("ok: "), "{verdict}");
}

// --- a level file ------------------------------------------------------------------------------

#[test]
fn a_level_file_places_an_emitter_and_a_pane_that_lets_sound_through() {
    use shards::level::{spawn_level, LevelAsset};
    let (_assets, mut app) = ambient("level", WALLS);
    let file = json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1, 2, 3]}],
        "nodes": [
            {"name": "Vent.North", "translation": [0.0, 0.0, -6.0], "extras": {"SoundEmitter": {"sound": "hum"}}},
            {"name": "Pane", "translation": [0.0, 1.0, -3.0], "extras": {"collider": "box", "size": [8.0, 4.0, 0.2], "SoundTransmission": 0.6}},
            {"name": "Drip.Off", "translation": [2.0, 0.0, -2.0], "extras": {"SoundEmitter": {"sound": "drip", "playing": false}}},
            {"name": "Mislaid", "translation": [4.0, 0.0, -2.0], "extras": {"SoundEmitter": {"sound": "hum", "volume": 3.0}}},
        ],
    });
    let asset = LevelAsset::from_slice("rooms/vent.gltf", file.to_string().as_bytes()).unwrap();
    // A field the component lacks is the artist's mistake, named, and nothing spawns.
    let refusal = spawn_level(app.world_mut(), &asset, Transform::IDENTITY).unwrap_err();
    assert!(refusal.contains("Mislaid") && refusal.contains("SoundEmitter"), "{refusal}");
    let mut file = file;
    file["nodes"].as_array_mut().unwrap().pop();
    file["scenes"][0]["nodes"] = json!([0, 1, 2]);
    let asset = LevelAsset::from_slice("rooms/vent.gltf", file.to_string().as_bytes()).unwrap();
    let spawned = spawn_level(app.world_mut(), &asset, Transform::IDENTITY).unwrap();
    assert_eq!(spawned.len(), 3);

    let world = app.world();
    let (vent, pane, off) = (spawned[0], spawned[1], spawned[2]);
    assert_eq!(world.get::<SoundEmitter>(vent), Some(&SoundEmitter { sound: "hum".into(), playing: true }), "`playing` is on unless the node says otherwise");
    assert_eq!(world.get::<SoundEmitter>(off), Some(&SoundEmitter { sound: "drip".into(), playing: false }));
    assert_eq!(world.get::<SoundTransmission>(pane), Some(&SoundTransmission(0.6)));
    assert_eq!(world.get::<Name>(vent).map(Name::as_str), Some("Vent.North"));
    assert!(world.get::<shards::ai_core::AiId>(vent).is_none(), "a level's emitter is level content, not an id'd body");

    settle(&mut app);
    step(&mut app, 1);
    let v = voice_of(&app, vent).expect("the node's emitter plays");
    near(v.gain, ops::powf(1.0 - 4.0 / 18.0, 2.0) * 0.6);
    assert_eq!(v.occlusion.describe(), "Pane ×0.60");
    assert!(voice_of(&app, off).is_none(), "and the stopped one does not");
    let said = ok(&mut app, "audio", json!({}));
    assert!(said.contains("Drip.Off drip 2.8m stopped"), "{said}");
    assert!(said.contains("Vent.North hum 6.0m heard #1 gain 0.36 (occluded: Pane ×0.60)"), "{said}");
}

#[test]
fn the_content_check_finds_an_emitter_that_names_no_sound_or_one_that_does_not_loop() {
    let assets = Folder::new("emitter_check");
    let app = game(&assets);
    let kinds = app.world().resource::<ContentKinds>().clone();
    assets.put("content/audio/ambient.sound.ron", AMBIENT);
    std::fs::write(assets.path().join("sounds/hum.wav"), tone(1.0, 100.0)).unwrap();
    // A level with a good emitter, a typo, a sound that does not loop (under the component's full path
    // and as the text Blender may hold), and a prefab with the same mistakes.
    let level = json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1, 2, 3, 4]}],
        "nodes": [
            {"name": "Vent", "extras": {"SoundEmitter": {"sound": "hum"}}},
            {"name": "Blank", "extras": {"SoundEmitter": {"sound": ""}}},
            {"name": "Typo", "extras": {"SoundEmitter": {"sound": "hom"}}},
            {"name": "Short", "extras": {"shards_audio::emitter::SoundEmitter": {"sound": "beep"}}},
            {"name": "Text", "extras": {"SoundEmitter": "{\"sound\": \"drop\"}"}},
        ],
    });
    assets.put("rooms/vent.gltf", &level.to_string());
    assets.put("content/audio/vent.level.ron", r#"{"vent": (file: "rooms/vent.gltf")}"#);
    assets.put(
        "content/audio/fans.prefab.ron",
        r#"{
            "fan": (components: {"Name": "Fan", "SoundEmitter": (sound: "hmm")}),
            "lamp": (components: {"SoundEmitter": (sound: "beep", playing: false)}),
            "good": (components: {"SoundEmitter": (sound: "hum")}),
            "mute": (components: {"SoundEmitter": (sound: "")}),
        }"#,
    );
    let errors: Vec<String> = check_dir(assets.path(), &kinds).unwrap_err().iter().map(ToString::to_string).collect();
    let has = |needle: &str| assert!(errors.iter().any(|e| e.contains(needle)), "no `{needle}` in:\n{}", errors.join("\n"));
    has("node `Typo`: SoundEmitter.sound -> unknown sound `hom` (did you mean `hum`?)");
    has("node `Short`: SoundEmitter.sound: `beep` does not loop: an emitter plays a sound with `looped: true`");
    has("node `Text`: SoundEmitter.sound -> unknown sound `drop`");
    has("prefab `fan`: SoundEmitter.sound -> unknown sound `hmm` (did you mean `hum`?)");
    has("prefab `lamp`: SoundEmitter.sound: `beep` does not loop");
    has("node `Blank`: SoundEmitter.sound -> unknown sound ``");
    has("prefab `mute`: SoundEmitter.sound -> unknown sound ``");
    assert!(!errors.iter().any(|e| e.contains("Vent") || e.contains("prefab `good`")), "a looped sound is fine: {errors:#?}");
    assert_eq!(errors.len(), 7, "{errors:#?}");
}

#[test]
fn an_emitter_whose_sound_names_no_file_plays_nothing_and_does_not_panic() {
    // The content check refuses `files: []`, but a game keeps running with the content that loaded.
    let assets = Folder::new("hollow");
    assets.put("content/audio/hollow.sound.ron", r#"{"hollow": (files: [], looped: true, spatial: (near: 1.0, far: 9.0))}"#);
    let mut app = game(&assets);
    emit(&mut app, "Hollow", "hollow", Vec3::new(0.0, 0.0, -2.0));
    wait(&mut app, 0.5);
    assert!(playing(&app).is_empty(), "{:?}", playing(&app));
    let warned = journal(&app, JournalKind::Warn, "cannot play");
    assert_eq!(warned, ["audio: emitter Hollow (hollow) cannot play: `hollow` names no file"]);
    let said = ok(&mut app, "audio", json!({}));
    assert!(said.contains("Hollow hollow 2.0m cannot play: `hollow` names no file"), "{said}");
}
