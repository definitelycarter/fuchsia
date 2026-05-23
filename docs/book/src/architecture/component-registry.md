# Component Registry

Components are installed separately from workflows and referenced by name, similar to npm packages. The registry manages component metadata, storage, and lookup.

## Component Manifest

Each installed component has a manifest describing its exports and capabilities:

```json
{
  "name": "my-org/google-sheets",
  "version": "1.0.0",
  "description": "Google Sheets integration",
  "digest": "sha256:abc123...",
  "capabilities": {
    "allowed_hosts": ["sheets.googleapis.com", "*.google.com"],
    "allowed_paths": []
  },
  "tasks": {
    "write-row": {
      "description": "Write a row to a spreadsheet",
      "schema": {
        "type": "object",
        "required": ["spreadsheet_id", "values"],
        "properties": {
          "spreadsheet_id": { "type": "string" },
          "values": { "type": "array" }
        }
      }
    },
    "read-range": {
      "description": "Read a range of cells",
      "schema": {
        "type": "object",
        "required": ["spreadsheet_id", "range"],
        "properties": {
          "spreadsheet_id": { "type": "string" },
          "range": { "type": "string" }
        }
      }
    }
  }
}
```

Components can export multiple tasks. A single component (e.g., "google-sheets") can provide both `write-row` and `read-range` tasks.

## Registry Trait

```rust
#[async_trait]
pub trait ComponentRegistry: Send + Sync {
    async fn get(&self, name: &str, version: Option<&str>)
        -> Result<Option<InstalledComponent>, RegistryError>;
    async fn install(&self, package_path: &Path)
        -> Result<InstalledComponent, RegistryError>;
    async fn list(&self) -> Result<Vec<ComponentManifest>, RegistryError>;
    async fn remove(&self, name: &str, version: &str) -> Result<(), RegistryError>;
}
```

If version is omitted, the latest installed version is returned.

## Filesystem Registry

Components are stored in a predictable directory structure:

```
~/.fuchsia/components/
├── my-org--google-sheets--1.0.0/
│   ├── manifest.json
│   ├── component.wasm
│   └── README.md
└── my-org--google-sheets--2.0.0/
    ├── manifest.json
    └── component.wasm
```

## Component Package Format

Components are distributed as `.fcpkg` packages:

```
my-component-1.0.0.fcpkg
├── manifest.json
├── component.wasm
├── README.md (optional)
└── assets/ (optional)
    └── screenshot.png
```

## Schema

The `input_schema` in the manifest uses JSON Schema. This enables:

- **Validation** — verify workflow inputs at resolve time
- **Dynamic UI** — generate form fields for node configuration
- **Type coercion** — the engine uses the schema to parse template-resolved strings into typed values
- **Documentation** — self-describing components

## Resolution

When a workflow is resolved (config → locked workflow):

1. Look up each component reference in the registry
2. Validate the component exists and the requested task export exists
3. Copy the input schema from the manifest into the locked workflow
4. Lock with concrete name, version, and digest

This allows sharing workflows without bundling components, updating components independently, and verifying integrity via digest.
