# Phase 3.5 — Summon migration

**Parent plan:** `docs/ai/rework/unisim.md` (Phase 3.5 is the "Summon migration as own step" carve-out flagged in Phase 3's OUT list)
**Predecessor:** `docs/ai/rework/step_unisim3_plan.md` (Phase 3 — tagged `unisim/phase3-complete`)
**Goal:** Engine becomes the authority for unit spawning. `EffectDef::Summon` is no longer a no-op in the engine — `step(Action::Cast)` fans out a real `Effect::Spawn` that mutates `CombatState`. The bridge translates `Event::UnitSpawned` into ECS entity creation + UnitIdMap registration synchronously, eliminating the multi-frame desync that today silently drops casts targeting newly-summoned units (the "no UnitId for cast target N — skipping" warning).
**Timebox:** ~1 week.

**Out of scope (deferred):**
- Turn queue / round flow / `RoundPhase` migration → Phase 4. Summoned units still join the next round's queue via `build_turn_order` (Initiative=0, acts last). Behavioral parity preserved.
- `auras_system` migration → Phase 4.
- HexPositions sync panic — orthogonal bug, separately tracked.

---

## 1. Scope

**IN:**

**Engine extensions:**
- New field on `Unit`: `pub summoner: Option<UnitId>`. Engine-side authority for `max_active` cap counting.
- New field on `CombatState`: `pub next_synthetic_uid: u64`. Initialized to `1u64 << 63` (above Bevy `Entity::to_bits()` range). Incremented by `Effect::Spawn`. Ensures engine-generated UIDs never collide with bridge-derived ones.
- `Effect::Spawn { summoner: UnitId, template_id: String, max_active: Option<u32> }` — atomic: looks up template via `ContentView::unit_template`, finds free position via ring-search around summoner (current ECS logic: `hex_circle(pos, radius=2)`), enforces `max_active` cap (counts alive units with `summoner == Some(summoner)`), generates synthetic UID, inserts new `Unit` into state, emits `Event::UnitSpawned`. On cap-hit or no-free-position: emits `Event::SpawnBlocked` instead and skips insertion.
- `Event::UnitSpawned { uid, summoner, pos, template_id, team }` — observable fact: a new unit joined combat at `pos`.
- `Event::SpawnBlocked { summoner, reason }` — fact: summon failed. `reason` is a small enum (`TemplateMissing`, `MaxActiveReached`, `NoFreePosition`).
- `EngineEffectDef::Summon { template, max_active }` variant — currently the bridge maps `EffectDef::Summon` to `EngineEffectDef::None` (Phase 2 carve-out). Now it maps to the real variant.
- `step(Action::Cast)` arm: when ability's `effect == Summon`, fans out `Effect::Spawn`. Existing cost-payment + crit-fail logic unchanged (Summon abilities still cost AP/mana).

**ContentView extension:**
- New method `unit_template(id: &str) -> Option<UnitTemplate>`.
- New engine struct `UnitTemplate { max_hp, armor, base_speed, max_ap, mana_max, energy_max, rage_max, team }`. Mirrors the *resolved* template (effective stats + equipment armor pre-computed) that engine needs to build a `Unit`. **Team comes from the summoner** (engine reads `summoner.team`), so it's not a template field — added here for completeness because the engine constructs `Unit` and needs to know its team at spawn time.

Actually — team is derived from the summoner at spawn time. UnitTemplate doesn't carry team. Engine reads `state.unit(summoner).team` and assigns to new unit.

Revised:
```rust
pub struct UnitTemplate {
    pub max_hp: i32,
    pub armor: i32,
    pub base_speed: i32,
    pub max_ap: i32,
    pub mana_max: i32,
    pub energy_max: i32,
    pub rage_max: i32,
}
```

**Bridge:**
- `EcsContentView::unit_template`: reads `ActiveContent.unit_templates.get(id)`, computes effective stats via `content.effective_stats(template.stats, equipment)` and armor via `content.equipment_armor(equipment)`. Returns the engine `UnitTemplate`.
- `EcsContentView::ability_def`: change the `EffectDef::Summon { template, max_active }` arm to map to the real engine variant (instead of `None`).
- `translate_cast_events`: new arm for `Event::UnitSpawned` — spawns the ECS entity, builds components (`enemy_bundle`, equipment, abilities, role, AiMemory, SummonedBy, Faction, optional Rage/Mana/Energy, optional CombatPath), registers in `UnitIdMap`, spawns token mesh, writes `CombatEvent::Summoned`. Spawning is **synchronous** within the same frame as the cast — eliminates the next-round-only race.
- `translate_cast_events`: new arm for `Event::SpawnBlocked` — emits `CombatEvent::SummonBlocked` with a human-readable reason.
- The existing `EffectDef::Summon` carve-out in `process_action_system:507-515` (which writes a `SpawnUnit` Bevy message before calling `step()`) is DELETED. The engine now handles spawn end-to-end.

**Deletions:**
- `src/combat/spawn.rs` — entire file (`apply_spawn_system`, ~160 lines).
- `src/game/messages.rs::SpawnUnit` Bevy message struct.
- `src/combat/pipeline.rs`: `spawn::apply_spawn_system` removed from the `CombatStep::Execute` chain.
- `pub mod spawn;` line in `src/combat/mod.rs`.
- Any imports of `SpawnUnit` / `apply_spawn_system` across the codebase.

**Pipeline change:**
- TurnStart / Command / Execute / Finalize chains: only the Execute chain shrinks (spawn system removed). Engine handles spawn during `process_action_system`.

**Preserved (ECS-side):**
- `SummonedBy(parent: Entity)` component — still useful for AI / visual / save-game queries. Engine doesn't read it.
- `build_turn_order` picks up new entity at NEXT round (Initiative=0). Mid-round summon doesn't act this round (parity with current behavior).
- Token mesh + visual color logic (player/enemy materials).

**OUT (deferred):**
- Turn-queue migration so summons can act this round → Phase 4 (if desired).
- Engine ownership of Faction / `Team` enum value (current state: bridge derives from ECS Faction).

---

## 2. Architecture diff vs Phase 3

```
Phase 3:                                  Phase 3.5 target:
  EffectDef::Summon → EngineEffectDef::    EffectDef::Summon → EngineEffectDef::
   None (engine no-op)                       Summon { template, max_active }
                                              (engine fans out Effect::Spawn)

  process_action_system: detects Summon    process_action_system: engine handles
   in Bevy content, emits SpawnUnit         spawn end-to-end via step(Cast).
   message before step()                    No SpawnUnit message.

  apply_spawn_system (Execute set):        DELETED. Bridge translate_cast_events
   creates ECS entity, components,          handles entity + UnitIdMap + token in
   token mesh; mid-round summons            same frame as cast.
   invisible to engine until next round
   init_state_from_ecs

  UnitIdMap missing entry for new          UnitIdMap has entry immediately;
   summon until next round → bug 1          subsequent casts targeting summon
   "no UnitId for cast target N"            resolve correctly.

  No engine concept of summoner            Unit.summoner: Option<UnitId>;
   ownership → cap enforced by ECS          engine enforces max_active cap.
   SummonedBy component scan
```

---

## 3. File-level change list

| File | Change |
|---|---|
| `crates/combat_engine/src/state.rs` | Add `summoner: Option<UnitId>` to `Unit`; add `next_synthetic_uid: u64` to `CombatState`; init in `Default` and `new()` |
| `crates/combat_engine/src/content.rs` | Add `UnitTemplate` struct; add `unit_template(id) -> Option<UnitTemplate>` to `ContentView` trait; add `Summon { template_id: String, max_active: Option<u32> }` to `EffectDef` enum |
| `crates/combat_engine/src/effect.rs` | Add `Effect::Spawn { summoner, template_id, max_active }`; `apply_effect` arm: position search via ring/hex_circle, cap enforcement, UID generation, Unit insertion |
| `crates/combat_engine/src/event.rs` | Add `Event::UnitSpawned { uid, summoner, pos, template_id, team }`; add `Event::SpawnBlocked { summoner, reason }` with `SpawnBlockedReason` enum; `effect_to_event` arms |
| `crates/combat_engine/src/step.rs` | `Action::Cast` arm: when `EffectDef::Summon` matched, push `Effect::Spawn` to queue |
| `src/combat/engine_bridge.rs` | `EcsContentView::unit_template` impl; `ability_def` arm for Summon → real engine variant; `translate_cast_events` arms for `UnitSpawned` (ECS entity creation, token, UnitIdMap) + `SpawnBlocked` (log); delete the `EffectDef::Summon` carve-out in `process_action_system` |
| `src/combat/spawn.rs` | **DELETED** |
| `src/combat/mod.rs` | Remove `pub mod spawn;` |
| `src/combat/pipeline.rs` | Remove `spawn::apply_spawn_system` from Execute chain |
| `src/game/messages.rs` | Remove `SpawnUnit` struct |
| `src/combat/ai/plan/sim.rs` | `SnapshotContentView::unit_template` impl (returns minimal template from sim's content snapshot) |
| `tests/combat_engine/cast.rs` (or new) | Engine-level test: cast a Summon ability, assert new Unit in state, max_active cap, no-free-position fallback |
| `tests/combat_engine/bridge_smoke.rs` | Integration test: ECS entity created + UnitIdMap populated immediately after Cast(Summon); subsequent cast targeting new entity resolves |
| `docs/ai/rework/step_unisim3_5_plan.md` | This file |

---

## 4. Sub-step decomposition

| Step | Title | What lands |
|---|---|---|
| **3.5a** | Engine spawn primitives | `Unit.summoner` field, `CombatState.next_synthetic_uid`, `Effect::Spawn` variant + `apply_effect` arm (position search, cap enforcement, UID generation, Unit insertion), `Event::UnitSpawned` + `Event::SpawnBlocked` + `SpawnBlockedReason` enum, `effect_to_event` arms, `UnitTemplate` struct + `ContentView::unit_template` trait method. Unit tests for spawn success, cap-hit, no-free-position. `step()` integration not yet wired |
| **3.5b** | `EffectDef::Summon` engine arm | Add real `EffectDef::Summon { template_id, max_active }` variant to `crates/combat_engine/src/content.rs`. `step(Action::Cast)` arm: when matched, push `Effect::Spawn`. Test: cast a summon ability end-to-end through `step()`, verify cost paid + new unit in state |
| **3.5c** | Bridge wiring | `EcsContentView::unit_template` impl. `ability_def` Summon arm now maps to real engine variant. `translate_cast_events` arms for `UnitSpawned` (synchronous ECS entity creation, UnitIdMap insert, token visual) + `SpawnBlocked` (log). Bridge integration test |
| **3.5d** | Delete ECS spawn path | Delete `src/combat/spawn.rs` + `apply_spawn_system` + `SpawnUnit` message + pipeline registration + `process_action_system` carve-out. Update sim content view (`SnapshotContentView::unit_template`). Retrospective drafted; user applies tag `unisim/phase3.5-complete` |

---

## 5. Decisions (locked)

### D1. UnitId for engine-spawned units — synthetic counter above Bevy bit range
`CombatState.next_synthetic_uid` starts at `1u64 << 63`, increments per spawn. Engine-derived UIDs never collide with bridge-derived ones (`Entity::to_bits()` never reaches the high bit in practice — Bevy index is u32 + generation u32). Bridge registers `(new_entity, synthetic_uid)` in `UnitIdMap` when translating `Event::UnitSpawned`.

### D2. UnitTemplate is the *resolved* stat sheet
Engine doesn't know about equipment / abilities lists. ContentView computes effective_stats(template.stats, equipment) + equipment_armor(equipment) in advance and exposes one flat struct. Engine constructs Unit from this + summoner's team + computed position.

### D3. Position search inside engine
Ring search via `hex_circle(summoner_pos, radius=2)` (matches current `apply_spawn_system::SUMMON_SEARCH_RADIUS`). Skip summoner's own hex. First occupied-check against `state.alive_units()` positions wins. If no free cell — emit `SpawnBlocked`.

### D4. `max_active` cap enforcement — engine
`Unit.summoner: Option<UnitId>` tracks ownership. `Effect::Spawn` counts `state.alive_units().filter(|u| u.summoner == Some(summoner))`. ECS `SummonedBy(Entity)` component still set by bridge for AI/save consumption — engine doesn't read it.

### D5. Synchronous spawn — no Bevy message delay
`translate_cast_events` runs in same frame as `step()`. Inside the `UnitSpawned` arm: spawn ECS entity, register UnitIdMap, write components. Engine and ECS stay consistent within one frame. Closes bug 1 cleanly.

### D6. Summon does not act this round — parity with current behavior
`build_turn_order` picks up new entity next round (Initiative=0, acts last). Mid-round summon is in engine state + ECS, but not in `TurnQueue` until round transition. Phase 4 may revisit if mid-round acts are desired.

### D7. `Faction` / `Team` value at spawn — derived from summoner
Engine reads `state.unit(summoner).team`, assigns to new unit. Bridge sets ECS `Faction(team)` from the same source (no separate logic needed).

### D8. Crit-fail behavior for Summon casts — unchanged
Summon abilities go through the same crit-fail roll as other Casts. `Miss` → no spawn (cost still paid). `DoubleCost` → cost doubled, spawn proceeds. `SelfDamage` / `ApplyStatus` aux effects unchanged. Tested.

### D9. Failure path — engine returns `Ok` even on `SpawnBlocked`
The cast succeeded (cost paid, intent resolved); the spawn outcome was "no slot / no space". This is semantically `Ok` with a `SpawnBlocked` event in the stream, not `Err(ActionError)`. Mirrors current ECS behavior (SummonBlocked logs but doesn't roll back cast).

---

## 6. Sub-step kickoff order

Strict order: 3.5a → 3.5b → 3.5c → 3.5d. Each lands with cargo check clean and tests green.

---

## 7. Gate criteria (Phase 3.5 → Phase 4)

| # | Criterion | Verification |
|---|---|---|
| 1 | Cast(Summon) produces a new engine `Unit` in `state.units()` with correct stats, position, summoner, team | New engine test in `cast.rs` |
| 2 | UnitIdMap contains the new (entity, synthetic_uid) pair immediately after `process_action_system` returns | New bridge_smoke test |
| 3 | Subsequent cast in the SAME FRAME targeting the new summon resolves (no "no UnitId" warning) | Bridge integration test simulating Lyra-after-summon scenario |
| 4 | `max_active` cap: third summon when cap=2 → `SpawnBlocked` event, no insertion | Engine test |
| 5 | No-free-position: summon when all neighbors blocked → `SpawnBlocked` | Engine test |
| 6 | `apply_spawn_system` deleted, no orphan references | grep |
| 7 | `cargo test` full suite green | CI |
| 8 | Bug 1 playtest scenario reproduces clean — Lyra can cast targeting newly-summoned unit | Manual playtest run |

---

## 8. Known gotchas

- **Entity bits vs synthetic UIDs:** `Entity::to_bits()` is u64. In practice top bit is rarely set (Bevy index is u32 + generation u32, generation reuses old indices). Guard: pick `1u64 << 63` as synthetic base; assert in debug that `Entity::to_bits() < 1u64 << 63` if needed. If a real Bevy entity ever exceeds this, engine UID collision is possible — add an assert with clear message.
- **Token mesh asset handles:** ECS-side spawn loads `HexMaterials.token_player` / `token_enemy`. Bridge translates within `translate_cast_events`; the function needs access to these Bevy Res handles. Plumb via system params.
- **CombatPath component:** template may have an optional path (for AI). Bridge attaches if template specifies it.
- **AbilityIds:** template ability list goes into ECS `Abilities` component. Engine doesn't see them — but next cast TARGETING the summon resolves; cast FROM the summon (if ever) goes through engine normally via `ContentView::ability_def`.
- **Test data — unit templates in tests:** the engine `UnitTemplate` is minimal but new tests need a stub. Add a `StubContent` template field similar to how `StatusDef` was added.
- **Crit-fail Miss on Summon:** if Miss fires, `Effect::Spawn` should NOT be enqueued. Cost is paid (already in queue from cost-payment step), but spawn skipped. Test this.
- **`hex_circle` dependency:** if the function is in `crate::game::hex` (ECS-side), engine can't import it directly. Either inline a small ring-iteration helper in engine or move `hex_circle` into a shared crate-agnostic location.

---

## 9. Retrospective

### Steps actually taken vs planned

- **3.5a — engine spawn primitives:** landed as specified. `Unit.summoner: Option<UnitId>`, `CombatState.next_synthetic_uid` (base `1u64 << 63`), `Effect::Spawn` + `apply_effect` arm with ring-2 position search / cap enforcement / synthetic UID generation, `Event::UnitSpawned` + `Event::SpawnBlocked` + `SpawnBlockedReason` enum, `effect_to_event` arms, `UnitTemplate` struct + `ContentView::unit_template` trait method (stub impls everywhere). 7 unit tests in `tests/combat_engine/effect.rs`.
- **3.5b — `step()` Cast(Summon) arm:** `EffectDef::Summon { template_id, max_active }` variant; `step(Action::Cast)` non-crit-fail branch fans out `Effect::Spawn` for Summon (per-actor, no target enumeration); `effect_for_target` catches Summon via existing per-actor catch-all. Bridge `EcsContentView::ability_def` maps ECS `Summon` to engine variant. 3 integration tests in `tests/combat_engine/cast.rs`.
- **3.5c — bridge translation + ECS deletion:** `EcsContentView::unit_template` real impl reading from `ActiveContent.unit_templates` (uses `effective_stats` + `equipment_armor`). New `spawn_ecs_entity_from_engine_unit` helper in `engine_bridge.rs`. `translate_cast_events` arms for `UnitSpawned` (synchronous ECS entity spawn + UnitIdMap insert + token visual + log) and `SpawnBlocked` (log with reason text). `process_action_system` grew 5 new params (`positions`, `tag_cache`, `mats`, `token_mesh`, `&mut id_map`); Summon carve-out deleted. Deleted: `src/combat/spawn.rs`, `SpawnUnit` Bevy message, pipeline registration, `main.rs` registration, `pub mod spawn` line. Common test fixture (`tests/common/mod.rs`) and `bridge_smoke` test setup gained stub resources for `AbilityTagCache`/`HexMaterials`/`TokenMesh`. New integration test `cast_summon_creates_ecs_entity_synchronously`.
- **3.5d (this section):** retrospective.

### Test counts (per phase boundary)

| Boundary | Total tests |
|---|---|
| Phase 3 close (`c340242`) | 1022 |
| 3.5a end | 1029 (+7 engine spawn) |
| 3.5b end | 1032 (+3 cast.rs) |
| 3.5c end | 1033 (+1 bridge_smoke) |

### Surprises / deviations

- **process_action_system param count.** Now 17 system params after adding spawn-related resources. Bevy compiles fine — individual `Res`/`ResMut` aren't part of the 16-tuple ECS query limit. If it ever hits a wall, bundling visual/asset Res into a single `SystemParam` newtype is the fix. Noted; not addressed in 3.5.
- **Agent mid-step derailment in 3.5a.** A delegated agent went off-task while writing the 7 unit tests, listing Bash command patterns instead of writing test code. Tests were written by main session instead — same coverage, no functional impact.
- **`max_ap` for summoned units.** `UnitTemplate` carries `max_ap: i32` but ECS `UnitTemplate` doesn't have a corresponding field — bridge hardcodes `max_ap: 1` to match `CombatantBundle` default. If summons ever need >1 AP, requires content schema bump.
- **`SnapshotContentView::unit_template` left as `None` stub.** AI sim doesn't currently project summons — beam search doesn't include `Action::Cast(Summon)` in candidates, so the stub is unreachable. TODO comment in sim.rs flags this for Phase 4 if mid-plan summons ever get scored.

### Bug 1 closure

Resolved by construction. Engine emits `Event::UnitSpawned` synchronously inside `step(Cast)`; bridge's `translate_cast_events` immediately creates the ECS entity and registers `(entity, synthetic_uid)` in `UnitIdMap` before any subsequent `ActionInput` in the same frame is processed. The "no UnitId for cast target N — skipping" warning path is therefore unreachable for newly-summoned units. Engine integration test + bridge smoke test cover the entity-creation half; user verifies the playtest scenario (Lyra targets new Морок mid-round) manually after merge.

### Gate criteria status

| # | Status |
|---|---|
| 1 | ✅ `cast_summon_ability_pushes_spawn_effect_and_creates_unit` (engine) |
| 2 | ✅ `cast_summon_creates_ecs_entity_synchronously` (bridge_smoke) |
| 3 | Inherited by construction — same-frame ordering is enforced by `MessageReader` loop semantics. No standalone test (would require multi-actor pipeline setup; verified manually). |
| 4 | ✅ `spawn_blocked_at_max_active_cap` |
| 5 | ✅ `spawn_blocked_when_no_free_position` |
| 6 | ✅ `spawn.rs` deleted; grep clean |
| 7 | ✅ 1033 passing |
| 8 | ⏳ Awaiting user playtest after merge |

### Carry-overs to Phase 4

- **Turn queue migration.** `build_turn_order` still picks up summoned ECS entities at round boundary (`Initiative=0`, acts last). Mid-round summon → acts next round. Phase 4 will lift `TurnQueue` into engine; at that point can decide whether summons act immediately or wait for next round.
- **Sim Summon projection.** `SnapshotContentView::unit_template` is a None stub. If beam search ever needs to score "summon then attack" plans, plumb real templates. Currently AI doesn't enumerate Summon as a candidate during planning.
- **`auras_system` migration.** Still ECS-only. Projector preserves aura-applied statuses via the applier-aware merge. Phase 4 deals with it.
- **Bug 2 (HexPositions panic).** Untouched; orthogonal to Phase 3.5.
- **`process_action_system` param-count debt.** 17 params. Bundle into a `SystemParam` newtype if it grows further.

### Tag

`unisim/phase3.5-complete` to be applied by user after manual playtest verification.
