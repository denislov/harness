//! Application-level TOML configuration for composing a Harness runtime.
//!
//! This crate is intentionally above the Core state machine. Configuration is
//! composition input; durable SessionEvent history remains the execution source
//! of truth.

mod credentials;
mod error;
mod loader;
mod model;
mod plan;
mod scope;

pub use credentials::EnvironmentCredentialResolver;
pub use error::HarnessConfigError;
pub use loader::LoadedHarnessConfig;
pub use model::{
    CapabilityScopeConfig, CredentialConfig, HARNESS_CONFIG_SCHEMA_VERSION, HarnessConfig,
    ModelConfig, ObservabilityConfig, PolicyConfig, ProfileConfig, PromptMode, ProviderConfig,
    RuntimeConfig, ScopeConfig, ScopeModelConfig, SessionScopeConfig, ToolConfig,
};
pub use plan::RuntimePlan;
pub use scope::{
    PromptFragmentTrace, ResolvedModelTrace, ResolvedScope, ScopeResolutionTrace, ScopeSelection,
};

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn temp_config(contents: &str) -> std::path::PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harness-config-test-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("harness.toml");
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn compiles_relative_paths_and_profile_bindings() {
        let path = temp_config(
            r#"
schema_version = 1

[runtime]
data_dir = ".state"
default_profile = "default"

[[providers]]
id = "example-python"
program = "python3"
args = ["providers/example-python/provider.py"]

[profiles.default]
policy = "allow-all"

[profiles.default.model]
provider = "example-python"
model = "agent-model"
max_output_tokens = 128

[[profiles.default.tools]]
name = "echo"
provider = "example-python"
version = "1"
description = "Echo"
parallel_safe = true
side_effect = "read-only"
input_schema = { type = "object", properties = { text = { type = "string" } } }
"#,
        );
        let loaded = LoadedHarnessConfig::load(&path).unwrap();
        let plan = loaded.compile().unwrap();
        assert_eq!(plan.provider_count(), 1);
        assert_eq!(plan.profile_count(), 1);
        assert_eq!(plan.default_profile(), Some("default"));
        let expected_data_dir = loaded.base_dir().join(".state");
        assert_eq!(plan.data_dir(), expected_data_dir.as_path());
    }

    #[test]
    fn accepts_json_text_schema_when_toml_cannot_express_json_null() {
        let path = temp_config(
            r#"
schema_version = 1

[[providers]]
id = "provider"
program = "provider"

[profiles.default]
policy = "allow-all"

[profiles.default.model]
provider = "provider"
model = "model"

[[profiles.default.tools]]
name = "tool"
provider = "provider"
version = "1"
description = "tool"
side_effect = "read-only"
input_schema = '''{"type":"object","examples":[null]}'''
"#,
        );
        let plan = LoadedHarnessConfig::load(path).unwrap().compile().unwrap();
        assert_eq!(plan.profile_count(), 1);
    }

    #[test]
    fn static_compile_does_not_start_or_require_provider_process_or_credentials() {
        let path = temp_config(
            r#"
schema_version = 1

[observability]
runtime_events_jsonl = "logs/runtime-events.jsonl"

[credentials.token]
source = "env"
variable = "HARNESS_CONFIG_TEST_NOT_REQUIRED_DURING_COMPILE"

[[providers]]
id = "offline-provider"
program = "./definitely-not-present"
credentials = { TOKEN = "token" }
"#,
        );
        let loaded = LoadedHarnessConfig::load(path).unwrap();
        let plan = loaded.compile().unwrap();
        assert_eq!(plan.provider_count(), 1);
        assert_eq!(plan.credential_count(), 1);
        let expected_events = loaded.base_dir().join("logs/runtime-events.jsonl");
        assert_eq!(plan.runtime_events_jsonl(), Some(expected_events.as_path()));
    }

    #[test]
    fn rejects_unknown_provider_credential_reference() {
        let path = temp_config(
            r#"
schema_version = 1

[[providers]]
id = "provider"
program = "provider"
credentials = { TOKEN = "missing" }
"#,
        );
        let error = match LoadedHarnessConfig::load(path).unwrap().compile() {
            Ok(_) => panic!("config unexpectedly compiled"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            HarnessConfigError::UnknownCredentialReference { .. }
        ));
    }

    #[test]
    fn rejects_unknown_profile_provider() {
        let path = temp_config(
            r#"
schema_version = 1

[profiles.default]
policy = "allow-all"

[profiles.default.model]
provider = "missing"
model = "model"
"#,
        );
        let error = match LoadedHarnessConfig::load(path).unwrap().compile() {
            Ok(_) => panic!("config unexpectedly compiled"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            HarnessConfigError::UnknownProviderReference { .. }
        ));
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let path = temp_config(
            r#"
schema_version = 2
"#,
        );
        let error = match LoadedHarnessConfig::load(path).unwrap().compile() {
            Ok(_) => panic!("config unexpectedly compiled"),
            Err(error) => error,
        };
        assert!(matches!(error, HarnessConfigError::Invalid(_)));
    }

    #[test]
    fn rejects_default_profile_that_is_not_defined() {
        let path = temp_config(
            r#"
schema_version = 1

[runtime]
default_profile = "missing"
"#,
        );
        let error = match LoadedHarnessConfig::load(path).unwrap().compile() {
            Ok(_) => panic!("config unexpectedly compiled"),
            Err(error) => error,
        };
        assert!(matches!(error, HarnessConfigError::Invalid(_)));
    }

    #[test]
    fn resolves_global_workspace_profile_and_session_layers() {
        let path = temp_config(
            r#"
schema_version = 1

[runtime]
default_profile = "default"
default_workspace = "repo"

[[providers]]
id = "provider"
program = "provider"

[global]
system = "global prompt"

[global.model]
timeout_ms = 9000

[global.capabilities]
disable = ["echo"]

[workspaces.repo]
system = "workspace prompt"

[workspaces.repo.model]
timeout_ms = 7000

[workspaces.repo.capabilities]
enable = ["echo"]

[profiles.default]
policy = "allow-all"

[profiles.default.model]
provider = "provider"
model = "model"
system = "profile prompt"

[[profiles.default.tools]]
name = "echo"
provider = "provider"
version = "1"
description = "Echo"
side_effect = "read-only"
enabled = false

[sessions.ses_scope_test]
profile = "default"
workspace = "repo"
system = "session prompt"

[sessions.ses_scope_test.model]
max_output_tokens = 64

[sessions.ses_scope_test.capabilities]
enable = ["echo"]
"#,
        );
        let plan = LoadedHarnessConfig::load(path).unwrap().compile().unwrap();
        let session_id = harness_types::SessionId::new("ses_scope_test").unwrap();
        let resolved = plan
            .resolve_scope(
                ScopeSelection::new("default")
                    .with_workspace("repo")
                    .with_session(session_id),
            )
            .unwrap();
        let trace = resolved.trace();
        assert_eq!(
            trace.layers,
            vec![
                "global".to_owned(),
                "workspace:repo".to_owned(),
                "profile:default".to_owned(),
                "session:ses_scope_test".to_owned(),
            ]
        );
        assert_eq!(
            trace.system_prompt.as_deref(),
            Some("global prompt\n\nworkspace prompt\n\nprofile prompt\n\nsession prompt")
        );
        assert_eq!(trace.model.timeout_ms, 7000);
        assert_eq!(trace.model.max_output_tokens, Some(64));
        assert_eq!(trace.enabled_tools, vec!["echo".to_owned()]);
        assert!(trace.disabled_tools.is_empty());
        assert_eq!(
            plan.session_profile(resolved.session_id().unwrap()),
            Some("default")
        );
        assert_eq!(
            plan.session_workspace(resolved.session_id().unwrap()),
            Some("repo")
        );
    }

    #[test]
    fn workspace_prompt_can_replace_earlier_prompt_fragments() {
        let path = temp_config(
            r#"
schema_version = 1

[[providers]]
id = "provider"
program = "provider"

[global]
system = "global prompt"

[workspaces.repo]
system = "workspace prompt"
system_mode = "replace"

[profiles.default]
policy = "allow-all"

[profiles.default.model]
provider = "provider"
model = "model"
system = "profile prompt"
"#,
        );
        let plan = LoadedHarnessConfig::load(path).unwrap().compile().unwrap();
        let resolved = plan
            .resolve_scope(ScopeSelection::new("default").with_workspace("repo"))
            .unwrap();
        assert_eq!(
            resolved.trace().system_prompt.as_deref(),
            Some("workspace prompt\n\nprofile prompt")
        );
    }

    #[test]
    fn session_scope_binding_rejects_conflicting_profile() {
        let path = temp_config(
            r#"
schema_version = 1

[[providers]]
id = "provider"
program = "provider"

[profiles.default]
policy = "allow-all"
[profiles.default.model]
provider = "provider"
model = "model"

[profiles.other]
policy = "allow-all"
[profiles.other.model]
provider = "provider"
model = "model"

[sessions.ses_bound]
profile = "default"
"#,
        );
        let plan = LoadedHarnessConfig::load(path).unwrap().compile().unwrap();
        let error = plan
            .resolve_scope(
                ScopeSelection::new("other")
                    .with_session(harness_types::SessionId::new("ses_bound").unwrap()),
            )
            .err()
            .expect("conflicting session binding should fail");
        assert!(matches!(error, HarnessConfigError::Invalid(_)));
    }
}
