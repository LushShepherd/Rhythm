//! A local development harness for the Rust core; not a background timer service.

mod machine;

use rhythm_application::{ApplicationError, ProcessClock, Rhythm, TimerRepository};
use rhythm_domain::{
    Clock, Command, MAX_PERIOD_MS, PeriodKind, RecoveryChoice, Settings, TimerState,
};
use rhythm_storage::{ReflectionInput, Store};
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

type LocalRhythm = Rhythm<Store, ProcessClock>;

const HELP: &str = "Commands:
  status                    Show the current state without a countdown
  focus [minutes]           Explicitly begin focus (usual duration if omitted)
  rest [minutes]            Explicitly begin the offered rest
  pause | resume | stop      Pause, resume, or end the current period early
  poll                      Check whether the current period has ended
  new-rhythm                Begin a fresh cycle while no period is running
  recover continue | end    Resolve an interrupted period after reopening
  note <text>               Save a standalone journal reflection
  journal                   Revisit the latest 100 reflections and their IDs
  edit <record-id> <text>    Save a new version of a reflection
  drafts                    List unfinished drafts and their IDs
  finalize <draft-id>        Save an unfinished draft as a reflection
  help                      Show these commands
  quit                      End an active period and close the harness

Notes are single-line in this harness; the journal storage supports multiline text.
No next period starts automatically. This harness has no operating-system alerts.
Use status or poll to observe a deadline; closing input retains the saved state.";

#[derive(Debug, PartialEq, Eq)]
enum ConsoleCommand {
    Status,
    Focus(Option<u64>),
    Rest(Option<u64>),
    Pause,
    Resume,
    Stop,
    Poll,
    NewRhythm,
    Recover(RecoveryChoice),
    Note(String),
    Journal,
    Edit { record_id: String, text: String },
    Drafts,
    Finalize(String),
    Help,
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
enum Options {
    Run(PathBuf),
    Help,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|argument| argument == "--json") {
        std::process::exit(machine::run(args));
    }
    if let Err(error) = run() {
        eprintln!("Rhythm could not continue: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let database = match parse_options(std::env::args().skip(1))? {
        Options::Run(path) => path,
        Options::Help => {
            println!(
                "Usage: rhythm-cli --json <command> (control the running desktop app)\n       rhythm-cli --json schema (machine command reference)\n       rhythm-cli [--database <path>] (isolated development harness)\n\n{HELP}"
            );
            return Ok(());
        }
    };
    let store = Store::open(&database)?;
    let mut rhythm = Rhythm::open(store, ProcessClock::new(), Settings::default())?;
    println!("Rhythm — local development harness");
    println!("Journal: {}", database.display());
    println!("Type help for commands. No background alerts are scheduled.");
    print_status(&rhythm);

    let stdin = io::stdin();
    let mut input = stdin.lock();
    loop {
        print!("rhythm> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            checkpoint_on_input_close(&mut rhythm)?;
            println!("\nInput closed. Saved state and drafts are retained.");
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        match parse_command(line) {
            Ok(command) => match execute(&mut rhythm, command) {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => eprintln!("Could not complete that action: {error}"),
            },
            Err(error) => eprintln!("{error} Type help for commands."),
        }
    }
    Ok(())
}

/// A graceful input close is a lifecycle boundary: preserve elapsed time that can still be
/// verified in this process, while leaving an unfinished period for explicit restart recovery.
fn checkpoint_on_input_close<R: TimerRepository, C: Clock>(
    rhythm: &mut Rhythm<R, C>,
) -> Result<(), ApplicationError<R::Error>> {
    rhythm.dispatch(Command::Poll)?;
    Ok(())
}

fn parse_options(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None => Ok(Options::Run(PathBuf::from("rhythm.sqlite3"))),
        Some("--help" | "-h") if args.next().is_none() => Ok(Options::Help),
        Some("--database") => {
            let path = args
                .next()
                .filter(|path| !path.is_empty())
                .ok_or_else(|| "--database requires a path".to_owned())?;
            if args.next().is_some() {
                return Err("Unexpected arguments after the database path".into());
            }
            Ok(Options::Run(PathBuf::from(path)))
        }
        _ => Err("Usage: rhythm-cli [--database <path>]".into()),
    }
}

/// Consume exactly one separator so note content retains additional whitespace.
fn split_word(input: &str) -> (&str, &str) {
    match input.find(char::is_whitespace) {
        Some(index) => {
            let separator_length = input[index..].chars().next().unwrap().len_utf8();
            (&input[..index], &input[index + separator_length..])
        }
        None => (input, ""),
    }
}

fn parse_command(line: &str) -> Result<ConsoleCommand, String> {
    let (word, rest) = split_word(line.trim_start());
    match word {
        "focus" => Ok(ConsoleCommand::Focus(parse_minutes(rest)?)),
        "rest" => Ok(ConsoleCommand::Rest(parse_minutes(rest)?)),
        "note" if !rest.trim().is_empty() => Ok(ConsoleCommand::Note(rest.to_owned())),
        "edit" => {
            let (record_id, text) = split_word(rest.trim_start());
            if record_id.is_empty() || text.trim().is_empty() {
                return Err("Use edit <record-id> <text>.".into());
            }
            Ok(ConsoleCommand::Edit {
                record_id: record_id.into(),
                text: text.into(),
            })
        }
        "finalize" => {
            let id = one_argument(rest, "finalize <draft-id>")?;
            Ok(ConsoleCommand::Finalize(id.to_owned()))
        }
        "recover" => match one_argument(rest, "recover continue | end")? {
            "continue" => Ok(ConsoleCommand::Recover(RecoveryChoice::Continue)),
            "end" => Ok(ConsoleCommand::Recover(RecoveryChoice::End)),
            _ => Err("Use recover continue or recover end.".into()),
        },
        _ if !rest.trim().is_empty() => Err("Unexpected text after the command.".into()),
        "status" => Ok(ConsoleCommand::Status),
        "pause" => Ok(ConsoleCommand::Pause),
        "resume" => Ok(ConsoleCommand::Resume),
        "stop" => Ok(ConsoleCommand::Stop),
        "poll" => Ok(ConsoleCommand::Poll),
        "new-rhythm" => Ok(ConsoleCommand::NewRhythm),
        "journal" => Ok(ConsoleCommand::Journal),
        "drafts" => Ok(ConsoleCommand::Drafts),
        "help" => Ok(ConsoleCommand::Help),
        "quit" | "exit" => Ok(ConsoleCommand::Quit),
        "note" => Err("Use note <text>, or leave writing for another time.".into()),
        _ => Err(format!("Unknown command: {word}.")),
    }
}

fn one_argument<'a>(input: &'a str, usage: &str) -> Result<&'a str, String> {
    let mut parts = input.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(value), None) => Ok(value),
        _ => Err(format!("Use {usage}.")),
    }
}

fn parse_minutes(input: &str) -> Result<Option<u64>, String> {
    if input.trim().is_empty() {
        return Ok(None);
    }
    let value = one_argument(input, "focus [minutes] or rest [minutes]")?;
    let millis = value
        .parse::<u64>()
        .ok()
        .and_then(|minutes| minutes.checked_mul(60_000))
        .filter(|millis| *millis > 0 && *millis <= MAX_PERIOD_MS)
        .ok_or_else(|| {
            format!(
                "Use a whole number of minutes from 1 to {}.",
                MAX_PERIOD_MS / 60_000
            )
        })?;
    Ok(Some(millis))
}

fn execute(rhythm: &mut LocalRhythm, command: ConsoleCommand) -> Result<bool, Box<dyn Error>> {
    let timer_command = match command {
        ConsoleCommand::Focus(duration_ms) => Some(Command::StartFocus {
            session_id: Uuid::new_v4().to_string(),
            duration_ms,
        }),
        ConsoleCommand::Rest(duration_ms) => Some(Command::StartRest {
            session_id: Uuid::new_v4().to_string(),
            duration_ms,
        }),
        ConsoleCommand::Pause => Some(Command::Pause),
        ConsoleCommand::Resume => Some(Command::Resume),
        ConsoleCommand::Stop => Some(Command::EndEarly),
        ConsoleCommand::Poll | ConsoleCommand::Status => Some(Command::Poll),
        ConsoleCommand::NewRhythm => Some(Command::NewRhythm),
        ConsoleCommand::Recover(choice) => Some(Command::Recover(choice)),
        ConsoleCommand::Help => {
            println!("{HELP}");
            None
        }
        ConsoleCommand::Note(text) => {
            let draft = rhythm
                .repository_mut()
                .create_draft(ReflectionInput::thought(text), now_ms()?)?;
            println!("Saved draft {} on this device.", draft.id);
            let revision =
                rhythm
                    .repository_mut()
                    .finalize_draft(&draft.id, draft.version, now_ms()?)?;
            println!("Saved reflection {}.", revision.record_id);
            None
        }
        ConsoleCommand::Edit { record_id, text } => {
            let draft = rhythm
                .repository_mut()
                .edit_reflection(&record_id, now_ms()?)?;
            let draft =
                rhythm
                    .repository_mut()
                    .save_draft(&draft.id, draft.version, &text, now_ms()?)?;
            println!(
                "Draft {} contains this edit if finalization cannot be completed.",
                draft.id
            );
            let revision =
                rhythm
                    .repository_mut()
                    .finalize_draft(&draft.id, draft.version, now_ms()?)?;
            println!("Saved a new version of {}.", revision.record_id);
            None
        }
        ConsoleCommand::Finalize(id) => {
            let draft = rhythm
                .repository()
                .get_draft(&id)?
                .ok_or("That draft was not found.")?;
            let revision = rhythm
                .repository_mut()
                .finalize_draft(&id, draft.version, now_ms()?)?;
            println!("Saved reflection {}.", revision.record_id);
            None
        }
        ConsoleCommand::Journal => {
            let reflections = rhythm.repository().list_reflections(100, 0)?;
            if reflections.is_empty() {
                println!("Your journal is empty. Writing is always optional.");
            }
            for reflection in reflections {
                println!("\n{}", reflection.id);
                for revision in reflection.heads {
                    println!("  version {}\n{}", revision.id, revision.content.body);
                }
            }
            None
        }
        ConsoleCommand::Drafts => {
            let drafts = rhythm.repository().list_drafts()?;
            if drafts.is_empty() {
                println!("No unfinished drafts.");
            }
            for draft in drafts {
                println!(
                    "\n{} (version {})\n{}",
                    draft.id, draft.version, draft.content.body
                );
            }
            None
        }
        ConsoleCommand::Quit => {
            let ending = match rhythm.state() {
                TimerState::Running(_) | TimerState::Paused(_) => Some(Command::EndEarly),
                TimerState::RecoveryRequired(_) => Some(Command::Recover(RecoveryChoice::End)),
                TimerState::Ready { .. } => None,
            };
            if let Some(ending) = ending {
                rhythm.dispatch(ending)?;
            }
            println!(
                "Saved journal entries remain on this device. Come back whenever you are ready."
            );
            return Ok(true);
        }
    };
    if let Some(command) = timer_command {
        if let Some(session) = rhythm.dispatch(command)? {
            println!(
                "{} period saved. The next period waits for you.",
                kind_name(session.kind)
            );
        }
        print_status(rhythm);
    }
    Ok(false)
}

fn print_status(rhythm: &LocalRhythm) {
    match rhythm.state() {
        TimerState::Ready { next } => println!(
            "Ready for {}. Begin only when you choose.",
            kind_name(*next)
        ),
        TimerState::Running(period) => println!(
            "{} is in progress. No next period will start automatically.",
            kind_name(period.kind)
        ),
        TimerState::Paused(period) => println!(
            "{} is paused. Resume when you are ready.",
            kind_name(period.kind)
        ),
        TimerState::RecoveryRequired(_) => println!(
            "An interrupted period is waiting for your decision. Use recover continue or recover end."
        ),
    }
}

fn kind_name(kind: PeriodKind) -> &'static str {
    match kind {
        PeriodKind::Focus => "Focus",
        PeriodKind::ShortRest => "Short rest",
        PeriodKind::LongRest => "Long rest",
    }
}

fn now_ms() -> Result<i64, Box<dyn Error>> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhythm_domain::ClockSample;
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Clone)]
    struct ManualClock {
        elapsed: Rc<Cell<u64>>,
        epoch: &'static str,
    }

    impl Clock for ManualClock {
        fn sample(&self) -> ClockSample {
            ClockSample {
                epoch: self.epoch.into(),
                monotonic_ms: self.elapsed.get(),
                utc_ms: 10_000 + self.elapsed.get() as i64,
            }
        }
    }

    #[test]
    fn graceful_input_close_preserves_unpolled_elapsed_for_restart_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("eof.sqlite3");
        let clock = ManualClock {
            elapsed: Rc::new(Cell::new(0)),
            epoch: "before-close",
        };
        let mut app = Rhythm::open(
            Store::open(&path).unwrap(),
            clock.clone(),
            Settings::default(),
        )
        .unwrap();
        app.dispatch(Command::StartFocus {
            session_id: "eof-focus".into(),
            duration_ms: Some(1_000),
        })
        .unwrap();
        clock.elapsed.set(350);
        checkpoint_on_input_close(&mut app).unwrap();
        drop(app);

        let reopened = Rhythm::open(
            Store::open(&path).unwrap(),
            ManualClock {
                elapsed: Rc::new(Cell::new(0)),
                epoch: "after-close",
            },
            Settings::default(),
        )
        .unwrap();
        match reopened.state() {
            TimerState::RecoveryRequired(active) => {
                assert_eq!(active.elapsed_ms, 350);
                assert_eq!(active.remaining_ms(), 650);
            }
            state => panic!("expected explicit recovery, got {state:?}"),
        }
        assert!(
            reopened
                .repository()
                .list_sessions(10, 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn graceful_input_close_records_late_completion_once_without_starting_rest() {
        let clock = ManualClock {
            elapsed: Rc::new(Cell::new(0)),
            epoch: "before-close",
        };
        let mut app = Rhythm::open(
            Store::open_in_memory().unwrap(),
            clock.clone(),
            Settings::default(),
        )
        .unwrap();
        app.dispatch(Command::StartFocus {
            session_id: "completed-at-close".into(),
            duration_ms: Some(1_000),
        })
        .unwrap();
        clock.elapsed.set(4_000);
        checkpoint_on_input_close(&mut app).unwrap();
        checkpoint_on_input_close(&mut app).unwrap();
        assert_eq!(
            app.state(),
            &TimerState::Ready {
                next: PeriodKind::ShortRest
            }
        );
        let sessions = app.repository().list_sessions(10, 0).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].elapsed_ms, 1_000);
        assert_eq!(sessions[0].ended_utc_ms, Some(11_000));
    }

    #[test]
    fn graceful_input_close_propagates_a_failed_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("eof-conflict.sqlite3");
        let clock = ManualClock {
            elapsed: Rc::new(Cell::new(0)),
            epoch: "same-clock",
        };
        let mut first = Rhythm::open(
            Store::open(&path).unwrap(),
            clock.clone(),
            Settings::default(),
        )
        .unwrap();
        first
            .dispatch(Command::StartFocus {
                session_id: "concurrent-focus".into(),
                duration_ms: Some(1_000),
            })
            .unwrap();
        let mut second = Rhythm::open(
            Store::open(&path).unwrap(),
            clock.clone(),
            Settings::default(),
        )
        .unwrap();
        clock.elapsed.set(350);
        second.dispatch(Command::Poll).unwrap();
        let before = first.checkpoint();
        assert!(matches!(
            checkpoint_on_input_close(&mut first),
            Err(ApplicationError::Repository(
                rhythm_storage::Error::TimerConflict
            ))
        ));
        assert_eq!(first.checkpoint(), before);
    }

    #[test]
    fn note_and_edit_preserve_unicode_and_content_whitespace() {
        assert_eq!(
            parse_command("note   留一点空间。  ").unwrap(),
            ConsoleCommand::Note("  留一点空间。  ".into())
        );
        assert_eq!(
            parse_command("edit abc  給未來的自己  ").unwrap(),
            ConsoleCommand::Edit {
                record_id: "abc".into(),
                text: " 給未來的自己  ".into()
            }
        );
    }

    #[test]
    fn duration_parser_rejects_invalid_and_overflowing_input() {
        for value in ["0", "-1", "1.5", "1441", "18446744073709551615", "5 extra"] {
            assert!(parse_minutes(value).is_err(), "accepted {value}");
        }
        assert_eq!(parse_minutes("1440").unwrap(), Some(MAX_PERIOD_MS));
        assert_eq!(parse_minutes("").unwrap(), None);
        assert_eq!(parse_minutes(" 25 ").unwrap(), Some(1_500_000));
    }

    #[test]
    fn strict_commands_reject_surplus_arguments_and_empty_notes() {
        assert!(parse_command("pause anything").is_err());
        assert!(parse_command("recover continue extra").is_err());
        assert!(parse_command("recover later").is_err());
        assert!(parse_command("finalize").is_err());
        assert!(parse_command("note   ").is_err());
        assert!(parse_command("edit abc").is_err());
    }

    #[test]
    fn database_options_preserve_paths_with_spaces_and_reject_ambiguity() {
        assert_eq!(
            parse_options(["--database".into(), "some folder/journal.db".into()]).unwrap(),
            Options::Run(PathBuf::from("some folder/journal.db"))
        );
        assert!(parse_options(["--database".into()]).is_err());
        assert!(parse_options(["--database".into(), "a.db".into(), "b.db".into()]).is_err());
        assert!(parse_options(["--unknown".into()]).is_err());
    }
}
