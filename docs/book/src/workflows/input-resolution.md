# Input Resolution

Node inputs are processed in two stages before being passed to the runtime for execution.

## Stage 1: Template Resolution

All input values are [minijinja](https://docs.rs/minijinja) template strings. They are rendered against upstream node data to produce concrete string values.

```json
{
  "recipient": "{{ email }}",
  "count": "{{ items | length }}",
  "greeting": "Hello {{ name | title }}!"
}
```

### Template Context

The context available to templates depends on the node's position in the graph:

**Single upstream node**: Context is the upstream node's `data` output directly.

```json
// Upstream output: { "email": "user@example.com", "name": "john" }
// Template: "{{ email }}" → "user@example.com"
// Template: "{{ name | upper }}" → "JOHN"
```

**Entry-point nodes (no incoming edges)**: The invocation payload acts as the single virtual upstream. Fields of the payload are accessed directly.

```json
// Invocation payload: { "msg": "hello", "count": 3 }
// Template: "{{ msg }}" → "hello"
// Template: "{{ count }}" → "3"
```

**Join nodes (multiple upstream)**: Context is keyed by upstream node IDs.

```json
// Template: "{{ fetch_user.email }}"
// Template: "{{ get_config.setting }}"
```

### Data Visibility

**Strict single-hop visibility**: each node only sees its immediate upstream node's `data` output (or the invocation payload, for entry-point nodes).

- No cross-branch data sharing without explicit join
- Future: `context` escape hatch for explicit cross-node data sharing

This keeps data flow predictable and prevents implicit coupling between distant nodes.

## Stage 2: Type Coercion

After template resolution, all values are strings. The engine parses them to typed JSON values using the component's input schema:

| Schema Type | Parsing |
|-------------|---------|
| `string` | Used as-is |
| `integer` | `str.parse::<i64>()` |
| `number` | `str.parse::<f64>()` |
| `boolean` | `"true"` / `"false"` (case-insensitive) |
| `null` | Empty string or `"null"` |
| `array` | JSON parse |
| `object` | JSON parse |

If a value doesn't match the expected type, a clear error is returned with the node ID and field name.

## Why Two Stages?

- **Separation of concerns**: template rendering and type parsing are independent, testable operations
- **All config values are strings**: simplifies serialization and UI editing
- **Schema-based coercion**: catches type errors early with actionable error messages
- **Familiar syntax**: Jinja2 is well-known, with filters, conditionals, and safe expression evaluation
