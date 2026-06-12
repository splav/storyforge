# Combat Engine Architecture (moved)

This document was split into focused files under `docs/combat/`:

- **[`docs/combat/engine.md`](combat/engine.md)** — pure engine internals (`step`, `CombatState`, `ContentView`, determinism contract).
- **[`docs/combat/bridge.md`](combat/bridge.md)** — `src/combat/bridge/` boundary, ECS projection, system schedule.
- **[`docs/combat/pipeline.md`](combat/pipeline.md)** — system chain, EndTurn ownership, edge cases, animation blocking.
- **[`docs/combat/lifecycle.md`](combat/lifecycle.md)** — combat start/end, bootstrap, restart, dynamic spawn.

For navigation, see [`docs/architecture.md`](architecture.md).
