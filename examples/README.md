# Examples

This directory contains example workflows and pre-built components for testing the fuchsia CLI.

## Directory Structure

```
examples/
├── README.md
├── simple-workflow.json      # A simple workflow with one task
└── components/               # Pre-built components for examples
    └── test-task--1.0.0/     # Format: {name}--{version}/
        ├── component.wasm
        └── manifest.json
```

## Running the Examples

### Run a workflow

```bash
echo '{"message": "hello world"}' | cargo run -- run workflow examples/simple-workflow.json --data-dir examples
```

### Run a single task

```bash
echo '{"message": "direct execution"}' | cargo run -- run task examples/simple-workflow.json --node process --data-dir examples
```

### Without piping (empty payload)

```bash
cargo run -- run workflow examples/simple-workflow.json --data-dir examples
```

## Rebuilding Components

The components in `examples/components/` are pre-built for convenience. To rebuild them:

```bash
cd test-components/test-task-component
cargo component build --release
cp target/wasm32-wasip1/release/test_task_component.wasm \
   ../../examples/components/test-task--1.0.0/component.wasm
```
