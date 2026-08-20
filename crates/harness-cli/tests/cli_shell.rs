use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

fn temp_root() -> PathBuf {
    let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("harness-cli-shell-{}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn toml_text(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn toml_path(path: &Path) -> String {
    toml_text(&path.to_string_lossy())
}

fn write_config(root: &Path) -> PathBuf {
    let repo = repo_root();
    let config = format!(
        r#"
schema_version = 1

[runtime]
data_dir = "state"
default_profile = "default"

[observability]
runtime_events_jsonl = "runtime-events.jsonl"

[credentials.cli-test-path]
source = "env"
variable = "PATH"

[[providers]]
id = "example-python"
program = "{}"
args = ["providers/example-python/provider.py"]
cwd = "{}"
request_timeout_ms = 5000
shutdown_timeout_ms = 2000
credentials = { HARNESS_CLI_TEST_PATH = "cli-test-path" }

[profiles.default]
policy = "allow-all"
max_automatic_tool_attempts = 2

[profiles.default.model]
provider = "example-python"
model = "agent-model"
system = "Use the echo tool, then answer with its result."
timeout_ms = 5000
max_output_tokens = 256

[[profiles.default.tools]]
name = "echo"
provider = "example-python"
version = "1"
description = "Echo a JSON object through the Python provider"
parallel_safe = true
side_effect = "read-only"
timeout_ms = 5000
input_schema = {{ type = "object", properties = {{ text = {{ type = "string" }} }}, required = ["text"], additionalProperties = false }}
"#,
        toml_text(&std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_owned())),
        toml_path(&repo)
    );
    let path = root.join("harness.toml");
    fs::write(&path, config).unwrap();
    path
}

fn harness(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harness"));
    command.arg("--config").arg(config);
    command
}

fn run_interactive(config: &Path, session_id: &str, input: &str) -> std::process::Output {
    let mut child = harness(config)
        .arg("run")
        .arg(session_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn config_session_run_and_inspect_form_a_durable_cli_shell() {
    let root = temp_root();
    let config = write_config(&root);

    let checked = harness(&config).args(["config", "check"]).output().unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(String::from_utf8_lossy(&checked.stdout).contains("config ok:"));

    let created = harness(&config)
        .args(["session", "create"])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let session_id = String::from_utf8(created.stdout).unwrap().trim().to_owned();
    assert!(session_id.starts_with("ses_"));

    let first = run_interactive(&config, &session_id, "hello from cli\n/quit\n");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        String::from_utf8_lossy(&first.stdout)
            .contains("assistant> final: {\"text\": \"hello from cli\"}")
    );

    let second = run_interactive(&config, &session_id, "second turn\n/quit\n");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second.stdout)
            .contains("assistant> final: {\"text\": \"second turn\"}")
    );

    let inspected = harness(&config)
        .arg("inspect")
        .arg(session_id.as_str())
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let events = String::from_utf8(inspected.stdout).unwrap();
    assert!(events.contains("\"type\":\"session/created\""));
    assert!(events.contains("hello from cli"));
    assert!(events.contains("second turn"));

    let runtime_events = fs::read_to_string(root.join("runtime-events.jsonl")).unwrap();
    assert!(runtime_events.contains("\"type\":\"runtime/started\""));
    assert!(runtime_events.contains("\"type\":\"provider/ready\""));
    assert!(runtime_events.contains("\"type\":\"agent/opened\""));
    assert!(runtime_events.contains("\"type\":\"runtime/stopped\""));
    if let Ok(path_secret) = std::env::var("PATH") {
        assert!(!path_secret.is_empty());
        assert!(!runtime_events.contains(&path_secret));
    }

    fs::remove_dir_all(root).unwrap();
}
