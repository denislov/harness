use std::collections::BTreeMap;

use harness_types::{
    ContentBlock, ErrorCode, JsonText, PortableError, StreamSeq, TokenUsage, ToolCallId,
};
use thiserror::Error;

use crate::{BlockType, FinishEvent, FinishReason, SequencedStreamEvent, StreamEvent};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum LlmStreamOutcome {
    Assistant {
        content: Vec<ContentBlock>,
        usage: Option<TokenUsage>,
        finish_reason: FinishReason,
    },
    Failure {
        failure: PortableError,
        finish_reason: FinishReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum StreamAssemblyError {
    #[error(
        "stream sequence must start at 1 and increase by exactly one; expected {expected}, got {actual}"
    )]
    InvalidSequence {
        expected: StreamSeq,
        actual: StreamSeq,
    },

    #[error("stream event was received after finish")]
    EventAfterFinish,

    #[error("block index {0} was started more than once")]
    DuplicateBlock(u32),

    #[error("block index {0} is not open")]
    MissingOpenBlock(u32),

    #[error("stream delta kind does not match block index {0}")]
    DeltaKindMismatch(u32),

    #[error("tool-call delta for block {index} changed call id from {expected} to {actual}")]
    ToolCallIdChanged {
        index: u32,
        expected: ToolCallId,
        actual: ToolCallId,
    },

    #[error("tool-call delta for block {index} changed name from {expected} to {actual}")]
    ToolCallNameChanged {
        index: u32,
        expected: String,
        actual: String,
    },

    #[error("tool-call block {0} ended without a call id")]
    MissingToolCallId(u32),

    #[error("tool-call block {0} ended without a name")]
    MissingToolCallName(u32),

    #[error("tool-call block {index} ended with invalid JSON arguments: {message}")]
    InvalidToolCallJson { index: u32, message: String },

    #[error("block-end content for index {0} does not match the normalized deltas")]
    BlockEndMismatch(u32),

    #[error("usage may be emitted at most once")]
    DuplicateUsage,

    #[error("usage may not be emitted while a content block is open")]
    UsageWhileBlockOpen,

    #[error("finish may not be emitted while a content block is open")]
    FinishWhileBlockOpen,

    #[error("finish reason {0:?} has an invalid failure payload")]
    InvalidFinishFailure(FinishReason),

    #[error("stream ended without exactly one finish event")]
    MissingFinish,

    #[error("stream sequence overflow")]
    SequenceOverflow,
}

#[derive(Clone, Debug)]
enum OpenBlock {
    Text(String),
    Reasoning(String),
    ToolCall {
        call_id: Option<ToolCallId>,
        name: Option<String>,
        arguments: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct LlmStreamAssembler {
    last_seq: Option<StreamSeq>,
    open: BTreeMap<u32, OpenBlock>,
    completed: BTreeMap<u32, ContentBlock>,
    usage: Option<TokenUsage>,
    finish: Option<FinishEvent>,
}

impl LlmStreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: SequencedStreamEvent) -> Result<(), StreamAssemblyError> {
        if self.finish.is_some() {
            return Err(StreamAssemblyError::EventAfterFinish);
        }

        let expected = match self.last_seq {
            Some(seq) => seq
                .checked_next()
                .map_err(|_| StreamAssemblyError::SequenceOverflow)?,
            None => StreamSeq::FIRST,
        };
        if item.seq != expected {
            return Err(StreamAssemblyError::InvalidSequence {
                expected,
                actual: item.seq,
            });
        }
        self.last_seq = Some(item.seq);

        match item.event {
            StreamEvent::BlockStart { index, block_type } => {
                if self.open.contains_key(&index) || self.completed.contains_key(&index) {
                    return Err(StreamAssemblyError::DuplicateBlock(index));
                }
                let block = match block_type {
                    BlockType::Text => OpenBlock::Text(String::new()),
                    BlockType::Reasoning => OpenBlock::Reasoning(String::new()),
                    BlockType::ToolCall => OpenBlock::ToolCall {
                        call_id: None,
                        name: None,
                        arguments: String::new(),
                    },
                };
                self.open.insert(index, block);
            }
            StreamEvent::TextDelta { index, text } => match self.open.get_mut(&index) {
                Some(OpenBlock::Text(buffer)) => buffer.push_str(&text),
                Some(_) => return Err(StreamAssemblyError::DeltaKindMismatch(index)),
                None => return Err(StreamAssemblyError::MissingOpenBlock(index)),
            },
            StreamEvent::ReasoningDelta { index, text } => match self.open.get_mut(&index) {
                Some(OpenBlock::Reasoning(buffer)) => buffer.push_str(&text),
                Some(_) => return Err(StreamAssemblyError::DeltaKindMismatch(index)),
                None => return Err(StreamAssemblyError::MissingOpenBlock(index)),
            },
            StreamEvent::ToolCallDelta {
                index,
                call_id,
                name,
                arguments_delta,
            } => match self.open.get_mut(&index) {
                Some(OpenBlock::ToolCall {
                    call_id: current_call_id,
                    name: current_name,
                    arguments,
                }) => {
                    if let Some(existing) = current_call_id {
                        if existing.as_str() != call_id.as_str() {
                            return Err(StreamAssemblyError::ToolCallIdChanged {
                                index,
                                expected: existing.clone(),
                                actual: call_id,
                            });
                        }
                    } else {
                        *current_call_id = Some(call_id);
                    }
                    if let Some(name) = name {
                        if let Some(existing) = current_name {
                            if existing.as_str() != name.as_str() {
                                return Err(StreamAssemblyError::ToolCallNameChanged {
                                    index,
                                    expected: existing.clone(),
                                    actual: name,
                                });
                            }
                        } else {
                            *current_name = Some(name);
                        }
                    }
                    arguments.push_str(&arguments_delta);
                }
                Some(_) => return Err(StreamAssemblyError::DeltaKindMismatch(index)),
                None => return Err(StreamAssemblyError::MissingOpenBlock(index)),
            },
            StreamEvent::BlockEnd { index, block } => {
                let open = self
                    .open
                    .remove(&index)
                    .ok_or(StreamAssemblyError::MissingOpenBlock(index))?;
                let expected = finalize_open_block(index, open)?;
                if expected != block {
                    return Err(StreamAssemblyError::BlockEndMismatch(index));
                }
                self.completed.insert(index, block);
            }
            StreamEvent::Usage(usage) => {
                if !self.open.is_empty() {
                    return Err(StreamAssemblyError::UsageWhileBlockOpen);
                }
                if self.usage.replace(usage).is_some() {
                    return Err(StreamAssemblyError::DuplicateUsage);
                }
            }
            StreamEvent::Finish(finish) => {
                if !self.open.is_empty() {
                    return Err(StreamAssemblyError::FinishWhileBlockOpen);
                }
                validate_finish(&finish)?;
                self.finish = Some(finish);
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<LlmStreamOutcome, StreamAssemblyError> {
        let finish = self.finish.ok_or(StreamAssemblyError::MissingFinish)?;
        match finish.reason {
            FinishReason::Completed | FinishReason::MaxTokens => Ok(LlmStreamOutcome::Assistant {
                content: self.completed.into_values().collect(),
                usage: self.usage,
                finish_reason: finish.reason,
            }),
            FinishReason::Error | FinishReason::Cancelled => Ok(LlmStreamOutcome::Failure {
                failure: finish
                    .failure
                    .expect("validated error/cancelled finish must contain failure"),
                finish_reason: finish.reason,
            }),
        }
    }
}

fn finalize_open_block(index: u32, open: OpenBlock) -> Result<ContentBlock, StreamAssemblyError> {
    match open {
        OpenBlock::Text(text) => Ok(ContentBlock::Text { text }),
        OpenBlock::Reasoning(text) => Ok(ContentBlock::Reasoning { text }),
        OpenBlock::ToolCall {
            call_id,
            name,
            arguments,
        } => {
            let call_id = call_id.ok_or(StreamAssemblyError::MissingToolCallId(index))?;
            let name = name.ok_or(StreamAssemblyError::MissingToolCallName(index))?;
            let arguments_json = JsonText::new(arguments).map_err(|error| {
                StreamAssemblyError::InvalidToolCallJson {
                    index,
                    message: error.to_string(),
                }
            })?;
            Ok(ContentBlock::ToolCall {
                id: call_id,
                name,
                arguments_json,
            })
        }
    }
}

fn validate_finish(finish: &FinishEvent) -> Result<(), StreamAssemblyError> {
    match (finish.reason, finish.failure.as_ref()) {
        (FinishReason::Completed | FinishReason::MaxTokens, None) => Ok(()),
        (FinishReason::Error, Some(failure)) if failure.code != ErrorCode::Cancelled => Ok(()),
        (FinishReason::Cancelled, Some(failure)) if failure.code == ErrorCode::Cancelled => Ok(()),
        _ => Err(StreamAssemblyError::InvalidFinishFailure(finish.reason)),
    }
}

#[cfg(test)]
mod tests {
    use harness_types::{ContentBlock, JsonText, StreamSeq, ToolCallId};

    use super::*;

    fn seq(value: u64) -> StreamSeq {
        StreamSeq::new(value).unwrap()
    }

    #[test]
    fn assembles_text_stream_and_requires_finish() {
        let mut assembler = LlmStreamAssembler::new();
        assembler
            .push(SequencedStreamEvent::new(
                seq(1),
                StreamEvent::BlockStart {
                    index: 0,
                    block_type: BlockType::Text,
                },
            ))
            .unwrap();
        assembler
            .push(SequencedStreamEvent::new(
                seq(2),
                StreamEvent::TextDelta {
                    index: 0,
                    text: "hel".to_owned(),
                },
            ))
            .unwrap();
        assembler
            .push(SequencedStreamEvent::new(
                seq(3),
                StreamEvent::TextDelta {
                    index: 0,
                    text: "lo".to_owned(),
                },
            ))
            .unwrap();
        assembler
            .push(SequencedStreamEvent::new(
                seq(4),
                StreamEvent::BlockEnd {
                    index: 0,
                    block: ContentBlock::text("hello"),
                },
            ))
            .unwrap();
        assembler
            .push(SequencedStreamEvent::new(
                seq(5),
                StreamEvent::Finish(FinishEvent::completed()),
            ))
            .unwrap();

        let outcome = assembler.finish().unwrap();
        assert!(matches!(
            outcome,
            LlmStreamOutcome::Assistant { content, .. }
                if content == vec![ContentBlock::text("hello")]
        ));
    }

    #[test]
    fn rejects_non_contiguous_stream_sequence() {
        let mut assembler = LlmStreamAssembler::new();
        let error = assembler
            .push(SequencedStreamEvent::new(
                seq(2),
                StreamEvent::Finish(FinishEvent::completed()),
            ))
            .unwrap_err();
        assert!(matches!(error, StreamAssemblyError::InvalidSequence { .. }));
    }

    #[test]
    fn cancelled_finish_requires_cancelled_error_code() {
        let mut assembler = LlmStreamAssembler::new();
        let error = assembler
            .push(SequencedStreamEvent::new(
                seq(1),
                StreamEvent::Finish(FinishEvent::cancelled(PortableError::new(
                    ErrorCode::Internal,
                    "wrong normalized code",
                ))),
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            StreamAssemblyError::InvalidFinishFailure(FinishReason::Cancelled)
        ));
    }

    #[test]
    fn validates_tool_call_delta_against_complete_block() {
        let call_id = ToolCallId::new("call_1").unwrap();
        let mut assembler = LlmStreamAssembler::new();
        assembler
            .push(SequencedStreamEvent::new(
                seq(1),
                StreamEvent::BlockStart {
                    index: 0,
                    block_type: BlockType::ToolCall,
                },
            ))
            .unwrap();
        assembler
            .push(SequencedStreamEvent::new(
                seq(2),
                StreamEvent::ToolCallDelta {
                    index: 0,
                    call_id: call_id.clone(),
                    name: Some("read_file".to_owned()),
                    arguments_delta: "{\"path\":".to_owned(),
                },
            ))
            .unwrap();
        assembler
            .push(SequencedStreamEvent::new(
                seq(3),
                StreamEvent::ToolCallDelta {
                    index: 0,
                    call_id: call_id.clone(),
                    name: None,
                    arguments_delta: "\"README.md\"}".to_owned(),
                },
            ))
            .unwrap();
        let block = ContentBlock::ToolCall {
            id: call_id,
            name: "read_file".to_owned(),
            arguments_json: JsonText::new(r#"{"path":"README.md"}"#.to_owned()).unwrap(),
        };
        assembler
            .push(SequencedStreamEvent::new(
                seq(4),
                StreamEvent::BlockEnd { index: 0, block },
            ))
            .unwrap();
        assembler
            .push(SequencedStreamEvent::new(
                seq(5),
                StreamEvent::Finish(FinishEvent::completed()),
            ))
            .unwrap();
        assert!(matches!(
            assembler.finish().unwrap(),
            LlmStreamOutcome::Assistant { .. }
        ));
    }
}
