//! Subtitles (U4): a played voice's line through the source `subtitles`, for its length and a
//! moment after; the same line once; a taken voice's line gone; a voice cut short (`stop`), faded out
//! and its line gone at once; the setting; the language; the check.

use serde_json::json;
use shards::ai_core::JournalKind;
use shards::audio::{play, stop, Played, SoundAt};
use shards::content::{check_dir, ContentKinds};
use shards::prelude::*;
use shards::ui::{read_source, UiValue};

use crate::*;

/// A warden's line (`calm.wav`, 2 s) in two languages, a guard's bark (`beep.wav`, 0.5 s, weak) as
/// written, and a line that is not a voice's.
const LINES: &str = r#"{
    "gate": (files: ["music/calm.wav"], bus: Voice, subtitle: "@warden-gate", speaker: "@warden"),
    "bark": (files: ["sounds/beep.wav"], bus: Voice, priority: 50, subtitle: "Halt!"),
}"#;

/// The test's asset folder with [`LINES`] and English and Russian text.
fn folder(name: &str) -> Folder {
    let assets = Folder::new(name);
    assets.put("content/audio/lines.sound.ron", LINES);
    assets.put("content/languages.locale.ron", r#"{ "en": (name: "English"), "ru": (name: "Русский", fallback: "en") }"#);
    assets.put("locale/en/lines.ftl", "warden = Warden\nwarden-gate = The gate holds.\n");
    assets.put("locale/ru/lines.ftl", "warden = Страж\nwarden-gate = Ворота выстоят.\n");
    assets
}

/// The engine on `assets`, speaking English.
fn english(assets: &Folder) -> App {
    let mut app = game(assets);
    ok(&mut app, "settings_set", json!({"group": "locale", "key": "language", "value": "en"}));
    app
}

/// What a widget bound to `bind` shows.
fn shows(app: &App, bind: &str) -> String {
    match read_source(app.world(), bind) {
        Ok(Some(UiValue::Text(text))) => text,
        other => panic!("`{bind}` gives {other:?}"),
    }
}

#[test]
fn a_played_voice_shows_its_line_for_its_length_and_a_moment_after() {
    let assets = folder("subtitles_time");
    let mut app = english(&assets);
    assert_eq!(shows(&app, "subtitles"), "", "nothing said yet");
    play(app.world_mut(), "gate", SoundAt::Everywhere).unwrap();
    assert_eq!(shows(&app, "subtitles"), "Warden: The gate holds.");
    assert_eq!(journal(&app, JournalKind::Local, "subtitle:"), ["subtitle: Warden: The gate holds."]);

    // The bark is short: it shows 2 s at least. The warden's 2 s line shows 0.75 s more.
    play(app.world_mut(), "bark", SoundAt::Everywhere).unwrap();
    play(app.world_mut(), "bark", SoundAt::Everywhere).unwrap();
    assert_eq!(shows(&app, "subtitles"), "Warden: The gate holds.\nHalt!", "oldest first, the same line once");
    assert_eq!(shows(&app, "subtitles:1"), "Halt!", "the newest one");
    assert_eq!(shows(&app, "subtitles:x"), shows(&app, "subtitles"), "a bad number shows the default two");
    let said = ok(&mut app, "audio", json!({}));
    assert!(said.contains("subtitles (shown): \"Warden: The gate holds.\" 2.") && said.contains("s more · \"Halt!\" 2.0s more"), "{said}");

    wait(&mut app, 1.9);
    assert_eq!(shows(&app, "subtitles"), "Warden: The gate holds.\nHalt!", "time goes by the steps: a pause holds them");
    wait(&mut app, 0.2);
    assert_eq!(shows(&app, "subtitles"), "Warden: The gate holds.", "the bark's 2 s are over");
    wait(&mut app, 0.75);
    assert_eq!(shows(&app, "subtitles"), "");
    assert_eq!(journal(&app, JournalKind::Local, "subtitle:").len(), 3, "each start is journaled");
}

#[test]
fn a_voice_taken_by_a_stronger_sound_takes_its_line_once_no_voice_says_it() {
    let assets = folder("subtitles_taken");
    let mut app = english(&assets);
    play(app.world_mut(), "gate", SoundAt::Everywhere).unwrap();
    play(app.world_mut(), "bark", SoundAt::Everywhere).unwrap();
    play(app.world_mut(), "bark", SoundAt::Everywhere).unwrap();
    play(app.world_mut(), "click", SoundAt::Everywhere).unwrap();
    assert_eq!(shows(&app, "subtitles"), "Warden: The gate holds.\nHalt!");
    // Four voices; `boom` (200) takes the weakest, the oldest of priority 50: the first bark. The
    // second still says it.
    play(app.world_mut(), "boom", SoundAt::Everywhere).unwrap();
    assert_eq!(playing(&app), ["gate #1", "bark #3", "click #4", "boom #5"]);
    assert_eq!(shows(&app, "subtitles"), "Warden: The gate holds.\nHalt!");
    play(app.world_mut(), "boom", SoundAt::Everywhere).unwrap();
    assert_eq!(playing(&app), ["gate #1", "click #4", "boom #5", "boom #6"]);
    assert_eq!(shows(&app, "subtitles"), "Warden: The gate holds.", "no voice says it now");
}

#[test]
fn a_voice_cut_short_fades_out_and_its_line_goes_at_once() {
    let assets = folder("subtitles_stopped");
    let mut app = english(&assets);
    let Ok(Played::Started { id: gate, .. }) = play(app.world_mut(), "gate", SoundAt::Everywhere) else { panic!("the gate's line is not heard") };
    wait(&mut app, 0.5);
    play(app.world_mut(), "bark", SoundAt::Everywhere).unwrap();
    let full = voices(&app)[0].gain;
    assert!(full > 0.0);

    // Half a second into its 2 s, the warden is cut short: his line goes, the bark's stays, and his voice fades.
    stop(app.world_mut(), gate, 0.2).unwrap();
    assert_eq!(shows(&app, "subtitles"), "Halt!");
    assert!(ok(&mut app, "audio", json!({})).contains(&format!("#{gate} gate voice")), "the voice is still there, fading");
    assert!(ok(&mut app, "audio", json!({})).contains("(stopping)"));
    wait(&mut app, 0.1);
    let fading = voices(&app).into_iter().find(|v| v.id == gate).expect("still fading").gain;
    assert!(fading > 0.2 * full && fading < 0.8 * full, "halfway through the fade: {fading} of {full}");
    // Stopped again, sooner: the fade goes on from where it is, never louder.
    stop(app.world_mut(), gate, 0.05).unwrap();
    wait(&mut app, 0.02);
    let sooner = voices(&app).into_iter().find(|v| v.id == gate).expect("still fading").gain;
    assert!(sooner < fading, "the shorter fade goes on from {fading}, at {sooner}");
    wait(&mut app, 0.1);
    assert_eq!(playing(&app), ["bark #2"], "silent and let go, long before its 2 s");

    // A voice that is gone is no voice to stop, and 0 stops one at once.
    assert_eq!(stop(app.world_mut(), gate, 0.2), Err(format!("no voice #{gate} plays: it played out, a stronger sound took it, or it was stopped")));
    stop(app.world_mut(), 2, 0.0).unwrap();
    assert!(playing(&app).is_empty());
    assert_eq!(shows(&app, "subtitles"), "");
}

#[test]
fn the_setting_off_shows_none_and_a_language_switched_to_says_the_lines_showing() {
    let assets = folder("subtitles_settings");
    let mut app = english(&assets);
    play(app.world_mut(), "gate", SoundAt::Everywhere).unwrap();
    ok(&mut app, "settings_set", json!({"group": "accessibility", "key": "subtitles", "value": false}));
    assert_eq!(shows(&app, "subtitles"), "", "off");
    assert!(ok(&mut app, "audio", json!({})).contains("subtitles (not shown: accessibility.subtitles is off)"));
    play(app.world_mut(), "bark", SoundAt::Everywhere).unwrap();
    assert_eq!(journal(&app, JournalKind::Local, "subtitle: Halt!").len(), 1, "agents read them all the same");

    ok(&mut app, "settings_set", json!({"group": "accessibility", "key": "subtitles", "value": true}));
    ok(&mut app, "settings_set", json!({"group": "locale", "key": "language", "value": "ru"}));
    assert_eq!(shows(&app, "subtitles"), "Страж: Ворота выстоят.\nHalt!");
}

#[test]
fn the_content_check_names_a_line_that_cannot_be_shown() {
    let assets = folder("subtitles_check");
    let app = game(&assets);
    let kinds = app.world().resource::<ContentKinds>().clone();
    assert!(check_dir(assets.path(), &kinds).is_ok(), "{:?}", check_dir(assets.path(), &kinds).err());
    assets.put(
        "content/audio/broken.sound.ron",
        r#"{
            "mute": (files: ["sounds/beep.wav"], subtitle: " "),
            "who": (files: ["sounds/beep.wav"], speaker: "Warden"),
            "lost": (files: ["sounds/beep.wav"], subtitle: "@warden-gat", speaker: "@wardn"),
        }"#,
    );
    let errors: Vec<String> = check_dir(assets.path(), &kinds).unwrap_err().iter().map(ToString::to_string).collect();
    let has = |needle: &str| assert!(errors.iter().any(|e| e.contains(needle)), "no `{needle}` in:\n{}", errors.join("\n"));
    has("subtitle: empty; leave it out for a sound with no line");
    has("speaker without a subtitle: there is no line to put the name before");
    has("`subtitle` names message `warden-gat`, which en and ru have not, nor their fallbacks (did you mean `@warden-gate`?)");
    has("`speaker` names message `wardn`, which en and ru have not, nor their fallbacks (did you mean `@warden`?)");
    assert_eq!(errors.len(), 4, "{errors:#?}");
}
