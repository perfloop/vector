# RFC DRAFT - Split Component Lifecycle into Four Distinct Phases

`TransformConfig::build()` currently conflates structural validation, environment validation, pure
construction, and task spawning into a single method. This RFC proposes splitting it into four
explicit lifecycle phases — `validate_structure`, `validate_environment`, `build`, and `start` —
to make `vector validate` reliable, prevent resource leaks on topology reload rollback, and
simplify unit testing.

## Context

- Immediate motivation: PR fixing `vector validate --no-environment` silently skipping VRL/condition
  errors, which required ~540 lines to work around without a clean trait contract.
- The workaround introduced `validate_env()` (a parallel method) and guard clauses on
  `TransformContext::key` — symptoms of the underlying entanglement.

## Scope

### In scope

- `TransformConfig` trait: introduce `validate_structure`, `validate_environment`, and `start`;
  redefine `build` as pure, synchronous construction.
- Update `TopologyPiecesBuilder` and `vector validate` to call each phase at the right point.
- Migrate all existing transforms.

### Out of scope

- `SourceConfig` and `SinkConfig` — same pattern applies but deferred.
- Changes to user-visible configuration format or component behavior.

## Motivation

- `vector validate` has no clean way to "check VRL without starting threads." The current workaround
  (stub enrichment tables, `validate_env()`, `context.key` guards) must be replicated per-transform.
- `build()` spawns background tokio tasks before a topology reload is committed. If the reload is
  rolled back, those tasks leak.
- Testing transform logic requires spinning up background machinery because construction and startup
  are inseparable.
- The `build()` signature gives no signal about whether an implementation is safe to call
  speculatively (during validation) or whether it has observable side effects.

## Proposal

### User Experience

No user-visible change. `vector validate` and `vector validate --no-environment` behave the same
externally; the difference is that `validate` now exercises the same VRL compilation path as normal
startup rather than a separate, potentially divergent one.

### Implementation

Four phases replace the current monolithic `build()`:

```rust
pub trait TransformConfig: ... {
    /// Phase 1 — pure structural checks (reserved output names, duplicate route keys,
    /// invalid sample rates). No context, no I/O. Called during config compilation on
    /// both `vector validate` and normal startup.
    fn validate_structure(&self) -> Result<(), Vec<String>> { Ok(()) }

    /// Phase 2 — environment-dependent checks. Compile VRL, build conditions, resolve
    /// enrichment table references against stub (validate) or real (startup) resources.
    /// Returns () — build() recompiles from the same context. Double-compilation is an
    /// accepted tradeoff; if it becomes a problem an artifact return can be added later.
    async fn validate_environment(&self, cx: &TransformContext) -> Result<(), Vec<String>>;

    /// Phase 3 — pure, synchronous construction. Receives context; produces a Transform.
    /// No task spawning, no I/O. Safe to discard on topology rollback.
    /// Needing `async` here is a design smell — startup logic has leaked into construction.
    fn build(&self, cx: &TransformContext) -> crate::Result<Transform>;
}

impl Transform {
    /// Phase 4 — startup. Spawns background tasks, opens connections, registers metrics.
    /// Lives on Transform, not TransformConfig: startup is a runtime concern on the built
    /// value, not a configuration concern. Called only after the topology diff is committed.
    async fn start(self, cx: &TransformContext) -> RunningTransform { ... }
}
```

**Call sites:**

| Call site | Phases invoked |
|---|---|
| `vector validate --no-environment` | `validate_structure` |
| `vector validate` | `validate_structure` + `validate_environment` (with stubs) |
| Normal startup / reload (pre-commit) | `validate_structure` + `validate_environment` (real resources) + `build` |
| Normal startup / reload (post-commit) | `start` |

**Migration:**

1. Add `validate_environment`, `build` (sync), and `start` as new required methods with a blanket
   adapter that delegates all three to the existing async `build()` for un-migrated components.
2. Migrate transforms one at a time, starting with `remap` (VRL) and `filter` / `route` (conditions).
3. Update `TopologyPiecesBuilder` to invoke phases at the appropriate points.
4. Update `vector validate` to call `validate_environment` with stub enrichment tables; remove the
   `validate_env()` workaround method.
5. Remove the blanket adapter once all transforms are migrated.

## Alternatives

- **Keep the current approach and add more per-transform workarounds.** Already proven insufficient
  — the fix PR added hundreds of lines of guard logic with no improvement to the trait contract.
- **Return a compiled artifact from `validate_environment` to avoid double-compilation.** Adds
  type-system complexity (GAT vs. `Box<dyn Any>`) for a cost that has not been measured. Deferred.
- **Merge `validate_environment` into `build` and call `build` speculatively.** This is the status
  quo and is the root of the problem; it conflates side-effect-free compilation with construction.

## Outstanding Questions

1. Should un-migrated components that use the blanket adapter be tracked with a lint or a GitHub
   tracking issue to ensure migration completes?
2. Should `start` dispatch via the `Transform` enum directly, or should each variant's inner type
   implement a `Startable` trait that `Transform::start` delegates to?

## Plan Of Attack

- [ ] Spike: add `validate_environment` + sync `build` + `start` to `TransformConfig` with blanket
      adapter; confirm it compiles and existing tests pass.
- [ ] Migrate `remap` transform (VRL compilation into `validate_environment`).
- [ ] Migrate `filter`, `route`, `exclusive_route` (condition building).
- [ ] Migrate remaining transforms (`reduce`, `sample`, `throttle`, `delay`, `window`).
- [ ] Update `TopologyPiecesBuilder` to call phases at correct commit points.
- [ ] Update `vector validate` path; remove `validate_env()` workaround.
- [ ] Remove blanket adapter; make all three methods required with no default.

## Future Improvements

- Apply the same four-phase contract to `SourceConfig` and `SinkConfig`.
- If double-compilation is measured to be costly, introduce a typed artifact return from
  `validate_environment` to pass compiled programs directly into `build`.
