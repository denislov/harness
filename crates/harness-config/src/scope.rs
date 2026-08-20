use std::{collections::BTreeMap, sync::Arc};

use harness_llm::ModelOptions;
use harness_runtime::{AgentProfile, ModelBinding, RuntimeToolBinding};
use harness_tools::AllowAllToolPolicy;
use harness_types::{ProviderId, SessionId};
use serde::Serialize;

use crate::{
    HarnessConfigError, PolicyConfig, PromptMode,
    model::{DEFAULT_MAX_AUTOMATIC_TOOL_ATTEMPTS, DEFAULT_MODEL_TIMEOUT_MS},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeSelection {
    profile: String,
    workspace: Option<String>,
    session_id: Option<SessionId>,
}

impl ScopeSelection {
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            workspace: None,
            session_id: None,
        }
    }

    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    pub fn with_session(mut self, session_id: SessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }
}

#[derive(Clone)]
pub struct ResolvedScope {
    selection: ScopeSelection,
    agent_profile: AgentProfile,
    trace: ScopeResolutionTrace,
}

impl ResolvedScope {
    pub fn selection(&self) -> &ScopeSelection {
        &self.selection
    }

    pub fn profile_name(&self) -> &str {
        self.selection.profile()
    }

    pub fn workspace(&self) -> Option<&str> {
        self.selection.workspace()
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.selection.session_id()
    }

    pub fn trace(&self) -> &ScopeResolutionTrace {
        &self.trace
    }

    pub(crate) fn agent_profile(&self) -> AgentProfile {
        self.agent_profile.clone()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeResolutionTrace {
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub layers: Vec<String>,
    pub prompt_fragments: Vec<PromptFragmentTrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub model: ResolvedModelTrace,
    pub enabled_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_capability_directives: Vec<String>,
    pub policy: String,
    pub max_automatic_tool_attempts: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptFragmentTrace {
    pub layer: String,
    pub mode: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedModelTrace {
    pub provider: String,
    pub model: String,
    pub timeout_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

#[derive(Clone, Default)]
pub(crate) struct CompiledScopePatch {
    pub system: Option<String>,
    pub system_mode: PromptMode,
    pub model: CompiledModelPatch,
    pub capabilities: CompiledCapabilityPatch,
    pub policy: Option<PolicyConfig>,
    pub max_automatic_tool_attempts: Option<u32>,
}

#[derive(Clone, Default)]
pub(crate) struct CompiledModelPatch {
    pub provider: Option<ProviderId>,
    pub model: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Clone, Default)]
pub(crate) struct CompiledCapabilityPatch {
    pub enable: Vec<String>,
    pub disable: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct CompiledProfile {
    pub model_provider: ProviderId,
    pub model: String,
    pub system: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub tools: BTreeMap<String, CompiledProfileTool>,
    pub policy: Option<PolicyConfig>,
    pub max_automatic_tool_attempts: Option<u32>,
}

#[derive(Clone)]
pub(crate) struct CompiledProfileTool {
    pub binding: RuntimeToolBinding,
    pub enabled: Option<bool>,
}

#[derive(Clone, Default)]
pub(crate) struct CompiledSessionScope {
    pub profile: Option<String>,
    pub workspace: Option<String>,
    pub patch: CompiledScopePatch,
}

#[derive(Clone)]
pub(crate) struct ScopeCatalog {
    global: CompiledScopePatch,
    workspaces: BTreeMap<String, CompiledScopePatch>,
    profiles: BTreeMap<String, CompiledProfile>,
    sessions: BTreeMap<SessionId, CompiledSessionScope>,
}

impl ScopeCatalog {
    pub(crate) fn new(
        global: CompiledScopePatch,
        workspaces: BTreeMap<String, CompiledScopePatch>,
        profiles: BTreeMap<String, CompiledProfile>,
        sessions: BTreeMap<SessionId, CompiledSessionScope>,
    ) -> Self {
        Self {
            global,
            workspaces,
            profiles,
            sessions,
        }
    }

    pub(crate) fn workspace_count(&self) -> usize {
        self.workspaces.len()
    }

    pub(crate) fn session_scope_count(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn contains_workspace(&self, name: &str) -> bool {
        self.workspaces.contains_key(name)
    }

    pub(crate) fn session_profile(&self, session_id: &SessionId) -> Option<&str> {
        self.sessions
            .get(session_id)
            .and_then(|scope| scope.profile.as_deref())
    }

    pub(crate) fn session_workspace(&self, session_id: &SessionId) -> Option<&str> {
        self.sessions
            .get(session_id)
            .and_then(|scope| scope.workspace.as_deref())
    }

    pub(crate) fn resolve(
        &self,
        selection: ScopeSelection,
    ) -> Result<ResolvedScope, HarnessConfigError> {
        let profile = self.profiles.get(selection.profile()).ok_or_else(|| {
            HarnessConfigError::Invalid(format!(
                "scope references profile {:?}, which is not configured",
                selection.profile()
            ))
        })?;
        let workspace = match selection.workspace() {
            Some(name) => Some(self.workspaces.get(name).ok_or_else(|| {
                HarnessConfigError::Invalid(format!(
                    "scope references workspace {name:?}, which is not configured"
                ))
            })?),
            None => None,
        };
        let session = selection
            .session_id()
            .and_then(|session_id| self.sessions.get(session_id));

        if let Some(session_scope) = session {
            if let Some(bound_profile) = &session_scope.profile
                && bound_profile != selection.profile()
            {
                return Err(HarnessConfigError::Invalid(format!(
                    "session scope binds profile {bound_profile:?}, but resolution requested {:?}",
                    selection.profile()
                )));
            }
            if let Some(bound_workspace) = &session_scope.workspace
                && Some(bound_workspace.as_str()) != selection.workspace()
            {
                return Err(HarnessConfigError::Invalid(format!(
                    "session scope binds workspace {bound_workspace:?}, but resolution requested {:?}",
                    selection.workspace()
                )));
            }
        }

        let mut layers = vec!["global".to_owned()];
        if let Some(name) = selection.workspace() {
            layers.push(format!("workspace:{name}"));
        }
        layers.push(format!("profile:{}", selection.profile()));
        if let Some(session_id) = selection.session_id()
            && session.is_some()
        {
            layers.push(format!("session:{session_id}"));
        }

        let mut prompt_fragments = Vec::new();
        apply_prompt("global", &self.global, &mut prompt_fragments);
        if let Some(workspace_patch) = workspace {
            apply_prompt(
                &format!("workspace:{}", selection.workspace().unwrap_or_default()),
                workspace_patch,
                &mut prompt_fragments,
            );
        }
        if let Some(system) = &profile.system
            && !system.trim().is_empty()
        {
            prompt_fragments.push(PromptFragmentTrace {
                layer: format!("profile:{}", selection.profile()),
                mode: "append".to_owned(),
                text: system.clone(),
            });
        }
        if let Some(session_scope) = session {
            apply_prompt(
                &format!(
                    "session:{}",
                    selection
                        .session_id()
                        .expect("session scope requires a selected SessionId")
                ),
                &session_scope.patch,
                &mut prompt_fragments,
            );
        }
        let system_prompt = compose_prompt(&prompt_fragments);

        let mut values = ResolvedValues::default();
        values.apply_patch(&self.global);
        if let Some(workspace_patch) = workspace {
            values.apply_patch(workspace_patch);
        }
        values.apply_profile(profile);
        if let Some(session_scope) = session {
            values.apply_patch(&session_scope.patch);
        }

        let model_provider = values
            .model_provider
            .expect("profile always supplies a model provider");
        let model_name = values.model.expect("profile always supplies a model name");
        let timeout_ms = values.timeout_ms.unwrap_or(DEFAULT_MODEL_TIMEOUT_MS);
        let max_output_tokens = values.max_output_tokens;
        let max_automatic_tool_attempts = values
            .max_automatic_tool_attempts
            .unwrap_or(DEFAULT_MAX_AUTOMATIC_TOOL_ATTEMPTS);
        let policy = values.policy.ok_or_else(|| {
            HarnessConfigError::Invalid(format!(
                "scope for profile {:?} does not resolve a Tool policy",
                selection.profile()
            ))
        })?;

        let mut tool_states = profile
            .tools
            .keys()
            .cloned()
            .map(|name| (name, true))
            .collect::<BTreeMap<_, _>>();
        let mut ignored_capability_directives = Vec::new();
        apply_capability_patch(
            "global",
            &self.global.capabilities,
            &mut tool_states,
            &mut ignored_capability_directives,
        );
        if let Some(workspace_patch) = workspace {
            apply_capability_patch(
                &format!("workspace:{}", selection.workspace().unwrap_or_default()),
                &workspace_patch.capabilities,
                &mut tool_states,
                &mut ignored_capability_directives,
            );
        }
        for (name, tool) in &profile.tools {
            if let Some(enabled) = tool.enabled {
                let previous = tool_states.insert(name.clone(), enabled);
                debug_assert!(previous.is_some(), "profile tool state is pre-populated");
            }
        }
        if let Some(session_scope) = session {
            apply_capability_patch(
                &format!(
                    "session:{}",
                    selection
                        .session_id()
                        .expect("session scope requires a selected SessionId")
                ),
                &session_scope.patch.capabilities,
                &mut tool_states,
                &mut ignored_capability_directives,
            );
        }

        let enabled_tools = tool_states
            .iter()
            .filter(|(_, enabled)| **enabled)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let disabled_tools = tool_states
            .iter()
            .filter(|(_, enabled)| !**enabled)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();

        let model = ModelBinding::new(model_provider.clone(), model_name.clone())
            .with_options(ModelOptions { max_output_tokens })
            .with_timeout_ms(timeout_ms);
        let model = if let Some(system) = &system_prompt {
            model.with_system(system.clone())
        } else {
            model
        };
        let policy_name = policy_name(policy).to_owned();
        let mut agent_profile = AgentProfile::new(model, policy_impl(policy))
            .with_max_automatic_tool_attempts(max_automatic_tool_attempts);
        for tool_name in &enabled_tools {
            let tool = profile
                .tools
                .get(tool_name)
                .expect("enabled tool names originate from profile tools");
            agent_profile = agent_profile.with_tool(tool.binding.clone());
        }

        let trace = ScopeResolutionTrace {
            profile: selection.profile().to_owned(),
            workspace: selection.workspace().map(str::to_owned),
            session_id: selection.session_id().map(ToString::to_string),
            layers,
            prompt_fragments,
            system_prompt,
            model: ResolvedModelTrace {
                provider: model_provider.to_string(),
                model: model_name,
                timeout_ms,
                max_output_tokens,
            },
            enabled_tools,
            disabled_tools,
            ignored_capability_directives,
            policy: policy_name,
            max_automatic_tool_attempts,
        };

        Ok(ResolvedScope {
            selection,
            agent_profile,
            trace,
        })
    }
}

fn apply_prompt(layer: &str, patch: &CompiledScopePatch, fragments: &mut Vec<PromptFragmentTrace>) {
    let Some(system) = patch.system.as_ref() else {
        return;
    };
    if system.trim().is_empty() {
        return;
    }
    if patch.system_mode == PromptMode::Replace {
        fragments.clear();
    }
    fragments.push(PromptFragmentTrace {
        layer: layer.to_owned(),
        mode: match patch.system_mode {
            PromptMode::Append => "append",
            PromptMode::Replace => "replace",
        }
        .to_owned(),
        text: system.clone(),
    });
}

fn compose_prompt(fragments: &[PromptFragmentTrace]) -> Option<String> {
    if fragments.is_empty() {
        None
    } else {
        Some(
            fragments
                .iter()
                .map(|fragment| fragment.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        )
    }
}

#[derive(Default)]
struct ResolvedValues {
    model_provider: Option<ProviderId>,
    model: Option<String>,
    timeout_ms: Option<u64>,
    max_output_tokens: Option<u32>,
    policy: Option<PolicyConfig>,
    max_automatic_tool_attempts: Option<u32>,
}

impl ResolvedValues {
    fn apply_patch(&mut self, patch: &CompiledScopePatch) {
        if let Some(provider) = &patch.model.provider {
            self.model_provider = Some(provider.clone());
        }
        if let Some(model) = &patch.model.model {
            self.model = Some(model.clone());
        }
        if patch.model.timeout_ms.is_some() {
            self.timeout_ms = patch.model.timeout_ms;
        }
        if patch.model.max_output_tokens.is_some() {
            self.max_output_tokens = patch.model.max_output_tokens;
        }
        if patch.policy.is_some() {
            self.policy = patch.policy;
        }
        if patch.max_automatic_tool_attempts.is_some() {
            self.max_automatic_tool_attempts = patch.max_automatic_tool_attempts;
        }
    }

    fn apply_profile(&mut self, profile: &CompiledProfile) {
        self.model_provider = Some(profile.model_provider.clone());
        self.model = Some(profile.model.clone());
        if profile.timeout_ms.is_some() {
            self.timeout_ms = profile.timeout_ms;
        }
        if profile.max_output_tokens.is_some() {
            self.max_output_tokens = profile.max_output_tokens;
        }
        if profile.policy.is_some() {
            self.policy = profile.policy;
        }
        if profile.max_automatic_tool_attempts.is_some() {
            self.max_automatic_tool_attempts = profile.max_automatic_tool_attempts;
        }
    }
}

fn apply_capability_patch(
    layer: &str,
    patch: &CompiledCapabilityPatch,
    states: &mut BTreeMap<String, bool>,
    ignored: &mut Vec<String>,
) {
    for name in &patch.enable {
        if let Some(state) = states.get_mut(name) {
            *state = true;
        } else {
            ignored.push(format!("{layer}:enable:{name}"));
        }
    }
    for name in &patch.disable {
        if let Some(state) = states.get_mut(name) {
            *state = false;
        } else {
            ignored.push(format!("{layer}:disable:{name}"));
        }
    }
}

fn policy_impl(policy: PolicyConfig) -> Arc<dyn harness_tools::ToolPolicy> {
    match policy {
        PolicyConfig::AllowAll => Arc::new(AllowAllToolPolicy),
    }
}

const fn policy_name(policy: PolicyConfig) -> &'static str {
    match policy {
        PolicyConfig::AllowAll => "allow-all",
    }
}
