use std::collections::{BTreeMap, BTreeSet};

use harness_provider_host::{ProviderHost, ProviderState};
use harness_provider_protocol::ProviderManifest;
use harness_types::ProviderId;

use crate::{HarnessRuntimeBuildError, HarnessRuntimeInfo, ProviderProcessSpec};

struct ProviderEntry {
    host: ProviderHost,
    manifest: ProviderManifest,
}

/// Immutable set of provider processes owned by one HarnessRuntime.
///
/// Batch 14 intentionally freezes provider membership at build time. Provider
/// crash/restart policy and dynamic registration remain supervisor work for a
/// later batch.
pub struct ProviderRegistry {
    entries: BTreeMap<ProviderId, ProviderEntry>,
    startup_order: Vec<ProviderId>,
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
        }
        Ok(())
    }

    pub(crate) async fn start(
        specs: Vec<ProviderProcessSpec>,
        runtime: &HarnessRuntimeInfo,
    ) -> Result<Self, HarnessRuntimeBuildError> {
        let mut registry = Self {
            entries: BTreeMap::new(),
            startup_order: Vec::new(),
        };

        for spec in specs {
            let expected = spec.expected_provider_id().clone();
            let host = match ProviderHost::start(spec.host_config(runtime)).await {
                Ok(host) => host,
                Err(source) => {
                    let _ = registry.shutdown_all().await;
                    return Err(HarnessRuntimeBuildError::ProviderStart {
                        expected,
                        source: Box::new(source),
                    });
                }
            };

            let Some(manifest) = host.manifest().await else {
                let _ = host.shutdown().await;
                let _ = registry.shutdown_all().await;
                return Err(HarnessRuntimeBuildError::ProviderManifestMissing(expected));
            };
            let actual = match ProviderId::new(manifest.provider_id.clone()) {
                Ok(actual) => actual,
                Err(error) => {
                    let _ = host.shutdown().await;
                    let _ = registry.shutdown_all().await;
                    return Err(HarnessRuntimeBuildError::InvalidManifestProviderId {
                        value: manifest.provider_id,
                        message: error.to_string(),
                    });
                }
            };
            if actual != expected {
                let _ = host.shutdown().await;
                let _ = registry.shutdown_all().await;
                return Err(HarnessRuntimeBuildError::ProviderIdentityMismatch {
                    expected,
                    actual,
                });
            }

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
            if let Err(error) = entry.host.shutdown().await {
                failures.push(format!("provider {provider_id}: {error}"));
            }
        }
        failures
    }
}
