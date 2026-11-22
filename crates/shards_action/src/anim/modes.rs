//! Motion modes as content: `"@mode:<mode>"` and `"@left:<mode>"` mount rules, a layer's `off_in`,
//! `drops`, and the places they decide.

use super::mounting::{at, carried, drink, errors_of, kit, PROPS, SET};
use super::*;

/// The kit with a layer that is off in the water, the mode rules, the draw after leaving, and a drop.
fn with_modes() -> String {
    SET.replace(r#"layers: {"upper": (root: "spine")},"#, r#"layers: {"upper": (root: "spine", off_in: ["water"])}, root_motion: "root","#)
        .replace(
            r#"mounts: {"#,
            r#"drops: {"water": (down: 0.125, seconds: 0.25)},
    mounts: {
        "@mode:water": (moves: [(prop: "sword", to: Some("back"), span: Secs(0.0, 3600.0))], yield_after: 1.2),
        "@left:water": (moves: [(prop: "sword", to: Some("back"), span: Secs(0.0, 0.176))], wait: 1.0),
        "@mode:mantle": (moves: [(prop: "sword", to: Some("back"), span: Secs(0.0, 3600.0))]),"#,
        )
}

fn set_of(text: &str) -> AnimSet {
    let sets: BTreeMap<String, AnimSet> = ron::Options::default()
        .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
        .from_str(text)
        .unwrap_or_else(|e| panic!("{e}"));
    sets["h"].clone()
}

#[test]
fn the_mode_keys_validate_and_round_trip() {
    let text = with_modes();
    assert_eq!(errors_of(PROPS, &text), Vec::<String>::new());
    let set = set_of(&text);
    let json = serde_json::to_value(&set).unwrap();
    assert_eq!(serde_json::from_value::<AnimSet>(json.clone()).unwrap(), set);
    assert_eq!(json["mounts"]["@mode:water"]["yield_after"], 1.2f32);
    assert_eq!(json["mounts"]["@left:water"]["wait"], 1.0);
    assert!(json["mounts"]["@mode:mantle"].get("yield_after").is_none(), "a zero is left out");
    assert!(json["mounts"]["drink"].get("wait").is_none());
    assert_eq!(json["layers"]["upper"], serde_json::json!({"root": "spine", "off_in": ["water"]}));
    assert_eq!(json["drops"]["water"], serde_json::json!({"down": 0.125, "seconds": 0.25}));
    // A set that uses none of it keeps its JSON: no `off_in`, no `drops`.
    let plain = serde_json::to_value(kit()).unwrap();
    assert_eq!(plain["layers"]["upper"], serde_json::json!({"root": "spine"}));
    assert!(plain.get("drops").is_none());
    assert_eq!(set.mode_rules(MotionMode::Water).unwrap().yield_after, 1.2);
    assert_eq!(set.left_rules(MotionMode::Water).unwrap().wait, 1.0);
    assert!(set.mode_rules(MotionMode::Air).is_none() && set.left_rules(MotionMode::Mantle).is_none());
    assert!(set.layer_off("upper", MotionMode::Water) && !set.layer_off("upper", MotionMode::Ground));
    assert!(!set.layer_off("lower", MotionMode::Water), "a layer the set lacks is never off");
}

#[test]
fn the_modes_have_content_names() {
    for (mode, name) in [
        (MotionMode::Ground, "ground"),
        (MotionMode::Air, "air"),
        (MotionMode::Water, "water"),
        (MotionMode::Glide, "glide"),
        (MotionMode::Mantle, "mantle"),
        (MotionMode::Ladder, "ladder"),
    ] {
        assert_eq!(mode_name(mode), name);
        assert_eq!(parse_mode(name), Some(mode));
        assert!(MODES.contains(&name));
    }
    assert_eq!(parse_mode("swim"), None);
    assert_eq!(ModeLeft { entity: Entity::PLACEHOLDER, mode: "water".into() }.name(), "left:water");
}

#[test]
fn a_mode_key_is_checked() {
    let text = with_modes();
    let bad = text
        .replace(r#""@mode:mantle": (moves"#, r#""@mode:watr": (moves"#)
        .replace(r#""@left:water": (moves"#, r#""@left:space": (moves"#);
    let errors = errors_of(PROPS, &bad);
    assert_eq!(
        errors,
        [
            "content/h.animset.ron: animset `h`: mounts.@left:space names an unknown motion mode `space`; modes: ground, air, water, glide, mantle, ladder",
            "content/h.animset.ron: animset `h`: mounts.@mode:watr names an unknown motion mode `watr` (did you mean `water`?); modes: ground, air, water, glide, mantle, ladder",
        ]
    );
    // The fields belong to their key; the numbers are not negative; a span has no phases.
    let bad = text
        .replace(r#"yield_after: 1.2"#, r#"yield_after: -1.0, wait: 2.0, on_cut: {"sword": 0.1}, previous: Some(Secs(0.0, 1.0))"#)
        .replace(r#"wait: 1.0"#, r#"wait: -1.0, yield_after: 0.5, ended_by: ["blocked"]"#)
        .replace(r#"span: Secs(0.0, 0.176)"#, r#"span: Phase("drink")"#);
    assert_eq!(
        errors_of(PROPS, &bad),
        [
            "content/h.animset.ron: animset `h`: mounts.@left:water.moves[0].span: under `@left:water` a span is `Secs(from, to)`, counted from the moment the wait ends (it has no phases)",
            "content/h.animset.ron: animset `h`: mounts.@left:water.ended_by belongs under `@moveset`: only a weapon switch's equip ends by an event",
            "content/h.animset.ron: animset `h`: mounts.@left:water.yield_after belongs under `@mode:<mode>`: it is how long the rules step aside after an action in the mode",
            "content/h.animset.ron: animset `h`: mounts.@left:water.wait must be >= 0 (is -1)",
            "content/h.animset.ron: animset `h`: mounts.@mode:water.on_cut belongs under an action's key: it is what a cut run holds",
            "content/h.animset.ron: animset `h`: mounts.@mode:water.previous belongs under an action's key or `@moveset`: a mode's rules show the props the body carries",
            "content/h.animset.ron: animset `h`: mounts.@mode:water.wait belongs under `@left:<mode>`: it is how long the moves wait after the body left the mode",
            "content/h.animset.ron: animset `h`: mounts.@mode:water.yield_after must be >= 0 (is -1)",
        ]
    );
    // A move of a prop nobody carries does nothing, under a mode as anywhere.
    let ghost = text.replace(r#"(prop: "sword", to: Some("back"), span: Secs(0.0, 0.176))"#, r#"(prop: "boat", to: None, span: Secs(0.0, 0.176))"#);
    assert_eq!(
        errors_of(PROPS, &ghost),
        ["content/h.animset.ron: animset `h`: mounts.@left:water.moves[0].prop `boat` is never carried (no `carry` entry has it); the move does nothing"]
    );
}

#[test]
fn drops_and_off_in_are_checked() {
    let text = with_modes();
    let bad = text
        .replace(r#"drops: {"water": (down: 0.125, seconds: 0.25)}"#, r#"drops: {"watr": (down: 0.125, seconds: 0.25), "air": (down: 0.0, seconds: -1.0)}"#)
        .replace(r#"off_in: ["water"]"#, r#"off_in: ["water", "sea"]"#);
    assert_eq!(
        errors_of(PROPS, &bad),
        [
            "content/h.animset.ron: animset `h`: layers.upper.off_in names an unknown motion mode `sea`; modes: ground, air, water, glide, mantle, ladder",
            "content/h.animset.ron: animset `h`: drops.air.down must be > 0 (is 0)",
            "content/h.animset.ron: animset `h`: drops.air.seconds must be > 0 (is -1)",
            "content/h.animset.ron: animset `h`: drops has an unknown motion mode `watr` (did you mean `water`?); modes: ground, air, water, glide, mantle, ladder",
        ]
    );
    let bare = text.replace(r#" root_motion: "root","#, "");
    assert_eq!(
        errors_of(PROPS, &bare),
        ["content/h.animset.ron: animset `h`: `drops` needs `root_motion`: the drop is written on that bone; name the root bone, e.g. `root_motion: \"root\"`"]
    );
    let misspelt = text.replace("down: 0.125", "dwon: 0.125");
    assert!(errors_of(PROPS, &misspelt).iter().any(|e| e.contains("dwon")));
}

#[test]
fn the_modes_rules_sit_between_the_action_and_the_equip() {
    let set = set_of(&with_modes());
    let own = carried(&[("sword", "hand_r")]);
    let water = set.mode_rules(MotionMode::Water).unwrap();
    let left = set.left_rules(MotionMode::Water).unwrap();
    let equip = set.equip_rules().unwrap();
    let mut run_rules = MountRules::default();
    run_rules.moves.push(MountMove { prop: "sword".into(), to: Some("hip".into()), span: Span::Secs(0.0, 1.0) });
    let ctx = drink();
    let view = MountView {
        carried: &own,
        previous: &[],
        run: Some((&run_rules, &ctx, 0.5)),
        equip: Some((equip, 0.15)),
        mode: Some((water, 4.0)),
        left: Some((left, 0.1)),
        cut: &[],
    };
    assert_eq!(mount_places(&view), [("sword".into(), at("hip"))], "the running action first");
    let no_run = MountView { run: None, ..view };
    assert_eq!(mount_places(&no_run), [("sword".into(), at("back"))], "then the mode's rules");
    // The mode's span is open for a long while; past it (or without the mode) the draw's rules apply.
    let late = MountView { mode: Some((water, 4000.0)), ..no_run };
    assert_eq!(mount_places(&late), [("sword".into(), at("back"))], "then the rules after leaving (0 - 0.176 s)");
    let drawn = MountView { left: Some((left, 0.2)), ..late };
    assert_eq!(mount_places(&drawn), [("sword".into(), at("back"))], "the equip's own move: 0.1 - 0.2 s");
    let after = MountView { equip: None, ..drawn };
    assert_eq!(mount_places(&after), [("sword".into(), at("hand_r"))], "then the carry socket");
    // Waiting, the left is read at time 0.
    let waiting = MountView { mode: None, equip: None, left: Some((left, 0.0)), ..view };
    assert_eq!(mount_places(&MountView { run: None, ..waiting }), [("sword".into(), at("back"))]);
}
