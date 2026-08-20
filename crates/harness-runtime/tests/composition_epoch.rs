use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use harness_agent::AgentEventSource;
use harness_runtime::{
    AgentProfile, ExecutionCompositionSnapshot, HarnessRuntime, HarnessRuntimeBuilder,
    HarnessRuntimeError, ModelBinding, ProviderProcessSpec, RuntimeIdSource, RuntimeToolBinding,
};
use harness_session::{
    AssistantMessage, ModelRequested, NewSessionEvent, SessionEventPayload, SessionStore,
    StepStarted, ToolCallRecorded, TurnStarted, UserMessage,
};
use harness_tools::{
    AllowAllToolPolicy, ToolArgumentValidationError, ToolArgumentValidator, ToolDefinition,
};
use harness_types::{
    AgentInstanceId, ContentBlock, EventId, EventSeq, JsonText, Message, MessageId, MessageSource,
    ProviderId, RequestId, Role, SessionId, SideEffectClass, StepNo, Timestamp, ToolCallId, TurnNo,
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
        EventId::new(format!("evt_composition_{}", self.next_value())).unwrap()
    }

    fn now(&self) -> Timestamp {
        self.timestamp
    }
}

impl RuntimeIdSource for TestIdentitySource {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(format!("ses_composition_{}", self.next_value())).unwrap()
    }

    fn next_agent_instance_id(&self) -> AgentInstanceId {
        AgentInstanceId::new(format!("agt_composition_{}", self.next_value())).unwrap()
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
        "test/object-validator/v1".to_owned()
    }
}

fn provider_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers/example-python/provider.py")
}

fn python_program() -> String {
    std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_owned())
}

fn temp_root(label: &str) -> PathBuf {
    let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "harness-runtime-composition-{label}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    path
}

fn profile(provider_id: &ProviderId, system: &str) -> AgentProfile {
    let tool = ToolDefinition {
        name: "echo".to_owned(),
        version: "1".to_owned(),
        description: "Echo a JSON object through the Python provider".to_owned(),
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
            .with_system(system)
            .with_timeout_ms(5_000),
        Arc::new(AllowAllToolPolicy),
    )
    .with_tool(RuntimeToolBinding::new(
        tool,
        provider_id.clone(),
        Arc::new(ObjectValidator),
    ))
}

fn provider_spec(provider_id: &ProviderId) -> ProviderProcessSpec {
    ProviderProcessSpec::new(provider_id.clone(), python_program())
        .arg(provider_script().into_os_string())
        .request_timeout(Duration::from_secs(5))
        .shutdown_timeout(Duration::from_secs(2))
}

async fn build_runtime(
    root: &Path,
    identity: Arc<TestIdentitySource>,
    system: &str,
) -> HarnessRuntime {
    let provider_id = ProviderId::new("example-python").unwrap();
    HarnessRuntimeBuilder::durable_local(root.to_path_buf(), identity.clone(), identity)
        .unwrap()
        .provider(provider_spec(&provider_id))
        .profile("python-agent", profile(&provider_id, system))
        .build()
        .await
        .unwrap()
}

async fn composition_events(
    runtime: &HarnessRuntime,
    session_id: &SessionId,
) -> Vec<harness_session::CompositionActivated> {
    SessionStore::read(
        runtime.session_store().as_ref(),
        session_id,
        EventSeq::FIRST,
        128,
    )
    .await
    .unwrap()
    .into_iter()
    .filter_map(|event| match event.payload() {
        SessionEventPayload::CompositionActivated(data) => Some(data.clone()),
        _ => None,
    })
    .collect()
}

async fn append_undispatched_tool_call(
    runtime: &HarnessRuntime,
    identity: &TestIdentitySource,
    session_id: &SessionId,
) {
    let head = runtime.session_store().head(session_id).await.unwrap();
    let turn = TurnNo::new(1).unwrap();
    let step = StepNo::new(1).unwrap();
    let request_id = RequestId::new("req_composition_inflight").unwrap();
    let call_id = ToolCallId::new("call_composition_inflight").unwrap();
    let arguments_json = JsonText::new(r#"{"text":"before crash"}"#.to_owned()).unwrap();
    let request_snapshot = runtime
        .blob_store()
        .put(
            br#"{"fixture":"composition-drift"}"#.to_vec(),
            Some("application/json".to_owned()),
        )
        .await
        .unwrap();
    let history_through_seq = EventSeq::new(head.seq.get() + 3).unwrap();

    let user = Message {
        id: MessageId::new("msg_composition_inflight_user").unwrap(),
        role: Role::User,
        source: MessageSource::user(),
        content: vec![ContentBlock::text("call echo")],
    };
    let assistant = Message {
        id: MessageId::new("msg_composition_inflight_assistant").unwrap(),
        role: Role::Assistant,
        source: MessageSource::model(ProviderId::new("example-python").unwrap(), "agent-model"),
        content: vec![ContentBlock::ToolCall {
            id: call_id.clone(),
            name: "echo".to_owned(),
            arguments_json: arguments_json.clone(),
        }],
    };

    runtime
        .session_store()
        .append(
            session_id,
            head.seq,
            vec![
                NewSessionEvent::new(
                    identity.next_event_id(),
                    identity.now(),
                    SessionEventPayload::TurnStarted(TurnStarted { turn }),
                )
                .in_turn(turn),
                NewSessionEvent::new(
                    identity.next_event_id(),
                    identity.now(),
                    SessionEventPayload::StepStarted(StepStarted { turn, step }),
                )
                .in_step(turn, step),
                NewSessionEvent::new(
                    identity.next_event_id(),
                    identity.now(),
                    SessionEventPayload::UserMessage(UserMessage { message: user }),
                )
                .in_step(turn, step),
                NewSessionEvent::new(
                    identity.next_event_id(),
                    identity.now(),
                    SessionEventPayload::ModelRequested(ModelRequested {
                        request_id: request_id.clone(),
                        provider: ProviderId::new("example-python").unwrap(),
                        model: "agent-model".to_owned(),
                        history_through_seq,
                        request_snapshot,
                        attempt: 1,
                    }),
                )
                .in_step(turn, step),
                NewSessionEvent::new(
                    identity.next_event_id(),
                    identity.now(),
                    SessionEventPayload::AssistantMessage(AssistantMessage {
                        request_id,
                        message: assistant,
                        usage: None,
                    }),
                )
                .in_step(turn, step),
                NewSessionEvent::new(
                    identity.next_event_id(),
                    identity.now(),
                    SessionEventPayload::ToolCall(ToolCallRecorded {
                        call_id,
                        tool: "echo".to_owned(),
                        arguments_json,
                        side_effect: SideEffectClass::ReadOnly,
                    }),
                )
                .in_step(turn, step),
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn composition_snapshot_is_durable_and_decodable() {
    let script = provider_script();
    assert!(script.is_file(), "missing provider at {}", script.display());
    let root = temp_root("snapshot");
    let runtime = build_runtime(
        &root,
        Arc::new(TestIdentitySource::new(100, "2026-08-20T14:20:00Z")),
        "composition A",
    )
    .await;
    let session_id = runtime.create_session().await.unwrap();
    let _handle = runtime
        .open_agent(session_id.clone(), "python-agent")
        .await
        .unwrap();

    let activations = composition_events(&runtime, &session_id).await;
    assert_eq!(activations.len(), 1);
    let activation = &activations[0];
    runtime
        .blob_store()
        .verify(&activation.snapshot)
        .await
        .unwrap();
    let bytes = runtime
        .blob_store()
        .get(&activation.snapshot.id)
        .await
        .unwrap();
    let snapshot: ExecutionCompositionSnapshot = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(snapshot.schema_version, 1);
    assert_eq!(snapshot.profile, "python-agent");
    assert_eq!(snapshot.model.provider.as_str(), "example-python");
    assert!(!snapshot.model.provider_version.is_empty());
    assert_eq!(snapshot.model.model, "agent-model");
    assert_eq!(snapshot.model.system.as_deref(), Some("composition A"));
    assert_eq!(snapshot.tools.len(), 1);
    assert_eq!(snapshot.tools[0].definition.name, "echo");
    assert_eq!(snapshot.tools[0].definition.version, "1");
    assert_eq!(snapshot.tools[0].provider.as_str(), "example-python");
    assert!(!snapshot.tools[0].provider_version.is_empty());
    assert!(!snapshot.tools[0].supports_idempotency_key);
    assert_eq!(
        snapshot.tools[0].validator_identity,
        "test/object-validator/v1"
    );
    assert_eq!(snapshot.policy_identity, "harness-tools/allow-all/v1");
    assert_eq!(snapshot.max_automatic_tool_attempts, 2);

    runtime.shutdown().await.unwrap();
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn quiescent_session_rotates_epoch_but_inflight_drift_is_blocked() {
    let script = provider_script();
    assert!(script.is_file(), "missing provider at {}", script.display());

    let quiescent_root = temp_root("quiescent");
    let first = build_runtime(
        &quiescent_root,
        Arc::new(TestIdentitySource::new(1_000, "2026-08-20T14:21:00Z")),
        "composition A",
    )
    .await;
    let quiescent_session = first.create_session().await.unwrap();
    first
        .open_agent(quiescent_session.clone(), "python-agent")
        .await
        .unwrap();
    first.close_agent(&quiescent_session).await.unwrap();
    first
        .open_agent(quiescent_session.clone(), "python-agent")
        .await
        .unwrap();
    assert_eq!(
        composition_events(&first, &quiescent_session).await.len(),
        1
    );
    first.close_agent(&quiescent_session).await.unwrap();
    first.shutdown().await.unwrap();
    drop(first);

    let second = build_runtime(
        &quiescent_root,
        Arc::new(TestIdentitySource::new(2_000, "2026-08-20T14:22:00Z")),
        "composition B",
    )
    .await;
    second
        .open_agent(quiescent_session.clone(), "python-agent")
        .await
        .unwrap();
    assert_eq!(
        composition_events(&second, &quiescent_session).await.len(),
        2
    );
    second.shutdown().await.unwrap();
    drop(second);
    fs::remove_dir_all(quiescent_root).unwrap();

    let inflight_root = temp_root("inflight");
    let identity = Arc::new(TestIdentitySource::new(3_000, "2026-08-20T14:23:00Z"));
    let first = build_runtime(&inflight_root, identity.clone(), "composition A").await;
    let inflight_session = first.create_session().await.unwrap();
    first
        .open_agent(inflight_session.clone(), "python-agent")
        .await
        .unwrap();
    first.close_agent(&inflight_session).await.unwrap();

    append_undispatched_tool_call(&first, identity.as_ref(), &inflight_session).await;
    first.shutdown().await.unwrap();
    drop(first);

    let second = build_runtime(
        &inflight_root,
        Arc::new(TestIdentitySource::new(4_000, "2026-08-20T14:24:00Z")),
        "composition B",
    )
    .await;
    let error = match second
        .open_agent(inflight_session.clone(), "python-agent")
        .await
    {
        Ok(_) => panic!("composition drift unexpectedly opened an Agent"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        HarnessRuntimeError::CompositionDrift { .. }
    ));
    assert_eq!(
        composition_events(&second, &inflight_session).await.len(),
        1
    );
    second.shutdown().await.unwrap();
    drop(second);
    fs::remove_dir_all(inflight_root).unwrap();
}

#[tokio::test]
async fn unfinished_legacy_session_without_epoch_fails_closed() {
    let script = provider_script();
    assert!(script.is_file(), "missing provider at {}", script.display());
    let root = temp_root("legacy");
    let identity = Arc::new(TestIdentitySource::new(5_000, "2026-08-20T14:25:00Z"));
    let runtime = build_runtime(&root, identity.clone(), "composition A").await;
    let session_id = runtime.create_session().await.unwrap();
    let head = runtime.session_store().head(&session_id).await.unwrap();
    let turn = TurnNo::new(1).unwrap();
    runtime
        .session_store()
        .append(
            &session_id,
            head.seq,
            vec![
                NewSessionEvent::new(
                    identity.next_event_id(),
                    identity.now(),
                    SessionEventPayload::TurnStarted(TurnStarted { turn }),
                )
                .in_turn(turn),
            ],
        )
        .await
        .unwrap();

    let error = match runtime.open_agent(session_id.clone(), "python-agent").await {
        Ok(_) => panic!("unfinished legacy Session unexpectedly opened an Agent"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        HarnessRuntimeError::LegacyCompositionUnbound { .. }
    ));
    assert!(composition_events(&runtime, &session_id).await.is_empty());

    runtime.shutdown().await.unwrap();
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}
