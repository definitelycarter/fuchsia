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
/// This type is deliberately a small open struct so the *later* failure-handling
/// slices slot in without churning the public surface:
///
/// - `#[non_exhaustive]` lets fields be added (a `restart: RestartPolicy` and a
///   `poison_after: u32` are coming — see the node-failure-handling RFC) without
///   breaking the struct's construction (callers build it with
///   `..Default::default()`).
/// - [`OnError`] is itself `#[non_exhaustive]`, so the future `RouteToError`
///   arm (the reserved error output port) is a non-breaking addition.
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
}

impl FailurePolicy {
  /// A policy that retries an errored `handle` up to `max` times with `backoff`
  /// between attempts before falling back to the terminal action. A small
  /// constructor so callers don't depend on the (non-exhaustive) struct shape.
  pub fn retry(max: u32, backoff: Backoff) -> Self {
    Self {
      on_error: OnError::Retry { max, backoff },
    }
  }

  /// A policy that stops the node on the first errored `handle`.
  pub fn fail() -> Self {
    Self {
      on_error: OnError::Fail,
    }
  }
}

/// What the runtime does when an actor's `handle` returns `Err`.
///
/// `#[non_exhaustive]` so the RFC's future `RouteToError` arm — emit the failed
/// message on the node's reserved `"error"` port (part 3) — can be added without
/// breaking exhaustive `match`es in downstream code.
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
}
