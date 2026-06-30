//! The **restart supervisor** — the per-node task for a node that opted into
//! restart (`failure.restart.max_restarts > 0`).
//!
//! A default node (`max_restarts == 0`) keeps the lean slice-1 path
//! ([`run_actor`](crate::runtime) + [`supervise`](crate::runtime)): the actor
//! task moves in `rx`, and a panic unwinds it. That path pays **nothing** new
//! per message — no `catch_unwind`, no extra task hop — which is the whole point
//! of branching on the budget. This module is reached only when a node asks to
//! be restarted.
//!
//! When restart is on, the per-node task is restructured so the **mailbox
//! survives a crash**:
//!
//! - The supervisor task **owns `rx`** (and the rebuild recipe), and never
//!   unwinds on a `handle` panic — each `handle_with_policy` call is wrapped in
//!   [`AssertUnwindSafe`]`(..).catch_unwind()`, so a panic is *caught* in this
//!   task rather than unwinding it. `AssertUnwindSafe` is justified because the
//!   panicked actor is **discarded and rebuilt fresh** (a new `&mut self`, a
//!   fresh `setup`) — the poisoned-state objection that rejects `catch_unwind`
//!   for in-place *resume* does not apply to discard-and-rebuild.
//! - On a caught panic (or an actor that returned a `fail`-`stop` — see below),
//!   with restart budget left, the supervisor counts a restart, waits the
//!   backoff, and rebuilds the actor on the **same `rx`** — the queue and the
//!   router entry are untouched, so routing is uninterrupted and queued messages
//!   are drained by the new incarnation. The in-flight message that panicked is
//!   **dropped, not re-fed** (its ack drops: a `Complete` reads as lost and
//!   retries on the feeder's side, a `Health` ack is simply uncounted), so a bad
//!   *in-memory* message can't loop the budget away.
//! - **Poison protection.** A panic charges the restart budget *only* when the
//!   crashing delivery was a **first attempt** (`attempts <= 1`). A crash on a
//!   feeder **re-delivery** (`attempts > 1`) rebuilds the node *without* spending
//!   budget — it's attributed to the message, not the node — so a poison message
//!   re-delivered by an at-least-once feeder can't kill an otherwise-healthy node
//!   (**mechanism B**). Paired with the quarantine gate
//!   ([`poison_check`](crate::runtime::poison_check), **mechanism A**) at the top
//!   of the loop, which diverts that message once its `attempts` cross the node's
//!   `poison_after`: the message crashes once (charging one restart), its
//!   re-deliveries crash without charging, then it is quarantined — node spared,
//!   message preserved.
//! - When the budget is exhausted the node **dies permanently**: it records the
//!   death (deregister + `Health::died` + the death listener, via
//!   [`record_death`](crate::runtime::record_death)) and drains whatever is left
//!   in `rx` to the dead-letter sink (reason
//!   [`NodeDied`](fuchsia_transport::DeadLetterReason::NodeDied)).
//!
//! A `fail`-policy stop is a **deliberate** shutdown, not a crash, so it is
//! **never** restarted even with budget left (see [`Incarnation::Stopped`]).
//!
//! [`AssertUnwindSafe`]: std::panic::AssertUnwindSafe

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorId, Emit, FailurePolicy,
  RestartPolicy,
};
use fuchsia_transport::{
  DeadLetter, DeadLetterReason, DeadLettered, Delivery, Health, MailboxRx, WeakMailboxTx,
};
use futures_util::FutureExt;
use tokio::sync::Notify;

use crate::registry::{ActorHandle, ActorRegistry};
use crate::runtime::{DeathListener, FailureSinks, handle_with_policy, poison_check};

/// The rebuild recipe + run-loop state a [`supervise_with_restart`] task owns —
/// everything needed to build *one more* incarnation of a node on its surviving
/// mailbox. Built once at spawn (`commit`) and kept for the supervisor's life.
///
/// The capability bag (`caps`) is **owned** here, not re-derived: `create` only
/// *borrows* it, so the same bag — with the already-injected `schedule` (whose
/// weak mailbox handle stays valid across rebuilds) and `emit` — is re-offered
/// to each incarnation, exactly as the RFC specifies. `emit` / `dead_letter` are
/// the runtime's own refcount-bumped handles into that bag, pulled once.
pub(crate) struct RestartRecipe {
  pub(crate) creator: Arc<dyn ActorCreator>,
  pub(crate) type_name: String,
  pub(crate) config: ActorConfig,
  pub(crate) caps: ActorCapabilities,
  pub(crate) ctx: ActorContext,
  pub(crate) node: ActorId,
  pub(crate) failure: FailurePolicy,
  pub(crate) emit: Arc<dyn Emit>,
  pub(crate) dead_letter: Option<Arc<dyn DeadLetter>>,
}

impl RestartRecipe {
  /// Build a fresh actor instance for the next incarnation — a new `&mut self`
  /// from the same creator + config + capability bag the spawn used. Errors
  /// (e.g. a `create` that fails on rebuild) abort the incarnation, treated by
  /// the caller as a death.
  fn build(&self) -> Result<Box<dyn Actor>, fuchsia_actor::ActorError> {
    self.creator.create(&self.config, &self.caps)
  }
}

/// A node-side control channel an [`Engine`] holds for a restart-enabled node,
/// so it can force a restart from *outside* the automatic budget — the
/// runtime-side half of `Engine::restart_node`.
///
/// It does two things over a shared [`Notify`] + a flag:
/// - **Revive a dead node**: a supervisor whose budget is exhausted *parks*
///   (rather than exiting), holding its recipe, and waits on `revive`. A manual
///   restart fires `revive`, so the supervisor re-registers, resets its budget,
///   and resumes draining the surviving `rx`. This is what keeps the recipe
///   alive across a permanent death without the engine duplicating it.
/// - **Force-restart a live node**: `force` sets a flag and fires `revive`; the
///   supervisor's inner loop notices, tears the current incarnation down, resets
///   the budget, and rebuilds.
///
/// [`Engine`]: ../../fuchsia_engine/struct.Engine.html
#[derive(Clone)]
pub struct RestartControl {
  inner: Arc<RestartControlInner>,
}

struct RestartControlInner {
  /// Fired by `Engine::restart_node` to wake a parked (dead) supervisor or
  /// signal a live one to force-rebuild.
  revive: Notify,
  /// Set when the most recent revive request asked to force-restart a *live*
  /// incarnation (vs. just reviving a dead one). Read + cleared by the
  /// supervisor when it acts on a wake.
  force: AtomicBool,
  /// Whether the node is currently parked dead (budget exhausted, awaiting
  /// revive). Read by `Engine::restart_node` to tell a dead node (revive) from a
  /// live one (force-only) without racing the supervisor.
  parked_dead: AtomicBool,
}

impl RestartControl {
  fn new() -> Self {
    Self {
      inner: Arc::new(RestartControlInner {
        revive: Notify::new(),
        force: AtomicBool::new(false),
        parked_dead: AtomicBool::new(false),
      }),
    }
  }

  /// Whether the node is currently parked dead (budget exhausted), awaiting a
  /// manual revive. A live node returns `false`.
  pub fn is_dead(&self) -> bool {
    self.inner.parked_dead.load(Ordering::SeqCst)
  }

  /// Request a manual restart. `force` restarts a *live* incarnation (teardown +
  /// rebuild); without it, only a dead (parked) node is revived — the engine
  /// rejects a force-less restart of a live node *before* calling this. Either
  /// way the budget is reset. Wakes the supervisor.
  pub fn request_restart(&self, force: bool) {
    if force {
      self.inner.force.store(true, Ordering::SeqCst);
    }
    self.inner.revive.notify_one();
  }
}

/// The result of one incarnation's recv loop — why it ended, which decides
/// whether the supervisor restarts, parks dead, or shuts down cleanly.
enum Incarnation {
  /// `handle` panicked (caught by `catch_unwind`). A crash → restart if budget
  /// remains, else die permanently. Carries the crashing delivery's
  /// cross-delivery `attempts` count so the supervisor can charge the restart
  /// budget **only on a first attempt** (`attempts <= 1`) — **mechanism B**: a
  /// re-delivery crash is attributed to the *message*, not the node, so a poison
  /// message can't burn an otherwise-healthy node's budget.
  Panicked {
    /// The `attempts` count of the delivery that panicked (`1` for a first/normal
    /// delivery, `> 1` for a feeder re-delivery).
    attempts: u32,
  },
  /// `rx` closed (every sender dropped) without an intentional stop, while the
  /// runtime is still up — senders vanished out from under a live node. Treated
  /// as a death, like the non-restart path's abnormal-exit classification.
  Abandoned,
  /// The `fail` policy stopped the node on an errored handle — a *deliberate*
  /// stop. Never restarted (it's intentional); runs teardown and shuts down.
  Stopped,
  /// A clean shutdown: `rx` closed after an intentional `stop` /
  /// `remove_graph` (or the whole runtime dropped). Not a death, not restarted.
  Shutdown,
  /// `create` failed building this incarnation — treated as a crash (restart if
  /// budget remains).
  BuildFailed,
  /// A manual `Engine::restart_node(force)` asked to rebuild this live
  /// incarnation. Teardown + rebuild with a reset budget.
  ForceRestart,
}

/// Build the runtime-side control handle for a restart-enabled node. The engine
/// keeps a clone to drive `restart_node`. Returned from `commit` alongside the
/// node's mailbox/health.
pub(crate) fn restart_control() -> RestartControl {
  RestartControl::new()
}

/// The per-node restart supervisor task. Owns `rx`, the rebuild recipe, the
/// restart policy, and the death machinery; runs the incarnation loop described
/// in the module docs. See [`RestartRecipe`] for what it holds.
///
/// `initial` is the **first** incarnation — already built and `setup`-run by the
/// spawn (`prepare` + `Spawning::setup`, outside the runtime lock, exactly as a
/// non-restart node). The supervisor adopts it for the first loop; every
/// *re*-build (a restart, a revive, a force) goes through the recipe (a fresh
/// `create` + `setup`). This keeps the setup-outside-the-lock guarantee for the
/// initial spawn while the supervisor owns the rebuild path.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn supervise_with_restart(
  initial: Box<dyn Actor>,
  recipe: RestartRecipe,
  mut rx: MailboxRx,
  // A **weak** sender into the surviving mailbox — held only so a *revived* node
  // can be re-registered with a routable handle (the registry's copy was removed
  // on death). It is deliberately weak so it does **not** keep `rx` open: an
  // intentional `stop` / `remove_graph` (dropping the registry's *and* the
  // engine's strong senders) must be able to close `rx` for a clean shutdown.
  // On revival the engine still holds a strong sender (in its restart handle),
  // so this upgrades.
  tx: WeakMailboxTx,
  policy: RestartPolicy,
  health: Arc<Health>,
  stopping: Arc<AtomicBool>,
  registry: Weak<Mutex<ActorRegistry>>,
  death_listener: Option<DeathListener>,
  control: RestartControl,
) {
  // Restarts spent so far this *budget* — reset to 0 by a manual restart.
  let mut restarts: u32 = 0;
  // The first incarnation is handed in already set up; later incarnations are
  // rebuilt from the recipe. `take()` on the first loop, then `None`.
  let mut initial = Some(initial);

  loop {
    // The first iteration adopts the pre-built initial actor; subsequent
    // iterations rebuild from the recipe (fresh `create` + `setup`). A build /
    // setup failure on a *rebuild* is a crash (restart if budget remains).
    let actor = match initial.take() {
      Some(actor) => Some(actor),
      None => match recipe.build() {
        Ok(mut actor) => {
          // `setup` for a rebuilt incarnation. A panic is caught (the actor is
          // about to be discarded on failure anyway); a returned `Err` aborts as
          // a build failure (restartable).
          match AssertUnwindSafe(actor.setup(&recipe.ctx))
            .catch_unwind()
            .await
          {
            Ok(Ok(())) => Some(actor),
            Ok(Err(err)) => {
              tracing::error!(node = %recipe.node, error = %err, "actor setup failed on restart");
              None
            }
            Err(_panic) => {
              tracing::error!(node = %recipe.node, "actor setup panicked on restart");
              None
            }
          }
        }
        Err(err) => {
          tracing::error!(node = %recipe.node, error = %err, "actor rebuild failed");
          None
        }
      },
    };

    let outcome = match actor {
      None => Incarnation::BuildFailed,
      Some(mut actor) => {
        run_incarnation(&mut actor, &recipe, &mut rx, &stopping, &control, &health).await
        // `actor` is dropped here on every exit, running its `Drop`. On a
        // `Stopped`/`Shutdown`/`ForceRestart` we explicitly ran `teardown`
        // inside `run_incarnation`; on a crash we deliberately discard without
        // `teardown` (the instance is poisoned) — matching the non-restart path,
        // where a panic also skips `teardown`.
      }
    };

    match outcome {
      // Clean shutdown — intentional stop / remove_graph / runtime drop. Not a
      // death, no restart, no dead-letter drain. The node is simply gone.
      Incarnation::Shutdown => return,

      // Deliberate `fail` stop. Runs as a death (slice 1/2 semantics: teardown
      // already ran, the errored ack + `Health::died` were recorded), and is
      // **never** restarted — it's intentional. Record the death and exit.
      Incarnation::Stopped => {
        crate::runtime::record_death(
          &recipe.node,
          &health,
          registry.upgrade().as_ref(),
          death_listener.as_ref(),
        );
        return;
      }

      // A crash (panic / abandoned / build failure). Restart if budget remains,
      // else die permanently.
      Incarnation::Panicked { .. } | Incarnation::Abandoned | Incarnation::BuildFailed => {
        // **Mechanism B** — the budget-charging rule. A crash charges the restart
        // budget only when it is attributed to the *node*, not a *message*:
        // - A panic on a **first attempt** (`attempts <= 1`) may be a genuine
        //   sick node, so it charges (and a node crashing on varied first-attempt
        //   inputs burns its budget and dies).
        // - A panic on a **re-delivery** (`attempts > 1`) is the *same* input
        //   crashing again — attributed to the message — so it rebuilds the node
        //   *without* charging, sparing an otherwise-healthy node. This sparing
        //   applies **only when poison quarantine is enabled** (`poison_after >
        //   0`): mechanism A then bounds the loop by diverting the message once
        //   `attempts` cross the threshold. With quarantine **off**
        //   (`poison_after == 0`) nothing would stop the re-deliveries, so a
        //   re-delivery crash charges normally — the node dies after
        //   `max_restarts` (slice-5 behavior) instead of rebuilding forever.
        // - An `Abandoned` / `BuildFailed` crash has no triggering delivery, so
        //   it is always node-attributed and charges.
        let charge = match outcome {
          Incarnation::Panicked { attempts } => attempts <= 1 || recipe.failure.poison_after == 0,
          _ => true,
        };

        if !charge {
          // A re-delivery panic: rebuild on the surviving `rx` without spending
          // budget. Back off (indexed by restarts already made) so a fast
          // re-delivery loop doesn't hot-spin, but leave `restarts` untouched.
          let delay = policy.backoff.delay_for(restarts);
          if !delay.is_zero() {
            tokio::time::sleep(delay).await;
          }
          tracing::warn!(node = %recipe.node, "rebuilding node after a re-delivery crash (budget spared — message-attributed)");
          continue;
        }

        if restarts < policy.max_restarts {
          // Budget remains: wait the backoff (indexed by restarts already made),
          // count it, and loop to rebuild on the surviving `rx`.
          let delay = policy.backoff.delay_for(restarts);
          if !delay.is_zero() {
            tokio::time::sleep(delay).await;
          }
          restarts += 1;
          tracing::warn!(node = %recipe.node, restart = restarts, "restarting node after death");
          continue;
        }

        // Budget exhausted → permanent death. Record it (deregister + Health +
        // listener) and drain whatever's left in `rx` to the dead-letter sink,
        // then park awaiting a manual revive (keeping the recipe alive).
        permanently_dead(
          &recipe,
          &mut rx,
          &health,
          &registry,
          death_listener.as_ref(),
          restarts,
        );

        // Park awaiting a manual `Engine::restart_node`, but also watch `rx`. The
        // supervisor holds only a *weak* sender, so when the engine drops its
        // retained strong sender (a `remove_graph` tearing this dead node down)
        // `rx` closes — and without watching it the parked task would leak
        // forever, holding `rx` + the recipe with no revive ever coming. A stray
        // delivery that raced deregistration is dead-lettered, and parking
        // continues. On revive, reset the budget and loop.
        //
        // `is_dead` flips true only here, *after* `permanently_dead` above
        // deregistered the node — so a `restart_node` landing in that brief
        // window sees the node as still-live and returns `AlreadyRunning`
        // (retryable). Flipping it earlier would race `restart_node`'s
        // router-restore against that deregister, which is worse; the transient
        // is accepted.
        control.inner.parked_dead.store(true, Ordering::SeqCst);
        let revived = loop {
          tokio::select! {
            // Biased: poll the revive first, so a pending revive always wins over
            // a concurrent delivery arriving in the same wakeup — reviving must
            // never lose to (or dead-letter) a message it should handle after the
            // revive.
            biased;
            _ = control.inner.revive.notified() => break true,
            maybe = rx.recv() => match maybe {
              // Every strong sender dropped → the node was removed while parked.
              // Reap the task, freeing `rx` + the recipe.
              None => break false,
              // A late bystander to a dead (deregistered) node; preserve it and
              // keep parking.
              Some(delivery) => dead_letter_one(&recipe, delivery, restarts),
            },
          }
        };
        control.inner.parked_dead.store(false, Ordering::SeqCst);
        if !revived {
          return;
        }
        control.inner.force.store(false, Ordering::SeqCst);
        restarts = 0;
        tracing::info!(node = %recipe.node, "reviving permanently-dead node (manual restart)");
        // Re-register so the node resolves again before it starts handling. The
        // engine separately re-registers it in its router (see
        // `Engine::restart_node`); this restores the runtime's own address book.
        reregister(&recipe, &tx, &health, &stopping, &registry);
        continue;
      }

      // Manual force-restart of a live node: teardown already ran in
      // `run_incarnation`. Reset the budget and rebuild on the surviving `rx`;
      // the router entry never went away, so routing is uninterrupted.
      Incarnation::ForceRestart => {
        restarts = 0;
        tracing::info!(node = %recipe.node, "force-restarting node (manual restart)");
        continue;
      }
    }
  }
}

/// Run `teardown`, **catching a panic** so it can't unwind the supervisor task.
/// The restart supervisor *is* the per-node task — unlike the lean default path,
/// there is no `JoinHandle` watcher behind it — so an unhandled `teardown` panic
/// here would kill the node silently and leave its router entry behind as a
/// zombie (exactly the failure death detection exists to prevent). The instance
/// is being discarded either way, so a panicking or erroring `teardown` is logged
/// and swallowed; the caller proceeds with its intended lifecycle outcome.
async fn teardown_caught(actor: &mut Box<dyn Actor>, recipe: &RestartRecipe) {
  match AssertUnwindSafe(actor.teardown(&recipe.ctx))
    .catch_unwind()
    .await
  {
    Ok(Ok(())) => {}
    Ok(Err(err)) => tracing::error!(node = %recipe.node, error = %err, "actor teardown errored"),
    Err(_panic) => tracing::error!(node = %recipe.node, "actor teardown panicked"),
  }
}

/// One incarnation's recv→handle loop, returning *why* it ended. Mirrors
/// [`run_actor`](crate::runtime)'s loop, but each `handle_with_policy` is wrapped
/// in `catch_unwind` so a panic is caught here (the supervisor's task) instead of
/// unwinding `rx`. Also watches the [`RestartControl`] for a force-restart.
async fn run_incarnation(
  actor: &mut Box<dyn Actor>,
  recipe: &RestartRecipe,
  rx: &mut MailboxRx,
  stopping: &AtomicBool,
  control: &RestartControl,
  health: &Health,
) -> Incarnation {
  let sinks = FailureSinks {
    emit: recipe.emit.as_ref(),
    dead_letter: recipe.dead_letter.as_deref(),
    node: &recipe.node,
  };
  let poison_after = recipe.failure.poison_after;

  loop {
    // Act on a pending force-restart up front. The `force` flag — not the
    // wakeup — is the source of truth: a force requested while the node was busy
    // handling is honored after the current message rather than waiting behind a
    // steady stream (the `select!` below only *wakes* an idle node for it).
    // Checking the flag every iteration means a force is never lost to the
    // `select!`'s race, and is honored within one message even under load.
    if control.inner.force.swap(false, Ordering::SeqCst) {
      teardown_caught(actor, recipe).await;
      return Incarnation::ForceRestart;
    }

    tokio::select! {
      maybe = rx.recv() => {
        let Some(delivery) = maybe else {
          // `rx` closed: an intentional stop set the flag (clean shutdown),
          // otherwise senders vanished out from under a live node (a death).
          teardown_caught(actor, recipe).await;
          return if stopping.load(Ordering::SeqCst) {
            Incarnation::Shutdown
          } else {
            Incarnation::Abandoned
          };
        };

        // **Mechanism A**: quarantine a poison delivery before it reaches
        // `handle`, so a re-delivered message that keeps panicking this node is
        // diverted (sink/Health) + `Ok`-acked instead of crashing it yet again.
        // `None` means it was quarantined here; loop to the next message.
        let Some(delivery) = poison_check(delivery, poison_after, &sinks, health) else {
          continue;
        };

        // Capture the crashing delivery's `attempts` *before* `msg` is moved into
        // `handle` — on an unwind it's gone, so it's read here for mechanism B's
        // budget-charging decision.
        let Delivery { msg, ack, span: parent, correlation, attempts } = delivery;

        let result = AssertUnwindSafe(handle_with_policy(
          actor,
          &recipe.ctx,
          &recipe.failure.on_error,
          msg,
          &parent,
          correlation,
          &sinks,
        ))
        .catch_unwind()
        .await;

        match result {
          Ok((outcome, stop)) => {
            ack.report(outcome);
            if stop {
              // `OnError::Fail`: a deliberate stop. Run teardown and report it as
              // a non-restartable stop.
              teardown_caught(actor, recipe).await;
              return Incarnation::Stopped;
            }
          }
          Err(_panic) => {
            // `ack` was moved into `handle_with_policy` and dropped on the
            // unwind, so it is already unreported (lost). Record the crash so this
            // otherwise-silent loss is observable: a *transient* rebuild bumps no
            // `died`, so without this a flapping node would look healthy and the
            // dropped in-flight at-most-once delivery would vanish uncounted. Then
            // discard the actor and restart — no teardown on the poisoned
            // instance. Carry the crashing delivery's `attempts` so the supervisor
            // charges the budget only on a first attempt (mechanism B): a
            // re-delivery crash is the message's fault, not the node's, so it
            // spares the budget.
            health.record_crash();
            tracing::error!(node = %recipe.node, attempts, "handle panicked; restarting node");
            return Incarnation::Panicked { attempts };
          }
        }
      }

      // A manual force-restart arrived for this live node. Tear the current
      // incarnation down and signal a rebuild. (A *revive* of a parked-dead node
      // never reaches here — the supervisor only awaits `revive` while parked.)
      _ = control.inner.revive.notified() => {
        if control.inner.force.swap(false, Ordering::SeqCst) {
          teardown_caught(actor, recipe).await;
          return Incarnation::ForceRestart;
        }
        // A non-force notify hit a live node (e.g. a redundant revive); ignore
        // and keep serving.
      }
    }
  }
}

/// Record a permanent death and drain the surviving mailbox to the dead-letter
/// sink. Called when the restart budget is exhausted.
fn permanently_dead(
  recipe: &RestartRecipe,
  rx: &mut MailboxRx,
  health: &Health,
  registry: &Weak<Mutex<ActorRegistry>>,
  death_listener: Option<&DeathListener>,
  restarts: u32,
) {
  // Deregister + Health::died + fire the listener — identical to the non-restart
  // death path, so a budget-exhausted node looks the same to the engine.
  crate::runtime::record_death(
    &recipe.node,
    health,
    registry.upgrade().as_ref(),
    death_listener,
  );

  // Drain whatever is still queued: the node is dead and its router entry is
  // gone, so these bystander messages would otherwise be lost. Preserve them on
  // the dead-letter sink (reason NodeDied) if one was granted; absent a sink,
  // they drop (each `Complete` ack closing → the feeder retries), matching the
  // no-sink count-and-drop fallback elsewhere.
  while let Ok(delivery) = rx.try_recv() {
    dead_letter_one(recipe, delivery, restarts);
  }
}

/// Dead-letter one bystander delivery to a permanently-dead node: preserve it on
/// the sink (reason [`NodeDied`]) if one was granted, else drop it (its ack
/// closing — a `Complete` reads as lost and retries on the feeder, a `Health`
/// ack is uncounted). The node died; it did not "handle" the message, so no
/// synthetic outcome is reported. Shared by the budget-exhaustion drain and the
/// parked-node reaper (a stray delivery that raced deregistration).
///
/// [`NodeDied`]: fuchsia_transport::DeadLetterReason::NodeDied
fn dead_letter_one(recipe: &RestartRecipe, delivery: Delivery, restarts: u32) {
  let Delivery {
    msg,
    ack,
    correlation,
    ..
  } = delivery;
  if let Some(sink) = recipe.dead_letter.as_deref() {
    crate::runtime::record_dead_letter(
      sink,
      DeadLettered::new(
        msg,
        correlation,
        // Cold death-drain path — an owned id per drained message for the sink. A
        // real `String` clone, paid only when a node dies with a sink and a
        // backlog (or a stray late arrival).
        recipe.node.clone(),
        DeadLetterReason::NodeDied { restarts },
      ),
    );
  }
  // The ack is dropped: a `Complete` reads as lost (the feeder retries — it
  // can't land on a dead node, so it eventually dead-letters on its own side), a
  // `Health` ack is uncounted. We do not report a synthetic outcome.
  drop(ack);
}

/// Re-register a revived node in the runtime's address book so `deliver`
/// resolves it again. The engine separately re-registers it in its router (the
/// death listener dropped it there); the runtime side is restored here so the
/// two views stay consistent. Best-effort on a poisoned lock / dropped runtime.
fn reregister(
  recipe: &RestartRecipe,
  tx: &WeakMailboxTx,
  health: &Arc<Health>,
  stopping: &Arc<AtomicBool>,
  registry: &Weak<Mutex<ActorRegistry>>,
) {
  // Upgrade the weak sender — on revival the engine still holds a strong sender
  // in its restart handle, so the channel is open and this succeeds. If it
  // fails, the engine has dropped the node (removed the graph), so there's
  // nothing to revive.
  let Some(tx) = tx.upgrade() else {
    return;
  };
  let Some(registry) = registry.upgrade() else {
    return;
  };
  let Ok(mut registry) = registry.lock() else {
    return;
  };
  // Rebuild a routable handle from the surviving mailbox sender, sharing the same
  // health + stop flag the supervisor already reads, so a later `stop` is honest.
  // Cold revive path; the id/type clones match every other registry insert.
  // Refcount bumps of the sender / health / flag.
  registry.insert(ActorHandle::new_sharing(
    recipe.node.clone(),
    recipe.type_name.clone(),
    tx,
    Arc::clone(health),
    Arc::clone(stopping),
  ));
}
