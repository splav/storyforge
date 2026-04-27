# Implementer Plan — Сабшаг 8.B (3 коммита)

**Source of truth:** `/Users/splav/personal/storyforge/docs/ai_rework_step8_plan.md` §"Сабшаг 8.B — `PlanModifier` + `PlanModifiersStage`". Этот план — implementer-friendly декомпозиция; не дублирует spec — отсылает к нему.

**Working tree:** `/Users/splav/personal/storyforge` (post-8.A.7 baseline, commit `c2af44b`). Schema v29 active, ai_scenarios зелёный 14/14.

**Что меняется по итогу 8.B (file inventory).**
- New: `src/combat/ai/modifiers/{mod,summon_bonus,trade_bonus,repair_bonus}.rs`, `src/combat/ai/pipeline/stages/plan_modifiers.rs`.
- Heavily edited: `src/combat/ai/planning/scorer.rs` (теряет `plan_summon_bonus`, `plan_trade_bonus`, `build_summon_dpr_cache` re-use, repair-affinity apply block), `src/combat/ai/pipeline/{mod,stages/mod}.rs`, `src/combat/ai/outcome/mod.rs` (новое поле `modifiers`).
- Untouched: factor registry (`factors/{registry,step,plan,terminal}/*`), schema (v29 stays), `plan_noise` + `plan_start_tile` (8.C scope).

---

## Open questions перед стартом

Перечитай spec §"Сабшаг 8.B" + ловушки ниже. Если ниже что-то не сходится с твоим чтением кода — задай вопрос **до** написания первой строки.

1. **Финал контракт `finalize_scores`.** Spec §"Удаляется" говорит вынести summon/trade/repair apply из неё. Текущая `finalize_scores` (`scorer.rs:163`) возвращает `Vec<f32>` и **не трогает `ann.score`**. Новый PlanModifiersStage пишет в `ann.score += contribution` (in-place). Двухсторонний контракт: **(a)** `score_plans_with_raw` (`scorer.rs:85`) перестаёт включать modifier contribution в возвращённый `Vec<f32>`; **(b)** caller в `utility/mod.rs:280` пишет это в `ann.score`, и далее PlanModifiersStage добавляет modifiers поверх. То же для `rescore_with_intent` / `rescore_with_per_plan_modes` (вызываются в `viability.rs:92` / `adaptation.rs:294,323,362`) — они тоже возвращают **pre-modifier** score, а modifiers применяются в один проход в конце pipeline. **Подтверди:** `finalize_scores` в 8.B продолжает возвращать `Vec<f32>` с factor_sum + terminal_sum; modifiers/repair-bonus apply живут только в PlanModifiersStage. ✅ Это согласовано со spec §"Pipeline order updated" (modifiers — **между** RepairAffinity и PickBest, после rescore стадий).

2. **Порядок суммирования FP-репродукции.** Legacy: `factor_sum (with summon+trade inline) → terminal_sum → repair_bonus`. Новый: `factor_sum → terminal_sum → modifiers[summon, trade, repair]`. Изменение: summon/trade сдвигаются с **до** terminal_sum на **после** terminal_sum. `f32` non-associative → возможен per-entry FP-edge сдвиг. Spec явно даёт ≤3/N tolerance per entry. **Принять spec budget**, не пытаться сохранять legacy summation order — порядок в `PLAN_MODIFIERS` static `[summon, trade, repair]` (документировано спекой §"PLAN_MODIFIERS" definition).

3. **`ModifierCtx` lifetime.** Spec даёт `pub struct ModifierCtx<'a> { stage: &'a StageCtx<'a>, summon_dpr: &'a HashMap<String, f32>, ... }`. `StageCtx` имеет два lifetime'а `<'w, 's>`. Подтверди унификацию через одну параметризацию `'a` достаточно (внутрь modify не пробрасываются `&'w`-only borrowed ссылки). Если компилятор upset — расширить до `ModifierCtx<'w, 's, 'a>` где `'a: 's: 'w`.

---

## Commit 1 — Modifier trait + 3 модуля + unit-тесты, без подключения к pipeline

**Цель.** Vocabulary lands. `PlanModifier` trait + 3 implementations скомпилены и unit-тестированы against legacy formulas. **`finalize_scores` не трогается; PlanModifiersStage не существует. ai_scenarios behavior идентичен.** Pure additive plumbing.

**Файлы (создать).**

- `src/combat/ai/modifiers/mod.rs`:
  - `pub trait PlanModifier: Sync { fn name(&self) -> &'static str; fn modify(&self, plan: &TurnPlan, ann: &PlanAnnotation, ctx: &ModifierCtx<'_>) -> f32; }`.
  - `pub struct ModifierCtx<'a> { pub stage: &'a StageCtx<'a, 'a>, pub summon_dpr: &'a HashMap<String, f32>, pub actor_value: f32, pub repair_weights: RepairWeights }`. **Note:** `last_goal` в spec figured как separate field, но `StageCtx::scoring::last_goal` уже даёт его — НЕ дублируй. Repair-bonus modifier читает `ctx.stage.scoring.last_goal`.
  - `#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)] pub struct ModifierContribution { pub name: String, pub contribution: f32 }`.
  - `pub static PLAN_MODIFIERS: &[&dyn PlanModifier] = &[&summon_bonus::MODIFIER, &trade_bonus::MODIFIER, &repair_bonus::MODIFIER];`. Порядок фиксирован.
  - `pub mod summon_bonus; pub mod trade_bonus; pub mod repair_bonus;`.

- `src/combat/ai/modifiers/summon_bonus.rs`:
  - `pub struct SummonBonus; pub static MODIFIER: SummonBonus = SummonBonus;`.
  - `impl PlanModifier for SummonBonus { fn name(&self) -> "summon_bonus"; fn modify(...) -> f32 { /* lifted body */ } }`.
  - **Source:** `scorer.rs:370–411` — full `plan_summon_bonus` body. Bind `active = ctx.stage.scoring.active`, `world = ctx.stage.scoring.world`, `snap = ctx.stage.scoring.snap`, `summon_dpr = ctx.summon_dpr`. Логика identical, byte-for-byte.

- `src/combat/ai/modifiers/trade_bonus.rs`:
  - `pub struct TradeBonus; pub static MODIFIER: TradeBonus = TradeBonus;`.
  - `impl PlanModifier for TradeBonus { ... fn modify(...) { let br = trade_delta(plan, ctx.stage.scoring.active, ctx.stage.scoring.snap, ctx.stage.scoring.world.content); trade_score(&br, ctx.actor_value) } }`.
  - **Source:** `scorer.rs:419–428`.

- `src/combat/ai/modifiers/repair_bonus.rs`:
  - `pub struct RepairBonus; pub static MODIFIER: RepairBonus = RepairBonus;`.
  - `fn modify(...) { if ctx.stage.scoring.last_goal.is_none() { return 0.0; } let bonus_scale = ctx.stage.scoring.world.tuning.thresholds.repair_bonus_scale; let affinity = ann.repair_affinity; let bonus = affinity.aggregate(&ctx.repair_weights).max(0.0); bonus * (1.0 + ctx.stage.scoring.need_signals.continue_commitment) * bonus_scale }`.
  - **Source:** `scorer.rs:282–294`. **Note:** `aggregate(&weights).max(0.0)` clamp есть в legacy (`:291`). Не упустить.

**Файлы (изменить).**

- `src/combat/ai/mod.rs` — добавить `pub mod modifiers;`.
- `src/combat/ai/planning/scorer.rs` — **ничего не трогаем в коммите 1**. `build_summon_dpr_cache` остаётся private — pub-видимость откроется в коммите 2.

**Подшаги (порядок исполнения).**

1. `mkdir src/combat/ai/modifiers`. Файлы создать.
2. Определить `PlanModifier` trait + `ModifierCtx<'a>` + `ModifierContribution` в `mod.rs`. Скомпилировать пустой каркас (3 модификатора с `modify(...) -> 0.0`). `cargo check`.
3. Заполнить `summon_bonus::modify` — copy-paste body из `scorer.rs:370–411`. **Сохрани локальный `count` mutable** (legacy инкрементит per-step). **Saturation_mult** считается **один раз** до loop'а (legacy line `:392`). Не трогать порядок операций.
4. Заполнить `trade_bonus::modify` — два-line wrapper.
5. Заполнить `repair_bonus::modify` — guard на `last_goal.is_none()` first. **Watch:** clamp `.max(0.0)` после aggregate (legacy line `:291`).
6. Написать unit-тесты per-modifier (см. чек-лист). Каждый тест строит fixture-plan + legacy formula выход → сравнивает.
7. `cargo test --package ... modifiers::`. `cargo clippy --all-targets`.
8. Проверка ai_scenarios — должна быть идентична post-8.A.7 baseline (никакая стадия modifiers ещё не подключена).

**Ловушки и риски.**

- **`StageCtx` lifetime mismatch.** `StageCtx<'w, 's>` имеет два lifetime'а; `ModifierCtx<'a>` — один. Если контейнер `&'a StageCtx` теряет один из lifetime-параметров, требуется `&'a StageCtx<'a, 'a>` или эквивалент. Скорее всего проще: `pub struct ModifierCtx<'w, 's, 'a> { pub stage: &'a StageCtx<'w, 's>, ... }`. Если попадёшь в lifetime hell — упрости до конкретного `Box<dyn>` или вынеси нужные поля плоско в `ModifierCtx` (active, snap, world, need_signals, last_goal). Spec §"ModifierCtx" sketch — guideline, не закон.
- **`trade_delta` доступность.** В `scorer.rs:426` он импортируется из `crate::combat::ai::trade::trade_delta`. Проверь pub-видимость; если нужен — open up `pub(crate)`.
- **`repair_weights` тип.** `active.role.repair_weights(world.tuning) -> RepairWeights` (struct из `repair::affinity`), не `[f32; 6]`. Spec §"ModifierCtx" неточно указывает `[f32; 6]` — игнорируй, используй real тип `RepairWeights`. Pin via `repair_bonus_uses_role_repair_weights` test.
- **`PLAN_MODIFIERS` static + `Sync`.** Trait bound `Sync` обязательный для `static &[&dyn PlanModifier]`. Все три impl должны быть `+ Sync` (zero-state structs — auto-impl).
- **No production wiring yet.** В коммите 1 `PLAN_MODIFIERS` создан, но **нигде не вызывается** в production коде. dead-code warning подавить через `#[allow(dead_code)]` на `mod.rs` — снять в коммите 2.
- **Tests должны сами строить `ModifierCtx`** — это ad-hoc. Пиши test helper `fn make_modifier_ctx<'a>(...)` в `modifiers/mod.rs::tests` (можно cfg(test)) или дублируй per-file.

**Gate-проверка (commit 1).**

- `cargo build --all-targets` зелёный.
- `cargo clippy --all-targets -- -D warnings` зелёный.
- New tests (см. чек-лист):
  - `summon_bonus_matches_legacy_formula`
  - `summon_bonus_zero_for_no_summon_plan`
  - `trade_bonus_matches_legacy_formula`
  - `trade_bonus_zero_for_neutral_plan` (мигрирует тело из `scorer.rs:2052`)
  - `repair_bonus_zero_when_no_stored_goal`
  - `repair_bonus_matches_legacy_formula` (pin против hand-computed `aggregate*scale*(1+continue_commitment)`)
- Existing `cargo test` зелёный.
- `cargo run --bin ai_scenarios` — **0 diff** vs post-8.A.7 baseline. Никакого modifier wiring.

---

## Commit 2 — `PlanModifiersStage` подключён в pipeline; `finalize_scores` теряет 3 apply-блока

**Цель.** Production pipeline runs new stage. Legacy inline `summon`+`trade`+`repair-affinity` apply удалены из `finalize_scores`. `PlanAnnotation.modifiers: Vec<ModifierContribution>` field added. **Behavior: ≤3/N FP-edge tolerance per entry vs post-8.A.7 baseline.**

**Файлы (создать).**

- `src/combat/ai/pipeline/stages/plan_modifiers.rs`:
  - `pub struct PlanModifiersStage;`.
  - `impl PlanStage for PlanModifiersStage { fn name() -> "plan_modifiers"; fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) { /* main loop */ } }`.
  - Body: per spec §"Новая стадия":
    1. `let summon_dpr = build_summon_dpr_cache(&pool.plans, ctx.scoring.world);` (теперь pub-crate в scorer).
    2. `let actor_value = unit_value(ctx.scoring.active, ctx.scoring.world.content);`.
    3. `let repair_weights = ctx.scoring.active.role.repair_weights(ctx.scoring.world.tuning);`.
    4. `let mctx = ModifierCtx { stage: ctx, summon_dpr: &summon_dpr, actor_value, repair_weights };`.
    5. `for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) { if !ann.score.is_finite() { continue; } for m in PLAN_MODIFIERS { let c = m.modify(plan, ann, &mctx); ann.modifiers.push(ModifierContribution { name: m.name().into(), contribution: c }); ann.score += c; } }`.

**Файлы (изменить).**

- `src/combat/ai/pipeline/mod.rs:125–142` (`run_pool_pipeline`): добавить `PlanModifiersStage` между `RepairAffinityStage` и `PickBestStage`. Добавить `use stages::plan_modifiers::PlanModifiersStage;` в use-block. Обновить doc-comment (`:117–124`) — pipeline order pinned: `… RepairAffinity → PlanModifiers → PickBest`.
- `src/combat/ai/pipeline/stages/mod.rs` — добавить `pub mod plan_modifiers;`.
- `src/combat/ai/outcome/mod.rs:148`: добавить поле в `PlanAnnotation`:
  ```rust
  /// Step 8.B: per-modifier additive contributions applied in PlanModifiersStage.
  /// Empty until that stage runs; populated in canonical PLAN_MODIFIERS order
  /// [summon_bonus, trade_bonus, repair_bonus]. Sum equals the score delta
  /// produced by the stage. Pure observability — does not influence picking.
  #[serde(default)]
  pub modifiers: Vec<ModifierContribution>,
  ```
  Импорт: `use crate::combat::ai::modifiers::ModifierContribution;`. Schema v29 stays — `#[serde(default)]` обеспечивает forward-compat для v29 logs without modifier field.
- `src/combat/ai/planning/scorer.rs`:
  - **Удалить** `plan_summon_bonus` (`:370–411`).
  - **Удалить** `plan_trade_bonus` (`:419–428`).
  - **Сделать `pub(crate)`** функцию `build_summon_dpr_cache` (`:434–465`) — она нужна `PlanModifiersStage`. **Рекомендация:** оставить в scorer.rs `pub(crate)` (минимальный диф, доступ к `CasterContext`/`Abilities`/`estimate_st_damage` — те же impls, что и сейчас). Spec §"Удаляется" говорит "переезжает" но не настаивает на физическом перемещении — `pub(crate)` re-use достаточно.
  - **Удалить из `finalize_scores`** строки `:177` (`let summon_dpr = build_summon_dpr_cache(...)`), `:180` (`let actor_value = unit_value(...)`), `:235` (`score += plan_summon_bonus(...)`), `:241` (`score += plan_trade_bonus(...)`), `:282–295` (repair-affinity apply block, целиком).
  - Удалить imports которые больше не нужны: `unit_value`, `trade::trade_delta`, `trade::trade_score`. **Сохранить:** `CasterContext`, `Abilities`, `estimate_st_damage`, `EffectDef`, `modifier` — они нужны `build_summon_dpr_cache`, который остался в scorer.rs.
  - **Не трогать** `plan_noise` + `plan_start_tile` (`:330–358`) и noise apply block (`:298–321`) — это 8.C.

**Подшаги (порядок исполнения).**

1. Расширить `PlanAnnotation` полем `modifiers`. `cargo check` — все callsite зелёные (default добавляется bot).
2. Создать `pipeline/stages/plan_modifiers.rs`. Подключить mod в `stages/mod.rs`. `cargo check` ожидает `PLAN_MODIFIERS` достижимым.
3. Снять `#[allow(dead_code)]` с `modifiers/mod.rs` (если ставил в коммите 1).
4. В `finalize_scores` удалить три apply блока (`:235`, `:241`, `:282–295`) **и** вспомогательные строки `:177` (summon_dpr), `:180` (actor_value). Запустить `cargo test --package ... scorer`. Тесты `repair_bonus_*` (которые читают `finalize_scores`) **сломаются** — это ожидаемо. **НЕ чинить тесты scorer.rs здесь** — миграция тестов в коммите 3.
5. Подключить `PlanModifiersStage` в `run_pool_pipeline` между Repair и Pick. `cargo build`. ai_scenarios гонять — pipeline должен производить тот же winner per scenario, score numerically близкий (≤3/N FP-edge). Если winner поменялся — bug.
6. Удалить `plan_summon_bonus` и `plan_trade_bonus` (dead code now).
7. Сделать `build_summon_dpr_cache` `pub(crate)`. `cargo clippy`.
8. Smoke: `cargo run --bin ai_scenarios` → ожидаем 14/14 зелёных, max FP-edge per entry ≤3.

**Ловушки и риски.**

- **`finalize_scores` repair-bonus block guards `if !score.is_finite() { continue; }`** (`:287`) — этот guard перенесён в PlanModifiersStage main loop в виде `if !ann.score.is_finite() { continue; }`. Гарантирует что **все 3** модификатора пропускают masked plans (раньше summon/trade применялись внутри `map(...)` без guard'а — но `score == NEG_INFINITY` всё равно остаётся NEG_INFINITY после addition, так что на picking не влияет; новый guard сильнее, но безопасен).
- **PlanModifiersStage запускается ПОСЛЕ rescoring stages.** `ViabilityStage::apply` (`viability.rs:92`) и `AdaptationStage::apply` (`adaptation.rs:294,323,362`) вызывают `rescore_with_*` → `finalize_scores`. После 8.B `finalize_scores` возвращает **pre-modifier** score. Это OK потому что rescoring перетирает `ann.score` целиком новым вызовом → modifiers добавятся уже в `PlanModifiersStage` поверх **финального** rescored value. **Но:** если adaptation rescoring перетёр `ann.score`, **должен ли** `ann.modifiers` тоже сброситься? Иначе после rescoring у плана `ann.modifiers = []` (потому что мы ещё в pipeline до PlanModifiersStage), а после `PlanModifiersStage` snapshot будет правильным. **Контракт:** `ann.modifiers` populated **ровно один раз** — в `PlanModifiersStage`. Adaptation rescoring **до** PlanModifiers, поэтому никаких повторов. **Pin via `pipeline_runs_modifiers_after_repair_before_pick`.**
- **`ann.modifiers` accumulation invariance.** `Vec::push` не идемпотентен; если `PlanModifiersStage::apply` вызвался дважды на одном пуле — modifiers удвоятся. Spec не требует idempotence, но defensive: в начале `apply` сделать `ann.modifiers.clear()` — нет, это лишнее, потому что пайплайн запускается один раз на пуле. **Не добавляй clear.** Если test'ы fail на repeat — расследовать как bug.
- **Утечка `score` через `Vec<f32>` from `finalize_scores`.** Caller в `utility/mod.rs:280` пишет `ann.score = score` ИЗ возвращённого Vec'а. После 8.B этот score **меньше** чем legacy (no modifiers). Это OK — modifiers добавятся в PlanModifiersStage. **Но:** пишутся `pool.annotations[i].score`, не tracked отдельной vector — сохраняется invariant. Pin: `pool.annotations[i].score == finalize_scores_output[i] + sum(modifier_contribs[i])` после PlanModifiers stage.
- **`utility/mod.rs:281`** также пишет `ann.factors = raw` — modifiers не влияют, OK.
- **`#[serde(default)]` для `modifiers` поля.** Старые v29 logs (post-8.A) не имеют поля `modifiers`. После добавления поля Deserialize должен принять их. Pin: `actor_tick_v29_loaded_without_modifiers_field_yields_empty_vec`.
- **Tests scorer.rs::tests которые читают `finalize_scores` для repair_bonus / trade_bonus.** Тесты в `scorer.rs:2076,2272,2322,2371,2380,2449,2467,2484` после удаления apply блоков **изменят выход**. Их миграция — commit 3.
- **`use crate::combat::ai::modifiers::PLAN_MODIFIERS;`** в plan_modifiers.rs. Убедиться, что путь работает. Также `use crate::combat::ai::planning::scorer::build_summon_dpr_cache;`.

**Gate-проверка (commit 2).**

- `cargo build --all-targets` зелёный.
- `cargo clippy --all-targets -- -D warnings` зелёный.
- New tests:
  - `plan_modifiers_stage_skips_masked_plans` (`-inf` plan: `ann.modifiers` empty, `ann.score` остаётся `NEG_INFINITY`).
  - `plan_modifiers_stage_writes_contributions_per_modifier` (3 entries в порядке `[summon_bonus, trade_bonus, repair_bonus]`).
  - `plan_modifiers_stage_total_matches_sum_of_contributions` (`ann.score - pre_modifier_score == sum(contribs)` modulo FP).
  - `pipeline_runs_modifiers_after_repair_before_pick` — pipeline-level test, проверяет что после `run_pool_pipeline` chosen plan имеет non-empty `ann.modifiers` если applicable.
  - `actor_tick_v29_loaded_without_modifiers_field_yields_empty_vec` (v29 log БЕЗ modifiers field deserialises with empty Vec).
- Tests scorer.rs::tests (`repair_bonus_*`, `trade_bonus_*`) — **expected fail** в коммите 2 (миграция → commit 3). Это контролируемо.
- `cargo run --bin ai_scenarios` — **per-entry FP-edge ≤3/N** vs post-8.A.7 baseline. Winner should be identical для каждого scenario. Если winner shift — расследовать ordering или skipped guard.

---

## Commit 3 — Migrate scorer.rs::tests + final cleanup + dead-code sweep

**Цель.** Tests которые тестировали `finalize_scores` end-to-end теперь работают через `run_pool_pipeline` или прямой вызов `PlanModifiersStage`. Dead imports удалены. Doc-comments updated.

**Файлы (изменить).**

- `src/combat/ai/planning/scorer.rs::tests` (`:1180+`):
  - `trade_bonus_zero_for_neutral_plan` (`:2052`) — **удалить** (мигрирует в `modifiers/trade_bonus.rs::tests` в коммите 1, оригинал dead).
  - `plan_trade_bonus_*` тесты (`:1181+, :1228+, :1231+`) — пересмотреть: если они тестируют формулу — мигрируй в `modifiers/trade_bonus.rs::tests`; если тестируют end-to-end pipeline — переписать через `run_pool_pipeline`.
  - `repair_bonus_*` тесты (`:2354–2469+`) — мигрируй в `modifiers/repair_bonus.rs::tests` (формула pin) + leave один integration test в `pipeline/stages/plan_modifiers.rs::tests` (через `PlanModifiersStage::apply` directly).
  - Тесты которые остаются в scorer.rs::tests (`terminal_aggregator_*`, `factor_*`) **должны** продолжать работать — они не зависели от modifier apply блоков.
- `src/combat/ai/planning/scorer.rs:148–161` (doc-block `finalize_scores`) — обновить: убрать упоминание summon bonus / score noise / repair bonus. Теперь `finalize_scores` вычисляет factor_sum + terminal_sum + noise (noise остаётся до 8.C). Update wording.
- `src/combat/ai/planning/scorer.rs` imports (`:1–40`) — удалить unused: `unit_value`, `trade::trade_delta`, `trade::trade_score` если они больше не упоминаются в файле. **Сохранить:** `CasterContext`, `Abilities`, `estimate_st_damage`, `EffectDef`, `modifier` — они нужны `build_summon_dpr_cache`, который остался в scorer.rs.
- `src/combat/ai/pipeline/mod.rs:117–124` doc-comment — добавить упоминание `PlanModifiers` в pipeline order (если не сделано в commit 2).
- `src/combat/ai/modifiers/mod.rs` — обновить module-level doc-comment с pipeline integration note.

**Подшаги (порядок исполнения).**

1. `cargo test --package ... scorer 2>&1 | grep FAIL` — список failing тестов после commit 2.
2. Per failing test: решить **migrate** или **delete**:
   - Тесты которые pin **формулу** (один plan, expected value) — migrate в `modifiers/<name>::tests`, удалить из scorer.rs.
   - Тесты которые pin **end-to-end interaction** (multi-plan, picker-affecting) — переписать через `run_pool_pipeline` с `ScoredPool::new` setup. Пример: `repair_bonus_no_op_when_no_stored_goal` → создать pool с одним plan, `last_goal = None`, прогнать `run_pool_pipeline`, assert `ann.modifiers` empty + winner unchanged.
3. После миграции: `cargo test --all-targets` зелёный.
4. Dead imports cleanup: `cargo clippy -- -D warnings` будет жаловаться на unused imports в scorer.rs. Удалить.
5. Doc-comment refresh.
6. Final smoke: `cargo run --bin ai_scenarios` — winner per scenario identical, FP ≤3/N.

**Ловушки и риски.**

- **Тесты scorer.rs::tests могут массово ломаться** — не оставляй disabled. Каждый failing test либо мигрируется в новое место, либо явно удаляется с обоснованием. Don't `#[ignore]`.
- **`b_support` / `b_rat` (scorer.rs:1228, 1231)`** — это сравнительные тесты на `plan_trade_bonus`. После удаления функции переписать как direct calls на `modifiers::trade_bonus::MODIFIER.modify(...)`. Сохранить семантику теста.
- **Integration tests в `pipeline/mod.rs::tests` или scorer.rs** которые тестируют `score_plans_with_raw → finalize_scores → ann.score`. Их семантика изменилась: сейчас `finalize_scores` не включает modifiers. Если тест ассертит конкретное value, сравни с pre-modifier expectation. Если тест ассертит relative ordering — должен сохраниться (modifiers не меняют argmax в этих fixtures, надеемся). Pin осторожно; runs ai_scenarios — главный gate.
- **`dpr` cache regression risk.** `build_summon_dpr_cache` вызывается дважды если в pipeline есть rescore: один раз в `score_plans_with_raw → finalize_scores` (legacy путь? — нет, после commit 2 finalize_scores не вызывает его), один раз в `PlanModifiersStage`. Только один вызов. ✅
- **`utility/mod.rs:280` invariant.** `ann.score = score` пишет pre-modifier score. PlanModifiersStage потом дополняет. Не trip'нись на этом invariant в тестах.

**Gate-проверка (commit 3).**

- `cargo test --all-targets` зелёный (zero failing).
- `cargo clippy --all-targets -- -D warnings` зелёный.
- `cargo run --bin ai_scenarios` — 14/14 зелёных, **per-entry FP-edge ≤3/N**, winners identical to post-8.A.7 baseline.
- Dead-code grep: `rg -n "plan_summon_bonus|plan_trade_bonus" src/` → zero hits in `combat/ai/`.
- Modifier observability: открыть один свежий v29 log → `annotation.modifiers` populated с 3 entries в правильном порядке.

---

## Чек-лист тестов (полный 8.B)

| # | Test name | File |
|---|---|---|
| 1 | `summon_bonus_matches_legacy_formula` | `modifiers/summon_bonus.rs::tests` |
| 2 | `summon_bonus_zero_for_no_summon_plan` | `modifiers/summon_bonus.rs::tests` |
| 3 | `trade_bonus_matches_legacy_formula` | `modifiers/trade_bonus.rs::tests` |
| 4 | `trade_bonus_zero_for_neutral_plan` (мигрирует из `scorer.rs:2052`) | `modifiers/trade_bonus.rs::tests` |
| 5 | `repair_bonus_zero_when_no_stored_goal` | `modifiers/repair_bonus.rs::tests` |
| 6 | `repair_bonus_matches_legacy_formula` (pin против hand-computed) | `modifiers/repair_bonus.rs::tests` |
| 7 | `repair_bonus_uses_role_repair_weights` | `modifiers/repair_bonus.rs::tests` |
| 8 | `plan_modifiers_stage_skips_masked_plans` | `pipeline/stages/plan_modifiers.rs::tests` |
| 9 | `plan_modifiers_stage_writes_contributions_per_modifier` | `pipeline/stages/plan_modifiers.rs::tests` |
| 10 | `plan_modifiers_stage_total_matches_sum_of_contributions` | `pipeline/stages/plan_modifiers.rs::tests` |
| 11 | `pipeline_runs_modifiers_after_repair_before_pick` | `pipeline/mod.rs::tests` |
| 12 | `actor_tick_v29_loaded_without_modifiers_field_yields_empty_vec` | `log.rs::tests` |
| 13 | (миграция) integration repair-bonus test через `run_pool_pipeline` | `pipeline/stages/plan_modifiers.rs::tests` |

**Existing tests that MUST still pass unchanged** (regression guard):
- `repair_affinity_stage_no_op_when_no_stored_goal` / `repair_affinity_stage_populates_annotation` (`pipeline/stages/repair_affinity.rs:115,138`) — RepairAffinityStage не меняется.
- `terminal_aggregator_zero_when_all_axes_zero` (`scorer.rs:2125`) — `finalize_scores` terminal aggregation не меняется.
- All factor / step / plan / terminal tests от 8.A.
- `actor_tick_v29_round_trip` от 8.A — должен пройти и с пустым `modifiers: []` field, и с populated.

---

## Финальный gate 8.B

After commit 3 lands:

1. `cargo test --all-targets` — все зелёные, including 13 new tests above.
2. `cargo clippy --all-targets -- -D warnings` зелёный.
3. `cargo build --all-targets --release` зелёный.
4. `cargo run --bin ai_scenarios` 14/14 зелёных:
   - Per-entry numeric values: ≤3/N FP-edge differ от post-8.A.7 baseline. >3/N → расследовать summation order или missing guard.
   - Winners per scenario **identical** к post-8.A.7. Winner-shift = **stop & investigate**, это behavior diff — недопустимо для pure refactor.
5. Pipeline order pinned: `Viability → Sanity → Adaptation → ProtectSelfMask → KillableGate → RepairAffinity → PlanModifiers → PickBest`. Pin через `pipeline_runs_modifiers_after_repair_before_pick`.
6. Schema version stays **29** (no bump). v29 logs БЕЗ `modifiers` field читаются (Deserialize default = empty Vec); v29 logs С `modifiers` field читаются и round-trip'ят bit-for-bit.
7. Dead-code grep clean: `rg -n "plan_summon_bonus|plan_trade_bonus" src/combat/ai/` → 0 hits (имена живут только в `modifiers/<name>.rs::name()` вызовах).
8. Mining baseline reproduction: post-8.A.7 v29 corpus → re-run scenarios on post-8.B code → mine v29 corpus → metrics reproduce post-8.A.7 metrics modulo ≤3/N FP-edge per entry.

---

### Critical Files for Implementation

- `/Users/splav/personal/storyforge/src/combat/ai/modifiers/mod.rs` — trait + ModifierCtx + PLAN_MODIFIERS static (NEW).
- `/Users/splav/personal/storyforge/src/combat/ai/pipeline/stages/plan_modifiers.rs` — main stage с loop'ом (NEW).
- `/Users/splav/personal/storyforge/src/combat/ai/planning/scorer.rs` — финал-stage cleanup, source of truth для legacy formulas (lines 282–294, 370–411, 419–428, 434–465).
- `/Users/splav/personal/storyforge/src/combat/ai/pipeline/mod.rs` — pipeline order register.
- `/Users/splav/personal/storyforge/src/combat/ai/outcome/mod.rs` — `PlanAnnotation.modifiers: Vec<ModifierContribution>` field add.
