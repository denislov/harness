# Batch 15 — Durable Local Storage

Status: normative v0.1 supplement.

## 1. Scope

Batch 15 adds the first process-restart-durable local storage composition without changing the Session, Agent, LLM, Tool, or Provider Protocol domain contracts.

The durable local layout is:

```text
<root>/
├── sessions.sqlite3
└── blobs/
    └── sha256/<prefix>/<digest>
```

`SessionStore` remains the durable event-log authority. `BlobStore` remains a separate immutable byte store.

## 2. SQLite SessionStore

`SqliteSessionStore` implements the existing `harness_session::SessionStore` contract.

### 2.1 Schema

Schema version 1 uses SQLite `PRAGMA user_version = 1` and two logical tables:

- `sessions(session_id, head_seq)`
- `session_events(session_id, seq, event_id, event_json)`

The database enforces uniqueness of `(session_id, seq)` and `(session_id, event_id)`.

Each committed `SessionEvent` is stored as its complete canonical JSON envelope in `event_json`. The indexed SQLite columns are checked against the decoded envelope when reading. A mismatch is `SESSION_CORRUPT` semantics, not a recoverable projection difference.

### 2.2 Append transaction

A mutating operation MUST execute inside an IMMEDIATE SQLite transaction.

For `append(sessionId, expectedSeq, events)`:

1. validate the existing committed prefix and read its durable `head_seq`;
2. if the valid head differs from `expectedSeq`, return `CONFLICT` without validating the proposed event batch;
3. validate event shape and Session-local event-id uniqueness;
4. construct the entire committed sequence in memory;
5. insert every committed event and advance `head_seq` in one transaction;
6. commit atomically.

No subset of one append batch may become visible.

### 2.3 Read integrity

`read`, `head`, and `fork` validate the persisted event log before returning authoritative state. Validation includes:

- Session id binding;
- contiguous sequence numbers starting at 1;
- Session-local EventId uniqueness;
- event envelope validation;
- first event is `session/created`;
- SQLite indexed `seq/event_id` agree with encoded event JSON;
- `sessions.head_seq` agrees with the final event.

### 2.4 Fork

`fork` preserves the v0.1 semantics already implemented by `MemorySessionStore`: event id, sequence, timestamp, turn/step, and payload are copied from the source prefix; only the event-envelope `sessionId` is rebound to the target Session.

The complete target Session is created atomically.

### 2.5 SQLite durability configuration

The reference local backend configures:

- WAL journal mode;
- `synchronous=FULL`;
- foreign keys enabled;
- five-second busy timeout.

Batch 15 does not define multi-machine leases or distributed Session ownership. SQLite serialization protects local database writers; the Agent-level single-writer/expected-seq invariant remains authoritative.

## 3. Filesystem BlobStore

`FilesystemBlobStore` implements the existing immutable `BlobStore` contract.

Blob ids use the same reference convention as the memory backend:

```text
blob_sha256_<64 lowercase hex characters>
```

Bytes are stored at:

```text
<root>/sha256/<first-two-hex>/<full-digest>
```

### 3.1 Commit protocol

A new blob write:

1. computes SHA-256 and BlobId;
2. creates a temporary file in the destination directory;
3. writes all bytes;
4. syncs the temporary file;
5. atomically hard-links the fully-written temporary inode to the content-addressed destination;
6. on Unix, syncs the containing directory.

An already-present destination is accepted only if its bytes exactly match the proposed content. The reference backend assumes a local filesystem that supports file hard links; unsupported publication is a backend error.

A crash before publication must not expose a partial committed blob.

### 3.2 Verification

`verify(BlobRef)` MUST compare both SHA-256 and byte length. A content mismatch is a Blob integrity error.

## 4. DurableLocalStorage composition

`DurableLocalStorage::open(root)` creates/opens:

- `SqliteSessionStore` at `<root>/sessions.sqlite3`;
- `FilesystemBlobStore` at `<root>/blobs`.

`HarnessRuntimeBuilder::durable_local(root, eventSource, idSource)` is a convenience composition only. It does not alter Provider/Profile configuration or runtime identity semantics.

Production `AgentEventSource` and `RuntimeIdSource` implementations remain responsible for collision-resistant identities across process restarts.

## 5. Runtime restart acceptance

The Batch 15 acceptance path is:

```text
Runtime A
  create Session
  run user -> Python LLM -> Tool -> Python LLM -> final
  close/shutdown
      ↓
process-local objects dropped
      ↓
Runtime B opens same durable root
  read previous Session events
  verify previous ModelRequest BlobRefs
  open same Session
  replay model-visible projection
  run another complete Turn
```

The second Runtime MUST observe the first Runtime's committed event log and immutable request snapshots without migration or repair.

## 6. Batch 14 Clippy correction

Batch 15 also corrects two composition-root representation issues discovered under Rust 1.96 Clippy with `-D warnings`:

- source-heavy `HarnessRuntimeBuildError` variants store their source errors behind `Box`, reducing the Result error representation;
- the eight-argument `HarnessRuntime::from_parts` constructor is replaced by one crate-private `HarnessRuntimeParts` value.

These changes do not alter runtime semantics.

## 7. Non-goals

Batch 15 does not add:

- distributed Session leases;
- database encryption;
- blob garbage collection;
- schema migration beyond version 1;
- remote/object storage;
- Provider restart supervision;
- CLI/config-file loading.

Those remain later skeleton work.
