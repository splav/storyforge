# Enemy AI

## Overview

AI-система выбирает действие для вражеских юнитов (и героев под `pact_control`). Работает в рамках `CombatStep::Command`: `enemy_ai_system` для Team::Enemy, `pact_ai_system` для героев с `ai_controlled`-статусом.

Каждый AI-тик всегда строит **свежий `pick_action`** — beam-search строит полную цепочку шагов, коммитится только первый (или `Move→Cast` бандлом). Reservations координируют параллельно действующих юнитов и резервируют только закоммиченный prefix.

**Plan freeze** (`ai_freeze_plan_after_move`, по умолчанию включён): после `MoveOnly`-тика AI сохраняет план в `AiMemory.last_plan`. На следующем тике — проверяет снапшот (HP/rage/статусы актора, HP/pos таргета). Если изменений нет — выполняет `steps[step_index]` из сохранённого плана, подавляя осцилляцию "вперёд-назад" от немонотонного скоринга. При изменении состояния (AoO-удар, смерть/движение цели, новый статус) — replan. Fresh-план строится всегда (для divergence-логирования), но используется только если continuation не валиден.

Файлы: `src/combat/ai/` + shared core в `src/combat/effects_*`.

| Файл / модуль | Назначение |
|---|---|
| `enemy_turn.rs` | Главная система: строит snapshot/maps, вызывает `pick_action`, отправляет сообщения |
| `utility/mod.rs` | Top-level pipeline: `pick_action` (intent → plans → scoring → sanity → pick → commit), `UtilityContext`, `AiDecision` |
| `utility/fallback.rs` | Движение без касто́в для edge-case (мёртвый актор в snapshot) |
| `planning/` | Многошаговые планы: `types` (`PlanStep`, `TurnPlan`, `StepOutcome`), `sim` (чистая симуляция, использует shared `compute_ability_outcome`), `generator` (beam search), `scorer` (10-факторный скоринг плана), `sanity` (multiplicative penalties + ProtectSelf mask), `picker` (`commit_plan`, `pick_best_plan`, `record_committed_reservations`, `PickMechanics`) |
| `factors/` | 10-факторный скоринг: `mod.rs` (`ScoredStep`, `compute_factors(ctx, step, outcome)`), `offensive.rs` (читает `ActionOutcomeEstimate` — damage/heal/kill_now/kill_promised/cc + `aoe_area`), `scarcity.rs`, `adjustments.rs` (reservations + `crit_fail_adjusted`), `tempo.rs` (plan-terminal `tempo_gain`), `saturation.rs` (buff-redundancy penalty), `survival.rs` (plan-level `self_survival`) |
| `intent.rs` | `TacticalIntent` — выбор стратегической цели + `intent_score(intent, step, step_ctx, outcome)` + `AiMemory` (stickiness + `last_plan` для plan freeze) + `PlanSnapshot` / `StoredPlan` (инвалидация) |
| `outcome.rs` | `ActionOutcomeEstimate` (9 полей: expected_damage, p_kill_now, p_kill_soon, deny_value, rescue_value, board_pressure, exposure_delta, geometry_gain, resource_swing) + `PlanAnnotation` + extraction helpers (`estimate_hypothetical`, `estimate_kill_soon`, `estimate_deny_value`, `estimate_rescue_value`, `estimate_expected_damage`, `compute_score_core`) |
| `scoring.rs` | HP-эквивалент helpers: `estimate_st_damage`, `estimate_damage_horizon`, `status_applications`, `stun_denial_value`, `applies_cc`. Центральный `score_action` удалён в step 4.5 — формула переехала в `outcome::compute_score_core`, consumers читают outcome vector |
| `tuning.rs` | `AiTuning` — центральный Resource с константами тюнинга (`thresholds`, `tables`, `difficulty` curves) + `AiTuningOverride` per-unit scaffolding |
| `target_priority.rs` | Оценка важности цели (threat, killability, density, vulnerability, proximity, role) |
| `trade.rs` | MVP2 trade economics: actor-agnostic `unit_value(u)`, per-plan `trade_delta(plan)` на commit-prefix, `trade_score` для плана-модификатора |
| `position_eval.rs` | Оценка клетки по картам влияния с весами роли |
| `snapshot.rs` | `BattleSnapshot` + `UnitSnapshot.statuses: Vec<ActiveStatusView>` + `refresh_status_aggregates` |
| `role.rs` | `AxisProfile` — 5-мерная роль, инференс по kit'у |
| `difficulty.rs` | `DifficultyProfile` — ручки качества решений |
| `influence.rs` | Карты влияния + `InfluenceConfig` resource |
| `reservations.rs` | Координация команды: забронированный урон / CC / тайлы (reset на round-start) |
| `debug.rs`, `log.rs` | Debug overlay + JSONL-лог решений |

### Shared effects core (вне `ai/`)

`src/combat/effects_math.rs`, `effects_state.rs`, `effects_outcome.rs` — **единый источник истины** для разрешения способности. Real pipeline (`combat/resolution.rs`) и AI sim (`combat/ai/planning/sim.rs`) вызывают один и тот же `compute_ability_outcome`; различаются только backend'ами (RNG vs EV, Bevy components vs snapshot). См. раздел «Ability Resolution».

## Цикл принятия решения

Один тик `enemy_ai_system`:

```
1. Проверка AP/MP (ничего нельзя → EndTurn)
2. Построить BattleSnapshot + InfluenceMaps
3. pick_action:
   a. ★ select_intent → TacticalIntent
   b. generate_plans: beam-search глубиной plan_max_depth, шириной
      plan_beam_width. Шаг — Cast (из top-N threat ∪ top-M killability)
      или Move (top по escape/opportunity/priority-adj). Hard constraints
      (taunt, overheal, wasted-CC, self-AoE) режут невалидные Cast.
      Дубликаты по logical_key схлопываются.
   c. score_plans_with_raw: 10-факторный utility scoring → scores + raw
      factor matrix.
   d. ★ Intent viability guard: если max(intent_factor) < threshold —
      fallback intent (midpanic → ProtectSelf; иначе FocusTarget над
      достижимой целью) и rescore.
   e. sanity_adjust_plans: мультипликативные штрафы (survival quadratic,
      LoS blindspot, retreat trap, self-AoE, AoO risk). **Только мягкие
      penalty — никаких `-∞` масок.**
   f. ★ apply_adaptation: value-function overrides на основе фактов,
      обнаруженных после measurement+correction. Per-plan ExpectedSelfLethal
      (aoo_dmg ≥ hp && intent != ProtectSelf) → EvaluationMode::LastStand;
      global ProtectSelfNoDefensive (intent=ProtectSelf && ни одного
      defensive) → mode=LastStand всем. Rescore затронутых intent-column.
   g. Если intent == ProtectSelf: маскировать не-defensive в -∞ (contract
      enforcement). Маска применяется только к планам с mode=Default.
   h. pick_best_plan: mercy окно → rerank по cruelty → top-K sampling
      внутри similarity window (max(score_noise × 2, 0.05)).
4. commit_plan(best, actor_pos) → (AiDecision, consumed):
   - []                  → (EndTurn, 0)
   - [Cast, ..]          → (CastInPlace, 1)
   - [Move, Cast, ..]    → (MoveAndCast, 2) — атомарный бандл
   - [Move, ..]          → (MoveOnly, 1)
5. record_committed_reservations(plan, consumed, ...) — резервирует
   урон/CC/тайл только для закоммиченного prefix.
6. Нет планов вообще (актор пропал из snapshot) → fallback_move.
```

При `MoveOnly` commit: план сохраняется в `AiMemory.last_plan` (см. **Plan freeze** выше). При `Cast`/`MoveAndCast`/`EndTurn` — `last_plan` очищается. Следующий тик всегда строит fresh-план; при наличии `last_plan` и валидном снапшоте — продолжает его вместо следования fresh-результату.

### `GrantMovement` mid-turn

Способности с эффектом `GrantMovement { distance }` **немедленно** добавляют `distance` в пул активного юнита. Следующий AI-тик re-planit уже с расширенным пулом.

## Ability Resolution (shared core)

Общее ядро разрешения способности живёт в `src/combat/effects_*.rs`. Оба потребителя — live pipeline и AI sim — вызывают одну и ту же pure-функцию, отличаясь только реализациями traits.

### `TargetState` (`effects_state.rs`)

Read-only абстракция над battle state для AoE target enumeration и friendly-fire фильтрации:
```rust
pub trait TargetState {
    fn actor_pos(&self, actor: Entity) -> Option<Hex>;
    fn unit_at_cell(&self, pos: Hex) -> Option<TargetRef>;
    fn team_of(&self, entity: Entity) -> Option<Team>;
}
pub fn compute_affected_targets<S: TargetState>(
    actor, def, primary_target, target_pos, state,
) -> Vec<Entity>;
```
Backend'ы: `BevyTargetState` (в `resolution.rs`) и `SnapshotTargetState` (в `sim.rs`).

### `DiceSource` (`effects_outcome.rs`)

Абстракция над dice rolls:
```rust
pub trait DiceSource {
    fn roll_dice(&mut self, expr: &DiceExpr, disadvantage: bool) -> (i32, String);
    fn roll_crit_fail(&mut self, crit_fail_die: u32) -> bool;
}
```
- `RngDice<'a>(&mut DiceRng)` — реальный роллер.
- `ExpectedValue` — EV round'ится до i32; `roll_crit_fail` → **false** (sim допускает MAP-estimate: «самый вероятный исход — попадание»). Документированное решение; плюс scoring дисконтирует crit-chance через `crit_fail_adjusted` на факторе damage, и каждый тик — fresh replan.

### `AbilityOutcome` + `compute_ability_outcome`

Нейтральная структура-результат:
```rust
pub struct AbilityOutcome {
    pub affected: Vec<Entity>,
    pub primary: OutcomePrimary,  // Damage{raw, pierces} | Heal{amount} | GrantMovement{distance} | RestoreResources | Summon{...} | None
    pub statuses: Vec<StatusApply>,
    pub breakdown: String,
    pub crit_fail: Option<CritFail>,   // Miss / SelfStatus / SelfDamage
    pub mana_overload: bool,           // effects fire, mana cost doubles
}
```

`compute_ability_outcome` вычисляет всё это за один проход: rolls crit-fail, применяет crit mapping (BrokenFaith / CircuitBreach / Exhaustion / PactControl → `CritFail::SelfStatus` / `SelfDamage`; ManaOverload → `mana_overload = true`, primary fires), определяет primary branch из `EffectDef`, разворачивает statuses для `StatusOn::Target` и `MySelf`.

**Backends:**
- Real (`resolution.rs`): конвертирует outcome в `ApplyDamage`/`ApplyHeal`/`ApplyStatus`/`SpawnUnit` messages + `BonusMovement` tag + log events.
- Sim (`sim.rs::apply_primary`): мутирует snapshot напрямую. `Damage` применяет `final_damage_f32(raw, armor, vuln, pierces)` с floor `max(1.0)`. `Heal` сначала нейтрализует target DoT (`dot_per_tick`), затем восстанавливает HP. `apply_statuses` пушит `ActiveStatusView` в `unit.statuses` с `dot_per_tick = dice.expected().round()` и обновляет агрегаты через `refresh_status_aggregates` — следующий шаг плана видит свежую броню/vuln от только что применённого статуса.

### Drift scorecard (sim ↔ real)

Фиксированные drift'ы:
- ✅ #1 — damage floor `max(1)` единый
- ✅ #2 — DoT cleanse on heal в sim
- ✅ #4 — crit-fail mapping в shared core (sim игнорирует, но через явный `roll_crit_fail → false`)
- ✅ #5 — mid-plan status-adjusted armor

Остаются:
- ⏳ #3 — rage gain (+1 rage attacker/defender на damage в real, sim не моделирует).
- **Speed mid-plan**: base speed не трекается отдельно в `UnitSnapshot`, поэтому speed-изменяющие статусы, применённые в step[k], не re-flow в pathing следующих шагов.

## Utility Scoring

Каждый `TurnPlan` оценивается по 10 факторам. Факторы делятся на два типа с разной нормализацией:

* **Non-negative** `[0, 1]`: `/ max` в батче
* **Signed** `[-1, 1]`: `/ max(|min|, |max|)` — симметричная нормализация

Финальный скор = `dot(normalized_factors, role_weights) + summon_bonus + noise`.

### `ScoredStep` — единица скоринга

`factors::ScoredStep<'a>` — ref-based view над `PlanStep` + caster tile на момент шага. Заменяет старый owned `ActionCandidate`; scoring не аллоцирует per step.

```rust
pub enum ScoredStep<'a> {
    Cast { ability: &'a AbilityId, target: Entity, target_pos: Hex, caster_tile: Hex },
    Move { caster_tile: Hex },
}
```

- Для Cast: `caster_tile` = позиция актора в момент каста.
- Для Move: `caster_tile` = destination пути.

Конструируется через `ScoredStep::from_plan_step(step, pre_step_pos)` (per-step сканнинг в scorer) или `ScoredStep::from_plan_committed(plan, actor_pos)` (view того, что commit_plan выполнит этот тик — для debug).

### Факторы

| Фактор | Тип | Источник | Агрегация по шагам плана |
|---|---|---|---|
| `damage` | non-neg | `compute_factors()` per step | **Discounted sum** (`base_discount^k`) |
| `kill_now` | non-neg | 1.0 если direct expected damage ≥ target.hp сейчас | **Discounted sum** |
| `kill_promised` | non-neg | 1.0 если `kill_now=0` И direct+DoT(new+pending) ≥ target.hp | **Discounted sum** |
| `cc` | non-neg | Ценность статусов (stun × threat × duration) | **Discounted sum** |
| `heal` | non-neg | Скор хила | **Discounted sum** |
| `intent` | **signed** | `intent_score()` | **discounted sum** (Cast и Move; latching ×0 после kill intent-цели) |
| `scarcity` | **signed** | `swing_value − resource_ratio` | **Discounted sum** |
| `tempo_gain` | **signed** | `compute_plan_tempo_gain()` | **терминальная** (последний шаг) |
| `saturation` | **signed** | `-0.4 × count_redundant_buff_classes` | **Discounted sum** |
| `self_survival` | **signed** | `compute_plan_self_survival()` | **план-уровень** (single value) |

Удалённые в Phase 6: `position` (заменён сигналами `tempo_gain`), `risk` (заменён `self_survival`), `focus` (`target_priority × damage` уже учтён через intent-контракт).

`evaluate_position` в `position_eval.rs` остаётся как вспомогательная функция — используется в `sanity.rs` (reposition-penalty) и `intent.rs` (Reposition intent selection).

`tempo_gain` — мера прогресса к intent-target: `Δdist/speed` + `+0.3` за вход в cast-range + `+exit_danger_bonus`. Для Cast без предшествующего Move = 0. Без intent-target (Reposition, ProtectSelf, …) = 0.

`saturation` — штраф за повторное наложение баффа той же `buff_class` (ArmorBuff, Haste, DamageUp, Shield) на того же получателя, который уже несёт статус этого класса. `-0.4` за каждый избыточный класс. Читает `pre_snap` (симулированное состояние до шага) — intra-plan буфы автоматически учитываются.

`self_survival` — план-уровневая мера защиты актора. Формула: `Σ(self-heal EV / max_hp) + Σ(armor_bonus × 3 / max_hp) + max(0, danger(start) − danger(final_pos))`. Только самонаправленные касты (target == actor.entity). Используется как основа для ProtectSelf contract: план считается **defensive** iff `self_survival ≥ SELF_SURVIVAL_EPSILON (0.15)`.

Плюс **`summon_bonus`** — post-normalisation additive: за каждый `Summon` Cast в плане `dpr × cap_decay × sat_mult`, где `cap_decay = 1 − count/cap` (running, второй Summon ценится меньше), а `sat_mult = 0.65^total_allies` — глобальный штраф насыщения (много союзников → польза от нового суммона ниже).

Плюс **`trade_score`** — см. раздел «Trade Economy» ниже. HP-equivalent оценка размена (killed enemies − lost allies − self-lethal actor), применяется после role-composition с tanh-squash.

**Discount.** `plan_step_discount` (easy 0.75 / normal 0.85 / hard 0.90). step[k] — `base^k`.

### Outcome vector (outcome.rs)

`ActionOutcomeEstimate` — общий словарь «что произошло в одном шаге плана», структурированная оценка последствий. Живёт на `TurnPlan.annotation: PlanAnnotation { outcomes: Vec<ActionOutcomeEstimate> }` — по одной записи на каждый `plan.steps[i]`. Populated в `generator.rs::build_step_outcome_estimate` после `sim::apply_step`, потребители (compute_factors, intent_score, future_value, picker) читают поля напрямую.

Поля:

| Поле | Семантика | Population (step 4.2) |
|---|---|---|
| `expected_damage` | Scorer-compatible damage value (HP-equivalent, через `score_action` formula + crit_fail_adjusted) | `estimate_expected_damage` для single-target enemy; sim-derived `outcome.damage` для AoE |
| `p_kill_now` | 1.0 если шаг убивает цель в этот ход | `1.0 if !outcome.killed.is_empty()` |
| `p_kill_soon` | 1.0 если не убивает сейчас, но накопленный DoT (pending + новый) убьёт | `estimate_kill_soon` (single-target) / max над AoE hits |
| `deny_value` | CC/vulnerability/armor-shred denial value (stun × threat + status bonuses) | `estimate_deny_value` per primary (single) / sum over AoE enemy hits |
| `rescue_value` | Heal value с urgency baked-in (hp_missing × danger_multiplier) — для SingleAlly | `estimate_rescue_value` (копия score_action.heal 1:1) |
| `board_pressure` | 0.0 в Волне 1; заполняется в step 5 (terminal eval) |
| `exposure_delta` | Δdanger от шага: `worst_path_danger(maps, path)` для Move, 0 для Cast | `step_path_danger` |
| `geometry_gain` | 0.0 в Волне 1; заполняется в step 17 (geometry awareness) |
| `resource_swing` | Signed: `-cost_ap - Σresource_costs` для Cast; `-path.len()` для Move (negative = spent) |

**Architectural invariant (step 4).** `compute_factors` / `intent_score` получают `&ActionOutcomeEstimate` явным параметром и читают поля — не re-derive через `score_action`. `scorer::compute_plan_factors_sans_intent` и `compute_plan_intent_sum` пробрасывают `&plan.annotation.outcomes[idx]`.

**Hypothetical outcome (no sim).** `outcome::estimate_hypothetical(def, target, caster, content, danger)` — для committed state / reservations, где sim не запускается. Вычисляет те же поля напрямую. Используется в `future_value::λ_attack` (3 call-sites) и `picker::record_committed_reservations`.

**JSONL schema v19+** сериализует `annotation` внутри `PlanLogEntry` — old v18 логи читаются через `#[serde(default)]` с пустым annotation (generate_plans перестраивает при replay).

**Schema v23+** (step 5.6): `annotation.terminal` (`TerminalScore`, 8 axes) сериализуется в JSONL. v22 логи читаются через `#[serde(default)]` → zero-filled `TerminalScore`.

### Terminal state evaluation (planning/terminal.rs)

`TerminalScore` — оценка плана по состоянию **доски после финального шага**, параллельная step-sum факторам. Populated в `terminal_state_score(plan, initial_snap, ctx)`, вызывается в `finalize_scores`. Хранится в `plan.annotation.terminal`.

8 осей в 3 кластерах:

| Ось | Кластер | Семантика |
|-----|---------|-----------|
| `exposure_at_end` | Defensive | Danger-map penalty финальной позиции (чем ниже, тем лучше) |
| `next_turn_lethality` | Defensive | Вероятность гибели актора от всех врагов на следующем ходу |
| `secure_kill` | Offensive | Гарантированное убийство цели планом |
| `ally_rescue` | Offensive | Выход союзника из danger-зоны после хила |
| `board_control_gain` | Offensive | Улучшение контроля доски (threat × доступность цели) |
| `line_actionability` | Geometric | Число AoE-линий, открытых с финальной позиции |
| `density_value` | Geometric | Ценность кластеров врагов в reach |
| `pressure_spacing_zone` | Geometric | Тактическое расстояние до враждебных юнитов |

**Агрегация** в `finalize_scores`: `dot(terminal_weights, terminal_score_vec)` суммируется в финальный скор плана. `terminal_weights = AxisProfile::terminal_weights(tuning)` — symmetric к `factor_weights`, sourced из `AiTuning.tables.axis_terminal_weights[5][8]` (`assets/data/ai_tuning.toml`). Каждая ось модулируется сигналом из `NeedSignals`:

| Ось | NeedSignal модулятор |
|-----|---------------------|
| `exposure_at_end`, `next_turn_lethality` | `× (1 + self_preserve)` |
| `secure_kill` | `× (1 + finish_target)` |
| `ally_rescue` | `× (1 + rescue_ally)` |
| `board_control_gain` | `× (1 + reposition)` |
| `density_value` | `× (1 + setup_aoe)` |
| `line_actionability`, `pressure_spacing_zone` | без модуляции |

**Калибровочное состояние**: defensive + offensive кластеры активны; geometric (`line_actionability`, `density_value`, `pressure_spacing_zone`) обнулены до фазы 2b mining-калибровки — ненулевые веса требуют corpus replay-данных для корректного баланса.

Полная декомпозиция: `docs/ai_rework_step5_plan.md`.

### Goal-preserving repair (combat/ai/repair/)

Заменяет binary plan freeze (exact-continuation либо replan-from-zero) на **goal context + repair affinity bonus**: на каждом тике строится свежий план, но планы, сохраняющие сохранённый замысел, получают скоринговый bonus.

**`StoredGoalContext`** (хранится в `AiMemory.last_goal`, пишется при Move-decision в `run_ai_turn`, очищается на Cast/EndTurn):

```rust
pub enum GoalKind {
    Finish { target },        // FocusTarget kill (p_kill_now ≥ 0.6 или target hp < 30%)
    Pressure { target },      // FocusTarget damage без kill
    DisableEnemy { target },  // ApplyCC
    HealAlly { ally },        // ProtectAlly heal
    Retreat { region_anchor }, // ProtectSelf, LastStand
    SetupAOE { region_center, planned_ability },
    Reposition { region_center },
}
```

Поля: `kind`, `region_anchor` + `region_radius`, `planned_ability` (step[1] cast если был), `ttl` (default 2 rounds), `confidence` (`chosen.score / pool_max_score` в момент store), `created_round`, плюс severity-snapshot полей actor/target.

**`StoredGoalContext::check_continuation(actor, target)`** возвращает `Option<PlanContinuationCheck>` с `ContinuationSeverity`:

- **`Cosmetic`** — изменение не влияет на goal (rage tick).
- **`Relevant`** — goal достижим, метод может смениться (target moved, hp drop, status changed).
- **`Invalidating`** — goal недостижим (target dead/gone, actor pos mismatch).

**`RepairAffinity`** — 6 axes (`goal_alignment`, `region_alignment`, `method_alignment`, `severity_factor`, `ttl_factor`, `confidence`), aggregate'ятся в bonus через role-weighted `RepairWeights = AxisProfile::repair_weights(tuning)` (table `axis_repair_weights[5][3]`).

**Consumer в `finalize_scores`:**

```rust
if ctx.last_goal.is_some() {
    score += affinity.aggregate(weights) * (1.0 + need_signals.continue_commitment) * tuning.thresholds.repair_bonus_scale;
}
```

`repair_bonus_scale = 0.4` default, additive, всегда ≥ 0.

**Continuation evaluator** — два набора role-axis весов в `AiTuning.tables`:
- `axis_factor_weights` (discovery) / `axis_factor_weights_continuation` — переключаются по `ctx.last_goal.is_some()`.
- `axis_terminal_weights` (discovery) / `axis_terminal_weights_continuation` — то же для terminal axes.

Множители continuation от discovery:
- factors: `kill_now ×1.2`, `kill_promised ×1.2`, `tempo_gain ×1.15`, `self_survival ×0.7`, остальные ×1.0.
- terminal: `exposure_at_end ×0.8`, `next_turn_lethality ×0.6`, `secure_kill ×1.3`, `board_control_gain ×1.3`, остальные ×1.0.

Sanity-mask и `apply_protect_self_mask` contract нетронуты — continuation меняет только axis weights в aggregator'е.

**`ContinuationOutcome`** для логов (`PlanDivergenceEntry.continuation_outcome`, schema v26+):
- `GoalPreservedMethodDelivered` — same goal, fresh = Cast/MoveAndCast → actor delivered the arc.
- `GoalPreservedInTransit` — same goal, fresh = Move-only → actor walking toward it.
- `GoalAbandonedReactive { source }` — forced by environment (taunt, panic, viability fallback).
- `GoalAbandonedVoluntary` — actor freely picked another intent (the real commitment-failure signal).
- `GoalAbandonedInvalidating` — target dead / position mismatch (hard invalidation).
- `GoalAbandonedTtlExpired` — goal age ≥ ttl.
- `NoStoredGoal` — первый тик / после Cast/EndTurn.
- `LegacyV25Abandoned { reason }` — pre-v26 entry с нераздельным `goal_abandoned`; voluntary/reactive split неизвестен.

`mine_ai_logs` секция **C6** агрегирует распределение — целевые таргеты (v26+ corpus):
`goal_preserved (combined) ≥ 60%`, `goal_abandoned|voluntary ≤ 10%`, `method_delivered ≥ 10%`.

**Примечание по mining на v25 логах:** v25 corpus показывает только partial breakdown через alias;
full breakdown (voluntary/reactive split) — на v26+ playtest'ах.
v25 пример: `in_transit: 24 (58.5%)`, `legacy_v25_abandoned: 17 (41.5%)`.

Schema versions: `continuation_outcome` (v25 shape: `{kind,reason}`) появилось в v25 (step 6.6a);
рефинировано до 7-variant split в v26 (step 6.6b). v25 aliases + `LegacyV25Abandoned` обеспечивают
backward-compat десериализацию без потерь. v22-v24 logs — `#[serde(default)]` → `NoStoredGoal`.

Полная декомпозиция: `docs/ai_rework_step6_plan.md`.

### Весовые таблицы по осям (AxisProfile)

Roles emergent — вектор весов по 5 осям (Tank/Melee/Ranged/Control/Support). Таблицы живут в `AiTuning.tables.axis_factor_weights` и `AiTuning.tables.axis_position_weights` (`assets/data/ai_tuning.toml`, step 2.4/2.5) — data-driven, редактируются без перекомпиляции.

**Axis factor weights** (`AiTuning.tables.axis_factor_weights`):

| Фактор | Tank | Melee | Ranged | Control | Support |
|--------|------|-------|--------|---------|---------|
| damage | 0.4 | 1.3 | 1.3 | 0.4 | 0.2 |
| kill_now | 0.6 | 1.6 | 1.3 | 0.5 | 0.3 |
| kill_promised | 0.3 | 0.8 | 0.65 | 0.4 | 0.15 |
| cc | 0.5 | 0.2 | 0.3 | 1.6 | 0.6 |
| heal | 0.2 | 0.0 | 0.0 | 0.0 | 2.0 |
| intent | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 |
| scarcity | 0.4 | 0.3 | 0.5 | 1.2 | 0.8 |
| tempo_gain | 0.8 | 1.0 | 1.2 | 1.0 | 0.8 |
| saturation | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 |
| self_survival | 1.0 | 0.8 | 0.8 | 0.8 | 1.2 |

### Composition: squared-smooth bias

Итоговые role-weights — **смещённые в сторону доминирующей оси** через power exponent `1.5`:

```
biased[i] = profile[i]^1.5 / Σ(profile[j]^1.5)
factor_weight[f] = Σ(biased[i] × AXIS_FACTOR_WEIGHTS[i][f])
```

### Инференс профиля (role.rs::infer_profile)

Каждая ability голосует за оси с весом `1 + total_cost`:

| Ability pattern | Голос |
|---|---|
| SingleAlly + Heal effect | Support +weight |
| Myself + no damage (taunt, rush) | Tank +weight |
| AoE / SpellDamage / ranged physical + damage | Ranged +weight (+0.4×w Control если есть status) |
| Melee physical + damage | Melee +weight (+0.4×w Control если есть status) |
| Status-only (stun, paralyze) | Control +weight |
| Movement/utility fallback | Melee +0.3×w |

Плюс **stat-based Tank bonus**: `(max_hp + armor×2) / 20`, clamped [0.3, 2.0].

### TacticalIntent (intent.rs)

AI выбирает один стратегический интент перед генерацией планов. Интент **не фильтрует жёстко** — выражается через фактор `intent` в scoring + viability guard.

#### Выбор интента (scored — max wins)

| Условие | Intent | Score |
|---------|--------|-------|
| HP < `survival_hp_threshold` И danger > `awareness_danger_threshold` | **ProtectSelf** (hard override) | — |
| HP < 40% И danger > 0 | **ProtectSelf** | (1−hp%)×danger |
| CAN_HEAL И союзник (вкл. self) с HP < threshold | **ProtectAlly { ally }** | 1 − ally_hp% |
| Есть враг с FORCES_TARGETING | **FocusTarget { taunter }** (override) | 1.2 |
| Taunter И CAN_CC И не оглушён | **ApplyCC { taunter }** | 0.8 + threat×0.1 |
| Нет taunter: враг убиваем И достижим за `speed+max_attack_range` | **FocusTarget { killable }** | 1.2 + (1−hp%)×0.3 |
| Нет taunter: — | **FocusTarget { default }** | 0.5 + prio×0.3 |
| CAN_CC И есть не-оглушённый враг | **ApplyCC { target }** | 0.8 + threat×0.1 |
| HAS_AOE И враги кластерируются (≤ 2) | **SetupAOE** | 0.7 + clusters×0.2 |
| pos_eval(текущая) < `awareness_reposition_threshold` | **Reposition** | 0.3 + gap×0.4 |

**ProtectAlly threshold** — role-aware: `0.5 + profile.support × 0.2`.
Stickiness bonus `+0.25` за continuation (+`0.15` если target тот же), до 3 ходов.

#### Intent viability guard

После scoring: если `max(intent_factor)` по планам ниже порога — intent переключается через `default_focus_target(active, snap, plans, actor_pos, exclude)`. Reachable target извлекается через `ScoredStep::from_plan_committed` над каждым планом.

| Intent | Порог viability |
|--------|---|
| Reposition | 0.01 |
| FocusTarget | 0.5 |
| ApplyCC | 0.5 |
| ProtectAlly | 0.5 |
| SetupAOE | 0.01 |
| ProtectSelf / LastStand | — (спец-ветка) |

#### Intent-скоринг

`intent_score(intent, step, step_ctx, outcome) -> f32` вычисляет alignment одного шага плана. `outcome` — `&ActionOutcomeEstimate` для текущего шага (из `plan.annotation.outcomes[idx]`).

**`FocusTarget` и `ApplyCC`** используют dot-product факторов × intent-специфичный вектор весов (`IntentWeights`). Вначале `compute_factors(step_ctx, step, outcome)` читает поля outcome (damage/kill_now/kill_promised/cc/heal); затем `filter_offensive_for_target` обнуляет offensive-оси для шагов, не направленных на интент-цель:

| Шаг | Offensive-оси |
|-----|---------------|
| Cast → focus entity напрямую | полный кредит |
| Cast → AoE, покрывающий тайл focus entity | × 0.6 |
| Cast → другая цель / нет цели | обнулить |
| Move | обнулить (geometry hook считает через pursuit) |

После фильтрации dot-product с:

| Intent | Вектор весов |
|--------|-------------|
| FocusTarget | `kill_now×2.0, kill_promised×0.3, damage×1.0, cc×0.5` |
| ApplyCC | `cc×1.5, damage×0.3` |

**Move во время `FocusTarget` / `ApplyCC`**: `pursuit_move_score(from, to, target, reach)` (без факторов).

**`ProtectSelf`, `ProtectAlly`, `SetupAOE`, `LastStand`** сохраняют прежние формулы (ported to new signature):

| Intent | Cast score | Move score |
|--------|-----------|-----------|
| Reposition | **tiered** | tiered |
| ProtectSelf | self-heal/self-buff на self = 1.0; иначе `1 − danger(tile)` | `1 − danger(tile)` |
| ProtectAlly | 1.0 heal ally; −0.3 heal wrong; 0.5 tile adj | 0.5 если adj к ally |
| SetupAOE | hits/total или −0.3 single-target | 0.0 |
| LastStand | dmg+kill+CC offensive combo | −0.3 |

**Почему factor-based для FocusTarget/ApplyCC?** Исправляет S5: низкоурон удар (1 дамага через броню) больше не получает тот же alignment credit 1.0, что убивающий удар. Пропорциональность к реальному импакту делает intent-фактор содержательным сигналом относительно severity шага.

**Pursuit Move score (FocusTarget / ApplyCC).** Чистый Move во время фокус-интента оценивается `pursuit_move_score(from_pos, to_pos, target_pos, reach)`:

| Условие | Score |
|---|---|
| `new_dist ≤ reach` — вошёл в threat bubble | `0.8` |
| closing (`Δ > 0`) — сократил дистанцию | `min(0.3 × Δ / reach, 0.3)` |
| retreat (`Δ < 0`) — увеличил дистанцию | `-min(0.1 × |Δ| / reach, 0.1)` (soft, не ломает обходы) |
| без изменений | `0.0` |

**Reach семантика** — "смогу ли я действовать на своём следующем meaningful action":
- FocusTarget: `active.speed + active.max_attack_range`
- ApplyCC: `active.speed + cc_reach(active, content)` (max range среди CC-способностей)

Enter-reach (0.8) выбран ниже Cast (1.0), чтобы Cast план всегда побеждал когда достижим. Closing capped at 0.3 — ниже viability threshold 0.5, значит "просто сближаюсь" не проходит guard в одиночку. Retreat soft (cap 0.1) — position/risk колонки доминируют над intent для обходных манёвров через choke/LoS.

**Viability threshold `FocusTarget=0.5` семантически = "уже почти в контакте"**, не "иду в нужную сторону". Фокусно: если best план не enter-reach этим тиком, guard переключает на достижимый `default_focus_target` — по дизайну, не случайное совпадение.

**Reposition tiered:**

| Условие | Score |
|---|---|
| `improvement ≥ reposition_min_improvement` | `improvement.min(2.0)` |
| `0 < improvement < min` | `0.0` |
| `improvement ≤ 0` + Cast | `−0.3` |
| `improvement ≤ 0` + Move | `−1.0` |

#### ProtectSelf branch (contract enforcement)

После adaptation, если intent == ProtectSelf: не-defensive планы с `EvaluationMode::Default` → `−∞` (contract mask). Defensive iff `raw_factors[i].self_survival ≥ 0.15` (`SELF_SURVIVAL_EPSILON`). Это заменяет старую tile/target-type эвристику: план с self-heal, armor-buff или выходом из danger-зоны попадает под порог независимо от структуры шагов. Планы с `mode != Default` маску не проходят — они уже вышли из-под ProtectSelf-контракта через ADAPTATION.

Случай «нет ни одного defensive» **обрабатывается раньше** — в ADAPTATION (`ProtectSelfNoDefensive` → все планы получают `mode=LastStand`), и затем contract mask никого не задевает.

## Adaptation Layer

Отдельный слой pipeline между SANITY (мягкие штрафы) и CONTRACT (intent-coherence masks). Отвечает за **value-function reassessment**: если факты, обнаруженные после measurement+correction, делают текущий `TacticalIntent` неадекватным оценочной моделью плана — переключает **режим оценки** (`EvaluationMode`) для этого плана и пересчитывает его intent-column.

### Зачем

Sanity работает с *ценой* (cost corrector). Intent определяет *функцию ценности*. Есть случаи, когда функция ценности сама становится неправильной по отношению к плану — например, план гарантирует смерть актора, значит `continue-to-exist value = 0`, и оценка «что я ещё хочу сохранить» неуместна. Раньше такие случаи лечились hard-маской `-∞` (lethal AoO) или rescue-веткой внутри `apply_protect_self` — оба костыли в неправильных слоях.

### Invariants

Слой узкий. Зафиксировано:

1. **ONE PASS** — вызывается один раз в `pick_action`, после `apply_sanity`.
2. **FACTS ONLY** — триггеры только snapshot-факты (`expected_aoo_damage ≥ hp`, `plan_is_defensive`, `global_intent`). Никаких post-score сравнений.
3. **NO PENALTIES / NO MASKS** — слой только маппит `(plan → EvaluationMode)` и триггерит rescore intent-column. Не умножает, не обнуляет.
4. **IDEMPOTENT** — повторный вызов на уже адаптированном состоянии — no-op. `EvaluationMode` меняется ≤ 1 раз на план.
5. **CONTRACT-NEUTRAL** — не знает про contract masks. Контракт применяется ПОСЛЕ и только к планам с `mode = Default`.

### `EvaluationMode`

```rust
enum EvaluationMode { Default, LastStand }
```

`Default` использует глобальный `TacticalIntent` для скоринга intent-column. `LastStand` переиспользует существующую `intent_score(step, &TacticalIntent::LastStand, …)` — `TacticalIntent::LastStand` остаётся data-type для rescore, но `select_intent` его никогда не выбирает (это job адаптации).

### `AdaptationReason`

| Reason | Триггер | Gate | Mode | Horizon |
|---|---|---|---|---|
| `ExpectedSelfLethal { aoo_dmg, actor_hp }` | `expected_aoo_damage(plan) ≥ actor_hp` | `intent != ProtectSelf` | `LastStand` (per-plan) | **step-local** (AoO per-transition) |
| `ProtectSelfNoDefensive` | ни один план не `plan_is_defensive` | `intent == ProtectSelf` | `LastStand` (глобально) | — (spatial) |
| `ProtectSelfFutile { pending_dot, actor_hp }` | `pending_dot_before_next_action(active) ≥ hp` **AND** ни один план не `plan_has_self_rescue` | `intent == ProtectSelf`, defensive option ∃ | `LastStand` (глобально) | **end-of-turn** (`sim_snapshots.last()`) |

**Horizon per threat type.** AoO fires внутри шага → step-local rescue невозможна суффиксом → смотрим per-step AoO bleed. DoT, в движке с гарантией «только текущий актор меняет состояние в рамках хода», тикает на ходу *applier'а*, после окончания хода отравленного — значит правильный horizon для doom-rescue = конец полного плана (`sim_snapshots.last()`). Два разных типа угроз → два разных horizon'а в одном слое.

`ExpectedSelfLethal` под ProtectSelf не срабатывает: если есть defensive options и doom-check не фатален, contract прав — актор не должен сам себя ставить под смертельный AoO. Если defensive нет → `ProtectSelfNoDefensive` делает глобальный switch. Если defensive есть, но pending DoT ≥ hp и ни один план не спасает → `ProtectSelfFutile` делает глобальный switch.

**MVP scope `ProtectSelfFutile`**: gate только под `intent == ProtectSelf`. Doomed-актор, у которого `select_intent` выбрал не-ProtectSelf (например, ушёл на safe tile, urgency не триггернула), — граничный случай, покрывается при появлении replay-свидетельства.

«Expected» в названии `ExpectedSelfLethal` — потому что `expected_aoo_damage` это EV-оценка (sim живёт на EV без crit-fail), а не гарантия смерти в живом бою. `pending_dot_before_next_action` — детерминированный snapshot-факт, без EV-проекции.

### Логи / debug

Для каждого плана в JSONL (schema v6+):
- `evaluation_mode: "default" | "last_stand"`
- `adaptation_reason: null | { kind: "expected_self_lethal", …} | { kind: "protect_self_no_defensive" } | { kind: "protect_self_futile", pending_dot, actor_hp }` (v8+)
- `base_score` — score до adaptation
- `adapted_score` — финальный (= `score`)

Если `adaptation.modes[best_idx] != Default`, `IntentReason` выбранного плана оборачивается в `IntentReason::Adapted { prior, reason }`.

### Что MVP1 НЕ решает

MVP1 — **архитектурный refactor**. Он убирает lethal-AoO hard-mask и перестаёт душить self-lethal планы в `-∞` — они возвращаются в pool и становятся сравнимыми. **Экономику размена** — выгодно ли умереть ради убийства конкретной цели — закрывает MVP2 (см. «Trade Economy» ниже).

## Trade Economy

Plan-level signed modifier, параллельный `summon_bonus`. Оценивает размен: ценность убитых врагов минус потерянные союзники минус стоимость собственной смерти, если план self-lethal. Применяется **после** composition-фазы, вне factor normalization — потому что `kill` factor даёт только бинарный сигнал «что-то убили», а модифицировать role-weights для «чего стоит размен» неправильно: trade экономика одинакова для всех ролей.

Живёт в `src/combat/ai/trade.rs`. Интегрируется в `scorer::finalize_scores` параллельно `plan_summon_bonus`.

### `unit_value(u)`

HP-equivalent actor-agnostic ценность юнита:

```
unit_value(u) = lifetime_rounds(u) × (offense + heal + cc)
```

| Слагаемое | Формула | Источник |
|---|---|---|
| `offense_projection` | `horizon_avg(u)` | resource-aware DPR из `scoring.rs` |
| `heal_projection` | best legal `SingleAlly + Heal` EV | `u.caster_ctx` + `u.abilities` |
| `cc_projection` | `max { Σ duration × u.threat : skips_turn statuses on target }` | `u.abilities` + `content.statuses` |
| `lifetime_rounds(u)` | **константа 2.0** | см. «Known limitations» |

**Инварианты:**

1. **Actor-agnostic** — зависит только от `u` и статического контента; никакой proximity, никакого relative threat. self/ally/enemy оцениваются одинаково.
2. **HP-equivalent units** — всё в «HP в минуту», слагаемые можно складывать.
3. **Нет внутреннего floor** — floor `UNIT_VALUE_FLOOR = 1.0` применяется только в знаменателе `trade_score`, чтобы сумма по трешу не раздувалась.

### `trade_delta(plan)`

Анализирует исходы плана **только в пределах commit-prefix** (first fired step для solo, [0..=1] для Move→Cast bundle). Tail steps — lookahead, следующий тик перепланирует.

```
trade_delta = Σ unit_value(killed_enemy)
            − Σ unit_value(lost_ally)
            − (self_lethal ? unit_value(self) : 0)
```

| Поле | Как считается |
|---|---|
| `killed_value` | Σ по `plan.outcomes[k].killed` для `k < prefix_len`, victim на вражеской команде |
| `lost_value` | то же для цели на команде актора (self-AoE FF тоже тут) |
| `self_lethal` | `expected_aoo_damage(active, plan, enemies) ≥ active.hp` ИЛИ actor в killed list |
| `self_lost` | `unit_value(active)` если self_lethal И actor **не** в killed list; иначе 0 (guard против double-count) |

В валидном commit-prefix AoO-релевантный Move всегда шаг 0, поэтому сравнение с `active.hp` (plan-start HP) точное — никакой self-heal не может прогнать до движения.

### `trade_score`

```
trade_score = tanh(delta / max(unit_value(self), UNIT_VALUE_FLOOR)) × TRADE_WEIGHT
```

Добавляется к final score **после** нормализации и role-composition. Tanh-squash гарантирует `trade_score ∈ [−TRADE_WEIGHT, +TRADE_WEIGHT]` — сатурация при «явно выгодном» или «явно катастрофическом» размене. Делитель на `unit_value(self)` нормирует по масштабу актора: дешёвый громила и дорогой мастер видят одну и ту же «форму» размена, не абсолютные HP.

`TRADE_WEIGHT = 0.5` — conservative launch default; повышение — только после replay-свидетельств, что self-trade-for-support не пробивается.

### Log schema v7

`PlanLogEntry.trade`:

```json
{
  "delta": 12.0,
  "killed": 16.0,
  "lost": 4.0,
  "self_lost": 0.0,
  "self_lethal": false,
  "score": 0.38
}
```

`score` в блоке — ровно тот increment, который `plan_trade_bonus` добавил к top-level `score`. Для plan'ов без размена (`delta == 0 && !self_lethal`) блок — null-ish (все нули); `replay_ai_log --verbose` в таких случаях не печатает trade-строку.

### Known limitations

- **lifetime_rounds — константа.** Phase 2c должна заменить на `clamp(eff_hp / incoming_dpr, 0.5, 3.0)` с actor-agnostic прокси для `incoming_dpr`. Сейчас танки получают ценность живучести только косвенно — через то, что их kit нечасто доходит до `offense + heal + cc`.
- **Taunt / forces_targeting redirect не оценивается.** Pure tanks скорятся у нижнего floor — consistent с существующей `role_value` иерархией (Tank 0.3). Если replay покажет «AI радостно меняет танка на крысу», это триггер для `redirect_value`.
- **Multi-cast scaling отсутствует в heal / cc.** Осознанно: resource limits, overheal, non-stacking stuns делают multi-cast projection оптимистичной. best-single-legal — консервативный underestimate.

### Resource Scarcity

```
scarcity = (swing_value - resource_ratio).clamp(-1.0, 1.0)
```
`resource_ratio = max(cost / current_pool)` по всем ресурсам.

**swing_value:**

| Условие | Бонус |
|---|---|
| Kill (kill_now > 0) | +0.8 |
| Kill role-value | +0.35 × `target.role.role_value()` |
| AoE hits > 1 | +0.2 × (hits − 1) |
| CC на high-threat unstunned | +0.5 × (threat/10) |
| Цель < 25% HP и есть free-attack | −0.3 |
| Round ≤ 1 | −0.15 |

## Базовый скоринг (outcome.rs::compute_score_core)

HP-эквивалентная оценка пары (ability, target). Живёт в `outcome.rs` как `pub(crate) fn compute_score_core(def, target, caster, content, danger) -> f32` (step 4.5 — перенесена из `scoring::score_action`, которая удалена). Результат = суммарная HP-value (damage/heal + status score). Вызывается helper'ами `estimate_expected_damage`, `estimate_rescue_value`, `estimate_hypothetical` и внутренне `factors::offensive::compute_aoe_damage` / `friendly_fire_penalty`.

### Damage
```
raw = max(0, expected - armor + damage_taken_bonus)
progress = min(raw / target.hp, 1.0)
score = raw × (0.5 + 0.5 × progress)
```

### Heal (urgency-weighted)
```
delta_pct = min(expected, missing_hp) / max_hp
horizon_sum = max(Σ damage_horizon, threat)
urgency = 1.0 + max(hp_missing, min(danger/hp, 1.0))  # capped at 2.0
score = delta_pct × horizon_sum × urgency
```

Urgency baked-in — включает `hp_missing` и `danger_at_target`. В волне 1 вся эта формула живёт в `estimate_rescue_value` и попадает в `outcome.rescue_value` напрямую. Step 3 (need layer) разделит: `outcome.rescue_value` → чистый effect, urgency → `NeedSignals.rescue_ally`.

### Status Effects (status_score)
```
skips_turn      → +threat × duration
damage_taken_δ  → +|delta| × duration
armor_δ         → +|delta| × duration
dot_dice        → +dice.expected() × duration
hp_percent_dot  → +ceil(max_hp × pct / 100) × duration
silence         → +threat × 0.5 × duration
speed_penalty   → +|bonus| × duration
```

### AoE
Сумма `compute_score_core` по enemies в зоне. Friendly fire вычитается через `friendly_fire_penalty` с весом `raw_dmg × (1 + raw_dmg / max_hp)`. Результат кладётся в `outcome.expected_damage` для AoE; consumer `compute_offensive` читает его напрямую из outcome.

### Critical Failure Adjustment
- Miss: `score × (1 - crit_chance)`
- ManaOverload: `score - crit_chance × mana_cost`
- CircuitBreach: `score × (1 - crit_chance) - crit_chance × mana_cost × 0.5`

Применяется через `factors::adjustments::crit_fail_adjusted` вокруг результата `compute_score_core`.

## Target Priority (target_priority.rs)

| Фактор | Вес | Формула |
|--------|-----|---------|
| Threat | 0.20 | `target.threat / max_threat` |
| Killability | 0.20 | `1 − eff_hp / eff_max_hp` |
| Threat density | 0.20 | `(threat / eff_hp) / max_density` |
| Vulnerability | 0.15 | `+0.3` если LOW_HP, `+0.2` если damage_taken_bonus > 0 |
| Proximity | 0.15 | `1 / (1 + distance)` |
| Role value | 0.10 | Support=1.0, Control=0.8, Ranged=0.7, Melee=0.5, Tank=0.3 |

`eff_hp = hp + armor + armor_bonus`.

## Position Evaluation

Линейная комбинация 3 карт влияния с весами по профилю. Escape (derived) не включён. Веса живут в `AiTuning.tables.axis_position_weights` (step 2.5, data-driven).

| Карта | Tank | Melee | Ranged | Control | Support |
|-------|------|-------|--------|---------|---------|
| danger | −1.0 | −0.9 | −1.8 | −1.5 | −2.5 |
| ally_support | 0.7 | 0.4 | 0.7 | 0.8 | 1.3 |
| opportunity | 0.9 | 1.5 | 1.0 | 0.8 | 0.5 |

## Plan Sanity Adjust

Мультипликативные штрафы после scoring. **Инвариант слоя: только мягкие penalty, никаких hard-масок.** Ранее-существовавший «lethal AoO → -∞» переехал в [Adaptation Layer](#adaptation-layer) как `ExpectedSelfLethal` переключение режима оценки.

| Проверка | Эффект | Условие |
|----------|--------|---------|
| **Survival квадратичный** | `×(1 − LOW_HP_FACTOR × hp_need × max(0, danger−0.5)²)`, пол 0.25 | всегда |
| **AoO bleed** | `×(1 − AOO_PENALTY_K × (aoo/hp)²)`, пол 0.25 | путь пересекает AoO (включая EV-летальные — их переоценивает adaptation) |
| **Healer exposure** | `×0.5` | non-support уходит от единственного healer'а |
| **LoS blindspot** | `×0.3` | RANGED финальная клетка без LoS |
| **Retreat trap** | `×0.5` | final_pos с < 2 свободных соседей |
| **Self-AoE** | `×0.5` | AoE с friendly_fire, кастер в зоне |
| **Synergy bonus** | `×1.1` | move в safer/better tile + useful cast |

## Hard Constraints (в generate_plans)

1. **Taunt** — SingleEnemy Cast только на taunted-целях.
2. **Team safety** — `pick_targets` из `allies_of` / `enemies_of`.
3. **Overheal** — SingleAlly на цели > 90% HP отбрасывается.
4. **Wasted CC** — single-target CC на оглушённой цели отбрасывается.
5. **Self-AoE friendly-fire** — если `enemies_hit < allies_hit × 2`.

## Pick Best Plan + Commit

После scoring + sanity:

1. **Mercy окно** `[best − mercy, best]` → rerank по `score − mercy × cruelty`, где `cruelty = kill_now + kill_promised×0.5 + min(0.5, cc × 0.1)`.
2. **Similarity window** для top-K: pool = top-K с `score ≥ best_after_mercy − window`.
3. **Случайный выбор** в пределах pool.
4. `commit_plan(plan, actor_pos)` → `(AiDecision, consumed)` — единственный source-of-truth для bundling rules (1 для solo / 2 для Move→Cast).
5. `record_committed_reservations(plan, consumed, ...)` — только consumed prefix + end-tile.

## Difficulty

| Параметр | Easy | Normal | Hard | Описание |
|----------|------|--------|------|----------|
| `awareness` | 0.55 | 0.80 | 1.00 | Сдвиг порогов в intent.rs |
| `decision_quality` | 0.30 | 0.75 | 1.00 | Derived → `score_noise` + `top_k_choice` |
| `intent_commitment` | 0.75 | 1.00 | 1.20 | Множитель веса `intent` |
| `survival_instinct` | 0.55 | 0.80 | 1.00 | Derived → reposition/defensive/survival thresholds |
| `resource_discipline` | 0.60 | 1.00 | 1.20 | Множитель веса `scarcity` |
| `coordination` | 0.40 | 0.90 | 1.30 | Overkill penalty + focus-fire bonus |
| `mercy` | 0.35 | 0.10 | 0.00 | Cruelty-shift в tie-breaker окне |
| `plan_max_depth` | 3 | 3 | 3 | Длина плана в beam search |
| `plan_beam_width` | 8 | 16 | 24 | Partial-plan survivor count per depth |
| `plan_step_discount` | 0.75 | 0.85 | 0.90 | `base^k` discount на cumulative-факторы |

`awareness` сдвигает **пороги решений** в intent.rs, а не множит нормализованные скоры (иначе сократится при симметричной нормализации).

**Производные lerp-кривые** (`survival_hp_threshold`, `reposition_min_improvement`, `awareness_danger_threshold`) раньше были hardcoded константами в `difficulty.rs`; step 2.6 перенёс endpoints `{lo, hi}` в `AiTuning.difficulty.*_curve` — методы профиля делают `lerp(curve.lo, curve.hi, tier_param)`. Формулы не изменились, значения редактируются в `assets/data/ai_tuning.toml`.

**Per-unit override scaffolding** (step 2.7): `UnitTemplateDef.ai_tuning_override: Option<AiTuningOverride>` (сейчас только `thresholds`) — позволяет quirk'ам (Berserker/Coward/Focused) сдвигать отдельные пороги. В `pick_action` при наличии override строится локальный `AiTuning` через `apply_override` и локальный `AiWorld` — downstream call-sites не меняются. В текущем контенте ни один unit не декларирует override, инфраструктура inert.

## Snapshot

`BattleSnapshot` — чистый снимок без Bevy-зависимостей (кроме Entity).

### UnitSnapshot

Позиция, HP/max_hp, armor + агрегаты `armor_bonus`/`damage_taken_bonus` (снимаются в build-time, обновляются через `refresh_status_aggregates` при status-mutation в sim), ресурсы (mana/rage/energy), speed (base + status_bonus на snapshot-time), список способностей, **`statuses: Vec<ActiveStatusView>`** (mirror `StatusEffects` component — `id`, `rounds_remaining`, `dot_per_tick`), threat, `AiTags`, `max_attack_range`, `aoo_expected_damage`, `summoner`.

### AiTags (bitflags)
```
LOW_HP | CAN_HEAL | CAN_CC | HAS_AOE | IS_STUNNED | FORCES_TARGETING | RANGED | MELEE_ONLY
```

## Influence Maps

- `danger`, `ally_support`, `opportunity` ∈ [0, 1]
- `escape` ∈ [-1, +1] — derived (`ally_support − danger`)

### Danger Map
Для каждого врага BFS по speed → достижимые тайлы + `hex_circle(max_attack_range)` → `danger += enemy.threat`. Норм: `/ Σ(enemy.threat)`.

### Ally Support Map
`support_weight(ally) × exp(-dist / λ)`, λ=2.5. Healer×2.0, melee×1.5, базовый=1.0.

### Opportunity Map
`target_value × exp(-dist / λ)`, λ=3.0. `target_value = 0.7 × (1 − hp%) + 0.3 × (threat / max_threat)`.

### Escape Map
Derived. Используется только в `pick_top_move_tiles` и debug overlay.

## Debug Overlay

`assets/data/settings.toml`:
```toml
[debug]
ai_debug = true
```

| Клавиша | Действие |
|---|---|
| `~` | Toggle overlay карт |
| `1`..`4` | Danger / AllySupport / Opportunity / Escape |

### Консольный лог

При `ai_debug = true` каждый AI-ход печатает: actor + intent + priority target + топ-5 планов + финальная decision. Formatter ходит по `&[TurnPlan]` напрямую через `ScoredStep::from_plan_committed(plan, actor_pos)` — никаких синтезированных адаптеров.

JSONL-лог с raw-факторами и всем пулом планов — через `AiLogger` (см. `src/bin/replay_ai_log.rs`).

---

## Extension Checklist

Куда смотреть при добавлении разных типов механик. Списки — стартовая точка, а не полная диагностика: Rust exhaustive-match ошибки при компиляции доведут до остальных принудительных точек.

**Общий принцип**: правишь ядро → правишь shared resolution (если meaningfully меняет исход каста) → правишь AI (enumerator → filters → scoring → intent). Каждый слой проходит один и тот же каст с разных сторон; пропуск в одном слое — это либо невалидное действие, либо неучтённое в планировщике.

### Новая способность (только TOML)

Файлы: `assets/data/abilities.toml` + соответствующие `classes.toml` / `unit_templates.toml` / `encounters.toml` для владельцев.

Код не трогаешь, **если** способность укладывается в существующие `TargetType`, `EffectDef`, `AoEShape`, `StatusOn`, `ResourceKind`. Если нет — см. соответствующий раздел ниже.

### Новый `TargetType`

Конкретный пример — `Ground` (см. git log вокруг fireball). Затрагивает:

| Файл | Что |
|---|---|
| `src/content/abilities.rs` | Вариант enum + парсер строки из TOML |
| `src/combat/actions/mod.rs` | match arm в `check_legality` (team/alive семантика) |
| `src/combat/resolution.rs` | `primary_target` match arm |
| `src/combat/ai/planning/sim.rs` | `primary` match arm |
| `src/combat/ai/planning/generator.rs::rank_targets` | Как перебирать кандидатов (сущности / клетки) |
| `src/combat/ai/scoring.rs` | Фильтры `estimate_st_damage`, `estimate_damage_horizon` — если offensive |
| `src/combat/ai/snapshot.rs` | `max_attack_range` фильтр — если это "атака" |
| `src/combat/ai/intent.rs` | LastStand +0.5 offensive, прочие intent score'ы если релевантно |
| `src/ui/ability_panel.rs::build_description` | Русская подпись «цель: …» |
| `src/ui/hex_grid/input.rs` | Логика клика (что происходит при выборе клетки / сущности) |
| `src/combat/command_input.rs` | Tab-цикл (что перебирать), Enter-конфирм |
| `docs/content-guide.md` | Строка в списке допустимых target_type |

Тесты: позитивный + негативный кейсы в `combat::actions::tests` + генератор-тест в `combat::ai::planning::generator::tests`.

### Новый `EffectDef`

| Файл | Что |
|---|---|
| `src/content/abilities.rs` | Вариант enum + парсер + `EffectDef::calc` (если даёт число урона/хила) |
| `src/combat/effects_outcome.rs` | `OutcomePrimary` ветка + dispatch в `compute_ability_outcome` |
| `src/combat/resolution.rs` | Обработка нового `OutcomePrimary` (writer / side effects) |
| `src/combat/ai/planning/sim.rs::apply_primary` | Как sim мутирует snapshot |
| `src/combat/ai/outcome.rs::compute_score_core` | HP-эквивалент (central formula; раньше был `scoring::score_action`, удалён в step 4.5) |
| `src/combat/ai/outcome.rs::build_step_outcome_estimate` (в generator.rs) | Если новый эффект должен заполнять поле `ActionOutcomeEstimate` (damage/heal/cc/etc.) |
| `src/combat/ai/role.rs::ability_vote` | Голос за ось |
| `src/combat/ai/factors/offensive.rs` | Обычно менять не надо — `compute_offensive` читает `outcome` vector; новые эффекты попадают через `build_step_outcome_estimate` |

### Новое поле `StatusDef`

| Файл | Что |
|---|---|
| `src/content/statuses.rs` | Поле структуры + парсер |
| `src/combat/statuses.rs` | Применение эффекта в реальной резолюции (tick / damage_modifier / etc.) |
| `src/combat/ai/snapshot.rs::status_bonuses` | Агрегация в `UnitSnapshot` если это численный бонус |
| `src/combat/ai/snapshot.rs::compute_tags` | Выставление `AiTag` если флаг — сигнал для интента |
| `src/combat/ai/outcome.rs::estimate_deny_value` / `compute_score_core::status_score` | Оценка ценности для планировщика (deny_value / status_score в outcome) |
| `docs/content-guide.md` | Комментарий в примере `[[statuses]]` |

### Новый `AiTag`

| Файл | Что |
|---|---|
| `src/combat/ai/snapshot.rs` | `AiTags` bitflag |
| `src/combat/ai/snapshot.rs::compute_tags` | Условие выставления |
| `src/combat/ai/intent.rs::select_intent` | Используется в лестнице выбора интента |
| Прочие consumer'ы тега (например, фактор scarcity читает `AiTags::IS_STUNNED`) |

### Новый `TacticalIntent`

| Файл | Что |
|---|---|
| `src/combat/ai/intent.rs` | Вариант enum |
| `src/combat/ai/intent.rs::select_intent` | Скоринг условия выбора (таблица в разделе «TacticalIntent») |
| `src/combat/ai/intent.rs::intent_score` | Alignment scoring на `ScoredStep` |
| `src/combat/ai/intent.rs` viability thresholds | Порог в viability guard |
| `src/combat/ai/intent.rs::AiMemory` | Stickiness continuation — `kind()` + сравнение last_intent (если применимо) |

### Новая `AoEShape`

| Файл | Что |
|---|---|
| `src/content/abilities.rs` | Вариант enum + парсер |
| `src/combat/effects_math.rs::aoe_cells` | Перечисление клеток |
| `src/ui/hex_grid/visuals.rs::update_hex_visuals` | Preview-рендер под ховером |
| `src/combat/ai/factors/aoe_hits.rs` | Покрытие enemies/allies (если формула нестандартная) |
| `src/ui/ability_panel.rs::build_description` | Строка-подпись формы |

### Новый фактор scoring'а

| Файл | Что |
|---|---|
| `src/combat/ai/factors/mod.rs` | Поле в `Factors` + `compute_factors` + нормализация (non-neg vs signed) |
| `src/combat/ai/role.rs::AXIS_FACTOR_WEIGHTS` | Весовая колонка на 5 ролей |
| `src/combat/ai/planning/scorer.rs` | Агрегация по шагам плана (sum / max / discounted) |
| `src/combat/ai/difficulty.rs` | Ручка difficulty, если фактор должен зависеть от сложности |
| Этот документ | Строка в таблице факторов |

### Новая `SanityCheck`

| Файл | Что |
|---|---|
| `src/combat/ai/planning/sanity.rs` | Функция-пенальти (multiplicative, floor'ится) |
| Этот документ | Строка в таблице Plan Sanity Adjust |

SanityCheck = только мягкая корректировка цены. Если у тебя новое правило «если *факт X*, функция ценности этого плана неверна → пересчитай под другим `EvaluationMode`» — это `AdaptationReason`, не `SanityCheck`.

### Новый `AdaptationReason`

| Файл | Что |
|---|---|
| `src/combat/ai/planning/adaptation.rs` | Вариант `AdaptationReason` + триггер (fact-based) + applicability gate |
| `src/combat/ai/intent.rs` или `scorer.rs` | Если требуется новый `EvaluationMode`, добавить вариант + обработку в `compute_plan_intent_sum` |
| `src/combat/ai/log.rs` | Serde-представление новой ветки reason в JSONL |
| `src/bin/replay_ai_log.rs` | Деструктура в verbose-выводе |
| Этот документ | Строка в таблице AdaptationReason |

### Ценность юнита / trade-экономика

| Файл | Что |
|---|---|
| `src/combat/ai/trade.rs` | `unit_value` slagаемое / `TradeBreakdown` поле / `trade_score` множитель |
| `src/combat/ai/planning/scorer.rs::plan_trade_bonus` | Уже читает через public helper — при изменении формулы больше ничего |
| `src/combat/ai/log.rs::TradeBlock` + `SCHEMA_VERSION` bump | Новое поле в JSONL / миграция старых логов через `#[serde(default)]` |
| `src/bin/replay_ai_log.rs::LoggedTradeBlock` | Mirror поля для деструктуризации |
| Этот документ | Строки в разделе «Trade Economy» |

SanityCheck-аналог: если новое правило «эта *часть плана* даёт отрицательный value неочевидным образом» — это **не** trade-ветвь. Trade отвечает только на «что умирает, чья ценность списывается» — любая другая динамика (урон не до смерти, перемещение важного юнита, position lock) уходит в SanityCheck или в отдельный factor.

### Новый `DifficultyProfile` параметр

| Файл | Что |
|---|---|
| `src/combat/ai/difficulty.rs` | Поле + трио значений easy/normal/hard + derived |
| Потребитель(и) | Чтение поля при принятии решения |
| Этот документ | Строка в таблице Difficulty |

### Новая константа тюнинга (`AiTuning`)

Вместо hardcoded в `const` — миграция в data-driven `AiTuning` (step 2a).

| Файл | Что |
|---|---|
| `src/combat/ai/tuning.rs` | Поле в `Thresholds` / `Tables` / `Difficulty` + дефолт |
| `assets/data/ai_tuning.toml` | Значение в соответствующей секции |
| Потребитель | Читает через `ctx.world.tuning.thresholds.<field>` (или `.tables.` / `.difficulty.`) |
| `src/combat/ai/tuning.rs::ThresholdsOverride` | Поле override если поле должно уметь перекрываться per-unit (scaffolding сейчас только для `Thresholds`) |

Правила:
- Классы thresholds (scalar) / tables (role-axis matrices) / difficulty (LerpCurve) — зависит от природы параметра.
- Формулы не менять в миграции — только перенос данных; golden-replay должен быть 0 diff.
- `DifficultyProfile` per-tier values (easy/normal/hard/epic) остаются в `difficulty.rs`; lerp endpoints для derived методов — в `AiTuning.difficulty`.

### Новое поле `ActionOutcomeEstimate`

Добавление новой оси в outcome vector — для future consumer'ов (terminal eval, critics, need layer).

| Файл | Что |
|---|---|
| `src/combat/ai/outcome.rs` | Поле в `ActionOutcomeEstimate` + docstring с семантикой |
| `src/combat/ai/planning/generator.rs::build_step_outcome_estimate` | Как populate (Cast / Move branches) |
| Consumer(ы) (`factors/offensive.rs`, `intent.rs`, `future_value.rs`, critics) | Чтение поля |
| `src/combat/ai/log.rs::SCHEMA_VERSION` | Bump при изменении shape annotation |
| Этот документ | Строка в таблице «Outcome vector (outcome.rs)» |

---

### Трассировка: «почему AI не использует Х?»

Если новая способность / механика в игре не задействуется AI, проверяй по порядку:

1. **Знает ли актор способность?** — `snapshot.rs::build` фильтрует по `actor.abilities`.
2. **Проходит ли legality?** — `check_legality` в `actions/mod.rs`. Запусти с прицельным `check_legality` в тесте или debug-логе.
3. **Генерит ли кандидатов?** — `generator.rs::rank_targets` match по `TargetType`. Пустой вектор = никогда не увидит каст.
4. **Проходит ли `ai_policy_ok`?** — эвристики overheal / wasted-CC / FF-ratio режут легальные, но невыгодные касты. Логируй возврат в тесте.
5. **Правильно ли populated outcome?** — `build_step_outcome_estimate` (generator.rs, step 4.2) заполняет 9 полей `ActionOutcomeEstimate` после sim. Если новый effect / status не попал в `expected_damage` / `deny_value` / `rescue_value` — compute_offensive прочтёт 0 и план получит низкий damage/cc/heal factor. JSONL-лог содержит annotation (schema v19+) — проверить там.
6. **Выживает ли beam-pruning?** — если `partial_score` низкий из-за неучтённого фактора, план режется на глубине. Покрутить `plan_beam_width` на hard для диагностики.
7. **Не роняется ли в sanity?** — `sanity_adjust_plans` умножает на малые факторы, но не зануляет; если итоговый score всё равно проигрывает — значит эвристики считают что-то другое лучше.
8. **Подходит ли `intent`?** — intent_score может увести на -1.0, сделав план хуже любых альтернатив. Проверь `intent_score` для своей цепочки `(intent, step, outcome)`.

Debug-оверлей + JSONL-лог (`AiLogger`) показывают топ-планы + raw-факторы + annotation (schema v19) — через них видно на каком слое запрос провалился.
