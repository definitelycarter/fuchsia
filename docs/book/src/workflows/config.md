# Definition & Provisioning

A workflow is a persisted directed graph of nodes. `fuchsia-workflow` owns the
stored shape (and Slate-backed CRUD); `fuchsia-provisioner` turns one into a
running graph on the engine. You can also skip the stored form and drive the
[engine directly](#provisioning-without-the-store).

## The stored shape

```rust
pub struct Workflow {
    pub id: WorkflowId,        // serialized as "_id" (a bson ObjectId)
    pub name: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct Node {
    pub id: NodeId,                  // unique within the workflow
    pub definition: NodeDefinition,  // what it does
}

pub enum NodeDefinition {            // tagged: { "type", "configuration" }
    Builtin(BuiltinConfig),          //   "builtin"
    Component(ComponentConfig),      //   "component"
}

pub struct BuiltinConfig   { pub name: String, pub env: BTreeMap<String,String>, pub settings: Document }
pub struct ComponentConfig { pub runtime: Runtime, pub component: String, pub settings: Document }
pub enum   Runtime         { Wasm, Lua }    // "wasm" | "lua"

pub struct Edge { pub from: NodeId, pub to: NodeId }
```

Every type derives `serde`, so a workflow is a JSON (or BSON) document.

- A **`Builtin`** node names a registered native actor (`passthrough`,
  `debounce`, …). Its `env` is host-curated; `settings` is the operator's opaque
  config (e.g. `{ "delay_ms": 500 }`).
- A **`Component`** node is a Wasm or Lua guest. `runtime` selects which guest
  runtime backs it; `component` identifies the artifact within that runtime's
  catalog; `settings` is the component's own config.
A workflow carries **no notion of what fires it.** Triggering is the consumer's
concern: detect the event (a sensor change, a webhook, a schedule) and
`engine.push` a message into whichever node should receive it. The engine — and
this definition — stay invocation-agnostic.

## JSON form

```json
{
  "_id": "665f1c2a4b3a4e0012ab34cd",
  "name": "Fridge temperature",
  "nodes": [
    {
      "id": "ingress",
      "definition": { "type": "builtin", "configuration": { "name": "passthrough" } }
    },
    {
      "id": "normalize",
      "definition": {
        "type": "component",
        "configuration": { "runtime": "lua", "component": "celsius-to-f", "settings": {} }
      }
    },
    {
      "id": "debounce",
      "definition": { "type": "builtin", "configuration": { "name": "debounce", "settings": { "delay_ms": 500 } } }
    }
  ],
  "edges": [
    { "from": "ingress",   "to": "normalize" },
    { "from": "normalize", "to": "debounce"  }
  ]
}
```

Creating one via the store takes only `{ name, nodes?, edges? }` (`NewWorkflow`);
the store assigns the id and timestamps.

## From definition to running graph

`Provisioner::register_workflow(&workflow)` translates the definition into engine
calls. The translation (`plan`) is pure and testable:

- **Group = the workflow's id.** Every node id is scoped into a global
  `ActorId` as `ActorId::scoped(workflow_id, node_id)`, so the same node name in
  two workflows is two independent actors. The group is the unit
  `unregister_workflow` tears down.
- **Builtin node →** type name = `name`; `ActorConfig { env, settings }` carried
  through verbatim.
- **Component node →** type name = the **runtime** (`"wasm"` or `"lua"`), and the
  `component` id is placed in `ActorConfig.env` under `COMPONENT_ENV_KEY`
  (`"component"`). This is why guest creators are registered **per runtime**, not
  per component: the runtime is the type, the component id is config the creator
  resolves from its catalog (see [Host Extensibility](../architecture/host-extensibility.md)).

So the engine needs creators registered for every builtin name the workflow uses,
plus `"wasm"` / `"lua"` for any component nodes:

```rust
let engine = Arc::new(Engine::new());
fuchsia_actor_builtins::register(/* … */);     // or engine.register("passthrough", …)
engine.register("lua", lua_creator).await;
engine.register("wasm", wasm_creator).await;

let provisioner = Provisioner::new(engine.clone());
provisioner.register_workflow(&workflow).await?;
// later: provisioner.unregister_workflow(&workflow.id).await?;
```

## Provisioning without the store

For ad-hoc graphs (tests, examples, a host that doesn't persist), call the engine
directly — the provisioner is just a translator over the same API:

```rust
let a = ActorId::scoped("demo", "in");
let b = ActorId::scoped("demo", "out");

engine.add_node(a.clone(), "passthrough", &ActorConfig::default(), ActorCapabilities::new()).await?;
engine.add_node(b.clone(), "wasm", &component_config("echo"), ActorCapabilities::new()).await?;
engine.add_edge(a.clone(), b.clone())?;

engine.push(&a, Message::json("reading", json!(42)))?;
```

`crates/fuchsia-examples` builds exactly this kind of graph across all three
actor flavors.

## Semantics

- **Node ids** are unique within a workflow; the group namespaces them globally.
- **Edges** reference existing node ids. Edges sharing a `from` are
  [fan-out](../architecture/engine.md#topology-semantics); edges sharing a `to`
  are merge (interleaved, not a synchronous join).
- **Routing is the graph.** An actor decides *what* to emit; the *set of edges*
  changes only by provisioning, never from inside `handle`.
- **No per-edge config.** Edges are pure `from`/`to`; filtering or transforming
  on an edge is done by inserting a dedicated actor between the endpoints.
