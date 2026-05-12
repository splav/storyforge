# Tactical Intent

*Источник: `src/combat/ai/intent/mod.rs` (плюс `bands.rs` / `agenda.rs` / `considerations.rs` — см. [bands-agenda.md](bands-agenda.md)).*

AI выбирает один стратегический интент перед генерацией планов. Интент **не фильтрует жёстко** — выражается через фактор `intent` в scoring + viability guard. Step 11 расширил эту модель до агенды с N кандидатами; primary intent остаётся базовым ориентиром.

## Выбор интента (scored — max wins)

| Условие | Intent | Score |
|---------|--------|-------|
| HP < `survival_hp_threshold` И danger > `awareness_danger_threshold` | **ProtectSelf** (hard override) | — |
| HP < 40% И danger > 0 | **ProtectSelf** | (1 − hp%) × danger |
| CAN_HEAL И союзник (вкл. self) с HP < threshold | **ProtectAlly { ally }** | 1 − ally_hp% |
| Есть враг с FORCES_TARGETING | **FocusTarget { taunter }** (override) | 1.2 |
| Taunter И CAN_CC И не оглушён | **ApplyCC { taunter }** | 0.8 + threat × 0.1 |
| Нет taunter: враг убиваем И достижим за `speed + max_attack_range` | **FocusTarget { killable }** | 1.2 + (1 − hp%) × 0.3 |
| Нет taunter | **FocusTarget { default }** | 0.5 + prio × 0.3 |
| CAN_CC И есть не-оглушённый враг | **ApplyCC { target }** | 0.8 + threat × 0.1 |
| HAS_AOE И враги кластерируются (≤ 2) | **SetupAOE** | 0.7 + clusters × 0.2 |
| pos_eval(текущая) < `awareness_reposition_threshold` | **Reposition** | 0.3 + gap × 0.4 |

**ProtectAlly threshold** — role-aware: `0.5 + profile.support × 0.2`.
Stickiness bonus `+0.25` за continuation (+`0.15` если target тот же), до 3 ходов.

`TacticalIntent::LastStand` остаётся data-type для rescore через `EvaluationMode`, но `select_intent` его никогда не выбирает (это job [adaptation](adaptation.md)).

## Intent viability guard

`ViabilityStage` (`pipeline/stages/viability.rs`): если `max(intent_factor)` по планам ниже порога — intent переключается через `default_focus_target(active, snap, plans, actor_pos, exclude)`. Reachable target извлекается через `ScoredStep::from_plan_committed` над каждым планом.

| Intent | Порог viability |
|--------|---|
| Reposition | 0.01 |
| FocusTarget | 0.5 |
| ApplyCC | 0.5 |
| ProtectAlly | 0.5 |
| SetupAOE | 0.01 |
| ProtectSelf / LastStand | — (спец-ветка) |

## Intent-скоринг

`intent_score(intent, step, step_ctx, outcome) -> f32` вычисляет alignment одного шага плана. `outcome` — `&ActionOutcomeEstimate` для текущего шага (из `plan.annotation.outcomes[idx]`).

**`FocusTarget` и `ApplyCC`** используют dot-product факторов × intent-специфичный вектор весов (`IntentWeights`). Вначале `compute_factors(step_ctx, step, outcome)` читает поля outcome (damage / kill_now / kill_promised / cc / heal); затем `filter_offensive_for_target` обнуляет offensive-оси для шагов, не направленных на интент-цель:

| Шаг | Offensive-оси |
|-----|---------------|
| Cast → focus entity напрямую | полный кредит |
| Cast → AoE, покрывающий тайл focus entity | × 0.6 |
| Cast → другая цель / нет цели | обнулить |
| Move | обнулить (geometry hook считает через pursuit) |

После фильтрации dot-product с:

| Intent | Вектор весов |
|--------|-------------|
| FocusTarget | `kill_now × 2.0, kill_promised × 0.3, damage × 1.0, cc × 0.5` |
| ApplyCC | `cc × 1.5, damage × 0.3` |

**Move во время `FocusTarget` / `ApplyCC`**: `pursuit_move_score(from, to, target, reach)` (без факторов).

**`ProtectSelf`, `ProtectAlly`, `SetupAOE`, `LastStand`** сохраняют прежние формулы:

| Intent | Cast score | Move score |
|--------|-----------|-----------|
| Reposition | tiered | tiered |
| ProtectSelf | self-heal/self-buff на self = 1.0; иначе `1 − danger(tile)` | `1 − danger(tile)` |
| ProtectAlly | 1.0 heal ally; −0.3 heal wrong; 0.5 tile adj | 0.5 если adj к ally |
| SetupAOE | hits/total или −0.3 single-target | 0.0 |
| LastStand | dmg + kill + CC offensive combo | −0.3 |

**Почему factor-based для FocusTarget / ApplyCC?** Исправляет S5: низкоурон удар (1 дамага через броню) больше не получает тот же alignment credit 1.0, что убивающий удар.

### Pursuit Move score (FocusTarget / ApplyCC)

| Условие | Score |
|---|---|
| `new_dist ≤ reach` — вошёл в threat bubble | `0.8` |
| closing (`Δ > 0`) — сократил дистанцию | `min(0.3 × Δ / reach, 0.3)` |
| retreat (`Δ < 0`) — увеличил дистанцию | `−min(0.1 × |Δ| / reach, 0.1)` (soft, не ломает обходы) |
| без изменений | `0.0` |

**Reach семантика** — "смогу ли я действовать на своём следующем meaningful action":

- FocusTarget: `active.speed + active.max_attack_range`
- ApplyCC: `active.speed + cc_reach(active, content)` (max range среди CC-способностей)

Enter-reach (0.8) выбран ниже Cast (1.0), чтобы Cast план всегда побеждал когда достижим. Closing capped at 0.3 — ниже viability threshold 0.5, значит "просто сближаюсь" не проходит guard в одиночку. Retreat soft (cap 0.1) — позиционные/risk колонки доминируют над intent для обходных манёвров через choke / LoS.

**Viability threshold `FocusTarget = 0.5` семантически = "уже почти в контакте"**, не "иду в нужную сторону".

### Reposition tiered

| Условие | Score |
|---|---|
| `improvement ≥ reposition_min_improvement` | `improvement.min(2.0)` |
| `0 < improvement < min` | `0.0` |
| `improvement ≤ 0` + Cast | `−0.3` |
| `improvement ≤ 0` + Move | `−1.0` |

## ProtectSelf branch (contract enforcement)

`ProtectSelfMaskStage` (`pipeline/stages/protect_self.rs`) после adaptation: если intent == ProtectSelf — не-defensive планы с `EvaluationMode::Default` → `−∞` (contract mask). Defensive iff `raw_factors[i].self_survival ≥ 0.15` (`SELF_SURVIVAL_EPSILON`). Это заменяет старую tile/target-type эвристику: план с self-heal, armor-buff или выходом из danger-зоны попадает под порог независимо от структуры шагов. Планы с `mode != Default` маску не проходят — они уже вышли из-под ProtectSelf-контракта через [adaptation](adaptation.md).

Случай «нет ни одного defensive» **обрабатывается раньше** — в ADAPTATION (`ProtectSelfNoDefensive` → все планы получают `mode=LastStand`), и затем contract mask никого не задевает.

## AiMemory

`AiMemory` (`intent/mod.rs`):

- `last_intent: Option<IntentKind>` — для stickiness.
- `last_goal: Option<StoredGoalContext>` — для goal-preserving repair (см. [scoring.md](scoring.md#goal-preserving-repair)).
