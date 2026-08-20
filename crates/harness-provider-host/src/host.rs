use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use harness_provider_protocol::{
    CapabilityCancelParams, CapabilityDescriptor, InboundMessage, InitializeParams, LlmEventParams,
    LlmStartParams, LlmStartResult, METHOD_CAPABILITY_CANCEL, METHOD_LLM_EVENT, METHOD_LLM_START,
    METHOD_PROVIDER_INITIALIZE, METHOD_PROVIDER_PING, METHOD_PROVIDER_SHUTDOWN, METHOD_TOOL_INVOKE,
    PROTOCOL_VERSION, PingParams, PingResult, ProviderManifest, RpcErrorObject, RpcId,
    RpcNotification, RpcRequest, RpcResponseOutcome, RuntimeInfo, ShutdownParams, ShutdownResult,
    ToolInvokeParams, ToolInvokeResult, WireCancelCause, WireLlmStreamEvent, WireModelRequest,
    decode_inbound_line, encode_ndjson,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{Mutex, RwLock, mpsc, oneshot},
    time::timeout,
};

const RETIRED_RPC_ID_LIMIT: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderState {
    Starting,
    Ready,
    Unhealthy,
    Stopping,
    Stopped,
}

#[derive(Clone, Debug)]
pub struct ProviderHostConfig {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: BTreeMap<OsString, OsString>,
    pub current_dir: Option<PathBuf>,
    pub runtime: RuntimeInfo,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub max_stdout_line_bytes: usize,
    pub stderr_history_lines: usize,
}

impl ProviderHostConfig {
    pub fn new(program: impl Into<PathBuf>, runtime: RuntimeInfo) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            current_dir: None,
            runtime,
            request_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(5),
            max_stdout_line_bytes: 1024 * 1024,
            stderr_history_lines: 128,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        let _ = self.env.insert(key.into(), value.into());
        self
    }

    pub fn current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    pub fn request_timeout(mut self, value: Duration) -> Self {
        self.request_timeout = value;
        self
    }

    pub fn shutdown_timeout(mut self, value: Duration) -> Self {
        self.shutdown_timeout = value;
        self
    }

    fn validate(&self) -> Result<(), ProviderHostError> {
        if self.program.as_os_str().is_empty() {
            return Err(ProviderHostError::InvalidConfig(
                "provider program must not be empty".to_owned(),
            ));
        }
        if self.runtime.name.trim().is_empty() || self.runtime.version.trim().is_empty() {
            return Err(ProviderHostError::InvalidConfig(
                "runtime name/version must not be empty".to_owned(),
            ));
        }
        if self.request_timeout.is_zero() || self.shutdown_timeout.is_zero() {
            return Err(ProviderHostError::InvalidConfig(
                "provider request/shutdown timeouts must be greater than zero".to_owned(),
            ));
        }
        if self.max_stdout_line_bytes == 0 || self.stderr_history_lines == 0 {
            return Err(ProviderHostError::InvalidConfig(
                "stdout line limit and stderr history capacity must be greater than zero"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct ProviderHost {
    inner: Arc<Inner>,
}

struct Inner {
    state: RwLock<ProviderState>,
    manifest: RwLock<Option<ProviderManifest>>,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    pending: Mutex<BTreeMap<RpcId, oneshot::Sender<PendingReply>>>,
    retired_rpc_ids: Mutex<VecDeque<RpcId>>,
    streams: Mutex<BTreeMap<String, StreamRoute>>,
    stderr_history: Mutex<VecDeque<String>>,
    next_rpc_id: AtomicU64,
    next_stream_id: AtomicU64,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    max_stdout_line_bytes: usize,
    stderr_history_lines: usize,
}

type PendingReply = Result<Value, PendingFailure>;

#[derive(Clone, Debug)]
enum PendingFailure {
    Rpc(RpcErrorObject),
    Transport(String),
    Protocol(String),
}

struct StreamRoute {
    next_seq: u64,
    tx: mpsc::UnboundedSender<Result<LlmStreamItem, ProviderStreamError>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LlmStreamItem {
    pub seq: u64,
    pub event: WireLlmStreamEvent,
}

pub struct LlmStreamHandle {
    operation_id: String,
    stream_id: String,
    rx: mpsc::UnboundedReceiver<Result<LlmStreamItem, ProviderStreamError>>,
}

impl LlmStreamHandle {
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub async fn recv(&mut self) -> Option<Result<LlmStreamItem, ProviderStreamError>> {
        self.rx.recv().await
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ProviderStreamError {
    #[error("provider stream protocol violation: {0}")]
    Protocol(String),

    #[error("provider became unavailable: {0}")]
    ProviderUnavailable(String),
}

impl ProviderHost {
    pub async fn start(config: ProviderHostConfig) -> Result<Self, ProviderHostError> {
        config.validate()?;

        let mut command = Command::new(&config.program);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &config.env {
            command.env(key, value);
        }
        if let Some(current_dir) = &config.current_dir {
            command.current_dir(current_dir);
        }

        let mut child = command.spawn().map_err(ProviderHostError::Spawn)?;
        let stdin = child
            .stdin
            .take()
            .ok_or(ProviderHostError::MissingPipe("stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(ProviderHostError::MissingPipe("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(ProviderHostError::MissingPipe("stderr"))?;

        let inner = Arc::new(Inner {
            state: RwLock::new(ProviderState::Starting),
            manifest: RwLock::new(None),
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(Some(child)),
            pending: Mutex::new(BTreeMap::new()),
            retired_rpc_ids: Mutex::new(VecDeque::new()),
            streams: Mutex::new(BTreeMap::new()),
            stderr_history: Mutex::new(VecDeque::new()),
            next_rpc_id: AtomicU64::new(1),
            next_stream_id: AtomicU64::new(1),
            request_timeout: config.request_timeout,
            shutdown_timeout: config.shutdown_timeout,
            max_stdout_line_bytes: config.max_stdout_line_bytes,
            stderr_history_lines: config.stderr_history_lines,
        });

        // Reader tasks are intentionally detached; they retain only Weak<Inner>.
        std::mem::drop(tokio::spawn(stdout_loop(stdout, Arc::downgrade(&inner))));
        std::mem::drop(tokio::spawn(stderr_loop(stderr, Arc::downgrade(&inner))));

        let host = Self { inner };
        let initialize = InitializeParams {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            runtime: config.runtime,
        };
        let manifest: ProviderManifest = host
            .request_raw(METHOD_PROVIDER_INITIALIZE, &initialize)
            .await?;
        manifest
            .validate()
            .map_err(|error| ProviderHostError::InvalidManifest(error.to_string()))?;
        *host.inner.manifest.write().await = Some(manifest);
        *host.inner.state.write().await = ProviderState::Ready;
        Ok(host)
    }

    pub async fn state(&self) -> ProviderState {
        *self.inner.state.read().await
    }

    pub async fn manifest(&self) -> Option<ProviderManifest> {
        self.inner.manifest.read().await.clone()
    }

    pub async fn recent_stderr(&self) -> Vec<String> {
        self.inner
            .stderr_history
            .lock()
            .await
            .iter()
            .cloned()
            .collect()
    }

    pub async fn ping(&self) -> Result<(), ProviderHostError> {
        self.require_state(ProviderState::Ready).await?;
        let result: PingResult = self
            .request_raw(METHOD_PROVIDER_PING, &PingParams {})
            .await?;
        if !result.ok {
            return Err(ProviderHostError::UnexpectedResponse(
                "provider.ping returned ok=false".to_owned(),
            ));
        }
        Ok(())
    }

    pub async fn invoke_tool(
        &self,
        params: ToolInvokeParams,
    ) -> Result<ToolInvokeResult, ProviderHostError> {
        self.require_state(ProviderState::Ready).await?;
        params
            .validate()
            .map_err(|error| ProviderHostError::InvalidRequest(error.to_string()))?;
        self.require_tool_capability(&params.tool).await?;
        let result: ToolInvokeResult = self.request_raw(METHOD_TOOL_INVOKE, &params).await?;
        if let Err(error) = result.validate() {
            let message = format!("provider returned invalid tool.invoke result: {error}");
            protocol_fault(&self.inner, message.clone()).await;
            return Err(ProviderHostError::Protocol(message));
        }
        Ok(result)
    }

    pub async fn start_llm(
        &self,
        operation_id: impl Into<String>,
        request: WireModelRequest,
        deadline: Option<String>,
    ) -> Result<LlmStreamHandle, ProviderHostError> {
        self.require_state(ProviderState::Ready).await?;
        let operation_id = operation_id.into();
        let stream_id = self.next_stream_id();
        let params = LlmStartParams {
            operation_id: operation_id.clone(),
            stream_id: stream_id.clone(),
            request,
            deadline,
        };
        params
            .validate()
            .map_err(|error| ProviderHostError::InvalidRequest(error.to_string()))?;
        self.require_llm_capability(&params.request.provider, &params.request.model)
            .await?;

        let (tx, rx) = mpsc::unbounded_channel();
        let replaced = self
            .inner
            .streams
            .lock()
            .await
            .insert(stream_id.clone(), StreamRoute { next_seq: 1, tx });
        debug_assert!(replaced.is_none(), "generated streamId must be unique");

        let result: Result<LlmStartResult, ProviderHostError> =
            self.request_raw(METHOD_LLM_START, &params).await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let _ = self.inner.streams.lock().await.remove(&stream_id);
                return Err(error);
            }
        };
        if result.stream_id != stream_id {
            let _ = self.inner.streams.lock().await.remove(&stream_id);
            let message = format!(
                "llm.start echoed streamId {}, expected {stream_id}",
                result.stream_id
            );
            protocol_fault(&self.inner, message.clone()).await;
            return Err(ProviderHostError::Protocol(message));
        }
        if !result.accepted {
            let _ = self.inner.streams.lock().await.remove(&stream_id);
            return Err(ProviderHostError::StreamRejected(
                result
                    .reason
                    .unwrap_or_else(|| "provider rejected LLM stream".to_owned()),
            ));
        }

        Ok(LlmStreamHandle {
            operation_id,
            stream_id,
            rx,
        })
    }

    pub async fn cancel(
        &self,
        operation_id: impl Into<String>,
        cause: WireCancelCause,
    ) -> Result<(), ProviderHostError> {
        self.require_state(ProviderState::Ready).await?;
        let params = CapabilityCancelParams {
            operation_id: operation_id.into(),
            cause,
        };
        if params.operation_id.is_empty() {
            return Err(ProviderHostError::InvalidRequest(
                "capability.cancel operationId must not be empty".to_owned(),
            ));
        }
        self.notify(METHOD_CAPABILITY_CANCEL, &params).await
    }

    pub async fn shutdown(&self) -> Result<(), ProviderHostError> {
        let previous = self.state().await;
        if previous == ProviderState::Stopped {
            return Ok(());
        }
        *self.inner.state.write().await = ProviderState::Stopping;

        let graceful_result = if previous == ProviderState::Ready {
            match self
                .request_raw::<_, ShutdownResult>(METHOD_PROVIDER_SHUTDOWN, &ShutdownParams {})
                .await
            {
                Ok(result) if result.accepted => Ok(()),
                Ok(_) => Err(ProviderHostError::UnexpectedResponse(
                    "provider.shutdown returned accepted=false".to_owned(),
                )),
                Err(error) => Err(error),
            }
        } else {
            Ok(())
        };

        if let Some(mut stdin) = self.inner.stdin.lock().await.take() {
            let _ = stdin.shutdown().await;
        }
        self.wait_or_kill_child().await?;
        *self.inner.state.write().await = ProviderState::Stopped;
        fail_all(
            &self.inner,
            PendingFailure::Transport("provider stopped".to_owned()),
            ProviderStreamError::ProviderUnavailable("provider stopped".to_owned()),
        )
        .await;

        graceful_result
    }

    async fn require_state(&self, expected: ProviderState) -> Result<(), ProviderHostError> {
        let actual = self.state().await;
        if actual != expected {
            return Err(ProviderHostError::InvalidState { expected, actual });
        }
        Ok(())
    }

    async fn require_tool_capability(&self, tool: &str) -> Result<(), ProviderHostError> {
        let manifest = self.inner.manifest.read().await;
        let declared = manifest.as_ref().is_some_and(|manifest| {
            manifest.capabilities.iter().any(|capability| {
                matches!(capability, CapabilityDescriptor::Tool { name, .. } if name == tool)
            })
        });
        if !declared {
            return Err(ProviderHostError::InvalidRequest(format!(
                "provider manifest does not declare tool {tool}"
            )));
        }
        Ok(())
    }

    async fn require_llm_capability(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<(), ProviderHostError> {
        let manifest = self.inner.manifest.read().await;
        let Some(manifest) = manifest.as_ref() else {
            return Err(ProviderHostError::InvalidRequest(
                "provider manifest is unavailable".to_owned(),
            ));
        };
        if manifest.provider_id != provider {
            return Err(ProviderHostError::InvalidRequest(format!(
                "LLM request provider {provider} does not match manifest provider {}",
                manifest.provider_id
            )));
        }
        let declared = manifest.capabilities.iter().any(|capability| {
            matches!(capability, CapabilityDescriptor::Llm { models } if models.iter().any(|candidate| candidate == model))
        });
        if !declared {
            return Err(ProviderHostError::InvalidRequest(format!(
                "provider manifest does not declare LLM model {model}"
            )));
        }
        Ok(())
    }

    async fn notify<P: Serialize>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<(), ProviderHostError> {
        let params = serde_json::to_value(params).map_err(ProviderHostError::Serialize)?;
        let message = RpcNotification::new(method, params);
        let frame = encode_ndjson(&message)
            .map_err(|error| ProviderHostError::Protocol(error.to_string()))?;
        self.write_frame(&frame).await
    }

    async fn request_raw<P, R>(&self, method: &str, params: &P) -> Result<R, ProviderHostError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = self.next_rpc_id();
        let params = serde_json::to_value(params).map_err(ProviderHostError::Serialize)?;
        let request = RpcRequest::new(id.clone(), method, params);
        let frame = encode_ndjson(&request)
            .map_err(|error| ProviderHostError::Protocol(error.to_string()))?;
        let (tx, rx) = oneshot::channel();
        let replaced = self.inner.pending.lock().await.insert(id.clone(), tx);
        debug_assert!(replaced.is_none(), "generated RpcId must be unique");

        if let Err(error) = self.write_frame(&frame).await {
            let _ = self.inner.pending.lock().await.remove(&id);
            return Err(error);
        }

        let reply = match timeout(self.inner.request_timeout, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => {
                return Err(ProviderHostError::ProviderUnavailable(format!(
                    "response channel closed while waiting for {method}"
                )));
            }
            Err(_) => {
                // Retire before removing the active request. A response racing
                // with timeout either still finds `pending`, or finds the retired
                // id after removal; there is no uncorrelated-id window.
                retire_rpc_id(&self.inner, id.clone()).await;
                let _ = self.inner.pending.lock().await.remove(&id);
                return Err(ProviderHostError::RequestTimeout {
                    method: method.to_owned(),
                });
            }
        };

        let value = match reply {
            Ok(value) => value,
            Err(PendingFailure::Rpc(error)) => {
                return Err(ProviderHostError::Rpc {
                    code: error.code,
                    message: error.message,
                    data: error.data,
                });
            }
            Err(PendingFailure::Transport(message)) => {
                return Err(ProviderHostError::ProviderUnavailable(message));
            }
            Err(PendingFailure::Protocol(message)) => {
                return Err(ProviderHostError::Protocol(message));
            }
        };

        match serde_json::from_value(value) {
            Ok(result) => Ok(result),
            Err(error) => {
                let message = format!("provider returned invalid {method} result: {error}");
                protocol_fault(&self.inner, message).await;
                Err(ProviderHostError::DeserializeResponse(error))
            }
        }
    }

    async fn write_frame(&self, frame: &[u8]) -> Result<(), ProviderHostError> {
        let mut stdin_guard = self.inner.stdin.lock().await;
        let stdin = stdin_guard.as_mut().ok_or_else(|| {
            ProviderHostError::ProviderUnavailable("provider stdin is closed".to_owned())
        })?;
        let result = async {
            stdin.write_all(frame).await?;
            stdin.flush().await
        }
        .await;
        drop(stdin_guard);

        if let Err(error) = result {
            let message = format!("provider stdin write failed: {error}");
            provider_transport_ended(&self.inner, message).await;
            return Err(ProviderHostError::Io(error));
        }
        Ok(())
    }

    fn next_rpc_id(&self) -> RpcId {
        let value = self.inner.next_rpc_id.fetch_add(1, Ordering::Relaxed);
        RpcId::new(format!("rpc_{value}"))
            .expect("generated ProviderHost RpcId is always non-empty")
    }

    fn next_stream_id(&self) -> String {
        let value = self.inner.next_stream_id.fetch_add(1, Ordering::Relaxed);
        format!("str_{value}")
    }

    async fn wait_or_kill_child(&self) -> Result<(), ProviderHostError> {
        let mut child_guard = self.inner.child.lock().await;
        let Some(child) = child_guard.as_mut() else {
            return Ok(());
        };

        match timeout(self.inner.shutdown_timeout, child.wait()).await {
            Ok(status) => {
                status.map_err(ProviderHostError::Io)?;
            }
            Err(_) => {
                child.kill().await.map_err(ProviderHostError::Io)?;
                child.wait().await.map_err(ProviderHostError::Io)?;
            }
        }
        *child_guard = None;
        Ok(())
    }
}

async fn stdout_loop(stdout: ChildStdout, inner: Weak<Inner>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        let read = match reader.read_line(&mut line).await {
            Ok(read) => read,
            Err(error) => {
                if let Some(inner) = inner.upgrade() {
                    let message = format!("provider stdout read failed: {error}");
                    if error.kind() == std::io::ErrorKind::InvalidData {
                        protocol_fault(&inner, message).await;
                    } else {
                        provider_transport_ended(&inner, message).await;
                    }
                }
                return;
            }
        };
        let Some(inner) = inner.upgrade() else {
            return;
        };
        if read == 0 {
            provider_transport_ended(&inner, "provider stdout reached EOF".to_owned()).await;
            return;
        }
        if line.len() > inner.max_stdout_line_bytes {
            protocol_fault(
                &inner,
                format!(
                    "provider stdout frame exceeded {} bytes",
                    inner.max_stdout_line_bytes
                ),
            )
            .await;
            return;
        }

        match decode_inbound_line(&line) {
            Ok(message) => {
                if let Err(message) = route_inbound(&inner, message).await {
                    protocol_fault(&inner, message).await;
                    return;
                }
            }
            Err(error) => {
                protocol_fault(&inner, error.to_string()).await;
                return;
            }
        }
    }
}

async fn stderr_loop(stderr: ChildStderr, inner: Weak<Inner>) {
    let mut lines = BufReader::new(stderr).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) | Err(_) => return,
        };
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let mut history = inner.stderr_history.lock().await;
        if history.len() >= inner.stderr_history_lines {
            let _ = history.pop_front();
        }
        history.push_back(line);
    }
}

async fn route_inbound(inner: &Arc<Inner>, message: InboundMessage) -> Result<(), String> {
    match message {
        InboundMessage::Response(response) => {
            let sender = inner.pending.lock().await.remove(&response.id);
            let Some(sender) = sender else {
                if take_retired_rpc_id(inner, &response.id).await {
                    return Ok(());
                }
                return Err(format!(
                    "provider response id {} does not correlate to an active request",
                    response.id
                ));
            };
            let reply = match response.outcome {
                RpcResponseOutcome::Success(value) => Ok(value),
                RpcResponseOutcome::Error(error) => Err(PendingFailure::Rpc(error)),
            };
            let _ = sender.send(reply);
            Ok(())
        }
        InboundMessage::Notification(notification) => {
            if notification.method != METHOD_LLM_EVENT {
                return Err(format!(
                    "provider sent unsupported notification method {}",
                    notification.method
                ));
            }
            let params: LlmEventParams = serde_json::from_value(notification.params)
                .map_err(|error| format!("invalid llm.event params: {error}"))?;
            route_llm_event(inner, params).await
        }
        InboundMessage::Request(request) => Err(format!(
            "provider-to-Core JSON-RPC requests are not supported in protocol v1: {} ({})",
            request.method, request.id
        )),
    }
}

async fn route_llm_event(inner: &Arc<Inner>, params: LlmEventParams) -> Result<(), String> {
    params
        .validate()
        .map_err(|error| format!("invalid llm.event: {error}"))?;

    let mut streams = inner.streams.lock().await;
    let Some(route) = streams.get_mut(&params.stream_id) else {
        return Err(format!(
            "llm.event references unknown streamId {}",
            params.stream_id
        ));
    };
    if params.seq != route.next_seq {
        let message = format!(
            "llm.event stream {} expected seq {}, received {}",
            params.stream_id, route.next_seq, params.seq
        );
        let tx = route.tx.clone();
        let _ = streams.remove(&params.stream_id);
        drop(streams);
        let _ = tx.send(Err(ProviderStreamError::Protocol(message.clone())));
        return Err(message);
    }

    let is_finish = params.event.is_finish();
    if !is_finish {
        route.next_seq = route
            .next_seq
            .checked_add(1)
            .ok_or_else(|| "LLM stream sequence overflow".to_owned())?;
    }
    let tx = route.tx.clone();
    if is_finish {
        let _ = streams.remove(&params.stream_id);
    }
    drop(streams);

    let _ = tx.send(Ok(LlmStreamItem {
        seq: params.seq,
        event: params.event,
    }));
    Ok(())
}

async fn protocol_fault(inner: &Arc<Inner>, message: String) {
    let state = *inner.state.read().await;
    if !matches!(state, ProviderState::Stopping | ProviderState::Stopped) {
        *inner.state.write().await = ProviderState::Unhealthy;
    }
    fail_all(
        inner,
        PendingFailure::Protocol(message.clone()),
        ProviderStreamError::Protocol(message),
    )
    .await;
}

async fn provider_transport_ended(inner: &Arc<Inner>, message: String) {
    let state = *inner.state.read().await;
    *inner.state.write().await =
        if matches!(state, ProviderState::Stopping | ProviderState::Stopped) {
            ProviderState::Stopped
        } else {
            ProviderState::Unhealthy
        };
    fail_all(
        inner,
        PendingFailure::Transport(message.clone()),
        ProviderStreamError::ProviderUnavailable(message),
    )
    .await;
}

async fn fail_all(
    inner: &Arc<Inner>,
    pending_failure: PendingFailure,
    stream_failure: ProviderStreamError,
) {
    let pending = {
        let mut guard = inner.pending.lock().await;
        std::mem::take(&mut *guard)
    };
    for sender in pending.into_values() {
        let _ = sender.send(Err(pending_failure.clone()));
    }

    let streams = {
        let mut guard = inner.streams.lock().await;
        std::mem::take(&mut *guard)
    };
    for route in streams.into_values() {
        let _ = route.tx.send(Err(stream_failure.clone()));
    }
}

async fn retire_rpc_id(inner: &Arc<Inner>, id: RpcId) {
    let mut retired = inner.retired_rpc_ids.lock().await;
    if retired.len() >= RETIRED_RPC_ID_LIMIT {
        let _ = retired.pop_front();
    }
    retired.push_back(id);
}

async fn take_retired_rpc_id(inner: &Arc<Inner>, id: &RpcId) -> bool {
    let mut retired = inner.retired_rpc_ids.lock().await;
    let Some(index) = retired.iter().position(|candidate| candidate == id) else {
        return false;
    };
    let _ = retired.remove(index);
    true
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProviderHostError {
    #[error("invalid ProviderHost configuration: {0}")]
    InvalidConfig(String),

    #[error("failed to spawn provider process: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("spawned provider process is missing piped {0}")]
    MissingPipe(&'static str),

    #[error("provider process I/O failed: {0}")]
    Io(#[source] std::io::Error),

    #[error("provider protocol violation: {0}")]
    Protocol(String),

    #[error("failed to serialize provider request: {0}")]
    Serialize(#[source] serde_json::Error),

    #[error("failed to decode provider response result: {0}")]
    DeserializeResponse(#[source] serde_json::Error),

    #[error("provider manifest is invalid: {0}")]
    InvalidManifest(String),

    #[error("provider is unavailable: {0}")]
    ProviderUnavailable(String),

    #[error("provider request {method} timed out")]
    RequestTimeout { method: String },

    #[error("provider returned JSON-RPC error {code}: {message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },

    #[error("provider state is {actual:?}, expected {expected:?}")]
    InvalidState {
        expected: ProviderState,
        actual: ProviderState,
    },

    #[error("invalid provider request: {0}")]
    InvalidRequest(String),

    #[error("provider returned an unexpected response: {0}")]
    UnexpectedResponse(String),

    #[error("provider rejected LLM stream: {0}")]
    StreamRejected(String),
}
