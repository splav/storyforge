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

Параллельно Track 1. Отдельный worktree, production-код не меняется.

#### Мотивация

Step-1c и step-4a — **две точечные заплатки** одного phantom-tail bug'а. Третий candidate (`tempo_gain` → `plan.final_pos`) закроет ещё одну ось, но паттерн продолжит размножаться. Phase 7 decomposition:

```
Score(plan) = PrefixScore(committed_prefix) + γ · FutureValue(committed_state)
```

архитектурно решает все такие случаи сразу.

#### Что делать в prototype

1. **Новый модуль** `src/combat/ai/planning/future_value.rs` (cfg-gated или behind feature flag). Pure function:

   ```rust
   pub fn future_value_from_committed_state(
       actor: &UnitSnapshot,
       committed_pos: Hex,
       snap: &BattleSnapshot,
       maps: &InfluenceMaps,
   ) -> f32 { ... }
   ```

   Компоненты (начальные коэффициенты, перекалибровка после replay):
   - `λ_pos = 0.4` × `evaluate_position(committed_pos, role, maps)` (position_eval.rs).
   - `λ_attack = 0.5` × best `score_action` из `reachable_from(committed_pos, speed+max_attack_range)` против топ-3 по target_priority.
   - `λ_mob = 0.1` × `reachable_tile_count(committed_pos, speed) / max_mobility`.

2. **Prototype scorer** `score_plans_prototype(plans, ctx) -> Vec<f32>`:

   ```rust
   for plan in plans:
       prefix_factors = compute_factors(plan.prefix_only(), ctx)
       prefix_score   = finalize_scores(prefix_factors)   // как в production, но на prefix
       future_value   = future_value_from_committed_state(actor, committed_prefix_end, snap, maps)
       score[plan]    = prefix_score + γ · future_value   // γ = 0.25 start
   ```

   `plan.prefix_only()` — новый helper: срезает `steps / outcomes / sim_snapshots` до `committed_step_count()`.

3. **Flag в `replay_ai_log`** `--phase7-prototype`:
   - Применяет prototype scorer к каждому entry.
   - Выдаёт дополнительные метрики: `ranking_change_rate`, `phantom_tail_flips_committed` (post-prototype), `plateau_tie_rate`.
   - Все existing метрики (§5.1/5.2/5.3) перемеряются под prototype scoring.

4. **Regression corpus**: все post-step-3 логи (8 боёв) + оригинальный baseline_final (4 боя). Итого 12 логов.

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
                                                         └─→ Phase 7 prototype ─→ decision → next iteration
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
