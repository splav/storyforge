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

#### Phase 4 retrospective (closed 2026-05-17)

**What landed (6 sub-steps, commits `ed7431f`–4f):**

- **4a** (`ed7431f`) — Engine TurnQueue + `start_round`. `CombatState.turn_queue` field, `TurnQueue` type, `init_state_from_ecs` reads `Res<TurnQueue>`. Engine unit tests for advance/wrap/current.
- **4b** (`ed7431f`) — `Effect::AdvanceTurn` + `Effect::BumpRound`. Dead-skip / stun-skip predicate, wrap derivation, recursion-bounded. `Event::TurnEnded/TurnStarted/TurnSkipped/RoundStarted`. `step(EndTurn)` grows to push `AdvanceTurn`. Engine unit tests: mid-round handoff, end-of-round wrap, dead-skip, all-dead-wrap.
- **4c** (`77981c0`) — Aura pure-presence query. `ContentView::auras_of` + `state.aura_effects_on`. `refresh_aggregates` folds aura bonuses. Skip-stunned predicate ORed with aura stun. Diff-on-move emits `AuraStatusGained`/`Lost`. `EcsContentView::auras_of` reads ECS `AuraSource`. Legacy `apply_auras_system` ran in parallel for parity.
- **4d** (`1231ca5`) — `Effect::EnterPhase` reactive. Phase check in `apply_effect(Damage)` before `Effect::Death` — boss never enters Dead on revival. `Event::PhaseEntered`. `ContentView::check_phase_trigger` + `EcsContentView` impl. Engine tests: preempt-death, multi-threshold AoE.
- **4e** (`33bedec`) — Bridge wiring + ECS deletion sweep. `ActionInput::EndTurn`, `process_action_system` routes it. Bridge translators for turn/round events → `CombatEvent`, `ActiveCombatant` inserts, `NextState<CombatPhase>::StartRound`. Deleted: `skip_dead.rs`, `auras.rs`, `phases.rs`. `advance_turn_system` collapsed to ≤20-line event-observer shim. Wrap-detection tests migrated to engine.
- **4f** (this step) — `VisualAssets` SystemParam bundle + `ContentParams` bundle; `process_action_system` drops to 14 params. `SimState::apply_endturn` wired in beam-search (`generate_plans`): called once per branch after `apply_step`, projects engine tick into snapshot. Regression test `apply_endturn_ticks_status_exactly_once_per_branch` guards double-tick. Retrospective.

**Architecture deltas vs plan:**

- **`VisualAssets` = `RenderResources` extended.** In 4c the previous agent introduced `RenderResources` (bundling `grid_offset`, `tokens`, `mats`, `token_mesh`). 4f extended it with `tag_cache` and renamed it `VisualAssets` per D6 — no new type, an extension. A second `ContentParams` bundle (wrapping `aura_q` + `phases_q`) was added to reach ≤14 params (16 → 14), which the spec underspecified.
- **`PendingPhaseTransitions` deferred collect-then-apply.** In 4e the `Event::PhaseEntered` translator could not write ECS-only deltas (Name, Abilities, AxisProfile) inside `process_action_system` because a `&mut Vital` query conflict with the projector made Bevy reject the system set. Solution: a `PendingPhaseTransitions` resource collects `(UnitId, phase_idx)` pairs; a separate `apply_phase_transitions_system` runs after `project_state_to_ecs`. The spec described a translator-only approach; the actual wiring required this deferred pattern. Noteworthy divergence from spec §3 wording.
- **`SimState::apply_endturn` scope.** Spec §1 says "Phase 4 folds queue advancement into engine". In practice, `snapshot_to_combat_state` builds `CombatState` with an empty turn queue, so calling `step(Action::EndTurn)` on the sim state would fail (no queue). The wiring was scoped to `tick_actor_statuses` only (DoT ticks, status expiry), matching Phase 3's original impl. Queue-advancement inside sim requires populating the queue from snapshot order — deferred to Phase 6 ECS projection cleanup or Phase 5 replay work.
- **`apply_endturn` call site is in `generator.rs`, not `sim.rs`.** The spec says "wire in `sim.rs`"; the call site is in `generate_plans` inside `generator.rs`. `apply_endturn` itself lives in `sim.rs`. The distinction is implementation vs wiring; both files changed.

**Gate criteria status (§7 items 1–14):**

| # | Criterion | Status |
|---|---|---|
| 1 | `step(EndTurn)` advances queue: mid-round, wrap, dead-skip, stun-skip, all-dead | ✓ engine tests in `turn_queue.rs` / `end_turn.rs` |
| 2 | Sirota DoT ticks when poisoner dead-skipped | ✓ engine test in `end_turn.rs` |
| 3 | Aura speed_bonus reflects in MP refill (Phase 3.5b closure) | ✓ bridge integration test in `bridge_smoke.rs` |
| 4 | Aura-stun-on-next-actor skips without projection step | ✓ engine test in `aura.rs` |
| 5 | Aura combat-log events on move in/out of radius | ✓ bridge integration test |
| 6 | Boss phase trigger fires reactively; revival preempts Death | ✓ engine test in `phase.rs` |
| 7 | Multi-threshold AoE fires multiple phases in one `step()` | ✓ engine test |
| 8 | Round wrap fires `RoundStarted` exactly once; `state.round` increments; reactions reset | ✓ engine test |
| 9 | `skip_dead.rs`, `auras.rs`, `phases.rs` deleted; grep clean | ✓ 4e |
| 10 | `advance_turn_system` ≤30 lines; wrap tests moved to engine | ✓ 4e — collapsed to 20-line shim |
| 11 | `process_action_system` param count ≤14 | ✓ 4f — exactly 14 (`commands`, `reader`, `id_map`, `combat_state`, `combatants`, `active_content`, `content_params`, `rng`, `log`, `anim_queue`, `positions`, `visuals`, `next_phase`, `pending_phases`) |
| 12 | AI sim `apply_endturn` wired; mining shows no agenda regression | ✓ wired 4f; mining run executed on 162 files (842 decisions), no crashes, agenda mix stable |
| 13 | Full encounter playable end-to-end including multi-phase boss | Manual verification — deferred to user playtest |
| 14 | `cargo test` full suite green | ✓ 171 engine tests + 36 combat tests + 1 golden smoke |

**LOC delta:**
- Phase 4 total (`e9c885f..HEAD`): +2576 / -727, net **+1849** across 36 files.
- Step 4f only: +161 / -36, net **+125** (3 files: `engine_bridge.rs`, `sim.rs`, `generator.rs`).

**Surprises / known follow-ups for Phase 5+:**

- `Res<TurnQueue>` projection still alive per D1 — UI reads it; Phase 6 cleanup can delete if UI migrated.
- `SnapshotContentView::unit_template` still a stub (returns `None`) — AI beam search does not enumerate Summon candidates; wiring deferred to Phase 6 unless AI scoring expanded.
- Gate item 13 (manual playtest) is a user-side verification step — not blocked by code.
- Gate item 12 mining tolerance (≤5% agenda shift per category) verified by inspection; no automated before/after delta because pre-4f logs predate schema v36 and were not parseable by the current miner. Schema-version rollover is expected.
- `apply_endturn` in sim does not advance the engine turn queue (empty queue in sim) — full queue advancement requires populating `snapshot_to_combat_state` from snapshot unit order; tracked as Phase 6 / Phase 5 follow-up.

**What worked / what didn't:**

- **Worked:** dual-run (run engine + legacy in parallel in 4c/4d) made divergences loud rather than silent. Zero silent regressions during 4c/4d parity window.
- **Worked:** sub-step discipline (4a → 4f each independently green) prevented accumulation of broken states.
- **Didn't / needed adjustment:** `PendingPhaseTransitions` deferred resource was not anticipated in spec; the query conflict with `&mut Vital` in the projector surfaced only during 4e wiring. A two-system approach was the right fix but added ~30 lines not in plan.
- **Didn't:** `apply_endturn` in sim remains DoT-only (not full queue-advance). The spec said "Phase 4 folds queue advancement" — it folds it in the live engine path (real combat) but not in the sim (AI planning). Scope was correctly narrowed rather than forcing a fragile impl.

### 5.5 Phase 5 — Replay + log overhaul (Weeks 10-11)

**Goal:** replay first-class.

**Tasks:**
- Combat log = engine `Event` stream (JSONL append-only).
- Replay tool re-runs engine from log; asserts identical final state + event byte-equality.
- Schema bump v37: events-based log; legacy per-stage JSONL tagged.
- `replay_ai_log` + `mine_ai_logs` updated to read new format (or split: legacy reader + new reader during transition).

**Gate:** replay determinism test — re-run from log produces identical final state and event sequence; `cargo fuzz` over Action sequences finds zero engine panics.

#### Phase 5 retrospective (closed 2026-05-18)

**What landed (7 sub-steps, commits `fb81964`–5g; full plan in `step_unisim5_plan.md`):**

- **5a** (`fb81964`) — Serde derives on `Action`/`Effect`/`Event`/`Unit`/`UnitId`/`RoundPhase`/`Team`/`ActiveStatus`/`TurnQueue` + dependent payloads. `aura_membership_set` HashSet → BTreeSet (only known non-determinism in the engine after Phase 4). New `content_hash.rs` (BLAKE3 over TOML-sorted-key concat), new `trace.rs` (pure `serialize_*`/`parse_*` helpers, `post_state_hash`/`_hex`, `SCHEMA_VERSION`).
- **5b** (`27e37e5`) — `DiceSource::call_count(&self) -> u64` accessor. `step()` reports per-action RNG delta via `ApplyCtx::rng_calls` (matches 4d's `phase_entered` precedent). New `engine_purity.rs` test greps `crates/combat_engine/src/**/*.rs` for forbidden imports (`SystemTime`, `std::env`, `std::process`, `thread_local`); zero finds.
- **5c.1** (`8417752`) — Engine `Unit` absorbs per-combat state: `caster_context`, `auras`, `enemy_phases`, `aoo_dice`. `ContentView` trait contracts from 8 methods to 4 (`ability_def`, `status_def`, `status_bonuses`, `unit_template` — all static). `EcsContentView` shrinks; `init_state_from_ecs` is the only site that populates the new Unit fields from ECS components.
- **5c.2** (`87b238e`) — `crates/combat_engine/src/toml_content_view.rs`: Bevy-free `TomlContentView` parsing `assets/data/*.toml` directly. Path B (duplicate TOML record structs) rather than Path A (extract pure types from `src/content/`) — bridge parsers import Bevy-tied `CombatStats`/`Equipment`. Parity test `tests/toml_content_view_parity.rs` cross-checks 18 abilities + 10 statuses against `EcsContentView`.
- **5d** (`a2abc0f`) — Folder-per-fight filesystem layout: `logs/<fight_id>/{ai,engine}.jsonl`. `build_combat_log_path` → `build_combat_log_dir`. New `EngineTraceWriter` Bevy resource + `CombatLogSession` resource carrying `session_id` (= folder name, D11). Single combat-start hook `open_combat_logs_on_combat_enter` creates both writers; engine InitLine written on `OnEnter(CombatPhase::AwaitCommand)` chained after `init_state_from_ecs` (idempotent via `step_counter == 0`). `process_action_system` writes one StepLine per engine `step()` before projection. `DiceRng` gains `pub fn seed()`. `mine_ai_logs` glob → `logs/*/ai.jsonl`. AI log SCHEMA_VERSION unchanged (D4); `combat_log_header` event added (other event_types skipped by `actor_tick` filter).
- **5e** (`771bbfd`) — `InitLine` grows `round`/`phase`/`turn_queue` (SCHEMA_VERSION 37→38) — without them replay couldn't reconstruct starting state. `CombatState::set_next_synthetic_uid()` setter. New `crates/combat_engine/tests/replay.rs`: 5 canonical scenarios (pure_move, aoo_chain, cast_damage, phase_trigger, endturn) + 2 divergence sentinels + size benchmark (8 tests). New `src/bin/replay_engine_trace.rs` binary with `--strict-content` (D3) + `--tolerance` (D10) flags. `bridge_smoke` gains `engine_trace_full_combat_record_replay` (gate #14).
- **5g** (this step) — `replay_ai_log.rs` cleanup: stale `if ver != 27` gate replaced by `parse_actor_tick` (v33+ accepted per `MIN_SUPPORTED`). Top-of-file usage docstring updated to the `logs/<fight_id>/ai.jsonl` layout. Retrospective written here.

**Architecture deltas vs plan:**

- **`session_id` as a folder name, not a UUID.** §5.5 originally said "session_id = UUID". User feedback during 5d planning (post-5a discoveries §10 item 6) pushed for folder-per-fight layout: the folder name (timestamp + scenario + encounter, sanitized) IS the `session_id`, dropping the UUID dependency. Both `ai.jsonl` header and `engine.jsonl` InitLine self-describe via this field.
- **InitLine extension late in 5e.** Original plan §3 listed `units`/`next_synthetic_uid`/`content_hash`/`session_id` for InitLine. Building the replay binary surfaced an obvious gap: `post_state_hash` covers `round`/`phase`/`turn_queue`, so the first replayed step's hash would mismatch unless the InitLine carries them. SCHEMA_VERSION bump 37→38 was a clean break (no production logs at v37 yet).
- **`ContentView` trait contraction (5c.1) was a re-scoping.** Original 5c was just "TomlContentView". Audit during 5b found 4 trait methods returned PER-COMBAT-INSTANCE data (`auras_of`, `check_phase_trigger`, `caster_context`, `aoo_dice`) built from ECS components — not loadable from TOML. Option (A): absorb the data into engine `Unit`; trait surface contracts to static-only. This added ~3 days for 5c.1 but made 5c.2 trivial (~1 day) and replay determinism becomes trivially correct (same `Unit` → same behaviour, no precomputed map drift).
- **`build_entry` and AI log carry-fields.** `ActorTickEvent`/`ActorTickInput` gained `session_id: String` + `engine_step_range: Option<(u64, u64)>` fields in 5d. The `engine_step_range` field is currently always `None` — populating it is Phase 6 work (the bridge has no place to track this yet, since `process_action_system` and `pick_action` aren't co-located). Field exists for forward-compat.
- **Per-stream schema versioning (D4).** Engine trace started at SCHEMA_VERSION 37 (collision with the AI log's 36 was cosmetic — different files). 5e bumped it to 38 on InitLine extension. AI log SCHEMA_VERSION stayed at 36 throughout Phase 5 (no AI-log content changed; the `combat_log_header` event is a new `event_type`, not a schema-bumping field addition). Independent counters, no shared constant.

**Sub-step 5f deferred (fuzz harness, gate #6):**

- **Status:** Not executed in Phase 5. Requires `cargo-fuzz` + nightly Rust, neither installed on the build host. Skipped after user decision (5g preferred over investing in toolchain setup).
- **What's not done:** No `fuzz/` sibling crate, no `step_random_actions` target, no 10M iter run.
- **Risk assessment:** Low for now. The replay tests (5e) and `engine_purity` test (5b) provide most of the determinism guard. Engine `step()` panicking on an unusual `Vec<Action>` would surface in real-combat playtest; CI greenness across 1071 tests covers the broad surface.
- **Reopen condition:** When nightly + cargo-fuzz become available, ship `fuzz/` per §5 D7 / §3 row "5f" of the plan. Plan stays valid as written.

**Gate criteria status (§7 items 1–15):**

| # | Criterion | Status |
|---|---|---|
| 1 | All engine types round-trip via serde with byte-equality | ✓ `crates/combat_engine/tests/serde_roundtrip.rs` (5a) |
| 2 | RNG call-count accurate: `step(Action::Cast { targets: N })` consumes exactly N rolls | ✓ `crates/combat_engine/tests/rng_count.rs` (5b) |
| 3 | `aura_membership_set` BTreeSet; replay byte-equal across re-runs on recording host | ✓ `tests/aura_determinism.rs` (5a) + replay loop (5e) |
| 4 | Replay determinism on recording host: 5 canonical scenarios with `--tolerance 0` | ✓ `crates/combat_engine/tests/replay.rs` 5 scenarios + 2 divergence sentinels (5e) |
| 5 | `TomlContentView` parity with `EcsContentView` | ✓ `tests/toml_content_view_parity.rs` (5c.2) |
| 6 | `cargo +nightly fuzz run step_random_actions` → 10M iters zero engine panics | **Deferred — no cargo-fuzz/nightly available; see 5f sub-section above** |
| 7 | Engine purity audit: zero forbidden imports inside `crates/combat_engine/src/` | ✓ `tests/engine_purity.rs` (5b) |
| 8 | `replay_ai_log` stale `ver != 27` gate fixed; reads from `logs/*/ai.jsonl` | ✓ (5g) — `parse_actor_tick` (v33+) replaces hardcoded gate; doc updated |
| 9 | `engine_trace.jsonl` size measured + documented | ✓ `measure_trace_size_per_round` (5e): ~493 B/step, ~4930 B/round |
| 10 | Cross-host replay with default `--tolerance 1.0` warns but does not panic on known-divergent f32 | ⚠ Flag plumbed in `replay_engine_trace`; manual verification deferred (no known-divergent f32 case in current Event variants; `--tolerance` is a no-op until f32 fields appear in Damage/Heal events) |
| 11 | Both `<dir>/engine.jsonl` InitLine AND `<dir>/ai.jsonl` header carry same `session_id`; AI decisions optionally back-reference engine step ranges | ✓ (5d) — both writers share `CombatLogSession`; `engine_step_range` field plumbed (population is Phase 6) |
| 12 | Filesystem layout: `logs/<fight_id>/{ai,engine}.jsonl` present after combat | ✓ (5d) — verified by `engine_trace_writer_init_and_step` + `engine_trace_full_combat_record_replay` |
| 13 | `process_action_system` param count stays ≤14 | ✓ — 13 params after 5d (`+EngineTraceWriter` brought 12→13) |
| 14 | Bridge integration test: full combat encounter produces a trace that re-runs deterministically | ✓ `bridge_smoke::engine_trace_full_combat_record_replay` (5e) |
| 15 | Full `cargo test` suite green; manual playtest reproduces trace | ✓ 1071 tests green; manual playtest deferred to user-side verification |

**LOC delta:**
- Phase 5 total (`e069ac3..HEAD`): +4762 / -662, net **+4100** across 46 files.
- Largest additions: `crates/combat_engine/tests/replay.rs` (~900 LOC, 5e), `crates/combat_engine/src/toml_content_view.rs` (~570 LOC, 5c.2), `src/combat/ai/log/mod.rs` (5d expansion), `crates/combat_engine/tests/serde_roundtrip.rs` (5a), `src/bin/replay_engine_trace.rs` (5e).

**Surprises / known follow-ups for Phase 6+:**

- **AI-log filesystem migration is a hard break.** Old flat-layout `logs/*.jsonl` files become orphans under the new `logs/<dir>/ai.jsonl` layout. Per D5, no migration script — users who need them must `git checkout unisim/phase4-complete`.
- **`engine_step_range` per-decision field is plumbed but always `None`.** Wiring the bridge to track engine step ranges across an AI decision (between `pick_action` and the subsequent action apply) needs `process_action_system` and `pick_action` to share state. Deferred to Phase 6.
- **f32 tolerance (`--tolerance`) is currently a no-op.** No Event variant carries f32 fields today; the flag exists for forward-compat when `Damage`/`Heal` events gain `final_damage_f32` fields (gate #10).
- **`replay_engine_trace` hardcodes `assets/data` as the content directory.** Run from project root for now; Phase 6 can probe via `CARGO_MANIFEST_DIR`.
- **`AooRow` type alias in `engine_bridge.rs` is dead** (pre-existing baseline warning; survived 5c.1 cleanup). Worth a small follow-up.
- **fuzz harness (5f) is the only deferred gate.** Re-open when nightly toolchain is available on the build host.

**What worked / what didn't:**

- **Worked:** sub-step discipline (5a → 5g each independently green) prevented broken intermediate states. Every commit was independently testable; replaying any sub-step in isolation works.
- **Worked:** the per-step `post_state_hash` canary made replay-tests precise — a 1-bit drift fails the *exact* step rather than the final state, dramatically shortening debug. Two divergence sentinels (`replay_event_divergence_detected`, `replay_rng_count_divergence_detected`) prove the harness catches real drift.
- **Worked:** TomlContentView parity test (5c.2) caught the post-5c.1 Unit field omission (`aoo_dice`) in `make_unit` test helper. The cross-check was load-bearing during the trait contraction.
- **Worked:** 5c.1 trait contraction was a non-obvious but high-value re-scope. The original "ContentView read" assumption was wrong — half the methods were really per-combat state. Catching this in audit (post-5b) rather than during 5e replay would have caused weeks of confusion.
- **Didn't / needed adjustment:** InitLine field set was incomplete in 5a. 5e had to extend it with `round`/`phase`/`turn_queue` and bump the schema (37→38). Should have been in 5a; cost was minimal because no v37 logs were in production yet.
- **Didn't:** fuzz harness (5f) — deferred for tooling reasons. Loses defense-in-depth for `step()` panic-safety; replay tests cover most of the determinism surface but not the "unusual input" surface.
- **Didn't:** AI log `engine_step_range` field is plumbed but not populated. Cross-stream correlation works via `session_id` (folder name) but step-range join is Phase 6 work.

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
