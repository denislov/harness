use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use harness_llm::ModelOptions;
use harness_runtime::{
    AgentProfile, HarnessRuntimeInfo, ModelBinding, ProviderProcessSpec, RuntimeToolBinding,
};
use harness_tools::{
    AllowAllToolPolicy, ToolArgumentValidationError, ToolArgumentValidator, ToolDefinition,
};
use harness_types::{JsonText, ProviderId};

use crate::{
    HARNESS_CONFIG_SCHEMA_VERSION, HarnessConfig, HarnessConfigError, PolicyConfig, RuntimePlan,
    ToolConfig,
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
}

fn compile_config(
    config: &HarnessConfig,
    base_dir: &Path,
) -> Result<RuntimePlan, HarnessConfigError> {
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

    let data_dir = resolve_path(base_dir, &config.runtime.data_dir);
    let mut runtime_info = HarnessRuntimeInfo::default();
    runtime_info.name = config.runtime.name.clone();

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
        providers.push(spec);
    }

    let mut profiles = Vec::with_capacity(config.profiles.len());
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
        if profile.model.timeout_ms == 0 {
            return Err(HarnessConfigError::Invalid(format!(
                "profile {profile_name:?} model.timeout_ms must be greater than zero"
            )));
        }
        if profile.model.max_output_tokens == Some(0) {
            return Err(HarnessConfigError::Invalid(format!(
                "profile {profile_name:?} model.max_output_tokens must be greater than zero"
            )));
        }
        if profile.max_automatic_tool_attempts == 0 {
            return Err(HarnessConfigError::Invalid(format!(
                "profile {profile_name:?} max_automatic_tool_attempts must be greater than zero"
            )));
        }
        let model_provider = provider_id_values
            .get(&profile.model.provider)
            .cloned()
            .ok_or_else(|| HarnessConfigError::UnknownProviderReference {
                profile: profile_name.clone(),
                provider: profile.model.provider.clone(),
            })?;

        let model = ModelBinding::new(model_provider, profile.model.model.clone())
            .with_options(ModelOptions {
                max_output_tokens: profile.model.max_output_tokens,
            })
            .with_timeout_ms(profile.model.timeout_ms);
        let model = if let Some(system) = &profile.model.system {
            model.with_system(system.clone())
        } else {
            model
        };

        let policy: Arc<dyn harness_tools::ToolPolicy> = match profile.policy {
            PolicyConfig::AllowAll => Arc::new(AllowAllToolPolicy),
        };
        let mut agent_profile = AgentProfile::new(model, policy)
            .with_max_automatic_tool_attempts(profile.max_automatic_tool_attempts);

        let mut tool_names = BTreeSet::new();
        for tool in &profile.tools {
            if !tool_names.insert(tool.name.clone()) {
                return Err(HarnessConfigError::Invalid(format!(
                    "profile {profile_name:?} configures tool {:?} more than once",
                    tool.name
                )));
            }
            let tool_provider =
                provider_id_values
                    .get(&tool.provider)
                    .cloned()
                    .ok_or_else(|| HarnessConfigError::UnknownToolProviderReference {
                        profile: profile_name.clone(),
                        tool: tool.name.clone(),
                        provider: tool.provider.clone(),
                    })?;
            let definition = compile_tool_definition(profile_name, tool)?;
            definition.validate().map_err(|error| {
                HarnessConfigError::Invalid(format!(
                    "profile {profile_name:?} tool {:?} is invalid: {error}",
                    tool.name
                ))
            })?;
            agent_profile = agent_profile.with_tool(RuntimeToolBinding::new(
                definition,
                tool_provider,
                Arc::new(ObjectArgumentValidator),
            ));
        }
        profiles.push((profile_name.clone(), agent_profile));
    }

    if let Some(default_profile) = &config.runtime.default_profile
        && !config.profiles.contains_key(default_profile)
    {
        return Err(HarnessConfigError::Invalid(format!(
            "runtime.default_profile {default_profile:?} is not defined in profiles"
        )));
    }

    Ok(RuntimePlan {
        runtime_info,
        data_dir,
        providers,
        profiles,
        default_profile: config.runtime.default_profile.clone(),
    })
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
