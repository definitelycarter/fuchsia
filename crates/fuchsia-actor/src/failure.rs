use std::time::Duration;

use serde::Deserialize;

/// The **host-understood** failure policy for one node — read by the runtime's
/// run loop (the consumer), *not* by the actor. It rides a typed field on
/// [`ActorConfig`](crate::ActorConfig) rather than the guest-opaque `settings`,
/// because the runtime decides what to do when a `handle` returns `Err`; the
/// actor never sees this.
///
/// The default ([`FailurePolicy::default`]) is today's behavior: an errored
/// message is folded into `Health` and dropped, the node keeps going
/// ([`OnError::Continue`]). An unset policy therefore changes nothing.
///
/// This type is deliberately a small open struct so the failure-handling slices
/// slot in without churning the public surface:
///
/// - `#[non_exhaustive]` lets fields be added without breaking the struct's
///   construction (callers build it with `..Default::default()`).
/// - [`OnError`] is itself `#[non_exhaustive]`, so a future arm (e.g. the
///   dead-letter terminal action) is a non-breaking addition.
///
/// `Deserialize` is derived so a product can parse its own JSON (`on_error`
/// block) straight into this; the runtime/engine construct it programmatically.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct FailurePolicy {
  /// What the runtime does when `handle` returns `Err`. Defaults to
  /// [`OnError::Continue`] — count + drop, today's behavior.
  pub on_error: OnError,
  /// Whether (and how) the runtime rebuilds this node after it *dies* — a panic
  /// (caught and isolated) or an abnormal task exit. Defaults to
  /// [`RestartPolicy::default`] = `max_restarts: 0` = **never restart**, so an
  /// unset policy keeps slice 1's behavior exactly: a death deregisters the node
  /// and stays dead, with no new per-message cost. See [`RestartPolicy`].
  pub restart: RestartPolicy,
  /// The **poison-quarantine threshold**: when a delivery's cross-delivery
  /// [`attempts`] count *exceeds* this, the runtime diverts it **without
  /// handling it** (so it can't crash the node again) and reports `Ok` on the
  /// ack (so an at-least-once feeder stops re-delivering). It goes to the
  /// dead-letter sink (reason
  /// [`Poison`](../../fuchsia_transport/enum.DeadLetterReason.html#variant.Poison))
  /// if one is granted, else it is counted on `Health`'s poisoned counter and
  /// dropped.
  ///
  /// `0` (the [`Default`]) **disables** quarantine — an unset policy is exactly
  /// slice-5 behavior, and a re-delivered message is handled normally however
  /// high its `attempts` climb. A node that opts in (`poison_after > 0`) bounds
  /// a **poison message**: an input that deterministically crashes the node
  /// every time, which an at-least-once feeder would otherwise re-deliver
  /// forever. The first-attempt crash is attributed to the node (it may be a
  /// genuine sick node); the re-delivery crashes are attributed to the message
  /// and don't burn the node's restart budget; once `attempts` crosses
  /// `poison_after` the message is quarantined and the node is spared.
  ///
  /// [`attempts`]: ../../fuchsia_transport/struct.Delivery.html#structfield.attempts
  #[serde(default)]
  pub poison_after: u32,
}

impl FailurePolicy {
  /// A policy that retries an errored `handle` up to `max` times with `backoff`
  /// between attempts before falling back to the terminal action. A small
  /// constructor so callers don't depend on the (non-exhaustive) struct shape.
  pub fn retry(max: u32, backoff: Backoff) -> Self {
    Self {
      on_error: OnError::Retry { max, backoff },
      ..Default::default()
    }
  }

  /// A policy that stops the node on the first errored `handle`.
  pub fn fail() -> Self {
    Self {
      on_error: OnError::Fail,
      ..Default::default()
    }
  }

  /// A policy that, on an errored `handle`, emits an error envelope on the
  /// node's reserved `"error"` port and keeps the node alive — the failure is
  /// *diverted* to the error branch, not retried. See [`OnError::RouteToError`].
  pub fn route_to_error() -> Self {
    Self {
      on_error: OnError::RouteToError,
      ..Default::default()
    }
  }

  /// A policy that rebuilds the node up to `max_restarts` times with `backoff`
  /// between rebuilds when it *dies* (a panic, or an abnormal exit) — leaving the
  /// `on_error` policy at its default. A small constructor so callers don't
  /// depend on the (non-exhaustive) struct shape; for a node that wants both an
  /// error policy and a restart policy, build the struct with `..Default::default()`.
  pub fn restart(max_restarts: u32, backoff: Backoff) -> Self {
    Self {
      restart: RestartPolicy {
        max_restarts,
        backoff,
      },
      ..Default::default()
    }
  }

  /// A policy that **quarantines a poison message**: a delivery whose
  /// cross-delivery `attempts` count exceeds `poison_after` is diverted without
  /// handling (so it can't crash the node again), leaving the other arms at
  /// their defaults. A small constructor so callers don't depend on the
  /// (non-exhaustive) struct shape; for a node that also wants a restart budget
  /// (the typical pairing — restart catches the first crash, poison quarantine
  /// stops the re-delivery loop), build the struct with `..Default::default()`.
  /// `poison_after: 0` leaves quarantine disabled (slice-5 behavior).
  pub fn poison_after(poison_after: u32) -> Self {
    Self {
      poison_after,
      ..Default::default()
    }
  }
}

/// How (and whether) the runtime rebuilds a node after it **dies** — a panic in
/// `handle` (caught and isolated) or an abnormal task exit. Distinct from
/// [`OnError`], which governs a `handle` that *returns* `Err`: a death is a
/// crash, not a deliberate policy outcome.
///
/// `max_restarts: 0` (the [`Default`]) means **never restart**, so a node with
/// an unset policy behaves exactly as before this slice: a death is permanent —
/// deregister + record on `Health` — and the node pays **no** new per-message
/// cost (the runtime keeps the lean single-task path for it). Opting in
/// (`max_restarts > 0`) moves the node onto a supervisor that owns its mailbox
/// and rebuild recipe, so it can be rebuilt on the *same* mailbox across a death
/// (the queue survives, routing is uninterrupted).
///
/// A `fail`-policy stop is **never** a restart trigger — it's a deliberate
/// shutdown, not a crash. Only an undeliberate death (panic / abnormal exit)
/// consumes the restart budget.
///
/// **Restart requires `panic = unwind`** (the default). The supervisor recovers a
/// crashed handler by catching its panic with `catch_unwind`; under
/// `panic = abort` a panic terminates the whole process instead, so there is
/// nothing left to restart. A host that compiles with `panic = abort` should not
/// rely on restart for crash recovery.
///
/// `#[non_exhaustive]` + `#[serde(default)]` so it's a non-breaking field add and
/// a product's JSON can give partials (or nothing) and still deserialize.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RestartPolicy {
  /// How many times the runtime rebuilds the node after a death before giving up
  /// and letting it die permanently. `0` (the default) = never restart. The
  /// budget is per-node and is **reset** by a manual `Engine::restart_node`.
  pub max_restarts: u32,
  /// The delay before each rebuild — reusing the same exponential [`Backoff`] as
  /// the retry policy. The delay before the *n*-th rebuild (0-indexed) is
  /// `backoff.delay_for(n)`, so repeated crashes back off rather than hot-loop.
  pub backoff: Backoff,
}

/// What the runtime does when an actor's `handle` returns `Err`.
///
/// `#[non_exhaustive]` so a future arm can be added without breaking exhaustive
/// `match`es in downstream code (e.g. the dead-letter terminal action, part 4).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OnError {
  /// Fold the error into `Health` (errored) + drop the message, keep handling
  /// the next. **The default** — today's behavior, the right thing for lossy /
  /// conditioning paths.
  #[default]
  Continue,
  /// Stop the actor: run `teardown` and deregister so it stops resolving.
  /// Fail-fast nodes.
  Fail,
  /// Re-invoke `handle` on the *same* message up to `max` times with `backoff`
  /// between attempts before applying the terminal action. Distinct from the
  /// at-least-once *delivery* retry (`Ack::Complete`'s dropped-sender retry):
  /// this is a handler that errored on a *delivered* message.
  Retry {
    /// How many *re*-invocations to make after the first attempt fails. `0`
    /// behaves like [`Continue`](OnError::Continue).
    #[serde(default = "default_max_retries")]
    max: u32,
    #[serde(default)]
    backoff: Backoff,
  },
  /// On a handled `Err`, emit an **error envelope** — the error string, the node
  /// id, and the original message's type/payload — on the node's reserved
  /// `"error"` output port, then keep handling the next message. A flow wires
  /// that port to an error-handling sub-graph (n8n's "error workflow"); if
  /// nothing is wired, the engine counts the emit as `no_route` on
  /// `(node, "error")` (the dead-letter sink in part 4 becomes the real
  /// catch-all). The runtime emits on the node's behalf, so the failure is
  /// *diverted* — not retried — and the node continues.
  RouteToError,
}

fn default_max_retries() -> u32 {
  3
}

/// An exponential backoff schedule, reusable across the retry policy here and
/// the restart policy a later slice adds (the RFC sketches the same
/// `{ initial, multiplier, cap }` shape for both). The delay before attempt *n*
/// (0-indexed) is `min(initial * multiplier^n, cap)`.
///
/// `#[serde(default)]` fields with sensible defaults mean a product's JSON can
/// give just `backoff_ms`-style partials (or nothing) and still deserialize.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Backoff {
  /// The delay before the first retry.
  pub initial: Duration,
  /// Factor each subsequent delay is multiplied by (`1.0` = constant backoff).
  pub multiplier: f64,
  /// Upper bound on any single delay, so exponential growth can't run away.
  pub cap: Duration,
}

impl Backoff {
  /// A fixed (non-growing) backoff of `delay` between every attempt.
  pub fn fixed(delay: Duration) -> Self {
    Self {
      initial: delay,
      multiplier: 1.0,
      cap: delay,
    }
  }

  /// The delay before the retry that *follows* `attempt` already-made retries
  /// (0-indexed): `min(initial * multiplier^attempt, cap)`. Saturating, so an
  /// overflowing exponential just clamps to `cap`.
  pub fn delay_for(&self, attempt: u32) -> Duration {
    let factor = self.multiplier.powi(attempt as i32);
    // `initial` scaled by the (non-finite-safe) factor, then clamped to `cap`.
    // Guard NaN/inf and negatives by clamping the seconds to `cap` directly.
    let scaled_secs = self.initial.as_secs_f64() * factor;
    if !scaled_secs.is_finite() {
      return self.cap;
    }
    let scaled = Duration::try_from_secs_f64(scaled_secs).unwrap_or(self.cap);
    scaled.min(self.cap)
  }
}

impl Default for Backoff {
  /// Conservative starting numbers (the RFC leaves the exact curve open): a
  /// 100ms first delay, doubling, capped at 5s.
  fn default() -> Self {
    Self {
      initial: Duration::from_millis(100),
      multiplier: 2.0,
      cap: Duration::from_secs(5),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_policy_is_continue() {
    assert_eq!(FailurePolicy::default().on_error, OnError::Continue);
  }

  #[test]
  fn backoff_grows_exponentially_and_clamps_to_cap() {
    let b = Backoff {
      initial: Duration::from_millis(100),
      multiplier: 2.0,
      cap: Duration::from_millis(350),
    };
    assert_eq!(b.delay_for(0), Duration::from_millis(100));
    assert_eq!(b.delay_for(1), Duration::from_millis(200));
    // 400ms would exceed the 350ms cap, so it clamps.
    assert_eq!(b.delay_for(2), Duration::from_millis(350));
  }

  #[test]
  fn fixed_backoff_is_constant() {
    let b = Backoff::fixed(Duration::from_millis(50));
    assert_eq!(b.delay_for(0), Duration::from_millis(50));
    assert_eq!(b.delay_for(5), Duration::from_millis(50));
  }

  #[test]
  fn retry_constructor_sets_arms() {
    let p = FailurePolicy::retry(2, Backoff::fixed(Duration::from_millis(10)));
    assert_eq!(
      p.on_error,
      OnError::Retry {
        max: 2,
        backoff: Backoff::fixed(Duration::from_millis(10))
      }
    );
  }

  #[test]
  fn deserializes_continue_from_json() {
    let p: FailurePolicy =
      serde_json::from_value(serde_json::json!({ "on_error": { "policy": "continue" } }))
        .expect("parse");
    assert_eq!(p.on_error, OnError::Continue);
  }

  #[test]
  fn deserializes_retry_with_defaults() {
    // Only the discriminant given; `max` defaults to 3 and `backoff` to its Default.
    let on_error: OnError =
      serde_json::from_value(serde_json::json!({ "policy": "retry" })).expect("parse");
    assert_eq!(
      on_error,
      OnError::Retry {
        max: 3,
        backoff: Backoff::default()
      }
    );
  }

  #[test]
  fn deserializes_fail() {
    let on_error: OnError =
      serde_json::from_value(serde_json::json!({ "policy": "fail" })).expect("parse");
    assert_eq!(on_error, OnError::Fail);
  }

  #[test]
  fn deserializes_route_to_error() {
    // `#[serde(rename_all = "snake_case")]` → the snake_case discriminant.
    let on_error: OnError =
      serde_json::from_value(serde_json::json!({ "policy": "route_to_error" })).expect("parse");
    assert_eq!(on_error, OnError::RouteToError);
  }

  #[test]
  fn route_to_error_constructor_sets_arm() {
    assert_eq!(
      FailurePolicy::route_to_error().on_error,
      OnError::RouteToError
    );
  }

  #[test]
  fn default_restart_never_restarts() {
    // The default is restart-off — an unset policy keeps slice 1's behavior.
    assert_eq!(FailurePolicy::default().restart.max_restarts, 0);
  }

  #[test]
  fn restart_constructor_sets_budget_and_backoff() {
    let p = FailurePolicy::restart(3, Backoff::fixed(Duration::from_millis(10)));
    assert_eq!(p.restart.max_restarts, 3);
    assert_eq!(p.restart.backoff, Backoff::fixed(Duration::from_millis(10)));
    // The error policy is untouched — restart is orthogonal to on_error.
    assert_eq!(p.on_error, OnError::Continue);
  }

  #[test]
  fn deserializes_restart_from_json() {
    // A product's JSON can give the restart block; absent fields default in.
    let p: FailurePolicy =
      serde_json::from_value(serde_json::json!({ "restart": { "max_restarts": 5 } }))
        .expect("parse");
    assert_eq!(p.restart.max_restarts, 5);
    assert_eq!(p.restart.backoff, Backoff::default());
  }

  #[test]
  fn default_poison_after_is_disabled() {
    // The default is quarantine-off (`0`) — an unset policy keeps slice-5
    // behavior, so a re-delivered message is handled however high its attempts.
    assert_eq!(FailurePolicy::default().poison_after, 0);
  }

  #[test]
  fn poison_after_constructor_sets_threshold() {
    let p = FailurePolicy::poison_after(3);
    assert_eq!(p.poison_after, 3);
    // The other arms are untouched — poison quarantine is orthogonal.
    assert_eq!(p.on_error, OnError::Continue);
    assert_eq!(p.restart.max_restarts, 0);
  }

  #[test]
  fn deserializes_poison_after_from_json() {
    // A product's JSON can give the threshold; absent it defaults to disabled.
    let p: FailurePolicy =
      serde_json::from_value(serde_json::json!({ "poison_after": 5 })).expect("parse");
    assert_eq!(p.poison_after, 5);
    let p: FailurePolicy =
      serde_json::from_value(serde_json::json!({ "on_error": { "policy": "fail" } }))
        .expect("parse");
    assert_eq!(p.poison_after, 0);
  }

  #[test]
  fn restart_defaults_when_absent_from_json() {
    // No `restart` block → never-restart default, so existing config is unchanged.
    let p: FailurePolicy =
      serde_json::from_value(serde_json::json!({ "on_error": { "policy": "fail" } }))
        .expect("parse");
    assert_eq!(p.restart.max_restarts, 0);
    assert_eq!(p.on_error, OnError::Fail);
  }
}
