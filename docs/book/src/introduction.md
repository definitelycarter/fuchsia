# Fuschia

Fuschia is a workflow engine similar to [n8n](https://n8n.io), built on a pluggable runtime architecture. Workflow nodes can be backed by WebAssembly components, Lua scripts, or (in the future) JavaScript — each running in a sandboxed VM with explicitly defined capabilities.

## Goals

- **DAG-based workflows** — Workflows are directed acyclic graphs of nodes. Independent branches execute in parallel automatically.
- **Pluggable runtimes** — Nodes are executed by runtime backends (wasmtime, Lua, JS). The engine doesn't know or care which VM backs a node.
- **Sandboxed execution** — Each node runs in isolation with declared capabilities (HTTP hosts, filesystem paths, KV access). Runtimes enforce the sandbox.
- **Single host capability implementations** — KV, HTTP, logging, config are each implemented once. Runtimes wire thin bindings to the shared implementation.
- **Component registry** — Reusable components installed separately from workflows, referenced by name and version, similar to npm packages.

## How It Works

1. **Author** a workflow as a JSON config — nodes, edges, and input templates
2. **Resolve** the config into a locked workflow — validate the graph, resolve component references, lock versions and digests
3. **Invoke** — the orchestrator is called with a JSON payload; nodes with no incoming edges (entry points) receive the payload, and the engine walks the graph wave-by-wave, resolves inputs via minijinja templates, provisions nodes with a runtime, and collects outputs

## Project Status

Fuschia is under active development. The core pipeline (config → resolution → execution) works end-to-end with wasmtime. The runtime abstraction layer (to support Lua and JS backends) is the next major milestone.
