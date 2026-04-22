# AI Rework — Developer Plan

Шаги имплементации. Контекст и acceptance — [`docs/ai_rework.md`](ai_rework.md).

Фиксация: 2026-04-22, пост-step-4a.

---

## Shipped ✅

Завершённые шаги с указателями на ключевые коммиты / документацию. Детальные старые спеки в истории git; текущий код и тесты — источник истины.

| Step | Суть | Код | Commit |
|---|---|---|---|
| 0 | `replay_ai_log` compile fix + campaign inference | `src/bin/replay_ai_log.rs` | pre-e1f0d38 |
| 0.5 | Baseline corpus metrics (`repeated_tile`, `zero_net_move`, `post_cast_retreat`) | `replay_ai_log.rs` | pre-e1f0d38 |
| 1 | `tempo_gain` → net displacement (start→final) | `factors/tempo.rs` | e1f0d38 |
| 1b | `intent_sum` pure-move shortcut (`pursuit_move_score` на `actor_start → plan.final_pos`) | `planning/scorer.rs::compute_plan_intent_sum` | e1f0d38 |
| 1c | `intent_sum` post-cast tail shortcut (single `pursuit_move_score` после первого Cast) | `planning/scorer.rs` | e1f0d38 |
| 2 / 2.5 | Schema v15 + gate telemetry поля | `combat/ai/log.rs` | f5b2b0a |
| 3 | Tiered killable gate под `FocusTarget`: `None / Pressure / CanFinish`, live-pool (`mode == Default && scores.is_finite()`), intent-coherent detection и keep-set | `planning/killable_gate.rs` + wiring | 0952c96 |
| Track A | Replay метрики → ground-truth (committed-prefix kill, intent-coherent `has_real_kill_line`) | `bin/replay_ai_log.rs` | 7caac3a |
| 4a | Phantom-tail фикс для `self_survival.exit_danger` — aggregation по committed prefix, не `plan.final_pos` | `factors/survival.rs` | d5a1078 |

Ключевые инварианты (закреплены в коде и тестах):

- **Killable gate** композирует с предыдущими масками: strength и keep-set читают live-pool. Regression guard'ы — `gate_ignores_plans_already_masked_by_prior_layer`, `can_finish_ignores_collateral_kill_line`.
- **Replay ↔ production parity**: `KILLABLE_ALPHA = 0.3` и семантика `plan_is_offensive_vs` синхронизированы между `killable_gate.rs` и `replay_ai_log.rs` через `// KEEP IN SYNC` комментарии.
- **Phantom-tail gating**: `intent_sum` (step-1c) и `self_survival` (step-4a) считаются по committed prefix. `CommittedPrefix` enum из `planning/types.rs` — канонический decomposition.

---

## Активные треки

Два независимых трека, идут параллельно.

### Track 1 — закрыть текущую итерацию

#### Step-4a measurement (pending)

Прогнать 8 боёв (те же encounters) на post-4a binary, replay на новом corpus'е, сравнить с `logs/baseline_20260422_step3.txt`.

**Acceptance** (`ai_rework.md §4.1`):

| Метрика | Цель | Baseline |
|---|---|---|
| `panic_leak_rate` | ≤ 5% | 16.7% |
| `kill_conversion_rate` (guard) | ≥ 75% | 80% |
| `killable_non_offensive_rate` (guard) | остаётся 0% | 0% |
| `repeated_tile` / `zero_net_move` / `post_cast_retreat` (guards) | Δ ≤ 5 pp | 9.9% / 6.2% / 20% |
| `phantom_tail_chosen` / `flips` (guards) | Δ ≤ 5 pp | 31.7% / 65% |

Оба наблюдаемых leak'а в baseline (HP=1 Проводник, HP=2 Одержимый — оба `Cast @ enemy + phantom retreat`) должны закрыться: phantom retreat больше не даёт exit_danger credit.

**Ожидаемый output**: файл `logs/baseline_20260422_step4a.txt` с обновлённой табличкой. Если acceptance зелёный — step-4a подписан, иначе investigation.

#### Step-5 — Summon saturation + intent-specific credit filter

**Триггер**: §2.3 оригинального плана. Не замерено на post-step-3 corpus — возможно уже подавлен gate'ом, возможно ещё проявляется. Замер на baseline_step3 + replay → если `summon_spam_rate > 1%`, step-5 в работу.

**Файлы**:
- `src/combat/ai/factors/saturation.rs` — расширение на summon-class.
- `src/combat/ai/intent.rs::intent_score` — intent-specific credit filter.
- `src/combat/ai/planning/scorer.rs::plan_summon_bonus` — калибровка с per-template saturation.

**Что делать**:

1. **Saturation axis расширить на summon.** Добавить `summon_saturation_penalty(ability, snap, caster)`:
   - Для `EffectDef::Summon { template, .. }` считать `active_count = snap.units.filter(|u| u.summoner == Some(caster) && u.template == template && u.is_alive()).count()`.
   - Penalty = `-0.4 × active_count`.
   - Складывается с существующим `buff_saturation_penalty` в общий saturation axis.

2. **Intent-specific credit filter** в `intent_score`. Для `FocusTarget / ApplyCC / ProtectAlly` правило:
   ```rust
   if let Some((_, _, cast_target)) = cast {
       if cast_target != intent.target().unwrap_or(Entity::PLACEHOLDER) {
           return 0.0; // cast не на intent target — нет credit
       }
   }
   ```
   `SetupAOE / ProtectSelf / LastStand / Reposition` — правило **не** применяется (нет single-target).

3. **Калибровка**. `plan_summon_bonus` в `scorer.rs` сейчас использует `saturation_mult = 0.65^total_allies` как coarse bound. Оставить; per-template penalty из шага 1 умножается поверх.

**Тесты**:
- `factors/saturation::tests::summon_saturation_per_template` — 2 живых storm_spirit → penalty = −0.8.
- `factors/saturation::tests::different_templates_independent` — разные template'ы считаются независимо.
- `intent::tests::summon_no_credit_under_focus_target` — `FocusTarget(enemy_T)`, `Cast summon → Move`. Intent_score для Cast-шага = 0.
- `intent::tests::summon_earns_credit_under_setup_aoe` — `SetupAOE`, `Cast summon`. Intent_score сохраняется.
- Regression: legitimate buff-стэк (haste+armor у танка) не триггерит новые penalty.

**Acceptance** (`ai_rework.md §4.2`):

| Метрика | Цель |
|---|---|
| `summon_spam_rate` (≥3 summon одного класса подряд за 5 ходов у одного актора) | ≤ 1% |
| Legitimate buff-стэк (regression-тест) | pass |

Для `summon_spam_rate` метрика должна быть добавлена в `replay_ai_log.rs` (новая, не существует). Простая реализация: per-actor sliding window по round, счётчик summon'ов одного template.

**Риск**: средний. Intent-specific credit filter в `intent_score` затрагивает большой объём поведения, нужен полный corpus-replay перед merge. Guard: Δ на существующих acceptance-метриках ≤ 5 pp.

---

### Track 2 — Phase 7 prototype (offline)

Параллельно Track 1. Production-pipeline не вызывает prototype-код: новый модуль — pure helper, единственный потребитель — `replay_ai_log --phase7-prototype`. Поэтому cfg-gate не нужен, но `score_plans_prototype` остаётся `pub` и не используется production'ом — это и есть "offline".

#### Мотивация

Step-1c и step-4a — **две точечные заплатки** одного phantom-tail bug'а. Третий candidate (`tempo_gain` → `plan.final_pos`) закроет ещё одну ось, но паттерн продолжит размножаться. Phase 7 decomposition:

```
Score(plan) = PrefixScore(committed_prefix) + γ · FutureValue(committed_state)
```

архитектурно решает все такие случаи сразу.

#### Целевые артефакты

- `src/combat/ai/planning/future_value.rs` — новый модуль с `future_value_from_committed_state`, `score_plans_prototype`, `plan_prefix_only`.
- `src/bin/replay_ai_log.rs` — флаг `--phase7-prototype`, dual-метрики, новые `ranking_change_rate` и `plateau_tie_rate`.
- `logs/phase7_prototype_<date>.txt` — output прогона corpus'а + decision table.

#### Этапы

Независимы в плане review, зависимы по коду (каждый следующий строит на предыдущем). Между этапами — отдельный commit, чтобы progress видно в git log.

##### Step-2A — `plan_prefix_only` + skeleton `future_value.rs`

**Файлы**:
- `src/combat/ai/planning/future_value.rs` (new).
- `src/combat/ai/planning/mod.rs` — `pub mod future_value;` + re-export.
- `src/combat/ai/planning/types.rs` — опционально: `TurnPlan::prefix_only()` как метод (либо free-function в `future_value.rs`, выбор реализатора — что естественнее читается).

**Что делать**:
1. `plan_prefix_only(plan: &TurnPlan) -> TurnPlan` — клонирует первые `committed_step_count()` элементов `steps / outcomes / sim_snapshots`. `final_pos` = позиция актёра после последнего prefix-шага (для `EndTurn` — `plan.final_pos`; для `MoveOnly`/`MoveThenCast` — last tile пути; для solo `Cast` — prefix сохраняет `plan.final_pos` от pre-cast позиции). `residual_ap`/`residual_mp` — из соответствующего `sim_snapshots[prefix_len-1]`, либо (если `sim_snapshots.is_empty()` — deserialized plan) — оставить исходные значения (консервативно).
2. `future_value_from_committed_state(...)` — skeleton с только `λ_pos` компонентом (через `evaluate_position`). λ_attack и λ_mob — `todo!()` + `#[allow(dead_code)]` константы, либо возвращают 0.0 с TODO-комментарием. Signature и docstring финальные.

**Тесты**:
- `plan_prefix_only::tests::{end_turn, solo_cast, move_only, move_then_cast, sim_snapshots_truncated, deserialized_plan_sim_empty}`.
- `future_value::tests::pos_component_reads_position_eval` — minimal sanity что λ_pos работает.

##### Step-2B — `future_value` attack + mobility components

**Файлы**:
- `src/combat/ai/planning/future_value.rs` — дополняет skeleton.

**Что делать**:
1. **λ_attack = 0.5 × best-score-action against top-3 targets by `target_priority`**. 
   - Reachable tiles из `committed_pos` с бюджетом `active.speed + active.max_attack_range` — используется `planning::reach::reach_from` с pseudo-unit-snapshot в committed_pos (или ручной хелпер: для prototype допустима упрощённая BFS по `snap.units` как blockers).
   - Top-3 врагов: сортировать `snap.enemies_of(active.team)` по `target_priority(active, e, snap)`. Для каждого — берём best `score_action(ability, target, ctx, content, danger)` по `active.abilities` (требует `CasterContext` из `active`). Возвращаем `max` по всем (ability, target) парам.
   - **Упрощение**: для prototype можно не моделировать перемещение — смотреть `score_action` от текущей `committed_pos`, бюджет reach используется только чтобы отфильтровать недостижимые цели (в attack range от committed_pos учитывая speed).
2. **λ_mob = 0.1 × reachable_tile_count(committed_pos, speed) / max_mobility**. `max_mobility` = константа (напр. 30) либо `snap.units.map(mobility).max()`. Нормализация на max_mobility keeps output in roughly [0, 1].
3. Вес `γ = 0.25` — хардкод-константа `PHASE7_GAMMA` с `KEEP IN SYNC` комментарием для replay.

**Тесты** (каждый тест — один компонент, не ансамбль):
- `attack_component_picks_best_reachable_target`.
- `attack_component_zero_when_no_enemies`.
- `mobility_component_scales_with_reachable_count`.
- `future_value_sums_components` — regression guard на композицию.

##### Step-2C — `score_plans_prototype`

**Файлы**:
- `src/combat/ai/planning/future_value.rs` — `pub fn score_plans_prototype`.

**Что делать**:
1. Для каждого plan:
   - `prefix = plan_prefix_only(plan)`.
   - `prefix_factors = compute_plan_factors(&prefix, intent, ctx)` (используя production `compute_plan_factors`, чтобы в prefix попадали `intent_sum` / `tempo_gain` / `self_survival` уже на укороченной последовательности).
   - Накопить все `prefix_factors` в матрицу.
2. `prefix_scores = finalize_scores(&prefix_plans, &prefix_factors_matrix, ctx)` — production normalisation / noise / summon / trade по prefix-плану.
3. Для каждого plan: `score[i] = prefix_scores[i] + γ × future_value_from_committed_state(active, committed_prefix_end_pos(plan, active.pos), snap, maps)`.
4. Output `Vec<f32>`.

**Что такое `committed_prefix_end_pos`**: `plan_prefix_only(plan).final_pos` — одна точка правды.

**Тесты**:
- `score_plans_prototype::tests::empty_plans_returns_empty`.
- `score_plans_prototype::tests::phantom_tail_plans_tie_with_tailless_equivalents` — два плана, отличающиеся только phantom-tail'ом после Cast, должны получить одинаковый prototype-score (кроме noise). Central regression — это основной бенефит prototype.
- `score_plans_prototype::tests::longer_prefix_wins_over_shorter_with_same_end_state` — smoke.

##### Step-2D — `--phase7-prototype` в `replay_ai_log` + новые метрики

**Файлы**:
- `src/bin/replay_ai_log.rs` — флаг, dual-pipeline, метрики.

**Что делать**:
1. **CLI**: добавить `--phase7-prototype` (bool, default false).
2. **Dual pipeline**: когда флаг включён, для каждого entry считать **две** ranking'а (baseline + prototype) и сравнивать `committed_action_key` у `top_post_baseline` vs `top_post_prototype`. Baseline pipeline — как сейчас (production scoring).
3. **Новые метрики** в `Metrics`:
   - `ranking_change_rate` = (entries где committed_action_key разошёлся) / (total). Печатается только при `--phase7-prototype`.
   - `plateau_tie_rate` = (entries где top-K c `max − min < 0.05` содержит ≥ 3 планов) / (total). Печатается для ОБЕИХ ranking'ов (baseline и prototype) — это основной decision criterion.
   - `phantom_tail_flips_committed` под prototype — re-use существующей логики, но на prototype-ranking.
4. **Dual-emission**: когда флаг включён, summary печатает "== Baseline ==" и "== Phase7 Prototype ==" блоки со всеми существующими метриками из `§4.1/4.2/4.3` (вычисленными на соответствующем ranking'е). Это нужно для regression-сравнения в decision criteria.
5. **Invariance**: когда флаг выключен — поведение replay'а побайтно совпадает с текущим (no-op, даже `ranking_change_rate` / `plateau_tie_rate` не печатаются).

**Тесты**:
- Добавить отдельный test-case (unit либо небольшой integration) на `plateau_tie_rate`: синтетический вектор scores, проверка формулы.
- Smoke: `cargo run --bin replay_ai_log -- logs/<any>.jsonl --phase7-prototype` не падает.

##### Step-2E — corpus run + decision note

**Что делать**:
1. Прогнать 12-log corpus через `replay_ai_log --phase7-prototype --metrics-summary` (8 post-step-3 логов + 4 baseline_final).
2. Записать output в `logs/phase7_prototype_20260422.txt` (либо актуальная дата).
3. Дополнить `docs/ai_rework.md §4.4` / `docs/ai_rework_plan.md` этой секцией таблицей decision criteria:
   - `phantom_tail_flips_committed` baseline vs prototype (target: ≥ 40% drop).
   - `plateau_tie_rate` baseline vs prototype (target: < 10%).
   - Δ на acceptance-метриках (target: ≤ 5 pp).
4. Вердикт: **3/3** → "Phase 7 merge в следующей итерации"; **2/3** → "design-doc перед merge"; **≤ 1/3** → "прототип не оправдан, назад к точечным фиксам".

###### Results (run 2026-04-22)

Corpus: 12 logs (8 post-step-3 + 4 baseline_final). 222 entries total.

| Criterion | Threshold | Baseline | Prototype | Δ | Pass |
|---|---|---|---|---|---|
| phantom_tail_flips_committed | ≥40% relative drop from 64.7% (→ prototype ≤ 38.8%) | 64.7% | 47.6% | -17.1 pp (-26% relative) | ✗ |
| plateau_tie_rate | prototype < 10% (from >20%) | 24.3% | 20.3% | -4.1 pp | ✗ |
| Δ acceptance (killable_non_offensive, kill_conversion, post_cast_retreat, repeated_tile, zero_net_move) | all ≤ 5 pp | — | — | max +23.5 pp (killable_non_offensive: 0%→23.5%; kill_conversion: 88.2%→64.7%; post_cast_retreat: 23.5%→33.3%) | ✗ |

**Verdict: 0/3 → prototype не оправдан, возвращаемся к точечным фиксам.**

Анализ сбоев:

- **phantom_tail_flips_committed** снизился только на 26% относительно (с 64.7% до 47.6%), тогда как цель была ≥40%. FutureValue частично нивелирует phantom-tail различия через λ_mob (mobility из committed_pos), но λ_attack и λ_pos создают новые расслоения, не связанные с prefix-качеством.

- **plateau_tie_rate** практически не изменился (24.3% → 20.3%). `pursuit_move_score` step-function (flat 0.8 в attack range) порождает ties на уровне PrefixScore, и FutureValue с текущим γ=0.25 и λ-весами недостаточно сильна, чтобы дифференцировать их.

- **Acceptance деградация** — главная находка: `killable_non_offensive_rate` вырос с 0% до 23.5%, а `kill_conversion_rate` упал с 88.2% до 64.7%. Вероятная причина: prototype перевзвешивает планы по FutureValue(committed_state), где committed_state после Cast часто хуже positionally, чем committed_state после Move-then-Cast. В результате gate-passed offensive планы смещаются ниже non-offensive в prototype ranking. Это структурная проблема: `score_plans_prototype` применяет γ·FV поверх уже работающего killable gate, что может нарушать gate-инварианты.

Следствие: prototype в текущей форме не готов к merge. Точечные фиксы (tempo clean-up, `post_cast_retreat` via step-3 extension) предпочтительны.

Full output: `logs/phase7_prototype_20260422.txt`.

#### Decision criteria

| Сигнал | Threshold |
|---|---|
| `phantom_tail_flips_committed` на prototype | ≥ 40% снижение от baseline 65% |
| `plateau_tie_rate` (top-K с `max−min<0.05`) | текущий > 20% → prototype < 10% |
| Δ на acceptance §4.1/4.2/4.3 | ≤ 5 pp на всех |

**3/3** → Phase 7 merge в следующую итерацию (multi-PR разбиение: prefix-factors, future_value integration, schema bump, weight recalibration, sanity/mask refit, test-refactor).  
**2/3** → доп. design doc перед commit.  
**≤ 1/3** → prototype не оправдан, возвращаемся к точечным фиксам (например, tempo clean-up).

#### Что prototype НЕ закрывает

- Panic_leak (step-4a закрывает сейчас).
- Summon spam (step-5 scope).
- Existing acceptance — prototype только сравнивает, не перемеряет контракт.

#### Риск prototype

Нулевой для production (offline). Риск track'а — timeline (если decision blocked доп. design doc'ом, Phase 7 merge откладывается).

---

## Порядок и параллелизм

```
[shipped: 0 → 0.5 → 1 → 1b → 1c → 2/2.5 → 3 → Track A → 4a]
                                                         │
                                                         ├─→ Step-4a measurement ─→ Step-5 ─→ итерация закрыта
                                                         │
                                                         └─→ Phase 7 prototype [2A → 2B → 2C → 2D → 2E] ─→ decision → next iteration
```

- Track 1 и Track 2 независимы.
- Step-5 после step-4a measurement (чтобы видеть чистый baseline для summon-метрики).
- Phase 7 prototype — own worktree, никого не блокирует.
- **Phase 7 merge НЕ в текущей итерации** — только decision. Сам merge — следующая итерация с полноценным design-doc'ом.

---

## Вне scope

- Canonical-набор факторов (следующая итерация).
- Marginal board value для summon'ов (тех долг).
- Trade economy, difficulty knobs — изолированы.
- Intent selection (`select_intent`) — меняем как план оценивается, не как intent выбирается.
- Plateau-ties от step-function `pursuit_move_score` — Phase 7 FutureValue differentiation.

---

## Справочник: completed steps (deep detail)

Если нужно вспомнить, **почему** шаг был сделан именно так, — читай код + тесты:

- Step-3 gate (invariants, tiers): `src/combat/ai/planning/killable_gate.rs` docstring + 9 тестов.
- Step-1c intent_sum shortcut: `src/combat/ai/planning/scorer.rs::compute_plan_intent_sum` docstring.
- Step-4a phantom-tail fix: `src/combat/ai/factors/survival.rs` — `committed_prefix_final_pos` helper + 6 regression тестов.
- Track A replay metric semantics: `src/bin/replay_ai_log.rs::plan_committed_prefix_kills_target` + 4 теста.

Исторические решения (user critiques про live-pool и intent-coherent detection, ally-split drop) зафиксированы в commit messages коммитов выше. Git log — авторитетный источник.
