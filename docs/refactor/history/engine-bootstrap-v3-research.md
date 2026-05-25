# Engine Bootstrap V3 — Research Report (3.1)

**Статус**: research завершён. Отчёт Plan-агента по шагу 3.1 из [engine-bootstrap-v3.md](engine-bootstrap-v3.md).

V1 (fix `EcsContentView::status_bonuses` stub) уже в working tree (не закоммичен).
V2 (content-aware `from_ecs`) — рекомендован, но не сделан. Решение: **сплавить V2 в шаг 3.2** (cohesive change, V3 всё равно меняет signature `from_ecs` через bootstrap).

---

## Блок A: каталог мутаций engine `CombatState` в runtime

Engine state живёт в `CombatStateRes` ([src/combat/engine_bridge.rs:65-69](../../../src/combat/engine_bridge.rs:65)). Источник истины — `find_referencing_symbols(CombatStateRes)` + targeted re-scan `engine_bridge.rs` и `combat_engine` crate.

### A.1 Production ECS-side mutations of `CombatStateRes`

| # | File:line | Caller | What it mutates | Kind |
|---|---|---|---|---|
| 1 | `engine_bridge.rs:1561` (`init_state_from_ecs`) | system: `OnEnter(CombatPhase::AwaitCommand)` | `combat_state.0 = state;` — full replace (units, idx, round=ctx.round, phase=ActorTurn, turn_queue, caster_context per unit, aoo_dice, auras, enemy_phases). Gated by `combat_context.round < 2`. | **Authoritative bridge init** (the one V3 wants to delete) |
| 2 | `engine_bridge.rs:1560` (inside `init_state_from_ecs`) | same | `state.set_turn_queue(uid_order, ecs_queue.index)` before assigning to `combat_state.0` | part of #1 |
| 3 | `engine_bridge.rs:1588` (`engine_start_first_turn_system`) | system: `OnEnter(CombatPhase::AwaitCommand)` chained after #1 | `combat_state.0.start_actor_turn(first_actor, &content)` — AP/MP refill, mana/energy regen, status tick for the queue head. Gated `combat_context.round == 1`. | **Authoritative bridge** (round-1 priming because engine's `Effect::AdvanceTurn` cascade hasn't fired yet) |
| 4 | `engine_bridge.rs:730-799` (`process_action_system` — `ActionInput::Move`) | system: `CombatStep::Execute` | `step(&mut combat_state.0, Action::Move, ...)` | Engine-step (canonical mutator path) |
| 5 | `engine_bridge.rs:801-877` (`process_action_system` — `ActionInput::Cast`) | same | `step(&mut combat_state.0, Action::Cast, ...)` + conditional auto-end-turn `step(...Action::EndTurn...)` at line 855 | Engine-step |
| 6 | `engine_bridge.rs:879-901` (`process_action_system` — `ActionInput::EndTurn`) | same | `step(&mut combat_state.0, Action::EndTurn, ...)` | Engine-step |
| 7 | `engine_bridge.rs:1617` (`reset_engine_mirrors`, called from `reset_engine_mirrors_on_exit_combat` line 1622-1629 and `reset_engine_mirrors_on_restart` line 1631-1646) | `OnExit(AppState::Combat)` system and `Update` reader of `RestartCombat` | `*combat_state = CombatStateRes::default()` — wipe to empty | Lifecycle teardown |

### A.2 Test-only mutations (engine_bridge wires them through `run_system_once(init_state_from_ecs)`)

| # | File:line | What |
|---|---|---|
| T1 | `tests/common/mod.rs:152-164` `init_engine_state(&mut app)` → `run_system_once(init_state_from_ecs)` | rebuild from ECS after spawn (used by `movement_app` callers) |
| T2 | `tests/combat_engine/bridge_smoke.rs:130-140` `init_bridge_engine_state(&mut app)` → `run_system_once(init_state_from_ecs)` | same |
| T3 | `tests/combat_engine/legality_parity.rs:142-146` `init_bridge_engine_state` → `run_system_once(init_state_from_ecs)` | same |
| T4 | `tests/engine_step_range_correlation.rs:100-105` `seed_engine` → `run_system_once(init_state_from_ecs)` | same |
| T5 | `tests/combat_engine/state.rs:17-42` `run_from_ecs` — wraps `from_ecs` directly via `SystemState`. Does **not** go through `init_state_from_ecs`. | unit test of the lower-level builder (V2's signature change touches this) |
| T6 | `tests/combat_engine/bridge_smoke.rs:883, 1355, 1432, 1578` (`projector_*` tests) | `app.world_mut().resource_mut::<CombatStateRes>().0 = CombatState::new(...)` — explicit synthetic state for projector tests. Doesn't go through bridge. | direct synthesis |
| T7 | `tests/combat_engine/bridge_smoke.rs:894, 991, 1082, 1162, 1266, 1360, 1437, 1583, 1681, 1756, 1866` | `combat_state.0.unit_mut(uid).unwrap().action_points = N` etc — directly poking state for cast-test setup | test-only `unit_mut` writes after init |

### A.3 Mutations inside `combat_engine::step` (informational — correct mutations, not bridge concerns)

- `Effect::BumpRound` at `crates/combat_engine/src/effect.rs:669-673`: `state.round += 1; state.start_round(content)` — owns 2→3, 3→4 etc, **but not 0→1**.
- `Effect::Spawn` at `crates/combat_engine/src/effect.rs:693-776` then `state.insert_unit(new_unit)` at `:770`. New unit created with `caster_context: default(), aoo_dice: None, auras: empty, enemy_phases: empty` (line 764-767). **This is the dynamic-spawn hole** (V3 Q4).

### A.4 ECS-side `CombatContext.round` mutations (dual-owner problem, V3 #4)

| # | File:line | Caller | What |
|---|---|---|---|
| C1 | `src/combat/mod.rs:44` `start_combat_system` | `MessageReader<StartCombat>` in `Overworld` | `ctx.round = 0` |
| C2 | `src/combat/turn_order.rs:35` `build_turn_order` | `CombatPhase::StartRound` chained after `project_state_to_ecs` and `assign_hex_positions` | `ctx.round += 1` — does the 0→1, 1→2, 2→3 increments |
| C3 | `src/scenario/combat_scene.rs:117-132` `reset_combat_state` (from `spawn_combat_scene:136-153` and `restart_combat_system:213-272`) | `OnEnter(AppState::Combat)` (via `spawn_combat_scene`) and `RestartCombat` reader | `ctx.round = 0; ctx.encounter = None` |

### Summary of A

Bridge имеет ровно **3 production authoritative-mutation points** (init #1, first-turn-prime #3, teardown #7) и один canonical engine-step path (#4-6). Все test callsites pre-V3 идут через `init_state_from_ecs`. Hidden status-application / per-unit mutations вне engine step нет — confirmed via `find_referencing_symbols` on `CombatStateRes`.

---

## Блок B: ответы на 4 открытых вопроса

### B.1 — Где bootstrap-точка?

**Текущая chain (reconstructed)**:

```
RUN-TIME
  1. start_combat_system (Update, AppState::Overworld) → ctx.round = 0; next.set(AppState::Combat)
  2. OnEnter(AppState::Combat) → spawn_combat_scene
        ├── spawn_combatants (creates ECS entities with Combatant + StatusEffects::default())
        ├── spawn_background
        └── reset_combat_state (ctx.round = 0; log; cursors; anim queue)
  3. (Bevy default sub-state CombatPhase::StartRound is entered automatically when AppState::Combat begins)
  4. CombatPhase::StartRound (Update, run_if):
        ├── project_state_to_ecs       (writes empty engine state — no-op, units missing from id_map)
        ├── assign_hex_positions       (token sprites)
        └── build_turn_order           (ctx.round += 1  =>  1; rolls initiative; queue.order populated; next_phase.set(AwaitCommand))
  5. OnEnter(CombatPhase::AwaitCommand) chain (currently — pre-V3):
        ├── init_state_from_ecs        (combat_state.0 = from_ecs(...); set_turn_queue(...))
        ├── engine_start_first_turn_system  (start_actor_turn for queue.current — round==1 gate)
        └── write_engine_trace_init_system  (trace ↻)
  6. CombatStep::TurnStart → Command → Execute → Finalize loops until end.
```

**Рекомендуемая bootstrap-точка**: one-shot системой в конце `CombatPhase::StartRound` chain после `build_turn_order` (либо альтернатива — `OnExit(CombatPhase::StartRound)`).

**Обоснование**:
- Combatants заспавнены в step 2 — ECS уже содержит юнитов к step 4.
- `build_turn_order` накатывает initiative, заполняет `queue.order` + `queue.index = first_alive_idx`, ставит `ActiveCombatant`. Bootstrap'у эти данные нужны.
- Запуск как finaltail-система в `StartRound` chain (после `build_turn_order`) — НЕ как отдельный `OnEnter(AwaitCommand)` — устраняет round-gate guard полностью. Phase transition гарантирует one-shot semantics естественно.
- Закрывает subtle bug: на round 2+, `OnEnter(AwaitCommand)` срабатывает снова, сейчас гейтится `>= 2`. После V3 — не нужно.

**Отвергнутая альтернатива**: `OnEnter(CombatPhase::StartRound)` с `round == 0` guard. На round 2+ когда engine re-enters StartRound via `RoundStarted` event, `ctx.round` set by либо `build_turn_order` либо translate-layer — fragile.

**Naming**: `bootstrap_combat_state` как Bevy *system* (не free function — нужен SystemParam для query'ов).

### B.2 — Кто инкрементит round 0→1 после V3?

**Рекомендация: оставить `build_turn_order` ownership; bootstrap читает результат.**

Reasons:
- `build_turn_order` уже владеет initiative rolls, queue construction, log push, `ActiveCombatant` insertion. Все ECS-side concerns. Перенос round increment расщепил бы функцию.
- Engine `state.round` независим — `CombatState::new(units, round=1, phase, seed=0)` принимает round параметром. Bootstrap просто передаёт `ctx.round` (= 1 после build_turn_order).
- НЕ хотим engine emit synthetic `RoundStarted` на bootstrap — duplicate log entry.
- Implication: `build_turn_order` comment на line 36-41 остаётся точным.

Dual-owner #4 в V3 acknowledged но **не** unified — это V4 если когда-нибудь.

### B.3 — Что с `engine_start_first_turn_system`?

**Свернуть в `bootstrap_combat_state`.** Bootstrap runs at end of `StartRound` chain *after* `build_turn_order` (который заполняет ECS `TurnQueue`):

1. `from_ecs(..., &active_content)` (V2 content-aware signature — fold-in V2 here).
2. Populate per-unit fields (caster_context/aoo_dice/auras/enemy_phases — текущий блок 1475-1557 of init_state_from_ecs).
3. `state.set_turn_queue(uid_order_from_ecs, index_from_ecs)`
4. `state.start_actor_turn(state.turn_queue.current().unwrap(), &content)` — collecting events.
5. Translate those events (tick + phase queueing) — то что делает `engine_start_first_turn_system`.
6. `combat_state.0 = state`.

**Resulting ordering в `CombatPhase::StartRound` chain**:

```rust
.add_systems(
    Update,
    (
        project_state_to_ecs,                              // round 2+ engine→ECS projection (no-op on round 1)
        ui::hex_grid::assign_hex_positions,
        turn_order::build_turn_order,                       // ctx.round += 1; queue.order; queue.index; ActiveCombatant; next.set(AwaitCommand)
        engine_bridge::bootstrap_combat_state,             // NEW: full engine bootstrap; runs only when ctx.round == 1 OR equivalent first-time guard
        ai::log::write_engine_trace_init_system,           // trace init (был в AwaitCommand chain; релокейтит сюда чтобы trace видел bootstrap state)
    )
        .chain()
        .run_if(in_state(CombatPhase::StartRound)),
)
```

`OnEnter(CombatPhase::AwaitCommand)` chain становится **пустым** — удалить `(init_state_from_ecs, engine_start_first_turn_system, write_engine_trace_init_system).chain()` registration entirely (`src/combat/pipeline.rs:34-37`).

**Один sequencing risk**: `build_turn_order` зовёт `next_phase.set(CombatPhase::AwaitCommand)` на line 114. В Bevy 0.18 state-transitions флешатся только между schedule passes (не mid-chain) → `run_if(in_state(StartRound))` продолжит returning true для остальных систем в chain. Precedent: `assign_hex_positions` уже chained after `build_turn_order` и работает.

**Internal guard в bootstrap**: `if !combat_state.0.units().is_empty() { return; }` — idempotent re-entry. Работает и для production (round 2+ → engine state уже заполнен), и для тестов.

### B.4 — Dynamic spawn

`Effect::Spawn` (`combat_engine/src/effect.rs:693-776`) создаёт `Unit { caster_context: default(), aoo_dice: None, auras: empty, enemy_phases: empty, ... }`. Bridge `spawn_ecs_entity_from_engine_unit` (`engine_bridge.rs:463-555`) создаёт ECS entity из engine unit — но никогда round-trips back в engine `Unit` для per-combat populate.

**Сегодняшние последствия (pre-existing bugs, not V3-introduced)**:
- Summoned creatures с melee `WeaponAttack` ability + weapon не AoO (aoo_dice = None).
- Summoned creatures с `unit_template` carrying aura definition теряют aura (но `unit_template` сейчас не имеет aura field).
- Summoned creatures с `enemy_phases`: `unit_template` сейчас phases не несёт — vacuously fine но feature blocker.
- `caster_context.str_mod` / `int_mod` / `spell_power` / `weapon_dice` / `crit_fail_outcome` = 0. Summoned creature кастующий любой spell — zero spell power → **probable real bug today** если summon-template имеет cast ability.

**Рекомендация: V3 ships explicit scope-out с TODO + caveat test.**

Reasons:
- V3's stated goal — "remove `init_state_from_ecs` from round-cycle". Dynamic-spawn fix — это другой refactor (probably content-side: `UnitTemplate` в `combat_engine::content` должен grow `caster_context`/`aoo_dice`/`enemy_phases`/`auras` fields, и `Effect::Spawn` populates them).
- Coupling V3 to dynamic-spawn — `combat_engine` API changes (UnitTemplate signature), bumps risk profile с "lifecycle reshuffle" на "engine-content API churn".
- **Caveat regression test** belongs в V3 для документирования gap'а: assert summoned unit имеет `caster_context == default()` и `aoo_dice.is_none()`. Когда dedicated fix лэндится — test flips assertions.

V3.7 в current draft становится: "document gap, add ticket reference + caveat test". No code change в engine.

---

## Блок C: implementable декомпозиция 3.2-3.7

**Critical assumption**: V2 либо лэндится перед V3, либо folds в шаг 3.2 (рекомендуется — cohesive change).

### Sequencing rationale

| Sub-step | Depends on | Why this order |
|---|---|---|
| 3.2 (extract `bootstrap_combat_state` + fold V2) | nothing prior | Defines new function shape; everything downstream consumes it |
| 3.3 (turn_order changes + pipeline rewire) | 3.2 in place | Need bootstrap callable so chain can be rewired |
| 3.4 (delete old systems + guard + stale TODOs) | 3.2 + 3.3 done | Removing old path before tests migrated breaks builds |
| 3.5 (test harness migration) | 3.4 — atomic с 3.4 в одном PR | Tests still reference `init_state_from_ecs` until rewritten |
| 3.6 (regression test second combat) | 3.5 ready | Test consumes new bootstrap |
| 3.7 (dynamic spawn caveat) | 3.2 (knows engine API shape) | Documentation + caveat test; doesn't change engine code |

### Step 3.2 — Extract `bootstrap_combat_state` system (+ fold V2)

**File changes** (line numbers — pre-change anchors):

- `src/combat/engine_bridge.rs`:
  - **Sig change `from_ecs`** (V2 fold-in): `from_ecs(combatants, positions, round, id_map, content: &ActiveContent) -> CombatState`. Внутри после построения units пройти по каждому, пересчитать `armor_bonus`/`speed_bonus`/`damage_taken_bonus` суммируя через `ContentView::status_bonuses` + `status_def.damage_taken_bonus`.
  - **Add** новый `pub fn bootstrap_combat_state(...)` system на ~line 1437 (replacing `init_state_from_ecs` location). Signature:
    ```rust
    pub fn bootstrap_combat_state(
        combatants: Query<CombatantRow, With<Combatant>>,
        positions: Res<HexPositions>,
        combat_context: Res<CombatContext>,
        ecs_queue: Res<TurnQueue>,
        mut id_map: ResMut<UnitIdMap>,
        mut combat_state: ResMut<CombatStateRes>,
        caster_q: Query<...>,
        aoo_q: Query<...>,
        aura_q: Query<...>,
        phases_q: Query<...>,
        active_content: Res<ActiveContent>,
        mut commands: Commands,
        mut log: ResMut<CombatLog>,
        mut pending_phases: ResMut<PendingPhaseTransitions>,
    )
    ```
  - Body: guard `if !combat_state.0.units().is_empty() { return; }`. Internally:
    1. `from_ecs(...)` (V2 sig).
    2. Per-unit caster_context / aoo_dice / auras / enemy_phases populate (move existing 1475-1557 block).
    3. `state.set_turn_queue(...)` (existing 1559-1560).
    4. If `state.turn_queue.current().is_some()`: fold-in `engine_start_first_turn_system` body (start_actor_turn + translate_tick_events + pending_phases pushes).
    5. `combat_state.0 = state`.
  - **Delete** `init_state_from_ecs` (lines 1437-1562).
  - **Delete** `engine_start_first_turn_system` (lines 1564-1597).
  - **Delete** stale comments (`engine_bridge.rs:160-167`, `:211, 213, 216`, `:1456-1471` — see step 3.4).

**Risk**: Bevy query-conflict если `commands` + caster_q/aoo_q overlap. Verify `cargo check` first. Probably fine (commands writes via deferred ops).

### Step 3.3 — `build_turn_order` adjustments + pipeline rewire

**Files**:
- `src/combat/turn_order.rs`: **NO changes needed**. Stays ECS-side authority для round increment, queue.order, queue.index, ActiveCombatant, CombatLog::RoundStarted push.
- `src/combat/pipeline.rs:46-60`: append `bootstrap_combat_state` и `write_engine_trace_init_system` к `StartRound` chain после `build_turn_order`. Delete `OnEnter(CombatPhase::AwaitCommand)` chain (lines 34-37) entirely.

**Test added**: `tests/combat/bootstrap.rs` (new file) — assert после `StartRound` chain runs: `ctx.round == 1` AND `combat_state.0.round == 1` AND `combat_state.0.turn_queue.order` matches ECS `queue.order`.

### Step 3.4 — Delete dead code + stale TODOs

**Deletions**:
- `engine_bridge.rs:160-167` — stale "Phase 0 deferred to step 8+" doc на `from_ecs` (V2 handled `armor_bonus: 0`).
- `engine_bridge.rs:211, 213, 216` — `armor_bonus: 0` / `damage_taken_bonus: 0` / `speed: speed.0` comments "deferred to step 8+".
- `engine_bridge.rs:1456-1471` — long `if combat_context.round >= 2 { return; }` comment block ("history of guards"). Obsolete.
- `engine_bridge.rs:14-21` (file-level doc) — references to `init_state_from_ecs` на `OnEnter(AwaitCommand)` → update to "bootstrap on StartRound exit".
- `src/combat/turn_order.rs:36-41` — comment **остаётся accurate**; do NOT delete.

### Step 3.5 — Test harness migration

| # | File:line | Current call | Migration |
|---|---|---|---|
| T1 | `tests/common/mod.rs:152-164` `init_engine_state` | `run_system_once(init_state_from_ecs)` | Rename body to `run_system_once(bootstrap_combat_state)`. Verify `movement_app` already inserts CombatLog + PendingPhaseTransitions (line 96-112 — yes). |
| T2 | `tests/combat_engine/bridge_smoke.rs:130-140` `init_bridge_engine_state` | same | same |
| T3 | `tests/combat_engine/legality_parity.rs:142-146` `init_bridge_engine_state` | same | same |
| T4 | `tests/engine_step_range_correlation.rs:100-105` `seed_engine` | same | same |
| T5 | `tests/combat_engine/state.rs:17-42` `run_from_ecs` | Direct `from_ecs(...)` via `SystemState` | Add `ActiveContent` to SystemState tuple (V2 sig change). |

**Additional callsite (V2 plan missed)**:
- `tests/common/mod.rs:138-141` `movement_app` adds `init_state_from_ecs` to `OnEnter(AwaitCommand)`. Must be **removed entirely** — `enter_await_command` is called before unit spawning in tests, so OnEnter fire on empty world is wasted; tests already explicitly call `init_engine_state(app)` after spawning.

**Risk**: bootstrap depends on `PendingPhaseTransitions`. Audit confirms all 5 test apps already `init_resource::<PendingPhaseTransitions>()`. No additional setup.

### Step 3.6 — Regression test "two combats in same App session"

**Existing**: `tests/combat/handoff.rs:374-442` `combat_2_starts_clean_after_combat_1` — validates teardown only.

**New test** (same file): `combat_2_bootstraps_fresh_after_combat_1`:
1. `movement_app()`, spawn 2 units, `init_engine_state(&mut app)` (= bootstrap). Assert engine has 2 units.
2. Run несколько `process_action_system` cycles to mutate state.
3. Trigger `reset_engine_mirrors_on_exit_combat` via `run_system_once`. Assert engine empty.
4. Despawn ECS entities, spawn 2 fresh different units.
5. `init_engine_state(&mut app)` again. Assert: (a) engine has new entities by UID, (b) `combat_state.0.units().len() == 2` (no stale tombstones), (c) `combat_state.0.round == ctx.round`, (d) `combat_state.0.turn_queue.order` matches ECS TurnQueue.
6. Run one action — assert succeeds (catches stale id_map regressions).

### Step 3.7 — Dynamic spawn caveat (no code change)

- File-level comment в `engine_bridge.rs` `spawn_ecs_entity_from_engine_unit` (~line 463): "TODO(dynamic-spawn): caster_context/aoo_dice/auras/enemy_phases on summoned units are zero/empty — see ticket XXX".
- New test `tests/combat/dynamic_spawn_bootstrap_caveat.rs` (или в handoff.rs): spawn summoner с Spawn ability, summon unit, assert `combat_state.0.unit(new_uid).caster_context == default()` AND `aoo_dice.is_none()` — *документирует* current behavior. Comment: "When dynamic-spawn fix lands, this test should flip — see TODO".

---

## Топ-3 риска

| # | Риск | Обоснование | Митигация |
|---|---|---|---|
| 1 | **Bootstrap fires before ECS state-flush completes between systems in chain** | `build_turn_order` does `next_phase.set(CombatPhase::AwaitCommand)`. В Bevy 0.18 state changes flush at next state-transition pass (between schedules), не mid-chain → `run_if` reads *old* state for rest of chain. Verifiable: `assign_hex_positions` уже chained after `build_turn_order` и работает. Но fragile если Bevy internals change. | Cross-check Bevy 0.18 state docs; add integration test full single-tick `StartRound` chain end-to-end. If at risk, register bootstrap to `OnExit(CombatPhase::StartRound)` instead of in-chain — guaranteed one-shot post-chain. |
| 2 | **Second-combat regression via PresetInitiative path** | `restart_combat_system` (combat_scene.rs:213-272) saves initiative в `PresetInitiative`, despawns + respawns combatants, ставит `next_phase.set(CombatPhase::StartRound)`. `ctx.round` reset to 0 by `reset_combat_state`. Fresh entity IDs. After V3, bootstrap должен handle: engine mirrors уже torn down by `reset_engine_mirrors_on_restart`; bootstrap fires when StartRound chain hits tail; new entities, new UIDs, fresh `id_map`. `units().is_empty()` guard корректно срабатывает. **Но** bootstrap expects `state.turn_queue.current().is_some()` after `set_turn_queue` — depends на `ecs_queue.order` non-empty when bootstrap runs. `build_turn_order` populates just before. OK. Risk window: если any system between build_turn_order и bootstrap clears queue, second combat won't init. | Add debug-assert в bootstrap: `debug_assert!(!ecs_queue.order.is_empty(), "bootstrap requires turn queue populated by build_turn_order");`. 3.6 regression test should catch unobserved teardown sequencing. |
| 3 | **Test harness `movement_app` ordering invariant breaks** | `movement_app` (tests/common/mod.rs:82-150) sets up `OnEnter(AwaitCommand)` с `init_state_from_ecs`, потом зовёт `enter_await_command(&mut app)` (line 149) который fires OnEnter на empty world. Tests потом spawn units и зовут `init_engine_state` explicitly. After V3 — removing OnEnter registration и moving bootstrap to OnExit(StartRound) means `enter_await_command` doesn't fire bootstrap — correct (no units to bootstrap). Но some tests могут rely на subtle side-effect: empty `init_state_from_ecs` currently clears `id_map` (line 1442). Если any test counts на `id_map` clear *before* spawn — invariant disappears. | Audit `id_map.clear()` callsites в tests — confirm none rely на OnEnter pre-clear. `from_ecs` itself does `id_map.clear()` at line 170 → any bootstrap call re-clears. Add to test migration changelog (3.5). |

---

## Открытые вопросы (residual after research)

1. **`write_engine_trace_init_system` placement**: currently runs в `OnEnter(AwaitCommand)` chain. Если expects to see combat_state populated by `init_state_from_ecs` immediately before it в same OnEnter chain — moving bootstrap to OnExit(StartRound) means trace init теперь sees state от one frame earlier — should still be valid но verify. Read `src/combat/ai/log/mod.rs:645-675` `write_engine_trace_init_system` body.

2. **`build_turn_order` round-2+ behavior**: на round 2+ engine already emits RoundStarted и translate_end_turn_events sets `next_phase.set(StartRound)`. Re-enters StartRound chain → `build_turn_order` runs again, increments `ctx.round` to 2, потом `bootstrap_combat_state` runs с `combat_state.0.units().is_empty() == false` → bootstrap is no-op. Confirm intended (i.e., bootstrap one-shot per encounter, not per round).

3. **Should bootstrap's "empty units" guard быть replaced with explicit `bootstrap_done: bool` field on `CombatStateRes`?** `units().is_empty()` heuristic happens to work because of teardown semantics. Explicit flag — defensive but adds state. Recommendation: start с heuristic; если future flow requires re-bootstrapping with units present — add flag.

---

## Critical Files for Implementation

- /Users/splav/personal/storyforge/src/combat/engine_bridge.rs
- /Users/splav/personal/storyforge/src/combat/pipeline.rs
- /Users/splav/personal/storyforge/src/combat/turn_order.rs
- /Users/splav/personal/storyforge/tests/common/mod.rs
- /Users/splav/personal/storyforge/tests/combat/handoff.rs
