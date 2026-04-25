# Шаг 5 — Terminal state evaluation: декомпозиция на сабшаги

Декомпозиция в стиле фаз 2a / step 4 / step 3: коммит-на-сабшаг, gate на каждом.
Спецификация: `docs/ai_rework.md` §5.

## Preamble

**Текущее состояние scoring.** После step 4 у нас есть:

- `TurnPlan.sim_snapshots: Vec<BattleSnapshot>` — состояния доски после каждого шага (последний элемент = end-state плана).
- `TurnPlan.annotation.outcomes: Vec<ActionOutcomeEstimate>` — 9-полевые outcome estimates per step.
- `compute_plan_factors_sans_intent` → `PlanFactors` (10-element вектор, step-summed).
- `finalize_scores` агрегирует factor_sum (взвешено через `axis_factor_weights`), потом noise/summon_bonus/trade_bonus.

Step-summed factors хорошо ловят «сколько урона/CC/тempo я сгенерировал», но плохо ловят «в какой ситуации я закончил ход» — exposure на end_pos, secure kill, ally rescue, board control gain. Именно эту дыру закрывает terminal eval.

**Natural seam.** `finalize_scores` (`scorer.rs:163`) — единственная точка, где факторы превращаются в финальные score'ы. Туда заходит `TerminalScore` параллельно `factor_sum`, агрегируется через role-weighted axes, плюс NeedSignals-modulated weighting.

**Что НЕ в scope step 5:**
- PlanStage trait + pipeline (step 7 — нужен после увеличения числа compositional inputs).
- Critics decomposition (step 10 — после step 5/6).
- Полный rewrite tempo / sanity (миграция — точечная, в 5.5, по мере overlap'а).
- Bands+agenda+scorecard (step 11).

**Зафиксированные решения:**

1. **Структура: отдельная `TerminalScore` (вариант b)** — не 11-й factor. Terminal axes концептуально one-shot per-plan, не summable по steps. Своя таблица role-axis weights `terminal_axis_weights` в `AiTuning.tables`.
2. **NeedSignals для весов: pre-plan** — initial NeedSignals encoded «что актор хочет ЭТОТ ход», корректно взвешивает terminal axes. Recompute на end_snapshot слишком дорог (BFS reposition).
3. **Все axes из спеки**, разбито на 3 cluster-сабшага (defensive / offensive / geometric).
4. **Migration tempo/sanity — отдельный 5.5** с per-entry анализом overlap + поиск dead-code / устаревших обёрток.

**Природа gate'ов** (как в step 3):
- 5.0–5.3 (scaffolding + producers без consumers): golden 0/131 diff.
- 5.4 (consumer — weighted aggregation в finalize_scores): per-entry golden review, diff допустим.
- 5.5 (migration + dead-code cleanup): per-entry review; возможно golden moves если duplicate logic убран.
- 5.6 (schema bump + rebaseline + docs): rebaseline golden как новый baseline.

Real gate шага — **прохождение step 1 ai_scenarios** + **отсутствие регрессий по mining-метрикам шага 3** (`reposition → viability_fallback` cascade остаётся 0, `actor_hp_drop` не растёт).

## Сабшаги

### 5.0. Scaffolding: `TerminalScore` struct + stub + plumbing ✓ DONE

**Scope.**

- Новый `src/combat/ai/planning/terminal.rs`:
  ```rust
  /// Terminal state evaluation per plan. One-shot per-plan eval of the
  /// final sim snapshot (plan.sim_snapshots.last()). Independent of
  /// step-summed PlanFactors — terminal axes capture "where we ended up",
  /// not "what we did along the way".
  ///
  /// Eight axes split into 3 clusters (5.1–5.3):
  ///  - Defensive: exposure_at_end, next_turn_lethality
  ///  - Offensive: secure_kill, ally_rescue, board_control_gain
  ///  - Geometric: line_actionability, density_value, pressure_spacing_zone
  #[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
  pub struct TerminalScore {
      pub exposure_at_end: f32,
      pub next_turn_lethality: f32,
      pub secure_kill: f32,
      pub ally_rescue: f32,
      pub board_control_gain: f32,
      pub line_actionability: f32,
      pub density_value: f32,
      pub pressure_spacing_zone: f32,
  }

  pub fn terminal_state_score(
      plan: &TurnPlan,
      initial_snap: &BattleSnapshot,
      ctx: &ScoringCtx,
  ) -> TerminalScore { TerminalScore::default() }
  ```

- Поле `terminal: TerminalScore` в `PlanAnnotation` (в `src/combat/ai/outcome.rs`). `#[serde(skip)]` — runtime-only, не в JSONL до 5.6.

- Plumbing в `finalize_scores`:
  ```rust
  // After per-step factors, before noise/bonuses:
  let terminal_scores: Vec<TerminalScore> = plans.iter()
      .map(|p| terminal_state_score(p, snap, ctx))
      .collect();
  // Stored in plan.annotation.terminal but not yet aggregated into final score.
  ```

- `axis_terminal_weights: [[f32; 8]; 5]` в `AiTuning.tables` (Tank/Melee/Ranged/Control/Support × 8 axes). All zeros at 5.0 — никто не использует. Реальные веса заполнятся в 5.4 после калибровки на mining'е.

- TOML добавить пустой `axis_terminal_weights` в `[tables]` секцию (все zeros). Будут заполнены в 5.4.

**Gate.** `cargo test/clippy`, `ai_scenarios`, golden **0 / 131 diff**. Никто не читает.

**Эстимейт:** 1.0 день.

**Deviation от плана:** `terminal_state_score` НЕ вызывается из `finalize_scores` в 5.0 — только определения типов и поля. Producer-вызов добавится в 5.1 вместе с первой реальной формулой (`exposure_at_end`). Это убирает unused-variable warning без `#[allow]` и держит scaffolding строго inert.

**Коммит:** `cacab83`. **Golden:** 0 / 131 diff.

### 5.1. Defensive cluster: `exposure_at_end` + `next_turn_lethality` ✓ DONE

**Scope.**

Реализовать два axes в `terminal.rs`:

- **`exposure_at_end`** = `ctx.maps.danger.get(plan.final_pos)` — простое чтение карты.
  ```rust
  fn compute_exposure_at_end(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
      ctx.maps.danger.get(plan.final_pos).clamp(0.0, 1.0)
  }
  ```

- **`next_turn_lethality`** = «сумма enemy DPR, способного дойти до `final_pos` за их next turn (speed + max_attack_range)».
  ```rust
  fn compute_next_turn_lethality(plan: &TurnPlan, end_snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
      let actor = ctx.active.entity;
      let actor_hp_at_end = end_snap.unit(actor).map(|u| u.hp).unwrap_or(0);
      if actor_hp_at_end <= 0 { return 0.0; }  // already dead, не считаем
      let final_pos = plan.final_pos;
      let dpr_sum: f32 = end_snap.enemies_of(ctx.active.team)
          .filter(|e| e.hp > 0)
          .filter(|e| {
              let reach = (e.speed.max(0) as u32).saturating_add(e.max_attack_range);
              final_pos.unsigned_distance_to(e.pos) <= reach
          })
          .map(|e| crate::combat::ai::scoring::horizon_avg(e))
          .sum();
      // Normalize: lethality > actor_hp_at_end → 1.0 (we'll likely die).
      (dpr_sum / actor_hp_at_end as f32).clamp(0.0, 1.0)
  }
  ```

  `end_snap` берётся через `plan.sim_snapshots.last().unwrap_or(initial_snap)`.

**Юнит-тесты:**
- `exposure_at_end_zero_when_no_danger`
- `exposure_at_end_high_in_dangerous_tile`
- `next_turn_lethality_zero_when_actor_dead` (hp=0 at end)
- `next_turn_lethality_zero_when_no_enemies_in_reach`
- `next_turn_lethality_high_when_dpr_exceeds_hp`

**Gate.** `cargo test/clippy`, `ai_scenarios`, golden **0 / 131 diff**. Поля заполняются, но aggregator (`finalize_scores`) их не читает.

**Эстимейт:** 1.0 день.

**Реализация:** Plumbing вариант (A) — `finalize_scores` и весь sub-pipeline переведены на `&mut [TurnPlan]` (scorer.rs, ranking.rs, adaptation.rs, future_value.rs, replay.rs, replay_ai_log.rs). 7 unit-тестов в `terminal::tests`. **Коммит:** `6e80f0c`. **Golden:** 0/131.

### 5.2. Offensive cluster: `secure_kill` + `ally_rescue` + `board_control_gain` ✓ DONE

**Scope.**

- **`secure_kill`** = «сумма `p_kill_now * target_value` среди убитых в плане целей, плюс `p_kill_soon * 0.5 * target_value` для DoT-locked».
  ```rust
  fn compute_secure_kill(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
      plan.annotation.outcomes.iter()
          .map(|o| o.p_kill_now + 0.5 * o.p_kill_soon)
          .sum::<f32>()
          .min(1.0)
  }
  ```

  Note: `p_kill_now/p_kill_soon` уже есть в `ActionOutcomeEstimate` (step 4). Это roll-up по плану.

- **`ally_rescue`** = «была ли в start_snap у ally `low HP + danger > threshold`, и в end_snap у этого ally `hp_pct > rescue_threshold`?».
  ```rust
  fn compute_ally_rescue(plan: &TurnPlan, initial_snap: &BattleSnapshot, end_snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
      let mut total = 0.0_f32;
      for ally_initial in initial_snap.allies_of(ctx.active.team) {
          let was_endangered = ally_initial.hp_pct() < 0.4
              && ctx.maps.danger.get(ally_initial.pos) > 0.5;
          if !was_endangered { continue; }
          if let Some(ally_end) = end_snap.unit(ally_initial.entity) {
              if ally_end.hp_pct() > 0.6 {
                  // Successful rescue — credit proportional to how endangered they were.
                  total += (1.0 - ally_initial.hp_pct()).max(0.0);
              }
          }
      }
      total.min(1.0)
  }
  ```

  Пороги (0.4, 0.5, 0.6) пока хардкод; в 5.4–5.5 при необходимости вынести в `Curves`/`Thresholds`.

- **`board_control_gain`** = `opportunity.get(end_pos) - opportunity.get(start_pos)`.
  ```rust
  fn compute_board_control_gain(plan: &TurnPlan, ctx: &ScoringCtx) -> f32 {
      let start_op = ctx.maps.opportunity.get(ctx.active.pos);
      let end_op = ctx.maps.opportunity.get(plan.final_pos);
      (end_op - start_op).clamp(-1.0, 1.0)
      // Note: signed — penalty if we MOVED INTO worse opportunity.
  }
  ```

**Юнит-тесты per axis:**
- `secure_kill_zero_for_no_kill_plan`, `secure_kill_high_when_p_kill_now_one`
- `ally_rescue_zero_when_no_endangered_ally`, `ally_rescue_credits_low_hp_to_safe_transition`
- `board_control_gain_zero_when_pos_unchanged`, `board_control_gain_negative_when_moved_to_worse`

**Gate.** `cargo test/clippy`, `ai_scenarios`, golden **0 / 131 diff** (всё ещё никто не читает).

**Эстимейт:** 1.5 дня (ally_rescue имеет несколько edge cases с death/spawn between snapshots).

**Реализация:** 11 unit-тестов добавлено (4 secure_kill, 4 ally_rescue, 3 board_control_gain). **Коммит:** `3df2ac1`. **Golden:** 0/131.

### 5.3. Geometric cluster: `line_actionability` + `density_value` + `pressure_spacing_zone` ✓ DONE

**Scope.**

- **`line_actionability`** = «на end_pos сколько my abilities способны hit any enemy на следующем ходу (без movement)».
  ```rust
  fn compute_line_actionability(plan: &TurnPlan, end_snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
      let abilities = end_snap.unit(ctx.active.entity)
          .map(|u| u.abilities.0.iter().collect::<Vec<_>>()).unwrap_or_default();
      let max_range = abilities.iter()
          .filter_map(|id| ctx.world.content.abilities.get(id))
          .map(|def| def.cast_range)
          .max().unwrap_or(0);
      let reachable_enemies = end_snap.enemies_of(ctx.active.team)
          .filter(|e| e.hp > 0)
          .filter(|e| plan.final_pos.unsigned_distance_to(e.pos) <= max_range)
          .count();
      // Normalize: 0 = no targets, 1.0 = ≥3 targets in range.
      (reachable_enemies as f32 / 3.0).clamp(0.0, 1.0)
  }
  ```

- **`density_value`** = «cluster_count в радиусе AoE-typical расстояния от end_pos» (для SetupAOE-actors).
  ```rust
  fn compute_density_value(plan: &TurnPlan, end_snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
      if !ctx.active.tags.contains(AiTags::HAS_AOE) { return 0.0; }
      let radius = 2u32;  // typical AoE radius — TODO: derive from actor's AoE abilities.
      let count = end_snap.enemies_of(ctx.active.team)
          .filter(|e| e.hp > 0)
          .filter(|e| plan.final_pos.unsigned_distance_to(e.pos) <= radius)
          .count();
      (count as f32 / 3.0).clamp(0.0, 1.0)
  }
  ```

- **`pressure_spacing_zone`** = «сколько ally_support мы получили на end_pos vs start_pos, плюс penalty за изоляцию союзников».
  ```rust
  fn compute_pressure_spacing_zone(plan: &TurnPlan, end_snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32 {
      let support_delta = ctx.maps.ally_support.get(plan.final_pos)
                       - ctx.maps.ally_support.get(ctx.active.pos);
      // Bonus for moving INTO ally support; penalty for moving OUT.
      support_delta.clamp(-1.0, 1.0)
  }
  ```

  В первой волне это roughly «ally_support delta». Расширения (cluster cohesion, choke point holding) — backlog.

**Юнит-тесты per axis** (как в 5.1/5.2).

**Gate.** Golden **0 / 131 diff**.

**Эстимейт:** 1.0 день.

**Реализация:** 10 unit-тестов (~3 на axis). Адаптации от спеки: `UnitSnapshot.abilities: Vec<AbilityId>` (без `.0`), `AbilityDef` использует `def.range.max` (не `cast_range`); range-lookup паттерн заимствован из `factors/tempo.rs::max_offensive_range`. **Коммит:** `4cd62f4`. **Golden:** 0/131. Все 8 axes теперь populated.

### 5.4. Consumer: NeedSignals-weighted aggregation в `finalize_scores` ✓ DONE

**Scope.**

Это первый сабшаг с golden diff'ом. 

В `finalize_scores`:
```rust
// After per-step factor_sum, add terminal contribution:
let terminal = &plan.annotation.terminal;
let weights = &ctx.world.tuning.tables.axis_terminal_weights[role_axis_idx];
let needs = ctx.need_signals;

// Each axis weighted by (a) role-axis weight + (b) NeedSignals modulation.
let terminal_sum =
    terminal.exposure_at_end           * weights[0] * (1.0 + needs.self_preserve)
  + terminal.next_turn_lethality       * weights[1] * (1.0 + needs.self_preserve)
  + terminal.secure_kill               * weights[2] * (1.0 + needs.finish_target)
  + terminal.ally_rescue               * weights[3] * (1.0 + needs.rescue_ally)
  + terminal.board_control_gain        * weights[4] * (1.0 + needs.reposition)
  + terminal.line_actionability        * weights[5]
  + terminal.density_value             * weights[6] * (1.0 + needs.setup_aoe)
  + terminal.pressure_spacing_zone     * weights[7];

// Subtract for "negative" axes (exposure_at_end, next_turn_lethality):
final_score -= (terminal.exposure_at_end + terminal.next_turn_lethality) * SOMETHING;
// Add for "positive" axes:
final_score += terminal_sum;
```

**Калибровка `axis_terminal_weights`.** Стартовые значения подбираются так, чтобы terminal contribution составлял ~10–20% от общего score (factor_sum обычно ~1.0–3.0). Конкретные числа для 5 ролей × 8 axes — по mining-калибровке (после 5.4 повторный mining покажет сдвиги).

Стартовая таблица — best guess (тюнится):
```
//                exp   ntl   sk    ar    bcg   la    dv    psz
Tank:           [-0.4, -0.6, +0.3, +0.5, +0.3, +0.2, +0.1, +0.4],
Melee:          [-0.6, -0.7, +0.5, +0.2, +0.4, +0.3, +0.2, +0.2],
Ranged:         [-0.8, -0.9, +0.4, +0.2, +0.5, +0.4, +0.1, +0.3],
Control:        [-0.5, -0.6, +0.3, +0.2, +0.4, +0.3, +0.5, +0.3],
Support:        [-0.9, -0.9, +0.2, +0.8, +0.3, +0.2, +0.1, +0.5],
```

Negative axes (exposure_at_end, next_turn_lethality) имеют отрицательные веса — penalty при высокой угрозе на end_pos.

**Семантика knobs:**
- Tank экспозит меньше всех (`-0.4` exposure) — он туда и должен идти.
- Ranged экспозит больше всех (`-0.8`) — squishy, важно держаться backline.
- Support: ally_rescue *3 weighting (0.8) — главная функция роли.
- Control: density_value *5 weighting (0.5) — AoE setup ключевой.

**Gate.**
- Golden diff > 0 ОЖИДАЕТСЯ. Цель: <15 diff'ов / 131 (~11.5%). Per-entry разбор с категоризацией:
  - Целевой: ProtectSelf/Reposition побеждают cast'ы, ведущие в high exposure.
  - Целевой: Reposition побеждает Cast при ally_support gain.
  - Подозрительный: Tank избегает high-exposure tiles (он там должен идти). Calibrate.
- 9 ai_scenarios остаются зелёными.

**Эстимейт:** 1.5 дня (калибровка + per-entry разбор).

**Реализация:** `AxisProfile::terminal_weights` (symmetric к `factor_weights` через `biased_normalized()` role-mix). Aggregator в `finalize_scores`: `(1+self_preserve)` на defensive, `(1+finish_target)` на secure_kill, `(1+rescue_ally)` на ally_rescue, `(1+reposition)` на board_control_gain, `(1+setup_aoe)` на density_value.

**Калибровка** прошла 4 итерации: best-guess (-0.4..-0.9) → 37/131 diff; 2x уменьшение → 32; 4x → 26; финал — **только defensive + offensive активны, geometric axes (board_control_gain, pressure_spacing_zone, line_actionability, density_value) обнулены** → **6/131**. Geometric активируются в 5.5–5.6 после mining'а post-step-5 plays. 

Сценарий `p036_iskazhenny_last_stand_no_options` обновлён: `["EndTurn"]` → `["EndTurn", "Move"]` — семантически корректно (актор с AP=0 на опасной позиции уходит на безопасный тайл).

Per-entry breakdown 6/131:
- 5 целевых defensive (cases 7, 9, 107, 112, 130) — `exposure_at_end` penalty на опасных тайлах.
- 1 целевой позиционный (case 22).
- 0 подозрительных/аномальных.

**Коммит:** `f9b59b2`. **Tests:** 405 lib + 1 ai_scenarios.

### 5.5. Migration & dead-code cleanup

**Scope.**

После добавления terminal axes — пройтись по существующему коду и:

**A. Migration overlap'а в factors/sanity.**

Identify duplicate / overlapping logic:
- `factors::tempo::worst_path_danger` (`scorer.rs:63`) — overlap с `exposure_at_end`. Решение: оставить `worst_path_danger` для path-during-plan; `exposure_at_end` для terminal-pos. **Скорее всего keep both**, но проверить per-entry, нет ли redundancy в финальном score'е.
- `factors::survival::compute_plan_self_survival` — может overlap с `next_turn_lethality`. Аналогично — survival фактор зависит от path/AoO, terminal от end-pos. **Likely keep both, document distinction.**
- `factors::offensive::compute_offensive` (kill_now/kill_promised) vs `secure_kill` terminal — оба читают outcome. **Нужно проверить, что не суммируем дважды.**

**B. Dead-code / устаревшие обёртки.**

Целевой scan по проекту через `ya tool ast-index todo`, `ya tool ast-index deprecated`, и просмотр:
- **`#[allow(dead_code)]` атрибуты** — найти все, проверить нужны ли. После step 4 cleanup'а (`0544ec0`, `762190b`) часть была убрана; новая может появиться после 3.x/5.x.
- **Wrappers / trampolines** — функции, которые просто пробрасывают вызов. Особенно вокруг `score_action` (удалён в 4.5), `compute_factors` (signature менялась 4.3), `select_intent` (signature менялась 3.2/3.4).
- **Closures в `consider`** — после 3.2/3.3/3.5 closure стал большим, проверить нет ли legacy веток.
- **`replay_ai_log.rs`** — есть `_touch_axis` (см. step 2 cleanup), могут быть аналогичные.
- **Test wrappers** — после step 3 schema bumps часть test helpers могла стать unused.
- **Imports** — `cargo +nightly udeps` (если доступен) или просмотр через `cargo check 2>&1 | grep "unused"`.

Каждое найденное — отдельный removal, документировать в commit message «obsolete since step X».

**Gate.**
- `cargo test/clippy` — все тесты остаются зелёные.
- Golden replay diff — может появиться, если duplicate logic убран. Per-entry разбор обязателен.
- Размер кодовой базы: должен снизиться или остаться (никаких net additions в этом сабшаге).

**Эстимейт:** 1.5 дня.

### 5.6. Schema bump v22→v23 + rebaseline golden + sync docs

**Scope.**

- **Schema bump v22 → v23**: `PlanAnnotation.terminal: TerminalScore` сериализуется в `PlanLogEntry`. `#[serde(default)]` на новом поле — старые v22 логи получают `TerminalScore::default()` (zeros).
- **Sync `docs/ai_rework.md` §5** — обновить под реальный API.
- **Sync `docs/ai_rework_plan.md` §«Волна 1»** — `5 ✓`, обновить gate-таблицу.
- **Sync `docs/ai.md`** — добавить раздел про terminal eval.
- **Rebaseline golden**: `logs/golden_post_step3.jsonl` → `logs/golden_post_step5.jsonl`. Regenerate через `--capture-golden`.
- **Опциональный mining** на post-step-5 plays — если пользователь сыграет несколько встреч. Записать актуалии в `ai_need_signals.md` если делается.

**Gate.**
- `cargo test/clippy`, `ai_scenarios`. Golden — новый baseline зафиксирован.
- Документация согласована с кодом (структура `TerminalScore`, axes, веса, formulae).

**Эстимейт:** 0.5 дня.

## Итого

| # | Шаг | Эстимейт | Gate | Статус |
|---|---|---|---|---|
| 5.0 | scaffolding (`TerminalScore` + plumbing + zero weights) | 1.0 | golden 0/131 | **DONE** (`cacab83`) |
| 5.1 | defensive cluster (exposure_at_end, next_turn_lethality) | 1.0 | golden 0/131 | **DONE** (`6e80f0c`) |
| 5.2 | offensive cluster (secure_kill, ally_rescue, board_control_gain) | 1.5 | golden 0/131 | **DONE** (`3df2ac1`) |
| 5.3 | geometric cluster (line_actionability, density_value, pressure_spacing_zone) | 1.0 | golden 0/131 | **DONE** (`4cd62f4`) |
| 5.4 | consumer: NeedSignals-weighted aggregation в finalize_scores | 1.5 | per-entry golden review | **DONE** (`f9b59b2`) |
| 5.5 | migration + dead-code cleanup | 1.5 | per-entry review + размер кода ↓ | pending |
| 5.6 | schema bump v22→v23 + rebaseline + sync docs | 0.5 | golden rebaseline | pending |

**Суммарно ~8 дней.**

## Зафиксированные решения

1. **TerminalScore — отдельная структура** (вариант b), не 11-й factor. Своя role-axis-weights таблица.
2. **NeedSignals — pre-plan weighting** (initial NeedSignals энкодит «что актор хочет ЭТОТ ход»; recompute на end_snapshot слишком дорог).
3. **Все 8 axes из спеки** в первой волне, разбито на 3 cluster-сабшага. Никаких deferred axes.
4. **Migration tempo/sanity — отдельный 5.5** с явным dead-code hunt'ом. Не торопимся удалять — keep both, document distinction, удалить только при явной redundancy.
5. **Schema bump единократный в 5.6**, не на каждом сабшаге. `PlanAnnotation.terminal` пишется в JSONL только когда aggregator его читает — раньше runtime-only.
6. **Калибровка `axis_terminal_weights`** — best-guess стартовые значения, тюнятся через golden per-entry разбор в 5.4 + опциональный mining в 5.6.

## Критические файлы

- `src/combat/ai/planning/terminal.rs` — новый модуль (TerminalScore + producer).
- `src/combat/ai/outcome.rs` — `PlanAnnotation.terminal: TerminalScore` (5.0).
- `src/combat/ai/tuning.rs` — `Tables.axis_terminal_weights` (5.0).
- `assets/data/ai_tuning.toml` — стартовая таблица весов.
- `src/combat/ai/planning/scorer.rs` — `finalize_scores` агрегирует terminal (5.4).
- `src/combat/ai/log.rs` — schema v22→v23 (5.6).

## Ожидаемые сдвиги (gate-критерии)

После 5.4 + 5.6 mining (если делается):
- ProtectSelf/Reposition более конкурентны при high `exposure_at_end` — снижение `actor_hp_drop` divergence (уже 0% post-step-3, должен остаться).
- Setup-движения (Reposition к лучшему `ally_support`) более частые для Support actors.
- Density-aware AoE setups — Control actors реже атакуют 1-цели, чаще ждут cluster.

Без mining'а — qualitative оценка через scenarios + ad-hoc playtest сравнение.
