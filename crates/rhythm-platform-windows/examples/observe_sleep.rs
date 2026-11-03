//! Opt-in observer only. Never requests sleep, wake, or power-policy changes.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    observer::run()
}
#[cfg(not(windows))]
fn main() {
    eprintln!("The hardware sleep observer is available only on Windows.");
    std::process::exit(2);
}

#[cfg(windows)]
mod observer {
    use rhythm_application::Rhythm;
    use rhythm_domain::{Clock, Command, PeriodKind, SessionOutcome, Settings, TimerState};
    use rhythm_platform_windows::{PowerEvent, PowerSubscription, WindowsClock};
    use rhythm_storage::Store;
    use std::{
        error::Error,
        fs::{self, File},
        io::Write,
        path::PathBuf,
        time::Duration,
    };

    fn elapsed(state: &TimerState) -> u64 {
        match state {
            TimerState::Running(active)
            | TimerState::Paused(active)
            | TimerState::RecoveryRequired(active) => active.elapsed_ms,
            TimerState::Ready { .. } => 0,
        }
    }

    pub(super) fn run() -> Result<(), Box<dyn Error>> {
        let mut args = std::env::args().skip(1);
        let mut output = None;
        let mut opted_in = false;
        let mut timeout: u64 = 180;
        let mut late: u64 = 30;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--observe" => opted_in = true,
                "--output" => output = args.next().map(PathBuf::from),
                "--timeout-seconds" => timeout = args.next().ok_or("Missing timeout")?.parse()?,
                "--late-seconds" => late = args.next().ok_or("Missing late deadline")?.parse()?,
                _ => return Err(format!("Unknown argument: {argument}").into()),
            }
        }
        if !opted_in || output.is_none() {
            return Err("Usage: observe_sleep --observe --output <new-directory> [--timeout-seconds 180] [--late-seconds 30]".into());
        }
        if !(2..=900).contains(&timeout) || !(1..timeout).contains(&late) {
            return Err(
                "Timeout must be 2–900 seconds, with a positive shorter late deadline".into(),
            );
        }
        let output = output.unwrap();
        fs::create_dir(&output)?;
        let mut audit = File::create_new(output.join("audit.tsv"))?;
        writeln!(audit, "utc_ms\tinclusive_ms\tawake_ms\tgeneration\tevent")?;
        let clock = WindowsClock::new();
        let (subscription, events) = PowerSubscription::subscribe()?;
        let mut apps = Vec::new();
        for (name, duration, paused) in [
            ("running", (timeout + 60) * 1_000, false),
            ("paused", (timeout + 60) * 1_000, true),
            ("late", late * 1_000, false),
        ] {
            let mut app = Rhythm::open(
                Store::open(output.join(format!("{name}.sqlite3")))?,
                clock.clone(),
                Settings::default(),
            )?;
            app.dispatch(Command::StartFocus {
                session_id: format!("hardware-{name}"),
                duration_ms: Some(duration),
            })?;
            if paused {
                app.dispatch(Command::Pause)?;
            }
            apps.push((name, app));
        }
        let initial = clock.sample();
        let initial_inclusive = inclusive_time::milliseconds();
        let mut generation = clock.interruption_generation();
        fs::write(
            output.join("ready.txt"),
            format!(
                "process_id={}\nepoch={}\nlate_deadline_seconds={late}\n",
                std::process::id(),
                initial.epoch
            ),
        )?;
        println!(
            "{{\"kind\":\"ready\",\"process_id\":{},\"late_deadline_seconds\":{late}}}",
            std::process::id()
        );
        std::io::stdout().flush()?;
        let mut saw_suspend = false;
        let mut saw_resume = false;
        let mut interrupted_elapsed = None;
        let (final_sample, final_inclusive) = loop {
            let sample = clock.sample();
            let inclusive = inclusive_time::milliseconds();
            let current_generation = clock.interruption_generation();
            for event in events.try_iter() {
                match event {
                    PowerEvent::Suspending => saw_suspend = true,
                    PowerEvent::Resumed => saw_resume = true,
                }
                writeln!(
                    audit,
                    "{}\t{inclusive}\t{}\t{current_generation}\t{event:?}; worker receipt time",
                    sample.utc_ms, sample.monotonic_ms
                )?;
            }
            if current_generation != generation {
                interrupted_elapsed.get_or_insert_with(|| {
                    apps.iter()
                        .map(|(_, app)| elapsed(app.state()))
                        .collect::<Vec<_>>()
                });
                for (_, app) in &mut apps {
                    app.dispatch(Command::Interrupt)?;
                }
                generation = current_generation;
            }
            // The changed clock epoch also protects a poll if sleep occurs
            // between the generation check and dispatch.
            for (_, app) in &mut apps {
                app.dispatch(Command::Poll)?;
                if matches!(app.state(), TimerState::RecoveryRequired(_)) {
                    app.dispatch(Command::Interrupt)?;
                }
            }
            writeln!(
                audit,
                "{}\t{inclusive}\t{}\t{generation}\tpoll",
                sample.utc_ms, sample.monotonic_ms
            )?;
            audit.flush()?;
            if saw_resume || inclusive.saturating_sub(initial_inclusive) >= timeout * 1_000 {
                break (sample, inclusive);
            }
            std::thread::sleep(Duration::from_secs(1));
        };
        let mut interruption_checks = interrupted_elapsed.is_some();
        let mut persisted = true;
        for (index, (name, app)) in apps.iter_mut().enumerate() {
            for _ in 0..3 {
                app.dispatch(Command::Interrupt)?;
                app.dispatch(Command::Poll)?;
            }
            let sessions = app.repository().list_sessions(10, 0)?;
            interruption_checks &= sessions.len() == 1
                && sessions.first().is_some_and(|record| {
                    record.outcome == SessionOutcome::EndedEarly
                        && record.recovered
                        && record.ended_utc_ms.is_none()
                        && record.elapsed_ms
                            == interrupted_elapsed
                                .as_ref()
                                .map_or(u64::MAX, |values| values[index])
                        && record.elapsed_ms < record.planned_ms
                })
                && matches!(
                    app.state(),
                    TimerState::Ready {
                        next: PeriodKind::Focus
                    }
                )
                && app.checkpoint().completed_focus == 0;
            let independent = Store::open(output.join(format!("{name}.sqlite3")))?;
            persisted &= independent
                .load_timer()?
                .is_some_and(|saved| saved.checkpoint == app.checkpoint());
        }
        let inclusive_elapsed = final_inclusive.saturating_sub(initial_inclusive);
        let awake_elapsed = final_sample
            .monotonic_ms
            .saturating_sub(initial.monotonic_ms);
        let excluded_sleep_ms = inclusive_elapsed.saturating_sub(awake_elapsed);
        let hardware_proven = excluded_sleep_ms >= 1_000 && saw_suspend && saw_resume;
        let passed = hardware_proven && interruption_checks && persisted;
        let result = format!(
            "{{\"kind\":\"result\",\"passed\":{passed},\"real_sleep_evidence\":{hardware_proven},\"excluded_sleep_ms\":{excluded_sleep_ms},\"saw_native_suspend\":{saw_suspend},\"saw_native_resume\":{saw_resume},\"all_periods_interrupted_without_catchup\":{interruption_checks},\"all_checkpoints_persisted\":{persisted},\"inclusive_elapsed_ms\":{inclusive_elapsed},\"awake_elapsed_ms\":{awake_elapsed}}}"
        );
        fs::write(output.join("result.json"), &result)?;
        fs::write(
            output.join("result.txt"),
            format!(
                "{result}\n\nThis observer never suspends the machine. Missing physical sleep evidence is INCONCLUSIVE. No sleeping time may count as Focus; the next Focus requires an explicit action.\n"
            ),
        )?;
        println!("{result}");
        subscription.close()?;
        if passed {
            Ok(())
        } else {
            Err("Hardware evidence is incomplete or a recovery assertion failed; inspect result.json and audit.tsv".into())
        }
    }

    #[allow(unsafe_code)]
    mod inclusive_time {
        pub(super) fn milliseconds() -> u64 {
            // SAFETY: GetTickCount64 is a read-only parameterless elapsed counter.
            unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
        }
    }
}
