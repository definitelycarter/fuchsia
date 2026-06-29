//! The `Condition` a predicate node (`if`) evaluates over a message payload —
//! configuration, not code (it lives in a node instance's opaque `settings`).
//!
//! Two arms behind one `#[serde(untagged)]` enum, so a product picks the shape
//! without a re-design:
//!
//! - **Declarative** (`{ field, op, value }`, combinable with `all`/`any`
//!   groups) — pure data, the Home Assistant path.
//! - **`Expr`** (`{ "expr": "temp > 30" }`) — a [minijinja] expression, the
//!   n8n path. The payload→context mapping is documented on
//!   [`evaluate_expr`].
//!
//! [minijinja]: https://docs.rs/minijinja

use fuchsia_actor::{ActorError, Message, MessageValue};
use serde::Deserialize;

/// A predicate over a message payload. `#[serde(untagged)]`: a settings
/// document with an `expr` key selects [`Condition::Expr`]; anything else is the
/// tagless [`Condition::Declarative`] arm (the common case).
///
/// Variant order matters for `untagged`: `Expr` is tried first, so only a
/// document carrying `expr` matches it; every other shape falls through to
/// `Declarative`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Condition {
  /// A [minijinja] expression string, e.g. `{ "expr": "temp > 30" }` — the n8n
  /// path. Evaluated over the payload; see [`evaluate_expr`] for the
  /// payload→context mapping.
  ///
  /// [minijinja]: https://docs.rs/minijinja
  Expr { expr: String },
  /// A declarative predicate — `{ field, op, value }` with `all`/`any` groups.
  Declarative(DeclCondition),
}

/// A declarative predicate. Either a single `field op value` leaf or an
/// `all`/`any` group of sub-conditions. `#[serde(untagged)]` again: a document
/// with an `all` key is the conjunction, `any` the disjunction, otherwise a
/// leaf.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DeclCondition {
  /// All sub-conditions must hold (logical AND). An empty group is vacuously
  /// true.
  All { all: Vec<DeclCondition> },
  /// At least one sub-condition must hold (logical OR). An empty group is
  /// vacuously false.
  Any { any: Vec<DeclCondition> },
  /// A single comparison of one payload field against a literal.
  Leaf {
    field: String,
    op: Op,
    value: serde_json::Value,
  },
}

/// The comparison operators a leaf condition supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
  /// Equal (`==`). Compares JSON values directly (numbers numerically).
  Eq,
  /// Not equal (`!=`).
  Ne,
  /// Greater than (`>`). Numeric only.
  Gt,
  /// Greater than or equal (`>=`). Numeric only.
  Gte,
  /// Less than (`<`). Numeric only.
  Lt,
  /// Less than or equal (`<=`). Numeric only.
  Lte,
}

impl Condition {
  /// Validate and pre-build this condition into a [`PreparedCondition`] ready
  /// for the per-message hot path. For the [`Condition::Expr`] arm this is where
  /// the minijinja [`Environment`](minijinja::Environment) is constructed and
  /// the expression's **syntax is checked once** — a malformed `expr` fails here
  /// (at node construction), not per message.
  ///
  /// The `if`/`switch` builtins call this in their `create`, so a bad expression
  /// surfaces as [`ActorError::Config`] at provision time, like every other
  /// settings fault.
  pub fn prepare(self) -> Result<PreparedCondition, ActorError> {
    match self {
      Condition::Expr { expr } => {
        // Build the environment once and keep it for the actor's life. Validate
        // the expression up front; a compiled `Expression` borrows the env, so
        // we don't store it — but recompiling an already-validated expression
        // against this owned env on the hot path avoids the per-message
        // `Environment::new()` (the dominant cost) and can't reintroduce a
        // syntax error.
        let env = minijinja::Environment::new();
        env
          .compile_expression(&expr)
          .map_err(|e| ActorError::Config(format!("invalid expr condition {expr:?}: {e}")))?;
        Ok(PreparedCondition::Expr {
          env: Box::new(env),
          expr,
        })
      }
      Condition::Declarative(decl) => Ok(PreparedCondition::Declarative(decl)),
    }
  }
}

/// A [`Condition`] validated and made ready for repeated evaluation. Built once
/// (via [`Condition::prepare`]) and held by an `if`/`switch` actor; its
/// [`evaluate`](PreparedCondition::evaluate) is the per-message path.
#[derive(Debug)]
pub enum PreparedCondition {
  /// A pre-built minijinja environment plus the (already-validated) expression
  /// source. See [`evaluate_expr`] for the payload→context mapping.
  Expr {
    // Boxed so the variant — and therefore `PreparedCondition` — stays small;
    // the env is built once at construction and never reconstructed.
    env: Box<minijinja::Environment<'static>>,
    expr: String,
  },
  /// A declarative predicate — pure data, evaluated directly.
  Declarative(DeclCondition),
}

impl PreparedCondition {
  /// Evaluate against `msg`'s payload, yielding the branch boolean. A non-JSON
  /// payload (binary/empty) has no fields, so every field lookup misses and a
  /// leaf is `false`.
  pub fn evaluate(&self, msg: &Message) -> Result<bool, ActorError> {
    match self {
      PreparedCondition::Expr { env, expr } => evaluate_expr(env, expr, msg),
      PreparedCondition::Declarative(decl) => Ok(decl.evaluate(payload_json(msg))),
    }
  }
}

/// Evaluate a pre-validated [minijinja] expression over the payload, reusing the
/// `if`/`switch` actor's owned `env` (built once in [`Condition::prepare`]).
///
/// ## payload → context mapping (the RFC's deferred implementation choice)
///
/// - **Field access.** The payload JSON *object*'s top-level keys are the
///   expression's variables: `{ "temp": 42 }` makes `temp` a bare variable, so
///   `"temp > 30"` reads naturally. The mapping is value-preserving — numbers
///   stay numbers, strings strings, nested objects/arrays keep their structure
///   (`payload.sensor.id`, `items[0]`) — via minijinja's `serde` bridge over the
///   already-deserialized `serde_json::Value`.
/// - **Non-object / non-JSON payload.** A scalar, array, binary, or empty
///   message has no named fields, so the context is empty and any field
///   reference is undefined (see below). The whole payload is *not* injected
///   under a name here — top-level keys only — to keep the common
///   `field op value` spelling identical to the declarative arm.
/// - **Missing field → undefined, and a comparison against it is `false`.**
///   minijinja runs in its default (non-strict) undefined mode, so referencing
///   an absent key yields `undefined` rather than erroring; `undefined > 30`
///   (and `==`, `<`, …) evaluates to `false`. This matches the declarative
///   arm's "missing field is false" rule, so the two arms agree.
/// - **Type coercion.** Comparisons follow minijinja's own rules (numeric
///   compare for numbers, lexical for strings); we do not pre-coerce. The
///   expression's result is reduced to a branch boolean by minijinja
///   truthiness (`Value::is_true`) — a number, non-empty string, or `true` is
///   truthy.
/// - **Errors.** Syntax is validated once in [`Condition::prepare`], so the
///   expression here is known-good; a *runtime* error (e.g. an unknown filter)
///   still surfaces as [`ActorError::Config`] rather than silently routing to
///   `false`.
///
/// [minijinja]: https://docs.rs/minijinja
fn evaluate_expr(
  env: &minijinja::Environment<'static>,
  expr: &str,
  msg: &Message,
) -> Result<bool, ActorError> {
  // `compile_expression` borrows `env`, so the compiled form can't be stored
  // alongside it; reusing the owned env avoids the per-message
  // `Environment::new()`. The expression was validated at construction, so this
  // compile cannot fail in practice — map any error to a config fault anyway.
  let compiled = env
    .compile_expression(expr)
    .map_err(|e| ActorError::Config(format!("invalid expr condition {expr:?}: {e}")))?;

  // Build the context from the payload's top-level object keys. A non-object
  // payload contributes no variables (an empty context), so field references
  // are undefined → comparisons against them are `false`.
  let ctx = match payload_json(msg) {
    Some(serde_json::Value::Object(_)) => minijinja::Value::from_serialize(payload_json(msg)),
    _ => minijinja::Value::from_serialize(serde_json::Map::<String, serde_json::Value>::new()),
  };

  let result = compiled
    .eval(ctx)
    .map_err(|e| ActorError::Config(format!("evaluating expr condition {expr:?}: {e}")))?;
  Ok(result.is_true())
}

impl DeclCondition {
  /// Evaluate against an optional JSON payload (`None` for a non-JSON message).
  fn evaluate(&self, payload: Option<&serde_json::Value>) -> bool {
    match self {
      DeclCondition::All { all } => all.iter().all(|c| c.evaluate(payload)),
      DeclCondition::Any { any } => any.iter().any(|c| c.evaluate(payload)),
      DeclCondition::Leaf { field, op, value } => {
        // A missing field (or non-JSON payload) yields no actual — every
        // comparison against it is `false` (no silent panics on bad input).
        match payload.and_then(|p| p.get(field)) {
          Some(actual) => op.compare(actual, value),
          None => false,
        }
      }
    }
  }
}

impl Op {
  /// Apply this operator between the payload's `actual` value and the
  /// configured `expected`. Ordering ops are numeric-only; a non-number on
  /// either side makes them `false`.
  fn compare(self, actual: &serde_json::Value, expected: &serde_json::Value) -> bool {
    match self {
      Op::Eq => actual == expected,
      Op::Ne => actual != expected,
      Op::Gt | Op::Gte | Op::Lt | Op::Lte => match (actual.as_f64(), expected.as_f64()) {
        (Some(a), Some(b)) => match self {
          Op::Gt => a > b,
          Op::Gte => a >= b,
          Op::Lt => a < b,
          Op::Lte => a <= b,
          // The Eq/Ne arms are handled above; unreachable here.
          Op::Eq | Op::Ne => false,
        },
        _ => false,
      },
    }
  }
}

/// The payload's JSON value, if it is a JSON message; `None` for binary/empty.
fn payload_json(msg: &Message) -> Option<&serde_json::Value> {
  match &msg.value {
    MessageValue::Json(v) => Some(v),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use bson::doc;

  /// Deserialize and prepare a condition — the path the `if`/`switch` creators
  /// take. Panics if either step fails (the syntax-error case has its own test).
  fn cond(settings: bson::Document) -> PreparedCondition {
    let parsed: Condition =
      bson::de::deserialize_from_document(settings).expect("deserialize condition");
    parsed.prepare().expect("prepare condition")
  }

  fn json_msg(value: serde_json::Value) -> Message {
    Message::json("reading", value)
  }

  #[test]
  fn leaf_gt_true_and_false() {
    let c = cond(doc! { "field": "temp", "op": "gt", "value": 30 });
    assert!(
      c.evaluate(&json_msg(serde_json::json!({ "temp": 42 })))
        .unwrap()
    );
    assert!(
      !c.evaluate(&json_msg(serde_json::json!({ "temp": 20 })))
        .unwrap()
    );
  }

  #[test]
  fn leaf_eq_on_string() {
    let c = cond(doc! { "field": "kind", "op": "eq", "value": "temp" });
    assert!(
      c.evaluate(&json_msg(serde_json::json!({ "kind": "temp" })))
        .unwrap()
    );
    assert!(
      !c.evaluate(&json_msg(serde_json::json!({ "kind": "humidity" })))
        .unwrap()
    );
  }

  #[test]
  fn missing_field_is_false() {
    let c = cond(doc! { "field": "temp", "op": "gt", "value": 30 });
    assert!(
      !c.evaluate(&json_msg(serde_json::json!({ "other": 99 })))
        .unwrap()
    );
    // A non-JSON payload also has no fields.
    assert!(!c.evaluate(&Message::empty("x")).unwrap());
  }

  #[test]
  fn all_group_requires_every_arm() {
    let c = cond(doc! { "all": [
      { "field": "temp", "op": "gt", "value": 30 },
      { "field": "humidity", "op": "lt", "value": 50 },
    ] });
    assert!(
      c.evaluate(&json_msg(serde_json::json!({ "temp": 42, "humidity": 40 })))
        .unwrap()
    );
    // Second arm fails.
    assert!(
      !c.evaluate(&json_msg(serde_json::json!({ "temp": 42, "humidity": 60 })))
        .unwrap()
    );
  }

  #[test]
  fn any_group_requires_one_arm() {
    let c = cond(doc! { "any": [
      { "field": "temp", "op": "gt", "value": 100 },
      { "field": "humidity", "op": "lt", "value": 50 },
    ] });
    // First fails, second holds.
    assert!(
      c.evaluate(&json_msg(serde_json::json!({ "temp": 42, "humidity": 40 })))
        .unwrap()
    );
    // Neither holds.
    assert!(
      !c.evaluate(&json_msg(serde_json::json!({ "temp": 42, "humidity": 60 })))
        .unwrap()
    );
  }

  // ---- minijinja expr arm ----

  #[test]
  fn expr_true_and_false() {
    let c = cond(doc! { "expr": "temp > 30" });
    assert!(
      c.evaluate(&json_msg(serde_json::json!({ "temp": 42 })))
        .unwrap()
    );
    assert!(
      !c.evaluate(&json_msg(serde_json::json!({ "temp": 20 })))
        .unwrap()
    );
  }

  #[test]
  fn expr_reads_nested_fields() {
    let c = cond(doc! { "expr": "sensor.temp >= 100 and kind == 'temp'" });
    assert!(
      c.evaluate(&json_msg(
        serde_json::json!({ "sensor": { "temp": 100 }, "kind": "temp" })
      ))
      .unwrap()
    );
    assert!(
      !c.evaluate(&json_msg(
        serde_json::json!({ "sensor": { "temp": 99 }, "kind": "temp" })
      ))
      .unwrap()
    );
  }

  #[test]
  fn expr_missing_field_is_false_not_an_error() {
    // `absent` is undefined; `undefined > 30` is `false`, matching the
    // declarative arm's missing-field rule — and crucially *not* an error.
    let c = cond(doc! { "expr": "absent > 30" });
    assert!(
      !c.evaluate(&json_msg(serde_json::json!({ "temp": 42 })))
        .unwrap()
    );
  }

  #[test]
  fn expr_non_object_payload_has_no_fields() {
    let c = cond(doc! { "expr": "temp > 30" });
    // A non-JSON payload contributes no variables → `temp` undefined → false.
    assert!(!c.evaluate(&Message::empty("x")).unwrap());
  }

  #[test]
  fn expr_invalid_syntax_is_a_config_error_at_prepare() {
    // Syntax is validated once, up front in `prepare` — not per message.
    let parsed: Condition = bson::de::deserialize_from_document(doc! { "expr": "temp >" })
      .expect("deserialize condition");
    let err = parsed.prepare().unwrap_err();
    assert!(matches!(err, ActorError::Config(_)));
  }
}
