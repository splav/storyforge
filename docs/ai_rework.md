# AI Rework: текущее состояние и ближайший план

Верхнеуровневый документ. Разработческий план — [`docs/ai_rework_plan.md`](ai_rework_plan.md). Архитектура — [`docs/ai.md`](ai.md). Инструмент замера — [`docs/ai-replay.md`](ai-replay.md).

Фиксация: 2026-04-22, пост-Phase 7 prototype (verdict 0/3).

---

## 1. Что сделано в этой итерации

| Step | Суть | Commit |
|---|---|---|
| 0 / 0.5 | Починен `replay_ai_log`, зафиксирован baseline corpus + schema v15 с gate-полями | — |
| 1 / 1b / 1c | `tempo_gain → net displacement`, `intent_sum` shortcut для pure-move + post-cast tail | e1f0d38 |
| 3 | Tiered killable gate под `FocusTarget` (Pressure / CanFinish, live-pool, intent-coherent) | 0952c96 |
| Track A | Replay метрики переведены на ground-truth: committed-prefix kill check + intent-coherent `has_real_kill_line` | 7caac3a |
| 4a | Phantom-tail фикс для `self_survival.exit_danger` — aggregation по committed prefix вместо `plan.final_pos` | d5a1078 |
| Phase 7 prototype | Offline prototype `Score = PrefixScore + γ·FV` + dual-metric replay flag. 12-log corpus vердикт **0/3** — см. §4.4. Post-mortem дал две гипотезы (intent-agnostic FV, cross-class reranking) → Track 2 follow-up (P1/P2). | abbf481..60bbed2 |

Основные результаты на 8-log corpus (`logs/baseline_20260422_step3.txt`):

- `killable_non_offensive_rate`: 7.7% → **0.0%** ✓
- `killable_wrong_target_rate`: 7.7% → **0.0%** ✓
- `kill_conversion_rate`: 0% → **80%** (ground-truth, borderline vs 85% цели на малой N=5)
- `repeated_tile_rate`: 29.3% → **9.9%** (−20 pp vs исходный baseline)
- `zero_net_move_rate`: 17.3% → **6.2%** (−11 pp)

Из Phase 7 prototype (12-log corpus, 222 entries): committed-prefix ablation **одна** даёт −17 pp на `phantom_tail_flips_committed` без регрессий на prefix-factor метриках. Это чистый сигнал, что horizon-based discipline в production окупится. Провал prototype'а (см. §4.4) — из-за `γ·FV` как additive cross-class reranker, а не из-за prefix-discipline.

**Не замерено**: step-4a ещё не прогнан на свежих логах. Ожидаемый эффект — `panic_leak_rate 16.7% → ≤ 5%`.

---

## 2. Что остаётся сломанным

### 2.1. Panic-leak (step-4a measurement pending)

16.7% на baseline_step3 (2/12 ProtectSelf+Default). Корень — phantom-tail retreat инфлейтил `self_survival.exit_danger`. Step-4a должен закрыть оба наблюдаемых кейса; нужен прогон и сверка.

### 2.2. Phantom-tail искажает committed decision

`phantom_tail_chosen_rate 31.7%` + `phantom_tail_flips_committed 65%`: у 65% планов с post-cast tail лучший tailless-альтернативный план имеет **другой** committed action. Phantom-tail реально меняет выбор, не косметика.

Step-1c и step-4a — **две точечные заплатки** одного и того же паттерна в разных осях (`intent_sum`, `self_survival.exit_danger`). Структурно: факторы агрегируют по всему плану, `commit_plan` fire'ит только prefix. Третий candidate на фикс — `tempo_gain` (всё ещё смотрит на `plan.final_pos`). Patch-по-одной-оси масштабируется плохо.

### 2.3. Plateau в pursuit_move_score / tempo.range_bonus

`pursuit_move_score` — step-function `0.8 flat` в attack range: много планов с одинаковым score, ties разрешаются RNG через top-K. Driver для остатка `repeated_tile 9.9%` и `zero_net_move 6.2%`. Тоже Phase 7 territory (FutureValue даёт smooth differentiation).

### 2.4. Summon spam (не замерено)

Старый артефакт из §2.3 оригинального плана (Старшина r1/r2/r3 — summon storm_spirit подряд). `saturation` ось сейчас только про buff'ы, summon'ы не покрывает. Не замерено на post-step-3 corpus — возможно уже подавлен гейтом, возможно ещё проявляется. Step-5 scope.

### 2.5. Kill_conversion borderline

80% на N=5 — статистически неразличимо с 85%-целью (Wilson 95% CI (38%, 96%)). Gate работает (5/5 intent-coherent, 4/5 реальных kill'ов), один Pressure-tier случай — design-correct. Для tight CI нужен corpus ≥ 15 killable+kill_line entries.

---

## 3. Ближайший план

Два независимых трека.

### Track 1 — закрыть текущую итерацию

1. **Step-4a measurement** — 8 боёв на post-4a binary, сверка с baseline_step3 (ожидание: `panic_leak ≤ 5%`, остальные метрики Δ ≤ 5 pp).
2. **Step-5** — summon saturation axis + intent-specific credit filter (`ai_rework_plan.md §Шаг 5`). Мотивация: замерить `summon_spam_rate`, закрыть если ≥ baseline; refactor intent_score для target-coherence.
3. **Добор corpus'а для step-3 tight CI** — опционально, ещё 4 боя → kill_conversion на N≥15.

Выход: все acceptance §5.1/5.2/5.3 зелёные на расширенном corpus'е, текущая итерация закрыта.

### Track 2 — Phase 7 follow-up (offline, параллельно)

Phase 7 prototype закрыт со счётом 0/3 (см. §4.4). Post-mortem выделил две независимые гипотезы корня acceptance regression:

- **H1 (intent-agnostic FV)**: FV не знает intent и оценивает `future_attack` одинаково против любых врагов. Для `FocusTarget{T}` attack потенциал должен считаться **только** против T.
- **H2 (cross-class reranking)**: даже intent-aware FV не должен поднимать план из одного policy-class над планом из другого. Admissibility-классы (killable gate tiers, ProtectSelf mask) надо уважать как жёсткие bucket-границы.

Вместо одного "исправленного Phase 7" — два независимых эксперимента:

- **P1 — Intent-aware FutureValue**: FV принимает `intent`, per-intent λ-веса (FocusTarget → target-locked attack, ProtectSelf → λ_attack=0 и т.д.). Проверяет H1.
- **P2 — Narrow bucketed ranking**: для `FocusTarget` и `ProtectSelf` — admissibility-buckets перед scalar finalize_scores; bucket rank лексикографически выше scalar'а. FV отключён. Проверяет H2.
- **P3 (опционально)** — композиция: buckets из P2 + intent-aware FV из P1 как tiebreak внутри bucket'а.

P1 и P2 ортогональны по коду (P1 трогает `future_value.rs`, P2 — replay). Параллельный замер даёт **декомпозицию сигнала**: какой лечит — тот и merge'ится как step-6.

**Stop-rule диагностический** (§3 `ai_rework_plan.md`): сценарии A (P1 ≥ 3/4) → merge P1; B (P2 ≥ 3/4) → merge P2; C (оба ≥ 1/4) → P3 → merge P3 если ≥ 3/4; D (оба < 1/4) → production committed_state refactor без prototype'а.

Подробности, acceptance-таблицы, код-разметка — `ai_rework_plan.md §Track 2 — Phase 7 follow-up`.

---

## 4. Метрики и acceptance (актуальные)

### 4.1. Step-4a acceptance

| Метрика | Формула | Цель | Baseline |
|---|---|---|---|
| `panic_leak_rate` | ProtectSelf+Default & non-defensive chosen / знаменатель | **≤ 5%** | 16.7% |
| Regression guards §4.3 | Δ ≤ 5 pp на всех существующих метриках | ≤ 5 pp | — |

### 4.2. Step-5 acceptance

| Метрика | Цель |
|---|---|
| `summon_spam_rate` (≥3 summon одного класса подряд за 5 ходов) | ≤ 1% |
| Legitimate buff-стэк в regression-тесте | pass |

### 4.3. Regression guards (на всех шагах)

Δ метрик, которых шаг не должен трогать, ≤ 5 pp. Иначе — investigation, не auto-merge. Ключевые: `kill_conversion_rate`, `killable_non_offensive_rate`, `repeated_tile_rate`, `zero_net_move_rate`, `post_cast_retreat_rate`, `phantom_tail_chosen_rate`.

### 4.4. Phase 7 prototype — closed (verdict 0/3) + follow-up experiments

Прогон 12-log corpus (8 post-step-3 + 4 baseline_final, 222 entries) через `replay_ai_log --phase7-prototype` от 2026-04-22:

- `phantom_tail_flips_committed` 64.7% → **47.6%** (−26% rel., цель ≥40% — ✗).
- `plateau_tie_rate` 24.3% → **20.3%** (цель <10% — ✗).
- **Acceptance деградация**: `killable_non_offensive_rate` 0% → **23.5%**, `kill_conversion_rate` 88.2% → **64.7%** (Δ ≤ 5 pp — ✗).

**Вердикт**: prototype в форме `γ·FV` additive reranker не готов к merge. **НО**: committed-prefix ablation сама по себе дала −17 pp на phantom_tail без регрессий на prefix-factor метриках — это полезный сигнал в пользу production committed_state discipline. Полный output — [`logs/phase7_prototype_20260422.txt`](../logs/phase7_prototype_20260422.txt).

**Follow-up**: два независимых offline эксперимента (P1 intent-aware FV, P2 narrow bucketed ranking) с диагностическим stop-rule. Детали acceptance / sequence — `ai_rework_plan.md §Track 2 — Phase 7 follow-up`.

### 4.5. P1 / P2 / P3 acceptance (follow-up)

Таблицы acceptance для каждого эксперимента — в `ai_rework_plan.md`. Ключевые точки:

- **P1 acceptance**: `phantom_tail_flips_committed` drop ≥ 15 pp от Phase 7 baseline (47.6% → ≤ 32%), `kill_conversion_rate` **частичное** восстановление ≥ 75% (полное восстановление — работа P2).
- **P2 acceptance**: `killable_non_offensive_rate` **полное** восстановление ≤ 2%, `kill_conversion_rate` ≥ 80%.
- **P3 acceptance**: все P1 + все P2 одновременно + `phantom_tail_flips_committed` drop ≥ 20 pp от Phase 7 prototype.

"Честный провал" P1 (phantom_tail не сдвинулся, kill_conv < 75%) — информативный результат в пользу P2, не bad experiment.

---

## 5. Что вне scope

- **Trade economy** (`trade.rs`) — работает изолированно, не трогаем.
- **Adaptation layer** — триггеры fact-based, не меняем.
- **Intent selection** (`select_intent`) — меняем как план оценивается, не как intent выбирается.
- **Marginal board value** для summon'ов (`future_dpr × expected_lifetime`) — технический долг, не в текущую итерацию.
- **Difficulty knobs** — новые параметры (ε, α) пиннятся хардкодом; difficulty-профили после 2–3 недель стабильности.
- **step-6 merge** (production committed_state refactor или P1/P2/P3 semantics) — НЕ в текущей итерации. Только эксперименты + decision.
- **Full committed_state discipline в production** — scenario D follow-up'а, не prototype (эффект измеряется только живыми боями, не replay'ем).
- **ProtectAlly bucket** — откладывается до step-6 (в corpus'е мало ProtectAlly entries для честного сигнала).

---

## 6. Известные ограничения

- **Gate multi-cast prefix** (step-3 §3.2a): план с `[Cast @ other, Cast @ intent_target]` проходит gate (второй шаг offensive_vs_target), но commit_plan fire'ит только первый Cast — не ту цель. Мониторится через `killable_wrong_target_rate`. Потенциально закрывается P2 bucket-логикой (rank по committed prefix отбросит такие планы в weak-offense bucket).
- **α = 0.3** (killable gate), **ε_self = 0.15** (ProtectSelf mask), **γ = 0.25** и `PHASE7_MAX_MOBILITY = 30` (Phase 7 FV) — пинятся хардкодом в коде + replay diagnostic с `// KEEP IN SYNC` комментариями.
- **`pursuit_move_score` plateau** — step-function 0.8 flat в attack range. Phase 7 prototype подтвердил устойчивость плато к γ·FV (`plateau_tie_rate` 24.3% → 20.3%). Если после P1/P2/P3 плато останется > 10% — отдельный step "smooth pursuit" (scaled by остаток distance до оптимума, не binary).
- **Follow-up preconditions** — текущие step'ы (3, 4a) должны быть замерены и стабильны до начала P1/P2. Не блокер.

---

## 7. Long-term архитектурные идеи (не в этой итерации)

Идеи, обсуждавшиеся в post-mortem Phase 7 и признанные правильным направлением, но дорогими / преждевременными для текущей итерации:

- **Typed `PlanEffects` + effect ontology** — каждый план производит типизированный набор эффектов (`damage_now_vs_target`, `heal_self_now`, `control_now_vs_target`, `future_attack_value`, …) с явным тегированием горизонта (`NOW` / `NEXT` / `LATER`). Убирает класс багов "механика попадает не в тот фактор". Перевод factor pipeline на такую структуру — deep refactor, не prototypeable как offline, выгоден **после** накопления 10+ механик с рекуррентными bug patterns.
- **Declarative `IntentPolicy`** — вместо кодирования логики каждого intent вручную в 3–4 местах, декларативная таблица `{name, bucket_rules, required/forbidden effects, future_mask}`. Требует сначала PlanEffects ontology (выше); без неё — абстракция без тела. Отложено до роста числа intents.
- **Lexicographic bucketed ranking (full)** — расширение P2-паттерна на все intents с единой framework'ой. P2 делает это точечно для FocusTarget + ProtectSelf; **полная** система — после того, как P2/P3 покажут, что bucketing работает.
- **Committed_state full discipline в production** — системная версия step-1c / step-4a / phantom-tail fix'ов. Правило "factor видит только committed prefix" встроено в `compute_plan_factors`, `compute_plan_tempo_gain`, `compute_plan_self_survival`. Это `step-6` в сценарии D (если P1/P2/P3 не дадут сигнала).

Общее правило: **не разворачивать архитектурный слой без 2–3 независимых данных-точек, что он нужен**. Phase 7 prototype — первая такая точка; P1/P2/P3 дадут вторую; дальнейшее — по ситуации.
