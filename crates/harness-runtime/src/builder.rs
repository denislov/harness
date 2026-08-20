use std::{path::PathBuf, sync::Arc};

use harness_agent::AgentEventSource;
use harness_session::SessionStore;
use harness_storage::BlobStore;
use harness_storage_local::{DurableLocalStorage, MemoryBlobStore, MemorySessionStore};

use crate::{
    AgentProfile, CredentialResolver, HarnessRuntime, HarnessRuntimeBuildError, HarnessRuntimeInfo,
    LlmRegistry, ProfileRegistry, ProviderProcessSpec, ProviderRegistry,
    RejectingCredentialResolver, RuntimeBuildStage, RuntimeEventBus, RuntimeEventKind,
    RuntimeIdSource, runtime::HarnessRuntimeParts,
};

pub struct HarnessRuntimeBuilder {
    runtime_info: HarnessRuntimeInfo,
    provider_specs: Vec<ProviderProcessSpec>,
    profile_specs: Vec<(String, AgentProfile)>,
    session_store: Option<Arc<dyn SessionStore>>,
    blob_store: Option<Arc<dyn BlobStore>>,
    event_source: Option<Arc<dyn AgentEventSource>>,
    id_source: Option<Arc<dyn RuntimeIdSource>>,
    credential_resolver: Arc<dyn CredentialResolver>,
    runtime_events: RuntimeEventBus,
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
            credential_resolver: Arc::new(RejectingCredentialResolver),
            runtime_events: RuntimeEventBus::default(),
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

    /// Opens the conventional durable local storage layout and wires it into
    /// this builder. Provider/profile configuration remains explicit.
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

    pub fn credential_resolver(mut self, resolver: Arc<dyn CredentialResolver>) -> Self {
        self.credential_resolver = resolver;
        self
    }

    pub fn runtime_event_bus(mut self, events: RuntimeEventBus) -> Self {
        self.runtime_events = events;
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
        self.runtime_events
            .publish(RuntimeEventKind::RuntimeBuildStarted {
                name: self.runtime_info.name.clone(),
                version: self.runtime_info.version.clone(),
            });

        if !self.runtime_info.validate() {
            publish_build_failed(&self.runtime_events, RuntimeBuildStage::Preflight);
            return Err(HarnessRuntimeBuildError::InvalidRuntimeInfo);
        }
        if let Err(error) = ProviderRegistry::validate_specs(&self.provider_specs) {
            publish_build_failed(&self.runtime_events, RuntimeBuildStage::Preflight);
            return Err(error);
        }
        if let Err(error) = ProfileRegistry::validate_specs(&self.profile_specs) {
            publish_build_failed(&self.runtime_events, RuntimeBuildStage::Preflight);
            return Err(error);
        }

        let Self {
            runtime_info,
            provider_specs,
            profile_specs,
            session_store,
            blob_store,
            event_source,
            id_source,
            credential_resolver,
            runtime_events,
        } = self;

        let Some(session_store) = session_store else {
            publish_build_failed(&runtime_events, RuntimeBuildStage::Preflight);
            return Err(HarnessRuntimeBuildError::MissingSessionStore);
        };
        let Some(blob_store) = blob_store else {
            publish_build_failed(&runtime_events, RuntimeBuildStage::Preflight);
            return Err(HarnessRuntimeBuildError::MissingBlobStore);
        };
        let Some(event_source) = event_source else {
            publish_build_failed(&runtime_events, RuntimeBuildStage::Preflight);
            return Err(HarnessRuntimeBuildError::MissingEventSource);
        };
        let Some(id_source) = id_source else {
            publish_build_failed(&runtime_events, RuntimeBuildStage::Preflight);
            return Err(HarnessRuntimeBuildError::MissingIdSource);
        };

        let providers = match ProviderRegistry::start(
            provider_specs,
            &runtime_info,
            credential_resolver,
            runtime_events.clone(),
        )
        .await
        {
            Ok(providers) => providers,
            Err(error) => {
                publish_build_failed(&runtime_events, RuntimeBuildStage::Provider);
                return Err(error);
            }
        };
        let llms = match LlmRegistry::from_providers(&providers).await {
            Ok(llms) => llms,
            Err(error) => {
                publish_build_failed(&runtime_events, RuntimeBuildStage::Llm);
                let _ = providers.shutdown_all().await;
                return Err(error);
            }
        };
        let profiles =
            match ProfileRegistry::compile(profile_specs, &providers, &llms, blob_store.clone())
                .await
            {
                Ok(profiles) => profiles,
                Err(error) => {
                    publish_build_failed(&runtime_events, RuntimeBuildStage::Profile);
                    let _ = providers.shutdown_all().await;
                    return Err(error);
                }
            };

        let runtime = HarnessRuntime::from_parts(HarnessRuntimeParts {
            info: runtime_info.clone(),
            providers,
            llms,
            profiles,
            session_store,
            blob_store,
            event_source,
            id_source,
            events: runtime_events.clone(),
        });
        runtime_events.publish(RuntimeEventKind::RuntimeStarted {
            name: runtime_info.name,
            version: runtime_info.version,
        });
        Ok(runtime)
    }
}

fn publish_build_failed(events: &RuntimeEventBus, stage: RuntimeBuildStage) {
    events.publish(RuntimeEventKind::RuntimeBuildFailed { stage });
}
