use std::{path::PathBuf, sync::Arc};

use harness_agent::AgentEventSource;
use harness_session::SessionStore;
use harness_storage::BlobStore;
use harness_storage_local::{DurableLocalStorage, MemoryBlobStore, MemorySessionStore};

use crate::{
    AgentProfile, HarnessRuntime, HarnessRuntimeBuildError, HarnessRuntimeInfo, LlmRegistry,
    ProfileRegistry, ProviderProcessSpec, ProviderRegistry, RuntimeIdSource,
    runtime::HarnessRuntimeParts,
};

pub struct HarnessRuntimeBuilder {
    runtime_info: HarnessRuntimeInfo,
    provider_specs: Vec<ProviderProcessSpec>,
    profile_specs: Vec<(String, AgentProfile)>,
    session_store: Option<Arc<dyn SessionStore>>,
    blob_store: Option<Arc<dyn BlobStore>>,
    event_source: Option<Arc<dyn AgentEventSource>>,
    id_source: Option<Arc<dyn RuntimeIdSource>>,
}

impl Default for HarnessRuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessRuntimeBuilder {
    pub fn new() -> Self {
        Self {
            runtime_info: HarnessRuntimeInfo::default(),
            provider_specs: Vec::new(),
            profile_specs: Vec::new(),
            session_store: None,
            blob_store: None,
            event_source: None,
            id_source: None,
        }
    }

    /// Convenience composition for deterministic process-local development/tests.
    pub fn in_memory(
        event_source: Arc<dyn AgentEventSource>,
        id_source: Arc<dyn RuntimeIdSource>,
    ) -> Self {
        Self::new()
            .session_store(Arc::new(MemorySessionStore::new()))
            .blob_store(Arc::new(MemoryBlobStore::new()))
            .event_source(event_source)
            .id_source(id_source)
    }

    /// Opens the conventional Batch 15 durable local storage layout and wires
    /// it into this builder. Provider/profile configuration remains explicit.
    pub fn durable_local(
        root: impl Into<PathBuf>,
        event_source: Arc<dyn AgentEventSource>,
        id_source: Arc<dyn RuntimeIdSource>,
    ) -> Result<Self, HarnessRuntimeBuildError> {
        let storage = DurableLocalStorage::open(root).map_err(|source| {
            HarnessRuntimeBuildError::DurableLocalStorage {
                source: Box::new(source),
            }
        })?;
        Ok(Self::new()
            .session_store(storage.session_store())
            .blob_store(storage.blob_store())
            .event_source(event_source)
            .id_source(id_source))
    }

    pub fn runtime_info(mut self, info: HarnessRuntimeInfo) -> Self {
        self.runtime_info = info;
        self
    }

    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    pub fn blob_store(mut self, store: Arc<dyn BlobStore>) -> Self {
        self.blob_store = Some(store);
        self
    }

    pub fn event_source(mut self, source: Arc<dyn AgentEventSource>) -> Self {
        self.event_source = Some(source);
        self
    }

    pub fn id_source(mut self, source: Arc<dyn RuntimeIdSource>) -> Self {
        self.id_source = Some(source);
        self
    }

    pub fn provider(mut self, spec: ProviderProcessSpec) -> Self {
        self.provider_specs.push(spec);
        self
    }

    pub fn profile(mut self, name: impl Into<String>, profile: AgentProfile) -> Self {
        self.profile_specs.push((name.into(), profile));
        self
    }

    pub async fn build(self) -> Result<HarnessRuntime, HarnessRuntimeBuildError> {
        if !self.runtime_info.validate() {
            return Err(HarnessRuntimeBuildError::InvalidRuntimeInfo);
        }
        ProviderRegistry::validate_specs(&self.provider_specs)?;
        ProfileRegistry::validate_specs(&self.profile_specs)?;

        let session_store = self
            .session_store
            .ok_or(HarnessRuntimeBuildError::MissingSessionStore)?;
        let blob_store = self
            .blob_store
            .ok_or(HarnessRuntimeBuildError::MissingBlobStore)?;
        let event_source = self
            .event_source
            .ok_or(HarnessRuntimeBuildError::MissingEventSource)?;
        let id_source = self
            .id_source
            .ok_or(HarnessRuntimeBuildError::MissingIdSource)?;

        let providers = ProviderRegistry::start(self.provider_specs, &self.runtime_info).await?;
        let llms = match LlmRegistry::from_providers(&providers).await {
            Ok(llms) => llms,
            Err(error) => {
                let _ = providers.shutdown_all().await;
                return Err(error);
            }
        };
        let profiles = match ProfileRegistry::compile(
            self.profile_specs,
            &providers,
            &llms,
            blob_store.clone(),
        )
        .await
        {
            Ok(profiles) => profiles,
            Err(error) => {
                let _ = providers.shutdown_all().await;
                return Err(error);
            }
        };

        Ok(HarnessRuntime::from_parts(HarnessRuntimeParts {
            info: self.runtime_info,
            providers,
            llms,
            profiles,
            session_store,
            blob_store,
            event_source,
            id_source,
        }))
    }
}
