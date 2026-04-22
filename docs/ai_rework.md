# AI Rework: текущее состояние и ближайший план

Верхнеуровневый документ. Разработческий план — [`docs/ai_rework_plan.md`](ai_rework_plan.md). Архитектура — [`docs/ai.md`](ai.md). Инструмент замера — [`docs/ai-replay.md`](ai-replay.md).

Фиксация: 2026-04-22, пост-step-4a.

---

## 1. Что сделано в этой итерации

| Step | Суть | Commit |
|---|---|---|
| 0 / 0.5 | Починен `replay_ai_log`, зафиксирован baseline corpus + schema v15 с gate-полями | — |
| 1 / 1b / 1c | `tempo_gain → net displacement`, `intent_sum` shortcut для pure-move + post-cast tail | e1f0d38 |
| 3 | Tiered killable gate под `FocusTarget` (Pressure / CanFinish, live-pool, intent-coherent) | 0952c96 |
| Track A | Replay метрики переведены на ground-truth: committed-prefix kill check + intent-coherent `has_real_kill_line` | 7caac3a |
| 4a | Phantom-tail фикс для `self_survival.exit_danger` — aggregation по committed prefix вместо `plan.final_pos` | d5a1078 |

Основные результаты на 8-log corpus (`logs/baseline_20260422_step3.txt`):

- `killable_non_offensive_rate`: 7.7% → **0.0%** ✓
- `killable_wrong_target_rate`: 7.7% → **0.0%** ✓
- `kill_conversion_rate`: 0% → **80%** (ground-truth, borderline vs 85% цели на малой N=5)
- `repeated_tile_rate`: 29.3% → **9.9%** (−20 pp vs исходный baseline)
- `zero_net_move_rate`: 17.3% → **6.2%** (−11 pp)

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

### Track 2 — Phase 7 prototype (offline, параллельно)

Паттерн phantom-tail-per-axis требует архитектурного ответа. Вместо третьей заплатки готовим decomposition:

```
Score(plan) = PrefixScore(committed_prefix) + γ · FutureValue(committed_state)
```

где `FutureValue` — cheap one-ply surrogate от committed position (best next-turn move/attack/mobility, не зависит от конкретного хвоста).

**Prototype offline**: отдельный worktree, новая функция в `planning/future_value.rs` (cfg-gated, не вызывается production-pipeline'ом). `replay_ai_log --phase7-prototype` применяет prototype scorer, сравнивает ranking/metrics с production.

Собираем 3 сигнала:
- `phantom_tail_flips_committed` на prototype → должно упасть ≥ 40%.
- `plateau_tie_rate` (top-K с `max−min<0.05`) → должно упасть < 10%.
- Regression на acceptance-метриках §5.1/5.2/5.3 → Δ ≤ 5 pp.

**3/3** → Phase 7 merge следующая итерация (multi-PR).  
**2/3** — доп. design doc перед commit.  
**≤ 1/3** — prototype не оправдан, возвращаемся к точечным фиксам.

Подробнее — `ai_rework_plan.md §Phase 7 prototype`.

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

### 4.4. Phase 7 prototype decision criteria

Прогон 12-log corpus (8 post-step-3 + 4 baseline_final, 222 entries) через `replay_ai_log --phase7-prototype` от 2026-04-22 дал результат **0/3** по критериям принятия: `phantom_tail_flips_committed` снизился лишь на 26% (цель ≥40%), `plateau_tie_rate` осталась 20.3% (цель <10%), а acceptance-метрики показали критическую деградацию — `killable_non_offensive_rate` 0%→23.5%, `kill_conversion_rate` 88.2%→64.7%. **Вердикт: prototype не оправдан, возвращаемся к точечным фиксам.** Детальный анализ и таблица в `ai_rework_plan.md §Step-2E Results`, полный output — `logs/phase7_prototype_20260422.txt`.

---

## 5. Что вне scope

- **Trade economy** (`trade.rs`) — работает изолированно, не трогаем.
- **Adaptation layer** — триггеры fact-based, не меняем.
- **Intent selection** (`select_intent`) — меняем как план оценивается, не как intent выбирается.
- **Marginal board value** для summon'ов (`future_dpr × expected_lifetime`) — технический долг, не в текущую итерацию.
- **Difficulty knobs** — новые параметры (ε, α) пиннятся хардкодом; difficulty-профили после 2–3 недель стабильности.
- **Phase 7 merge** — НЕ в текущей итерации. Только prototype + decision.

---

## 6. Известные ограничения

- **Gate multi-cast prefix** (step-3 §3.2a): план с `[Cast @ other, Cast @ intent_target]` проходит gate (второй шаг offensive_vs_target), но commit_plan fire'ит только первый Cast — не ту цель. Phase 7 territory, мониторится через `killable_wrong_target_rate`.
- **α = 0.3** (killable gate), **ε_self = 0.15** (ProtectSelf mask) — пинятся хардкодом в коде + replay diagnostic с `// KEEP IN SYNC` комментариями.
- **Phase 7 prototype preconditions** — текущие step'ы (3, 4a) должны быть замерены и стабильны до начала prototype'а. Не блокер, но условие для честной сравнительной базы.
