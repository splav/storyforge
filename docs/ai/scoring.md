# Utility Scoring

*Источник: `src/combat/ai/factors/`, `src/combat/ai/outcome/`, `src/combat/ai/planning/scorer.rs`, `src/combat/ai/planning/terminal.rs`, `src/combat/ai/repair/`, `src/combat/ai/role.rs`.*

Каждый `TurnPlan` оценивается по 10 факторам. Факторы делятся на два типа с разной нормализацией:

- **Non-negative** `[0, 1]`: `/ max` в батче
- **Signed** `[-1, 1]`: `/ max(|min|, |max|)` — симметричная нормализация

Финальный скор = `dot(normalized_factors, role_weights) + summon_bonus + trade_score + repair_bonus + noise`.

## ScoredStep — единица скоринга

`factors::ScoredStep<'a>` — ref-based view над `PlanStep` + caster tile на момент шага. Заменяет старый owned `ActionCandidate`; scoring не аллоцирует per step.

```rust
pub enum ScoredStep<'a> {
    Cast { ability: &'a AbilityId, target: Entity, target_pos: Hex, caster_tile: Hex },
    Move { caster_tile: Hex },
}
```

- Для Cast: `caster_tile` = позиция актора в момент каста.
- Для Move: `caster_tile` = destination пути.

Конструируется через `ScoredStep::from_plan_step(step, pre_step_pos)` (per-step сканнинг в scorer) или `ScoredStep::from_plan_committed(plan, actor_pos)` (view того, что `commit_plan` выполнит этот тик — для debug).

## Факторы

Файлы: `factors/step/` (per-step), `factors/plan/` (plan-уровень). Регистрация — `factors/registry.rs`; общие типы — `factors/mod.rs`.

| Фактор | Тип | Источник | Агрегация по шагам плана |
|---|---|---|---|
| `damage` | non-neg | `factors/step/damage.rs` | **Discounted sum** (`base_discount^k`) |
| `kill_now` | non-neg | `factors/step/kill_now.rs` | **Discounted sum** |
| `kill_promised` | non-neg | `factors/step/kill_promised.rs` | **Discounted sum** |
| `cc` | non-neg | `factors/step/cc.rs` | **Discounted sum** |
| `heal` | non-neg | `factors/step/heal.rs` | **Discounted sum** |
| `intent` | **signed** | `factors/plan/intent.rs` (через `intent_score`) | **discounted sum** (Cast и Move; latching ×0 после kill intent-цели) |
| `scarcity` | **signed** | `factors/step/scarcity.rs` | **Discounted sum** |
| `tempo_gain` | **signed** | `factors/plan/tempo_gain.rs` | **терминальная** (последний шаг) |
| `saturation` | **signed** | `factors/step/saturation.rs` | **Discounted sum** |
| `self_survival` | **signed** | `factors/plan/self_survival.rs` | **план-уровень** (single value) |

Дополнительные слои:
- `factors/aoe_hits.rs` — покрытие enemies / allies для AoE-форм.
- `factors/offensive.rs` — общий оffensive-помощник, читающий outcome facts + `policy::*` (`damage` / `heal` / `cc` / `kill_now` / `kill_promised`).
- `factors/adjustments.rs` — reservations + `crit_fail_adjusted`.

`evaluate_position` в `position_eval.rs` остаётся как вспомогательная функция — используется в `sanity.rs` (reposition-penalty) и `intent/mod.rs` (Reposition intent selection).

`tempo_gain` — мера прогресса к intent-target: `Δdist/speed` + `+0.3` за вход в cast-range + `+exit_danger_bonus`. Для Cast без предшествующего Move = 0. Без intent-target (Reposition, ProtectSelf, …) = 0.

`saturation` — штраф за повторное наложение баффа той же `buff_class` (ArmorBuff, Haste, DamageUp, Shield) на того же получателя, который уже несёт статус этого класса. `−0.4` за каждый избыточный класс. Читает `pre_snap` (симулированное состояние до шага) — intra-plan буфы автоматически учитываются.

`self_survival` — план-уровневая мера защиты актора: `Σ(self-heal EV / max_hp) + Σ(armor_bonus × 3 / max_hp) + max(0, danger(start) − danger(final_pos))`. Только самонаправленные касты (target == actor.entity). Используется как основа для ProtectSelf contract: план считается **defensive** iff `self_survival ≥ SELF_SURVIVAL_EPSILON (0.15)`.

**Plan-level модификаторы** (additive после composition, источник `modifiers/`):
- `summon_bonus` — за каждый `Summon` Cast в плане `dpr × cap_decay × sat_mult`, где `cap_decay = 1 − count/cap` (running, второй Summon ценится меньше), а `sat_mult = 0.65^total_allies`.
- `trade_score` — HP-equivalent оценка размена; см. [trade-economy.md](trade-economy.md).
- `repair_bonus` — goal-preserving repair affinity; см. ниже.

**Discount.** `plan_step_discount` (easy 0.75 / normal 0.85 / hard 0.90). step[k] — `base^k`.

## Outcome vector (`outcome/`)

`ActionOutcomeEstimate` — общий словарь «что произошло в одном шаге плана», **строгий fact vector** (step 4, финализирован в 4.13). Живёт на `TurnPlan.annotation: PlanAnnotation { outcomes: Vec<ActionOutcomeEstimate> }` — по одной записи на каждый `plan.steps[i]`. Populated через `outcome::builder::from_sim_step` после `sim::apply_step`, для consumer'ов без sim — `outcome::builder::hypothetical`.

**Инвариант**: outcome содержит только raw facts — никакого `× progress` / `× urgency` / `× (1 + raw/max_hp)` в populator'е. Любое value judgment — в [`policy::*`](policy.md).

Поля (17 fact-полей после step 4.12):

| Группа | Поля |
|---|---|
| Damage facts | `enemy_damage`, `enemy_damage_per_entity`, `ally_damage`, `ally_damage_per_entity`, `self_damage` |
| Kill facts | `p_kill_now` (1.0 if killed ≥1 enemy this turn), `p_kill_soon` (1.0 if DoT/pending will kill within horizon) |
| Status / control | `cc_turns_applied` (Σ skips_turn × dur), `vulnerability_applied`, `armor_shred_applied` |
| Support | `hp_restored` (raw clamped heal) |
| Movement | `path_max_danger`, `mp_spent` |
| Resources | `ap_spent`, `mana_spent`, `rage_spent`, `other_resource_spent` |

Consumers (`compute_factors`, `intent_score`, `future_value`, `picker`) читают поля напрямую, применяя policy formulas из `combat::ai::policy::*` для HP-equivalent score.

**Hypothetical outcome (no sim).** `outcome::builder::hypothetical(def, target, caster_ctx)` — для consumer'ов без sim context (`future_value::λ_attack`, `picker::record_committed_reservations`). First-class API, parallel к `from_sim_step`.

**JSONL schema v28+** (step 4.12, clean break): outcome shape — fundamental data; v27 logs дают `LogError::UnsupportedSchema`. Schema v23+ (step 5.6): `annotation.terminal` (`TerminalScore`, 8 axes).

## Terminal state evaluation (`planning/terminal.rs`)

`TerminalScore` — оценка плана по состоянию **доски после финального шага**, параллельная step-sum факторам. Populated в `terminal_state_score(plan, initial_snap, ctx)`, вызывается в `aggregate_factors_to_score`. Хранится в `plan.annotation.terminal`.

Реализации 8 осей живут в `factors/terminal/` (по файлу на ось):

| Ось | Кластер | Семантика |
|-----|---------|-----------|
| `exposure_at_end` | Defensive | Danger-map penalty финальной позиции |
| `next_turn_lethality` | Defensive | Вероятность гибели актора от всех врагов на следующем ходу |
| `secure_kill` | Offensive | Гарантированное убийство цели планом |
| `ally_rescue` | Offensive | Выход союзника из danger-зоны после хила |
| `board_control_gain` | Offensive | Улучшение контроля доски (threat × доступность цели) |
| `line_actionability` | Geometric | Число AoE-линий, открытых с финальной позиции |
| `density_value` | Geometric | Ценность кластеров врагов в reach |
| `pressure_spacing_zone` | Geometric | Тактическое расстояние до враждебных юнитов |

**Агрегация** в `aggregate_factors_to_score`: `dot(terminal_weights, terminal_score_vec)` суммируется в финальный скор плана. `terminal_weights = AxisProfile::terminal_weights(tuning)` — symmetric к `factor_weights`, sourced из `AiTuning.tables.axis_terminal_weights[5][8]` (`assets/data/ai_tuning.toml`). Каждая ось модулируется сигналом из `NeedSignals`:

| Ось | NeedSignal модулятор |
|-----|---------------------|
| `exposure_at_end`, `next_turn_lethality` | `× (1 + self_preserve)` |
| `secure_kill` | `× (1 + finish_target)` |
| `ally_rescue` | `× (1 + rescue_ally)` |
| `board_control_gain` | `× (1 + reposition)` |
| `density_value` | `× (1 + setup_aoe)` |
| `line_actionability`, `pressure_spacing_zone` | без модуляции |

**Калибровочное состояние**: defensive + offensive кластеры активны; geometric (`line_actionability`, `density_value`, `pressure_spacing_zone`) обнулены до фазы 2b mining-калибровки — ненулевые веса требуют corpus replay-данных для корректного баланса.

Полная декомпозиция: [`rework/step5_plan.md`](rework/step5_plan.md).

## Goal-preserving repair (`repair/`)

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

**Consumer в `RepairAffinityStage`:**

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

- factors: `kill_now × 1.2`, `kill_promised × 1.2`, `tempo_gain × 1.15`, `self_survival × 0.7`, остальные × 1.0.
- terminal: `exposure_at_end × 0.8`, `next_turn_lethality × 0.6`, `secure_kill × 1.3`, `board_control_gain × 1.3`, остальные × 1.0.

Sanity-mask и `ProtectSelfMaskStage` contract нетронуты — continuation меняет только axis weights в aggregator'е.

**`ContinuationOutcome`** для логов (`PlanDivergenceEntry.continuation_outcome`, schema v26+):

- `GoalPreservedMethodDelivered` — same goal, fresh = Cast/MoveAndCast → actor delivered the arc.
- `GoalPreservedInTransit` — same goal, fresh = Move-only → actor walking toward it.
- `GoalAbandonedReactive { source }` — forced by environment (taunt, panic, viability fallback).
- `GoalAbandonedVoluntary` — actor freely picked another intent (the real commitment-failure signal).
- `GoalAbandonedInvalidating` — target dead / position mismatch (hard invalidation).
- `GoalAbandonedTtlExpired` — goal age ≥ ttl.
- `NoStoredGoal` — первый тик / после Cast/EndTurn.
- `LegacyV25Abandoned { reason }` — pre-v26 entry с нераздельным `goal_abandoned`; voluntary/reactive split неизвестен.

`mine_ai_logs` секция **C6** агрегирует распределение — целевые таргеты (v26+ corpus): `goal_preserved (combined) ≥ 60%`, `goal_abandoned|voluntary ≤ 10%`, `method_delivered ≥ 10%`.

Полная декомпозиция: [`rework/step6_plan.md`](rework/step6_plan.md).

## Весовые таблицы по осям (AxisProfile)

Roles emergent — вектор весов по 5 осям (Tank/Melee/Ranged/Control/Support). Таблицы живут в `AiTuning.tables.axis_factor_weights` и `AiTuning.tables.axis_position_weights` (`assets/data/ai_tuning.toml`, step 2.4 / 2.5) — data-driven, редактируются без перекомпиляции.

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

### Инференс профиля (`role.rs::infer_profile`)

Каждая ability голосует за оси через **`tag_axis_vote`** (не pattern-matching по EffectDef). Вес голоса = `1 + total_cost`. Правила приоритизированы — возвращается первый matched тег (single-return), за исключением Offensive + ApplyCC, где Control добавляется к Melee/Ranged:

| Условие (тег / комбинация) | Распределение |
|---|---|
| `Rescue` | Support `+weight` |
| `Summon` | Support `+weight × 0.7` + Ranged `+weight × 0.3` |
| `Defensive` (без Offensive и без Peel) | Tank `+weight` |
| `Offensive` ranged (`SpellDamage` / AoE / `range.min ≥ 2`) | Ranged `+weight` (+ Control `+weight × 0.4` если есть ApplyCC) |
| `Offensive` melee | Melee `+weight` (+ Control `+weight × 0.4` если есть ApplyCC) |
| `ApplyCC` (без Offensive) | Control `+weight` |
| `Peel` | Tank `+weight × 0.7` + Support `+weight × 0.3` |
| `Mobility` (только) | Melee `+weight × 0.3` (aggro move) |
| Empty tags | 0 (только stat-based Tank floor добавится) |

Override: если `ai_tags_override` выставлен в TOML — используется он вместо derived tags (replace, не append).

Плюс **stat-based Tank bonus**: `(max_hp + armor × 2) / 20`, clamped [0.3, 2.0].
