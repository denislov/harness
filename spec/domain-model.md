# Domain Model

**Status:** Draft v0.1

## 1. Identifier types

Rust Core MUST use distinct domain identifier types instead of untyped strings internally.

Minimum identifier vocabulary:

```rust
SessionId
AgentInstanceId
EventId
MessageId
RequestId
ToolCallId
InvocationId
IdempotencyKey
ProviderId
BlobId
```

The wire format for identifiers is a UTF-8 JSON string. Identifiers are opaque. Consumers MUST NOT infer ordering, creation time, tenancy, or security properties from the lexical form of an identifier.

Recommended human-readable prefixes are:

```text
ses_   session
agt_   live agent instance
evt_   durable event
msg_   message
req_   model request
call_  model-generated tool call
inv_   concrete tool invocation
idem_  stable logical idempotency key
prv_   provider
blob_  blob
```

Prefixes are a debugging convention, not a semantic contract.

## 2. Ordered counters

Core uses distinct numeric types for ordered positions:

```rust
EventSeq
TurnNo
StepNo
```

Wire values are non-negative JSON integers and MUST remain within the IEEE-754 safe integer range (`0..=9007199254740991`) for interoperability with JavaScript-based SDKs.

`EventSeq` is the authoritative event ordering mechanism. ID lexical ordering MUST NOT be used as a substitute.

## 3. Message

A provider-neutral message contains stable identity, role, source and ordered content blocks.

Conceptual Rust shape:

```rust
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub source: MessageSource,
    pub content: Vec<ContentBlock>,
}
```

### 3.1 Role

v0.1 defines:

```text
system
user
assistant
```

Tool results are represented as content blocks in model-visible messages rather than a separate top-level role contract.

### 3.2 MessageSource

A message MUST carry provenance. Minimum source kinds are:

```text
user
model
plugin
system
```

A model source SHOULD include the provider and model that produced the message.

Wire example:

```json
{
  "kind": "model",
  "provider": "deepseek-official",
  "model": "example-model"
}
```

Unknown source metadata MAY be carried in extension fields, but providers MUST NOT use source metadata as authorization evidence.

## 4. ContentBlock

v0.1 defines six block types:

```text
text
reasoning
image
tool-call
tool-result
blob
```

### 4.1 Text

```json
{
  "type": "text",
  "text": "hello"
}
```

### 4.2 Reasoning

```json
{
  "type": "reasoning",
  "text": "provider-neutral reasoning content"
}
```

Reasoning is semantically distinct from visible answer text. Presentation behavior is outside this specification.

### 4.3 Image

```json
{
  "type": "image",
  "blob": {
    "id": "blob_123",
    "sha256": "...",
    "size": 12345,
    "mediaType": "image/png"
  }
}
```

### 4.4 Tool call

```json
{
  "type": "tool-call",
  "id": "call_123",
  "name": "read_file",
  "argumentsJson": "{\"path\":\"README.md\"}"
}
```

`argumentsJson` MUST be a JSON text string containing one complete JSON value. The Harness preserves the raw JSON representation across the model/provider-neutral boundary. Schema validation occurs before tool execution.

### 4.5 Tool result

```json
{
  "type": "tool-result",
  "toolCallId": "call_123",
  "content": [
    {"type": "text", "text": "..."}
  ],
  "isError": false
}
```

The provider-neutral message representation is intentionally simpler than Core's internal `ToolOutcome`; Core maps richer outcomes to model-visible content according to policy.

### 4.6 Blob block

```json
{
  "type": "blob",
  "blob": {
    "id": "blob_123",
    "sha256": "...",
    "size": 50000000,
    "mediaType": "application/octet-stream"
  }
}
```

## 5. BlobRef

Large or binary data MUST be referenced rather than embedded in SessionEvents or Provider Protocol frames.

Conceptual shape:

```rust
pub struct BlobRef {
    pub id: BlobId,
    pub sha256: String,
    pub size: u64,
    pub media_type: Option<String>,
}
```

Requirements:

- `sha256` is lowercase hexadecimal SHA-256 of the stored bytes.
- Blob identity and content digest MAY be the same underlying value but are separate semantic fields.
- A BlobRef MUST NOT be considered trusted solely because it appears in a provider response; Core validates storage ownership and access.
- SessionEvents SHOULD store BlobRefs instead of large inline payloads.

## 6. Time

Wire timestamps MUST use RFC 3339 strings in UTC, normally rendered with `Z`.

Example:

```text
2026-08-19T13:00:00Z
```

Durable ordering MUST use `EventSeq`, not timestamp comparison.

## 7. JSON naming

Provider Protocol and durable JSON examples use lower camel case for object fields.

Rust implementation field names MAY use snake_case with explicit serialization attributes.

Stable string discriminators use lowercase words and hyphens where needed, for example:

```text
read-only
idempotent-write
non-idempotent-write
next-turn
next-step
```
