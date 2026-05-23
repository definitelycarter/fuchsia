# Resolution

Resolution transforms a `WorkflowDef` (config) into a `Workflow` (locked). This is handled by the `fuschia-resolver` crate.

## What Resolution Does

1. **Validates the graph** — checks for duplicate node IDs, invalid edges, and cycles (via DFS with coloring)
2. **Resolves component references** — looks up each component in the registry, verifies the requested task export exists
3. **Locks versions and digests** — captures the concrete name, version, and SHA-256 digest of each component
4. **Copies schemas** — extracts JSON Schema from the component manifest and embeds it in the locked workflow
5. **Resolves nested workflows** — recursively resolves Loop node sub-workflows

## Config vs Locked

| Concern | Config (`WorkflowDef`) | Locked (`Workflow`) |
|---------|----------------------|---------------------|
| Component reference | Name + optional version | Name + version + digest |
| Node storage | `Vec<NodeDef>` | `HashMap<String, Node>` |
| Edges | `Vec<Edge>` structs | `Vec<(String, String)>` tuples |
| Input schema | Not present | Copied from manifest |
| Graph structure | Not validated | Validated DAG |

## Cycle Detection

Uses depth-first search with three-color marking:

- **White** — not yet visited
- **Gray** — currently in the DFS stack (visiting descendants)
- **Black** — fully processed

If a gray node is encountered while visiting, a cycle exists. The algorithm runs from every unvisited node to handle disconnected components.

## Resolver Trait

```rust
#[async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, def: WorkflowDef) -> Result<Workflow, ResolveError>;
}
```

`StandardResolver` takes a `ComponentRegistry` and performs the full resolution pipeline.

## Error Types

```rust
pub enum ResolveError {
    ComponentNotFound { name: String },
    ComponentVersionNotFound { name: String, version: String },
    TaskNotFound { component: String, task_name: String },
    InvalidEdge { node_id: String },
    DuplicateNodeId { node_id: String },
    NoEntryPoints,
    CycleDetected,
    Registry(RegistryError),
}
```
