# Security and Trust Boundaries

**Status:** Draft v0.1

## 1. Principle

The model and capability providers are not authorization authorities.

Harness Core owns policy decisions. Security is enforced through explicit boundaries rather than prompt instructions.

## 2. Logical security path

```text
Model-generated ToolCall
        |
        v
ToolRegistry resolution
        |
        v
argument validation
        |
        v
PolicyEngine
   +----+----+
   |         |
 deny       ask approval / allow
             |
             v
Provider boundary
             |
             v
OS/process/network sandbox where applicable
```

## 3. Model trust

Model output MUST be treated as untrusted input.

A system prompt saying that a tool should not perform an unsafe action is not a substitute for policy enforcement.

## 4. Provider trust

Providers are capability executors and may themselves be buggy or malicious.

Core SHOULD assume providers can:

- return malformed data;
- hang;
- crash;
- emit excessive output;
- misuse credentials made available to them;
- attempt filesystem/network actions beyond declared intent.

ProviderHost and deployment sandboxing SHOULD constrain these risks.

## 5. Provider permissions

A provider manifest or deployment configuration may declare required resources such as:

```text
network
filesystem
subprocess
credentials
gpu
```

A declaration is a request, not an authorization grant.

Deployment policy decides what resources the provider process receives.

## 6. Credentials

Credentials SHOULD be represented by Core-owned references rather than inserted directly into model-visible Tool arguments.

Preferred pattern:

```json
{
  "credentialRef": "cred:github/default"
}
```

Core CredentialResolver maps the reference to an actual secret only at the authorized invocation boundary.

Providers receive only credentials required for their resolved operation.

Secrets MUST NOT be copied into SessionEvents, ModelRequest snapshots, logs, or provider manifests unless the explicit product contract requires and protects such storage.

## 7. Message provenance

`MessageSource` records provenance for presentation and context semantics. It MUST NOT be used as sufficient proof of authorization or identity.

## 8. Tool policy

Policy decisions are:

```text
allow
deny
ask
```

Mandatory deny is monotonic. Later middleware cannot override it.

Approval is scoped to the exact invocation or policy-defined grant. A human approval UI is outside v0.1, but Core must retain ownership of the approval decision.

## 9. Side-effect classification

Every Tool declares a side-effect class. This is required for both safety and recovery.

A provider MUST NOT be allowed to downgrade a Core-resolved `non-idempotent-write` into a `read-only` declaration at invocation time.

## 10. Deadlines and resource limits

Core supplies operation deadlines. ProviderHost SHOULD enforce bounded execution and deployment-defined process limits.

Tool-specific OS sandboxing may be implemented by Rust or other providers, but policy ownership remains in Core.

## 11. Protocol isolation

For stdio providers:

- stdout is protocol-only;
- stderr is diagnostic logging;
- provider stdin is protocol input;
- provider process environment SHOULD contain the minimum required secrets;
- providers SHOULD run with least OS privilege practicable.

## 12. Blob access

A provider must not be able to fetch arbitrary BlobIds solely by guessing identifiers. Blob access is mediated by Core/provider-specific transport and scope authorization.

The exact blob transfer mechanism to out-of-process providers is deferred, but access-control ownership is fixed in Core.
