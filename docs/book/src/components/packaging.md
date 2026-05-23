# Component Packaging

Components are distributed as packages and installed into a local registry.

## Package Format

Components are bundled as `.fcpkg` packages:

```
my-component-1.0.0.fcpkg
├── manifest.json
├── component.wasm    (or component.lua, component.js)
├── README.md (optional)
└── assets/ (optional)
    └── screenshot.png
```

## Installation Directory

Installed components live in a predictable directory structure:

```
~/.fuchsia/components/
├── my-org--http-fetch--1.0.0/
│   ├── manifest.json
│   ├── component.wasm
│   └── README.md
└── my-org--http-fetch--2.0.0/
    ├── manifest.json
    └── component.wasm
```

Directory naming: `{name}--{version}` with `/` in names replaced by `--`.

## Workflow References

Workflows reference components by name and optional version:

```json
{
  "node_id": "fetch_data",
  "type": "component",
  "component": "my-org/http-fetch",
  "version": "1.0.0",
  "task_name": "fetch"
}
```

If version is omitted, the latest installed version is used.

## Integrity

Each manifest includes a `digest` field (SHA-256 of the component binary). This allows verifying that the installed binary matches what was expected at resolution time.
