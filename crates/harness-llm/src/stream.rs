use harness_types::{ContentBlock, PortableError, StreamSeq, TokenUsage, ToolCallId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockType {
    Text,
    Reasoning,
    ToolCall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishReason {
    Completed,
    MaxTokens,
    Error,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FinishEvent {
    pub reason: FinishReason,
    pub failure: Option<PortableError>,
}

impl FinishEvent {
    pub const fn completed() -> Self {
        Self {
            reason: FinishReason::Completed,
            failure: None,
        }
    }

    pub const fn max_tokens() -> Self {
        Self {
            reason: FinishReason::MaxTokens,
            failure: None,
        }
    }

    pub fn error(failure: PortableError) -> Self {
        Self {
            reason: FinishReason::Error,
            failure: Some(failure),
        }
    }

    pub fn cancelled(failure: PortableError) -> Self {
        Self {
            reason: FinishReason::Cancelled,
            failure: Some(failure),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum StreamEvent {
    BlockStart {
        index: u32,
        block_type: BlockType,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    ReasoningDelta {
        index: u32,
        text: String,
    },
    ToolCallDelta {
        index: u32,
        call_id: ToolCallId,
        name: Option<String>,
        arguments_delta: String,
    },
    BlockEnd {
        index: u32,
        block: ContentBlock,
    },
    Usage(TokenUsage),
    Finish(FinishEvent),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SequencedStreamEvent {
    pub seq: StreamSeq,
    pub event: StreamEvent,
}

impl SequencedStreamEvent {
    pub const fn new(seq: StreamSeq, event: StreamEvent) -> Self {
        Self { seq, event }
    }
}
