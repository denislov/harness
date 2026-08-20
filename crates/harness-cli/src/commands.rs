use std::{
    io::{self, Write as _},
    sync::Arc,
};

use harness_agent::{AgentHandle, AgentState, ExecutionGate};
use harness_config::{LoadedHarnessConfig, RuntimePlan};
use harness_runtime::{HarnessRuntime, RuntimeEventBus};
use harness_session::{
    CreateSession, SessionCreated, SessionEvent, SessionEventPayload, SessionStore,
};
use harness_storage_local::DurableLocalStorage;
use harness_types::{ContentBlock, EventSeq, Message, MessageSource, Role, SessionId};

use crate::{
    cli::{Cli, Command, ConfigCommand, InspectArgs, RunArgs, SessionCommand},
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
        "config ok: {} (providers={}, profiles={}, credentials={}, data_dir={}, runtime_events={})",
        loaded.source_path().display(),
        plan.provider_count(),
        plan.profile_count(),
        plan.credential_count(),
        plan.data_dir().display(),
        plan.runtime_events_jsonl()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "disabled".to_owned())
    );
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
    let (session_id, profile) = resolve_run_target(args, plan)?;
    let events = RuntimeEventBus::default();
    let event_log = plan
        .runtime_events_jsonl()
        .map(|path| RuntimeEventLog::start(path, &events))
        .transpose()?;
    let result = run_with_events(session_id, profile, plan, events).await;
    let observation = match event_log {
        Some(log) => log.stop().await,
        None => Ok(()),
    };
    result?;
    observation?;
    Ok(())
}

fn resolve_run_target(args: RunArgs, plan: &RuntimePlan) -> Result<(SessionId, String), CliError> {
    let session_id = parse_session_id(args.session_id)?;
    let profile = args
        .profile
        .as_deref()
        .or_else(|| plan.default_profile())
        .ok_or(CliError::MissingProfile)?
        .to_owned();
    if !plan.contains_profile(&profile) {
        return Err(CliError::ProfileNotFound(profile));
    }
    Ok((session_id, profile))
}

async fn run_with_events(
    session_id: SessionId,
    profile: String,
    plan: &RuntimePlan,
    events: RuntimeEventBus,
) -> Result<(), CliError> {
    let identity = Arc::new(UuidIdentitySource);
    let builder = plan
        .runtime_builder(identity.clone(), identity.clone())
        .map_err(|error| CliError::RuntimeBuild(Box::new(error)))?
        .runtime_event_bus(events);
    let runtime = builder
        .build()
        .await
        .map_err(|error| CliError::RuntimeBuild(Box::new(error)))?;

    let handle = match runtime.open_agent(session_id.clone(), &profile).await {
        Ok(handle) => handle,
        Err(error) => {
            let _ = runtime.shutdown().await;
            return Err(CliError::Runtime(Box::new(error)));
        }
    };

    let interaction =
        interactive_loop(&runtime, &handle, &session_id, &profile, identity.as_ref()).await;
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
    identity: &UuidIdentitySource,
) -> Result<(), CliError> {
    wait_for_turn(handle).await?;
    println!("session {session_id} profile {profile}; /quit to exit");
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
