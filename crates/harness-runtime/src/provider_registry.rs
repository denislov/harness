use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use harness_provider_host::{
    ProviderHost, ProviderHostConfig, ProviderHostError, ProviderSlot, ProviderSlotStatus,
    ProviderState,
};
use harness_provider_protocol::ProviderManifest;
use harness_types::ProviderId;
use tokio::{sync::watch, task::JoinHandle, time::sleep};

use crate::{
    CredentialResolver, HarnessRuntimeBuildError, HarnessRuntimeInfo, ProviderProcessSpec,
    ProviderQuarantineReason, ProviderSupervisorConfig, RuntimeEventBus, RuntimeEventKind,
};

struct ProviderEntry {
    slot: ProviderSlot,
    manifest: ProviderManifest,
    spec: ProviderProcessSpec,
}

/// Stable provider catalog owned by one HarnessRuntime.
///
/// Each entry exposes one long-lived [`ProviderSlot`]. A supervisor may replace
/// the process generation behind that slot only after the restarted manifest is
/// semantically compatible with the baseline manifest captured during Runtime
/// build. Compiled LLM and Tool adapters therefore never pin one ProviderHost.
pub struct ProviderRegistry {
    entries: BTreeMap<ProviderId, ProviderEntry>,
    startup_order: Vec<ProviderId>,
    events: RuntimeEventBus,
    supervisor_shutdown: watch::Sender<bool>,
    supervisor_tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl ProviderRegistry {
    pub(crate) fn validate_specs(
        specs: &[ProviderProcessSpec],
    ) -> Result<(), HarnessRuntimeBuildError> {
        let mut ids = BTreeSet::new();
        for spec in specs {
            let id = spec.expected_provider_id().clone();
            if !ids.insert(id.clone()) {
                return Err(HarnessRuntimeBuildError::DuplicateProvider(id));
            }
            if let Some(key) = spec.environment_conflict() {
                return Err(HarnessRuntimeBuildError::ProviderEnvironmentConflict {
                    provider: id,
                    environment: key.to_string_lossy().into_owned(),
                });
            }
        }
        Ok(())
    }

    pub(crate) async fn start(
        specs: Vec<ProviderProcessSpec>,
        runtime: &HarnessRuntimeInfo,
        credentials: Arc<dyn CredentialResolver>,
        events: RuntimeEventBus,
        supervisor_config: ProviderSupervisorConfig,
    ) -> Result<Self, HarnessRuntimeBuildError> {
        let (supervisor_shutdown, _) = watch::channel(false);
        let mut registry = Self {
            entries: BTreeMap::new(),
            startup_order: Vec::new(),
            events,
            supervisor_shutdown,
            supervisor_tasks: Mutex::new(Vec::new()),
        };

        for spec in specs {
            let expected = spec.expected_provider_id().clone();
            registry.events.publish(RuntimeEventKind::ProviderStarting {
                provider: expected.clone(),
            });
            let host_config =
                match resolve_host_config(&spec, runtime, credentials.as_ref(), &registry.events)
                    .await
                {
                    Ok(config) => config,
                    Err(error) => {
                        registry
                            .events
                            .publish(RuntimeEventKind::ProviderStartFailed {
                                provider: expected.clone(),
                            });
                        let _ = registry.shutdown_all().await;
                        return Err(error);
                    }
                };
            let host = match ProviderHost::start(host_config).await {
                Ok(host) => host,
                Err(source) => {
                    registry
                        .events
                        .publish(RuntimeEventKind::ProviderStartFailed {
                            provider: expected.clone(),
                        });
                    let _ = registry.shutdown_all().await;
                    return Err(HarnessRuntimeBuildError::ProviderStart {
                        expected,
                        source: Box::new(source),
                    });
                }
            };

            let manifest = match validate_initial_manifest(&expected, &host).await {
                Ok(manifest) => manifest,
                Err(error) => {
                    registry
                        .events
                        .publish(RuntimeEventKind::ProviderStartFailed {
                            provider: expected.clone(),
                        });
                    let _ = host.shutdown().await;
                    let _ = registry.shutdown_all().await;
                    return Err(error);
                }
            };
            registry.events.publish(RuntimeEventKind::ProviderReady {
                provider: expected.clone(),
                provider_version: manifest.provider_version.clone(),
            });

            let slot = ProviderSlot::new(expected.clone(), host, manifest.clone())
                .expect("initial ProviderHost manifest identity was already validated");
            registry.startup_order.push(expected.clone());
            let replaced = registry.entries.insert(
                expected,
                ProviderEntry {
                    slot,
                    manifest,
                    spec,
                },
            );
            debug_assert!(
                replaced.is_none(),
                "provider ids were preflighted as unique"
            );
        }

        registry.spawn_supervisors(runtime.clone(), credentials, supervisor_config);
        Ok(registry)
    }

    fn spawn_supervisors(
        &mut self,
        runtime: HarnessRuntimeInfo,
        credentials: Arc<dyn CredentialResolver>,
        config: ProviderSupervisorConfig,
    ) {
        let tasks = self
            .supervisor_tasks
            .get_mut()
            .expect("ProviderRegistry supervisor task mutex is not poisoned");
        for (provider_id, entry) in &self.entries {
            let task = tokio::spawn(supervise_provider(
                provider_id.clone(),
                entry.slot.clone(),
                entry.spec.clone(),
                runtime.clone(),
                credentials.clone(),
                self.events.clone(),
                config,
                self.supervisor_shutdown.subscribe(),
            ));
            tasks.push(task);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, provider_id: &ProviderId) -> bool {
        self.entries.contains_key(provider_id)
    }

    pub fn provider_ids(&self) -> impl ExactSizeIterator<Item = &ProviderId> {
        self.entries.keys()
    }

    pub fn status(&self, provider_id: &ProviderId) -> Option<ProviderSlotStatus> {
        self.entries
            .get(provider_id)
            .map(|entry| entry.slot.status())
    }

    pub fn generation(&self, provider_id: &ProviderId) -> Option<u32> {
        self.status(provider_id).map(ProviderSlotStatus::generation)
    }

    pub(crate) fn slot(&self, provider_id: &ProviderId) -> Option<ProviderSlot> {
        self.entries
            .get(provider_id)
            .map(|entry| entry.slot.clone())
    }

    pub async fn state(&self, provider_id: &ProviderId) -> Option<ProviderState> {
        let entry = self.entries.get(provider_id)?;
        match entry.slot.status() {
            ProviderSlotStatus::Ready { .. } => {
                let generation = entry.slot.current()?;
                Some(generation.host().state().await)
            }
            ProviderSlotStatus::Unavailable { .. } | ProviderSlotStatus::Quarantined { .. } => {
                Some(ProviderState::Unhealthy)
            }
            ProviderSlotStatus::Stopped { .. } => Some(ProviderState::Stopped),
        }
    }

    /// Returns the immutable baseline manifest used for Runtime composition and
    /// restart compatibility checks. Compatible restarts never replace it.
    pub(crate) fn manifest(&self, provider_id: &ProviderId) -> Option<&ProviderManifest> {
        self.entries.get(provider_id).map(|entry| &entry.manifest)
    }

    pub(crate) async fn shutdown_all(&self) -> Vec<String> {
        let _ = self.supervisor_shutdown.send(true);
        let tasks = {
            let mut guard = self
                .supervisor_tasks
                .lock()
                .expect("ProviderRegistry supervisor task mutex is not poisoned");
            std::mem::take(&mut *guard)
        };

        let mut failures = Vec::new();
        for task in tasks {
            if let Err(error) = task.await {
                failures.push(format!("provider supervisor task failed to join: {error}"));
            }
        }

        for provider_id in self.startup_order.iter().rev() {
            let Some(entry) = self.entries.get(provider_id) else {
                continue;
            };
            self.events.publish(RuntimeEventKind::ProviderStopping {
                provider: provider_id.clone(),
            });
            let failed = match entry.slot.host_for_shutdown() {
                Some(host) => match host.shutdown().await {
                    Ok(()) => false,
                    Err(error) => {
                        failures.push(format!("provider {provider_id}: {error}"));
                        true
                    }
                },
                None => false,
            };
            entry.slot.mark_stopped();
            self.events.publish(RuntimeEventKind::ProviderStopped {
                provider: provider_id.clone(),
                failed,
            });
        }
        failures
    }
}

async fn validate_initial_manifest(
    expected: &ProviderId,
    host: &ProviderHost,
) -> Result<ProviderManifest, HarnessRuntimeBuildError> {
    let Some(manifest) = host.manifest().await else {
        return Err(HarnessRuntimeBuildError::ProviderManifestMissing(
            expected.clone(),
        ));
    };
    let actual = ProviderId::new(manifest.provider_id.clone()).map_err(|error| {
        HarnessRuntimeBuildError::InvalidManifestProviderId {
            value: manifest.provider_id.clone(),
            message: error.to_string(),
        }
    })?;
    if &actual != expected {
        return Err(HarnessRuntimeBuildError::ProviderIdentityMismatch {
            expected: expected.clone(),
            actual,
        });
    }
    Ok(manifest)
}

async fn supervise_provider(
    provider_id: ProviderId,
    slot: ProviderSlot,
    spec: ProviderProcessSpec,
    runtime: HarnessRuntimeInfo,
    credentials: Arc<dyn CredentialResolver>,
    events: RuntimeEventBus,
    config: ProviderSupervisorConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }

        let failed_generation = match slot.status() {
            ProviderSlotStatus::Ready { generation } => {
                if !sleep_or_shutdown(config.health_poll_interval(), &mut shutdown).await {
                    return;
                }
                let Some(current) = slot.current() else {
                    continue;
                };
                if current.generation() != generation {
                    continue;
                }
                if current.host().state().await == ProviderState::Ready {
                    continue;
                }
                let _ = slot.mark_unavailable(generation);
                events.publish(RuntimeEventKind::ProviderUnhealthy {
                    provider: provider_id.clone(),
                    generation,
                });
                generation
            }
            ProviderSlotStatus::Unavailable { generation } => {
                events.publish(RuntimeEventKind::ProviderUnhealthy {
                    provider: provider_id.clone(),
                    generation,
                });
                generation
            }
            ProviderSlotStatus::Quarantined { .. } | ProviderSlotStatus::Stopped { .. } => {
                return;
            }
        };

        if let Some(host) = slot.host_for_shutdown() {
            let _ = host.shutdown().await;
        }

        let Some(next_generation) = failed_generation.checked_add(1) else {
            let _ = slot.quarantine(failed_generation);
            events.publish(RuntimeEventKind::ProviderQuarantined {
                provider: provider_id.clone(),
                generation: failed_generation,
                reason: ProviderQuarantineReason::GenerationExhausted,
            });
            return;
        };

        let mut attempt = 1_u32;
        let mut backoff = config.initial_restart_backoff();
        loop {
            if *shutdown.borrow() {
                return;
            }
            events.publish(RuntimeEventKind::ProviderRestarting {
                provider: provider_id.clone(),
                generation: failed_generation,
                attempt,
            });

            let restart = restart_once(
                &provider_id,
                &slot,
                &spec,
                &runtime,
                credentials.as_ref(),
                &events,
            );
            let outcome = tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                    continue;
                }
                outcome = restart => outcome,
            };

            match outcome {
                RestartOutcome::Ready { host, manifest } => {
                    let provider_version = manifest.provider_version.clone();
                    if slot.replace(next_generation, host, manifest).is_err() {
                        return;
                    }
                    events.publish(RuntimeEventKind::ProviderRestarted {
                        provider: provider_id.clone(),
                        generation: next_generation,
                        provider_version,
                    });
                    break;
                }
                RestartOutcome::Retry => {
                    events.publish(RuntimeEventKind::ProviderRestartFailed {
                        provider: provider_id.clone(),
                        generation: failed_generation,
                        attempt,
                    });
                    if !sleep_or_shutdown(backoff, &mut shutdown).await {
                        return;
                    }
                    backoff =
                        std::cmp::min(backoff.saturating_mul(2), config.max_restart_backoff());
                    attempt = attempt.saturating_add(1);
                }
                RestartOutcome::Quarantine { reason } => {
                    let _ = slot.quarantine(failed_generation);
                    events.publish(RuntimeEventKind::ProviderQuarantined {
                        provider: provider_id.clone(),
                        generation: failed_generation,
                        reason,
                    });
                    return;
                }
            }
        }
    }
}

enum RestartOutcome {
    Ready {
        host: ProviderHost,
        manifest: ProviderManifest,
    },
    Retry,
    Quarantine {
        reason: ProviderQuarantineReason,
    },
}

async fn restart_once(
    expected: &ProviderId,
    slot: &ProviderSlot,
    spec: &ProviderProcessSpec,
    runtime: &HarnessRuntimeInfo,
    credentials: &dyn CredentialResolver,
    events: &RuntimeEventBus,
) -> RestartOutcome {
    let host_config = match resolve_host_config(spec, runtime, credentials, events).await {
        Ok(config) => config,
        Err(_) => return RestartOutcome::Retry,
    };
    let host = match ProviderHost::start(host_config).await {
        Ok(host) => host,
        Err(ProviderHostError::InvalidManifest(_)) => {
            return RestartOutcome::Quarantine {
                reason: ProviderQuarantineReason::InvalidManifest,
            };
        }
        Err(_) => return RestartOutcome::Retry,
    };
    let Some(manifest) = host.manifest().await else {
        let _ = host.shutdown().await;
        return RestartOutcome::Retry;
    };
    let actual = match ProviderId::new(manifest.provider_id.clone()) {
        Ok(actual) => actual,
        Err(_) => {
            let _ = host.shutdown().await;
            return RestartOutcome::Quarantine {
                reason: ProviderQuarantineReason::InvalidIdentity,
            };
        }
    };
    if &actual != expected {
        let _ = host.shutdown().await;
        return RestartOutcome::Quarantine {
            reason: ProviderQuarantineReason::IdentityMismatch,
        };
    }
    if !slot.manifest_compatible(&manifest) {
        let _ = host.shutdown().await;
        return RestartOutcome::Quarantine {
            reason: ProviderQuarantineReason::ManifestDrift,
        };
    }
    RestartOutcome::Ready { host, manifest }
}

async fn sleep_or_shutdown(
    duration: std::time::Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    if *shutdown.borrow() {
        return false;
    }
    tokio::select! {
        _ = sleep(duration) => true,
        changed = shutdown.changed() => changed.is_ok() && !*shutdown.borrow(),
    }
}

async fn resolve_host_config(
    spec: &ProviderProcessSpec,
    runtime: &HarnessRuntimeInfo,
    credentials: &dyn CredentialResolver,
    events: &RuntimeEventBus,
) -> Result<ProviderHostConfig, HarnessRuntimeBuildError> {
    let provider = spec.expected_provider_id().clone();
    let mut config = spec.host_config(runtime);
    for (environment, credential) in spec.credential_bindings() {
        let value = match credentials.resolve(credential).await {
            Ok(value) => value,
            Err(source) => {
                let environment = environment.to_string_lossy().into_owned();
                events.publish(RuntimeEventKind::CredentialResolutionFailed {
                    provider: provider.clone(),
                    environment: environment.clone(),
                    credential: credential.as_str().to_owned(),
                });
                return Err(HarnessRuntimeBuildError::ProviderCredential {
                    provider,
                    environment,
                    credential: credential.clone(),
                    source: Box::new(source),
                });
            }
        };
        let previous = config
            .env
            .insert(environment.clone(), value.expose_os_str().to_os_string());
        debug_assert!(
            previous.is_none(),
            "plain and credential environment keys were preflighted as disjoint"
        );
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::{CredentialKey, CredentialResolveError, SecretValue};

    struct StaticResolver;

    #[async_trait]
    impl CredentialResolver for StaticResolver {
        async fn resolve(
            &self,
            key: &CredentialKey,
        ) -> Result<SecretValue, CredentialResolveError> {
            assert_eq!(key.as_str(), "provider-token");
            Ok(SecretValue::new("resolved-secret"))
        }
    }

    #[tokio::test]
    async fn credential_binding_is_resolved_only_into_host_environment() {
        let provider = ProviderId::new("example").unwrap();
        let spec = ProviderProcessSpec::new(provider, "provider")
            .credential_env("TOKEN", CredentialKey::new("provider-token").unwrap());
        let events = RuntimeEventBus::default();
        let config = resolve_host_config(
            &spec,
            &HarnessRuntimeInfo::default(),
            &StaticResolver,
            &events,
        )
        .await
        .unwrap();

        assert_eq!(
            config.env.get(&std::ffi::OsString::from("TOKEN")),
            Some(&std::ffi::OsString::from("resolved-secret"))
        );
    }
}
