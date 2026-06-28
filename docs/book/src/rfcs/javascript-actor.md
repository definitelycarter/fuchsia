# RFC: JavaScript Actor (QuickJS)

> **Status: proposed.** Builds on the [async actor contract](./async-actor-contract.md).
> Tracked in the [roadmap](../reference/roadmap.md#features) Features table until it
> lands.

## Concept

Authors write an actor as a plain JavaScript **script** that runs dynamically in an
embedded JS engine — **no build or compile step.** A `fuchsia-actor-js` pack mirrors
the Lua pack exactly: an embedded QuickJS interpreter (via `rquickjs`, vendored like
`mlua`), a script catalog, and `JsActor<H: JsHost>` / `JsActorCreator<H>` /
`BaseJsHost` (registers only `emit`). The script's `handle(ctx, msg)` runs per
message; I/O such as `await fetch(url)` is an injected host capability, made
non-blocking by the [async actor contract](./async-actor-contract.md). For genuinely
untrusted / multi-tenant code, a compile-to-wasm path stays available as a hardened
alternative.

## Motivation

JavaScript is the lingua franca for an n8n-style product — its users write JS, its
"Code" node is JS, the ecosystem is JS. First-class JS makes the product approachable.
And authors want the **Lua experience**: drop in a script and it runs — no toolchain,
no AOT compile. The earlier "compile JS → wasm" design delivered a sandbox but forced
a build step; for the common case (author-written workflow code — the same trust tier
as the existing native Lua actor) the ergonomic dynamic interpreter is the better
default.

## Design (recommendation)

**A native embedded interpreter, parallel to the Lua pack.** `fuchsia-actor-js`:

- Embeds **QuickJS via `rquickjs`** (vendored — compiles QuickJS in, exactly as the
  Lua pack vendors Lua through `mlua`).
- `JsActorCreator<H>` is the one creator for the `"js"` runtime, holding a **script
  catalog** of source strings; the script id rides in `ActorConfig.env` under
  `COMPONENT_ENV_KEY`, exactly as the wasm/lua creators resolve their artifact.
- `JsActor<H: JsHost>` holds a QuickJS context. `setup` evaluates the script **once**
  (defining its handler); `handle` calls that handler with the message; `teardown`
  runs its cleanup. With the persistent-actor model
  ([runs & result correlation](./runs-and-results.md)), the script is eval'd once per
  *deployed node* and reused across all runs — no per-message or per-request eval
  cost.
- `BaseJsHost` registers only `emit` (the contract). Products add capabilities
  (HTTP `fetch`, KV, …) through the `JsHost` seam — the same extensibility point as
  `WasmHost` / `LuaHost`.

**Capabilities, including `fetch`.** QuickJS ships *no* host APIs — no `fetch`, no
networking — only the language. So `fetch` is a host-provided global: a Rust
capability (e.g. `reqwest`) the product injects via `JsHost`, exposed to the context
as `fetch`. Under the [async actor contract](./async-actor-contract.md) it is a real
`await` — `rquickjs` backs a JS Promise with the Rust future, so `await fetch(url)`
suspends the *script* (not a thread) until the response returns. QuickJS also supports
a memory cap and an interrupt handler, so a runaway script can be bounded.

**Trust posture — identical to the native Lua actor.** A QuickJS context has **no
ambient authority**: a script can do pure computation plus whatever the host injects
(`BaseJsHost` = `emit` only). That is capability-scoped, but it runs **in-process** —
no hardware memory isolation. This is exactly the Lua actor's posture: *hygiene, not
enforcement.* It is the right tier for author-written workflow code (the n8n "Code
node"); if that posture is acceptable for Lua, it is consistent for JS.

**Hardened alternative (when you need a real sandbox).** For genuinely hostile or
multi-tenant code, compile the JS to a wasm component (e.g.
[javy](https://github.com/bytecodealliance/javy)) and run it on the existing `"wasm"`
runtime — hardware-isolated, at the cost of a build step. It needs no new machinery
(it rides `fuchsia-actor-wasm`), so the two tiers coexist:

- **`js` runtime = native QuickJS** — dynamic, no compile, the ergonomic default.
- **wasm-compiled JS** — the hardened option when isolation must be enforced.

## Alternatives considered

- **Compile JS → wasm (javy) as the *primary*.** A hardware sandbox, but it forces a
  build step and an AOT toolchain — the wrong default for "drop in a script." Kept as
  the hardened alternative above. (This was the original proposal, flipped per the
  dynamic-script requirement.)
- **A different native engine** — `boa` (pure-Rust, no C dependency, but slower and
  less complete) or V8 via `deno_core` (fast, but a heavyweight dependency and complex
  embedding). `rquickjs`/QuickJS is the best fit: small, complete enough,
  vendored like `mlua`, capability-scoped, with memory/interrupt limits. `boa` is the
  fallback if avoiding the C dependency ever matters.
- **Lua only.** mlua covers trusted scripting, but it is not the language n8n users
  expect. Insufficient.

## Open questions / risks

- **`rquickjs` Send / threading.** It needs its threading feature to satisfy
  `Actor: Send`, the same way the Lua pack uses mlua's `send` feature; its async
  integration (future-backed promises) must compose with the
  [async actor contract](./async-actor-contract.md)'s executor.
- **Script API shape.** How does a script declare its handler — a global
  `handle(ctx, msg)`, an exported function, a default-export module? Mirror whatever
  the Lua pack settles on.
- **A baseline capability "standard library"** for JS authors (`fetch`, a logger,
  KV) vs. leaving each to the product. A product concern, but worth a recommended
  default.
- **Resource limits as policy.** A memory cap + interrupt-based timeout per script —
  defaults, and where they're configured (`settings`?).
