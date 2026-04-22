# AI Rework — Developer Plan

Шаги имплементации. Контекст и acceptance — [`docs/ai_rework.md`](ai_rework.md).

Фиксация: 2026-04-22, пост-Phase 7 prototype (verdict 0/3).

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
| Phase 7 prototype (2A–2E) | Offline prototype `Score = PrefixScore + γ·FV`; 12-log corpus verdict **0/3**. См. § Phase 7 prototype — closed. | `planning/future_value.rs`, `bin/replay_ai_log.rs` | abbf481..60bbed2 |

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

### Track 2 — Phase 7 follow-up (offline experiments)

Phase 7 prototype завершён со счётом **0/3**. Раздел ниже разбит на **(i)** post-mortem (что узнали, чтобы не потерять знание) и **(ii)** два независимых follow-up эксперимента **P1** и **P2** с опциональной композицией **P3**. Все три — offline, production-код не меняют.

#### Phase 7 prototype — closed

- Код: `src/combat/ai/planning/future_value.rs` (`plan_prefix_only`, `future_value_from_committed_state`, `score_plans_prototype`), флаг `replay_ai_log --phase7-prototype` с dual-pipeline и метриками `ranking_change_rate` / `plateau_tie_rate`. Коммиты abbf481..60bbed2.
- Corpus: 12 логов (8 post-step-3 + 4 baseline_final), 222 entries.
- Full output: [`logs/phase7_prototype_20260422.txt`](../logs/phase7_prototype_20260422.txt).

| Criterion | Threshold | Baseline | Prototype | Δ | Pass |
|---|---|---|---|---|---|
| `phantom_tail_flips_committed` | ≥ 40% rel. drop from 64.7% | 64.7% | 47.6% | −17.1 pp (−26% rel.) | ✗ |
| `plateau_tie_rate` | prototype < 10% | 24.3% | 20.3% | −4.1 pp | ✗ |
| Δ acceptance (§4.1–4.3) | ≤ 5 pp на всех | — | — | **+23.5 pp** (`killable_non_offensive` 0→23.5%, `kill_conversion` 88.2→64.7%) | ✗ |

**Ключевые находки**:

1. **Committed-prefix ablation сама по себе работает.** `score_plans_prototype` считает факторы на `plan_prefix_only(plan)` — это и есть partial committed_state discipline. −17 pp на phantom_tail без регрессий на prefix-factor метриках — чистый сигнал, что horizon-based discipline даст эффект в production.
2. **γ·FV как additive reranker ломает kill-line gate.** Prototype применяет `γ · FutureValue(committed_state)` поверх уже gate-passed пула. FV игнорирует intent (одинаковая оценка для любых достижимых врагов) — gate-passed offensive план с плохим committed_pos проигрывает non-offensive плану с хорошим committed_pos. Это **не вопрос калибровки γ**, а структурная проблема cross-class reranking.
3. **Plateau устойчив.** `pursuit_move_score` — step-function 0.8 flat в attack range → ties на уровне PrefixScore. γ=0.25 с текущими λ-весами FV недостаточно, чтобы дифференцировать плато.

Код prototype оставляем: (a) он infrastructure для P1/P2/P3 — `plan_prefix_only` переиспользуется; (b) regression guard от возврата к наивному scalar scoring.

---

#### Мотивация follow-up

Две independent гипотезы о корне acceptance regression:

- **H1 (intent-agnostic FV)**: FV не знает intent и оценивает `future_attack` одинаково для любых врагов. Исправление — `FocusTarget{T} ⇒ λ_attack только vs T`, `ProtectSelf ⇒ λ_attack = 0`, и т.д. Проверяется прототипом **P1**.
- **H2 (cross-class reranking)**: даже intent-aware FV не должен поднимать план из одного policy-class над планом из другого. Классы — admissibility regions существующего killable gate и `apply_protect_self_mask`. Исправление — bucketed ranking с FV только внутри bucket. Проверяется прототипом **P2**.

P1 и P2 ортогональны (модифицируют разные куски pipeline'а). Параллельный замер двух — это **декомпозиция сигнала**: если только один из двух даёт эффект, значит корень именно там; если оба частично, значит нужна композиция **P3**.

#### P1 — Intent-aware FutureValue

**Файлы**:
- `src/combat/ai/planning/future_value.rs` — сигнатура `future_value_from_committed_state(active, committed_pos, snap, maps, ctx)` расширяется параметром `intent: &TacticalIntent`.
- `src/bin/replay_ai_log.rs` — флаг `--p1` (или переиспользуем `--phase7-prototype` через parameter swap, пусть решает реализатор).

**Что делать**:

Переопределяет per-intent λ-веса в `future_value_from_committed_state`:

| Intent | λ_attack | λ_pos | λ_mob | Комментарий |
|---|---|---|---|---|
| `FocusTarget{T}` | только vs T (0 если T мёртв / недостижим) | default | default | target-locked attack budget |
| `ApplyCC{T}` | как FocusTarget, но `score_action` ограничен ability с `applies_cc = true` | default | default | |
| `ProtectSelf` | **0** | 2× вес (больший штраф за danger) | default | fleeing — attack будущее не релевантно |
| `ProtectAlly{A}` | **0** | default | default | ally-coverage term вне scope P1 |
| `SetupAOE` | best AoE ability против max-hits tile | default | default | |
| `Reposition` / `LastStand` | default (как в Phase 7) | default | default | |

Остальное — без изменений. `γ = 0.25` хардкод остаётся.

**Тесты**:
- `future_value::tests::focus_target_ignores_non_target_enemies` — `FocusTarget{T}` + лёгкий враг ≠ T рядом, тяжёлый T далеко: FV не поднимает план, где T недостижим.
- `future_value::tests::protect_self_suppresses_attack_component` — λ_attack = 0 для ProtectSelf.
- `future_value::tests::apply_cc_uses_only_cc_abilities` — регрессия для ApplyCC filter'а.

**Acceptance** (на 12-log corpus):

| Метрика | P1 acceptance |
|---|---|
| `phantom_tail_flips_committed` | drop ≥ 15 pp от Phase 7 prototype (47.6% → ≤ 32%) |
| `kill_conversion_rate` | **частичное** восстановление, ≥ 75% (не требуем 88% — это работа P2) |
| `killable_non_offensive_rate` | ≤ 10% (Phase 7 prototype 23.5%, baseline 0%) |
| Δ на прочих acceptance (panic_leak, repeated_tile, zero_net_move, post_cast_retreat) | ≤ 5 pp |

**P1 "честный провал"** — phantom_tail не сдвинулся и kill_conversion < 75%. Это **информативный** результат: значит intent-awareness одна не лечит, нужен class-respecting gate (P2). В stop-rule ниже — сценарий B.

#### P2 — Narrow bucketed ranking (FocusTarget + ProtectSelf)

**Файлы**:
- `src/bin/replay_ai_log.rs` — дополнительный pipeline-вариант `score_plans_p2`.
- Код живёт в replay, не в `planning/`: это узкий эксперимент, не будущая production-архитектура.

**Что делать**:

Для `FocusTarget{T}` и `ProtectSelf` — admissibility-buckets перед scalar finalize_scores. Остальные intents (ApplyCC, Reposition, SetupAOE, ProtectAlly, LastStand) — baseline scoring (изолированный эксперимент).

**FocusTarget{T} bucketing** (считается по committed prefix, не по full plan):
```
rank(plan, target=T) =
  2  if committed_prefix kills T
  1  if plan_is_offensive_vs(T) && damage_on_T_in_prefix ≥ α · T.hp   // α = 0.3, как в killable gate
  0  if plan_is_offensive_vs(T)                                       // weak offense
 -1  otherwise                                                         // off-intent
```
Compare: `(rank, scalar_finalize_score)` лексикографически. Внутри одного ранга — stock finalize_scores.

**ProtectSelf bucketing** (переиспользует существующую `apply_protect_self_mask` logic):
```
rank(plan) =
  1  if self_survival_now ≥ ε_self    // ε_self = 0.15, как в текущей mask
  0  otherwise                        // LastStand / hard panic территория
```
Compare: `(rank, scalar_finalize_score)` лексикографически.

**Инвариант**: все факторы, участвующие в bucket-классификации (damage_on_T, self_survival), считаются на **committed prefix**, не full plan. `plan_prefix_only` переиспользуется.

**Тесты**:
- `replay::p2::tests::focus_target_lethal_beats_pressure` — план-kill T всегда выше плана-pressure T.
- `replay::p2::tests::focus_target_weak_offense_beats_off_intent`.
- `replay::p2::tests::protect_self_defensive_beats_offensive_under_mask`.
- `replay::p2::tests::bucket_uses_prefix_damage_not_tail` — два плана `Cast T` vs `Cast other → Cast T`: оба попадают в rank=2, не расходятся по prefix (второй в prefix имеет только Cast other).

**Acceptance** (на 12-log corpus):

| Метрика | P2 acceptance |
|---|---|
| `killable_non_offensive_rate` | **полное** восстановление ≤ 2% (baseline 0%) |
| `kill_conversion_rate` | ≥ 80% (baseline 88.2%) |
| `panic_leak_rate` | ≤ baseline + 5 pp (baseline 13.3%) |
| `phantom_tail_flips_committed` | NOT acceptance для P2 — P2 не таргетирует phantom-tail, это P1/committed_state territory |
| `plateau_tie_rate` | ≤ baseline + 5 pp (buckets не должны увеличивать плато) |

**P2 "честный провал"** — acceptance metrics не вернулись к baseline (значит bucketing сам по себе не покрывает corruption, возможно проблема и в scalar-компоненте внутри bucket'а).

#### P3 — композиция P1 + P2 (опционально)

Запускается **только** если P1 и P2 каждый по себе показывают частичный signal (≥ 1/4 acceptance metrics прошли, но не все). Если один из них проходит полностью — P3 не нужен.

**Что**: intent-aware FV работает только как tiebreak **внутри** P2-bucket'а. γ остаётся, но умножается на `future_value_intent_aware(active, committed_pos, snap, maps, intent)`. Bucket rank остаётся lexicographically сверху.

**Acceptance** P3:
- Все P1 acceptance + все P2 acceptance одновременно.
- `phantom_tail_flips_committed` drop ≥ 20 pp от Phase 7 prototype (объединённый эффект).

#### Stop-rule (диагностический)

| Сценарий | Условие | Интерпретация | Следующий шаг |
|---|---|---|---|
| **A** — P1 работает | P1 пройдёт ≥ 3/4 acceptance | intent-awareness покрывает проблему без class-guard | Merge P1 semantics в production как step-6 (intent-aware factor/FV layer). P2 архивируется. |
| **B** — P2 работает | P2 пройдёт ≥ 3/4 acceptance | class-respecting ranking покрывает без FV rework | Merge P2 pattern в production как step-6 (расширение killable gate + ProtectSelf mask на явные buckets). P1 архивируется. |
| **C** — оба частично | P1 ≥ 1/4, P2 ≥ 1/4, ни один полностью | signal есть, нужна композиция | Запустить P3. Если P3 ≥ 3/4 acceptance — merge P3 как step-6 (dual approach); иначе — сценарий D. |
| **D** — ничего не работает | P1 < 1/4 И P2 < 1/4 | корень не в FV и не в bucket'ах — структурная проблема (plateau / intent selection / committed_state leakage в production факторах) | Идём в **production committed_state refactor** как step-6 без prototype'а: исправляем `compute_plan_tempo_gain`, `compute_plan_self_survival`, `compute_plan_intent_sum` чтобы читали **только** committed prefix. Это не prototypeable — эффект измеряется живыми боями. |

Критерий "прошло acceptance" — все перечисленные в таблице acceptance'а метрики в пределах target'ов.

#### Что НЕ в scope follow-up'а

- **ProtectAlly bucket** — откладывается до step-6 production. Статистически мало entries в corpus'е.
- **Declarative IntentPolicy** (typed PlanEffects, effect ontology) — far-future refactor, не текущая итерация. Обсуждается в §7 `ai_rework.md`.
- **Full committed_state discipline** в production — это сценарий D, не prototype. Prototype его частично покрывает через `plan_prefix_only`, но полный эффект от vещи вроде `tempo_gain prefix-only` измеряется только на production factor pipeline.
- **Plateau fix** (`pursuit_move_score` step-function → smooth) — отдельная работа, не P1/P2. Если plateau_tie_rate останется > 10% после любого из сценариев A/B/C — отдельный step.

#### Артефакты follow-up

- `logs/p1_<date>.txt`, `logs/p2_<date>.txt`, `logs/p3_<date>.txt` (если запустится) — output corpus-прогонов.
- Таблицы результатов + вердикт в этом же документе после каждого эксперимента.
- Решение о step-6 по stop-rule выше.

#### Риск follow-up

Нулевой для production. Timeline — 1 день на P1, 2 дня на P2, опционально 1 день на P3. Если попадём в сценарий D — prototype не даст ответа и мы идём в production refactor "вслепую"; но поскольку committed-prefix ablation из Phase 7 уже показала −17 pp на phantom_tail, мы не совсем вслепую.

---

## Порядок и параллелизм

```
[shipped: 0 → 0.5 → 1 → 1b → 1c → 2/2.5 → 3 → Track A → 4a → Phase 7 prototype (verdict 0/3)]
                                                                                       │
                                                                                       ├─→ Step-4a measurement ─→ Step-5 ─→ итерация закрыта
                                                                                       │
                                                                                       └─→ P1 ─┐
                                                                                               ├─→ [A|B|C|D] stop-rule ─→ step-6 (production)
                                                                                       └─→ P2 ─┘         │
                                                                                                        (опционально P3)
```

- Track 1 и Track 2 независимы.
- Step-5 после step-4a measurement (чтобы видеть чистый baseline для summon-метрики).
- P1 и P2 можно запускать **параллельно** (P1 трогает `future_value.rs`, P2 — replay_ai_log; зоны кода не пересекаются). Рекомендованный порядок: P1 → P2 один за другим, чтобы дифф каждого был чистым для review.
- P3 — условный; запускается только если P1 и P2 дают частичный signal (сценарий C).
- **step-6 НЕ в текущей итерации** — только P1/P2/(P3) + decision. Сам merge — следующая итерация.

---

## Вне scope

- Canonical-набор факторов (следующая итерация).
- Marginal board value для summon'ов (тех долг).
- Trade economy, difficulty knobs — изолированы.
- Intent selection (`select_intent`) — меняем как план оценивается, не как intent выбирается.
- Plateau-ties от step-function `pursuit_move_score` — если останутся после P1/P2/P3, отдельный step (smooth pursuit).
- **Typed `PlanEffects` / declarative IntentPolicy / effect ontology** — архитектурные идеи уровня “Phase 8+”, не для текущей итерации; обсуждаются в `ai_rework.md` как long-term direction.
- **ProtectAlly bucket** — откладывается до step-6 production (в follow-up'е мало data).

---

## Справочник: completed steps (deep detail)

Если нужно вспомнить, **почему** шаг был сделан именно так, — читай код + тесты:

- Step-3 gate (invariants, tiers): `src/combat/ai/planning/killable_gate.rs` docstring + 9 тестов.
- Step-1c intent_sum shortcut: `src/combat/ai/planning/scorer.rs::compute_plan_intent_sum` docstring.
- Step-4a phantom-tail fix: `src/combat/ai/factors/survival.rs` — `committed_prefix_final_pos` helper + 6 regression тестов.
- Track A replay metric semantics: `src/bin/replay_ai_log.rs::plan_committed_prefix_kills_target` + 4 теста.

Исторические решения (user critiques про live-pool и intent-coherent detection, ally-split drop) зафиксированы в commit messages коммитов выше. Git log — авторитетный источник.
