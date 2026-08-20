use std::{path::Path, sync::Arc};

use harness_agent::AgentEventSource;
use harness_runtime::{
    AgentProfile, HarnessRuntimeBuildError, HarnessRuntimeBuilder, HarnessRuntimeInfo,
    ProviderProcessSpec, RuntimeIdSource,
};

#[derive(Clone)]
pub struct RuntimePlan {
    pub(crate) runtime_info: HarnessRuntimeInfo,
    pub(crate) data_dir: std::path::PathBuf,
    pub(crate) providers: Vec<ProviderProcessSpec>,
    pub(crate) profiles: Vec<(String, AgentProfile)>,
    pub(crate) default_profile: Option<String>,
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

    pub fn profile_names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.profiles.iter().map(|(name, _)| name.as_str())
    }

    pub fn contains_profile(&self, name: &str) -> bool {
        self.profiles.iter().any(|(candidate, _)| candidate == name)
    }

    pub fn default_profile(&self) -> Option<&str> {
        self.default_profile.as_deref()
    }

    pub fn runtime_builder(
        &self,
        event_source: Arc<dyn AgentEventSource>,
        id_source: Arc<dyn RuntimeIdSource>,
    ) -> Result<HarnessRuntimeBuilder, HarnessRuntimeBuildError> {
        let mut builder =
            HarnessRuntimeBuilder::durable_local(self.data_dir.clone(), event_source, id_source)?
                .runtime_info(self.runtime_info.clone());

        for provider in &self.providers {
            builder = builder.provider(provider.clone());
        }
        for (name, profile) in &self.profiles {
            builder = builder.profile(name.clone(), profile.clone());
        }
        Ok(builder)
    }
}
