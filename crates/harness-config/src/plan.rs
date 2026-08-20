use std::{path::Path, sync::Arc};

use harness_agent::AgentEventSource;
use harness_runtime::{
    AgentProfile, CredentialResolver, HarnessRuntimeBuildError, HarnessRuntimeBuilder,
    HarnessRuntimeInfo, ProviderProcessSpec, RuntimeIdSource,
};
use harness_types::SessionId;

use crate::{HarnessConfigError, ResolvedScope, ScopeSelection, scope::ScopeCatalog};

#[derive(Clone)]
pub struct RuntimePlan {
    pub(crate) runtime_info: HarnessRuntimeInfo,
    pub(crate) data_dir: std::path::PathBuf,
    pub(crate) providers: Vec<ProviderProcessSpec>,
    pub(crate) profiles: Vec<(String, AgentProfile)>,
    pub(crate) default_profile: Option<String>,
    pub(crate) default_workspace: Option<String>,
    pub(crate) credential_resolver: Arc<dyn CredentialResolver>,
    pub(crate) credential_count: usize,
    pub(crate) runtime_events_jsonl: Option<std::path::PathBuf>,
    pub(crate) scope_catalog: ScopeCatalog,
}

impl RuntimePlan {
    pub fn runtime_info(&self) -> &HarnessRuntimeInfo {
        &self.runtime_info
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    pub fn profile_count(&self) -> usize {
        self.profiles.len()
    }

    pub fn workspace_count(&self) -> usize {
        self.scope_catalog.workspace_count()
    }

    pub fn session_scope_count(&self) -> usize {
        self.scope_catalog.session_scope_count()
    }

    pub const fn credential_count(&self) -> usize {
        self.credential_count
    }

    pub fn profile_names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.profiles.iter().map(|(name, _)| name.as_str())
    }

    pub fn contains_profile(&self, name: &str) -> bool {
        self.profiles.iter().any(|(candidate, _)| candidate == name)
    }

    pub fn contains_workspace(&self, name: &str) -> bool {
        self.scope_catalog.contains_workspace(name)
    }

    pub fn default_profile(&self) -> Option<&str> {
        self.default_profile.as_deref()
    }

    pub fn default_workspace(&self) -> Option<&str> {
        self.default_workspace.as_deref()
    }

    pub fn session_profile(&self, session_id: &SessionId) -> Option<&str> {
        self.scope_catalog.session_profile(session_id)
    }

    pub fn session_workspace(&self, session_id: &SessionId) -> Option<&str> {
        self.scope_catalog.session_workspace(session_id)
    }

    pub fn runtime_events_jsonl(&self) -> Option<&Path> {
        self.runtime_events_jsonl.as_deref()
    }

    pub fn resolve_scope(
        &self,
        selection: ScopeSelection,
    ) -> Result<ResolvedScope, HarnessConfigError> {
        self.scope_catalog.resolve(selection)
    }

    pub fn runtime_builder(
        &self,
        event_source: Arc<dyn AgentEventSource>,
        id_source: Arc<dyn RuntimeIdSource>,
    ) -> Result<HarnessRuntimeBuilder, HarnessRuntimeBuildError> {
        let mut builder = self.base_runtime_builder(event_source, id_source)?;
        for (name, profile) in &self.profiles {
            builder = builder.profile(name.clone(), profile.clone());
        }
        Ok(builder)
    }

    pub fn runtime_builder_for_scope(
        &self,
        resolved: &ResolvedScope,
        event_source: Arc<dyn AgentEventSource>,
        id_source: Arc<dyn RuntimeIdSource>,
    ) -> Result<HarnessRuntimeBuilder, HarnessRuntimeBuildError> {
        Ok(self
            .base_runtime_builder(event_source, id_source)?
            .profile(resolved.profile_name().to_owned(), resolved.agent_profile()))
    }

    fn base_runtime_builder(
        &self,
        event_source: Arc<dyn AgentEventSource>,
        id_source: Arc<dyn RuntimeIdSource>,
    ) -> Result<HarnessRuntimeBuilder, HarnessRuntimeBuildError> {
        let mut builder =
            HarnessRuntimeBuilder::durable_local(self.data_dir.clone(), event_source, id_source)?
                .runtime_info(self.runtime_info.clone())
                .credential_resolver(self.credential_resolver.clone());
        for provider in &self.providers {
            builder = builder.provider(provider.clone());
        }
        Ok(builder)
    }
}
