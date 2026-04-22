# AI Rework: следующая итерация после Phase 6

Документ верхнего уровня: **почему** продолжаем рефакторинг, **какие решения** зафиксированы на этом витке, **какие метрики** определяют «готово». Пошаговый план для разработчика — [`docs/ai_rework_plan.md`](ai_rework_plan.md). Текущая архитектура — [`docs/ai.md`](ai.md). Инструмент проверки — [`docs/ai-replay.md`](ai-replay.md).

---

## 1. Состояние на входе

Phase 1–6 impact-model рефакторинга замержены: `NUM_FACTORS = 10`, schema v14. В `PlanFactors` живут оси `damage / kill_now / kill_promised / cc / heal / intent / scarcity / tempo / saturation / self_survival`. Удалены `position / risk / focus`. Точечные фиксы прошлой итерации, уже в коде:

- **R3** (`factors/offensive.rs:166`): `calc.expected().round()` — устранён boundary-mismatch между scorer'ом и sim'ом.
- **R2** (`intent.rs:485`): killable-intent требует `action_points > 0`.

Эти правки закрыли несколько старых симптомов, но остаточные паттерны остались. Новая итерация начинается с анализа **двух свежих логов** `20260421T1646*` (beastblood + stormborn).

---

## 2. Причины: что ещё сломано

Анализ 92 committed-планов в `logs/20260421T164625_*stormborn*.jsonl` выявил структурные, а не калибровочные дефекты:

### 2.1. S1/R4 — массовый holostoy/post-cast retreat (33% планов)

**30 из 92** chosen-планов содержат повторное посещение тайла. Примеры: `(1,2)→(0,2)→(-1,3)→(0,2)` (Воин, ap=0), `Cast heal → Move(1,2),(0,2) → Move(1,2),(1,3)` (Буревестник, возврат на старт после каста).

Корень двойной:
- `intent_sum = Σ intent_score(step) × discount^k` (`planning/scorer.rs:519-549`). `pursuit_move_score` отдаёт flat `0.8` каждому Move-шагу, финал которого в reach. План с 3 Move-шагами получает `intent ≈ 2.058`, план с 1 — `0.8`, независимо от net displacement.
- `tempo_gain` (`factors/tempo.rs:42-64`) берёт value **последнего** шага, а не дельту `actor_start → plan.final_pos`. Туда-обратно в последнем Move → локальный tempo ≈ 0, но три Move'а уже насмасштабировали intent.

Ось `tempo_gain` **не дискриминирует** путь с нулевым net displacement от полезного сближения.

### 2.2. Killable → heal (новый лик S3)

r3 Буревестник, `selection_kind=killable`, target HP 3/20, committed plan — `Cast heal(self)`. Корень: `IntentWeights::FocusTarget` (`intent.rs:866-870`) содержит только `damage/kill_now/kill_promised/cc`. `heal` axis получает глобальный положительный вес через `AXIS_FACTOR_WEIGHTS`, intent-фильтрация его не затрагивает. Hard-контракт «killable → offensive» отсутствует.

### 2.3. Summon spam недоглушён

Старшина r1/r2/r3 — summon storm_spirit подряд. `scarcity` честно падает `-0.47 → -1.0`, но план всё равно выигрывает: summon-cast получает intent-credit под `FocusTarget(T)` (его target ≠ T), плюс positional axes. `saturation` axis (`factors/saturation.rs`) по дизайну **только для buff'ов**, summons не покрывает.

### 2.4. Panic-override с ally-heal

r4 Старшина HP 3/22, `panic_override`: `Cast heal(ally) → Move`. Heal уходит союзнику, актор остаётся на 3 HP. `self_survival` axis смешивает self-directed (heal_self, armor_self, exit_aoo) и ally-directed эффекты неявно; ProtectSelf ε-gate не отличает «защитил себя» от «защитил кого-то рядом».

### 2.5. Блокер измерения

`replay_ai_log` не компилируется на HEAD (`src/bin/replay_ai_log.rs:318` — `ContentView::load_global_for_tests` удалён в пользу `load_layered`). Без него любые дальнейшие правки слепые.

---

## 3. Принятые решения

### 3.1. Policy under condition вместо отрицательных весов

**Killable → heal** не лечится «отрицательным intent-weight на heal». Политика семантически точнее:

```
if intent == FocusTarget(killable)
   && exists plan P with (P.kill_now ≥ 1 OR P.damage_vs_target ≥ hp·α)
   && actor NOT in LastStand / ProtectSelf mode:
       prune non-offensive plans to -∞
else:
       normal scoring
```

Отрицательный вес ломает edge cases (death-save, status-strip, kill-line недостижима). Hard gate выражает смысл режима: *если kill реально достижим, heal не должен молча выиграть*.

### 3.2. Killable hard gate живёт **под** survival-policy

Gate применяется только если `evaluation_mode == Default`. В `LastStand` и под `ProtectSelf`-инвариантом он **не** активируется — иначе юнит может prune'нуть свой последний heal при гарантированной смерти просто потому, что где-то в пуле есть формальная kill-line.

Формальный порядок evaluation:
1. `apply_adaptation` → `LastStand` при `ExpectedSelfLethal` / `ProtectSelfFutile`.
2. ProtectSelf ε-gate на self-component (см. 3.3).
3. Killable hard gate — **только если не сработал шаг 2 и mode == Default**.
4. Обычный scoring.

### 3.3. Разделение `self_survival` и `ally_rescue`

Текущая ось `self_survival` неявно принимает ally-heal через AXIS_FACTOR_WEIGHTS. Разделяем:

- `self_survival` — только реальный self-effect (heal_self, armor_self, exit_aoo, distance-from-threat).
- `ally_rescue` — новая ось: heal/buff/taunt-redirect на союзника.

**ProtectSelf ε-gate** требует `plan.self_survival ≥ ε_self`, **не** смесь. Ally-heal под panic проходит только если попутно поднимает self_survival (например, AoE heal-beam с собой в зоне).

### 3.4. Tempo — plan-terminal через net displacement

`tempo_gain` переводится на `actor_start → plan.final_pos` дельту, а не локальное value последнего шага. План оценивается как план, а не набор локально-неплохих шагов.

Правка tempo может оказаться **неполной**: если `intent_sum` всё ещё аккумулирует по Move-цепочке, обратный ход получает кредит через другую ось. Поэтому шаг 1 проходит measurement gate (см. 5.1); если метрики не сходятся до порога — трогаем `intent_sum` (схлопываем Σ по Move-шагам в max или в один pursuit от start до final).

### 3.5. Summon — три компонента

1. `saturation` axis расширяется на summon-class (сейчас только buff'ы).
2. Intent-credit фильтруется **intent-specific**: для `FocusTarget / ApplyCC / ProtectAlly` cast с `target ≠ intent.target` intent-credit не получает. Для `SetupAOE / ProtectSelf / LastStand / Reposition` фильтр не применим — там нет single-target.
3. Marginal board value (`future_dpr × expected_lifetime`) — **вне scope** этой итерации, технический долг.

### 3.6. Checkpoint перед killable gate

После шагов 1–1b метрики перемеряются. Если `killable_non_offensive_rate` уже просел и `kill_conversion_rate` высокий — killable gate делается мягким (bias через weights), а не hard prune. Это экономит риск ложных gate'ов, если корректный tempo + R3 уже выпрямили решения.

---

## 4. Порядок работ

| # | Шаг | Зависимости |
|---|---|---|
| 0 | Починить `replay_ai_log` (compile fix) | — |
| 0.5 | Зафиксировать baseline corpus (10–20 боёв, фиксированные seeds) | 0 |
| 1 | `tempo_gain` → net displacement | 0.5 |
| 1b | **Условный** fix `intent_sum` для Move-цепочек, если шаг 1 не добил метрики | 1 + measurement |
| 2 | Replay checkpoint: замер M2.*, M4.1–3; решение о форме шага 3 | 1/1b |
| 2.5 | Schema v15: поля `gate_applied`, `survival_mode_active`, `last_stand_active` + R5 plumbing (evaluation_mode per-plan в replay) | 2 |
| 3 | Hard gate `FocusTarget(killable)` под ProtectSelf/LastStand | 2.5 |
| 4 | Split `self_survival` / `ally_rescue`; ε-gate ProtectSelf на self-component | 3 |
| 5 | Summon saturation axis + intent-specific credit filter | 4 |

Реальные правки в коде начинаются с шага 1 — шаги 0 и 0.5 инструментальные.

---

## 5. Метрики и acceptance

Все метрики считаются на зафиксированном corpus'е после каждого шага. Baseline на текущих логах: `repeated_tile_rate ≈ 33%`, `killable_non_offensive_rate` — неизвестно до шага 0 (reply не считает), `panic_leak_rate` — см. замечание ниже.

### 5.1. Шаг 1 (tempo) — acceptance

| Метрика | Формула | Цель |
|---|---|---|
| `repeated_tile_rate` | planы с ≥1 повторным тайлом / planы с move_steps>0 | **< 5%** (baseline ~33%) |
| `zero_net_move_rate` | planы с move_steps>0 & final_pos==start_pos / planы с move_steps>0 | **< 1%** |
| `post_cast_retreat_rate` | planы с post-cast move & net≤0 & repeated>0 / planы с post-cast move | падение **≥ 70%** от baseline |
| `same_destination_longer_path_wins_rate` | пары planов с одинаковым final_pos, где длинный путь выигрывает | **< 5%** (диагностический) |

Шаг 1 принят, если **одновременно**: 1, 2, 3 в целях. Если 1 или 2 не в цели, но 3 упал — шаг частично успешен, запускается **1b** (правка `intent_sum`).

### 5.2. Шаг 3 (killable gate) — acceptance

| Метрика | Формула | Цель |
|---|---|---|
| `killable_non_offensive_rate` | killable+real_kill_line & chosen=non_offensive / killable+real_kill_line | **< 2%** |
| `killable_wrong_target_rate` | killable+real_kill_line & chosen=offensive & target≠intent.target / знаменатель | **< 5%** |
| `kill_conversion_rate` | killable+real_kill_line & chosen_kills_now≥1 / знаменатель | **> 85%** |
| `false_gate_rate` | gate_applied & (survival OR last_stand) & committed должен был быть defensive / gate_applied | **< 3%** |
| `gate_uselessness_rate` | gate_applied & kn=0 & dmg_vs_target<hp·α / gate_applied | **< 5%** (диагностический) |

`α = 0.3` — фиксированный порог «real kill-line» (damage_vs_target ≥ hp·α). Значение согласовано между gate-проверкой и диагностической метрикой — меняется одновременно в обеих точках.

`has_real_kill_line` считается по `plans_shown` (top-10 в логе), а не по всем evaluated. Редкие kill-линии вне top-10 пропускаются — acceptable approximation, записано в методологии.

### 5.3. Шаг 4 (self/ally split) — acceptance

| Метрика | Цель |
|---|---|
| `panic_ally_directed_commit_rate` — ProtectSelf + chosen = ally-directed heal & self_survival < ε_self / ProtectSelf-entries | **< 2%** |
| unit-тесты: ProtectSelf с self-heal → pass; ProtectSelf с ally-heal (без self-AoE) → fail threshold | 100% |

### 5.4. Шаг 5 (summon) — acceptance

| Метрика | Цель |
|---|---|
| `summon_spam_rate` — ≥3 summon одного класса подряд у одного актора за 5 ходов | **≤ 1%** |
| Legitimate buff-стэк (haste+armor у танка) в regression-тесте | pass |

### 5.5. Регрессионные guard'ы

На каждом шаге Δ метрик, которых шаг **не должен** трогать, ≤ 5%. Иначе — investigation, не auto-merge.

### 5.6. Замечания по метрикам

- `panic_leak_rate` в прошлой итерации был переопределён (см. историю §11 удалённой версии). Текущее определение не ломается этой итерацией — раздел 5.3 добавляет более узкую метрику поверх.
- Baseline на corpus'е, не на одном логе: 33% — цифра из одного `stormborn` боя, для честного «до/после» нужен corpus ≥ 10 боёв с фиксированными seed'ами.
- M4.* метрики требуют schema v15 (шаг 2.5) — backfill на старых логах невозможен. Новый corpus собирается после v15.

---

## 6. Что вне scope

- **Trade economy** (`trade.rs`). Работает изолированно.
- **Adaptation layer**. Триггеры `ExpectedSelfLethal / ProtectSelfFutile` — fact-based, их не трогаем. Килlable gate живёт **поверх**, уважая adaptation'овый mode.
- **Intent selection** (`select_intent`). Меняем как план оценивается, не как intent выбирается. Исключение: если после шагов 1–5 остаются нелогичные intent-выборы — отдельная итерация.
- **Marginal board value** для summon'ов. Технический долг, следующий квартал.
- **Sanity pipeline** (multiplicative penalties). Вне scope — ценовая коррекция, не value function.
- **Difficulty knobs**. Веса новых осей пинним хардкодом; в `difficulty.rs` выносятся только после стабильности.

---

## 7. Риски и миграция

- **Schema bump v15** (шаг 2.5) ломает совместимость с v14-логами на уровне M4.* метрик. Старые поля остаются через `#[serde(default)]`, новые gate-поля на v14-логах считаются `false` — метрики корректно деградируют к «недоступно».
- **Параметр-тюнинг**: `ε_self` (3.3), `α = 0.3` для kill-line (5.2), buff_saturation_penalty `-0.4` (существующий). Все пиннятся в коде на первом проходе; в difficulty-профили выносятся только после 2–3 недель стабильности.
- **Порядок критичен**: шаг 3 (gate) **под** шагом 2 (checkpoint). Делать gate до того, как tempo + R3 стабилизировали `has_real_kill_line`, — риск лечить симптом, а не причину.
