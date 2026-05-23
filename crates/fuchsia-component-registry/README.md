# fuchsia-component-registry

Component registry system for managing installed Wasm components.

## Key Types

| Type | Description |
|------|-------------|
| `ComponentManifest` | Metadata for a component (name, version, description, digest, input_schema) |
| `ComponentRegistry` | Trait for component storage operations |
| `FsComponentRegistry` | Filesystem-based registry implementation |
| `InstalledComponent` | A resolved component with manifest and paths |

## Directory Layout

Components are stored in a predictable structure:

```
{root}/
├── my-org--sentiment-analysis--1.0.0/
│   ├── manifest.json
│   ├── component.wasm
│   ├── README.md (optional)
│   └── assets/ (optional)
└── my-org--sentiment-analysis--2.0.0/
    ├── manifest.json
    └── component.wasm
```

The directory name format is `{name}--{version}` where slashes in the name are replaced with `--`.

Examples:
- `my-org/processor` version `1.0.0` → `my-org--processor--1.0.0`
- `simple-component` version `2.1.0` → `simple-component--2.1.0`

## Registry Operations

```rust
#[async_trait]
pub trait ComponentRegistry: Send + Sync {
    /// Get a component by name, optionally at a specific version.
    /// If version is None, returns the latest installed version.
    async fn get(&self, name: &str, version: Option<&str>) 
        -> Result<Option<InstalledComponent>, RegistryError>;

    /// Install a component from a package directory.
    async fn install(&self, package_path: &PathBuf) 
        -> Result<InstalledComponent, RegistryError>;

    /// List all installed components.
    async fn list(&self) -> Result<Vec<ComponentManifest>, RegistryError>;

    /// Remove an installed component.
    async fn remove(&self, name: &str, version: &str) -> Result<(), RegistryError>;
}
```

## Manifest Format

```json
{
  "name": "my-org/sentiment-analysis",
  "version": "1.0.0",
  "description": "Analyzes text sentiment",
  "digest": "sha256:abc123...",
  "input_schema": {
    "type": "object",
    "properties": {
      "text": { "type": "string" }
    }
  }
}
```

The `digest` is the SHA-256 hash of the `component.wasm` file, used for integrity verification at runtime.
