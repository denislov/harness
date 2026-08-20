use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use harness_runtime::{
    CredentialKey, CredentialResolver, HarnessRuntimeInfo, ProviderProcessSpec, RuntimeToolBinding,
};
use harness_tools::{ToolArgumentValidationError, ToolArgumentValidator, ToolDefinition};
use harness_types::{JsonText, ProviderId, SessionId};

use crate::{
    CredentialConfig, EnvironmentCredentialResolver, HARNESS_CONFIG_SCHEMA_VERSION, HarnessConfig,
    HarnessConfigError, RuntimePlan, ScopeConfig, ScopeSelection, ToolConfig,
    scope::{
        CompiledCapabilityPatch, CompiledModelPatch, CompiledProfile, CompiledProfileTool,
        CompiledScopePatch, CompiledSessionScope, ScopeCatalog,
    },
};

#[derive(Clone, Debug)]
pub struct LoadedHarnessConfig {
    source_path: PathBuf,
    base_dir: PathBuf,
    config: HarnessConfig,
}

impl LoadedHarnessConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, HarnessConfigError> {
        let requested = path.as_ref().to_path_buf();
        let source_path = std::fs::canonicalize(requested.as_path()).map_err(|source| {
            HarnessConfigError::ResolvePath {
                path: requested,
                source: Box::new(source),
            }
        })?;
        let text = std::fs::read_to_string(source_path.as_path()).map_err(|source| {
            HarnessConfigError::Read {
                path: source_path.clone(),
                source: Box::new(source),
            }
        })?;
        let config: HarnessConfig =
            toml::from_str(text.as_str()).map_err(|source| HarnessConfigError::Parse {
                path: source_path.clone(),
                source: Box::new(source),
            })?;
        let base_dir = source_path
            .parent()
            .expect("canonical config path always has a parent")
            .to_path_buf();
        Ok(Self {
            source_path,
            base_dir,
            config,
        })
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn config(&self) -> &HarnessConfig {
        &self.config
    }

    pub fn compile(&self) -> Result<RuntimePlan, HarnessConfigError> {
        compile_config(&self.config, &self.base_dir)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ObjectArgumentValidator;

impl ToolArgumentValidator for ObjectArgumentValidator {
    fn validate(
        &self,
        _definition: &ToolDefinition,
        arguments_json: &JsonText,
    ) -> Result<(), ToolArgumentValidationError> {
        let value: serde_json::Value = serde_json::from_str(arguments_json.as_str())
            .map_err(|error| ToolArgumentValidationError::new(error.to_string()))?;
        if value.is_object() {
            Ok(())
        } else {
            Err(ToolArgumentValidationError::new(
                "configured CLI tools require a JSON object argument",
            ))
        }
    }

    fn composition_identity(&self) -> String {
        "harness-config/object-json-validator/v1".to_owned()
    }
}

fn compile_config(
    config: &HarnessConfig,
    base_dir: &Path,
) -> Result<RuntimePlan, HarnessConfigError> {
    validate_runtime_config(config)?;

    let data_dir = resolve_path(base_dir, &config.runtime.data_dir);
    let runtime_info = HarnessRuntimeInfo {
        name: config.runtime.name.clone(),
        ..HarnessRuntimeInfo::default()
    };
    let runtime_events_jsonl = config
        .observability
        .runtime_events_jsonl
        .as_ref()
        .map(|path| resolve_path(base_dir, path));
    if runtime_events_jsonl
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        return Err(HarnessConfigError::Invalid(
            "observability.runtime_events_jsonl must not be empty".to_owned(),
        ));
    }

    let credentials = compile_credentials(config)?;
    let (providers, provider_id_values) = compile_providers(config, base_dir, &credentials.keys)?;

    let global = compile_scope_patch("global", &config.global, &provider_id_values)?;
    let workspaces = compile_workspaces(config, &provider_id_values)?;
    let compiled_profiles = compile_profiles(config, &provider_id_values)?;
    let sessions = compile_sessions(config, &provider_id_values, &compiled_profiles, &workspaces)?;

    if let Some(default_profile) = &config.runtime.default_profile
        && !compiled_profiles.contains_key(default_profile)
    {
        return Err(HarnessConfigError::Invalid(format!(
            "runtime.default_profile {default_profile:?} is not defined in profiles"
        )));
    }
    if let Some(default_workspace) = &config.runtime.default_workspace
        && !workspaces.contains_key(default_workspace)
    {
        return Err(HarnessConfigError::Invalid(format!(
            "runtime.default_workspace {default_workspace:?} is not defined in workspaces"
        )));
    }

    let scope_catalog = ScopeCatalog::new(global, workspaces, compiled_profiles, sessions);
    validate_session_resolutions(config, &scope_catalog)?;

    let mut profiles = Vec::with_capacity(config.profiles.len());
    for profile_name in config.profiles.keys() {
        let mut selection = ScopeSelection::new(profile_name.clone());
        if let Some(default_workspace) = &config.runtime.default_workspace {
            selection = selection.with_workspace(default_workspace.clone());
        }
        let resolved = scope_catalog.resolve(selection)?;
        profiles.push((profile_name.clone(), resolved.agent_profile()));
    }

    Ok(RuntimePlan {
        runtime_info,
        data_dir,
        providers,
        profiles,
        default_profile: config.runtime.default_profile.clone(),
        default_workspace: config.runtime.default_workspace.clone(),
        credential_resolver: credentials.resolver,
        credential_count: credentials.count,
        runtime_events_jsonl,
        scope_catalog,
    })
}

fn validate_runtime_config(config: &HarnessConfig) -> Result<(), HarnessConfigError> {
    if config.schema_version != HARNESS_CONFIG_SCHEMA_VERSION {
        return Err(HarnessConfigError::Invalid(format!(
            "unsupported config schema_version {}; expected {HARNESS_CONFIG_SCHEMA_VERSION}",
            config.schema_version
        )));
    }
    if config.runtime.name.trim().is_empty() {
        return Err(HarnessConfigError::Invalid(
            "runtime.name must not be empty".to_owned(),
        ));
    }
    if config.runtime.data_dir.as_os_str().is_empty() {
        return Err(HarnessConfigError::Invalid(
            "runtime.data_dir must not be empty".to_owned(),
        ));
    }
    Ok(())
}

struct CompiledCredentials {
    keys: BTreeMap<String, CredentialKey>,
    resolver: Arc<dyn CredentialResolver>,
    count: usize,
}

fn compile_credentials(config: &HarnessConfig) -> Result<CompiledCredentials, HarnessConfigError> {
    let mut credential_keys = BTreeMap::new();
    let mut credential_variables = BTreeMap::new();
    for (name, credential) in &config.credentials {
        let key = CredentialKey::new(name.clone()).map_err(|error| {
            HarnessConfigError::InvalidCredential {
                credential: name.clone(),
                message: error.to_string(),
            }
        })?;
        match credential {
            CredentialConfig::Env { variable } => {
                if variable.trim().is_empty() {
                    return Err(HarnessConfigError::InvalidCredential {
                        credential: name.clone(),
                        message: "environment variable name must not be empty".to_owned(),
                    });
                }
                let previous =
                    credential_variables.insert(key.clone(), OsString::from(variable.as_str()));
                debug_assert!(
                    previous.is_none(),
                    "credential names originate from TOML table keys"
                );
            }
        }
        let previous = credential_keys.insert(name.clone(), key);
        debug_assert!(
            previous.is_none(),
            "credential names originate from TOML table keys"
        );
    }
    let credential_count = credential_variables.len();
    let resolver: Arc<dyn CredentialResolver> =
        Arc::new(EnvironmentCredentialResolver::new(credential_variables));
    Ok(CompiledCredentials {
        keys: credential_keys,
        resolver,
        count: credential_count,
    })
}

fn compile_providers(
    config: &HarnessConfig,
    base_dir: &Path,
    credential_keys: &BTreeMap<String, CredentialKey>,
) -> Result<(Vec<ProviderProcessSpec>, BTreeMap<String, ProviderId>), HarnessConfigError> {
    let mut provider_ids = BTreeSet::new();
    let mut provider_id_values = BTreeMap::new();
    let mut providers = Vec::with_capacity(config.providers.len());
    for provider in &config.providers {
        validate_provider_config(
            provider.id.as_str(),
            provider.program.as_str(),
            provider.request_timeout_ms,
            provider.shutdown_timeout_ms,
            provider.max_stdout_line_bytes,
            provider.stderr_history_lines,
        )?;
        let id = ProviderId::new(provider.id.clone()).map_err(|error| {
            HarnessConfigError::InvalidProviderId {
                value: provider.id.clone(),
                message: error.to_string(),
            }
        })?;
        if !provider_ids.insert(id.clone()) {
            return Err(HarnessConfigError::DuplicateProvider(provider.id.clone()));
        }
        let previous = provider_id_values.insert(provider.id.clone(), id.clone());
        debug_assert!(
            previous.is_none(),
            "provider ids were preflighted as unique"
        );

        let program = resolve_program(base_dir, &provider.program);
        let cwd = provider
            .cwd
            .as_ref()
            .map(|path| resolve_path(base_dir, path))
            .unwrap_or_else(|| base_dir.to_path_buf());
        let mut spec = ProviderProcessSpec::new(id, program)
            .current_dir(cwd)
            .request_timeout(Duration::from_millis(provider.request_timeout_ms))
            .shutdown_timeout(Duration::from_millis(provider.shutdown_timeout_ms))
            .max_stdout_line_bytes(provider.max_stdout_line_bytes)
            .stderr_history_lines(provider.stderr_history_lines);
        for arg in &provider.args {
            spec = spec.arg(arg.clone());
        }
        for (key, value) in &provider.env {
            if key.trim().is_empty() {
                return Err(HarnessConfigError::Invalid(format!(
                    "provider {:?} contains an empty env key",
                    provider.id
                )));
            }
            spec = spec.env(key.clone(), value.clone());
        }
        for (environment, credential_name) in &provider.credentials {
            if environment.trim().is_empty() {
                return Err(HarnessConfigError::Invalid(format!(
                    "provider {:?} contains an empty credential environment key",
                    provider.id
                )));
            }
            if provider.env.contains_key(environment) {
                return Err(HarnessConfigError::ProviderEnvironmentConflict {
                    provider: provider.id.clone(),
                    environment: environment.clone(),
                });
            }
            let credential = credential_keys
                .get(credential_name)
                .cloned()
                .ok_or_else(|| HarnessConfigError::UnknownCredentialReference {
                    provider: provider.id.clone(),
                    environment: environment.clone(),
                    credential: credential_name.clone(),
                })?;
            spec = spec.credential_env(environment.clone(), credential);
        }
        providers.push(spec);
    }
    Ok((providers, provider_id_values))
}

fn compile_workspaces(
    config: &HarnessConfig,
    providers: &BTreeMap<String, ProviderId>,
) -> Result<BTreeMap<String, CompiledScopePatch>, HarnessConfigError> {
    let mut workspaces = BTreeMap::new();
    for (name, scope) in &config.workspaces {
        if name.trim().is_empty() {
            return Err(HarnessConfigError::Invalid(
                "workspace names must not be empty".to_owned(),
            ));
        }
        let patch = compile_scope_patch(&format!("workspace {name:?}"), scope, providers)?;
        let previous = workspaces.insert(name.clone(), patch);
        debug_assert!(
            previous.is_none(),
            "workspace names originate from TOML table keys"
        );
    }
    Ok(workspaces)
}

fn compile_profiles(
    config: &HarnessConfig,
    providers: &BTreeMap<String, ProviderId>,
) -> Result<BTreeMap<String, CompiledProfile>, HarnessConfigError> {
    let mut profiles = BTreeMap::new();
    for (profile_name, profile) in &config.profiles {
        if profile_name.trim().is_empty() {
            return Err(HarnessConfigError::Invalid(
                "profile names must not be empty".to_owned(),
            ));
        }
        if profile.model.model.trim().is_empty() {
            return Err(HarnessConfigError::Invalid(format!(
                "profile {profile_name:?} model.model must not be empty"
            )));
        }
        validate_optional_timeout(
            &format!("profile {profile_name:?} model.timeout_ms"),
            profile.model.timeout_ms,
        )?;
        validate_optional_output_tokens(
            &format!("profile {profile_name:?} model.max_output_tokens"),
            profile.model.max_output_tokens,
        )?;
        validate_optional_attempts(
            &format!("profile {profile_name:?} max_automatic_tool_attempts"),
            profile.max_automatic_tool_attempts,
        )?;

        let model_provider = providers
            .get(&profile.model.provider)
            .cloned()
            .ok_or_else(|| HarnessConfigError::UnknownProviderReference {
                profile: profile_name.clone(),
                provider: profile.model.provider.clone(),
            })?;

        let mut tools = BTreeMap::new();
        let mut tool_names = BTreeSet::new();
        for tool in &profile.tools {
            if !tool_names.insert(tool.name.clone()) {
                return Err(HarnessConfigError::Invalid(format!(
                    "profile {profile_name:?} configures tool {:?} more than once",
                    tool.name
                )));
            }
            let tool_provider = providers.get(&tool.provider).cloned().ok_or_else(|| {
                HarnessConfigError::UnknownToolProviderReference {
                    profile: profile_name.clone(),
                    tool: tool.name.clone(),
                    provider: tool.provider.clone(),
                }
            })?;
            let definition = compile_tool_definition(profile_name, tool)?;
            definition.validate().map_err(|error| {
                HarnessConfigError::Invalid(format!(
                    "profile {profile_name:?} tool {:?} is invalid: {error}",
                    tool.name
                ))
            })?;
            let entry = CompiledProfileTool {
                binding: RuntimeToolBinding::new(
                    definition,
                    tool_provider,
                    Arc::new(ObjectArgumentValidator),
                ),
                enabled: tool.enabled,
            };
            let previous = tools.insert(tool.name.clone(), entry);
            debug_assert!(previous.is_none(), "duplicate profile tools were rejected");
        }

        let compiled = CompiledProfile {
            model_provider,
            model: profile.model.model.clone(),
            system: profile.model.system.clone(),
            timeout_ms: profile.model.timeout_ms,
            max_output_tokens: profile.model.max_output_tokens,
            tools,
            policy: profile.policy,
            max_automatic_tool_attempts: profile.max_automatic_tool_attempts,
        };
        let previous = profiles.insert(profile_name.clone(), compiled);
        debug_assert!(
            previous.is_none(),
            "profile names originate from TOML table keys"
        );
    }
    Ok(profiles)
}

fn compile_sessions(
    config: &HarnessConfig,
    providers: &BTreeMap<String, ProviderId>,
    profiles: &BTreeMap<String, CompiledProfile>,
    workspaces: &BTreeMap<String, CompiledScopePatch>,
) -> Result<BTreeMap<SessionId, CompiledSessionScope>, HarnessConfigError> {
    let mut sessions = BTreeMap::new();
    for (session_value, session) in &config.sessions {
        let session_id = SessionId::new(session_value.clone()).map_err(|error| {
            HarnessConfigError::Invalid(format!(
                "session scope key {session_value:?} is not a valid SessionId: {error}"
            ))
        })?;
        if let Some(profile) = &session.profile
            && !profiles.contains_key(profile)
        {
            return Err(HarnessConfigError::Invalid(format!(
                "session scope {session_value:?} references profile {profile:?}, which is not configured"
            )));
        }
        if let Some(workspace) = &session.workspace
            && !workspaces.contains_key(workspace)
        {
            return Err(HarnessConfigError::Invalid(format!(
                "session scope {session_value:?} references workspace {workspace:?}, which is not configured"
            )));
        }
        let patch = compile_scope_patch(
            &format!("session {session_value:?}"),
            &session.scope,
            providers,
        )?;
        let previous = sessions.insert(
            session_id,
            CompiledSessionScope {
                profile: session.profile.clone(),
                workspace: session.workspace.clone(),
                patch,
            },
        );
        debug_assert!(
            previous.is_none(),
            "session ids originate from TOML table keys"
        );
    }
    Ok(sessions)
}

fn compile_scope_patch(
    label: &str,
    scope: &ScopeConfig,
    providers: &BTreeMap<String, ProviderId>,
) -> Result<CompiledScopePatch, HarnessConfigError> {
    validate_optional_timeout(&format!("{label} model.timeout_ms"), scope.model.timeout_ms)?;
    validate_optional_output_tokens(
        &format!("{label} model.max_output_tokens"),
        scope.model.max_output_tokens,
    )?;
    validate_optional_attempts(
        &format!("{label} max_automatic_tool_attempts"),
        scope.max_automatic_tool_attempts,
    )?;

    let provider = match &scope.model.provider {
        Some(value) => Some(providers.get(value).cloned().ok_or_else(|| {
            HarnessConfigError::Invalid(format!(
                "{label} model.provider {value:?} is not configured"
            ))
        })?),
        None => None,
    };
    if scope
        .model
        .model
        .as_ref()
        .is_some_and(|model| model.trim().is_empty())
    {
        return Err(HarnessConfigError::Invalid(format!(
            "{label} model.model must not be empty"
        )));
    }

    let enable = validate_capability_names(label, "enable", &scope.capabilities.enable)?;
    let disable = validate_capability_names(label, "disable", &scope.capabilities.disable)?;
    if let Some(name) = enable.intersection(&disable).next() {
        return Err(HarnessConfigError::Invalid(format!(
            "{label} capability {name:?} appears in both enable and disable"
        )));
    }

    Ok(CompiledScopePatch {
        system: scope.system.clone(),
        system_mode: scope.system_mode,
        model: CompiledModelPatch {
            provider,
            model: scope.model.model.clone(),
            timeout_ms: scope.model.timeout_ms,
            max_output_tokens: scope.model.max_output_tokens,
        },
        capabilities: CompiledCapabilityPatch {
            enable: scope.capabilities.enable.clone(),
            disable: scope.capabilities.disable.clone(),
        },
        policy: scope.policy,
        max_automatic_tool_attempts: scope.max_automatic_tool_attempts,
    })
}

fn validate_capability_names(
    label: &str,
    action: &str,
    names: &[String],
) -> Result<BTreeSet<String>, HarnessConfigError> {
    let mut unique = BTreeSet::new();
    for name in names {
        if name.trim().is_empty() {
            return Err(HarnessConfigError::Invalid(format!(
                "{label} capabilities.{action} contains an empty tool name"
            )));
        }
        if !unique.insert(name.clone()) {
            return Err(HarnessConfigError::Invalid(format!(
                "{label} capabilities.{action} contains duplicate tool {name:?}"
            )));
        }
    }
    Ok(unique)
}

fn validate_session_resolutions(
    config: &HarnessConfig,
    catalog: &ScopeCatalog,
) -> Result<(), HarnessConfigError> {
    for session_value in config.sessions.keys() {
        let session_id =
            SessionId::new(session_value.clone()).expect("session ids were preflighted");
        let profile = catalog
            .session_profile(&session_id)
            .or(config.runtime.default_profile.as_deref());
        let Some(profile) = profile else {
            continue;
        };
        let workspace = catalog
            .session_workspace(&session_id)
            .or(config.runtime.default_workspace.as_deref());
        let mut selection = ScopeSelection::new(profile.to_owned()).with_session(session_id);
        if let Some(workspace) = workspace {
            selection = selection.with_workspace(workspace.to_owned());
        }
        let _ = catalog.resolve(selection)?;
    }
    Ok(())
}

fn validate_optional_timeout(label: &str, value: Option<u64>) -> Result<(), HarnessConfigError> {
    if value == Some(0) {
        return Err(HarnessConfigError::Invalid(format!(
            "{label} must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_optional_output_tokens(
    label: &str,
    value: Option<u32>,
) -> Result<(), HarnessConfigError> {
    if value == Some(0) {
        return Err(HarnessConfigError::Invalid(format!(
            "{label} must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_optional_attempts(label: &str, value: Option<u32>) -> Result<(), HarnessConfigError> {
    if value == Some(0) {
        return Err(HarnessConfigError::Invalid(format!(
            "{label} must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_provider_config(
    id: &str,
    program: &str,
    request_timeout_ms: u64,
    shutdown_timeout_ms: u64,
    max_stdout_line_bytes: usize,
    stderr_history_lines: usize,
) -> Result<(), HarnessConfigError> {
    if id.trim().is_empty() {
        return Err(HarnessConfigError::Invalid(
            "provider id must not be empty".to_owned(),
        ));
    }
    if program.trim().is_empty() {
        return Err(HarnessConfigError::Invalid(format!(
            "provider {id:?} program must not be empty"
        )));
    }
    if request_timeout_ms == 0 || shutdown_timeout_ms == 0 {
        return Err(HarnessConfigError::Invalid(format!(
            "provider {id:?} request/shutdown timeouts must be greater than zero"
        )));
    }
    if max_stdout_line_bytes == 0 || stderr_history_lines == 0 {
        return Err(HarnessConfigError::Invalid(format!(
            "provider {id:?} stdout/stderr limits must be greater than zero"
        )));
    }
    Ok(())
}

fn compile_tool_definition(
    profile_name: &str,
    tool: &ToolConfig,
) -> Result<ToolDefinition, HarnessConfigError> {
    let input_schema = schema_root_to_json(
        profile_name,
        tool,
        "input_schema",
        tool.input_schema.clone(),
    )?;
    let output_schema = tool
        .output_schema
        .clone()
        .map(|value| schema_root_to_json(profile_name, tool, "output_schema", value))
        .transpose()?;
    Ok(ToolDefinition {
        name: tool.name.clone(),
        version: tool.version.clone(),
        description: tool.description.clone(),
        input_schema,
        output_schema,
        parallel_safe: tool.parallel_safe,
        side_effect: tool.side_effect,
        default_timeout_ms: tool.timeout_ms,
    })
}

fn schema_root_to_json(
    profile_name: &str,
    tool: &ToolConfig,
    field: &'static str,
    value: toml::Value,
) -> Result<serde_json::Value, HarnessConfigError> {
    if let toml::Value::String(text) = value {
        return serde_json::from_str(text.as_str()).map_err(|error| {
            HarnessConfigError::SchemaJson {
                profile: profile_name.to_owned(),
                tool: tool.name.clone(),
                field,
                message: error.to_string(),
            }
        });
    }
    schema_to_json(profile_name, tool, field, value)
}

fn schema_to_json(
    profile_name: &str,
    tool: &ToolConfig,
    field: &'static str,
    value: toml::Value,
) -> Result<serde_json::Value, HarnessConfigError> {
    match value {
        toml::Value::String(value) => Ok(serde_json::Value::String(value)),
        toml::Value::Integer(value) => Ok(serde_json::Value::Number(value.into())),
        toml::Value::Float(value) => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| HarnessConfigError::SchemaNonFiniteFloat {
                profile: profile_name.to_owned(),
                tool: tool.name.clone(),
                field,
            }),
        toml::Value::Boolean(value) => Ok(serde_json::Value::Bool(value)),
        toml::Value::Datetime(_) => Err(HarnessConfigError::SchemaDatetime {
            profile: profile_name.to_owned(),
            tool: tool.name.clone(),
            field,
        }),
        toml::Value::Array(values) => values
            .into_iter()
            .map(|value| schema_to_json(profile_name, tool, field, value))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        toml::Value::Table(values) => values
            .into_iter()
            .map(|(key, value)| {
                schema_to_json(profile_name, tool, field, value).map(|value| (key, value))
            })
            .collect::<Result<serde_json::Map<String, serde_json::Value>, _>>()
            .map(serde_json::Value::Object),
    }
}

fn resolve_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn resolve_program(base_dir: &Path, program: &str) -> PathBuf {
    let path = PathBuf::from(program);
    if path.is_absolute() || path.components().count() > 1 {
        resolve_path(base_dir, &path)
    } else {
        path
    }
}
