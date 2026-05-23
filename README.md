# Fuchsia [![#FF00FF](https://img.shields.io/badge/%23FF00FF-FF00FF)](https://www.color-hex.com/color/ff00ff)

A workflow engine built on WebAssembly components.

## Features

- **Multi-runtime** — Write components in WebAssembly or Lua. Same execution model, same host capabilities, your choice of language.
- **Sandboxed execution** — Components run in isolated VMs with no access to the host filesystem, network, or environment unless explicitly granted through capability policies.
- **Parallel scheduling** — Independent branches execute concurrently as tokio tasks. Parallelism emerges from graph structure automatically.
- **Manual invocation** — Workflows are invoked with a JSON payload that is fed to graph entry points. No background pollers or webhook listeners.
- **Host capabilities** — Components access KV storage, configuration, logging, and HTTP through a shared capability layer. No capability logic is duplicated across runtimes.
- **Input templating** — Node inputs use Jinja2-style templates (`{{ upstream.field | upper }}`) with type coercion from schemas.
- **Component registry** — Install, version, and resolve components from a local registry. Components declare their capabilities and exported tasks in a manifest.
- **Cancellation** — Workflow-level cancellation propagates to all running nodes via `CancellationToken`.

## Quick Start

### Run a workflow

```bash
echo '{"message": "hello world"}' | cargo run -- run workflow examples/simple-workflow.json --data-dir examples
```

### Run a single node

```bash
echo '{"message": "test"}' | cargo run -- run node examples/simple-workflow.json --node process --data-dir examples
```

### CLI Help

```bash
cargo run -- --help
cargo run -- run --help
cargo run -- run workflow --help
```

## Examples

See the [examples directory](./examples/README.md) for sample workflows and pre-built components.

## Development

### Build

```bash
cargo build
```

### Test

```bash
cargo test --workspace
```

### Build test components

```bash
cd test-components/test-task-component
cargo component build --release
```
