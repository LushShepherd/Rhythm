//! One-shot, machine-readable control of the running desktop worker.
use rhythm_control::{
    ControlError, ControlRequest, ControlResponse, MAX_REQUEST_BYTES, PROTOCOL_VERSION,
};
use rhythm_desktop_service::DesktopRequest;
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    path::PathBuf,
};

pub fn run(args: Vec<String>) -> i32 {
    let fallback_id = uuid::Uuid::new_v4().to_string();
    let parsed = parse(args, fallback_id.clone());
    let (response, exit) = match parsed {
        Err(error) => (
            ControlResponse::failure(fallback_id, error.code, error.message),
            2,
        ),
        Ok(options) if options.command == "schema" => {
            (ControlResponse::success(options.id, schema()), 0)
        }
        Ok(options) => match request(&options) {
            Err(error) => (
                ControlResponse::failure(options.id, error.code, error.message),
                2,
            ),
            Ok(request) => match rhythm_control::send(&options.directory, &request) {
                Ok(response) => {
                    let exit = if response.ok { 0 } else { 4 };
                    (response, exit)
                }
                Err(error) => {
                    let exit = match error.code.as_str() {
                        "outcome_unknown" => 5,
                        "invalid_request" | "request_too_large" => 2,
                        _ => 3,
                    };
                    (
                        ControlResponse::failure(options.id, error.code, error.message),
                        exit,
                    )
                }
            },
        },
    };
    println!(
        "{}",
        serde_json::to_string(&response).expect("control envelope serializes")
    );
    exit
}

struct Options {
    directory: PathBuf,
    id: String,
    command: String,
    arguments: Vec<String>,
}
fn usage(message: impl Into<String>) -> ControlError {
    ControlError::new("usage", message)
}
fn parse(args: Vec<String>, fallback_id: String) -> Result<Options, ControlError> {
    let mut args = args.into_iter().peekable();
    let mut json_mode = false;
    let mut directory = None;
    let mut id = fallback_id;
    while let Some(flag) = args.peek().map(String::as_str) {
        match flag {
            "--json" if !json_mode => {
                args.next();
                json_mode = true;
            }
            "--data-dir" if directory.is_none() => {
                args.next();
                directory = Some(PathBuf::from(
                    args.next()
                        .filter(|v| !v.is_empty())
                        .ok_or_else(|| usage("--data-dir requires a path"))?,
                ));
            }
            "--request-id" => {
                args.next();
                id = args
                    .next()
                    .filter(|v| !v.is_empty() && v.len() <= 128)
                    .ok_or_else(|| usage("--request-id requires 1 to 128 bytes"))?;
            }
            _ => break,
        }
    }
    if !json_mode {
        return Err(usage(
            "Put --json before the command. Run rhythm-cli --json schema for examples.",
        ));
    }
    let command = args
        .next()
        .ok_or_else(|| usage("A command is required. Run rhythm-cli --json schema."))?;
    let arguments: Vec<_> = args.collect();
    if command == "schema" && !arguments.is_empty() {
        return Err(usage("schema takes no arguments"));
    }
    let directory = directory
        .or_else(|| std::env::var_os("RHYTHM_DATA_DIR").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|path| PathBuf::from(path).join("app.rhythm.quietroom"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(Options {
        directory,
        id,
        command,
        arguments,
    })
}

fn request(options: &Options) -> Result<ControlRequest, ControlError> {
    let args = &options.arguments;
    let mut operation = "execute";
    let payload = match options.command.as_str() {
        "status" | "journal" | "drafts" | "routines" | "invitations" | "quit"
            if args.is_empty() =>
        {
            operation = &options.command;
            None
        }
        "settings" if args.is_empty() => {
            operation = "settings";
            None
        }
        "focus" | "rest" => {
            if args.len() > 1 {
                return Err(usage("focus/rest accept at most one whole-minute duration"));
            }
            let duration = args
                .first()
                .map(|v| {
                    v.parse::<u64>()
                        .ok()
                        .and_then(|minutes| minutes.checked_mul(60_000))
                        .filter(|ms| *ms > 0 && *ms <= rhythm_domain::MAX_PERIOD_MS)
                        .ok_or_else(|| usage("Use whole minutes from 1 to 1440"))
                })
                .transpose()?;
            Some(
                json!({"op":if options.command == "focus" {"start_focus"} else {"start_rest"},"data":{"duration_ms":duration}}),
            )
        }
        "pause" | "resume" | "stop" | "new-rhythm" if args.is_empty() => Some(
            json!({"op":match options.command.as_str(){"stop"=>"end_early","new-rhythm"=>"new_rhythm", other=>other}}),
        ),
        "recover" if args.len() == 1 && matches!(args[0].as_str(), "continue" | "end") => {
            Some(json!({"op":"recover","data":{"choice":args[0]}}))
        }
        "settings" => Some(json!({"op":"update_settings","data":{"settings":read_json(args)?}})),
        "request" => Some(read_json(args)?),
        _ => {
            return Err(usage(
                "Unknown command or extra arguments. Run rhythm-cli --json schema.",
            ));
        }
    };
    // The CLI and desktop share the actual Rust request type. New routine and journal
    // operations work immediately through request --file/--stdin without a second parser.
    if let Some(payload) = &payload {
        serde_json::from_value::<DesktopRequest>(payload.clone())
            .map_err(|error| ControlError::new("invalid_request", error.to_string()))?;
    }
    Ok(ControlRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: options.id.clone(),
        operation: operation.into(),
        request: payload,
    })
}

fn read_json(arguments: &[String]) -> Result<Value, ControlError> {
    let mut reader: Box<dyn Read> = match arguments {
        [flag] if flag == "--stdin" => Box::new(io::stdin()),
        [flag, path] if flag == "--file" => Box::new(
            std::fs::File::open(path)
                .map_err(|error| usage(format!("Cannot read request file: {error}")))?,
        ),
        _ => return Err(usage("Use --file <path> or --stdin for JSON input")),
    };
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| usage(format!("Cannot read JSON: {error}")))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(ControlError::new(
            "request_too_large",
            "JSON input exceeds 1 MiB",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| ControlError::new("invalid_json", error.to_string()))
}

fn schema() -> Value {
    json!({
        "name":"Rhythm desktop control", "protocol_version":1,
        "usage":"rhythm-cli --json [--data-dir PATH] [--request-id ID] COMMAND",
        "transport":"Current-user Windows named pipe. Start the updated desktop app first. Machine commands never open SQLite directly.",
        "commands":["schema","status","focus [minutes]","rest [minutes]","pause","resume","stop","quit","new-rhythm","recover continue|end","journal","drafts","settings [--file PATH|--stdin]","routines","invitations","request --file PATH|--stdin"],
        "exit_codes":{"0":"success","2":"usage or invalid input","3":"desktop unavailable or unsupported platform; not sent","4":"application rejected request","5":"outcome unknown; inspect state before retrying"},
        "input":"request accepts one DesktopRequest JSON object. Multiline text is a JSON string. No confirmation prompt is shown: only send explicitly intended changes.",
        "request_examples":[
            {"op":"switch_to_focus","data":{"duration_ms":null}},
            {"op":"switch_to_break","data":{"duration_ms":null}},
            {"op":"extend_focus","data":{"additional_ms":300000}},
            {"op":"start_focus","data":{"duration_ms":1500000}},
            {"op":"start_rest","data":{"duration_ms":null}},
            {"op":"pause"}, {"op":"resume"}, {"op":"end_early"}, {"op":"new_rhythm"},
            {"op":"recover","data":{"choice":"continue"}},
            {"op":"create_draft","data":{"content":{"kind":"thought","body":"Dear me,\n慢慢来。\n慢慢來。","session_id":null,"reveal_at_utc_ms":null}}},
            {"op":"save_draft","data":{"id":"DRAFT_ID","version":1,"body":"Updated words"}},
            {"op":"save_period_note","data":{"id":"PERIOD_NOTE_ID","version":1,"body":"Words for this period"}},
            {"op":"import_period_notes","data":{"notes":[{"id":"ARCHIVED_NOTE_ID","kind":"focus","body":"Words from another device","version":1,"created_at_utc_ms":1800000000000i64,"updated_at_utc_ms":1800000001000i64,"archived_at_utc_ms":1800000002000i64}]}},
            {"op":"finalize_draft","data":{"id":"DRAFT_ID","version":2}},
            {"op":"edit_reflection","data":{"id":"RECORD_ID"}},
            {"op":"discard_draft","data":{"id":"DRAFT_ID","version":1}},
            {"op":"update_settings","data":{"settings":{"focus_ms":1500000,"short_rest_ms":300000,"long_rest_ms":1800000,"focus_before_long_rest":4}}},
            {"op":"save_routine","data":{"id":null,"routine":{"name":"Weekday rhythm","weekdays":[1,2,3,4,5],"windows":[{"start":"08:30","end":"12:00"},{"start":"14:30","end":"18:00"}],"timezone":"system","focus_ms":1500000,"enabled":true}}},
            {"op":"delete_routine","data":{"id":"ROUTINE_ID"}},
            {"op":"dismiss_invitation","data":{"routine_id":"ROUTINE_ID","due_at_utc_ms":1800000000000i64}},
            {"op":"accept_invitation","data":{"routine_id":"ROUTINE_ID","due_at_utc_ms":1800000000000i64}}
        ],
        "routine_windows":{"input":"Use 1 to 12 non-overlapping windows on each selected weekday. Start is HH:MM; end is later on the same day, with 24:00 allowed. Legacy local_time remains accepted instead of windows for a 15-minute invitation.","read":"routines returns routine_windows for active, undismissed windows and selected_routine_window for this app instance. Read the current routine_id and due_at_utc_ms before accepting or dismissing.","cycles":"accept_invitation explicitly starts focus and selects the window's focus preset for subsequent focus commands without a duration. Rest uses the saved short/long rest cycle. Every period requires an explicit start. Selection ends at the window boundary; running periods finish normally. After restarting the app, accept an available window again to select it."},
        "shutdown":{"command":"quit","operation":"quit","request":null,"success_result":{"closing":true},"behavior":"Save pending journal writing, end any active period safely, then close the desktop app after replying. No force termination. stop only ends the current timer and leaves the app open.","refusal":"Rhythm remains open if the interface is busy, composing text, editing an unsaved routine, unavailable, or unable to save. Resolve the issue before trying a new request ID."},
        "retry_policy":"No automatic retries. --request-id permits same-content replay only while the same desktop instance remains open. Responses cache up to 256 entries/64 MiB; evicted IDs reject re-execution. Restart resets the replay ledger. After uncertainty, inspect status or drafts; do not blindly retry with a new ID.",
        "limits":{"request_bytes":MAX_REQUEST_BYTES,"response_bytes":rhythm_control::MAX_RESPONSE_BYTES,"mutating_request_ids_per_app_instance":8192},
        "legacy_harness":"Without --json, the original interactive --database harness remains available for isolated development databases only. Never point it at the running app database."
    })
}
