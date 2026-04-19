# Known Issues — AI subsystem

Архитектурный аудит `src/combat/ai/` + `src/combat/effects_*.rs` (~9600 строк).
Дата: 2026-04-19.

Разбит на 5 осей: архитектура, дублирование, сомнительные абстракции, прочие проблемы, странная логика.

Статус-разметка: **✓ fixed** — исправлено в коммите, описание оставлено для контекста.

---

## 1. Архитектурные проблемы

### 1.1. `UtilityContext` — god-struct со смешанными обязанностями ✓ fixed (086b522)

`utility/mod.rs:63-78` тащил через все слои 7 разнородных полей:
- статические: `content`, `difficulty`
- per-actor: `caster`, `abilities`
- scoring-tuning: `crit_fail_effect`, `crit_fail_chance`
- per-turn infra: `blocked_tiles`
- `opponent_team` — не читался после построения snapshot (target'ы берутся через `snap.enemies_of(actor.team)`)

Результат — `#![allow(clippy::too_many_arguments)]` на каждом файле.

**Исправлено:** `UtilityContext { world: AiWorld, actor: ActorCtx }` — world-scope и per-actor разделены; `opponent_team` удалён; `blocked_tiles` вынесен в явный параметр entry-point функций.

### 1.2. Двойная симуляция одного плана ✓ fixed (8809b9e)

`generator.rs:71` → `replay(snap, actor, &plan.steps, ctx)` строил sim'ы при beam-search; каждое расширение клонило snapshot и гоняло `apply_step`.

Затем `scorer.rs:201` в `compute_plan_factors` **снова** создавал `SimState::from_snapshot(snap, ...)` и повторно применял все шаги, чтобы получить pre-step позицию для `ScoredStep::from_plan_step`.

Outcomes уже лежат в `plan.outcomes`; это было O(plans × depth) лишних `clone + apply_step` на каждый тик.

**Исправлено:** `TurnPlan.sim_snapshots: Vec<BattleSnapshot>` (runtime-only, `#[serde(skip)]`) — generator кэширует post-step snapshot при extend; scorer читает pre-step из кэша; `replay()` удалён.

### 1.3. Intent factor max-агрегация vs. committed-prefix семантика ✓ superseded (b517b05)

`scorer.rs:237-248` брал `intent_max = max(intent_score)` **по всем шагам плана**. Но `commit_plan` коммитит только первый (solo) или первые два (Move+Cast bundle). План с плохим шагом-1 и сильным шагом-3 получал высокий `intent` факторный сигнал, хотя шаг-3 никогда не выполнится.

**Первый фикс (b2e2237):** gate — `intent_score` участвует в агрегации только при `idx < committed_step_count`. Решал патологию, но отсекал real signal от длинных approach-планов.

**Superseded (b517b05):** переход на discounted sum. Intent аккумулируется по всем шагам с `step_weight = base^k`. Deep approach plans получают частичный signal (0.72 на depth 3), direct Cast даёт 1.0 — gradient вместо binary. Патология 1.3 решается через самой decay'ой — plохой глубокий Cast весит меньше commit'нутого, position/risk выбирают между plans с одинаковым первым шагом.

### 1.4. `BattleSnapshot.active_unit` — почти неиспользуемое поле ✓ fixed (664eea8)

Записывалось в `build_snapshot`, читалось только `active()` helper (который сам нигде не вызывался) и для `allies_of` фильтра «сам себя» в `influence.rs:135`.

**Исправлено:** поле `active_unit` и метод `active()` удалены. `build_influence_maps` теперь принимает `active_entity: Entity` явным параметром. Тесты освободились от 18+ фиктивных `active_unit: ...` инициализаций.

### 1.5. `sanity_adjust_plans` смешивает penalties + bonus

`sanity.rs:146-155` пункт 7 — **multiplicative +10% bonus** за «safer tile + useful cast». Остальные 6 пунктов — штрафы. Если «sanity» — это проверка на глупости, то бонус там чужеродный; логически он принадлежит scoring-этажу.

### 1.6. `run_ai_turn` — всё ещё god-function

`enemy_turn.rs:82-217` — 136 строк, 14 параметров (уже с двумя `SystemParam` группировками). `AiEnv` и `AiMessages` только обошли лимит Bevy, но не решили само засилье.

### 1.7. Drift sim ↔ real не закрыт

`docs/ai.md` сам признаёт:
- drift #3 (rage-gain не моделируется в sim)
- speed mid-plan не re-flow в pathing

Планировщик строит планы на предпосылке о статичном speed, но live pipeline может его менять.

### 1.8. `kill_max` и `focus_max` — параллельная патология с intent_max ✓ superseded (b517b05)

`scorer.rs` агрегировал `kill_max` и `focus_max` как **max по всем Cast-шагам плана** без discount и без гейтинга по committed prefix.

**Первый фикс (09659d5):** зеркально 1.3 — committed-prefix gate. Решал патологию, но отсекал real signal.

**Superseded (b517b05):** вместе с 1.3 переход на discounted sum. `kill_sum` и `focus_sum` аккумулируют per-Cast с `step_weight = base^k`. Плюс неочевидное улучшение: plan убивающий 2-х врагов теперь scorится выше plan убивающего 1-го (max collapsing их equal); plan с двумя Cast'ами на priority targets опережает один Cast. Consistent с damage/cc/heal/scarcity — все Cast-факторы теперь аккумуляты.

### 1.9. FocusTarget viability threshold не согласован с intent aggregation ✓ fixed (b517b05)

После фикса 1.3 (gate era) `intent_score(FocusTarget, Move) = 0.0`. Committed Move-toward-focus давал intent_factor=0.0. Viability threshold для FocusTarget = **1.0** — план приближения к focus-цели всегда валился в viability fallback → `default_focus_target` переключался на другого врага.

**Исправлено (b517b05):** с переходом на discounted sum (superseding 1.3/1.8), intent план approach'а с Cast@focus где-то в хвосте аккумулирует 0.72–1.0. Threshold снижен с 1.0 до **0.5** — approach-and-strike trajectory проходит viability, "no reachable focus at all" plans всё ещё попадают в fallback.

### 1.10. Post-goal bump был произвольный `×0.5` ✓ fixed (ea38be5)

В scorer'е был `POST_GOAL_DISCOUNT = 0.5`: когда шаг убивает текущую цель интента, **все** дальнейшие Cast-шаги получали дополнительный `×0.5` к step_weight — их damage/cc/heal/scarcity вклады халфились. Rationale в docs: "post-kill actions are bonuses, not peers of the goal step".

Проблема: семантика "bonus" неопределена. `×0.5` — произвольная магия. Если план после убийства focus-цели продолжает полезные действия (heal союзника, CC другого врага) — эти действия **сами по себе** полезны. Их scoring не должен ни поощряться, ни штрафоваться фактом предшествующего kill'а.

**Исправлено:** `POST_GOAL_DISCOUNT` удалён. step_weight теперь чисто геометрический (`base^k`), без post-goal bump'а. Intent aggregation **пропускает** шаги после goal (они ортогональны satisfied intent'у). Другие факторы scorят post-goal действия по их merit без лишнего множителя.

---

## 2. Дублирование

### 2.1. `build_reach` — 2 идентичных BFS (+ отдельная data-prep) ✓ fixed (51ce0bd)

Исходный аудит насчитал «3 реализации», но `enemy_turn.rs:124-128` — это не BFS, а конструкция входного `HashSet<Hex>` для `blocked_tiles`. Реальная дупликация:

- `generator.rs:432-459` — для sim внутри beam-search
- `fallback.rs:77-100` — edge-case когда актор пропал

Комментарий в `fallback.rs` прямо признавал: «duplicates but edge case». Две BFS-обёртки были байт-идентичны кроме источника `(actor, snapshot)` пары.

**Исправлено:** `planning/reach.rs::reach_from(snap, actor, blocked_tiles)` — единственный helper. Обе копии удалены, generator и fallback зовут общий API. Defensive early-return на `sim.actor_unit() == None` был мёртвым (upstream caller filters), новая сигнатура требует `&UnitSnapshot` — невозможное состояние не выражается.

### 2.2. AoE area — 5 мест, одно самописное ✓ fixed (be9fe65)

Канонический `effects_math::aoe_cells` + `factors/offensive::aoe_area` (HashSet-wrapper) используются в scoring, picker, intent, generator.

А `sanity.rs:240-246` (`plan_has_self_aoe`) **переопределял** геометрию: ручной `hex_circle` / `hex_line`, минуя общий `aoe_cells`. Добавление `AoEShape::Cone` молча обошло бы self-AoE проверку.

**Исправлено:** `plan_has_self_aoe` перведён на `aoe_area`. Dead импорты `hex_circle`/`hex_line` удалены. Regression-тест пришит.

### 2.3. AoE filtering of hits — 4 копии

`compute_affected_targets<TargetState>` в `effects_state.rs` — канон. Но:
- `offensive::compute_aoe_damage` (line 82-110)
- `offensive::compute_offensive` (AoE ветка line 53-69)
- `picker::record_committed_reservations` (line 232-238)
- `sanity`, `scarcity`

Везде самописное `snap.enemies_of(team).filter(|e| area.contains(&e.pos))`. Friendly-fire семантика реализована неполно (в scoring — только сам actor, в канонической — и allies).

### 2.4. `killability` — две копии ✓ fixed (664eea8)

- `target_priority.rs:36`: `1 - eff_hp/eff_max` inline
- `generator.rs:421-427`: идентичная private fn

**Исправлено:** добавлен метод `UnitSnapshot::killability(&self) -> f32` с zero-eff-max guard. Оба call site'а переведены на метод, приватная fn удалена.

### 2.5. «Can afford» — три копии ✓ fixed (2ad7c97)

- `generator::can_afford` (AP+ресурсы, UnitSnapshot)
- `snapshot::compute_tags` (inline по Bevy query, line 313-321)
- `scarcity::compute_scarcity` (resource_ratio по тем же полям, line 37-52)

Все три читали `match resource {Hp|Mana|Rage|Energy}` одинаково.

**Исправлено:** введён low-level `pool_amount(kind, hp, mana, rage, energy)` в `snapshot.rs`; на `UnitSnapshot` добавлены методы `resource_amount(kind)` и `can_afford(def)`. Все три call site'а пустили через общий helper — match на ResourceKind живёт в одной функции.

### 2.6. Проходы по статусам — три

В `build_snapshot` отдельно `compute_tags`, отдельно `status_bonuses` (`snapshot.rs:373-389`), плюс `refresh_status_aggregates` (`snapshot.rs:115-123`) в sim. Три прохода по `StatusEffects` на одном юните с пересекающимися полями.

### 2.7. `score_plans` — мёртвая обёртка ✓ fixed (664eea8)

`scorer.rs:48-59` — `score_plans(...) { score_plans_with_raw(...).0 }`. Единственный вызов `pick_action` идёт в `_with_raw`. Обёртка была только в `pub use`.

**Исправлено:** fn удалена, pub use обновлён, doc-refs в difficulty.rs и sanity.rs перенаправлены на `score_plans_with_raw`.

---

## 3. Сомнительные абстракции

### 3.1. Bundling-логика размазана по трём местам ✓ fixed (0bc399a)

Правила committed-prefix (`[Cast,..]→1 step`, `[Move,Cast,..]→2 steps`, `[Move,..]→1 step`) жили в трёх параллельных pattern-match'ах:

- `picker::commit_plan` (`picker.rs:45-83`) — конструировал `AiDecision`.
- `ScoredStep::from_plan_committed` (`factors/mod.rs:104-126`) — строил single-step view для debug и `default_focus_target`.
- `TurnPlan::committed_step_count` (`types.rs`, добавлено фиксом 1.3) — возвращал число закоммиченных шагов для scorer-gating.

Все три руками повторяли одни и те же match-arms. При добавлении нового варианта бандлинга — три синхронные правки, drift гарантирован если хоть одну забыть.

**Исправлено:** введён `CommittedPrefix<'a>` enum (`types.rs`) с 4 вариантами + `TurnPlan::committed_prefix()`. Все три потребителя матчат на enum'е; новый вариант бандлинга — один arm в enum + compile-error укажет три места где нужно дописать. Edge cases (empty path → EndTurn/CastInPlace) остались слоем над prefix в `commit_plan`, не протекли в общий контракт.

### 3.2. `PickMechanics` протаскивается через all pick API

`picker.rs:8-16` + возврат `(usize, PickMechanics)` — но `PickMechanics` используется только для debug overlay. Для реального pick'а это ненужный груз. Лучше две функции: `pick_best_plan` возвращающая index, и `pick_best_plan_with_mechanics` для debug.

### 3.3. `DiceSource::roll_crit_fail` + `CritFailEffect::Miss` — deadweight в sim-пути

`sim.rs:143-151` явно передаёт `crit_fail_die = 20`, `effect = CritFailEffect::Miss`, и тут же комментарий: «ignored in practice». Это симптом того, что абстракция `DiceSource` не совсем та, что нужна — реальный водораздел «вероятностный/MAP», а не «источник случайности».

### 3.4. `empty_blocked_tiles() -> &'static HashSet` через `OnceLock` — костыль

`utility/mod.rs:82-87` — чтобы тесты могли построить ctx. Запах: signature `blocked_tiles: &HashSet<Hex>` слишком жёсткая. Стоило бы `Cow<HashSet<Hex>>` или `Option<&HashSet<Hex>>` (None = empty).

### 3.5. `AiDecision::MoveCloser` vs. `MoveOnlyRetreat`

Два варианта с одинаковым payload и одинаковой обработкой (`enemy_turn.rs:207-212` — `|` pattern). Различие только семантическое (retreat vs approach) — используется в debug-строке. Семантика теряется сразу после commit'а. Слить.

### 3.6. `CritFail` enum + `mana_overload: bool` + `primary: None` — тройное кодирование одного события

`effects_outcome.rs:71-88`: `crit_fail: Option<CritFail>`, `mana_overload: bool`, и при crit-fail `primary` принудительно None. Три флага кодируют один факт; легко создать невозможные комбинации.

### 3.7. `plan_summon_bonus` — post-normalization additive-hack

`scorer.rs:127` подмешивает `summon_bonus` **после** `dot(weights, normalized_factors)`. Каждый следующий «особый бонус» будет так же bolted-on сбоку. Этой абстракции нет имени — неявный 10-й фактор.

---

## 4. Другие архитектурные проблемы

### 4.1. Debug-снапшот re-вычисляет факторы, но в другой семантике ✓ fixed (9135940)

`debug.rs:485-509` re-запускал `compute_factors(&ScoredStep::from_plan_committed, ...)` per top-5 — это давало **per-single-step** числа, тогда как `raw_factors` из scoring — plan-aggregate (discounted sum).

В дебаге и в JSONL-логе одинаково звались «factors», а числа были разные. Смысловой сдвиг скрыт.

**Исправлено:** `build_debug_snapshot` теперь принимает `raw_factors: &[[f32; NUM_FACTORS]]` параметром — те же plan-aggregate значения, что уходят в log. Нет recompute'а, нет drift'а.

### 4.2. `reservations` — global mutable state, mutation в одном pass со scoring

`pick_action` читает reservations внутри factor-adjustments (`adjustments.rs:22-38`), затем после commit'а пишет (`record_committed_reservations`). Работает только в single-threaded Bevy system; не годится для параллельного выполнения AI-тиков разных юнитов.

### 4.3. `memory` copy-out / copy-in каждый тик

`enemy_turn.rs:167-170`: `std::mem::take(&mut *m)` выхватывает всю `AiMemory`, потом `*mem = memory;` заливает обратно. Лишняя копия — ссылки из `memories.get_mut(actor)` хватило бы, если сигнатура `pick_action` приняла бы `&mut AiMemory`.

### 4.4. Hard thresholds в `select_intent`

- `intent.rs:162` (`hp_pct < 0.4`)
- `snapshot.rs:290` (`hp_pct < 0.3` для LOW_HP)

Рядом с `difficulty.survival_hp_threshold()` — смешение difficulty-driven и hard-coded порогов в одном модуле.

### 4.5. `default_focus_target` крутится через «plans → committed step targets»

`intent.rs:344-348`: множество «reachable targets» — это `plans.iter().map(|p| ScoredStep::from_plan_committed(p).target())`. То есть «какие враги достижимы» выводится косвенно через планировщик.

Прямее было бы: `enemies_of.filter(|e| reach_budget >= dist)`. Сейчас `default_focus_target` полагается на то, что планировщик породил хоть один план на каждый живой target.

### 4.6. AoO handling дублирован в двух слоях

- `sanity::expected_aoo_damage` (plan-level penalty)
- `snapshot::build_snapshot` (aoo_expected_damage на UnitSnapshot как источник)

Расчёт `net = raw - armor + vuln` — только в sanity. А live pipeline `movement.rs` (упомянут в комменте) — третий источник. 3 места, легко рассинхронизировать.

### 4.7. `enemies_of` / `allies_of` hardcoded на 2-team

`snapshot.rs:264-275` — match на `Team::Player/Enemy`. Не масштабируется. Возможно, сознательный дизайн — стоит зафиксировать enum exhaustive.

---

## 5. Странная логика

### 5.1. `picker::pick_best_plan` — sample через `rng.roll_d(len).saturating_sub(1)`

`picker.rs:184`. `roll_d` семантика — 1..=N, вычли 1 → 0-based. Если `pool.len() == 0`, `saturating_sub(1) = 0`, а `pool[0]` panic-unsafe (хотя есть `pool.is_empty()` guard выше). Стиль fragile — идиоматичнее `rng.gen_range(0..pool.len())`.

### 5.2. `plan_is_defensive` — empty plan = defensive by default

`sanity.rs:295`: `let Some(first) = plan.steps.first() else { return true };`. Под `ProtectSelf` это означает, что «ничего не делать» всегда считается защитной опцией. Но если актор стоит в high-danger тайле, empty plan = самоубийство. Логика справедлива только для low-danger позиций.

### 5.3. `score_action` для `Heal` возвращает HP-equivalent через `target.threat`

`scoring.rs:42-43`: `delta_pct × target.threat`. Т.е. «хилнуть союзника» оценивается как «сколько его damage output мы спасли». Но `threat` — это max-ST-damage (см. `estimate_st_damage`), не per-round DPR. За 1 round unit атакует может 1–2 раза. Скейлинг «HP-equiv» натянут.

### 5.4. `focus_max` для empty-plan — специальный hack

`scorer.rs:301-307`. Симптом: factor-aggregation плохо определена для «do nothing»; приходится городить исключение.

### 5.5. Taunt-check — full O(n) сканинг на каждый cast-кандидат

`generator.rs:302-316`: для каждой ability × каждая цель сканирует `sim.snapshot.enemies_of(actor.team)` в поисках FORCES_TARGETING. Это можно 1 раз вынести наружу цикла `enumerate_next_steps`. Сейчас — квадратично по targets × abilities.

### 5.6. `overkill_damage_multiplier` обнуляет kill вместе с уменьшением damage

`adjustments.rs:27-29`: `off.damage *= mult; off.kill = 0.0`. Damage-multiplier — «residual мультипликатор», kill — бинарное «убьёт ли».

Если reservations достаточно убивают цель, **наш ход не kill** — правильно обнулить. Но на hard-difficulty multiplier = 0.3, damage падает до 30%, kill → 0, а на самом деле нас-то кто-то должен добить. Агрессивно.

### 5.7. `infer_profile` Tank-floor всегда ≥ 0.3

`role.rs:190-191`: `p.tank += (eff_hp / 20.0).clamp(0.3, 2.0)` — **всегда** добавляется минимум 0.3, независимо от tank-абилок. У 12 HP glass-cannon голый `eff_hp/20 = 0.6 → tank += 0.6`. Это искажает профиль: любой юнит обычных 15–20 HP уже получит ~1.0 tank-веса, которого нет в его kit-диагностике.

### 5.8. Test-helper `make_ctx` дублирован

`generator.rs:677-693` и `picker.rs:347-362` определяют почти идентичный `make_ctx` для построения `UtilityContext` в тестах. Оба строят `AiWorld { content, difficulty }` + `ActorCtx { caster, abilities, crit_fail_effect: Miss, crit_fail_chance: 0.0 }`. Различия только в cosmetic окружении (какие тесты импортируют что).

Если добавить третий суб-ctx (например, `TurnInfra`), три копии надо править синхронно. Стоит вынести в `#[cfg(test)] pub(crate) mod test_helpers` под `ai/`.

### 5.9. `TurnPlan.sim_snapshots` инвариант только под debug_assert

После фикса 1.2: scorer читает `plan.sim_snapshots[idx - 1]` с предположением `sim_snapshots.len() == steps.len()`. Generator держит этот инвариант (push на каждый apply_step), но `#[serde(skip)]` означает, что десериализованный `TurnPlan` приходит с **пустым** `sim_snapshots` — если scorer когда-нибудь будет вызван на таком плане, в release-сборке будет index out of bounds.

Сегодня безопасно: `replay_ai_log` (единственный call-site десериализации) считает факторы вручную, scorer не зовёт. Но ловушка ждёт.

Подход к фиксу: либо сериализовать sim_snapshots (раздует лог), либо убрать инвариант (fallback к `snap` на миссе), либо typestate (`ScoredPlan` vs. `DeserializedPlan`).

---

## Приоритет фиксов

| Находка | Влияние | Риск фикса | Статус |
|---|---|---|---|
| 1.1 `UtilityContext` god-struct | читабельность, dead field | низкий | ✓ 086b522 |
| 1.2 Двойная симуляция в generator+scorer | CPU, O(plans·depth) лишних clone | средний | ✓ 8809b9e |
| 1.3 Intent max-over-steps vs. committed-prefix | корректность скоринга | средний | ✓ b517b05 (superseded gate@b2e2237) |
| 1.8 `kill_max`/`focus_max` — параллельная 1.3 патология | корректность скоринга | средний | ✓ b517b05 (superseded gate@09659d5) |
| 1.9 FocusTarget viability threshold не согласован | viability fallback loop | низкий | ✓ b517b05 |
| 1.10 post-goal произвольный `×0.5` bump | семантика scoring | низкий | ✓ ea38be5 |
| 2.1 `build_reach` × 2 | DRY | низкий | ✓ 51ce0bd |
| 2.2 `plan_has_self_aoe` своя геометрия AoE | drift-bug waiting | низкий | ✓ be9fe65 |
| 2.3 AoE hits filtering × 4 | friendly-fire drift | средний | — |
| 3.1 Bundling rules × 3 мест (усугубилось после 1.3) | drift bundling | низкий | ✓ 0bc399a |
| 2.4 `killability` × 2 | trivial DRY | низкий | ✓ 664eea8 |
| 2.5 «Can afford» × 3 | DRY | низкий | ✓ 2ad7c97 |
| 2.7 `score_plans` dead wrapper | dead code | низкий | ✓ 664eea8 |
| 1.4 `active_unit` почти dead field | trivial DRY | низкий | ✓ 664eea8 |
| 3.6 `CritFail` + `mana_overload` + `primary=None` | type safety | средний | — |
| 4.1 Debug vs log «factors» имеют разную семантику | аналитика вводит в заблуждение | низкий | ✓ 9135940 |
| 5.7 Tank-floor в `infer_profile` всегда ≥ 0.3 | role mis-inference | средний | — |
| 5.9 sim_snapshots инвариант только debug_assert | release-build crash если scorer зовут на десериализованном | низкий | — |
