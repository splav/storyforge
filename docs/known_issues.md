# Known Issues — AI subsystem

Повторный аудит `src/combat/ai/` + `src/combat/effects_*.rs` (~10 300 строк).
Дата: 2026-04-19.

Разбит на 5 осей: архитектура, дублирование, сомнительные абстракции, прочие проблемы, странная логика. Ранее исправленные находки (1.1–1.4, 1.8–1.10, 2.1–2.2, 2.4–2.5, 2.7, 3.1, 4.1) вырезаны — смотрите git-историю.

**Статус-разметка:** **✓ fixed** — исправлено в текущем цикле; описание оставлено для контекста.

---

## 1. Архитектурные проблемы

### 1.1. `sanity_adjust_plans` смешивает penalties + bonus

`sanity.rs:147–156`, пункт 7 — мультипликативный **+10 % bonus** за «safer tile + useful cast». Остальные 6 проверок — штрафы. Если sanity — это «проверка на глупости», то bonus — логически принадлежит scoring-этажу, а не sanity-этажу. Границы размыты, и следующий «ещё один бонус» легко добавится сбоку, раздув sanity в ad-hoc mini-scorer.

### 1.2. `run_ai_turn` — всё ещё god-function

`enemy_turn.rs:82–215` — 133 строки, 14 параметров (уже с двумя `SystemParam`-группировками `AiEnv`/`AiMessages`). Делает снапшот, maps, ctx, memory, зовёт `pick_action`, декомпозирует `AiDecision` в сообщения. `SystemParam`-bundling обошёл лимит Bevy, но само засилье параметров осталось.

### 1.3. Drift sim ↔ real не закрыт

`docs/ai.md` сам признаёт:
- **rage-gain не моделируется в sim** (real даёт +1 rage attacker/defender на damage — планировщик про это не знает).
- **speed mid-plan не re-flow в pathing**: `UnitSnapshot.speed = base + status_bonus` сохраняет агрегат, но не базу. Статус, меняющий speed в step[k], не пересчитывает achievable tiles для step[k+1].

Оба дрифта описаны — не зафиксированы.

### 1.4. Scoring прогонялся до 3 раз за один тик ✓ fixed

Было: `utility/mod.rs` звал полный `score_plans_with_raw` при viability fallback (стр. 210) и при LastStand-re-score (стр. 240) — каждый полный пересчёт factors per plan. Плюс `picker::mercy_cruelty` для каждого plan в mercy window звал `compute_plan_factors`, чтобы прочитать два числа (kill, cc), уже лежавшие в `raw_factors`.

**Исправлено:**
- `factors::compute_factors` больше не принимает intent: `factor[7]` заполняется отдельно на plan-level.
- `scorer` расщеплён: `compute_plan_factors_sans_intent` (intent-независимые агрегаты) + `compute_plan_intent_sum` (только intent column с `goal_achieved`-latching). `compute_plan_factors` оставлен как thin combinator.
- Новый `rescore_with_intent(plans, raw: &mut [...], new_intent, …) -> Vec<f32>` переписывает только `raw[_][7]` и пропускает весь intent-независимый compute. Используется в viability fallback и LastStand.
- `pick_best_plan` принимает `raw_factors`; `mercy_cruelty` читает `raw[1]`/`raw[2]` вместо recompute. Сигнатура похудела с 9 до 4 параметров.
- Equivalence-тест `rescore_matches_full_score_under_same_intent` в `scorer.rs` пинает, что reuse пути = full recompute путь (на hard difficulty без noise).

---

## 2. Дублирование

### 2.1. AoE filtering of hits — 7 копий ✓ fixed

Было: 7 независимых мест иteprировали `snap.units` и фильтровали по `area.contains(&pos)` — `offensive` (3 sub-вызова), `picker::record_committed_reservations`, `scarcity` (2×), `intent::SetupAOE`, `generator::is_valid_cast`. Плюс баг: `compute_aoe_damage` зовал `snap.allies_of(team)` (включает сам актор) и **затем** отдельно вычитал self-урон — кастера штрафовали дважды.

**Исправлено:** добавлен `factors::aoe_hits(area, active, snap) -> AoeHits { enemies, allies, self_hit }` — один проход, чистое разделение. 7 сайтов переведены на helper; `compute_aoe_damage` стал thin-wrapper (`enemy_sum − splash_sum`, splash = allies ∪ self через `chain(hits.self_hit.then_some(active))`). Double-count бага нет by construction — regression-тест пришит в `factors/aoe_hits.rs`.

### 2.2. Проходы по статусам — 3 раза

`build_snapshot` делает два прохода по одному и тому же `StatusEffects`:
- `compute_tags` (`snapshot.rs:385–397`) — флаги IS_STUNNED / FORCES_TARGETING.
- `status_bonuses` (`snapshot.rs:411–428`) — speed/armor/damage_taken агрегаты.

Плюс `refresh_status_aggregates` (`snapshot.rs:113–121`) в sim mid-plan. Три прохода по одному списку с пересекающимися выборками полей.

### 2.3. Test-helper `ctx`-builder — 3 копии

- `generator.rs:612–628` — `make_ctx`.
- `scorer.rs:389–405` — `test_ctx`.
- `scarcity.rs:186–201` — `scarcity_ctx`.

Все три собирают одинаковый `UtilityContext { world: AiWorld { content, difficulty }, actor: ActorCtx { caster, abilities, crit_fail_effect: Miss, crit_fail_chance: 0.0 } }`. Добавим 4-й sub-ctx — три синхронные правки.

Вынос в `#[cfg(test)] pub(crate) mod test_helpers` внутри `ai/` решает.

### 2.4. `worst_path_danger` дублирует `path_danger_max` scorer'а

`sanity.rs:209–225` считает максимум danger по `start → plan.final_pos`. scorer.rs:211–236 уже посчитал ровно это же значение внутри `compute_plan_factors`. sanity зовётся **после** scoring на тех же планах; доступа к scorer-локалу нет, поэтому считается второй раз. Можно вернуть `path_danger_max` из scorer'а (через faktор `risk` в обратную сторону или отдельным каналом) и передать в sanity.

---

## 3. Сомнительные абстракции

### 3.1. `PickMechanics` протаскивается через весь pick API

`picker.rs:8–16` + возврат `(usize, PickMechanics)` из `pick_best_plan`. `PickMechanics` нужен **только** для debug overlay (`debug.rs:474, 510–532`). В production-пути это неиспользуемая allocation-чересчур структура.

Расщепить: `pick_best_plan` возвращает index; `pick_best_plan_with_mechanics` — index + mechanics для debug. В utility/mod.rs звать второй вариант при `debug=true`, первый иначе.

### 3.2. `DiceSource::roll_crit_fail` + `CritFailEffect::Miss` — deadweight в sim-пути

`sim.rs:143–151` явно передаёт `crit_fail_die = 20`, `effect = CritFailEffect::Miss`, рядом комментарий: «ignored in practice». `ExpectedValue::roll_crit_fail` хардкодит `false`. То есть sim **никогда** не читает ни die, ни effect. Три параметра существуют только ради симметрии с real backend.

Знак того, что `DiceSource` — не та абстракция: реальный водораздел между backends — «вероятностный vs. MAP-estimate», а не «источник случайности». Можно сузить trait до `roll_dice` и вынести crit-fail через отдельный entry point real-backend'а.

### 3.3. `empty_blocked_tiles() -> &'static HashSet<Hex>` через `OnceLock` ✓ fixed

`utility/mod.rs:103–108` хранил `&'static HashSet` через `OnceLock` только ради тестов, которые не хотели материализовать пустой сет. Helper удалён; 5 тестов в `generator.rs` теперь создают локальный `HashSet::<Hex>::new()` перед вызовом — на строку длиннее, без `OnceLock` костыля.

### 3.4. `AiDecision::MoveCloser` vs. `MoveOnlyRetreat`

Два варианта с одинаковым payload (`{ path: Vec<Hex> }`). Обработка в `enemy_turn.rs:205` — один `|`-pattern. Различие только семантическое:
- `MoveOnlyRetreat` — из `commit_plan` (best plan-move).
- `MoveCloser` — из `fallback_move` (планов нет).

Различие нужно только для лейбла в debug/log (`log.rs:161–162`, `debug.rs:609–614`). Можно оставить одну enum variant с `origin: MoveOrigin` полем (enum { BestPlan, Fallback }) — семантика сохранена, арм'ы сливаются.

### 3.5. `CritFail` enum + `mana_overload: bool` + `primary: None` — тройное кодирование одного события ✓ fixed

Было: `crit_fail: Option<CritFail>` + `mana_overload: bool` + неявный invariant «crit_fail.is_some() ⇒ primary == None». Невалидные комбо вроде `Some(Miss) + mana_overload: true` были типово выразимы.

**Исправлено:** коллапс в один `CritOutcome { None, Miss, SelfStatus, SelfDamage, ManaOverload }` с методами `skips_primary()` / `is_mana_overload()`. Компилятор теперь гарантирует, что «crit случился в ManaOverload» и «crit skipped primary» — взаимоисключающие состояния. `map_crit_fail` напрямую эмитит варианты (в том числе graceful-fallback `ManaOverload → Miss` при zero-mana-cost). `resolution.rs` match'ит на одном enum'е с exhaustive-арми; `AbilityOutcome` лишилось одного поля.

### 3.6. `plan_summon_bonus` — post-normalization additive hack

`scorer.rs:125, 141–179`. После `dot(weights, normalized_factors)` подмешивается `summon_bonus` в HP-эквиваленте. Неявный 10-й factor без места в `NUM_FACTORS`, без нормализации, без role-weights. Каждый следующий «особый бонус» будет так же bolted-on сбоку. Абстракции, которая принимает «factor who doesn't fit 9-factor tensor» — пока нет.

---

## 4. Другие архитектурные проблемы

### 4.1. `reservations` — global mutable state, mutation в одном pass со scoring

`pick_action` читает reservations внутри factor-adjustments (`adjustments.rs:22–40`), затем после commit'а пишет (`record_committed_reservations`). Работает только в single-threaded Bevy system; не годится для параллельного выполнения AI-тиков разных юнитов. При переходе на async/parallel AI каждый тик должен взять snapshot reservations при старте и закоммитить дельту в конце.

### 4.2. `memory` copy-out / copy-in каждый тик ✓ fixed

Было: `std::mem::take(&mut *m)` + write-back `*mem = memory;` на выходе. Исправлено: прямой `Mut<AiMemory>::into_inner()` → `&mut AiMemory`, пробрасываемый в `pick_action`. Актору без компонента даётся короткоживущий локальный default (мутации отбрасываются, как и раньше в else-ветке write-back'а).

### 4.3. Hard thresholds в `select_intent` vs. `difficulty.rs`

Рядом в intent selection живут:
- `intent.rs:162` — hard-coded `hp_pct < 0.4` для ProtectSelf.
- `snapshot.rs:334` — hard-coded `hp_pct < 0.3` для `LOW_HP` tag.

Но survival/panic thresholds уже живут в `difficulty.survival_hp_threshold()` / `awareness_danger_threshold()`. Смешение difficulty-driven и магических констант в одном модуле — тяжёлый случай drift'а при балансе.

### 4.4. `default_focus_target` крутится через «plans → committed step targets»

`intent.rs:344–371`: множество «достижимых target'ов» выводится как
```rust
plans.iter().filter_map(|p| ScoredStep::from_plan_committed(p, actor_pos).target())
```

То есть «какие враги достижимы» выводится косвенно через планировщик — при условии, что он породил хоть один план на каждый живой target. Прямее: `enemies_of.filter(|e| reach_budget >= dist)`. Текущая форма скрывает зависимость от output'а beam-search'а внутри intent.rs.

### 4.5. AoO damage formula дублирован в 2 местах ✓ fixed

Было: `(raw − armor + vuln).max(1)` инлайнится в `movement.rs:195` (real pipeline) и `sanity.rs:202` (plan-level penalty). Канонический `effects_math::final_damage_{i32,f32}` уже был, но зовёт его только `sim.rs`.

**Исправлено:** оба call-site'а переведены на `final_damage_{i32,f32}(raw, armor, vuln, /* pierces_armor */ false)`. `snapshot::build_snapshot` (хранит pre-mitigation raw) не трогали — это upstream-data, не дублирующая формула.

### 4.6. `raw_factors[p][7]` — хардкод индекса фактора ✓ fixed

Было: магические индексы `[7]` (intent), `[1]`/`[2]` (kill/cc), `weights[8]` (scarcity) раскиданы по scorer / picker / utility. **Исправлено:** в `factors/mod.rs` добавлены `DAMAGE_IDX … SCARCITY_IDX` рядом с `SIGNED_FACTOR`. Все **reader**-сайты переведены на именованные константы; оставлен литерал только в одном месте — финальной return-строке `compute_plan_factors_sans_intent`, которая **объявляет** layout.

---

## 5. Странная логика

### 5.1. `picker::pick_best_plan` — sample через `rng.roll_d(len).saturating_sub(1)` ✓ fixed

`saturating_sub(1)` был защитой от невозможного случая (`roll_d` возвращает `1..=N`, не ноль — с guard'ом `pool.is_empty()` выше). Заменено на прямое `(rng.roll_d(pool.len() as u32) - 1) as usize`, precondition вынесен в комментарий. `if pool.len() == 1 { 0 }` специальная ветка тоже ушла — `roll_d(1)` детерминированно возвращает 1, поэтому общий путь корректен для всех `len ≥ 1`.

### 5.2. `plan_is_defensive` — empty plan = defensive by default

`sanity.rs:292`: `let Some(first) = plan.steps.first() else { return true };`. Под ProtectSelf это означает, что «ничего не делать» **всегда** считается защитной опцией. Но если актор стоит в high-danger тайле, empty plan = самоубийство. Справедливо только для low-danger позиций.

### 5.3. `score_action` для `Heal` возвращает HP-equivalent через `target.threat`

`scoring.rs:42–43`: `delta_pct × target.threat`. Т.е. «хилнуть союзника» оценивается как «сколько его damage output мы спасли». Но `threat` — это max-ST-damage (см. `estimate_st_damage`), не per-round DPR. За 1 round юнит атакует 1–2 раза. Скейлинг «HP-equiv via threat» натянут; HP-equiv через «сколько рантов он ещё продержится» был бы корректнее.

### 5.4. `focus_sum` empty-plan spec-case

`scorer.rs:303–310`. «Для пустого плана подменяем focus_sum = max(target_priority по всем enemies)», чтобы «ничего не делать» не зарэнкалось с focus=0. Симптом: factor-aggregation плохо определена для «do nothing». Move-only планы (`Move` не вносит в focus_sum ничего) тоже получают focus=0 — но на них этот хак не распространяется. Асимметрия внутри одного and the same «aggregation не покрывает случай».

### 5.5. Taunt-check — full O(n) скан на каждый Cast-кандидат ✓ fixed

Было: `is_valid_cast` на каждый (ability × target) сканировал `snapshot.enemies_of(team)` в поисках `FORCES_TARGETING`. **Исправлено:** сканер поднят на уровень `enumerate_next_steps` (один проход в начале функции → `taunter: Option<Entity>`). `is_valid_cast` принимает параметр и делает O(1) сравнение `target != taunter`. Квадратика превратилась в линейную.

### 5.6. `overkill_damage_multiplier` обнуляет kill вместе с уменьшением damage ✓ fixed

Было: `off.damage *= mult; off.kill = 0.0;` — kill абсолютный ноль, damage через мультипликатор. На hard (`mult≈0.15`) план «добиваем уже-мёртвого» сохранял 15 % damage-signal, в damage-dominant батчах оставался конкурентным.

**Исправлено:** мультипликатор применяется к **обоим** сигналам — `off.damage *= mult; off.kill *= mult;`. Одна difficulty-ручка, один consistent эффект: easy AI всё ещё иногда оверкилит (mult≈0.72 сохраняет signal), hard AI почти никогда (floor 0.15). Метод переименован `overkill_damage_multiplier` → `overkill_multiplier` (имя соответствует scope'у). Regression-тест `overkill_scales_damage_and_kill_uniformly`.

### 5.7. `apply_reservation_adjustments` — `position *= 0.5` на SIGNED факторе ✓ fixed

Было: `if reservations.is_tile_reserved(tile) { *position *= 0.5; }`. `position` — signed factor (`SIGNED_FACTOR[4] = true`), так что `*= 0.5` правильно штрафовал только при `position > 0`. При `position < 0` амплитуда уменьшалась — tile с плохой оценкой, зарезервированный союзником, выглядел **лучше**, чем без резервации.

**Исправлено:** subtractive penalty `*position -= RESERVED_TILE_PENALTY` (0.5, совпадает по величине со старым мультипликативом при `position ≈ 1.0`). Корректно толкает вниз при любом знаке. Regression-тест пришит: `reserved_tile_penalises_both_signs` в `factors/adjustments.rs`.

### 5.8. `infer_profile` Tank-floor всегда ≥ 0.3

`role.rs:190–191`: `p.tank += (eff_hp / 20.0).clamp(0.3, 2.0)` — **всегда** добавляется минимум 0.3 независимо от tank-абилок. 12 HP glass-cannon голый `eff_hp/20 = 0.6 → tank += 0.6`. Это искажает профиль: любой юнит обычных 15–20 HP уже получит ~1.0 tank-веса, которого нет в его kit-диагностике. Проявляется в тестах (`infer_molnienosets_is_melee_assassin`) — mix[0] «<0.25 tank for glass cannon» держится, но только благодаря bias^1.5.

### 5.9. `TurnPlan.sim_snapshots` инвариант только под `debug_assert_eq!` ✓ fixed

Было: scorer читал `plan.sim_snapshots[idx − 1]` с hard-assumption `len == steps.len()`. `#[serde(skip)]` ронял вектор при round-trip'е → любой caller на десериализованном плане (replay-tool, будущий editor) получил бы OOB panic в release.

**Исправлено:** shape-invariant расширен на «generator-filled **или** empty», закодирован в doc'е `TurnPlan::sim_snapshots`. Добавлен `TurnPlan::pre_step_snapshot(idx, initial)` — safe accessor, fall-back'ит на `initial` и при `idx == 0`, и при пустом векторе. Оба scorer loop'а (sans_intent + intent_sum) переведены на него; `debug_assert_eq!` смягчён до `is_empty() || len == steps.len()`. Factors на десериализованном плане чуть устаревают (все шаги видят initial snapshot), но **не крашатся** — guarantee «safe, not accurate». Regression-тест `scorer_tolerates_empty_sim_snapshots_from_deserialized_plan`.

---

## Приоритет фиксов

| Находка | Влияние | Риск фикса |
|---|---|---|
| 1.4 Scoring прогонялся до 3× | CPU, худший случай | средний | ✓ fixed |
| 2.1 AoE hits filtering × 7 + self double-count | friendly-fire drift | средний | ✓ fixed |
| 4.5 AoO формула дубль в movement + sanity | drift формулы | низкий | ✓ fixed |
| 5.7 `position *= 0.5` на signed факторе | корректность скоринга | низкий | ✓ fixed |
| 5.6 overkill mult теперь симметричен по damage/kill | аггрессивность hard AI | низкий | ✓ fixed |
| 3.5 CritFail + mana_overload + primary=None → одна enum | type safety | средний | ✓ fixed |
| 3.6 `plan_summon_bonus` post-norm hack | расширяемость | средний |
| 5.8 Tank-floor ≥ 0.3 в `infer_profile` | role mis-inference | низкий |
| 5.9 sim_snapshots инвариант только debug_assert | release-crash на deser | низкий | ✓ fixed |
| 2.2 Status passes × 3 | perf + DRY | низкий |
| 2.3 Test-helper ctx × 3 | test DRY | тривиальный |
| 2.4 `worst_path_danger` дубль | DRY | низкий |
| 3.1 `PickMechanics` через production-путь | layering | низкий |
| 3.2 `DiceSource::roll_crit_fail` deadweight | API hygiene | низкий |
| 3.3 `empty_blocked_tiles` OnceLock | test-only хак | тривиальный | ✓ fixed |
| 3.4 `MoveCloser` vs `MoveOnlyRetreat` | enum hygiene | тривиальный |
| 4.1 `reservations` global mut | future concurrency | средний |
| 4.2 `memory` copy-out/in | perf | тривиальный | ✓ fixed |
| 4.3 Hard thresholds в `select_intent` | balance drift | низкий |
| 4.4 `default_focus_target` через plans | layering | низкий |
| 4.6 `raw_factors[_][7]` magic index | brittleness | тривиальный | ✓ fixed |
| 5.1 `rng.roll_d.saturating_sub(1)` | fragility | тривиальный | ✓ fixed |
| 5.2 `plan_is_defensive` пустой plan = defensive | corner-case | низкий |
| 5.3 `score_action` Heal via threat | scoring semantics | средний |
| 5.4 `focus_sum` empty-plan spec-case | scoring уродство | низкий |
| 5.5 Taunt-check O(n) per Cast | perf | тривиальный | ✓ fixed |
| 1.1 sanity штрафы + bonus | разделение зон | низкий |
| 1.2 `run_ai_turn` god-function | читабельность | средний |
| 1.3 Drift sim ↔ real (rage, speed) | корректность sim | средний |
