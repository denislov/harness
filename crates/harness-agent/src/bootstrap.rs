use harness_session::{
    ProjectionError, SessionHead, SessionProjection, SessionProjector, SessionStore,
    SessionStoreError,
};
use harness_types::{EventSeq, SessionId};
use thiserror::Error;

use crate::{RecoveryAnalysisError, RecoveryAnalyzer, ResumeDecision};

#[derive(Clone, Debug, PartialEq)]
pub struct AgentBootstrap {
    pub head: SessionHead,
    pub projection: SessionProjection,
    pub resume: ResumeDecision,
}

#[derive(Debug, Error)]
pub enum AgentBootstrapError {
    #[error(transparent)]
    Storage(#[from] SessionStoreError),

    #[error(transparent)]
    Projection(#[from] ProjectionError),

    #[error(transparent)]
    Recovery(#[from] RecoveryAnalysisError),

    #[error("bootstrap page size must be greater than zero")]
    InvalidPageSize,

    #[error(
        "session snapshot was incomplete: expected to read through seq {expected}, observed {observed:?}"
    )]
    IncompleteSnapshot {
        expected: EventSeq,
        observed: Option<EventSeq>,
    },

    #[error("session snapshot is not contiguous: expected seq {expected}, observed {observed}")]
    NonContiguousSnapshot {
        expected: EventSeq,
        observed: EventSeq,
    },
}

#[derive(Clone, Debug)]
pub struct AgentBootstrapper<P> {
    projector: P,
    page_size: usize,
}

impl<P> AgentBootstrapper<P> {
    pub const fn new(projector: P, page_size: usize) -> Self {
        Self {
            projector,
            page_size,
        }
    }

    pub const fn page_size(&self) -> usize {
        self.page_size
    }

    pub const fn projector(&self) -> &P {
        &self.projector
    }
}

impl<P> AgentBootstrapper<P>
where
    P: SessionProjector,
{
    /// Loads one point-in-time Session head, reconstructs its projection, and
    /// classifies the work required before normal Agent execution may continue.
    ///
    /// The initial `head()` call defines the snapshot boundary. If a concurrent
    /// writer appends after that boundary, later events are intentionally ignored;
    /// the future actor's expected-seq append will detect that ownership conflict.
    pub async fn load<S>(
        &self,
        store: &S,
        session_id: &SessionId,
    ) -> Result<AgentBootstrap, AgentBootstrapError>
    where
        S: SessionStore + ?Sized,
    {
        if self.page_size == 0 {
            return Err(AgentBootstrapError::InvalidPageSize);
        }

        let head = store.head(session_id).await?;
        let mut events = Vec::new();
        let mut from_seq = EventSeq::FIRST;

        while from_seq <= head.seq {
            let page = store.read(session_id, from_seq, self.page_size).await?;
            let mut accepted_any = false;

            for event in page {
                if event.seq() > head.seq {
                    break;
                }
                if event.seq() != from_seq {
                    return Err(AgentBootstrapError::NonContiguousSnapshot {
                        expected: from_seq,
                        observed: event.seq(),
                    });
                }
                accepted_any = true;
                let event_seq = event.seq();
                events.push(event);
                if event_seq == head.seq {
                    break;
                }
                from_seq = event_seq.checked_next().map_err(|_| {
                    AgentBootstrapError::IncompleteSnapshot {
                        expected: head.seq,
                        observed: Some(event_seq),
                    }
                })?;
            }

            if !accepted_any {
                return Err(AgentBootstrapError::IncompleteSnapshot {
                    expected: head.seq,
                    observed: events.last().map(|event| event.seq()),
                });
            }

            if events.last().is_some_and(|event| event.seq() == head.seq) {
                break;
            }
        }

        if events.last().map(|event| event.seq()) != Some(head.seq) {
            return Err(AgentBootstrapError::IncompleteSnapshot {
                expected: head.seq,
                observed: events.last().map(|event| event.seq()),
            });
        }

        let projection = self.projector.project(&events)?;
        let resume = RecoveryAnalyzer.analyze(&projection)?;
        Ok(AgentBootstrap {
            head,
            projection,
            resume,
        })
    }
}
