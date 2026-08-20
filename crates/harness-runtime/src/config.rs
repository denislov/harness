use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};

use harness_provider_host::ProviderHostConfig;
use harness_provider_protocol::RuntimeInfo;
use harness_types::ProviderId;
use thiserror::Error;

use crate::CredentialKey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessRuntimeInfo {
    pub name: String,
    pub version: String,
}

impl Default for HarnessRuntimeInfo {
    fn default() -> Self {
        Self {
            name: "harness-runtime".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

impl HarnessRuntimeInfo {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }

    pub(crate) fn validate(&self) -> bool {
        !self.name.trim().is_empty() && !self.version.trim().is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderSupervisorConfig {
    health_poll_interval: Duration,
    initial_restart_backoff: Duration,
    max_restart_backoff: Duration,
}

impl Default for ProviderSupervisorConfig {
    fn default() -> Self {
        Self {
            health_poll_interval: Duration::from_millis(100),
            initial_restart_backoff: Duration::from_millis(100),
            max_restart_backoff: Duration::from_secs(5),
        }
    }
}

impl ProviderSupervisorConfig {
    pub fn new(
        health_poll_interval: Duration,
        initial_restart_backoff: Duration,
        max_restart_backoff: Duration,
    ) -> Result<Self, ProviderSupervisorConfigError> {
        if health_poll_interval.is_zero() {
            return Err(ProviderSupervisorConfigError::ZeroDuration(
                "health_poll_interval",
            ));
        }
        if initial_restart_backoff.is_zero() {
            return Err(ProviderSupervisorConfigError::ZeroDuration(
                "initial_restart_backoff",
            ));
        }
        if max_restart_backoff.is_zero() {
            return Err(ProviderSupervisorConfigError::ZeroDuration(
                "max_restart_backoff",
            ));
        }
        if max_restart_backoff < initial_restart_backoff {
            return Err(ProviderSupervisorConfigError::MaxBackoffBeforeInitial);
        }
        Ok(Self {
            health_poll_interval,
            initial_restart_backoff,
            max_restart_backoff,
        })
    }

    pub const fn health_poll_interval(&self) -> Duration {
        self.health_poll_interval
    }

    pub const fn initial_restart_backoff(&self) -> Duration {
        self.initial_restart_backoff
    }

    pub const fn max_restart_backoff(&self) -> Duration {
        self.max_restart_backoff
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ProviderSupervisorConfigError {
    #[error("Provider supervisor {0} must be greater than zero")]
    ZeroDuration(&'static str),

    #[error("Provider supervisor max_restart_backoff must be >= initial_restart_backoff")]
    MaxBackoffBeforeInitial,
}

#[derive(Clone, Debug)]
pub struct ProviderProcessSpec {
    expected_provider_id: ProviderId,
    program: PathBuf,
    args: Vec<OsString>,
    env: BTreeMap<OsString, OsString>,
    credential_env: BTreeMap<OsString, CredentialKey>,
    current_dir: Option<PathBuf>,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    max_stdout_line_bytes: usize,
    stderr_history_lines: usize,
}

impl ProviderProcessSpec {
    pub fn new(expected_provider_id: ProviderId, program: impl Into<PathBuf>) -> Self {
        Self {
            expected_provider_id,
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            credential_env: BTreeMap::new(),
            current_dir: None,
            request_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(5),
            max_stdout_line_bytes: 1024 * 1024,
            stderr_history_lines: 128,
        }
    }

    pub fn expected_provider_id(&self) -> &ProviderId {
        &self.expected_provider_id
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        let _ = self.env.insert(key.into(), value.into());
        self
    }

    pub fn credential_env(mut self, key: impl Into<OsString>, credential: CredentialKey) -> Self {
        let _ = self.credential_env.insert(key.into(), credential);
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

    pub fn max_stdout_line_bytes(mut self, value: usize) -> Self {
        self.max_stdout_line_bytes = value;
        self
    }

    pub fn stderr_history_lines(mut self, value: usize) -> Self {
        self.stderr_history_lines = value;
        self
    }

    pub(crate) fn environment_conflict(&self) -> Option<&OsString> {
        self.credential_env
            .keys()
            .find(|key| self.env.contains_key(*key))
    }

    pub(crate) fn credential_bindings(
        &self,
    ) -> impl ExactSizeIterator<Item = (&OsString, &CredentialKey)> {
        self.credential_env.iter()
    }

    pub(crate) fn host_config(&self, runtime: &HarnessRuntimeInfo) -> ProviderHostConfig {
        let mut config = ProviderHostConfig::new(
            self.program.clone(),
            RuntimeInfo {
                name: runtime.name.clone(),
                version: runtime.version.clone(),
            },
        );
        config.args = self.args.clone();
        config.env = self.env.clone();
        config.current_dir = self.current_dir.clone();
        config.request_timeout = self.request_timeout;
        config.shutdown_timeout = self.shutdown_timeout;
        config.max_stdout_line_bytes = self.max_stdout_line_bytes;
        config.stderr_history_lines = self.stderr_history_lines;
        config
    }
}
