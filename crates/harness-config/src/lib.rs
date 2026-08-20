//! Application-level TOML configuration for composing a Harness runtime.
//!
//! This crate is intentionally above the Core state machine. Configuration is
//! composition input; durable SessionEvent history remains the execution source
//! of truth.

mod error;
mod loader;
mod model;
mod plan;

pub use error::HarnessConfigError;
pub use loader::LoadedHarnessConfig;
pub use model::{
    HARNESS_CONFIG_SCHEMA_VERSION, HarnessConfig, ModelConfig, PolicyConfig, ProfileConfig,
    ProviderConfig, RuntimeConfig, ToolConfig,
};
pub use plan::RuntimePlan;

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
    fn static_compile_does_not_start_or_require_provider_process() {
        let path = temp_config(
            r#"
schema_version = 1

[[providers]]
id = "offline-provider"
program = "./definitely-not-present"
"#,
        );
        let plan = LoadedHarnessConfig::load(path).unwrap().compile().unwrap();
        assert_eq!(plan.provider_count(), 1);
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
}
