# Архитектурный отчёт по `combat/ai/`

Глубокий проход по AI-подсистеме (~10.3 K строк, 30 файлов). Дата: 2026-04-19.

Не дублирует находки из `docs/known_issues.md` (учитываются как фон). Фокус — новое.

---

## Что уже работает хорошо

- Чёткое разделение фаз в `pick_action` (intent → generate → score → sanity → mask → pick → commit → reserve).
- Shared core `combat/effects_*` для ability-resolution — sim и real исходят из одной правды.
- `AxisProfile` 5-осевая модель с biased-mixing — расширяемее enum-ролей.
- `CommittedPrefix` enum как single-source-of-truth для bundling rules — образцовая абстракция.
- `aoe_hits` + `self_hit` отделение от `allies` — закрыло баг double-count.
- `factors/` модули с разделением offensive/scarcity/adjustments — узкие зоны ответственности.

---

## Структурные проблемы — главные

### П1. **Гигантский «implicit context» через signature bloat**

Почти каждая scoring/planning функция тащит одно и то же: `(active, ctx, snap, maps, reservations)` + step/plan. Признаки: 7 файлов с `#![allow(clippy::too_many_arguments)]`. Примеры:
- `compute_plan_factors(plan, active, intent, ctx, snap, maps, reservations)` — 7 args
- `compute_offensive(ability, target_pos, target, caster_tile, active, ctx, snap)` — 7
- `record_committed_reservations(plan, consumed, active, ctx, snap, reservations, actor_pos)` — 7
- `pick_action` — 12 args, и `run_ai_turn` поверх — 14.

Всё это — стабильные read-only данные тика. **Единый `ScoringContext`**:
```rust
pub struct ScoringContext<'a> {
    pub utility: &'a UtilityContext<'a>,        // world+actor (уже есть)
    pub snap: &'a BattleSnapshot,
    pub maps: &'a InfluenceMaps,
    pub reservations: &'a Reservations,
    pub active: &'a UnitSnapshot,
}
```
Снизит 6→2 в 10+ сигнатурах, уберёт большую часть `too_many_arguments`-allow'ов. Совместимо с текущими структурами (composition, не slicing).

### П2. **Три параллельные репрезентации одного шага**

`PlanStep` (owned, persistent) → `ScoredStep<'a>` (ref+caster_tile, для скоринга) → `CommittedPrefix<'a>` (bundling-aware view). Каждая enum имеет дублирующиеся match arms. Конверсии `from_plan_step` / `from_plan_committed` — boilerplate. **И** есть **четвёртое** место — `StepKey` enum в `generator::dedup_by_logical_key`, отдельный hash-friendly form.

Минимально-инвазивный фикс — добавить **одно** API: `plan.walk_with_caster(actor_pos)` — итератор `(idx, &PlanStep, caster_tile_at)`. Сейчас этот walk-the-plan-and-track-caster делается **в пяти местах**:
- `scorer::compute_plan_factors_sans_intent` (через cloned sim_actor)
- `scorer::compute_plan_intent_sum` (то же)
- `sanity::plan_has_self_aoe` (свой track)
- `picker::record_committed_reservations` (свой track)
- `generator::logical_key` (свой track)

Один итератор — четыре повтора удалятся, новый walker гарантирует ту же семантику.

Дальняя цель: схлопнуть `ScoredStep` и `CommittedPrefix` через единый `Step` view; но это уже семантическая работа, оставить.

### П3. **`compute_plan_factors_sans_intent` и `compute_plan_intent_sum` — двойной обход с одинаковым setup**

Обе функции ~50 строк, обе:
- iterate `plan.steps.iter().enumerate()`
- call `pre_step_snapshot(idx, snap)`
- `pre_snap.unit(active.entity).cloned()` — **тяжёлый clone UnitSnapshot** (Vec<AbilityId>, Vec<ActiveStatusView>) в каждой итерации **обеих** функций
- умножают `step_weight *= base_discount`

Единственная причина расщепления — `rescore_with_intent` хочет обновить только intent column. Это валидно. Решение:

```rust
// shared walker; consumers handle their own accumulators
fn for_each_plan_step<F>(plan: &TurnPlan, snap: &BattleSnapshot, active: Entity, mut f: F)
where F: FnMut(usize, &PlanStep, &UnitSnapshot, &BattleSnapshot, f32 /* step_weight */)
```

`compute_plan_factors_sans_intent` и `compute_plan_intent_sum` становятся тонкими wrappers над одним walker. Per-plan clone снимается до одного раза (внутри walker — даже его можно избежать, передавая `&UnitSnapshot` напрямую, если scoring не мутирует sim_actor — он не мутирует). Это **уберёт ~50% копирований UnitSnapshot в горячем пути scoring'а**.

### П4. **`raw_factors[plan_idx][7]` — массив с magic indices в 10 местах**

Мы вылечили reader-сторону через `KILL_IDX, CC_IDX, INTENT_IDX...` но всё ещё есть:
```rust
raw[KILL_IDX] + (raw[CC_IDX] * 0.1).min(0.5)        // picker.rs
f[INTENT_IDX] = compute_plan_intent_sum(...)         // scorer.rs
weights[INTENT_IDX] *= ctx.world.difficulty.intent_commitment;  // scorer.rs
```

Очевидное улучшение — `struct PlanFactors { damage, kill, cc, heal, position, risk, focus, intent, scarcity }` + `as_array()` для нормализации. Compute-сайты пишут `f.intent = ...`, finalize/normalize работает с массивом. Type-safety + 0 runtime cost (Rust struct repr ≡ массив f32 при правильном layout). Расширение на 10-й фактор — добавил поле, компилятор показывает все callsites.

### П5. **`raw_factors` как `Vec<[f32; 9]>` это column-major хочет, row-major хранение**

Финализация делает per-column min/max нормализацию: `for factors in raw { for i ... mins[i] = min(mins[i], v); }`. Cache-неоптимально для кучи планов (>50). Если переписать на `struct FactorMatrix { columns: [Vec<f32>; 9] }` — линейный pass per column, SIMD-friendly. **Но**: per-plan compute пишет per-plan строку. Конфликт.

Прагматично: оставить row-major, **подсчитать min/max за один общий pass поверх uppertall enum** (уже так). Но избавиться от per-iteration `mins[i].abs().max(maxes[i].abs())` через `denom: [f32; 9]` precomputed once — уже сделано. Так что эта точка скорее «не-issue». Упоминаю чтобы вычеркнуть.

### П6. **`BattleSnapshot::unit(entity)` — линейный поиск, дёргается десятки раз**

`snap.unit(...)` — O(N). `enumerate_next_steps`, `pick_targets`, `default_focus_target`, `intent_score`, `target_priority`, `compute_aoe_damage`, `mercy_cruelty`, `record_committed_reservations` — все вызывают это многократно per plan/step.

Один тик AI с N=15 юнитов и ~150 plans × ~3 steps × ~3-5 unit-lookups = 6000+ linear scans. На 60Гц + 3-5 AI юнитов — ощутимо.

```rust
pub struct BattleSnapshot {
    pub units: Vec<UnitSnapshot>,
    by_entity: HashMap<Entity, usize>,  // index, не &
    round: u32,
}
```

Invalidation: только `sim::SimState::apply_cast` `retain`'ит killed units → нужно перестраивать map после kill (1×). Уже мы клоним snapshot per plan — стоимость пересборки сравнима с одним lookup'ом × N. Win 10-100×.

### П7. **`plan_summon_bonus` — O(plans × summons × content_lookup) c пересборкой `CasterContext`**

`scorer.rs:199` — для каждого plan, для каждого Summon-step:
- lookup ability def
- lookup unit_template
- lookup weapon
- собирает `CasterContext { str_mod, int_mod, ... }`
- `estimate_st_damage(...)` — линейный по abilities template'а

В большинстве боёв summons редки, но при их наличии этот лишний pass per план повторяется на сотнях планов. Trivial fix: **precompute `summon_dpr_by_template: HashMap<TemplateId, f32>` один раз в `pick_action`**, тогда `plan_summon_bonus` сводится к чтению map'a × decay.

Это бы заодно сняло известный 3.6 — сам additive hack останется, но станет дешёвым и тестируемым.

### П8. **RNG + порядок планов = недетерминизм по batch shape**

`finalize_scores`:
```rust
.map(|(factors, plan)| {
    ...
    if noise_amp > 0.0 {
        let noise = (rng.roll_d(1000) as f32 / 500.0 - 1.0) * noise_amp;
        score += noise;
    }
    score
})
```

N-й plan получает N-й roll. Если порядок планов меняется (а `dedup_by_logical_key` уже намекает: HashMap iteration unstable, лечится sort by index), seeded run воспроизводимости больше нет. Воспроизводимость сейчас спасает только то, что dedup сортирует.

Решение — детерминистический per-plan seed:
```rust
let plan_seed = hash((round, actor_entity, plan.canonical_key()));
let noise = pseudo_rand(plan_seed) * noise_amp;
```
Order-invariant. Воспроизводимо при любой перестановке plans.

Дополнительно: noise сейчас в score-units, после нормализации. Шум фиксированной амплитуды на batch с маленьким score-spread звучит «громче». Лучше масштабировать — `noise * (max_score - min_score)` — по batch.

### П9. **`run_ai_turn` (133 строки, 14 параметров) и `pick_action` (200 строк, 12 параметров) — god-functions**

Известно (1.2). Но конкретный план: **расщепить `pick_action` на phase-functions с типизированными input/output**.

```rust
struct ScoringPhase<'a> {
    ctx: ScoringContext<'a>,
    intent: TacticalIntent,
    intent_reason: String,
    plans: Vec<TurnPlan>,
    scored: Vec<f32>,
    raw_factors: Vec<PlanFactors>,
}

impl ScoringPhase<'_> {
    fn run_initial_scoring(&mut self, rng: &mut DiceRng);
    fn apply_viability_fallback(&mut self, rng: &mut DiceRng) -> bool; // returns "did fall"
    fn apply_sanity(&mut self);
    fn apply_protect_self_mask(&mut self, rng: &mut DiceRng);
    fn pick(&self, rng: &mut DiceRng, debug: bool) -> Pick;
}
```

Поток в `pick_action` становится 30 строк линейного кода вместо 200. Каждая фаза тестируется отдельно. Расширение (новая фаза «aspirational rescore») — один новый метод.

Бонус: `pick_action` сейчас делает **JSONL log inline** — 50 строк из 200 — это форматирование лога. Должно вернуть `AiTickResult { decision, log_payload, debug_payload }` и **caller** (`enemy_turn`) пишет лог. Снимет 50 строк из горячего пути.

### П10. **Hardcoded магические числа размазаны по 8 файлам**

Известно частично (4.3). Список (новые места поверх известных):
- `intent.rs`: `0.4` (ProtectSelf hp), `0.9` (overheal), `<= 2` (cluster threshold AoE), `+0.8 + threat*0.1` (CC score), `+1.2 + (1−hp%)*0.3` (kill score), `MISALIGN_PENALTY = -0.5`, `MILD_PENALTY = -0.3`, `STICKINESS_BONUS = 0.25`, `MAX_COMMITTED_TURNS = 3`.
- `target_priority.rs`: веса `0.20/0.20/0.20/0.15/0.10/0.15` (фиксированы; сравните: `AXIS_FACTOR_WEIGHTS` — табличные и role-aware).
- `scarcity.rs`: `0.8/0.35/0.2/0.5/-0.3/-0.15` — magic.
- `sanity.rs`: `SURVIVAL_FLOOR/LOW_HP_FACTOR/AOO_PENALTY_K/AOO_RISK_FLOOR`.
- `role.rs`: `eff_hp/20`, `clamp(0.3, 2.0)` (+ 5.8 issue).
- `influence.rs`: `0.92/0.80` coverage bases.
- `generator.rs`: `TARGETS_BY_THREAT=3/KILLABILITY=2/MOVE_TILES_*=2,2,1`, `partial_score` веса `0.1/1.0/0.5/0.5`.

`InfluenceConfig` уже паттерн (Bevy `Resource`, hot-reload friendly). Создать `BalanceConfig` (или `IntentConfig` / `ScoringConstants`) и переселить туда. Дешёвая работа, **высокая отдача для дизайна баланса** (можно крутить в TOML без recompile).

### П11. **Cycle: `intent.rs ↔ scorer.rs ↔ factors`**

`intent.rs` зовёт `factors::aoe_area`, `factors::aoe_hits`, `target_priority`, `position_eval`, `scoring::applies_cc`. `scorer.rs` зовёт `intent::intent_score`, `target_priority`, `position_eval`, `factors::compute_factors`. `factors/offensive.rs` зовёт `scoring::score_action`. `scoring.rs::status_score` дублирует кусок `factors/offensive.rs::status_cc_value` (оба считают «threat × duration» от skips_turn).

Не циклы в смысле "won't compile", но логические концентрические зависимости. Отделить:
- `scoring.rs` (HP-equivalent low-level) — низ
- `factors/` (per-step utility factors) — middle
- `target_priority.rs`, `position_eval.rs` — middle (utility helpers)
- `intent.rs` — выше: **должен зависеть от `factors::*` но не наоборот**. Сейчас `factors` не зовёт intent (хорошо), но `scorer` зовёт оба, что OK.

`scoring.rs::status_score` и `factors/offensive.rs::status_cc_value` — формальный дубль. Завести `pub fn status_value_threat(sd, target_threat, duration)` в одном модуле (например `factors::status_value`), оба сайта читают.

### П12. **`Threat` нагружен двойной семантикой**

`UnitSnapshot.threat` = `estimate_st_damage` = `max single-target expected damage`. Используется как:
- (а) **угроза для меня** в `build_danger` (`danger += enemy.threat`)
- (б) **ценность убить** в `target_priority`, `intent::cc_score`, `scarcity` swing
- (в) **deny-value** в `score_action` heal (`delta_pct × target.threat`)
- (г) **stun value** в `status_score` / `status_cc_value` (`threat × duration`)

(а) хочет «средний DPR» (round-rate), (б) хочет priority composite (с density, role, etc), (в) хочет «expected DPR target's still owes us», (г) хочет «damage prevented by skipping turn» — близко к (а).

Сейчас одна цифра принимает все четыре роли. Предложение:
- `expected_dpr: f32` — что юнит реально выдаёт за раунд (учитывает AP, free-attack ratio).
- `target_priority` остаётся composite (как уже).
- `threat`-как-«max ST» можно убрать или переименовать в `peak_attack` чтобы не путать с DPR.

Семантическая работа, риск регрессии. Не сейчас, но фиксировать в roadmap.

### П13. **Dual-path в `pick_inner` не оправдан**

Я недавно ввёл split на `pick_best_plan` и `pick_best_plan_with_mechanics` ради экономии аллокации `pool: Vec<(usize, f32)>`. Но `pool.len()` ≤ `top_k` (1-3), т.е. **3 элемента max** — `Vec::with_capacity(3)` это микроэкономия. Streaming vs pool-collect разница ~10ns.

Простой вариант — единый `pick_best_plan(...) -> (idx, PickMechanics)`, всегда возвращает mechanics, prod-путь её игнорит. Снимет dual-path сложность, добавит ~24 байта на стек.

### П14. **Tests: каждый файл строит свой `unit()` / `snap()` / `maps_*()` factory**

Не считая 4-х копий test_ctx (уже починили), есть:
- `unit(id, team, pos, ...)` — `scorer.rs`, `picker.rs`, `sanity.rs`, `aoe_hits.rs`, `target_priority.rs`, `intent.rs`, `factors/scarcity.rs`, `generator.rs`, `snapshot.rs`, `factors/adjustments.rs`. **10 копий.**
- `empty_maps()` — `scorer.rs`, `generator.rs`, `intent.rs`.
- `empty_content()` — `generator.rs`, `sanity.rs`.
- `BattleSnapshot { units, round: 1 }` factory — везде inline.

Расширить `test_helpers.rs` до билдеров: `UnitSnapshotBuilder::new(id).team(...).hp(...).build()`, `SnapshotBuilder::new().with(unit).build()`, `MapsBuilder::new().danger(hex, val).build()`. Снимет ~150-200 строк boilerplate в tests. Идёт в комплекте с П1 (scoring context bundle).

---

## Конкретный план рефакторинга — три волны

### Волна 1 — низкорисковая компактификация (≈ 1-2 дня)

| # | Задача | Risk | Impact |
|---|---|---|---|
| 1.1 | `ScoringContext` bundle (П1) | низкий | -200 строк сигнатур, убирает `too_many_arguments` |
| 1.2 | `plan.walk_with_caster()` итератор + миграция 5 callsites (П2) | низкий | -50 строк boilerplate, single source of truth |
| 1.3 | `BattleSnapshot::by_entity` HashMap (П6) | низкий | 10-100× ускорение `unit()` lookup |
| 1.4 | `PlanFactors` struct + `as_array()` (П4) | низкий | type safety, 0 runtime cost |
| 1.5 | Pre-compute `summon_dpr_by_template` (П7) | низкий | O(plans×summons) → O(summons + plans) |
| 1.6 | UnitSnapshot/Snapshot builders в `test_helpers` (П14) | trivial | -150 строк tests |
| 1.7 | Dedup `status_value_threat` helper (П11) | низкий | один источник для stun-value |

### Волна 2 — структурная (≈ 3-4 дня)

| # | Задача | Risk | Impact |
|---|---|---|---|
| 2.1 | `BalanceConfig` resource — переселение всех magic constants (П10) | средний | Hot-reloadable, designer-friendly |
| 2.2 | Phase pipeline в `pick_action` (П9) | средний | -150 строк god-function, тестируется по фазам |
| 2.3 | Scorer dual-walker → single shared iterator (П3) | средний | -50% UnitSnapshot clones в hot path |
| 2.4 | Detrminist per-plan seed для noise (П8) | средний | Воспроизводимость + порядконезависимость |
| 2.5 | `pick_action → AiTickResult` (логирование вынести) (П9 cont) | низкий | -50 строк из горячего пути |
| 2.6 | Слить `pick_best_plan_with_mechanics` обратно (П13) | trivial | -1 функция |

### Волна 3 — семантические (отдельные эпики)

| # | Задача | Risk |
|---|---|---|
| 3.1 | Split `threat` → `expected_dpr` + `peak_attack` (П12) | высокий |
| 3.2 | Reservations snapshot+delta для async/parallel (4.1) | высокий |
| 3.3 | Snapshot incremental update между AI ticks одного раунда | высокий |
| 3.4 | Sim drift closure — rage gain, speed mid-plan | средний |

---

## TL;DR

Архитектура осмысленная, фазы чёткие. Болезни — внутри фаз: signature bloat, дублирующиеся представления одного шага, magic-числа без конфига, два почти-идентичных walker'а в scoring, и god-function на 200 строк. Никаких фундаментальных пересборок не нужно — Волна 1 (низкорисковая) даёт ~30% компактности и убирает большинство знакомых раздражителей. Волна 2 закрывает архитектурные основания на следующие фичи (BalanceConfig, phase pipeline, deterministic noise). Волна 3 — semantic, отдельный эпик.
