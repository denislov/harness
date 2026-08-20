use std::{collections::BTreeMap, ffi::OsString};

use async_trait::async_trait;
use harness_runtime::{CredentialKey, CredentialResolveError, CredentialResolver, SecretValue};

#[derive(Clone, Debug, Default)]
pub struct EnvironmentCredentialResolver {
    variables: BTreeMap<CredentialKey, OsString>,
}

impl EnvironmentCredentialResolver {
    pub fn new(variables: BTreeMap<CredentialKey, OsString>) -> Self {
        Self { variables }
    }

    pub fn len(&self) -> usize {
        self.variables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.variables.is_empty()
    }
}

#[async_trait]
impl CredentialResolver for EnvironmentCredentialResolver {
    async fn resolve(&self, key: &CredentialKey) -> Result<SecretValue, CredentialResolveError> {
        let Some(variable) = self.variables.get(key) else {
            return Err(CredentialResolveError::NotFound { key: key.clone() });
        };
        let value = std::env::var_os(variable).ok_or_else(|| CredentialResolveError::Backend {
            key: key.clone(),
            message: format!(
                "environment variable {:?} is not set",
                variable.to_string_lossy()
            ),
        })?;
        Ok(SecretValue::new(value))
    }
}
