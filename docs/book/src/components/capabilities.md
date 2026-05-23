# Component Capabilities

Components declare the capabilities they need in their manifest. The engine enforces these at runtime through the host capability implementations.

## Capability Declaration

```json
{
  "capabilities": {
    "allowed_hosts": ["api.google.com", "*.googleapis.com"],
    "allowed_paths": ["/tmp", "/data"]
  }
}
```

## Available Capabilities

### Key-Value Store

Execution-scoped key-value storage. Each workflow execution gets isolated state — components cannot see other executions' data.

```wit
interface kv {
    get: func(key: string) -> option<string>;
    set: func(key: string, value: string);
    delete: func(key: string);
}
```

Use cases: caching intermediate results, sharing state between nodes within an execution.

### Configuration

Read-only configuration lookup. Values come from the workflow node definition.

```wit
interface config {
    get: func(key: string) -> option<string>;
}
```

### Logging

Structured logging routed to the host's tracing/OpenTelemetry infrastructure:

```wit
interface log {
    enum level { trace, debug, info, warn, error }
    log: func(level: level, message: string);
}
```

Log entries automatically include `execution_id` and `node_id` for correlation.

### HTTP (Planned)

Outbound HTTP requests filtered by `allowed_hosts` from the manifest. Requests to hosts not in the allowlist are rejected.

### Filesystem (Future)

File access filtered by `allowed_paths` from the manifest. Reads/writes outside the allowlist are rejected.

## Security Model

Capabilities are additive — components start with no access and must declare what they need. The engine:

1. Reads `capabilities` from the component manifest
2. Builds policy objects (`HttpPolicy`, `FsPolicy`)
3. Passes them to the runtime as part of the `Capabilities` bag
4. The runtime wires them into the VM's sandbox
5. Host capability implementations enforce the policies
