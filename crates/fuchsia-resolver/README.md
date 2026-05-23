# fuchsia-resolver

Transforms `WorkflowDef` (config) into `Workflow` (locked, execution-ready).

## Key Types

| Type | Description |
|------|-------------|
| `Resolver` | Trait for workflow resolution |
| `StandardResolver<R>` | Default implementation, generic over `ComponentRegistry` |
| `ResolveError` | Error enum for validation and resolution failures |

## Resolution Process

```
WorkflowDef (fuchsia-config)
        │
        ▼
   ┌─────────────┐
   │  Resolver   │
   └─────────────┘
        │
        ▼
   Workflow (fuchsia-workflow)
```

The resolver:
1. Validates the graph structure
2. Resolves component references via the registry
3. Produces a locked workflow ready for execution

## Validation

The resolver performs several validation checks:

| Check | Error |
|-------|-------|
| Duplicate node IDs | `ResolveError::DuplicateNodeId` |
| Edge references non-existent node | `ResolveError::InvalidEdge` |
| Cycle detected in graph | `ResolveError::CycleDetected` |
| No entry points (all nodes have incoming edges) | `ResolveError::NoEntryPoints` |
| Component not found in registry | `ResolveError::ComponentNotFound` |
| Component version not found | `ResolveError::ComponentVersionNotFound` |

### Cycle Detection

Uses depth-first search with three-color marking:
- White (0): Unvisited
- Gray (1): In current DFS path
- Black (2): Finished

A back edge to a gray node indicates a cycle.

## Recursive Resolution

Loop nodes contain nested workflows. The resolver handles these recursively:

```rust
ConfigNodeType::Loop { nodes, edges, ... } => {
    let nested_workflow = self.resolve_inner(..., nodes, edges, ...).await?;
    NodeType::Loop(LockedLoop {
        workflow: Box::new(nested_workflow),
        ...
    })
}
```

Each nested workflow undergoes the same validation (DAG check, component resolution, etc.).

## Usage

```rust
use fuchsia_component_registry::FsComponentRegistry;
use fuchsia_resolver::{Resolver, StandardResolver};

let registry = FsComponentRegistry::new("~/.fuchsia/components");
let resolver = StandardResolver::new(registry);

let workflow_def: WorkflowDef = load_from_json("workflow.json");
let locked_workflow = resolver.resolve(workflow_def).await?;
```

## Testing

The resolver is designed for testability. Pass a mock `ComponentRegistry` to test resolution logic without filesystem access:

```rust
struct MockRegistry { ... }

impl ComponentRegistry for MockRegistry {
    async fn get(&self, name: &str, version: Option<&str>) 
        -> Result<Option<InstalledComponent>, RegistryError> {
        // Return pre-configured components
    }
}

let resolver = StandardResolver::new(MockRegistry::new());
```
