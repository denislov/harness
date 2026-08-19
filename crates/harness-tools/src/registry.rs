use std::{collections::BTreeMap, sync::Arc};

use thiserror::Error;

use crate::{
    IdempotencySupport, ToolArgumentValidator, ToolDefinition, ToolDefinitionError, ToolExecutor,
};
use harness_types::SideEffectClass;

#[derive(Clone)]
pub struct ToolRegistration {
    definition: ToolDefinition,
    executor: Arc<dyn ToolExecutor>,
    validator: Arc<dyn ToolArgumentValidator>,
}

impl ToolRegistration {
    pub fn new(
        definition: ToolDefinition,
        executor: Arc<dyn ToolExecutor>,
        validator: Arc<dyn ToolArgumentValidator>,
    ) -> Result<Self, ToolRegistryError> {
        definition
            .validate()
            .map_err(ToolRegistryError::InvalidDefinition)?;
        if definition.side_effect == SideEffectClass::IdempotentWrite
            && executor.idempotency_support() != IdempotencySupport::Keyed
        {
            return Err(ToolRegistryError::IdempotencyCapabilityMismatch(
                definition.name.clone(),
            ));
        }
        Ok(Self {
            definition,
            executor,
            validator,
        })
    }

    pub fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    pub fn executor(&self) -> &Arc<dyn ToolExecutor> {
        &self.executor
    }

    pub fn validator(&self) -> &Arc<dyn ToolArgumentValidator> {
        &self.validator
    }
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    entries: BTreeMap<String, ToolRegistration>,
}

impl ToolRegistry {
    pub fn new(
        registrations: impl IntoIterator<Item = ToolRegistration>,
    ) -> Result<Self, ToolRegistryError> {
        let mut entries = BTreeMap::new();
        for registration in registrations {
            let name = registration.definition().name.clone();
            if entries.insert(name.clone(), registration).is_some() {
                return Err(ToolRegistryError::DuplicateToolName(name));
            }
        }
        Ok(Self { entries })
    }

    pub fn resolve(&self, name: &str) -> Option<&ToolRegistration> {
        self.entries.get(name)
    }

    pub fn definitions(&self) -> impl ExactSizeIterator<Item = &ToolDefinition> {
        self.entries.values().map(ToolRegistration::definition)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ToolRegistryError {
    #[error(transparent)]
    InvalidDefinition(#[from] ToolDefinitionError),

    #[error("tool {0} is registered more than once")]
    DuplicateToolName(String),

    #[error("idempotent-write tool {0} requires an executor with keyed idempotency support")]
    IdempotencyCapabilityMismatch(String),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use harness_types::{JsonText, PortableError, ProviderId, SideEffectClass, ToolOutcome};

    use super::*;
    use crate::{ToolArgumentValidationError, ToolExecutionFuture, ToolInvocation};

    struct NoopValidator;

    impl ToolArgumentValidator for NoopValidator {
        fn validate(
            &self,
            _definition: &ToolDefinition,
            _arguments_json: &JsonText,
        ) -> Result<(), ToolArgumentValidationError> {
            Ok(())
        }
    }

    struct Executor {
        provider_id: ProviderId,
        idempotency: IdempotencySupport,
    }

    impl ToolExecutor for Executor {
        fn provider_id(&self) -> &ProviderId {
            &self.provider_id
        }

        fn idempotency_support(&self) -> IdempotencySupport {
            self.idempotency
        }

        fn invoke(&self, _invocation: ToolInvocation) -> ToolExecutionFuture {
            Box::pin(async {
                Ok::<ToolOutcome, PortableError>(ToolOutcome::Success {
                    content: Vec::new(),
                })
            })
        }
    }

    fn definition(name: &str, side_effect: SideEffectClass) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            version: "1".to_owned(),
            description: "test tool".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            parallel_safe: true,
            side_effect,
            default_timeout_ms: 1_000,
        }
    }

    #[test]
    fn idempotent_write_requires_keyed_executor_support() {
        let executor = Arc::new(Executor {
            provider_id: ProviderId::new("prv_test").unwrap(),
            idempotency: IdempotencySupport::None,
        });
        let result = ToolRegistration::new(
            definition("write", SideEffectClass::IdempotentWrite),
            executor,
            Arc::new(NoopValidator),
        );
        assert!(matches!(
            result,
            Err(ToolRegistryError::IdempotencyCapabilityMismatch(name)) if name == "write"
        ));
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let executor = Arc::new(Executor {
            provider_id: ProviderId::new("prv_test").unwrap(),
            idempotency: IdempotencySupport::None,
        });
        let first = ToolRegistration::new(
            definition("read", SideEffectClass::ReadOnly),
            executor.clone(),
            Arc::new(NoopValidator),
        )
        .unwrap();
        let second = ToolRegistration::new(
            definition("read", SideEffectClass::ReadOnly),
            executor,
            Arc::new(NoopValidator),
        )
        .unwrap();

        assert!(matches!(
            ToolRegistry::new(vec![first, second]),
            Err(ToolRegistryError::DuplicateToolName(name)) if name == "read"
        ));
    }
}
