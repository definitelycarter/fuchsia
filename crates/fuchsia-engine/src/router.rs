use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use fuchsia_actor::{ActorId, DEFAULT_PORT, Emit, Message, OutputPorts};
use fuchsia_transport::{Ack, Delivery, Health, MailboxTx, Offer};

use crate::error::EngineError;

/// The reserved error port. Always accepted by `add_edge` (even on a `Fixed`
/// node that does not list it) so the failure-handling branch can be wired
/// before that machinery exists. See node failure handling.
const ERROR_PORT: &str = "error";

/// One outgoing edge: which actor a port's emission reaches. A *named struct*,
/// not a bare `ActorId`, so a dest-side `to_port` (named **input** ports, the
/// dual of this RFC) can be added as a field later without a breaking change to
/// the routing table or `add_edge`'s shape.
#[derive(Debug, Clone)]
struct Edge {
  to: ActorId,
}

/// Per-`(node, port)` route outcome tallies, bucketed so *no* routing outcome —
/// including an emit to an unwired port — is silent. Atomic, so they are bumped
/// while only the shared (read) routing lock is held: emits stay concurrent and
/// the routing path takes **no** counter-specific lock. In-process counters
/// only (not an event stream).
#[derive(Debug, Default)]
struct PortCounters {
  /// Offered into a successor's mailbox and buffered.
  delivered: AtomicU64,
  /// A successor's mailbox was full, so the delivery was dropped
  /// (at-most-once shedding).
  shed: AtomicU64,
  /// No edge resolved for this port — an unwired port, or every successor's
  /// mailbox is gone. Counted, never a silent early return.
  no_route: AtomicU64,
}

/// A snapshot of one port's counters — what the read accessor returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RouteCounts {
  pub delivered: u64,
  pub shed: u64,
  pub no_route: u64,
}

impl PortCounters {
  fn snapshot(&self) -> RouteCounts {
    RouteCounts {
      delivered: self.delivered.load(Ordering::Relaxed),
      shed: self.shed.load(Ordering::Relaxed),
      no_route: self.no_route.load(Ordering::Relaxed),
    }
  }
}

/// One node's route counters: a per-port table plus a per-node `fallback`.
///
/// The per-port `Arc<PortCounters>` are pre-created on the **cold** paths
/// (`register` for a `Fixed` node's declared ports + the always-emittable
/// `"out"`/`"error"`; `add_edge` for any wired port), so the routing path only
/// *looks them up* under the shared read lock and does a lockless `fetch_add` —
/// no per-emit lock, no per-emit allocation. The one case with no pre-created
/// port counter — a `Dynamic` node emitting on a port that was never declared
/// *and* never wired — falls to `fallback`, so its `no_route` is still recorded
/// (per-node granularity, but never silent).
#[derive(Default)]
struct NodeCounters {
  ports: HashMap<String, Arc<PortCounters>>,
  fallback: Arc<PortCounters>,
}

impl NodeCounters {
  /// The counter for `port`, falling back to the per-node counter when no
  /// per-port one was pre-created. Borrowed `&str` lookup — no allocation.
  fn counter(&self, port: &str) -> &Arc<PortCounters> {
    self.ports.get(port).unwrap_or(&self.fallback)
  }

  /// Ensure a per-port counter exists for `port` (idempotent). Cold path only
  /// (`register` / `add_edge`).
  fn ensure_port(&mut self, port: &str) {
    if !self.ports.contains_key(port) {
      self.ports.insert(port.to_owned(), Arc::default());
    }
  }
}

/// The engine's live routing table: who the successors of each `(node, port)`
/// are, how to reach every node's mailbox, what ports each node declares, and a
/// running tally of route outcomes. Mutable so graphs can be added or torn down
/// without re-instantiating actors (lookup, not baked wiring).
#[derive(Default)]
pub(crate) struct RouterState {
  // Nested by source then port — the inner map probes by `&str` on the hot
  // path with no per-emit allocation, where a flat `(ActorId, Port)` tuple key
  // could not be looked up from `(&ActorId, &str)` without constructing the
  // owned key each time.
  edges: HashMap<ActorId, HashMap<String, Vec<Edge>>>,
  targets: HashMap<ActorId, (MailboxTx, Arc<Health>)>,
  // Each node's declared output ports, recorded at `add_node`, so `add_edge`
  // can validate a source port against the node's interface.
  ports: HashMap<ActorId, OutputPorts>,
  // Per-node route counters, pre-created on the cold paths so `route` only
  // looks them up (under the read lock) and bumps an atomic.
  counters: HashMap<ActorId, NodeCounters>,
}

impl RouterState {
  pub(crate) fn register(
    &mut self,
    id: ActorId,
    mailbox: MailboxTx,
    health: Arc<Health>,
    ports: OutputPorts,
  ) {
    // Pre-create the node's counters: every node can always emit on `"out"` and
    // `"error"`, and a `Fixed` node has its declared ports too. Wired-but-not-
    // declared ports (a `Dynamic` node's edges) are added at `add_edge`; an
    // emit on a port that is neither declared nor wired falls to `fallback`.
    let mut node_counters = NodeCounters::default();
    node_counters.ensure_port(DEFAULT_PORT);
    node_counters.ensure_port(ERROR_PORT);
    if let OutputPorts::Fixed(declared) = &ports {
      for port in declared {
        node_counters.ensure_port(port);
      }
    }

    // Cold path (one call per `add_node`); the id keys the port-declaration,
    // target, and counter maps, so the clones are unavoidable.
    self.counters.insert(id.clone(), node_counters);
    self.ports.insert(id.clone(), ports);
    self.targets.insert(id, (mailbox, health));
  }

  /// Validate a source `port` against `from`'s declared ports. A `Fixed` node
  /// must list the port (plus the always-allowed `"out"` and reserved
  /// `"error"`); a `Dynamic` node — or one with no recorded declaration —
  /// accepts any port, since its ports exist only at emit time.
  fn validate_port(&self, from: &ActorId, port: &str) -> Result<(), EngineError> {
    if port == DEFAULT_PORT || port == ERROR_PORT {
      return Ok(());
    }
    match self.ports.get(from) {
      Some(OutputPorts::Fixed(declared)) if !declared.iter().any(|p| p == port) => {
        Err(EngineError::UnknownPort {
          node: from.clone(),
          port: port.to_owned(),
        })
      }
      _ => Ok(()),
    }
  }

  pub(crate) fn add_edge(
    &mut self,
    from: ActorId,
    port: &str,
    to: ActorId,
  ) -> Result<(), EngineError> {
    self.validate_port(&from, port)?;
    // Pre-create the wired port's counter (cold path) so `route` finds a
    // per-port counter for it — covers a `Dynamic` node's wired ports, which
    // `register` couldn't know about. `register` always runs before `add_edge`,
    // so the node's `NodeCounters` exists.
    if let Some(node_counters) = self.counters.get_mut(&from) {
      node_counters.ensure_port(port);
    }
    self
      .edges
      .entry(from)
      .or_default()
      .entry(port.to_owned())
      .or_default()
      .push(Edge { to });
    Ok(())
  }

  pub(crate) fn target(&self, id: &ActorId) -> Option<&(MailboxTx, Arc<Health>)> {
    self.targets.get(id)
  }

  pub(crate) fn ids_in_group(&self, group: &str) -> Vec<ActorId> {
    self
      .targets
      .keys()
      .filter(|id| id.group() == group)
      .cloned()
      .collect()
  }

  /// Drop a group's targets, the edges it owns (an edge belongs to its source
  /// node's group, so its whole inner port map goes), its port declarations,
  /// and its counters. Cross-group edges *into* this group are left to resolve
  /// to nothing — a graceful drop, handled by `route`.
  pub(crate) fn remove_group(&mut self, group: &str) {
    self.targets.retain(|id, _| id.group() != group);
    self.edges.retain(|from, _| from.group() != group);
    self.ports.retain(|id, _| id.group() != group);
    self.counters.retain(|id, _| id.group() != group);
  }

  /// Read the route counters for one `(node, port)` — zeroed if nothing has
  /// routed there yet. For tests/observability. A port with no pre-created
  /// counter (a `Dynamic` node's never-declared, never-wired port) reads as
  /// zero here; its routed-but-unwired emissions land on the node's `fallback`
  /// and so don't appear under that exact port name.
  pub(crate) fn route_counts(&self, node: &ActorId, port: &str) -> RouteCounts {
    self
      .counters
      .get(node)
      .and_then(|node_counters| node_counters.ports.get(port))
      .map(|c| c.snapshot())
      .unwrap_or_default()
  }

  /// Read a node's per-node **fallback** counters — the bucket for emissions on
  /// a port that was neither declared nor wired (a `Dynamic` node's ad-hoc
  /// ports). Zeroed if the node is unknown. For tests/observability.
  pub(crate) fn route_counts_fallback(&self, node: &ActorId) -> RouteCounts {
    self
      .counters
      .get(node)
      .map(|node_counters| node_counters.fallback.snapshot())
      .unwrap_or_default()
  }

  /// Deliver `msg` to each successor of `(source, port)`. Channel transport, so
  /// a full mailbox sheds (at-most-once); every outcome is tallied on the
  /// `(source, port)` counter (or the node's `fallback` for a never-declared,
  /// never-wired port), including the unwired-port case. Takes `&self`, so it
  /// runs under the *read* side of the outer router lock — and takes **no**
  /// counter-specific lock — so emits from different actors stay concurrent.
  fn route(&self, source: &ActorId, port: &str, msg: Message) {
    // The counter is a pure lookup under the read lock (pre-created on the cold
    // paths); the per-node `fallback` covers a port that was neither declared
    // nor wired, so nothing goes silent without a hot-path lock. A node with no
    // `NodeCounters` at all (only possible mid-teardown) skips counting.
    let counter = self.counters.get(source).map(|nc| nc.counter(port));

    // Resolve the port's successors. A missing source or a port with no edge is
    // a no-route — counted, not silent.
    let successors = match self.edges.get(source).and_then(|ports| ports.get(port)) {
      Some(successors) if !successors.is_empty() => successors,
      _ => {
        if let Some(counter) = counter {
          counter.no_route.fetch_add(1, Ordering::Relaxed);
        }
        return;
      }
    };

    for edge in successors {
      let outcome = match self.targets.get(&edge.to) {
        // The target was torn down (a graceful cross-group drop) — count it as
        // a no-route for this edge.
        None => Offer::Closed,
        Some((tx, health)) => {
          let ack = Ack::Health(Arc::clone(health));
          // Clone the message per successor: a port may fan out to several
          // edges, each needing its own copy. Matches the prior fan-out path.
          tx.offer(Delivery::new(msg.clone(), ack))
        }
      };
      if let Some(counter) = counter {
        match outcome {
          Offer::Delivered => counter.delivered.fetch_add(1, Ordering::Relaxed),
          Offer::Shed => counter.shed.fetch_add(1, Ordering::Relaxed),
          // A closed target delivered nothing — a no-route for this edge.
          Offer::Closed => counter.no_route.fetch_add(1, Ordering::Relaxed),
        };
      }
    }
  }
}

/// The `emit` sink handed to one actor. On emit, it looks the source's
/// successors up in the shared router and delivers — the actor stays
/// neighbor-ignorant; the engine owns the addressing.
pub(crate) struct RoutedEmit {
  pub(crate) source: ActorId,
  pub(crate) router: Arc<RwLock<RouterState>>,
}

impl Emit for RoutedEmit {
  fn emit_to(&self, port: &str, msg: Message) {
    // A poisoned router lock means a prior panic mid-mutation; drop rather
    // than propagate (emit is infallible and best-effort on this path). The
    // shared (read) lock keeps concurrent emits from serializing.
    if let Ok(state) = self.router.read() {
      state.route(&self.source, port, msg);
    }
  }
}
