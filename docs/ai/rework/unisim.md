# Combat Engine — unified sim/real architecture (project draft)

**Статус:** draft proposal, не утверждено. Кандидат на Wave 5 master plan.

**Цель документа:** описать архитектуру, которая закрывает sim/real drift **by construction** и делает добавление новых механик дешёвым (~3-4 файла вместо ~14 сейчас).

## TL;DR

Combat — это **детерминированный stack-machine с RNG**. Текущая архитектура размазала это по Bevy ECS systems + message bus, что делает sim (AI planner) и real (Bevy runtime) двумя независимыми имплементациями одних и тех же правил. Step 12 потратил 4 сабшага на закрытие 4 drift'ов — это **повторяющийся pattern**, не разовое.

Решение: вынести combat в **отдельный pure-Rust модуль** (`combat-engine`), zero Bevy dependency. Real и sim вызывают одну функцию `step(state, action, dice, content) -> events`. ECS components — projection из canonical `CombatState`, не source of truth.

**Стоимость:** ~10-14 недель incremental migration.
**Выгода:** drift permanently impossible; replay/network/save-load становятся первого класса.

---

## 1. Проблема

### 1.1 Симптомы

Step 12 (см. `step12_plan.md`) — 4 substep'а закрывающие drift между AI sim и real combat:
- **12.1** drift #speed + drift #status — статусы не пересчитывали derived stats в sim после mid-plan apply.
- **12.2** AoO suicide — sim не убивал actor'а на AoO, real убивал.
- **12.3** drift #3 rage — sim не давал rage per damage event; AoO branch — отдельно.
- **12.4** schema bump, golden corpus, docs.

Каждый — 1-2 дня implementation + tests + parity harness. **Это не баги, это структурная стоимость дублирования логики**: real и sim — два независимых кода для одних правил.

### 1.2 Класс bug'ов

| Drift | Что отсутствовало в sim | Когда обнаружили |
|---|---|---|
| #speed | Haste/Slow → `unit.speed` recompute | Mining + scenario review (step 12 prep) |
| #status | armor_bonus/vuln/CC refresh after apply | Static analysis (12.0 design) |
| #3 rage (direct) | Rage gain on damage event | Mining D1 voiceprint mismatch |
| #3 rage (AoO) | Rage gain on AoO hit | Code review during 12.3 |
| AoO suicide | Mid-plan death truncation | Playtest 2026-05-09 road_bridge |
| Forward-model death | `actor_unit_mut()` filter not propagating | Same playtest |

Это **5 mechanic'ов добавили в real → забыли в sim**. Будущие mechanic'и (reactions, environment, telegraph, ...) добавят больше.

### 1.3 Стоимость новой механики (extension-checklist.md)

Сейчас новый `EffectDef` затрагивает 7 файлов:
- `content/abilities.rs` (enum + parser)
- `combat/effects_outcome.rs` (OutcomePrimary)
- `combat/resolution.rs` (writer)
- `combat/ai/plan/sim.rs::apply_primary` (sim mutation)
- `combat/ai/scoring/policy/` (scoring formulas)
- `combat/ai/outcome/builder.rs` (outcome aggregation)
- `combat/ai/role.rs` (role vote)

Из них 3 (resolution, sim, outcome) дублируют семантику эффекта. **Любое расширение enum → drift risk × 3.**

---

## 2. Архитектура

### 2.1 Три различных concept'а

Текущий код смешивает три уровня в один Bevy message bus (`ApplyDamage`, `ApplyStatus`, `RageGained`, ...). Они **семантически разные**:

| Concept | Кто создаёт | Кто читает | Granularity | Пример |
|---|---|---|---|---|
| **Action** | Player / AI | Engine | Coarse — intent | `Cast { actor: A, ability: fireball, target: T }` |
| **Effect** | Engine internal | Engine state mutator | Atomic — single state mutation | `Damage { target: B, raw: 6 }`, `GainRage { target: A }` |
| **Event** | Engine emit | Observers (UI, log, replay) | Domain-level — facts | `UnitDamaged { B, 6 }`, `AbilityResolved { A, fireball, [B,C] }` |

**Action ≠ Event.** Action — намерение, Event — факт. Effect — реализация. Текущий код не различает; в новой архитектуре эта дифференциация — keystone.

### 2.2 Канонический shape

```rust
// crate::combat_engine (или src/combat_engine/, no Bevy dep)

pub struct CombatState {
    pub units: HashMap<UnitId, Unit>,     // indexed for O(1) lookup
    pub turn_queue: TurnQueue,
    pub round: u32,
    pub phase: RoundPhase,                // PreRound, ActorTurn, EndRound
    pub random_seed: u64,                 // for replay reproducibility
}

pub struct Unit {
    pub id: UnitId,
    pub team: Team,
    pub pos: Hex,
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32,
    pub base_speed: i32,
    pub speed: i32,            // derived = base_speed + Σ speed_bonus
    pub action_points: i32,
    pub movement_points: i32,
    pub reactions_left: i32,
    pub statuses: Vec<ActiveStatus>,
    pub rage:   Option<(i32, i32)>,
    pub mana:   Option<(i32, i32)>,
    pub energy: Option<(i32, i32)>,
    // ... derived caches
}

pub enum Action {
    Move    { actor: UnitId, path: Vec<Hex> },
    Cast    { actor: UnitId, ability: AbilityId, target: ActionTarget },
    EndTurn { actor: UnitId },
    // Internal — engine generates, не выходит наружу:
    // ReactionAoO { from: UnitId, victim: UnitId, trigger_pos: Hex },
    // RoundTick   { round: u32 },
}

pub enum Effect {
    // State mutations — atomic:
    PayCost            { actor: UnitId, kind: ResourceKind, amount: i32 },
    Damage             { target: UnitId, raw: f32, source: UnitId, pierces: bool },
    Heal               { target: UnitId, amount: i32 },
    GainRage           { target: UnitId },                  // +1 clamped to max
    ApplyStatus        { target: UnitId, status: StatusId, duration: u32 },
    RemoveStatus       { target: UnitId, status: StatusId },
    MovePosition       { actor: UnitId, to: Hex },
    DecrementMP        { actor: UnitId, by: i32 },
    DecrementReactions { actor: UnitId },
    Death              { unit: UnitId },
    RefreshAggregates  { unit: UnitId },                    // derived stats recompute
    TickDot            { target: UnitId },                  // round-tick
    // ... new mechanics extend here
}

pub enum Event {                                            // observable facts
    ActionStarted     { action: Action },
    UnitMoved         { actor: UnitId, from: Hex, to: Hex },
    UnitDamaged       { target: UnitId, amount: f32, source: UnitId },
    UnitHealed        { target: UnitId, amount: f32 },
    UnitDied          { unit: UnitId },
    RageGained        { unit: UnitId, current: i32, max: i32 },
    ReactionFired     { actor: UnitId, kind: ReactionKind, against: UnitId },
    AbilityResolved   { actor: UnitId, ability: AbilityId, hits: Vec<UnitId> },
    StatusApplied     { target: UnitId, status: StatusId, duration: u32 },
    StatusRemoved     { target: UnitId, status: StatusId },
    ActionFinished    { action: Action },
    // ...
}

pub trait DiceSource {
    fn roll(&mut self, dice: DiceExpr) -> i32;              // real path
    fn expected(&self, dice: DiceExpr) -> f32;              // sim path (no mut)
}

pub fn step(
    state: &mut CombatState,
    action: Action,
    rng: &mut dyn DiceSource,
    content: &ContentView,
) -> Result<Vec<Event>, ActionError>;
```

### 2.3 Внутренний механизм `step()`

Это ключевая часть. Engine — **effect-queue processor** с reaction-aware scheduling:

```
fn step(state, action, rng, content) -> Vec<Event>:
    validate(state, action)?  // returns ActionError if illegal

    let mut effect_queue: VecDeque<Effect> = expand_action(action, state, rng, content)
    let mut events = vec![Event::ActionStarted { action }]

    while let Some(effect) = effect_queue.pop_front():
        // 1) Apply atomic mutation; may produce derived effects
        let derived = apply_effect(state, effect, content)
        
        // 2) Map effect → event (some effects map, some are pure internal)
        if let Some(ev) = effect_to_event(effect, state):
            events.push(ev)
        
        // 3) Enqueue derived effects (e.g., Damage → Death if hp=0; Damage → GainRage source+target)
        for d in derived:
            effect_queue.push_back(d)
        
        // 4) Reaction scan: did this effect trigger any pending reaction?
        for reaction in scan_reactions(state, effect, content):
            events.push(Event::ReactionFired { ... })
            let r_effects = expand_reaction(reaction, state, rng, content)
            for re in r_effects:
                effect_queue.push_back(re)

    events.push(Event::ActionFinished { action })
    Ok(events)
```

**Свойства:**
- **Deterministic** given `(state, action, rng_seed, content)`. Тест-able by replay.
- **Pure** modulo `state` mutation — no I/O, no Bevy, no time.
- **Reactions = effects → effects**. Не специальный case.
- **Death triggers = derived effect от Damage**. Engine sees hp=0 в `apply_effect(Damage)`, pushes `Death`.
- **Status tick = `TickDot` effects fired at RoundPhase::EndRound`**. Same engine, same queue.
- **Effect ordering deterministic**. FIFO queue; ties broken by source order. Tests assert event sequence.

### 2.4 Effect derivation patterns

`apply_effect` для каждого variant возвращает `Vec<Effect>` of derived effects:

| Effect | Direct mutation | Derived |
|---|---|---|
| `Damage { target, raw, source }` | target.hp -= mitigated_damage | `GainRage { source }`, `GainRage { target }`, `Death { target }` if hp=0 |
| `Death` | unit.is_alive = false | `RemoveStatus { target, status }` for each status (cleanup), on-death triggers |
| `ApplyStatus` | unit.statuses.push(status) | `RefreshAggregates { unit }` |
| `RemoveStatus` | unit.statuses.retain(...) | `RefreshAggregates { unit }` |
| `Heal { target, amount }` | cleanse DoT first, then hp += | `RefreshAggregates` if statuses changed |
| `MovePosition { actor, to }` | unit.pos = to | (reactions scanned in step loop, not here) |
| `RefreshAggregates` | recompute speed/armor_bonus/vuln/tags | none |
| `TickDot { target }` | apply DoT damage | `Damage` effect for each tick → cascades |

### 2.5 Reaction model

Reactions = `Action`-like sub-procedures triggered by state changes:

```rust
fn scan_reactions(state: &CombatState, effect: &Effect, content: &ContentView) -> Vec<Reaction> {
    match effect {
        Effect::MovePosition { actor, to } => {
            // AoO scan: enemies adjacent to source/path, not adjacent to dest, reactions_left > 0
            let mut out = vec![];
            for enemy in state.enemies_of(actor).filter(|e| e.reactions_left > 0) {
                if adjacent_was(actor, enemy) && !adjacent_now(to, enemy) {
                    out.push(Reaction::OpportunityAttack { from: enemy.id, victim: actor });
                }
            }
            out
        }
        Effect::Damage { target, .. } if state.unit(target).has_retaliate() => {
            // future: Retaliate counter-damage
        }
        _ => vec![],
    }
}

fn expand_reaction(r: Reaction, state, rng, content) -> Vec<Effect> {
    match r {
        Reaction::OpportunityAttack { from, victim } => vec![
            Effect::DecrementReactions { actor: from },
            Effect::Damage { target: victim, raw: aoo_dice(from), source: from, pierces: false },
            // Derived effects (rage gain x2, possible death) cascade through normal effect queue
        ],
    }
}
```

**Reactions = first-class.** Counterspell, retaliate, mark-of-death, opportunity attack — все через одну машину. Текущий код имеет AoO логику только в `movement_system` и зеркальный код в `apply_move` sim'е; новые reactions потребовали бы пары для real+sim. В engine — один `scan_reactions` + `expand_reaction`.

**Recursion limit:** `step` enforces max effect-queue depth (e.g., 100) to prevent infinite loops от cyclic reactions. Каждая push увеличивает counter; overflow → `ActionError::ReactionDepthExceeded`.

### 2.6 State ownership

**Canonical state = `Res<CombatState>` в Bevy.**

```rust
// Bevy system orchestrator:
fn process_action_system(
    mut state: ResMut<CombatState>,
    mut input: EventReader<ActionInput>,
    mut events_out: EventWriter<CombatEvent>,
    mut rng: ResMut<DiceRng>,
    content: Res<ContentView>,
) {
    for input_action in input.read() {
        match combat_engine::step(&mut state, input_action.clone(), &mut *rng, &content) {
            Ok(events) => {
                for e in events { events_out.send(CombatEvent(e)); }
            }
            Err(err) => log::warn!("illegal action: {err:?}"),
        }
    }
}

// Render projection — read-only consumer of state changes:
fn project_state_to_ecs(
    state: Res<CombatState>,
    mut positions: Query<&mut HexPosition>,
    mut vitals: Query<&mut Vital>,
    // ...
) {
    if !state.is_changed() { return; }
    for (id, unit) in state.units.iter() {
        if let Some(entity) = id_to_entity(id) {
            positions.get_mut(entity).unwrap().0 = unit.pos;
            vitals.get_mut(entity).unwrap().hp = unit.hp;
            // ...
        }
    }
}

// Animations — react to events, не пишут в state:
fn animation_system(
    mut events: EventReader<CombatEvent>,
    mut transforms: Query<&mut Transform>,
    ...
) {
    for ev in events.read() {
        match ev {
            CombatEvent(Event::UnitMoved { actor, from, to }) => {
                /* schedule tween */
            }
            CombatEvent(Event::UnitDamaged { target, amount, .. }) => {
                /* spawn damage popup */
            }
            ...
        }
    }
}
```

**ECS components становятся одностороннюю projection от `CombatState`:**
- Read-only outside `project_state_to_ecs`.
- Bevy systems используют ECS для render/animation/asset/input — не для logic.
- Combat logic не trickle'ит через component mutations.

### 2.7 AI integration

```rust
pub fn pick_action(
    state: &CombatState,
    actor: UnitId,
    ai_world: &AiWorld,
) -> Action {
    let candidates = enumerate_actions(state, actor, ai_world.content);
    let mut best = (f32::NEG_INFINITY, candidates[0].clone());
    
    for action in candidates {
        let mut sim_state = state.clone();
        let mut dice = ExpectedValue::new();
        match combat_engine::step(&mut sim_state, action.clone(), &mut dice, ai_world.content) {
            Ok(events) => {
                let score = score_plan(&sim_state, &events, ai_world);
                if score > best.0 { best = (score, action); }
            }
            Err(_) => continue,
        }
    }
    best.1
}
```

**Sim = clone state + run engine.** Same code path. Drift impossible.

Beam search multi-step:
```rust
fn beam_search(state: &CombatState, actor: UnitId, depth: usize) -> Vec<(Action, Vec<Action>)> {
    let mut frontier = vec![(state.clone(), vec![])];
    for _ in 0..depth {
        let mut next_frontier = vec![];
        for (s, history) in &frontier {
            for action in enumerate_actions(s, actor) {
                let mut s2 = s.clone();
                if step(&mut s2, action.clone(), &mut ExpectedValue, content).is_ok() {
                    let mut h2 = history.clone();
                    h2.push(action);
                    next_frontier.push((s2, h2));
                }
            }
        }
        frontier = prune_top_k(next_frontier, k=BEAM_WIDTH);
    }
    frontier.into_iter().map(|(s, h)| (h[0].clone(), h)).collect()
}
```

### 2.8 Replay / logging

Event stream = log. Replay = re-run engine with same seed:

```rust
// Logging — Bevy system on Event channel:
fn log_combat_events(mut events: EventReader<CombatEvent>, mut logger: ResMut<CombatLog>) {
    for ev in events.read() {
        logger.append(ev);  // JSONL append
    }
}

// Replay tool:
fn replay(log_path: &Path) -> Result<CombatState, ReplayError> {
    let mut state = load_initial_state(log_path)?;
    let mut dice = ReplayDice::new(state.random_seed);
    let actions = parse_action_sequence(log_path)?;
    for action in actions {
        let _events = combat_engine::step(&mut state, action, &mut dice, &content)?;
    }
    Ok(state)
}
```

Replay verifies determinism: final state from log re-run must match logged final state. If not — engine bug or content drift.

---

## 3. What we get

| Capability | Сейчас | После |
|---|---|---|
| **Sim ≡ real** | Drift inevitable | Same code path |
| **Replay correctness** | Trust score formulas pure | Re-run engine; verify identical |
| **New mechanic cost** | ~14 files touched | ~3-4 files (engine extension + content + UI) |
| **Reactions / interrupts** | Ad-hoc in event bus | First-class via effect queue |
| **Network play** | Impossible without rewrite | Server runs engine; events broadcast |
| **Save/load** | Per-component serde | Serialize `CombatState` |
| **Determinism fuzzing** | Cannot | `cargo fuzz` over Action sequences |
| **AI testing** | Mock `BattleSnapshot` | Use canonical `CombatState` |
| **Score formula change** | Touch scoring layer only | Same — orthogonal to engine |
| **Bevy upgrade pain** | High (logic in systems) | Low (engine has no Bevy dep) |

---

## 4. Risks / trade-offs

### 4.1 Migration cost

**~10-14 weeks incremental.** Не all-at-once — per-mechanic migration:

| Phase | Migrate | Closes | Weeks |
|---|---|---|---|
| 1 | `Move` action + AoO reaction | drift on move, AoO suicide | 2 |
| 2 | `Cast` action (damage, heal, status) | drift on damage/heal/status | 3 |
| 3 | Rage / DoT / round tick | drift on rage, DoT, status ticks | 2 |
| 4 | EndTurn, turn queue | turn management | 1 |
| 5 | Replay + log integration | replay first-class | 2 |
| 6 | ECS projection cleanup | remove legacy systems | 2 |
| **Total** | | | **~12 wk** |

Каждая фаза — landed independently, тесты passing, no behavioural drift on the previously-migrated parts.

### 4.2 Lost Bevy idioms

Combat logic больше не использует ECS queries / message bus. Bevy users could протестовать.

**Counter:** Bevy ECS shines for animation, asset, render, input — where dataparallelism + system scheduling matter. Combat — turn-based with ~10 units; ECS performance не critical, и ECS makes logic harder to reason about (system order, query conflicts, message timing).

Bevy stays where it matters; combat moves to a model that fits its actual shape.

### 4.3 Performance of state clone in sim

`CombatState` ≈ current `BattleSnapshot` size (~10KB for 10 units). Sim clones ~50-150 times per `pick_action` (beam search × depth). ~1.5MB allocations per AI turn — same as today.

**If becomes bottleneck:** persistent collections (`im` crate) provide O(1) structural sharing clone. Migration is transparent (just change container types).

### 4.4 Effect ordering bugs

Engine has deterministic queue order. But subtle ordering issues (e.g., when should `Death` fire vs `Damage` to next target in AoE?) are easy to get wrong silently.

**Mitigation:** parity tests for canonical scenarios (existing 8 in `tests/parity.rs` + new ones per migration phase). Each phase: capture canonical sequence, assert exact event order.

### 4.5 Reaction recursion / infinite loops

If counterspell triggers counterspell, etc.

**Mitigation:** engine enforces max queue depth (e.g., 100); overflow → `ActionError::ReactionDepthExceeded`. Real combat already implicitly has this; making it explicit fails fast instead of hanging.

### 4.6 RNG threading

`DiceSource` injected into `step()`. Real uses seeded `DiceRng`; sim uses `ExpectedValue`; replay uses `ReplayDice` (returns recorded rolls in order).

**Risk:** if engine consumes RNG in different order for the same Action across versions, replay breaks. **Mitigation:** lock effect order via tests; document `step()` as RNG-stable interface.

### 4.7 Animation latency

Engine mutates state synchronously; UI projects on next frame. Animations are event-driven, run on their own timeline (already the case today).

**Concern:** state may visually "snap" if animation lags behind. **Mitigation:** UI tracks animation queue, gates next user action until animations complete (already the case in current UI).

---

## 5. Migration plan (locked)

12-week incremental migration. Each phase lands independently with passing tests and zero behavioural drift on previously-migrated parts. Each phase opens with a `step_unisimN_plan.md` (sub-step decomposition) and closes with retrospective notes.

### 5.0 Phase 0 — Steel-thread spike (Week 1)

**Goal:** validate API + projection pattern on smallest meaningful action — `Action::Move`.

**Tasks:**
- Create `src/combat_engine/` module (zero Bevy dep): `state.rs`, `action.rs`, `effect.rs`, `event.rs`, `dice.rs`, `step.rs`.
- Define minimal `Effect`: `MovePosition`, `DecrementMP`, `Damage`, `GainRage`, `DecrementReactions`, `Death`, `RefreshAggregates`.
- Define minimal `Event`: `UnitMoved`, `UnitDamaged`, `RageGained`, `ReactionFired`, `UnitDied`, `ActionStarted`, `ActionFinished`.
- `CombatState` (`Vec<Unit>` + `HashMap<UnitId, usize>` cache, `UnitId(u64)`) added as Bevy `Res`, NOT replacing ECS yet.
- Transitional `mirror_state_from_ecs` populates `CombatState` at frame start (one-way ECS → engine, throwaway after Phase 1).
- Implement `step(state, Action::Move, rng, content)` with AoO via `scan_reactions` + `expand_reaction`.
- Sim's `apply_move` rewrites to call `step()`.
- `DiceSource` trait with explicit `DiceExpr`; Move consumes 0-1 rolls.

**Gate (5/5 → green-light full migration):**
1. Effect variants < 15 for spike scope.
2. `step()` signature stable across Move + AoO; no special-case branches in caller.
3. Bench: `step(Move)` ≤ current `sim::apply_move` × 1.2 on canonical 10-unit scenario.
4. Reaction recursion (AoO chain → AoO victim dies → triggers retaliate stub) handled cleanly, no ad-hoc cases.
5. Phase 1 task list achievable without touching Cast/Status systems.

**Rollback:** drop `combat_engine/` module; sim reverts; spike thrown away.

**Decision point:** 5/5 ✅ → proceed Phase 1. 3-4/5 → revise approach, possibly fall back to Architecture B (events+applier without engine extraction). <3/5 → abort, status quo + per-mechanic parity tests as drift defence.

### 5.1 Phase 1 — Move canonical (Weeks 2-3)

**Goal:** Move is the canonical path on both real + sim. `CombatState` becomes source of truth for movement state.

**Tasks:**
- Replace `movement_system` with `process_action_system` for `Action::Move`.
- ECS `pos`/`movement_points`/`reactions_left` become read-only projection from `CombatState`.
- `MoveUnit` Bevy message removed → `ActionInput::Move`.
- `OpportunityAttack` event removed from `movement_system` → engine emits `ReactionFired`.
- Animation system reacts to `Event::UnitMoved` / `Event::ReactionFired` (not raw component changes).
- AI Move candidates score via engine (no more `apply_move` in sim).
- Drop `mirror_state_from_ecs` — engine writes; ECS reads.

**Gate:** `golden_smoke` green; parity tests (12.2 AoO suicide, AoO chain death, forward-model death) 8/8; no playtest regressions.

### 5.2 Phase 2 — Cast: damage, heal, status apply (Weeks 4-6)

**Goal:** Cast migrated; damage/heal/status flow through engine.

**Tasks:**
- Effects added: `PayCost`, `ApplyStatus`, `RemoveStatus`, `Heal`.
- Events added: `UnitHealed`, `StatusApplied`, `StatusRemoved`, `AbilityResolved`.
- `expand_action(Cast)` reads `AbilityDef` from `ContentView`, fans out per `EffectDef` arm.
- **Behaviour change #1 — per-target ordering (decision 6.3):** AoE applies damage→rage→death per target, then next target. Differs from current all-damages-first-then-rage. Parity test sequences rewritten.
- **Behaviour change #2 — strict failure (decision 6.5):** mid-execution effects on dead targets return `Err(ActionError::TargetGone)`. AI `enumerate_actions` pre-validates target liveness.
- `apply_effects_system` shrinks to a thin shim around engine.
- ECS `hp`/`mana`/`rage`/`statuses` become projection.
- Sim's `apply_cast` removed.
- Playtest snapshot captured pre-Phase-2 + post-Phase-2 to validate behaviour-change scope.

**Gate:** damage/heal/status parity with new expected sequences; 12.1 (speed+status refresh) + 12.3 (rage on damage + AoO) behaviours preserved by construction (drift impossible); AI scoring formulas unchanged and producing comparable agendas.

### 5.3 Phase 3 — Status ticks, DoT, rage details (Weeks 7-8)

**Goal:** EndTurn migration with round-tick mechanics.

**Tasks:**
- Effect added: `TickDot`.
- `Action::EndTurn` migration includes `TickDot` fired at `RoundPhase::EndRound` for each active DoT.
- `status_tick_system` removed.
- AI sim now simulates ticks (closes gap that exists today — sim previously skipped tick effects).

**Gate:** DoT damage parity; status duration decrement parity; AI now scores tick-aware plans (regression check: ranged kiting agendas should improve).

### 5.4 Phase 4 — Turn queue, round flow (Week 9)

**Goal:** round transitions handled by engine.

**Tasks:**
- `RoundPhase` enum (`PreRound`, `ActorTurn`, `EndRound`) lifted into `CombatState`.
- `combat_round_system` becomes orchestrator: read player input or AI decision → `step()` → advance turn.
- `TurnQueue` moves from ECS `Res` into `CombatState` field.

**Gate:** round flow parity from existing tests; AI does not regress on multi-round agendas.

### 5.5 Phase 5 — Replay + log overhaul (Weeks 10-11)

**Goal:** replay first-class.

**Tasks:**
- Combat log = engine `Event` stream (JSONL append-only).
- Replay tool re-runs engine from log; asserts identical final state + event byte-equality.
- Schema bump v37: events-based log; legacy per-stage JSONL tagged.
- `replay_ai_log` + `mine_ai_logs` updated to read new format (or split: legacy reader + new reader during transition).

**Gate:** replay determinism test — re-run from log produces identical final state and event sequence; `cargo fuzz` over Action sequences finds zero engine panics.

### 5.6 Phase 6 — ECS projection cleanup (Week 12)

**Goal:** legacy combat systems removed.

**Tasks:**
- Delete `apply_effects.rs`, `movement.rs`, `status_apply.rs`, `status_tick.rs`.
- ECS components marked `#[engine_projected]` (compile-time check via wrapper newtype or proc-macro that only `project_state_to_ecs` writes them).
- `extension-checklist.md` updated: 7-file checklist → 3-4 (engine variant + content + UI projector).
- Final perf bench vs Phase 0 baseline.

**Gate:** full test suite green; docs current; no behaviour drift since Phase 1 baseline.

### 5.7 Cross-cutting

- **Parity harness** (`tests/parity.rs`) expands every phase — each phase adds canonical scenarios; suite never shrinks.
- **Behaviour-change bookkeeping** (6.3, 6.5):
  - Playtest snapshot before Phase 2 captures pre/post diff.
  - AI scoring left orthogonal — only the substrate changes, not the formulas.
  - Mining run before/after Phase 2 flags any agenda-mix shifts.
- **Risk monitors:**
  - Clone cost — bench at end of Phase 1 (10-unit Move); revisit if > 1.5× current. Fallback: `im` persistent collections (transparent swap).
  - Per-target ordering surprises — track parity test rewrites; if > 2× expected, reconsider 6.3.
  - Replay drift — Phase 5 determinism test as canary.
  - Strict failure surprises — count `ActionError::TargetGone` occurrences in AI mining post-Phase-2; if AI repeatedly trips it, target-liveness pre-validation needs strengthening.

---

## 6. Locked decisions

All technical open questions resolved (2026-05-12). Two answers differ from the doc's prior tentative defaults — flagged as ⚠ **behaviour changes**.

### 6.1 State container — `Vec<Unit>` + id→idx cache

Matches current `BattleSnapshot` pattern. Deterministic iteration order (critical for replay); clone preserves order. Lookup by id via `HashMap<UnitId, usize>` cache rebuilt on insert/remove.

### 6.2 Unit identity — `UnitId(u64)`

New opaque type in `combat_engine`. One-time `Entity ↔ UnitId` mapping established at combat start. Engine has zero Bevy dep.

### 6.3 Effect ordering — per-target ⚠ behaviour change

AoE damages 3 targets: damage → rage → death applied **per target**, then next target. **Differs from current real-pipeline** (current = all-damages-first-then-rage). Parity test sequences rewrite in Phase 2.

**Rationale:** matches reaction model semantics (each target's resolution is self-contained); makes mid-AoE death cascades natural; reduces split-damage/split-rage subtle bugs.

### 6.4 RNG semantics — explicit `DiceExpr` + documented call count

`DiceSource::roll(DiceExpr)` is the only entry point. Engine documents per-action RNG consumption ("Cast on N targets = N rolls in target order"). Replay assertion: RNG consumed exactly N times for action X.

### 6.5 Failure handling — strict throughout ⚠ behaviour change

Pre-validation: illegal action → `Err(ActionError)`, state unchanged. Mid-execution: effect targeting a unit that died from an earlier effect in the queue → `Err(ActionError::TargetGone)`. **Differs from current behaviour** (current = silent skip mid-execution).

**Implication:** AI's `enumerate_actions` must pre-validate target liveness; sim's beam search must not enqueue partially-stale plans. Stricter, but bugs become loud instead of silent.

### 6.6 Engine vs content separation — TOML stays

Abilities, statuses, effects continue to live in `assets/data/*.toml`. Engine reads `AbilityDef`/`EffectDef` via `ContentView` and expands into `Effect` queue per arm. Confirmed unchanged.

### 6.7 Bevy dependency — zero in engine

`src/combat_engine/` has no `bevy::` imports. Bevy systems are the glue layer (`process_action_system`, `project_state_to_ecs`, animation handlers). Engine survives Bevy upgrades unchanged.

---

## 7. Validation criteria

After Week 1 spike, decide go/no-go based on:

1. **API cleanliness.** `Effect` enum has < 15 variants for current mechanics. `step()` signature stable across Move/Cast/EndTurn.
2. **Performance.** `step()` on canonical 10-unit Move ≤ current `sim::apply_move` × 1.2.
3. **Test surface.** Unit tests for `apply_effect(state, Effect::Damage, content)` cover what currently takes parity tests + sim tests + real combat tests separately.
4. **Reaction model.** AoO recursion handled cleanly without ad-hoc special cases.
5. **Migration path is incremental.** Phase 1 lands without touching Cast/Status systems.

If 5/5 ✅ → green light for full migration (~12 weeks).
If 3-4/5 → revise approach, possibly fall back to Architecture B (events + applier, less radical).
If <3/5 → abort, status quo + per-mechanic parity tests as drift defence.

---

## 8. Related work / references

- **Step 12** (`step12_plan.md`) — per-drift fixes that motivated this proposal.
- **Event sourcing pattern** (Greg Young, Martin Fowler) — `Effect` queue + state-as-projection-of-events.
- **Pure functional core, imperative shell** (Russ Olsen, Gary Bernhardt) — engine = pure, runtime = shell.
- **Magic: the Gathering stack model** — reactions as effects producing effects, recursive resolution.
- **`combat/apply_effects.rs`, `movement.rs`, `status_apply.rs`, `status_tick.rs`** — code that this proposal would replace.
- **`combat/ai/plan/sim.rs`** — sim path that would become engine call.
- **`tests/parity.rs`** — existing parity harness; would become regression suite during migration.

---

## 9. What this is NOT

- **Not a multiplayer-first design** — but compatible (server-authoritative becomes natural).
- **Not replacing Bevy** — Bevy stays for rendering, input, animation, asset, UI; just combat logic exits.
- **Not removing data-driven content** — TOML stays as source for abilities/statuses; engine reads via `ContentView`.
- **Not changing scoring/AI logic** — AI scoring formulas, intent selection, agenda, bands all unchanged; only the sim substrate changes.
- **Not a 2-week refactor** — this is 10-14 weeks of careful incremental work.

---

## 10. Decision (resolved: GO)

All 4 strategic prerequisites answered in favour of engine extraction (2026-05-12):

1. **Reactions in roadmap?** Yes — core. Multiple reaction-class mechanics planned beyond AoO (counterspell, retaliate, mark, environment, telegraphs). Engine's first-class reaction model pays off rapidly.
2. **Replay / server-authoritative?** Replay yes, networked play later. Engine is the only practical path to bit-exact state-level replay.
3. **Refactor appetite?** Yes — fix arch first. Accept ~12 weeks of content pause; compounding velocity afterward.
4. **Bevy upgrade frequency?** Moderate (every 6-12 months). Engine isolation is a nice-to-have, not decisive — but reinforces the case.

→ Commit to migration. Section 5 plan is canonical.
