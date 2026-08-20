use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use harness_provider_host::{ProviderHost, ProviderHostConfig, ProviderState};
use harness_provider_protocol::ProviderManifest;
use harness_types::ProviderId;

use crate::{
    CredentialResolver, HarnessRuntimeBuildError, HarnessRuntimeInfo, ProviderProcessSpec,
    RuntimeEventBus, RuntimeEventKind,
};

struct ProviderEntry {
    host: ProviderHost,
    manifest: ProviderManifest,
}

/// Immutable set of provider processes owned by one HarnessRuntime.
pub struct ProviderRegistry {
    entries: BTreeMap<ProviderId, ProviderEntry>,
    startup_order: Vec<ProviderId>,
    events: RuntimeEventBus,
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
    ) -> Result<Self, HarnessRuntimeBuildError> {
        let mut registry = Self {
            entries: BTreeMap::new(),
            startup_order: Vec::new(),
            events,
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

            let Some(manifest) = host.manifest().await else {
                registry
                    .events
                    .publish(RuntimeEventKind::ProviderStartFailed {
                        provider: expected.clone(),
                    });
                let _ = host.shutdown().await;
                let _ = registry.shutdown_all().await;
                return Err(HarnessRuntimeBuildError::ProviderManifestMissing(expected));
            };
            let actual = match ProviderId::new(manifest.provider_id.clone()) {
                Ok(actual) => actual,
                Err(error) => {
                    registry
                        .events
                        .publish(RuntimeEventKind::ProviderStartFailed {
                            provider: expected.clone(),
                        });
                    let _ = host.shutdown().await;
                    let _ = registry.shutdown_all().await;
                    return Err(HarnessRuntimeBuildError::InvalidManifestProviderId {
                        value: manifest.provider_id,
                        message: error.to_string(),
                    });
                }
            };
            if actual != expected {
                registry
                    .events
                    .publish(RuntimeEventKind::ProviderStartFailed {
                        provider: expected.clone(),
                    });
                let _ = host.shutdown().await;
                let _ = registry.shutdown_all().await;
                return Err(HarnessRuntimeBuildError::ProviderIdentityMismatch {
                    expected,
                    actual,
                });
            }

            registry.events.publish(RuntimeEventKind::ProviderReady {
                provider: actual.clone(),
                provider_version: manifest.provider_version.clone(),
            });
            registry.startup_order.push(actual.clone());
            let replaced = registry
                .entries
                .insert(actual, ProviderEntry { host, manifest });
            debug_assert!(
                replaced.is_none(),
                "provider ids were preflighted as unique"
            );
        }

        Ok(registry)
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

    pub(crate) fn host(&self, provider_id: &ProviderId) -> Option<ProviderHost> {
        self.entries
            .get(provider_id)
            .map(|entry| entry.host.clone())
    }

    pub async fn state(&self, provider_id: &ProviderId) -> Option<ProviderState> {
        let host = self.host(provider_id)?;
        Some(host.state().await)
    }

    pub(crate) fn manifest(&self, provider_id: &ProviderId) -> Option<&ProviderManifest> {
        self.entries.get(provider_id).map(|entry| &entry.manifest)
    }

    pub(crate) async fn shutdown_all(&self) -> Vec<String> {
        let mut failures = Vec::new();
        for provider_id in self.startup_order.iter().rev() {
            let Some(entry) = self.entries.get(provider_id) else {
                continue;
            };
            self.events.publish(RuntimeEventKind::ProviderStopping {
                provider: provider_id.clone(),
            });
            let failed = match entry.host.shutdown().await {
                Ok(()) => false,
                Err(error) => {
                    failures.push(format!("provider {provider_id}: {error}"));
                    true
                }
            };
            self.events.publish(RuntimeEventKind::ProviderStopped {
                provider: provider_id.clone(),
                failed,
            });
        }
        failures
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
