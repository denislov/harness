use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use harness_agent::{AgentEventSource, AgentState};
use harness_runtime::{
    AgentProfile, HarnessRuntime, HarnessRuntimeBuilder, ModelBinding, ProviderProcessSpec,
    ProviderQuarantineReason, ProviderSlotStatus, ProviderSupervisorConfig, RuntimeEventBus,
    RuntimeEventKind, RuntimeIdSource, RuntimeToolBinding,
};
use harness_session::{SessionEventPayload, SessionStore};
use harness_tools::{
    AllowAllToolPolicy, ToolArgumentValidationError, ToolArgumentValidator, ToolDefinition,
};
use harness_types::{
    AgentInstanceId, ContentBlock, EventId, EventSeq, JsonText, Message, MessageId, MessageSource,
    ProviderId, Role, SessionId, SideEffectClass, Timestamp,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

struct TestIdentitySource {
    next: AtomicU64,
    timestamp: Timestamp,
}

impl TestIdentitySource {
    fn new(start: u64, timestamp: &str) -> Self {
        Self {
            next: AtomicU64::new(start),
            timestamp: Timestamp::parse(timestamp).unwrap(),
        }
    }

    fn next_value(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

impl AgentEventSource for TestIdentitySource {
    fn next_event_id(&self) -> EventId {
        EventId::new(format!("evt_supervisor_{}", self.next_value())).unwrap()
    }

    fn now(&self) -> Timestamp {
        self.timestamp
    }
}

impl RuntimeIdSource for TestIdentitySource {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(format!("ses_supervisor_{}", self.next_value())).unwrap()
    }

    fn next_agent_instance_id(&self) -> AgentInstanceId {
        AgentInstanceId::new(format!("agt_supervisor_{}", self.next_value())).unwrap()
    }
}

struct ObjectValidator;

impl ToolArgumentValidator for ObjectValidator {
    fn validate(
        &self,
        _definition: &ToolDefinition,
        arguments_json: &JsonText,
    ) -> Result<(), ToolArgumentValidationError> {
        let value: serde_json::Value = serde_json::from_str(arguments_json.as_str())
            .map_err(|error| ToolArgumentValidationError::new(error.to_string()))?;
        if value.is_object() {
            Ok(())
        } else {
            Err(ToolArgumentValidationError::new(
                "arguments must be a JSON object",
            ))
        }
    }

    fn composition_identity(&self) -> String {
        "test/supervisor-object-validator/v1".to_owned()
    }
}

fn provider_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/supervised_provider.py")
}

fn python_program() -> String {
    std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_owned())
}

fn temp_root(label: &str) -> PathBuf {
    let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "harness-runtime-supervisor-{label}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    path
}

fn profile(provider_id: &ProviderId) -> AgentProfile {
    let tool = ToolDefinition {
        name: "echo".to_owned(),
        version: "1".to_owned(),
        description: "Echo one JSON object".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false
        }),
        output_schema: None,
        parallel_safe: true,
        side_effect: SideEffectClass::ReadOnly,
        default_timeout_ms: 5_000,
    };
    AgentProfile::new(
        ModelBinding::new(provider_id.clone(), "agent-model")
            .with_system("Use echo, then answer with the tool result.")
            .with_timeout_ms(5_000),
        Arc::new(AllowAllToolPolicy),
    )
    .with_tool(RuntimeToolBinding::new(
        tool,
        provider_id.clone(),
        Arc::new(ObjectValidator),
    ))
    .with_max_automatic_tool_attempts(3)
}

fn provider_spec(
    provider_id: &ProviderId,
    marker: &Path,
    drift: bool,
    fail_first_restart: bool,
) -> ProviderProcessSpec {
    let mut spec = ProviderProcessSpec::new(provider_id.clone(), python_program())
        .arg(provider_script().into_os_string())
        .env(
            "HARNESS_SUPERVISOR_MARKER",
            marker.as_os_str().to_os_string(),
        )
        .request_timeout(Duration::from_secs(5))
        .shutdown_timeout(Duration::from_secs(2));
    if drift {
        spec = spec.env("HARNESS_SUPERVISOR_DRIFT", "1");
    }
    if fail_first_restart {
        spec = spec.env("HARNESS_SUPERVISOR_FAIL_FIRST_RESTART", "1");
    }
    spec
}

async fn build_runtime(
    root: &Path,
    marker: &Path,
    drift: bool,
    fail_first_restart: bool,
    events: RuntimeEventBus,
    identity: Arc<TestIdentitySource>,
) -> HarnessRuntime {
    let provider_id = ProviderId::new("supervised-python").unwrap();
    HarnessRuntimeBuilder::durable_local(root.to_path_buf(), identity.clone(), identity)
        .unwrap()
        .provider_supervisor_config(
            ProviderSupervisorConfig::new(
                Duration::from_millis(5),
                Duration::from_millis(5),
                Duration::from_millis(25),
            )
            .unwrap(),
        )
        .runtime_event_bus(events)
        .provider(provider_spec(
            &provider_id,
            marker,
            drift,
            fail_first_restart,
        ))
        .profile("supervised-agent", profile(&provider_id))
        .build()
        .await
        .unwrap()
}

fn user_message(message_id: &str, text: &str) -> Message {
    Message {
        id: MessageId::new(message_id).unwrap(),
        role: Role::User,
        source: MessageSource::user(),
        content: vec![ContentBlock::text(text)],
    }
}

async fn wait_for_messages(
    handle: &harness_agent::AgentHandle,
    expected_messages: usize,
) -> AgentState {
    for _ in 0..5_000 {
        let state = handle.snapshot().await.unwrap();
        if state.active_operation.is_none()
            && state.projection.lifecycle.open_turn.is_none()
            && state.projection.model_messages.len() == expected_messages
        {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("supervised Agent did not converge within test budget");
}

async fn wait_for_status(
    runtime: &HarnessRuntime,
    provider_id: &ProviderId,
    predicate: impl Fn(ProviderSlotStatus) -> bool,
) -> ProviderSlotStatus {
    for _ in 0..5_000 {
        if let Some(status) = runtime.providers().status(provider_id)
            && predicate(status)
        {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("provider status did not converge within test budget");
}

#[tokio::test]
async fn compatible_restart_rebinds_existing_profile_to_generation_two() {
    let script = provider_script();
    assert!(script.is_file(), "missing provider at {}", script.display());
    let root = temp_root("compatible");
    let marker = root.join("provider-crashed.marker");
    let events = RuntimeEventBus::default();
    let mut receiver = events.subscribe();
    let runtime = build_runtime(
        &root,
        &marker,
        false,
        true,
        events,
        Arc::new(TestIdentitySource::new(100, "2026-08-20T15:45:00Z")),
    )
    .await;
    let provider_id = ProviderId::new("supervised-python").unwrap();
    let session_id = runtime.create_session().await.unwrap();
    let handle = runtime
        .open_agent(session_id.clone(), "supervised-agent")
        .await
        .unwrap();

    handle
        .followup(user_message("msg_supervisor", "hello"))
        .await
        .unwrap();
    let state = wait_for_messages(&handle, 4).await;
    assert_eq!(
        state.projection.model_messages.last().unwrap().content,
        vec![ContentBlock::text("final: {\"text\": \"hello\"}")]
    );

    let status = wait_for_status(&runtime, &provider_id, |status| {
        status == ProviderSlotStatus::Ready { generation: 2 }
    })
    .await;
    assert_eq!(status, ProviderSlotStatus::Ready { generation: 2 });
    assert_eq!(runtime.providers().generation(&provider_id), Some(2));

    let session_events = SessionStore::read(
        runtime.session_store().as_ref(),
        &session_id,
        EventSeq::FIRST,
        128,
    )
    .await
    .unwrap();
    assert_eq!(
        session_events
            .iter()
            .filter(|event| matches!(event.payload(), SessionEventPayload::ToolDispatched(_)))
            .count(),
        2
    );

    let mut saw_unhealthy = false;
    let mut saw_restarting = false;
    let mut saw_restart_failed = false;
    let mut saw_restarted = false;
    while let Ok(event) = receiver.try_recv() {
        match event.kind {
            RuntimeEventKind::ProviderUnhealthy {
                provider,
                generation: 1,
            } if provider == provider_id => saw_unhealthy = true,
            RuntimeEventKind::ProviderRestarting {
                provider,
                generation: 1,
                ..
            } if provider == provider_id => saw_restarting = true,
            RuntimeEventKind::ProviderRestartFailed {
                provider,
                generation: 1,
                ..
            } if provider == provider_id => saw_restart_failed = true,
            RuntimeEventKind::ProviderRestarted {
                provider,
                generation: 2,
                ..
            } if provider == provider_id => saw_restarted = true,
            _ => {}
        }
    }
    assert!(saw_unhealthy);
    assert!(saw_restarting);
    assert!(saw_restart_failed);
    assert!(saw_restarted);

    runtime.shutdown().await.unwrap();
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn manifest_drift_quarantines_restart_without_advancing_generation() {
    let script = provider_script();
    assert!(script.is_file(), "missing provider at {}", script.display());
    let root = temp_root("drift");
    let marker = root.join("provider-crashed.marker");
    let events = RuntimeEventBus::default();
    let mut receiver = events.subscribe();
    let runtime = build_runtime(
        &root,
        &marker,
        true,
        false,
        events,
        Arc::new(TestIdentitySource::new(10_000, "2026-08-20T15:46:00Z")),
    )
    .await;
    let provider_id = ProviderId::new("supervised-python").unwrap();
    let session_id = runtime.create_session().await.unwrap();
    let handle = runtime
        .open_agent(session_id, "supervised-agent")
        .await
        .unwrap();

    handle
        .followup(user_message("msg_supervisor_drift", "trigger drift"))
        .await
        .unwrap();

    let status = wait_for_status(&runtime, &provider_id, |status| {
        matches!(status, ProviderSlotStatus::Quarantined { generation: 1 })
    })
    .await;
    assert_eq!(status, ProviderSlotStatus::Quarantined { generation: 1 });
    assert_eq!(runtime.providers().generation(&provider_id), Some(1));

    let mut saw_quarantine = false;
    while let Ok(event) = receiver.try_recv() {
        if matches!(
            event.kind,
            RuntimeEventKind::ProviderQuarantined {
                provider,
                generation: 1,
                reason: ProviderQuarantineReason::ManifestDrift,
            } if provider == provider_id
        ) {
            saw_quarantine = true;
        }
    }
    assert!(saw_quarantine);

    runtime.shutdown().await.unwrap();
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}
