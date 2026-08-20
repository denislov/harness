use std::{collections::BTreeMap, path::PathBuf};

use harness_types::SideEffectClass;
use serde::Deserialize;

pub const HARNESS_CONFIG_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize)]
pub struct HarnessConfig {
    pub schema_version: u16,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub observability: ObservabilityConfig,
    #[serde(default)]
    pub credentials: BTreeMap<String, CredentialConfig>,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default = "default_runtime_name")]
    pub name: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub default_profile: Option<String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            name: default_runtime_name(),
            data_dir: default_data_dir(),
            default_profile: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ObservabilityConfig {
    #[serde(default)]
    pub runtime_events_jsonl: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CredentialConfig {
    Env { variable: String },
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub credentials: BTreeMap<String, String>,
    #[serde(default = "default_provider_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "default_provider_shutdown_timeout_ms")]
    pub shutdown_timeout_ms: u64,
    #[serde(default = "default_max_stdout_line_bytes")]
    pub max_stdout_line_bytes: usize,
    #[serde(default = "default_stderr_history_lines")]
    pub stderr_history_lines: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProfileConfig {
    pub model: ModelConfig,
    #[serde(default)]
    pub tools: Vec<ToolConfig>,
    pub policy: PolicyConfig,
    #[serde(default = "default_max_automatic_tool_attempts")]
    pub max_automatic_tool_attempts: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default = "default_model_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolConfig {
    pub name: String,
    pub provider: String,
    pub version: String,
    pub description: String,
    #[serde(default = "default_input_schema")]
    pub input_schema: toml::Value,
    #[serde(default)]
    pub output_schema: Option<toml::Value>,
    #[serde(default)]
    pub parallel_safe: bool,
    pub side_effect: SideEffectClass,
    #[serde(default = "default_tool_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyConfig {
    AllowAll,
}

fn default_runtime_name() -> String {
    "harness-cli".to_owned()
}

fn default_data_dir() -> PathBuf {
    PathBuf::from(".harness")
}

const fn default_provider_request_timeout_ms() -> u64 {
    30_000
}

const fn default_provider_shutdown_timeout_ms() -> u64 {
    5_000
}

const fn default_max_stdout_line_bytes() -> usize {
    1024 * 1024
}

const fn default_stderr_history_lines() -> usize {
    128
}

const fn default_max_automatic_tool_attempts() -> u32 {
    2
}

const fn default_model_timeout_ms() -> u64 {
    120_000
}

const fn default_tool_timeout_ms() -> u64 {
    30_000
}

fn default_input_schema() -> toml::Value {
    let mut table = toml::map::Map::new();
    let _ = table.insert("type".to_owned(), toml::Value::String("object".to_owned()));
    toml::Value::Table(table)
}
