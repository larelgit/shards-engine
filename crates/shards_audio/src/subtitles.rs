//! Subtitles (U4): the line a sound has (a voice's words), shown while it plays.
//!
//! - **Content.** A [`SoundDef`] with `subtitle: "@warden-gate"` (and `speaker: "@warden"`) has a
//!   line. When the mixer starts it (heard: not dropped, not out of earshot), the line shows for the
//!   sound's length plus [`LINGER_S`], at least [`MIN_SHOWN_S`] and at least its `shown` seconds, on
//!   the voice's clock (the simulation's; real time on the `Ui` bus), so a pause holds it.
//! - **The game's own lines.** [`say`] shows a line that no voice says, for the seconds the game
//!   gives, on the clock it names: words the game puts together (a tablet's new task as it chimes), or
//!   a line that outlasts its sound. It reaches everything a sound's line reaches, and a new one the
//!   game says for the same sound takes the place of the one before.
//! - **Shown by the game's UI.** A widget bound to the source `subtitles` (`Hint(bind:
//!   "subtitles")`) shows the newest lines, oldest first, one a line: `Warden: The gate holds.`;
//!   `subtitles:3` shows three ([`SHOWN`] without a number). Messages are resolved when the line is
//!   read, so a language changed in the menu applies to the lines showing. With
//!   `accessibility.subtitles` off the source is empty.
//! - **The same line twice** (two guards bark it) shows once, while either voice has time left (each
//!   by its own length and clock).
//! - **A voice taken** by a stronger sound takes its line with it, unless another voice says it too,
//!   and stopping every voice takes them all: a line is what is heard. A line the game said goes by
//!   its time alone, and with every line when every voice stops (a new region, a loaded save).
//! - **Agents** read each line as it starts in the journal (`[local] subtitle: Warden: The gate
//!   holds.`) whatever the setting, and the lines showing in `audio`.
//! - **This machine's**, like the mixer: never in a snapshot or the replay hash.

use ai_core::journal::{self, JournalKind};
use bevy::prelude::*;
use shards_settings::AccessibilitySettings;
use shards_ui::UiValue;

use crate::def::{Bus, SoundDef};
use crate::mixer::Mixer;

/// Seconds a line stays after its sound ends, to finish reading it.
pub const LINGER_S: f64 = 0.75;
/// Seconds a line shows at least: a short bark is still read.
pub const MIN_SHOWN_S: f64 = 2.0;
/// Lines the source shows without a number (`subtitles`).
pub const SHOWN: usize = 2;
/// Lines kept at most: the oldest goes for a new one.
pub const KEPT: usize = 8;

/// A line showing.
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleLine {
    /// The sound it belongs to.
    pub sound: String,
    /// The voices that say it (two guards barking it are one line), each with its own time: the
    /// line shows while one of them has some left.
    pub voices: Vec<LineVoice>,
    /// As content writes them: text, or a message `@id`.
    pub speaker: Option<String>,
    pub text: String,
}

/// A voice saying a line, and when its part of the line is over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineVoice {
    /// The mixer's voice; `None` for the game, which said the line itself ([`say`]).
    pub voice: Option<u64>,
    /// Whose clock it goes by.
    pub bus: Bus,
    /// When it is over, on that clock.
    pub until: f64,
}

impl SubtitleLine {
    /// Seconds it shows on: the most a voice saying it has left (`sim` and `real`: the clocks now).
    pub fn left(&self, sim: f64, real: f64) -> f64 {
        self.voices.iter().map(|v| v.until - clock(v.bus, sim, real)).fold(f64::NEG_INFINITY, f64::max)
    }
}

/// `bus`'s clock: the simulation's, real time on the `Ui` bus.
fn clock(bus: Bus, sim: f64, real: f64) -> f64 {
    if bus == Bus::Ui { real } else { sim }
}

/// The lines showing, oldest first.
#[derive(Resource, Default, Debug)]
pub struct Subtitles {
    lines: Vec<SubtitleLine>,
}

impl Subtitles {
    /// The lines showing, oldest first.
    pub fn lines(&self) -> &[SubtitleLine] {
        &self.lines
    }

    /// `line` starts: it is the newest. The same line still showing (two guards bark it) takes the
    /// new voice and moves up with its voices and their times, so it is not left out of the newest few.
    fn push(&mut self, mut line: SubtitleLine) {
        if let Some(at) = self.lines.iter().position(|l| l.speaker == line.speaker && l.text == line.text) {
            let mut same = self.lines.remove(at);
            same.voices.append(&mut line.voices);
            line = same;
        } else if self.lines.len() == KEPT {
            self.lines.remove(0);
        }
        self.lines.push(line);
    }
}

/// Whether the player wants subtitles (`accessibility.subtitles`; on without the settings module).
pub fn enabled(world: &World) -> bool {
    world.get_resource::<AccessibilitySettings>().is_none_or(|a| a.subtitles)
}

/// `line` in the language played: `Warden: The gate holds.`, or the text alone.
pub fn line_text(world: &World, line: &SubtitleLine) -> String {
    let text = shards_locale::resolve(world, &line.text);
    match &line.speaker {
        Some(speaker) => format!("{}: {text}", shards_locale::resolve(world, speaker)),
        None => text,
    }
}

/// The newest `max` lines showing, oldest first, in the language played; none with the setting off.
pub fn shown(world: &World, max: usize) -> Vec<String> {
    let Some(subtitles) = world.get_resource::<Subtitles>().filter(|_| enabled(world)) else {
        return Vec::new();
    };
    let lines = subtitles.lines();
    lines[lines.len().saturating_sub(max)..].iter().map(|l| line_text(world, l)).collect()
}

/// The source `subtitles[:max]`: the lines showing, one a line (empty when none).
pub(crate) fn source(world: &World, arg: &str) -> Option<UiValue> {
    let max = arg.trim().parse::<usize>().ok().filter(|n| *n > 0).unwrap_or(SHOWN).min(KEPT);
    Some(UiValue::Text(shown(world, max).join("\n")))
}

/// The mixer started voice `voice` of `sound` (`def`), `length` seconds long: its line, if it has
/// one, shows from now.
pub(crate) fn start(world: &mut World, sound: &str, def: &SoundDef, voice: u64, length: f64) {
    let Some(text) = def.subtitle.clone() else { return };
    let until = world.resource::<Mixer>().now(def.bus) + shown_for(length, def.shown);
    let says = LineVoice { voice: Some(voice), bus: def.bus, until };
    let line = SubtitleLine { sound: sound.to_owned(), voices: vec![says], speaker: def.speaker.clone(), text };
    show(world, line);
}

/// Seconds the line of a voice `length` seconds long shows: the length and [`LINGER_S`], at least
/// [`MIN_SHOWN_S`], and at least `shown` (its sound's `shown`, when it names one).
pub fn shown_for(length: f64, shown: Option<f32>) -> f64 {
    (length + LINGER_S).max(MIN_SHOWN_S).max(shown.map_or(0.0, f64::from))
}

/// `line` shows from now, the newest, and the journal says it as it reads.
fn show(world: &mut World, line: SubtitleLine) -> String {
    let said = line_text(world, &line);
    world.get_resource_or_init::<Subtitles>().push(line);
    journal::emit(world, JournalKind::Local, format!("subtitle: {said}"));
    said
}

/// A line the game says itself, which no voice says ([`say`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Saying {
    /// What it goes by ([`SubtitleLine::sound`]): the sound it comes with, or any name the game knows it by.
    pub sound: String,
    /// Text, or a message `@id`: resolved when the line is read, so a language changed in the menu
    /// applies to it while it shows.
    pub text: String,
    /// Who says it, before the line (`Warden: …`): text or `@id`.
    pub speaker: Option<String>,
    /// Seconds it shows, more than 0.
    pub secs: f32,
    /// Whose clock it goes by: the simulation's, so a pause holds it; real time on `Ui` (a menu's line).
    pub bus: Bus,
}

/// The game says `line`: it shows from now for its `secs` on its bus's clock, the newest line, and
/// reaches everything a sound's line reaches (the source `subtitles`, [`shown`], the `audio` tool, the
/// journal's `subtitle:` line). No voice says it, so a voice taken or stopped leaves it: it goes when
/// its time is up, or with every line when every voice stops ([`stop_all`](crate::mixer::stop_all): a
/// new region, a loaded save). It takes the place of the line the game said for the same `sound`
/// before, if that one still shows (a new task over the last); the same words showing for a voice show
/// once, as two voices' do.
///
/// Returns the line as it reads now. An empty text, or `secs` not more than 0, is refused.
pub fn say(world: &mut World, line: Saying) -> Result<String, String> {
    if line.text.trim().is_empty() {
        return Err(format!("say: the line for `{}` is empty", line.sound));
    }
    if !(line.secs > 0.0 && line.secs.is_finite()) {
        return Err(format!("say: `secs` is how long the line shows, more than 0, not {}", line.secs));
    }
    let Some(now) = world.get_resource::<Mixer>().map(|mixer| mixer.now(line.bus)) else {
        return Err("say: there is no mixer (the audio module is left out)".into());
    };
    if let Some(mut subtitles) = world.get_resource_mut::<Subtitles>() {
        for said in subtitles.lines.iter_mut().filter(|said| said.sound == line.sound) {
            said.voices.retain(|v| v.voice.is_some());
        }
        subtitles.lines.retain(|said| !said.voices.is_empty());
    }
    let says = LineVoice { voice: None, bus: line.bus, until: now + f64::from(line.secs) };
    Ok(show(world, SubtitleLine { sound: line.sound, voices: vec![says], speaker: line.speaker, text: line.text }))
}

/// Voice `voice` was taken by another sound: its line goes, unless another voice says it too (and
/// then it shows for that voice's time, not the taken one's).
pub(crate) fn cut(world: &mut World, voice: u64) {
    if let Some(mut subtitles) = world.get_resource_mut::<Subtitles>() {
        for line in &mut subtitles.lines {
            line.voices.retain(|v| v.voice != Some(voice));
        }
        subtitles.lines.retain(|l| !l.voices.is_empty());
    }
}

/// Voices whose time is up stop saying their lines (each by its bus's clock), and a line no voice
/// says goes.
pub(crate) fn expire(subtitles: &mut Subtitles, sim: f64, real: f64) {
    for line in &mut subtitles.lines {
        line.voices.retain(|v| clock(v.bus, sim, real) < v.until);
    }
    subtitles.lines.retain(|l| !l.voices.is_empty());
}

/// Every line goes (every voice stopped), the game's too.
pub(crate) fn clear(world: &mut World) {
    if let Some(mut subtitles) = world.get_resource_mut::<Subtitles>() {
        subtitles.lines.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, bus: Bus, until: f64) -> SubtitleLine {
        SubtitleLine { sound: "s".into(), voices: vec![LineVoice { voice: Some(1), bus, until }], speaker: None, text: text.into() }
    }

    fn texts(world: &World) -> Vec<String> {
        world.resource::<Subtitles>().lines().iter().map(|l| l.text.clone()).collect()
    }

    #[test]
    fn a_line_two_voices_say_goes_by_each_voices_own_time_and_clock() {
        // A long voice and a short one say the same line; the long one is taken.
        let mut world = World::new();
        let said = |voice, bus, until| LineVoice { voice: Some(voice), bus, until };
        let both = SubtitleLine { voices: vec![said(1, Bus::Voice, 10.0), said(2, Bus::Voice, 3.0)], ..line("Halt!", Bus::Voice, 0.0) };
        world.insert_resource(Subtitles { lines: vec![both] });
        assert_eq!(world.resource::<Subtitles>().lines()[0].left(1.0, 0.0), 9.0, "the longest voice's time");
        cut(&mut world, 1);
        assert_eq!(world.resource::<Subtitles>().lines()[0].left(1.0, 0.0), 2.0, "the taken voice's time went with it");
        expire(&mut world.resource_mut::<Subtitles>(), 3.0, 0.0);
        assert!(texts(&world).is_empty(), "gone with the voice left, not at the taken one's 10 s");

        // A menu voice (real time) and a world voice (the simulation's) say it: a pause holds the
        // world's, the menu's runs on.
        let mixed = SubtitleLine { voices: vec![said(3, Bus::Voice, 2.0), said(4, Bus::Ui, 5.0)], ..line("Halt!", Bus::Voice, 0.0) };
        world.insert_resource(Subtitles { lines: vec![mixed] });
        expire(&mut world.resource_mut::<Subtitles>(), 1.0, 6.0);
        assert_eq!(texts(&world), ["Halt!"], "the paused world's voice still has time");
        assert_eq!(world.resource::<Subtitles>().lines()[0].voices.len(), 1, "the menu's voice is done");
        expire(&mut world.resource_mut::<Subtitles>(), 2.0, 6.0);
        assert!(texts(&world).is_empty());
    }

    #[test]
    fn a_line_said_again_is_the_newest_and_keeps_its_first_voice() {
        let said = |text: &str, voice, until| SubtitleLine { voices: vec![LineVoice { voice: Some(voice), bus: Bus::Voice, until }], ..line(text, Bus::Voice, 0.0) };
        let mut subtitles = Subtitles::default();
        for (i, text) in ["Halt!", "Who goes there?", "Stand fast."].into_iter().enumerate() {
            subtitles.push(said(text, i as u64 + 1, 5.0));
        }
        subtitles.push(said("Halt!", 9, 3.0));
        let order: Vec<_> = subtitles.lines().iter().map(|l| l.text.as_str()).collect();
        assert_eq!(order, ["Who goes there?", "Stand fast.", "Halt!"], "the newest two show `Halt!`");
        let voices: Vec<_> = subtitles.lines()[2].voices.iter().map(|v| (v.voice, v.until)).collect();
        assert_eq!(voices, [(Some(1), 5.0), (Some(9), 3.0)], "both voices, each with its own time");

        // Eight lines are kept: a ninth pushes out the oldest, a line said again pushes out none.
        for i in 0..5 {
            subtitles.push(said(&format!("line {i}"), 20 + i, 5.0));
        }
        assert_eq!(subtitles.lines().len(), KEPT);
        subtitles.push(said("Stand fast.", 30, 5.0));
        assert_eq!(subtitles.lines().len(), KEPT);
        assert_eq!(subtitles.lines()[0].text, "Who goes there?");
        subtitles.push(said("new", 31, 5.0));
        assert_eq!(subtitles.lines()[0].text, "Halt!", "the oldest went");
    }

    #[test]
    fn each_line_goes_by_its_own_clock() {
        let mut subtitles = Subtitles { lines: vec![line("sim", Bus::Voice, 2.0), line("menu", Bus::Ui, 2.0)] };
        expire(&mut subtitles, 1.9, 5.0);
        assert_eq!(subtitles.lines().iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["sim"]);
        expire(&mut subtitles, 2.0, 0.0);
        assert!(subtitles.lines().is_empty(), "a line goes at its `until`");
    }

    #[test]
    fn a_line_the_game_says_goes_by_its_own_time_whatever_the_voices_do() {
        let mut world = World::new();
        world.insert_resource(Mixer::default());
        world.insert_resource(Subtitles { lines: vec![line("Halt!", Bus::Voice, 1.0)] });
        let task = |text: &str| Saying { sound: "tablet".into(), text: text.into(), speaker: None, secs: 7.0, bus: Bus::Sfx };
        assert_eq!(say(&mut world, task("TASK: the airlock")), Ok("TASK: the airlock".into()));
        assert_eq!(texts(&world), ["Halt!", "TASK: the airlock"], "the newest line");
        assert_eq!(world.resource::<Subtitles>().lines()[1].voices, [LineVoice { voice: None, bus: Bus::Sfx, until: 7.0 }]);

        // Voice #1 is taken: its line goes, the game's stays.
        cut(&mut world, 1);
        assert_eq!(texts(&world), ["TASK: the airlock"]);
        expire(&mut world.resource_mut::<Subtitles>(), 6.9, 100.0);
        assert_eq!(texts(&world), ["TASK: the airlock"], "on the simulation's clock: real time does not run it out");

        // A new task for the same sound takes the old one's place; another sound's line stays, and the same
        // words a voice says show once, with both times.
        let door = Saying { sound: "door".into(), text: "[LOCKED]".into(), secs: 2.0, ..task("") };
        say(&mut world, door).unwrap();
        say(&mut world, task("TASK: the valve")).unwrap();
        assert_eq!(texts(&world), ["[LOCKED]", "TASK: the valve"]);
        world.resource_mut::<Subtitles>().push(SubtitleLine { voices: vec![LineVoice { voice: Some(5), bus: Bus::Voice, until: 9.0 }], ..line("[LOCKED]", Bus::Voice, 0.0) });
        assert_eq!(texts(&world), ["TASK: the valve", "[LOCKED]"]);
        say(&mut world, Saying { sound: "door".into(), text: "[OPEN]".into(), secs: 2.0, ..task("") }).unwrap();
        assert_eq!(texts(&world), ["TASK: the valve", "[LOCKED]", "[OPEN]"], "the game's part of `[LOCKED]` went, the voice's stays");
        assert_eq!(world.resource::<Subtitles>().lines()[1].voices.len(), 1);

        // A line on `Ui` goes by real time, which a pause does not hold.
        say(&mut world, Saying { sound: "menu".into(), text: "Saved.".into(), secs: 1.0, bus: Bus::Ui, ..task("") }).unwrap();
        expire(&mut world.resource_mut::<Subtitles>(), 0.0, 0.9);
        assert!(texts(&world).contains(&"Saved.".to_owned()));
        expire(&mut world.resource_mut::<Subtitles>(), 0.0, 1.0);
        assert!(!texts(&world).contains(&"Saved.".to_owned()), "its second of real time is up");

        // Every voice stopped (a new region, a loaded save): the game's lines go too.
        clear(&mut world);
        assert!(texts(&world).is_empty());
    }

    #[test]
    fn a_line_the_game_cannot_say_is_refused() {
        let mut world = World::new();
        let line = |text: &str, secs: f32| Saying { sound: "tablet".into(), text: text.into(), speaker: None, secs, bus: Bus::Sfx };
        assert_eq!(say(&mut world, line("Go.", 1.0)), Err("say: there is no mixer (the audio module is left out)".into()));
        world.insert_resource(Mixer::default());
        assert_eq!(say(&mut world, line(" ", 1.0)), Err("say: the line for `tablet` is empty".into()));
        for secs in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(say(&mut world, line("Go.", secs)).is_err_and(|e| e.contains("more than 0")), "{secs}");
        }
        assert!(world.get_resource::<Subtitles>().is_none_or(|s| s.lines().is_empty()));
    }

    #[test]
    fn a_line_shows_for_its_sound_and_a_moment_at_least_its_shown_seconds() {
        assert_eq!(shown_for(0.5, None), MIN_SHOWN_S, "a short bark");
        assert_eq!(shown_for(3.0, None), 3.75, "its length and a moment to finish reading");
        assert_eq!(shown_for(0.5, Some(7.0)), 7.0, "a long line on a short beep");
        assert_eq!(shown_for(9.0, Some(7.0)), 9.75, "a long sound keeps its own time");
    }

    #[test]
    fn a_line_without_a_speaker_is_its_text() {
        let world = World::new();
        assert_eq!(line_text(&world, &line("Hold the gate.", Bus::Voice, 1.0)), "Hold the gate.");
        let spoken = SubtitleLine { speaker: Some("Warden".into()), ..line("Hold the gate.", Bus::Voice, 1.0) };
        assert_eq!(line_text(&world, &spoken), "Warden: Hold the gate.");
    }
}
