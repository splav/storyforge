# AI Need Signals — входная спецификация для шага 3

**Источник:** mining 36 JSONL-логов, 761 AI-решение, 273 `plan_divergence` (commit `7af7e95`, утилита `cargo run --release --bin mine_ai_logs -- --dir logs/`).

**Роль документа.** Артефакт шага 0.4: фиксирует *реальные* патологии поведения AI, обнаруженные в существующих логах, сопоставляет их с need-сигналами из [шага 3 плана](ai_rework.md#3-appraisal--need-layer) и даёт конкретные входы под response curves. Коммитится вместе с реализацией шага 3; дизайн need layer опирается на этот документ, а не на гипотезу о патологиях.

**Как читать.** Каждая секция — «патология → частота → гипотеза → need signal / новые входы». Частоты округлены и подразумевают долю от релевантного знаменателя (он указан явно). Числа будут смещаться по мере накопления новых прогонов; document rebuildable по той же утилите перед каждой волной.

---

## P1. FocusTarget переключается на новую цель, пока старая жива и здорова

**Частота.** 53 из 490 FocusTarget→FocusTarget переходов (~10.8%) — старая цель ещё жива в snapshot'е следующего решения. Из них **36 переходов (~7.3% от всех FT→FT)** — старая цель стоит на ≥50% HP, явный отказ от ещё не начатой работы. Только 5 переходов (1%) — это финиш низкохитовой цели (≤25% HP), остальное — дрейф приоритета.

**Гипотеза.** Factor-scoring пересчитывает приоритет каждый ход. Если новая цель оказалась чуть привлекательнее (AoO risk, range, роль) — план-пул спокойно выбирает её, даже если по старой цели уже начата работа. Commitment-сигнала на эту механику сейчас нет; `last_intent` из `AiMemory` хранится, но в scoring не протекает как штраф за abandon'ment.

**Need signal.** `continue_commitment` (уже в списке шага 3).

**Входы, которые должен получать appraisal:**
- `last_target_id: Option<EntityId>` — из `AiMemory.last_intent`.
- `last_target_alive: bool` и `last_target_hp_ratio: f32` — по snapshot'у.
- `last_target_reachable: bool` — есть ли маршрут в AP-бюджете.
- `damage_already_dealt_to_last_target: i32` — сколько HP мы ему уже выбили (из `AiMemory` или восстановлено из разницы max_hp - hp, если мы были единственным атакующим; точное — через team blackboard в шаге 13).

**Response curve.** Logistic: сигнал высокий, если старая цель жива, достижима и ещё >25% HP, и падает к нулю, когда цель уже низкохитовая (финишер — это не abandon) или недосягаема (правильная смена).

**Замечание.** Не делать `continue_commitment` жёстким приоритетом — он должен проигрывать явно лучшему ходу (новая цель умирает этим ходом, старая укрылась за AoO-цепью). Logistic с плавным плато 0.6–0.8 в середине, а не step function.

---

## P2. Panic override повторно стреляет на стабильном HP

**Частота.** 38 panic_override решений (5.0% от всех). 100% срабатывают при hp_ratio ≤ 0.25 — порог `survival_hp_threshold` работает. Но **9 из 31 panic'ов с известным предыдущим HP (~29%)** — HP не изменился больше чем на 5% с предыдущего решения актёра. То есть актёр стрелял panic'ом, не получив нового урона. Из panic'ов 6/38 сразу после предыдущего panic'а — то есть тот же актёр «паникует» ход за ходом при том же состоянии.

**Гипотеза.** Panic — это state-driven override, без hysteresis и без учёта того, что актёр уже на прошлом ходу выбрал defensive действие. Результат — либо застревание в цикле «panic → no defensive → fallback → panic again», либо flip-flop между panic и best_priority при одном и том же HP. Нужен сигнал «ситуация ухудшилась», а не «ситуация плохая».

**Need signal.** `self_preserve` (уже в списке) **+ новый вход `recent_damage_taken`**.

**Входы appraisal:**
- `current_hp_ratio: f32`.
- `hp_ratio_at_last_turn: f32` — из `AiMemory`, *per-actor*, обновляется в `post_action` системе.
- `recent_damage_taken: f32` = `hp_ratio_at_last_turn - current_hp_ratio`, клампится в [0, 1].
- `last_turn_was_defensive: bool` — выбрал ли актёр ProtectSelf на прошлом ходу.
- `turns_in_low_hp: u8` — сколько ходов подряд HP ≤ threshold.

**Response curve.** Урон на самом деле создаёт urgency, низкий HP — поддерживает её. Комбинация: `self_preserve = logistic(hp_deficit) * (1 + alpha * recent_damage_taken)`, где `alpha` — множитель за свежий урон. Если `last_turn_was_defensive` и `recent_damage_taken ≈ 0` — сигнал приглушается (мы уже защитились, ситуация не деградирует).

**Отдельный артефакт.** Этот вход **не упомянут** в текущей формулировке шага 3 в `ai_rework.md:92`. При коммите реализации обновить список входов явно.

---

## P3. Depth-0 «пустой план» при доступном AP

**Частота.** 152 из 761 chosen plans (20%) имеют `steps.len() == 0`. Из них:
- 105 при `actor_ap = 0` (69%, легитимно — нет ресурса).
- **47 при `actor_ap ≥ 1` (31%, ~6.2% от всех решений)** — у актёра был AP, выбран null. Разбивка по selection_kind: 22 `protect_self_no_defensive`, 19 `viability_fallback`, 6 прочее.

**Гипотеза.** Когда fallback-путь не нашёл ни defensive, ни атакующего плана с положительным скором — возвращается пустой план. Но `Reposition` как intent выбирается только в 3 решениях из 761 (0.4%) — он *почти никогда* не побеждает, значит его скоринг систематически слабее, чем должен быть для «АП есть, делать нечего». Система выбирает «ничего», когда должна была выбирать «встать поближе / спрятаться / занять LoS».

**Need signal.** `reposition` (уже в списке).

**Входы appraisal:**
- `has_ap: bool` и `residual_ap_after_plan: i32` — насколько простаиваем.
- `threat_distance: f32` — дистанция до ближайшей реальной угрозы (из influence maps).
- `best_position_improvement: f32` — лучший делта `evaluate_position` среди достижимых гексов текущим AP-бюджетом.
- `engagement_gap: bool` — никто из врагов в `max_attack_range` любым нашим действием.

**Response curve.** Linear clamped: `reposition ∝ max(0, best_position_improvement - idle_threshold)`. Дополнительно — boost, если `engagement_gap && has_ap`, чтобы «свободные AP» не протухали в null-плане.

**Побочный эффект.** После того, как `reposition` начнёт нормально конкурировать, depth-0 decisions с AP≥1 должны упасть с ~6% до ≤1% (нижняя планка — случаи, когда реально и спрятаться некуда, и встать лучше некуда). Это measurable gate для шага 3.

---

## P4. `viability_fallback` выбирается в 5% решений

**Частота.** 39 из 761 (5.1%). Это fallback-ветка, когда ни один intent не прошёл viability. Из них 19 оказались в depth-0 (см. P3), остальные 20 — какие-то вырожденные planы. 

**Гипотеза.** Пересечение с P3: viability-проверка режет все планы, падает в fallback, fallback не умеет «переместиться и ждать». Частично лечится тем же `reposition` сигналом из P3 + возможностью intent'а быть «низкой срочности», но не вырожденным.

**Need signal.** `reposition` (то же, что P3) + пересмотр `conserve_resource` для кейса «все планы дорогие, резерв ценнее».

**Входы appraisal:**
- `resource_pressure: f32` — отношение `current_mana / max_mana`, аналогично rage/energy.
- `cheap_action_available: bool` — есть ли план с `cost_ap ≤ 1` и положительным outcome.

**Response curve.** `conserve_resource` — logistic от `resource_pressure` с порогом ~0.3: при низких ресурсах вес «дешёвого» плана растёт, вес «потратить последнюю ману» — падает.

---

## P5. Continuation чаще всего ломается по актёрскому урону

**Частота.** 273 `plan_divergence` события. Разбивка:
- `continuation_invalid` — 191 (70.0%). Технический код: план просто не был продолжен (новая валидация, новый ход). Не патология.
- `actor_hp_drop` — **59 (21.6%)**. Актёр получил урон, план инвалидирован.
- `target_hp_drop` — 13 (4.8%). Цель уронили союзники/реакции, план инвалидирован.
- `target_moved` — 5 (1.8%).
- `actor_status_changed` — 4 (1.5%).
- `actor_pos_mismatch` — 1 (0.4%).

**Гипотеза.** 21.6% continuation'ов ломаются, потому что актёр получил урон между ходами — это *тот же самый сигнал*, что P2 (recent_damage_taken), только наблюдаемый со стороны continuation. Если appraisal уже знает про свежий урон *до* scoring, pick_action сразу выберет правильный план, и continuation-инвалидация станет избыточной. Это не «continuation сломан» — это «continuation-репланирование перекрывает дыру, которую должен был закрыть need signal».

**Need signal.** `self_preserve` с входом `recent_damage_taken` (см. P2) + `focus_fire` (новый, см. ниже).

**Входы appraisal (в дополнение к P2):**
- `target_damage_taken_since_last_turn: f32` — по цели `last_intent`, из снапшотов.

**Response curve.** Для target_hp_drop — это вход в `finish_target` need: если цель потеряла заметный HP между ходами, finish'ить её сейчас ценнее. А также сигнал для coord — если кто-то из своих уже начал работу по цели, `focus_fire` предлагает присоединиться, но это уже зона шага 13 (team blackboard).

---

## P6. Intent `Reposition` выбирается почти никогда (0.4%)

**Частота.** 3 из 761 (0.4%). Одновременно 47 решений «AP есть, плана нет» (P3). Асимметрия очевидна.

**Гипотеза.** Либо scoring Reposition слишком слаб, либо его viability отсекается тем же фильтром, который душит fallback'и. Диагностика через шаг 2a (миграция констант) + response curve'ы в шаге 2b: вес repositioning при idle должен расти нелинейно (logistic от `engagement_gap` + threshold).

**Need signal.** `reposition` (см. P3).

**Замечание.** Это не отдельная патология, а follow-up-метрика для P3. Целевой diapazon post-шаг-3 — `Reposition` должен вырасти до ~3–5% от всех решений (заменяя часть depth-0 и часть viability_fallback).

---

## P7. adaptation_reason = `protect_self_no_defensive` шумит (4.3% планов)

**Частота.** 270 из 6236 планов (4.3%). Adaptation срабатывает потому, что у актёра нет defensive-способности, — применяется штраф/подстройка.

**Гипотеза.** Это не патология поведения, а патология сигнала: для юнита без defensive abilities флаг срабатывает *всегда*, когда адаптация запрашивает защиту. Постоянный сигнал ничего не сообщает. Решение для шага 3 — не создавать adaptation-path для таких юнитов, а сразу направлять self_preserve на `reposition` / `tactical_retreat` / `hide` как альтернативу защите.

**Need signal.** `self_preserve` с **веткой по наличию defensive tool**.

**Входы appraisal:**
- `has_defensive_ability: bool` — derived из `caster_ctx.abilities` + ability tags (потребует semantic AI tags из шага 8).
- `retreat_direction_available: bool` — есть ли reachable-гекс с `threat_distance` больше текущего.

**Response curve.** Если `has_defensive_ability = false`, `self_preserve` транслируется в `reposition` с boost за `retreat_direction_available`. Это прямая связка двух need-сигналов; нужно явное правило «если A и не B, то усилить C».

**Прим.** Полноценное решение требует semantic AI tags (шаг 8) или явного поля `defensive` в TOML abilities. До этого можно закодировать whitelist способностей в appraisal.

---

## P8. taunt_forced self-loop — *не* патология

**Частота.** 118 из 150 taunt_forced→taunt_forced (78%). Self-loop rate всего датасета 59%.

**Гипотеза.** Taunt — это ход-блокирующий статус с длительностью ≥1 хода. Высокий self-loop — механическое следствие длительности, не ошибка.

**Need signal.** Не нужен. Документируется как «ожидаемое поведение, не оптимизируем».

**Замечание.** Если после шага 3 self-loop `taunt_forced` начнёт расти сильно выше 78%, это будет сигнал о том, что taunt-длительности завышены по контенту; но сейчас — baseline.

---

## Сводная таблица: need signal → входы → response curve

| Need signal | Новые входы appraisal | Forma curve | Основная патология |
|-------------|----------------------|-------------|-------------------|
| `continue_commitment` | `last_target_id/alive/hp/reachable`, `damage_already_dealt` | logistic (плавное плато 0.6–0.8) | P1 |
| `self_preserve` | `recent_damage_taken`, `hp_ratio_at_last_turn`, `last_turn_was_defensive`, `turns_in_low_hp`, `has_defensive_ability` | logistic(hp_deficit) × (1 + α·recent_damage) | P2, P5, P7 |
| `finish_target` | `target_damage_taken_since_last_turn` | logistic от killability | P5 |
| `reposition` | `threat_distance`, `best_position_improvement`, `engagement_gap`, `has_ap`, `residual_ap_after_plan` | linear clamped + boost за idle AP | P3, P4, P6, P7 |
| `conserve_resource` | `resource_pressure`, `cheap_action_available` | logistic от `resource_pressure` | P4 |
| `rescue_ally` | — (не возникло в текущих логах) | — | — |
| `apply_cc` | — (метрик мало, попадёт в след. итерацию) | — | — |
| `setup_aoe` | — (17 из 761 = 2.2%, baseline стабилен) | — | — |

## Что не покрыто текущими логами

- **Rescue ally.** В демо-сценариях союзник под угрозой смерти возникает редко; нужна сборка логов из сценариев с ближнебойным тылом. Добавится во второй итерации mining'а.
- **CC urgency.** Apply_cc не видно в selection_kind — возможно, потому что CC-способности работают через FocusTarget и не требуют отдельного intent'а. Уточнится после outcome vector (шаг 4), когда `effect_type ∈ {CC}` станет явным в `ActionOutcomeEstimate`.
- **Team focus-fire конфликты.** `target_hp_drop` (4.8% continuation-failure) видно, но без reservations-snapshot'а (шаг 1.1) нельзя отличить «свой finisher» от «перекрывающиеся удары». Повторить mining после внедрения 1.1.

## Обновление контракта шага 3

В [ai_rework.md:92](ai_rework.md#3-appraisal--need-layer) список входов need layer расширяется:

> **Входы** — tactical facts из `BattleSnapshot` + `AiMemory` (last_intent, hp_at_last_turn, last_turn_kind) + influence maps: hp%, **recent_damage_taken**, **last_target_commitment**, danger, killability, reach gap, LoS quality, cluster quality, retaliation risk, resource ratio, **engagement_gap**, **best_position_improvement**.

Эту поправку закоммитить **одновременно** с реализацией шага 3 — чтобы дескриптор шага соответствовал реально подаваемым входам.

## Gate метрики (повторный mining после шага 3)

После реализации шага 3 прогнать `mine_ai_logs` на новом наборе логов и свериться с таргетами:

| Метрика | Baseline (текущий) | Таргет после шага 3 | Примечание |
|---------|--------------------|---------------------|------------|
| FocusTarget-switches с живой старой целью ≥50% HP | 7.3% от FT→FT | ≤ 2.5% | P1, `continue_commitment` |
| Panic'и на стабильном HP (|Δ|<5%) | 29% от panic'ов | ≤ 10% | P2, `recent_damage_taken` |
| Depth-0 при actor_ap ≥ 1 | 6.2% всех решений | ≤ 1.0% | P3, `reposition` |
| Reposition как chosen intent | 0.4% | 3–5% | P6, подтверждение P3 |
| plan_divergence / actor_hp_drop | 21.6% divergence | ≤ 12% | P5, front-loading |

Цель — не «все вверх/вниз», а *ожидаемый сдвиг в нужную сторону*. Если после шага 3 какая-то метрика не сдвинулась или ушла не туда — это повод вернуться к спецификации, а не проталкивать дальше.
