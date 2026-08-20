use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use harness_agent::{AgentActorConfig, AgentLlmRuntime, AgentToolRuntime, DEFAULT_LLM_TIMEOUT_MS};
use harness_llm::{LlmProvider, ModelOptions, ModelRequestConfig};
use harness_provider_host::ProviderHostToolAdapter;
use harness_storage::BlobStore;
use harness_tools::{
    ToolArgumentValidator, ToolDefinition, ToolExecutor, ToolPolicy, ToolRegistration, ToolRegistry,
};
use harness_types::ProviderId;

use crate::{HarnessRuntimeBuildError, LlmRegistry, ProviderRegistry};

#[derive(Clone, Debug, PartialEq)]
pub struct ModelBinding {
    pub provider: ProviderId,
    pub model: String,
    pub system: Option<String>,
    pub options: ModelOptions,
    pub timeout_ms: u64,
}

impl ModelBinding {
    pub fn new(provider: ProviderId, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            system: None,
            options: ModelOptions::default(),
            timeout_ms: DEFAULT_LLM_TIMEOUT_MS,
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_options(mut self, options: ModelOptions) -> Self {
        self.options = options;
        self
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

#[derive(Clone)]
pub struct RuntimeToolBinding {
    pub definition: ToolDefinition,
    pub provider: ProviderId,
    pub validator: Arc<dyn ToolArgumentValidator>,
}

impl RuntimeToolBinding {
    pub fn new(
        definition: ToolDefinition,
        provider: ProviderId,
        validator: Arc<dyn ToolArgumentValidator>,
    ) -> Self {
        Self {
            definition,
            provider,
            validator,
        }
    }
}

#[derive(Clone)]
pub struct AgentProfile {
    pub model: ModelBinding,
    pub tools: Vec<RuntimeToolBinding>,
    pub policy: Arc<dyn ToolPolicy>,
    pub max_automatic_tool_attempts: u32,
    pub actor_config: AgentActorConfig,
}

impl AgentProfile {
    pub fn new(model: ModelBinding, policy: Arc<dyn ToolPolicy>) -> Self {
        Self {
            model,
            tools: Vec::new(),
            policy,
            max_automatic_tool_attempts: 2,
            actor_config: AgentActorConfig::default(),
        }
    }

    pub fn with_tool(mut self, binding: RuntimeToolBinding) -> Self {
        self.tools.push(binding);
        self
    }

    pub fn with_max_automatic_tool_attempts(mut self, attempts: u32) -> Self {
        self.max_automatic_tool_attempts = attempts;
        self
    }

    pub fn with_actor_config(mut self, config: AgentActorConfig) -> Self {
        self.actor_config = config;
        self
    }
}

#[derive(Clone)]
pub(crate) struct CompiledAgentProfile {
    pub llm_runtime: AgentLlmRuntime,
    pub tool_runtime: AgentToolRuntime,
    pub actor_config: AgentActorConfig,
}

/// Immutable profile catalog compiled against the provider manifests available
/// when HarnessRuntime is built.
pub struct ProfileRegistry {
    entries: BTreeMap<String, CompiledAgentProfile>,
}

impl ProfileRegistry {
    pub(crate) fn validate_specs(
        profiles: &[(String, AgentProfile)],
    ) -> Result<(), HarnessRuntimeBuildError> {
        let mut names = BTreeSet::new();
        for (name, _) in profiles {
            if name.trim().is_empty() {
                return Err(HarnessRuntimeBuildError::EmptyProfileName);
            }
            if !names.insert(name.clone()) {
                return Err(HarnessRuntimeBuildError::DuplicateProfile(name.clone()));
            }
        }
        Ok(())
    }

    pub(crate) async fn compile(
        profiles: Vec<(String, AgentProfile)>,
        providers: &ProviderRegistry,
        llms: &LlmRegistry,
        blob_store: Arc<dyn BlobStore>,
    ) -> Result<Self, HarnessRuntimeBuildError> {
        let mut entries = BTreeMap::new();
        for (name, profile) in profiles {
            let compiled =
                compile_profile(&name, profile, providers, llms, blob_store.clone()).await?;
            let replaced = entries.insert(name, compiled);
            debug_assert!(
                replaced.is_none(),
                "profile names were preflighted as unique"
            );
        }
        Ok(Self { entries })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub(crate) fn resolve(&self, name: &str) -> Option<&CompiledAgentProfile> {
        self.entries.get(name)
    }
}

async fn compile_profile(
    profile_name: &str,
    profile: AgentProfile,
    providers: &ProviderRegistry,
    llms: &LlmRegistry,
    blob_store: Arc<dyn BlobStore>,
) -> Result<CompiledAgentProfile, HarnessRuntimeBuildError> {
    if profile.actor_config.mailbox_capacity == 0 {
        return Err(HarnessRuntimeBuildError::InvalidActorConfig {
            profile: profile_name.to_owned(),
            message: "mailbox_capacity must be greater than zero".to_owned(),
        });
    }
    if profile.actor_config.bootstrap_page_size == 0 {
        return Err(HarnessRuntimeBuildError::InvalidActorConfig {
            profile: profile_name.to_owned(),
            message: "bootstrap_page_size must be greater than zero".to_owned(),
        });
    }

    if !providers.contains(&profile.model.provider) {
        return Err(HarnessRuntimeBuildError::ProfileProviderNotFound {
            profile: profile_name.to_owned(),
            provider: profile.model.provider,
        });
    }
    let Some(llm_adapter) = llms.resolve(&profile.model.provider, &profile.model.model) else {
        return Err(HarnessRuntimeBuildError::ProfileModelNotDeclared {
            profile: profile_name.to_owned(),
            provider: profile.model.provider,
            model: profile.model.model,
        });
    };

    let request_config = ModelRequestConfig {
        provider: profile.model.provider.clone(),
        model: profile.model.model.clone(),
        system: profile.model.system,
        tools: Vec::new(),
        options: profile.model.options,
    };
    let llm_provider: Arc<dyn LlmProvider> = llm_adapter;
    let llm_runtime = AgentLlmRuntime::new(request_config, llm_provider, blob_store)
        .map_err(|source| HarnessRuntimeBuildError::LlmRuntime {
            profile: profile_name.to_owned(),
            source: Box::new(source),
        })?
        .with_timeout_ms(profile.model.timeout_ms)
        .map_err(|source| HarnessRuntimeBuildError::LlmRuntime {
            profile: profile_name.to_owned(),
            source: Box::new(source),
        })?;

    let mut registrations = Vec::with_capacity(profile.tools.len());
    for binding in profile.tools {
        let tool_name = binding.definition.name.clone();
        let Some(host) = providers.host(&binding.provider) else {
            return Err(HarnessRuntimeBuildError::ProfileProviderNotFound {
                profile: profile_name.to_owned(),
                provider: binding.provider,
            });
        };
        let adapter = ProviderHostToolAdapter::from_definition(host, &binding.definition)
            .await
            .map_err(|source| HarnessRuntimeBuildError::ToolAdapter {
                profile: profile_name.to_owned(),
                tool: tool_name,
                source: Box::new(source),
            })?;
        let executor: Arc<dyn ToolExecutor> = Arc::new(adapter);
        let registration = ToolRegistration::new(binding.definition, executor, binding.validator)
            .map_err(|source| HarnessRuntimeBuildError::ToolRegistry {
            profile: profile_name.to_owned(),
            source: Box::new(source),
        })?;
        registrations.push(registration);
    }

    let tool_registry = ToolRegistry::new(registrations).map_err(|source| {
        HarnessRuntimeBuildError::ToolRegistry {
            profile: profile_name.to_owned(),
            source: Box::new(source),
        }
    })?;
    let tool_runtime = AgentToolRuntime::new(
        Arc::new(tool_registry),
        profile.policy,
        profile.max_automatic_tool_attempts,
    )
    .map_err(|source| HarnessRuntimeBuildError::ToolRuntime {
        profile: profile_name.to_owned(),
        source: Box::new(source),
    })?;

    Ok(CompiledAgentProfile {
        llm_runtime,
        tool_runtime,
        actor_config: profile.actor_config,
    })
}
