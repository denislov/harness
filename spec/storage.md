# Storage Specification

**Status:** Draft v0.1

## 1. Storage roles

v0.1 defines two storage abstractions:

```text
SessionStore
BlobStore
```

SessionStore owns durable ordered domain events. BlobStore owns large or binary immutable payloads referenced by durable events.

## 2. SessionStore semantic API

The exact Rust trait signature may evolve, but the semantic operations are fixed:

```text
create(sessionId, metadata)
append(sessionId, expectedSeq, events) -> newSeq
read(sessionId, fromSeq, limit)
head(sessionId)
fork(sourceSessionId, throughSeq, targetSessionId)
```

## 3. Create

`create` establishes an empty or initial event log for a unique SessionId.

Creating an existing SessionId MUST fail with `CONFLICT`.

A successful creation results in a durable `session/created` event.

## 4. Append and expected sequence

`append` is conditional on the caller's expected head sequence.

Example:

```text
committed head = 100
caller expectedSeq = 100
append events -> success, new head 103
```

If another writer has already advanced the log:

```text
committed head = 101
caller expectedSeq = 100
```

SessionStore MUST reject with `CONFLICT` and MUST NOT partially append the caller's batch.

Append batches MUST be atomic with respect to event visibility.

## 5. Read

`read` returns committed events in strict ascending EventSeq order.

Backends MUST NOT expose partially committed batches.

## 6. Head

`head` returns at least the current highest EventSeq and enough metadata to detect missing/corrupt sessions.

## 7. Fork

`fork(source, throughSeq, target)` creates a new durable session whose initial history is equivalent to the source session through the specified committed boundary.

The physical storage strategy may be:

- copied events;
- shared immutable segments;
- lineage reference plus projection;
- database-native snapshot.

Logical behavior must be equivalent.

Fork MUST NOT include source events after `throughSeq`.

## 8. Corruption handling

SessionStore MUST fail loudly on detected structural corruption, including:

- duplicate EventSeq;
- invalid gap according to backend's gap-free guarantee;
- malformed event envelope;
- impossible session identity mismatch;
- checksum failure when checksums are implemented.

Core surfaces durable structural corruption as `SESSION_CORRUPT` and MUST NOT continue normal Agent execution for that session.

## 9. BlobStore semantic API

Minimum operations:

```text
put(bytes, mediaType?) -> BlobRef
get(BlobId) -> bytes/stream
verify(BlobRef)
```

Additional operations such as delete, retention, leases, and remote URLs are implementation details outside v0.1.

## 10. Blob immutability

A BlobRef used by a committed SessionEvent MUST refer to immutable bytes.

Replacing bytes in place under the same committed BlobRef is forbidden.

## 11. Blob integrity

BlobStore verifies SHA-256 when writing or reading according to backend policy. A digest mismatch is a storage integrity failure.

## 12. Request snapshot durability

Before Core commits `model/requested`, the associated serialized ModelRequest snapshot MUST have been successfully persisted in BlobStore.

The event and blob need not be stored by the same backend, but Core must order operations so a committed event never intentionally references a blob that was never successfully persisted.

## 13. Initial local backend

The reference MVP SHOULD provide:

- an in-memory SessionStore for deterministic tests;
- a local durable SessionStore using SQLite or an equivalent embedded transactional database;
- a filesystem content-addressed BlobStore.

The specification does not require a specific production storage engine.
