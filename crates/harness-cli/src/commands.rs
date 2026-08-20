use std::{
    io::{self, Write as _},
    sync::Arc,
};

use harness_agent::{AgentHandle, AgentState, ExecutionGate};
use harness_config::{LoadedHarnessConfig, ResolvedScope, RuntimePlan, ScopeSelection};
use harness_runtime::{HarnessRuntime, RuntimeEventBus};
use harness_session::{
    CreateSession, SessionCreated, SessionEvent, SessionEventPayload, SessionStore,
};
use harness_storage_local::DurableLocalStorage;
use harness_types::{ContentBlock, EventSeq, Message, MessageSource, Role, SessionId};

use crate::{
    cli::{Cli, Command, ConfigCommand, InspectArgs, ResolveArgs, RunArgs, SessionCommand},
    error::CliError,
    identity::UuidIdentitySource,
    observability::RuntimeEventLog,
};

const READ_PAGE_SIZE: usize = 256;

pub async fn execute(cli: Cli) -> Result<(), CliError> {
    let loaded = LoadedHarnessConfig::load(&cli.config)
        .map_err(|error| CliError::Config(Box::new(error)))?;
    let plan = loaded
        .compile()
        .map_err(|error| CliError::Config(Box::new(error)))?;

    match cli.command {
        Command::Config(args) => match args.command {
            ConfigCommand::Check => {
                config_check(&loaded, &plan);
                Ok(())
            }
            ConfigCommand::Resolve(args) => config_resolve(args, &plan),
        },
        Command::Session(args) => match args.command {
            SessionCommand::Create => session_create(&plan).await,
        },
        Command::Run(args) => run(args, &plan).await,
        Command::Inspect(args) => inspect(args, &plan).await,
    }
}

fn config_check(loaded: &LoadedHarnessConfig, plan: &RuntimePlan) {
    println!(
        "config ok: {} (providers={}, profiles={}, workspaces={}, session_scopes={}, credentials={}, data_dir={}, runtime_events={})",
        loaded.source_path().display(),
        plan.provider_count(),
        plan.profile_count(),
        plan.workspace_count(),
        plan.session_scope_count(),
        plan.credential_count(),
        plan.data_dir().display(),
        plan.runtime_events_jsonl()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "disabled".to_owned())
    );
}

fn config_resolve(args: ResolveArgs, plan: &RuntimePlan) -> Result<(), CliError> {
    let session_id = args.session.map(parse_session_id).transpose()?;
    let resolved = resolve_scope_target(args.profile, args.workspace, session_id.as_ref(), plan)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(resolved.trace())
                .map_err(|error| CliError::ScopeSerialize(Box::new(error)))?
        );
    } else {
        let trace = resolved.trace();
        println!(
            "scope profile={} workspace={} session={}",
            trace.profile,
            trace.workspace.as_deref().unwrap_or("-"),
            trace.session_id.as_deref().unwrap_or("-")
        );
        println!("layers: {}", trace.layers.join(" -> "));
        println!(
            "model: {}/{} timeout_ms={} max_output_tokens={}",
            trace.model.provider,
            trace.model.model,
            trace.model.timeout_ms,
            trace
                .model
                .max_output_tokens
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_owned())
        );
        println!("tools: {}", trace.enabled_tools.join(", "));
        if let Some(system) = &trace.system_prompt {
            println!("system:\n{system}");
        }
    }
    Ok(())
}

async fn session_create(plan: &RuntimePlan) -> Result<(), CliError> {
    let storage = open_storage(plan)?;
    let identity = UuidIdentitySource;
    let session_id = harness_runtime::RuntimeIdSource::next_session_id(&identity);
    let event_id = harness_agent::AgentEventSource::next_event_id(&identity);
    let timestamp = harness_agent::AgentEventSource::now(&identity);
    storage
        .session_store()
        .create(CreateSession {
            session_id: session_id.clone(),
            event_id,
            timestamp,
            data: SessionCreated::default(),
        })
        .await
        .map_err(|error| CliError::Session(Box::new(error)))?;
    println!("{session_id}");
    Ok(())
}

async fn inspect(args: InspectArgs, plan: &RuntimePlan) -> Result<(), CliError> {
    let session_id = parse_session_id(args.session_id)?;
    let storage = open_storage(plan)?;
    let store = storage.session_store();
    let events = read_all(store.as_ref(), &session_id).await?;
    for event in events {
        if args.pretty {
            println!(
                "{}",
                serde_json::to_string_pretty(&event)
                    .map_err(|error| CliError::Serialize(Box::new(error)))?
            );
        } else {
            println!(
                "{}",
                serde_json::to_string(&event)
                    .map_err(|error| CliError::Serialize(Box::new(error)))?
            );
        }
    }
    Ok(())
}

async fn run(args: RunArgs, plan: &RuntimePlan) -> Result<(), CliError> {
    let session_id = parse_session_id(args.session_id)?;
    let resolved = resolve_scope_target(args.profile, args.workspace, Some(&session_id), plan)?;
    let events = RuntimeEventBus::default();
    let event_log = plan
        .runtime_events_jsonl()
        .map(|path| RuntimeEventLog::start(path, &events))
        .transpose()?;
    let result = run_with_events(session_id, resolved, plan, events).await;
    let observation = match event_log {
        Some(log) => log.stop().await,
        None => Ok(()),
    };
    result?;
    observation?;
    Ok(())
}

fn resolve_scope_target(
    requested_profile: Option<String>,
    requested_workspace: Option<String>,
    session_id: Option<&SessionId>,
    plan: &RuntimePlan,
) -> Result<ResolvedScope, CliError> {
    let profile = requested_profile
        .as_deref()
        .or_else(|| session_id.and_then(|id| plan.session_profile(id)))
        .or_else(|| plan.default_profile())
        .ok_or(CliError::MissingProfile)?
        .to_owned();
    if !plan.contains_profile(&profile) {
        return Err(CliError::ProfileNotFound(profile));
    }

    let workspace = requested_workspace
        .as_deref()
        .or_else(|| session_id.and_then(|id| plan.session_workspace(id)))
        .or_else(|| plan.default_workspace())
        .map(str::to_owned);
    if let Some(workspace) = &workspace
        && !plan.contains_workspace(workspace)
    {
        return Err(CliError::WorkspaceNotFound(workspace.clone()));
    }

    let mut selection = ScopeSelection::new(profile);
    if let Some(workspace) = workspace {
        selection = selection.with_workspace(workspace);
    }
    if let Some(session_id) = session_id {
        selection = selection.with_session(session_id.clone());
    }
    plan.resolve_scope(selection)
        .map_err(|error| CliError::Config(Box::new(error)))
}

async fn run_with_events(
    session_id: SessionId,
    resolved: ResolvedScope,
    plan: &RuntimePlan,
    events: RuntimeEventBus,
) -> Result<(), CliError> {
    let identity = Arc::new(UuidIdentitySource);
    let builder = plan
        .runtime_builder_for_scope(&resolved, identity.clone(), identity.clone())
        .map_err(|error| CliError::RuntimeBuild(Box::new(error)))?
        .runtime_event_bus(events);
    let runtime = builder
        .build()
        .await
        .map_err(|error| CliError::RuntimeBuild(Box::new(error)))?;

    let profile = resolved.profile_name().to_owned();
    let workspace = resolved.workspace().map(str::to_owned);
    let handle = match runtime.open_agent(session_id.clone(), &profile).await {
        Ok(handle) => handle,
        Err(error) => {
            let _ = runtime.shutdown().await;
            return Err(CliError::Runtime(Box::new(error)));
        }
    };

    let interaction = interactive_loop(
        &runtime,
        &handle,
        &session_id,
        &profile,
        workspace.as_deref(),
        identity.as_ref(),
    )
    .await;
    let close = runtime.close_agent(&session_id).await;
    let shutdown = runtime.shutdown().await;

    interaction?;
    close.map_err(|error| CliError::Runtime(Box::new(error)))?;
    shutdown.map_err(|error| CliError::Runtime(Box::new(error)))?;
    Ok(())
}

async fn interactive_loop(
    runtime: &HarnessRuntime,
    handle: &AgentHandle,
    session_id: &SessionId,
    profile: &str,
    workspace: Option<&str>,
    identity: &UuidIdentitySource,
) -> Result<(), CliError> {
    wait_for_turn(handle).await?;
    println!(
        "session {session_id} profile {profile} workspace {}; /quit to exit",
        workspace.unwrap_or("-")
    );
    let stdin = io::stdin();
    loop {
        print!("> ");
        io::stdout().flush().map_err(|source| CliError::Io {
            context: "flushing interactive prompt",
            source: Box::new(source),
        })?;

        let mut line = String::new();
        let bytes = stdin.read_line(&mut line).map_err(|source| CliError::Io {
            context: "reading interactive input",
            source: Box::new(source),
        })?;
        if bytes == 0 {
            break;
        }
        let text = line.trim_end_matches(&['\r', '\n'][..]);
        if text == "/quit" {
            break;
        }
        if text.is_empty() {
            continue;
        }

        let before = runtime
            .session_store()
            .head(session_id)
            .await
            .map_err(|error| CliError::Session(Box::new(error)))?
            .seq;
        handle
            .followup(Message {
                id: identity.next_message_id(),
                role: Role::User,
                source: MessageSource::user(),
                content: vec![ContentBlock::text(text)],
            })
            .await
            .map_err(|error| CliError::Agent(Box::new(error)))?;

        wait_for_turn(handle).await?;
        let events = read_after(runtime.session_store().as_ref(), session_id, before).await?;
        render_turn_events(&events);
    }
    Ok(())
}

async fn wait_for_turn(handle: &AgentHandle) -> Result<AgentState, CliError> {
    loop {
        let state = handle
            .snapshot()
            .await
            .map_err(|error| CliError::Agent(Box::new(error)))?;
        if state.projection.pending_approval.is_some() {
            return Err(CliError::ApprovalPending);
        }
        if let ExecutionGate::Blocked(block) = &state.gate {
            return Err(CliError::RecoveryBlocked(format!("{:?}", block.data)));
        }
        if state.active_operation.is_none()
            && state.projection.inbox.is_empty()
            && state.projection.lifecycle.open_turn.is_none()
            && state.projection.lifecycle.open_step.is_none()
        {
            return Ok(state);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn render_turn_events(events: &[SessionEvent]) {
    for event in events {
        match event.payload() {
            SessionEventPayload::AssistantMessage(message) => {
                for block in &message.message.content {
                    if let ContentBlock::Text { text } = block {
                        println!("assistant> {text}");
                    }
                }
            }
            SessionEventPayload::ModelFailed(failed) => {
                eprintln!(
                    "model-error> {:?}: {}",
                    failed.failure.code, failed.failure.message
                );
            }
            _ => {}
        }
    }
}

async fn read_after(
    store: &dyn SessionStore,
    session_id: &SessionId,
    previous_head: EventSeq,
) -> Result<Vec<SessionEvent>, CliError> {
    let events = read_from(store, session_id, previous_head).await?;
    Ok(events
        .into_iter()
        .filter(|event| event.seq() > previous_head)
        .collect())
}

async fn read_all(
    store: &dyn SessionStore,
    session_id: &SessionId,
) -> Result<Vec<SessionEvent>, CliError> {
    read_from(store, session_id, EventSeq::FIRST).await
}

async fn read_from(
    store: &dyn SessionStore,
    session_id: &SessionId,
    start: EventSeq,
) -> Result<Vec<SessionEvent>, CliError> {
    let head = store
        .head(session_id)
        .await
        .map_err(|error| CliError::Session(Box::new(error)))?;
    let mut from = start;
    let mut result = Vec::new();
    loop {
        let page = store
            .read(session_id, from, READ_PAGE_SIZE)
            .await
            .map_err(|error| CliError::Session(Box::new(error)))?;
        if page.is_empty() {
            return Err(CliError::EventSequence(format!(
                "SessionStore returned an empty page at seq {from} before head {}",
                head.seq
            )));
        }
        let last = page
            .last()
            .expect("non-empty SessionStore page has a last event")
            .seq();
        result.extend(page);
        if last >= head.seq {
            break;
        }
        from = last
            .checked_next()
            .map_err(|error| CliError::EventSequence(error.to_string()))?;
    }
    Ok(result)
}

fn open_storage(plan: &RuntimePlan) -> Result<DurableLocalStorage, CliError> {
    DurableLocalStorage::open(plan.data_dir()).map_err(|error| CliError::Storage(Box::new(error)))
}

fn parse_session_id(value: String) -> Result<SessionId, CliError> {
    SessionId::new(value.clone()).map_err(|error| CliError::InvalidSessionId {
        value,
        message: error.to_string(),
    })
}
