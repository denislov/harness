use std::{
    ffi::{OsStr, OsString},
    fmt,
};

use async_trait::async_trait;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CredentialKey(String);

impl CredentialKey {
    pub fn new(value: impl Into<String>) -> Result<Self, CredentialKeyError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CredentialKeyError);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CredentialKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("credential key must not be empty")]
pub struct CredentialKeyError;

/// Secret material returned by a [`CredentialResolver`].
///
/// `Debug` is intentionally redacted and this type is not serializable. Callers
/// must explicitly opt in to exposure when injecting the value into a capability
/// process environment.
pub struct SecretValue(OsString);

impl SecretValue {
    pub fn new(value: impl Into<OsString>) -> Self {
        Self(value.into())
    }

    pub fn expose_os_str(&self) -> &OsStr {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CredentialResolveError {
    #[error("credential {key} was not found")]
    NotFound { key: CredentialKey },

    #[error("credential resolver failed for {key}: {message}")]
    Backend { key: CredentialKey, message: String },
}

#[async_trait]
pub trait CredentialResolver: Send + Sync {
    async fn resolve(&self, key: &CredentialKey) -> Result<SecretValue, CredentialResolveError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RejectingCredentialResolver;

#[async_trait]
impl CredentialResolver for RejectingCredentialResolver {
    async fn resolve(&self, key: &CredentialKey) -> Result<SecretValue, CredentialResolveError> {
        Err(CredentialResolveError::NotFound { key: key.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_never_exposes_value() {
        let secret = SecretValue::new("super-secret-value");
        let debug = format!("{secret:?}");
        assert!(!debug.contains("super-secret-value"));
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn credential_key_rejects_blank_values() {
        assert!(CredentialKey::new("   ").is_err());
        assert_eq!(CredentialKey::new("api").unwrap().as_str(), "api");
    }
}
