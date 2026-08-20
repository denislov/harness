use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HarnessConfigError {
    #[error("failed to resolve Harness config path {path}: {source}")]
    ResolvePath {
        path: PathBuf,
        #[source]
        source: Box<std::io::Error>,
    },

    #[error("failed to read Harness config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: Box<std::io::Error>,
    },

    #[error("failed to parse Harness config {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },

    #[error("invalid Harness config: {0}")]
    Invalid(String),

    #[error("provider id {value:?} cannot be represented by Harness: {message}")]
    InvalidProviderId { value: String, message: String },

    #[error("provider {0} is configured more than once")]
    DuplicateProvider(String),

    #[error("profile {profile:?} references provider {provider:?}, which is not configured")]
    UnknownProviderReference { profile: String, provider: String },

    #[error(
        "profile {profile:?} tool {tool:?} references provider {provider:?}, which is not configured"
    )]
    UnknownToolProviderReference {
        profile: String,
        tool: String,
        provider: String,
    },

    #[error("profile {profile:?} tool {tool:?} contains invalid JSON text in {field}: {message}")]
    SchemaJson {
        profile: String,
        tool: String,
        field: &'static str,
        message: String,
    },

    #[error("profile {profile:?} tool {tool:?} has an unsupported TOML datetime in {field}")]
    SchemaDatetime {
        profile: String,
        tool: String,
        field: &'static str,
    },

    #[error("profile {profile:?} tool {tool:?} has a non-finite float in {field}")]
    SchemaNonFiniteFloat {
        profile: String,
        tool: String,
        field: &'static str,
    },
}
