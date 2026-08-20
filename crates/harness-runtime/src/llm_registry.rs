use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use harness_provider_host::ProviderSlotLlmAdapter;
use harness_provider_protocol::CapabilityDescriptor;
use harness_types::ProviderId;

use crate::{HarnessRuntimeBuildError, ProviderRegistry};

struct LlmEntry {
    adapter: Arc<ProviderSlotLlmAdapter>,
    models: BTreeSet<String>,
}

/// Runtime-level model capability index derived from the baseline provider manifests.
///
/// Adapters bind to stable ProviderSlots, so compiled model routes survive a
/// compatible Provider process restart without rebuilding Agent profiles.
pub struct LlmRegistry {
    entries: BTreeMap<ProviderId, LlmEntry>,
}

impl LlmRegistry {
    pub(crate) async fn from_providers(
        providers: &ProviderRegistry,
    ) -> Result<Self, HarnessRuntimeBuildError> {
        let mut entries = BTreeMap::new();
        for provider_id in providers.provider_ids() {
            let manifest = providers
                .manifest(provider_id)
                .expect("ProviderRegistry entries always retain their baseline manifest");
            let mut models = BTreeSet::new();
            for capability in &manifest.capabilities {
                if let CapabilityDescriptor::Llm { models: declared } = capability {
                    models.extend(declared.iter().cloned());
                }
            }
            if models.is_empty() {
                continue;
            }
            let slot = providers
                .slot(provider_id)
                .expect("ProviderRegistry manifest and slot entries are paired");
            let _ = entries.insert(
                provider_id.clone(),
                LlmEntry {
                    adapter: Arc::new(ProviderSlotLlmAdapter::new(slot)),
                    models,
                },
            );
        }
        Ok(Self { entries })
    }

    pub fn supports(&self, provider_id: &ProviderId, model: &str) -> bool {
        self.entries
            .get(provider_id)
            .is_some_and(|entry| entry.models.contains(model))
    }

    pub fn models(&self, provider_id: &ProviderId) -> Vec<String> {
        self.entries
            .get(provider_id)
            .map(|entry| entry.models.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn resolve(
        &self,
        provider_id: &ProviderId,
        model: &str,
    ) -> Option<Arc<ProviderSlotLlmAdapter>> {
        let entry = self.entries.get(provider_id)?;
        if entry.models.contains(model) {
            Some(entry.adapter.clone())
        } else {
            None
        }
    }
}
