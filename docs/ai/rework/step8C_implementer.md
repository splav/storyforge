# Implementer Plan — Сабшаг 8.C (3 коммита)

**Source of truth:** `/Users/splav/personal/storyforge/docs/ai_rework_step8_plan.md` §"Сабшаг 8.C — Picking jitter + cleanup". Этот план — implementer-friendly декомпозиция; не дублирует spec — отсылает к нему.

**Working tree:** `/Users/splav/personal/storyforge` (post-8.B.3 baseline, commit `e6ebdb4`). Schema v29 active, ai_scenarios зелёный 14/14, lib tests 554. Vocabulary post-8.B: `modifiers/` модуль + `PlanModifiersStage` подключён, `PlanAnnotation.modifiers` populated, pipeline order: `Viability → Sanity → Adaptation → ProtectSelfMask → KillableGate → RepairAffinity → PlanModifiers → PickBest`.

**Что меняется по итогу 8.C (file inventory).**
- New: ничего (jitter живёт внутри существующего `pipeline/stages/pick_best.rs`).
- Heavily edited: `src/combat/ai/planning/scorer.rs` (теряет noise pass + 2 helper'а; `finalize_scores` уменьшается до ~80 строк), `src/combat/ai/pipeline/stages/pick_best.rs` (jitter pre-sort step + `plan_noise_internal` + locally-defined `plan_start_tile`), `src/combat/ai/outcome/mod.rs` (новое поле `PickInfo.noise_applied: f32`), `src/bin/mine_ai_logs.rs` (две новые секции).
- Untouched: `factors/*` (registry stays), `modifiers/*` (post-8.B), schema (v29 stays — `noise_applied` через `#[serde(default)]`), `pick_best_plan` argmax/mercy/tie-break logic, `bin/replay_ai_log.rs` (label v29 уже актуален с 8.A.3 — только sanity check).

**Spec line-references vs реальное состояние post-8.B.** Spec preamble написан до 8.B и указывает старые адреса. Актуальные:
- Noise apply block: `scorer.rs:252–276` (spec говорит `:298–321`).
- `plan_noise` helper: `scorer.rs:285–298` (spec `:330+`).
- `plan_start_tile` helper: `scorer.rs:308–313` (spec `:358+`).
- `noise_amp` site: `scorer.rs:205` (использует `world.difficulty.score_noise()`).
- Spec также указывает финальный размер `finalize_scores` как ~80 строк "из ~250" — реальный pre-8.C размер ~117 строк (`:163–279`); цель ~80 после удаления Pass 2 noise + 2 helpers (≈40 строк).

---

## Open questions перед стартом

Перечитай spec §"Сабшаг 8.C — Picking jitter + cleanup" + ловушки ниже. Если что-то не сходится с твоим чтением — задай вопрос **до** написания первой строки.

1. **Когда именно писать `noise_applied` в `PickInfo`.** `PickInfo` сейчас (`pick_best.rs:30`) создаётся **только для победителя** через `pool.annotations[best_idx].pick = Some(PickInfo { mechanics: mech })`. Plans вне winner-slot НЕ имеют `PickInfo`. Spec §"Изменения" pseudo-code предлагает: "если `pi.is_none()` — создаётся в argmax pass с noise_applied записанным". В реальности `pick_best_plan` (`picker.rs:108`) внутрь `PickInfo` не пишет — он возвращает `(best_idx, PickMechanics)`. Решение: **noise_applied фиксируется только для победителя**. Накопить per-plan noise в локальном `Vec<f32>` (parallel to `pool.annotations`) внутри `apply_pick_jitter`, затем в `PickBestStage::apply` после argmax записать `noise_per_plan[best_idx]` в новый `PickInfo`. Plans вне winner — observability noise лежит в `ann.score - sum(modifiers) - factor_terminal_sum` (косвенно), но в JSONL не выводится. **Подтверди этот контракт перед стартом.** Если решено наоборот (PickInfo для всех finite plan'ов) — это расширение скоупа за пределы spec, делай в follow-up.

2. **Финальный контракт `ann.score`.** Post-8.C invariant:
   `ann.score = factor_sum + terminal_sum + sum(ann.modifiers) + (noise_applied if winner-or-finite else 0)`.
   Конкретно: `finalize_scores` пишет `factor_sum + terminal_sum`. `PlanModifiersStage` добавляет modifiers in-place (`ann.score += contribution`). `PickBestStage::apply_pick_jitter` добавляет noise per-plan in-place. `pick_best_plan` видит уже-noisy scores и делает argmax/mercy на их основе. Spec §"Pipeline order updated" явно подтверждает: jitter — pre-sort step, mercy/argmax видит modified score. ✅

3. **`noise_applied` semantics: "raw noise" или "noise actually applied"?** Spec неявно подразумевает: `n = plan_noise_internal(...)` записывается в `noise_applied`. Если `noise_amp <= 0.0` — early return, `noise_applied` остаётся `0.0` (default). Если spread floor сработал (`s_min` или `s_max` не finite) — текущий legacy code (`scorer.rs:264–268`) ставит `spread = 0.05` и **продолжает** apply. Spec §"Изменения" pseudo-code говорит `if !s_min.is_finite() || !s_max.is_finite() { return; }` — это **изменение** legacy semantics. **Принять spec semantics** (early return, не fallback) — возможен per-entry FP-сдвиг на edge cases где все scores -inf, но spec явно описывает этот контракт. Pin via `pick_jitter_no_op_when_all_scores_masked` test.

---

## Commit 1 — `apply_pick_jitter` в `PickBestStage` + `PickInfo.noise_applied`, без удаления legacy

**Цель.** Vocabulary lands. `apply_pick_jitter` функция + `plan_noise_internal` + локальная копия `plan_start_tile` живут в `pick_best.rs`. `PickInfo.noise_applied` поле есть. **`finalize_scores` Pass 2 noise остаётся нетронутым; в `PickBestStage::apply` jitter ВЫКЛЮЧЕН (закомментирован или под `if false`).** Compile + tests за исключением behavior-tests новой стадии.

Цель этого коммита — изолировать механику переноса от behavior-changes. После коммита 1 ai_scenarios должен быть **bit-identical** к post-8.B.3 baseline. Behavior switch — в коммите 2.

**Файлы (изменить).**

- `src/combat/ai/pipeline/stages/pick_best.rs`:
  - Добавить `fn apply_pick_jitter(pool: &mut ScoredPool, ctx: &StageCtx) -> Vec<f32>`. Возвращает Vec длиной `pool.len()` с накопленным noise per-plan (0.0 для skipped). Логика — копия Pass 2 из `scorer.rs:252–276` с заменой `scores` → `pool.annotations[i].score` и `plans` → `pool.plans`.
  - Добавить `fn plan_noise_internal(plan: &TurnPlan, round: u32, actor: Entity, amp: f32) -> f32` — byte-for-byte копия `scorer.rs:285–298`.
  - Добавить локальную `fn plan_start_tile(plan: &TurnPlan) -> Hex` — byte-for-byte копия `scorer.rs:308–313`.
  - Импорты: `crate::combat::ai::planning::types::{TurnPlan}`, `crate::game::hex::Hex`, `bevy::prelude::Entity`, `std::hash::{Hash, Hasher}`.
  - В `PickBestStage::apply` пока **не вызывать** `apply_pick_jitter` (под `#[allow(dead_code)]` либо `if false { let _noise = apply_pick_jitter(pool, ctx); }`). Это включится в коммите 2.

- `src/combat/ai/outcome/mod.rs`:
  - В `PickInfo` (`:236–240`) добавить:
    ```rust
    #[serde(default)]
    pub noise_applied: f32,
    ```
  - `Default` derive автоматически даст `0.0`. Schema v29 forward-compat OK через `#[serde(default)]`.

**Подшаги (порядок исполнения).**

1. Read `scorer.rs:252–276`, `:285–298`, `:308–313`. Убедиться что внутри ничего lifetime/private не использует.
2. Расширить `PickInfo` полем `noise_applied: f32` + `#[serde(default)]`.
3. Добавить три функции в `pick_best.rs` (`apply_pick_jitter`, `plan_noise_internal`, `plan_start_tile`). Pin imports.
4. **Не вызывать** `apply_pick_jitter` из `PickBestStage::apply`. Подавить dead-code warning через `#[allow(dead_code)]` на функции (или `pub(super)` + единственный test caller).
5. Написать unit-тесты (см. чек-лист): `pick_jitter_no_op_when_noise_amp_zero`, `pick_jitter_skips_masked_plans`, `pick_jitter_is_plan_order_invariant` (мигрирует body из `scorer.rs:1162` тестов или пишется заново — два пула с противоположным порядком plan'ов, проверить что noise per-plan совпадает).
6. `cargo build --all-targets`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
7. ai_scenarios прогон: должен быть bit-identical post-8.B.3 (jitter ещё не подключён в production).

**Ловушки и риски.**

- **`StageCtx` borrow checker.** `apply_pick_jitter` принимает `&StageCtx`, читает `ctx.scoring.world.difficulty.score_noise()`, `ctx.scoring.active.entity`, `ctx.scoring.snap.round`. Все три — immutable reads. `pool: &mut ScoredPool` ОК — функция мутирует `pool.annotations[i].score`. Не передавай `&mut StageCtx` — borrow conflict с `pool.plans.iter().zip(pool.annotations.iter_mut())`.
- **`Vec<f32>` accumulator allocation.** Для `pool.len()` в районе 100–500 это негорячо, но если переживаешь — pre-allocate `Vec::with_capacity(pool.len())`.
- **`hash_canonical` exposure.** `TurnPlan::hash_canonical` уже pub (`scorer.rs:293` его вызывает не через `crate::`). Confirm нет `pub(crate)` ограничения, иначе open up.
- **Двойное определение `plan_start_tile`.** В commit 1 будет **две копии** — одна в `scorer.rs`, одна в `pick_best.rs`. Это intermediate state. Cleanup в коммите 2 удалит scorer-version. Add comment "// Copy of scorer.rs::plan_start_tile — будет удалён в 8.C commit 2" чтобы code reviewer не запутался.
- **`PickInfo` round-trip serde.** Existing JSONL fixtures без `noise_applied` field должны читаться через `#[serde(default)]`. Pin via `pick_info_v29_load_pre_8c_round_trip` test — сериализация без поля → десериализация выдаёт `0.0`.

**Gate-проверка (commit 1).**

- `cargo build --all-targets` зелёный.
- `cargo clippy --all-targets -- -D warnings` зелёный.
- New tests:
  - `pick_jitter_no_op_when_noise_amp_zero` — `DifficultyProfile::default()` (или fixture с `score_noise = 0.0`) → vec[0.0; n].
  - `pick_jitter_skips_masked_plans` — `ann.score = -inf` для одного plan'а → noise[i] = 0.0 для него, ann.score не модифицирован.
  - `pick_jitter_is_plan_order_invariant` — два пула одинаковых plan'ов в обратном порядке → noise per-plan совпадает по канонической identity.
  - `pick_info_default_noise_applied_zero` — `PickInfo::default().noise_applied == 0.0`.
- Existing `cargo test` зелёный.
- ai_scenarios идентичен post-8.B.3 (jitter не подключён).

---

## Commit 2 — Подключение jitter в `PickBestStage::apply` + удаление Pass 2 + helpers из `scorer.rs`

**Цель.** Behavior switch. `apply_pick_jitter` вызывается из `PickBestStage::apply` ДО `pick_best_plan`. `finalize_scores` теряет Pass 2 noise block + два helper'а. `noise_applied` записывается в `PickInfo` для победителя. После коммита: ai_scenarios зелёный с допустимым FP-budget (≤3/N per spec §"Gate"), winners идентичны.

**Файлы (изменить).**

- `src/combat/ai/pipeline/stages/pick_best.rs`:
  - В `PickBestStage::apply` (`:19`):
    ```rust
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        if pool.is_empty() { return; }
        let noise_per_plan = apply_pick_jitter(pool, ctx);
        let scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
        let raw_factors: Vec<_> = pool.annotations.iter().map(|a| a.factors).collect();
        let (best_idx, mech) = pick_best_plan(&scores, &raw_factors, ctx.scoring.world, ctx.rng);
        pool.annotations[best_idx].chosen = true;
        pool.annotations[best_idx].pick = Some(PickInfo {
            mechanics: mech,
            noise_applied: noise_per_plan[best_idx],
        });
    }
    ```
  - Снять `#[allow(dead_code)]` с `apply_pick_jitter` / `plan_noise_internal` / `plan_start_tile`.

- `src/combat/ai/planning/scorer.rs`:
  - Удалить Pass 2 целиком: `:252–276` (комментарий "// Pass 2: add deterministic, batch-scaled noise." и весь if-блок).
  - Удалить переменную `let noise_amp = world.difficulty.score_noise();` (`:205`). Не используется больше.
  - Удалить fn `plan_noise` (`:281–298`) — целиком.
  - Удалить fn `plan_start_tile` (`:300–313`) — целиком.
  - Очистить unused imports: `std::hash::{Hash, Hasher}` (line 43), `bevy::prelude::Entity` (line 42) — проверить через `cargo check` что других callers нет в этом файле; если есть — оставить.
  - Обновить doc-comment `finalize_scores` (`:146–162`): убрать упоминания "deterministic, batch-scaled noise" — теперь pure factor+terminal aggregation. Заменить на: "Returns pre-modifier, pre-noise scores. PlanModifiersStage добавляет modifiers; PickBestStage добавляет jitter."
  - Обновить header doc-comment файла (`:1–25`) если там упоминается noise — заменить указание на pick_best stage.

**Подшаги.**

1. Включить вызов `apply_pick_jitter` в `PickBestStage::apply` + writer `noise_applied` в `PickInfo`.
2. `cargo build` — должен скомпилироваться (jitter теперь работает в обоих местах: scorer Pass 2 + PickBest). Behavior: noise applied **дважды** — это intermediate breakage. **Не запускать ai_scenarios на этом sub-step.**
3. Удалить Pass 2 block в `finalize_scores` (`scorer.rs:252–276`) + удалить `let noise_amp = ...` (`:205`).
4. Удалить fn `plan_noise` и `plan_start_tile` в `scorer.rs`.
5. Очистить unused imports: попробуй `cargo build`; компилятор укажет какие.
6. Обновить doc-comments `finalize_scores` + scorer.rs header.
7. `cargo build --all-targets`, `cargo clippy --all-targets -- -D warnings`.
8. `cargo test --lib` — сейчас могут упасть тесты, читающие legacy noise behavior. См. ловушки.
9. ai_scenarios — должен быть зелёный с FP-budget ≤3/N.

**Ловушки и риски.**

- **Двойной apply во время refactor.** Между шагами 1 и 3 noise apply'ится дважды (один раз в scorer Pass 2, один в pick_best). Не commit'ить промежуточное состояние; не запускать ai_scenarios — score double-counted. Шаги 1+3+4 — атомарный transition, делай в одной правке перед `cargo build`.
- **Tests, читающие legacy noise.** В `scorer.rs:1162` тест `self_lethal_kill_support_outscores_passive_under_last_stand` упоминает "score_noise > 0 — but the ranking assertion below is robust to noise". Этот тест должен продолжать работать (noise теперь живёт ниже, в pick_best stage), но если он вызывает `score_plans_with_raw` напрямую и НЕ идёт через pipeline — он больше НЕ видит noise, и assertion `>` может стать строже. Проверить тест явно. Возможно: добавить вызов `PickBestStage::apply` или мигрировать тест на pipeline-level.
- **Тесты в scorer.rs `:1016, :1021`** — содержат assert `difficulty.score_noise() > 0.0`. Они проверяют поведение `easy()` / `hard()` profiles, не носят noise apply behavior. Не трогать.
- **`scorer.rs:2018, :2052, :2103, :2108`** — прямые вызовы `finalize_scores` в тестах. Они получали noise в legacy. После 8.C — НЕ получают. Вероятный fix: либо тест проверял именно factor-aggregation behavior (тогда post-8.C score чище и тест проходит), либо проверял final-output behavior (тогда мигрировать на `score_plans_with_raw → pipeline`). Прочитать каждый тест и решить per-case. Spec §"Тесты" pin: `finalize_scores_no_longer_writes_modifiers_or_noise` — добавь ОДИН такой pin-test, остальные четыре — рекалибруй.
- **`apply_pick_jitter` returns `Vec<f32>` allocation per pick.** Hot path concern: `PickBestStage` вызывается раз на actor-tick. `pool.len()` ≤ ~500. ~2 KB alloc — negligible. Не оптимизируй.
- **Pipeline order test (`pipeline_pick_runs_jitter_before_argmax`).** Spec §"Тесты" предлагает этот pin. Реализация: построить pool с двумя plan'ами одинакового score, прогнать `PickBestStage::apply`, проверить `winner.pick.noise_applied != 0.0` под non-zero noise_amp difficulty (DifficultyProfile::hard или fixture).

**Gate-проверка (commit 2).**

- `cargo build --all-targets` зелёный.
- `cargo clippy --all-targets -- -D warnings` зелёный.
- New / migrated tests:
  - `pick_jitter_records_noise_applied_in_pick_info` — pin поле в PickInfo для победителя.
  - `pipeline_pick_runs_jitter_before_argmax` — order pin.
  - `finalize_scores_no_longer_writes_noise` — fixture: zero modifiers, zero noise → finalize_scores output = factor_sum + terminal_sum (legacy hand-computed).
  - Migrated: `self_lethal_kill_support_outscores_passive_under_last_stand` (ranking assertion работает через pipeline).
- ai_scenarios зелёный (14/14, может быть FP-edge ≤3/N).
- `cargo test --lib` зелёный (554 + new tests).
- `cargo run --release --bin ai_scenarios -- --diff` показывает winners идентичны post-8.B.3.

---

## Commit 3 — Mining sections v29 (modifiers + jitter)

**Цель.** `bin/mine_ai_logs.rs` получает две новые секции: `=== Modifier contributions ===` и `=== Picking jitter ===`. `bin/replay_ai_log.rs` schema-label sanity check (label "v29" уже из 8.A.3 — verify, нет работы). Не блокирует core gate; если выходит за время — выноси в follow-up commit без потери 8.C scope.

**Файлы (изменить).**

- `src/bin/mine_ai_logs.rs`:
  - Расширить struct `Aggregate` (`:39+`):
    ```rust
    // E1: modifier contribution distributions (per-actor, per-plan).
    e1_summon_bonus: Vec<f32>,    // non-zero only
    e1_trade_bonus: Vec<f32>,
    e1_repair_bonus: Vec<f32>,
    e1_total_modifier_entries: usize,  // counts plans with at least one modifier emitted
    // E2: picking jitter (per chosen plan).
    e2_noise_applied: Vec<f32>,
    e2_chosen_count: usize,
    ```
  - В main aggregation loop (по плодам actor_tick events) добавить collection:
    - Walk `event.pool[i].annotations.modifiers` — для каждой `ModifierContribution` push в соответствующий vec по `name`. Skip zero-contributions (consistency with d1_* style, см. `:80`).
    - Walk `event.pool[i].annotations.pick` — если `Some(pi)` и `pi.noise_applied != 0.0` (или pin via `chosen=true`), push в `e2_noise_applied`.
  - Добавить два print-helpers либо использовать существующий `print_fact_field` (`:433`).
  - В main report after Class D output добавить:
    ```
    println!("=== Modifier contributions (E1) ===");
    print_fact_field("summon_bonus", &mut agg.e1_summon_bonus, agg.e1_total_modifier_entries);
    print_fact_field("trade_bonus", &mut agg.e1_trade_bonus, agg.e1_total_modifier_entries);
    print_fact_field("repair_bonus", &mut agg.e1_repair_bonus, agg.e1_total_modifier_entries);
    println!();
    println!("=== Picking jitter (E2) ===");
    print_fact_field("noise_applied", &mut agg.e2_noise_applied, agg.e2_chosen_count);
    ```
  - Header doc (`:1–22`) — добавить раздел "Class E (factor breakdown)" с пунктами E1 / E2.

- `src/bin/replay_ai_log.rs`:
  - Verify schema label "v29" уже есть (8.A.3 update). Если modifiers / noise_applied не отображаются в replay output — добавить вывод annotation.modifiers вектора + pick.noise_applied на строку с pick info. Опционально, если scope позволяет.

**Подшаги.**

1. Прочитать `mine_ai_logs.rs:39–105` (Aggregate struct + processing). Понять style.
2. Прочитать `mine_ai_logs.rs:433–451` (print_fact_field) — переиспользовать.
3. Расширить `Aggregate` struct полями E1/E2.
4. В processing loop — walk annotation.modifiers + annotation.pick.
5. Добавить два print sections в main report.
6. Запустить mine_ai_logs против существующего v29 corpus (если корпус есть в repo — `logs/`); если нет, создать через `cargo run --bin ai_scenarios`. **Этот шаг — read-only execution, не file write — допустим.** Если corpus не доступен — добавить unit-test `mine_v29_corpus_produces_modifier_section` через synthetic `ActorTickEvent` (см. `replay_ai_log_test*`) и захардкоженные `ModifierContribution`s.
7. `cargo build --bins`, `cargo clippy --all-targets`.

**Ловушки и риски.**

- **`ActorTickEvent` schema reach.** Modifier contributions живут глубоко в `event.pool[i].annotations.modifiers`. Verify path к `pool` field в `LoggedDecision` / `ActorTickEvent` (`use storyforge::combat::ai::log::{ActorTickEvent, LoggedDecision}`). Скорее всего pool приходит из event.plans / event.annotations — не из Decision. Прочитать `combat/ai/log.rs` структуру event'а перед началом.
- **No corpus available.** Если `logs/` пустой / не в git — нужен либо synthetic test (предпочтительно), либо пользователь запустит mine после landing. Spec §"Что НЕ в scope 8.C" подтверждает: "Mining v29 corpus — после 8.C, отдельным коммитом (не в 8.C scope; rebuild — user-driven)". Принять: тест на code path (`mine_v29_corpus_produces_modifier_section` с in-memory event), не на real corpus.
- **`print_fact_field` mutates input.** Имя suggests reordering input (sort для percentiles). Если modifier contributions содержат negative values (trade_bonus может быть signed?) — verify percentile semantics не ломаются. Если trade_bonus signed — добавить отдельный print branch с sign-aware aggregation. Spec §"Picking jitter" явно говорит "mean / max / sign of effect" — именно sign-aware. Реализуй mini sign-aware reporter.
- **Dead bin scope.** `mine_ai_logs.rs` не покрывается `cargo test --lib` стандартно. `cargo test --bins` обязательно для CI gate.

**Gate-проверка (commit 3).**

- `cargo build --bins` зелёный.
- `cargo test --bins` зелёный (если есть test).
- New tests:
  - `mine_v29_corpus_produces_modifier_section` — synthetic event → output contains "=== Modifier contributions ===".
  - `mine_v29_corpus_produces_jitter_section` — synthetic event → output contains "=== Picking jitter ===".
- `cargo run --release --bin mine_ai_logs -- --dir logs/` (если corpus есть) — выводит обе секции без panics.

---

## Чек-лист тестов (полный 8.C)

| Test | Расположение | Commit |
|---|---|---|
| `pick_jitter_no_op_when_noise_amp_zero` | `pick_best.rs::tests` | 1 |
| `pick_jitter_skips_masked_plans` | `pick_best.rs::tests` | 1 |
| `pick_jitter_is_plan_order_invariant` (мигрировать idea из `scorer.rs:1162`) | `pick_best.rs::tests` | 1 |
| `pick_info_default_noise_applied_zero` | `outcome/mod.rs::tests` | 1 |
| `pick_info_v29_load_pre_8c_round_trip` (forward-compat для existing logs) | `outcome/mod.rs::tests` | 1 |
| `pick_jitter_records_noise_applied_in_pick_info` | `pick_best.rs::tests` | 2 |
| `pipeline_pick_runs_jitter_before_argmax` | `pipeline/mod.rs::tests` | 2 |
| `finalize_scores_no_longer_writes_noise` (factor+terminal sum identical к hand-computed) | `scorer.rs::tests` | 2 |
| Migrate / adapt: `self_lethal_kill_support_outscores_passive_under_last_stand` (`scorer.rs:1162`) | `scorer.rs::tests` или pipeline | 2 |
| `mine_v29_corpus_produces_modifier_section` | `bin/mine_ai_logs.rs::tests` | 3 |
| `mine_v29_corpus_produces_jitter_section` | `bin/mine_ai_logs.rs::tests` | 3 |
| (опц.) `replay_v29_round_trip_zero_diff` | `bin/replay_ai_log.rs::tests` | 3 |

Spec § "Тесты" перечисляет 8 пунктов; здесь добавлены 2 forward-compat / sanity (commit 1) и явно расщеплён `finalize_scores_no_longer_writes_modifiers_or_noise` на migrate-existing вместо new-test (modifiers уже не пишет finalize_scores с post-8.B; в 8.C проверяется noise removal).

---

## Финальный gate 8.C

- `cargo build --all-targets` + `cargo clippy --all-targets -- -D warnings` зелёные.
- `cargo test --all-targets` зелёный (lib 554 + new + bin tests).
- ai_scenarios: 14/14 зелёный. **Winners идентичны post-8.B.3 baseline** (sanity check через `cargo run --release --bin ai_scenarios -- --diff` или эквивалент).
- Per-entry FP-edge **≤3/N** (spec §"Gate"). Если >3/N — расследовать:
  - первый подозреваемый — порядок суммирования (legacy: `factor + terminal + noise`; new: `factor + terminal → modifiers → noise`). Modifiers уже сдвинули порядок в 8.B; 8.C добавил noise после modifiers → cumulative shift.
  - Если spread close to floor (0.05) — early-return change в spec §"Open question 3" может изменить behavior на edge-cases.
- `finalize_scores` size: целевая ~80 строк (was ~117 pre-8.C, ~250 pre-step-8). Pin via line-count в reviewer-comment commit message.
- `bin/mine_ai_logs.rs` выводит обе новые секции на synthetic event без panics.
- v29 corpus rebuild — **out of 8.C scope** (user-driven follow-up commit).

---

### Critical Files for Implementation

- /Users/splav/personal/storyforge/src/combat/ai/pipeline/stages/pick_best.rs
- /Users/splav/personal/storyforge/src/combat/ai/planning/scorer.rs
- /Users/splav/personal/storyforge/src/combat/ai/outcome/mod.rs
- /Users/splav/personal/storyforge/src/bin/mine_ai_logs.rs
- /Users/splav/personal/storyforge/docs/ai_rework_step8_plan.md (spec — re-read §"Сабшаг 8.C" перед каждым коммитом)
