use serde_json::Value;
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn cli(args: &[&str], input: Option<&str>) -> (i32, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rhythm-cli"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        output.status.code().unwrap(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}
#[test]
fn schema_is_machine_readable_without_starting_desktop() {
    let (code, value) = cli(&["--json", "schema"], None);
    assert_eq!(code, 0);
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["protocol_version"], 1);
    assert!(
        value["result"]["commands"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("quit"))
    );
    assert_eq!(
        value["result"]["shutdown"]["success_result"]["closing"],
        true
    );
    let examples = value["result"]["request_examples"].as_array().unwrap();
    assert_eq!(examples.len(), 22);
    assert!(
        examples
            .iter()
            .any(|example| example["op"] == "switch_to_focus")
    );
    assert!(
        examples
            .iter()
            .any(|example| example["op"] == "switch_to_break")
    );
    assert!(
        examples
            .iter()
            .any(|example| example["op"] == "extend_focus")
    );
    assert!(
        examples
            .iter()
            .any(|example| example["op"] == "import_period_notes")
    );
    for example in examples {
        let request =
            serde_json::from_value::<rhythm_desktop_service::DesktopRequest>(example.clone())
                .unwrap();
        if let rhythm_desktop_service::DesktopRequest::SaveRoutine { routine, .. } = request {
            routine.validate().unwrap();
            assert_eq!(routine.windows.len(), 2);
            assert_eq!(routine.windows[0].start, "08:30");
            assert_eq!(routine.windows[1].end, "18:00");
        }
    }
}

#[cfg(windows)]
#[test]
fn read_commands_request_only_the_selected_server_data() {
    let directory = tempfile::tempdir().unwrap();
    let _guard = rhythm_control::start_server(directory.path(), |r| {
        assert!(r.request.is_none());
        rhythm_control::ControlResponse::success(
            r.request_id,
            serde_json::json!({"operation":r.operation}),
        )
    })
    .unwrap();
    for command in [
        "status",
        "journal",
        "drafts",
        "settings",
        "routines",
        "invitations",
    ] {
        let (code, value) = cli(
            &[
                "--json",
                "--data-dir",
                directory.path().to_str().unwrap(),
                command,
            ],
            None,
        );
        assert_eq!(code, 0);
        assert_eq!(value["result"]["operation"], command);
    }
}

#[cfg(windows)]
#[test]
fn quit_uses_the_safe_shutdown_operation_and_rejects_force_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let _guard = rhythm_control::start_server(directory.path(), |request| {
        assert_eq!(request.operation, "quit");
        assert!(request.request.is_none());
        rhythm_control::ControlResponse::success(
            request.request_id,
            serde_json::json!({"closing":true}),
        )
    })
    .unwrap();
    let args = [
        "--json",
        "--data-dir",
        directory.path().to_str().unwrap(),
        "quit",
    ];
    let (code, reply) = cli(&args, None);
    assert_eq!(code, 0);
    assert_eq!(reply["result"]["closing"], true);
    for extra in ["force", "--force", "now"] {
        let mut with_extra = args.to_vec();
        with_extra.push(extra);
        let (code, reply) = cli(&with_extra, None);
        assert_eq!(code, 2);
        assert_eq!(reply["error"]["code"], "usage");
    }
}
#[test]
fn malformed_input_returns_json_and_nonzero_exit() {
    let (code, value) = cli(&["--json", "request", "--stdin"], Some("{bad"));
    assert_eq!(code, 2);
    assert_eq!(value["error"]["code"], "invalid_json");
    let (code, value) = cli(
        &["--json", "request", "--stdin"],
        Some(r#"{"op":"pause","surprise":true}"#),
    );
    assert_eq!(code, 2);
    assert_eq!(value["error"]["code"], "invalid_request");
    assert_eq!(cli(&["--json", "focus", "0"], None).0, 2);
    let (code, value) = cli(
        &["--json", "request", "--stdin"],
        Some(
            r#"{"op":"create_draft","data":{"content":{"kind":"thought","body":"keep this","session_id":null,"reveal_at_utc_ms":null,"misspelled_field":true}}}"#,
        ),
    );
    assert_eq!(code, 2);
    assert_eq!(value["error"]["code"], "invalid_request");
}
#[test]
fn unavailable_desktop_is_not_replaced_by_direct_database_access() {
    let directory = tempfile::tempdir().unwrap();
    let (code, value) = cli(
        &[
            "--json",
            "--data-dir",
            directory.path().to_str().unwrap(),
            "--request-id",
            "stable-id",
            "status",
        ],
        None,
    );
    assert_eq!(code, 3);
    assert_eq!(value["request_id"], "stable-id");
    assert!(!directory.path().join("rhythm.sqlite3").exists());
}
#[cfg(windows)]
#[test]
fn multiline_json_reaches_desktop_protocol_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let _guard = rhythm_control::start_server(directory.path(), |r| {
        rhythm_control::ControlResponse::success(r.request_id, r.request.unwrap())
    })
    .unwrap();
    let body = "  English\n简体\r\n繁體  ";
    let request = serde_json::json!({"op":"create_draft","data":{"content":{"kind":"thought","body":body,"session_id":null,"reveal_at_utc_ms":null}}});
    let (code, value) = cli(
        &[
            "--json",
            "--data-dir",
            directory.path().to_str().unwrap(),
            "request",
            "--stdin",
        ],
        Some(&request.to_string()),
    );
    assert_eq!(code, 0);
    assert_eq!(value["result"]["data"]["content"]["body"], body);
}
