# Example Python Provider

This provider intentionally has **zero third-party dependencies**. It demonstrates the Provider Protocol v1 contract directly over stdin/stdout NDJSON.

It exposes:

- Tool capability `echo`
- LLM capability `echo-model`
- `provider.initialize`
- `provider.ping`
- `provider.shutdown`
- `tool.invoke`
- `llm.start` + `llm.event`
- `capability.cancel` notification acceptance

Protocol messages are written only to stdout. Diagnostics are written only to stderr.

Manual launch:

```bash
python3 providers/example-python/provider.py
```

A real provider should normally be launched by `harness-provider-host`, not by a terminal user.
