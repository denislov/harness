use serde::{Deserialize, Serialize};

use crate::{CancelCause, ContentBlock};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ToolOutcome {
    Success {
        content: Vec<ContentBlock>,
    },
    Error {
        code: String,
        message: String,
        #[serde(default)]
        content: Vec<ContentBlock>,
    },
    Denied {
        reason: String,
    },
    Cancelled {
        cause: CancelCause,
    },
    Unknown {
        reason: String,
    },
}
