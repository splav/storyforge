# AI Rework — Developer Plan

План-руководство для имплементации следующей итерации. Контекст, решения и acceptance-метрики — [`docs/ai_rework.md`](ai_rework.md).

Каждый шаг описан в формате: **файлы + функции**, **суть правки**, **тесты**, **acceptance-хук**. Ссылки на код даны на актуальный HEAD.

---

## Шаг 0. Починить `replay_ai_log` ✅

**Блокер всей итерации** — без работающего replay метрики неизмеримы.

### Решение

Лог не содержит `campaign_dir`/`scenario_dir`, но имя файла кодирует их:
`<timestamp>_<campaign>_<scenario>_<encounter>.jsonl`.

Инференс: `infer_content_dirs()` сканирует `assets/data/campaigns/` и однозначно
определяет campaign/scenario по имени файла. Fallback — `--campaign`/`--scenario`
CLI-флаги, затем global-only с предупреждением.

`ContentView::load_global_for_tests()` (cfg-гейтирован, нет в release) заменён на
`ContentView::load_layered(&campaign_dir, &scenario_dir)` — работает и в debug, и в release.

### Acceptance

Выполнено: `cargo run --bin replay_ai_log -- logs/20260421T164625_*.jsonl`
выводит список replayed планов. Release-build компилируется без ошибок.

---

## Шаг 0.5. Baseline corpus

### Что делать

1. Собрать 10–20 боёв с **фиксированными seed'ами**. Использовать `[debug].ai_log = true` и `[debug].rng_seed = <N>` в `assets/data/settings.toml` (если нет — добавить, это отдельный one-liner).
2. Разнообразие сценариев: `demo_stormborn_camp`, `demo_beastblood_raid`, `bell_under_veil_ch1_*`.
3. Зафиксировать в `logs/corpus_20260422/` (новый каталог, отдельно от старого `corpus_20260421/`).
4. Прогнать текущие метрики, сохранить в `logs/baseline_20260422.txt`.

### Минимальный набор baseline-метрик

Сейчас `replay_ai_log` считает `wasted_mp_ratio`, `panic_leak_rate`, `killable_closure_rate`. Добавить:

- `repeated_tile_rate`, `zero_net_move_rate`, `post_cast_retreat_rate` (шаг 1 acceptance).

Реализация — в `Metrics` struct (`src/bin/replay_ai_log.rs:428+`). Для `post_cast_retreat_rate` нужен helper, разбивающий план на pre-cast и post-cast сегменты.

### Acceptance

Файл `baseline_20260422.txt` существует; репозиторный `cargo run --bin replay_ai_log -- logs/corpus_20260422/*.jsonl --metrics-summary` воспроизводит его цифры.

---

## Шаг 1. `tempo_gain` → net displacement ✅ (код готов, метрики не сошлись)

### Файл / функция

- `src/combat/ai/factors/tempo.rs:28-64` — `compute_plan_tempo_gain`.

### Что сделано

Per-step итерация заменена на одиночный `step_tempo(actor_start → plan.final_pos)`. Семантика `step_tempo` не менялась: dist_before/after теперь считается между start и final, range_bonus и exit_bonus работают как прежде.

Тесты в `factors/tempo.rs::tests`:
- `round_trip_move_gives_nonpositive_tempo` — реальный round-trip (A→B→A) возвращает tempo ≤ 0.
- `backtrack_longer_path_no_credit` — длина пути не даёт кредита при равном net displacement.
- 4 теста проходят, `cargo test combat::ai::factors::tempo` ok.

Параллельно — фикс `reach.rs`: `enemies_of` → `all_enemies_of`, чтобы BFS видел трупы и не планировал pass-through через них.

### Результат измерения (post-step-1 корпус, 2 боя)

`logs/baseline_20260422_step1.txt` vs `baseline_20260422.txt` (15 боёв):

| Метрика | baseline-15 | post-step-1 (2 боя) | delta |
|---|---|---|---|
| repeated_tile_rate | 29.3% | 27.5% | −1.8 pp (шум) |
| zero_net_move_rate | 17.3% | 15.7% | −1.6 pp (шум) |
| post_cast_retreat_rate | 33.3% | 30.0% | −3.3 pp (шум) |

**Acceptance не достигнут.** Целевые `<5%` / `<1%` / `↓≥70%` — далеко. Step 1 чинил только tempo; `intent_sum` остался доминирующим источником кредита за длину Move-цепочки → переходим в 1b безусловно.

### Доказательство из логов

`logs/20260421T191051_demo_campaign_demo_stormborn_camp.jsonl` line 12 — изолированный случай с контролируемыми переменными:

| | shape | final_pos | INT | TEMPO | surv | score |
|---|---|---|---|---|---|---|
| chosen #1 | Move(4,5) · **Move(4,4)** · Move(3,6) | [3,6] | **+2.06** | +0.11 | +0.36 | **2.07** |
| alt #2 | Move(4,5) · Move(3,6) | [3,6] | +1.48 | +0.11 | +0.36 | 1.91 |

Оба плана заканчиваются в **одной клетке**. Start = (4,4), chosen делает петлю через стартовую клетку. Tempo корректно одинаковый. Intent отличается на +0.58 — ровно за лишний Move. Round-trip выиграл исключительно из-за `intent_sum`.

Арифметика: `pursuit_move_score ≈ 0.8`, `base_discount = 0.9`:
- 3 шага: `0.8·(1+0.9+0.81) ≈ 2.05` ≈ **+2.06** в логе
- 2 шага: `0.8·(1+0.9) = 1.52` ≈ **+1.48** в логе

### Риск

Средний. Меняет семантику axis, которая уже учитывается в `AXIS_FACTOR_WEIGHTS`. Веса могут требовать пере-калибровки. Guard: regression на `damage_now`, `kill_now`, `cc_impact` axis distributions — Δ ≤ 5%.

---

## Шаг 1b. `intent_sum` для Move-цепочек — ✅

**Статус триггера**: метрики step-1 не сошлись (см. таблицу выше), изолированный кейс подтверждает что именно `intent_sum` — драйвер repeated_tile/zero_net.

### Файл / функция

- `src/combat/ai/planning/scorer.rs:519-549` — цикл аккумулирования `intent_sum`.
- `src/combat/ai/intent.rs:694-709` — `pursuit_move_score`.

### Варианты правки

**Вариант А (рекомендуется):** для pure-move цепочек (нет Cast в плане) заменить `Σ intent_score` на одиночный `pursuit_move_score(actor_start, plan.final_pos, target.pos, reach)`. План оценивается как план.

**Вариант Б:** `intent_sum` для Move-шагов — `max`, не `Σ`. Сохраняет per-step оценку, но запрещает длине быть credit'ом.

Предпочтение — А: концептуально проще, чинит root cause напрямую. Б оставить как fallback, если А сломает какой-нибудь edge case с pursuit reach'ем.

### Что сделано

Вариант А реализован в `compute_plan_intent_sum` (`scorer.rs`).

- Детекция pure-move: `plan.steps.iter().all(|s| matches!(s, PlanStep::Move { .. }))`.
- Для pure-move + FocusTarget/ApplyCC: один вызов `pursuit_move_score(actor_start, plan.final_pos, target.pos, reach)`. Путь не имеет значения.
- Для Cast-планов и всех остальных intent'ов: прежний per-step discounted sum сохранён без изменений.
- goal_achieved latch остался в per-step ветке (только Cast может записать kill в outcomes).

Импорт в scorer.rs: `cc_reach` и `pursuit_move_score` добавлены к импортам из `intent`.

Добавлены 4 теста:
- `pure_move_chain_intent_equals_single_pursuit` — 1/2/3 шага к одной final клетке дают одинаковый intent_sum.
- `round_trip_pure_move_intent_no_credit` — прямой pin изолированного кейса из лога (round-trip = прямой путь).
- `cast_after_moves_keeps_cast_intent` — Move+Cast не использует shortcut, Cast contributes normally.
- `goal_achieved_latch_still_works` — latch подавляет Move-credit после kill.

### Результат

`cargo test combat::ai::planning::scorer`: 12 тестов, все ok (было 8).  
`cargo test`: 261 unit + все интеграционные — 0 FAILED.  
`cargo build --release`: без ошибок.

Replay divergence (post-step-1b):
- `beastblood_raid.jsonl`: 5 divergence / 9 entries = 56%
- `stormborn_camp.jsonl`: 29 divergence / 45 entries = 64%
- Итого: 34/54 = **63%** — рост со старого ~63% ... однако эти цифры сравниваются с логами которые были сгенерированы до шага 1, поэтому дальнейшее сравнение метрик будет возможно только на новых боях с пересобранным бинарником (шаг 2).

Наблюдение: для `ProtectSelf` pure-move intent per-step оценка не заменяется — это семантически верно (tile safety отличается для каждого промежуточного шага). Фикс специфичен для FocusTarget/ApplyCC, где pursuit-геометрия опирается только на конечную точку.

### Acceptance

Метрики шага 1 в целях — измерение на новых боях (шаг 2).

---

## Шаг 1c. `intent_sum` для Cast-планов — post-cast Move tail

**Триггер**: Step-1b закрыл pure-move ветку (`repeated_tile_rate` 27.5% → 17.1%), но 6/6 остаточных round-trip случаев в новых логах — Cast-планы с post-cast Move tail. Cast-ветка `compute_plan_intent_sum` сохранила per-step `Σ pursuit_move_score × discount^k` — это даёт phantom-tail'у с лишним Move кредит ~+0.58 INT (см. stormborn line 23 в `logs/baseline_20260422_step1b.txt`).

Важный контекст: phantom tail'ы **не исполняются физически** (committed_decision = только Cast/MoveAndCast prefix), но участвуют в скоринге и смещают выбор плана vs альтернатив с тем же префиксом.

### Файл / функция

- `src/combat/ai/planning/scorer.rs:504-549` — `compute_plan_intent_sum`.
- `src/combat/ai/intent.rs:694-709` — `pursuit_move_score` (reused, не меняется).

### Что делать

Расширить terminal-pursuit логику (вариант А из step-1b) на Cast-планы для **tail ПОСЛЕ первого Cast**.

Новая схема обработки плана `steps[0..cast_idx]·Cast·steps[cast_idx+1..]`:

1. **До Cast**: per-step Σ с дисконтом — без изменений (pre-cast Move шаги оцениваются как setup для каста).
2. **Cast step**: per-step intent_score с дисконтом — без изменений.
3. **После Cast**: вместо `Σ intent_score(tail_step_k) × discount^k` — **один вызов**:
   ```rust
   pursuit_move_score(cast_pos, plan.final_pos, target.pos, reach)
   ```
   где `cast_pos` = позиция кастующего в момент Cast (= последний pre-cast Move dest или actor_start если Cast — первый шаг), `reach` = `max_attack_range` или `cc_reach` в зависимости от intent.

   Учёт дисконта: domен итогового post-cast contribution должен быть на уровне одного tail-step, т.е. multiply by `base_discount^(cast_idx+1)`.

4. **goal_achieved latch сохраняется**: если Cast убивает intent target, `goal_achieved = true`, post-cast tail обнуляется (pursuit не нужен — goal solved).

5. **Pure-cast планы (нет Move после Cast)** → ветка не меняется. **Pure-move планы** → step-1b shortcut продолжает работать.

6. **Неприменимо к `ProtectSelf`** — там per-step семантика про tile safety содержательна, pursuit-шорткат неприменим (step-1b установил этот же guardrail).

### Тесты

Добавить в `src/combat/ai/planning/scorer.rs::tests`:

1. `cast_plus_move_tail_collapses_to_single_pursuit` — план `Move→A · Cast(target) · Move→B · Move→C` с final=C. `intent_sum` для post-cast части = `pursuit_move_score(cast_pos, C, target.pos, reach) × discount^(cast_idx+1)`. Длина tail не влияет.

2. `cast_plus_roundtrip_tail_no_credit` — план `Cast · Move→A · Move→start`, final = cast_pos. Post-cast contribution ≈ 0 (либо точно = 0 если pursuit_move_score вернёт 0 для no-displacement, либо negligibly small). Regression pin для line 25 / line 23 из `baseline_20260422_step1b.txt`.

3. `cast_plus_approach_tail_earns_credit` — план `Cast · Move→closer_to_target`. Post-cast contribution положительный. Legitimate case preserved.

4. `cast_kills_then_tail_no_credit` — `Cast(kills target) · Move→A`. `outcomes[cast_idx].killed` содержит target. Post-cast tail обнуляется через `goal_achieved` латч.

5. Regression: `pure_move_chain_intent_equals_single_pursuit` (step-1b test) продолжает проходить — step-1b shortcut для pure-move не нарушен.

### Acceptance

1. `cargo test combat::ai::planning::scorer` — все тесты ok.
2. `cargo test` — 0 failed.
3. **Offline replay**: `cargo run --release --bin replay_ai_log -- logs/20260421T195030_*.jsonl logs/20260421T195059_*.jsonl --metrics-summary` — сравнить с `baseline_20260422_step1b.txt`. Ожидаемо: `repeated_tile_rate` и `post_cast_retreat_rate` упадут (эти метрики сейчас считают полный план; после фикса round-trip-tail планы получат меньший score и проиграют alt'ам).

   **Реалистичный прогноз** (из анализа 2 боёв): из 6 round-trip случаев step-1c прямо закрывает ~1-2 (line 23). Остальные — sanity pipeline + tempo plateau, отдельные проблемы. Ждать `repeated_tile_rate < 5%` не следует; ожидание — ~12–15%.

4. Обновить `docs/ai_rework_plan.md` — пометить шаг 1c как ✅ с divergence и метриками до/после.

### Риск

Низкий-средний. Правка локальная в одной функции. Основной риск — случайно обнулить legitimate "cast then reposition" через слишком агрессивный shortcut. Guard — тест `cast_plus_approach_tail_earns_credit`.

---

## Шаг 2. Replay checkpoint

### Что делать

1. Прогнать полный набор метрик шагов 1/1b на corpus'е.
2. Записать результаты в `logs/baseline_20260422_step1b.txt` (формат как `baseline_20260422_step1.txt`).
3. Принять решение о форме шага 3:
   - Если `killable_non_offensive_rate < 5%` и `kill_conversion_rate > 70%` уже на пост-шаг-1b — шаг 3 делается **мягким** (bias weights, не prune).
   - Если метрики ниже — шаг 3 делается **hard prune** как описано.

Документировать выбор в commit message шага 3.

### Acceptance

Записанный файл + chosen вариант для шага 3 в PR description.

---

## Шаг 2.5. Schema v15 + R5 plumbing

### Файлы / функции

- `src/combat/ai/log.rs` — `SCHEMA_VERSION`, `PlanLogEntry`.
- `src/bin/replay_ai_log.rs` — `LoggedPlan` struct, `evaluation_mode` plumbing.
- `docs/ai-replay.md` — раздел Schema versions.

### Что делать

1. **Bump `SCHEMA_VERSION = 15`** в `log.rs`.
2. Добавить в `PlanLogEntry`:
   - `gate_applied: bool` (default false, serde).
   - `gate_pruned_count: usize`.
   - `survival_mode_active: bool`.
   - `last_stand_active: bool`.
   Либо одним структурированным полем `gate_telemetry: Option<GateTelemetry>`. Выбор — в зависимости от того, сколько полей в итоге набежит.
3. **R5 (plumbing из `ai_rework.md §11` предыдущей итерации — оставалось открытым):** `replay_ai_log` должен читать `evaluation_mode` per-plan из логов и передавать в `apply_protect_self_mask`. Сейчас — заглушка, все планы `Default`. `LoggedPlan` добавить `#[serde(default)] evaluation_mode: EvaluationMode`. Передать в mask.
4. Обновить `docs/ai-replay.md` — добавить v15 в Schema versions.

### Тесты

- `log.rs` unit-test: round-trip сериализации v15-entry → deserialize → поля сохранены.
- `replay_ai_log`: на v14 логах `evaluation_mode` = `Default` для всех планов (serde default), поведение эквивалентно заглушке. На v15 — читает реальный mode.

### Acceptance

`cargo run --bin replay_ai_log -- logs/corpus_20260422/*.jsonl` на v14 логах даёт тот же output, что до bump'а. Новые логи пишут v15.

### Риск

Низкий. Schema bump обратно-совместим через `#[serde(default)]`.

---

## Шаг 3. Tiered killable gate под `FocusTarget`

**Триггер**: baseline_20260422_final показал `killable_non_offensive_rate = 7.7%` (~на цели) но `kill_conversion_rate = 0%` (0/13). Single-predicate gate «non-offensive → `-∞`» закроет только первую метрику; вторую драйвит выбор *offensive-но-не-убивающего* плана при наличии убивающего в пуле. Решение — **стратифицированный** gate по силе kill-line, с intent-coherent detection и keep-set.

Детальная мотивация и alternatives rejected — [`ai_rework.md §3.1`](ai_rework.md#31-policy-under-condition-вместо-отрицательных-весов) / [§3.2](ai_rework.md#32-killable-hard-gate-композирует-с-предыдущими-масками).

### Файл / функция

- **Новый файл**: `src/combat/ai/planning/killable_gate.rs`. Sanity-слой держит инвариант «только soft multiplicative penalties»; hard mask с другой семантикой правильнее изолировать. ProtectSelf mask живёт в `sanity.rs` по историческим причинам — новых mask'ов туда не добавляем.
- `src/combat/ai/planning/mod.rs` — `pub mod killable_gate; pub use ...`.
- `src/combat/ai/utility/ranking.rs` — новое поле `gate_stats` на `PlanRanking`, новый метод `apply_killable_gate`, вызов под guard'ом `if intent == FocusTarget` после `apply_protect_self`.
- `src/combat/ai/utility/mod.rs:362–363` — `write_decision_log` читает `ranking.gate_stats` вместо stub `false/0`.

### Что делать

#### 3.1. Типы

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KillLineStrength {
    #[default]
    None,       // нет kill-line против intent target
    Pressure,   // damage ≥ hp·α у offensive-vs-target плана
    CanFinish,  // kill_now ≥ 1 у offensive-vs-target плана
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GateStats {
    pub applied: bool,       // true если strength != None
    pub strength: KillLineStrength,
    pub pruned_count: usize,
}

// KEEP IN SYNC with src/bin/replay_ai_log.rs::KILLABLE_ALPHA
pub const KILLABLE_ALPHA: f32 = 0.3;
```

#### 3.2. Helper

```rust
/// Matches the semantics of replay_ai_log::plan_is_offensive_vs — plan is
/// offensive vs `target` iff it has ≥1 Cast step whose `target == target`.
/// AoE casts aimed at another tile that happen to cover the target are NOT
/// counted — keeps gate and diagnostic metric in lock-step.
pub fn plan_is_offensive_vs(plan: &TurnPlan, target: Entity) -> bool {
    plan.steps.iter().any(|s| matches!(s, PlanStep::Cast { target: t, .. } if *t == target))
}
```

#### 3.3. Main function

```rust
pub fn apply_killable_gate(
    plans: &[TurnPlan],
    raw: &[PlanFactors],
    scores: &mut [f32],
    modes: &[EvaluationMode],
    intent: &TacticalIntent,
    snap: &BattleSnapshot,
) -> GateStats {
    let TacticalIntent::FocusTarget { target } = *intent else {
        return GateStats::default();
    };
    let Some(t) = snap.unit(target) else { return GateStats::default() };
    let hp_f = t.hp.max(0) as f32;

    // Live pool: survived adaptation + any prior hard mask. Sanity soft
    // penalties leave scores finite → plan stays in consideration.
    let live: Vec<usize> = (0..plans.len())
        .filter(|&i| matches!(modes[i], EvaluationMode::Default))
        .filter(|&i| scores[i].is_finite())
        .collect();
    if live.is_empty() { return GateStats::default() }

    // Strength: intent-coherent detection on the live pool.
    let can_finish = live.iter().any(|&i| {
        plan_is_offensive_vs(&plans[i], target) && raw[i].kill_now >= 1.0
    });
    let has_pressure = live.iter().any(|&i| {
        plan_is_offensive_vs(&plans[i], target) && raw[i].damage >= hp_f * KILLABLE_ALPHA
    });
    let strength = match (can_finish, has_pressure) {
        (true, _) => KillLineStrength::CanFinish,
        (false, true) => KillLineStrength::Pressure,
        _ => return GateStats::default(),
    };

    // Apply keep-set. Only prune indices in `live`; plans already at -∞
    // or in non-Default mode stay untouched.
    let mut pruned = 0usize;
    for &i in &live {
        let keep = match strength {
            KillLineStrength::None => true,
            KillLineStrength::Pressure => plan_is_offensive_vs(&plans[i], target),
            KillLineStrength::CanFinish => {
                plan_is_offensive_vs(&plans[i], target) && raw[i].kill_now >= 1.0
            }
        };
        if !keep {
            scores[i] = f32::NEG_INFINITY;
            pruned += 1;
        }
    }

    GateStats { applied: true, strength, pruned_count: pruned }
}
```

**Два ключевых инварианта в коде** (зеркалят `ai_rework.md §3.1, §3.2`):

- *Live pool* — `mode == Default ∧ scores[i].is_finite()`. Композиция с предыдущими mask-слоями: план, задавленный в `-∞` sanity/adaptation/future mask'ом, автоматически выпадает из strength detection. Sanity soft penalty (multiplicative, finite) — остаётся.
- *Intent-coherent detection* — strength поднимается только если `offensive_vs_target` плана же даёт kn ≥ 1 или dmg ≥ hp·α. Без этого коллатеральный kill в AoE-соседа поднимает strength до CanFinish и gate прунит все non-killing vs intent target — классический «kill_conversion_rate вверх, killable_wrong_target_rate тоже вверх».

#### 3.4. Wiring в `utility/ranking.rs`

```rust
pub struct PlanRanking {
    // ...existing fields...
    pub gate_stats: GateStats,
}

impl PlanRanking {
    // new method, mirrors apply_protect_self style
    pub fn apply_killable_gate(&mut self, plans: &[TurnPlan], ctx: &ScoringCtx) {
        self.gate_stats = apply_killable_gate(
            plans, &self.raw_factors, &mut self.scored,
            &self.adaptation.modes, &self.intent, ctx.snap,
        );
    }
}
```

`pick_action` порядок:

```rust
ranking.apply_viability(...);
ranking.apply_sanity(...);
ranking.apply_adaptation(...);
if matches!(ranking.intent, TacticalIntent::ProtectSelf) {
    ranking.apply_protect_self();
}
if matches!(ranking.intent, TacticalIntent::FocusTarget { .. }) {
    ranking.apply_killable_gate(&plans, &scoring_ctx);
}
```

`ProtectSelf` и `FocusTarget` — взаимоисключающие `TacticalIntent` варианты, маски никогда не пересекаются.

#### 3.5. Telemetry (schema v15 уже готова)

В `utility/mod.rs:362–363` stub заменяется на:

```rust
ranking.gate_stats.applied,          // gate_applied
ranking.gate_stats.pruned_count,     // gate_pruned_count
```

Опционально (можно отдельным мелким PR после step-3): добавить поле `gate_strength: Option<KillLineStrength>` в `PlanLogEntry` через `#[serde(default)]`, чтобы replay мог различать Pressure / CanFinish tier-based срабатывания. Не обязательно для acceptance §5.2.

### Тесты

В `planning/killable_gate::tests`:

1. **`no_kill_line_is_noop`** — пул = heal + reposition, нет dmg≥α·hp, нет kn≥1 → `strength=None`, `pruned_count=0`, scores неизменны.

2. **`pressure_tier_prunes_non_offensive_only`** — offensive план с dmg=0.5·hp (kn=0) + heal-план. Strength=Pressure. После gate: heal=-∞, offensive живой.

3. **`can_finish_tier_prunes_all_non_killing`** — offensive план с kn=1 + offensive-но-weak план (kn=0) + heal. Strength=CanFinish. Gate маскирует heal И weak-offensive, оставляет только killing.

4. **`can_finish_ignores_collateral_kill_line`** ← regression для user-критики #2. План A: `Cast @ other_enemy` kn=1 (collateral kill). План B: `Cast @ intent_target` dmg≥α·hp kn=0. План C: heal. Strength должна упасть до Pressure (A не `offensive_vs_target`), B сохраняется, C маскируется. Не CanFinish, не forcing A.

5. **`pressure_ignores_collateral_damage`** — симметрично предыдущему: план A `Cast @ other` dmg=0.5·hp, план B heal, план C reposition-offensive-at-intent (если такой possible). Strength=None (никакой plan не coherent pressure). Gate no-op.

6. **`gate_ignores_plans_already_masked_by_prior_layer`** ← regression для user-критики #1. Два плана: (a) `Cast @ intent_target kn=1`, но `scores[0] = -∞` (замаскирован); (b) offensive kn=0, score=0.5. Без `.is_finite()` фильтра: strength=CanFinish → b тоже `-∞` → пул всех `-∞`. С фиксом: (a) не в live_pool → strength падает до Pressure (если b даёт dmg≥α·hp) или None → b остаётся живым.

7. **`gate_respects_last_stand_mode`** — план killing-но-AoO-lethal: adaptation уже flipped `mode=LastStand`. Gate его не видит (`live` фильтрует). Другой план defensive под Default остаётся живым, не маскируется CanFinish (плана с kn≥1 в live нет → strength=None).

8. **`gate_disabled_under_apply_cc`** — `intent=ApplyCC`. Early return `let FocusTarget = intent else return`. Gate no-op даже при kn≥1.

9. **`gate_disabled_under_protect_self`** — `intent=ProtectSelf`. В `ranking.rs` guard не вызывает gate. (Проверяем на уровне `ranking.apply_killable_gate` через guard-контракт, не внутри самой функции.)

**Regression**: 
- `planning::sanity::tests::*` — не задевается (gate в отдельном файле).
- `planning::adaptation::tests::*` — не задевается.
- `utility::ranking::tests::apply_protect_self_*` — pipeline порядок не изменил их контекст.

### Acceptance

См. [`ai_rework.md §5.2`](ai_rework.md#52-шаг-3-killable-gate--acceptance). Все conjuncts (AND), не OR.

**Измерение**: пересобрать, перезапустить те же 2 сценария (`demo_beastblood_raid`, `demo_stormborn_camp`) на hard difficulty с теми же seeds. Прогнать replay на свежих логах:

```
cargo run --release --bin replay_ai_log -- logs/corpus_20260422_post_step3/*.jsonl --metrics-summary
```

Сравнить с `logs/baseline_20260422_final.txt`. Ожидаемые дельты:

- `killable_non_offensive_rate`: 7.7% → < 2% (минимум 1 из 13 non-offensive → offensive).
- `kill_conversion_rate`: 0% → > 85% (CanFinish tier форсит `kn ≥ 1` среди offensive).
- `killable_wrong_target_rate`: ~7.7% → **≤ baseline** (guard: не должна расти; если поднялась — strength detection ловит collateral, regression).
- `repeated_tile_rate`, `zero_net_move_rate`, `post_cast_retreat_rate`: Δ ≤ 5 pp (не в scope шага).
- `phantom_tail_chosen_rate / flips`: Δ ≤ 5 pp (не в scope — Phase 7).

### Риск

Средний.

- **Forcing bad kill-plan**: если sanity задавила единственный killing-план, gate всё равно поднимает strength до CanFinish и прунит альтернативы. Осознанный выбор (см. §3.2 в `ai_rework.md`): contract побеждает soft penalty. Guard — `false_gate_rate < 3%`. Если метрика высокая — сигнал добавить relative-score threshold в Phase 7.
- **Collateral kill/damage**: закрыт intent-coherent detection (`offensive_vs_target` в обоих условиях). Regression-тесты #4, #5.
- **Interactions с future masks**: `.is_finite()` фильтр делает gate composable с любым будущим слоем, который пишет `-∞`. Regression-тест #6.
- **Multi-cast план с Cast @ other на prefix**: известное ограничение, задокументировано в `ai_rework.md §3.2a`. Phase 7 territory; если replay покажет частоту > 5% — выделить отдельный issue.

---

## Шаг 4. Split `self_survival` / `ally_rescue`

### Файлы / функции

- `src/combat/ai/factors/survival.rs` — сейчас вычисляет единую `self_survival` ось.
- `src/combat/ai/factors/mod.rs:144+` — `NUM_FACTORS`, `*_IDX` константы.
- `src/combat/ai/role.rs` — `AXIS_FACTOR_WEIGHTS` (добавить столбец для `ally_rescue`).
- `src/combat/ai/intent.rs:910-925` — `TacticalIntent::ProtectSelf` intent_score.
- `src/combat/ai/planning/sanity.rs:276+` — `apply_protect_self_mask`.
- `src/combat/ai/log.rs` — schema v16.

### Что делать

1. **Факторный layout.** `NUM_FACTORS = 11`, добавить `ALLY_RESCUE_IDX = 10`. `PlanFactors::ally_rescue: f32`.
2. **Вычисление.**
   - `self_survival` — **только** self-directed: heal_self, armor_self, exit_aoo, distance_from_threat. Ally-эффекты из формулы убираются.
   - `ally_rescue` — новая функция в `factors/survival.rs` (или отдельный файл `factors/ally_rescue.rs`): heal на союзника, buff на союзника, taunt-redirect. Агрегация — discounted sum.
3. **`AXIS_FACTOR_WEIGHTS`** — колонка для `ally_rescue`: Tank 0.3, Melee 0.5, Ranged 0.3, Control 0.4, Support 1.2.
4. **ProtectSelf ε-gate.** В `apply_protect_self_mask` проверка:
   ```rust
   if plan.factors.self_survival < EPS_SELF { scores[i] = f32::NEG_INFINITY; }
   ```
   `EPS_SELF = 0.15` (≈ 15% max_hp эквивалент). Ally-rescue **не зачитывается** в этот порог.
5. **Schema v16 bump** — новое поле `ally_rescue: f32` в `PlanLogEntry::raw_factors`. Старые логи — `#[serde(default)]`, получают 0.
6. **Замапить старые тесты.** `adaptation.rs` / `intent.rs` тесты на ProtectSelf → проверить, что self-heal планы проходят, ally-heal планы (без self-AoE) — fail gate.

### Тесты

- `factors/survival::tests::self_heal_raises_self_survival_only`: план с `Cast heal(self)`. `self_survival > 0`, `ally_rescue = 0`.
- `factors/ally_rescue::tests::ally_heal_raises_ally_rescue_only`: план с `Cast heal(ally)`. `self_survival = 0`, `ally_rescue > 0`.
- `factors/survival::tests::aoe_heal_self_in_zone_raises_both`: AoE heal, caster в зоне. Обе оси положительные.
- `sanity::tests::protect_self_eps_gate_blocks_ally_only_heal`: ProtectSelf intent, план с `Cast heal(ally)`, `self_survival = 0`. Должен получить `-inf`.
- `sanity::tests::protect_self_eps_gate_passes_self_heal`: ProtectSelf intent, план `Cast heal(self)`. `self_survival ≥ ε`, сохраняется.

### Acceptance

См. [`ai_rework.md §5.3`](ai_rework.md#53-шаг-4-selfally-split--acceptance).

### Риск

Высокий — трогает осевой layout, `AXIS_FACTOR_WEIGHTS`, intent.rs, sanity pipeline. Schema bump. Нужен полный replay-corpus перед merge. Guard: Δ метрик шагов 1–3 ≤ 5%.

---

## Шаг 5. Summon: saturation axis + intent-specific credit filter

### Файлы / функции

- `src/combat/ai/factors/saturation.rs` — расширение.
- `src/combat/ai/factors/mod.rs` — возможно новая ось или расширение существующей.
- `src/combat/ai/intent.rs:866-905` — `IntentWeights` / intent_score.
- `src/combat/ai/planning/scorer.rs:311-352` — `plan_summon_bonus`.

### Что делать

1. **Saturation axis расширить на summon.** Сейчас `factors/saturation.rs:22-63` считает `buff_saturation_penalty` через `buff_class`. Добавить `summon_saturation_penalty` — `-0.4` за каждого живого summon'а того же template'а у актора. Либо вынести обе в общий `saturation_axis` value.
   - Входы: `ability` (чтобы понять, что это `EffectDef::Summon { template, .. }`), `snap`, `caster: Entity`.
   - Формула: `active_count = snap.units.filter(|u| u.summoner == Some(caster) && u.template == template && u.is_alive()).count()`. Penalty = `-0.4 × active_count`.
2. **Intent-specific credit filter.** В `intent_score` (`intent.rs:850+`) добавить правило для `FocusTarget / ApplyCC / ProtectAlly`:
   ```rust
   if let Some((_, _, cast_target)) = cast {
       if cast_target != intent.target().unwrap_or(Entity::PLACEHOLDER) {
           // For these intents, cast must be vs intent target to earn intent credit
           return 0.0;
       }
   }
   ```
   `SetupAOE`, `ProtectSelf`, `LastStand`, `Reposition` — правило **не** применяется (у них нет single-target или target ∈ allies).
3. **`plan_summon_bonus` калибровка.** Сейчас `saturation_mult = 0.65^total_allies` — это global, не per-template. Оставить как coarse bound, добавить per-template через шаг 1 saturation axis (они умножатся).

### Тесты

- `factors/saturation::tests::summon_saturation_per_template`: актор с 2 живыми storm_spirit того же template → penalty = -0.8.
- `factors/saturation::tests::different_templates_independent`: 2 storm_spirit + 1 другой template → penalty только от storm_spirit'ов.
- `intent::tests::summon_no_credit_under_focus_target`: `FocusTarget(enemy_T)`, план `Cast summon_X → Move`. intent_score для Cast-шага = 0.
- `intent::tests::summon_earns_credit_under_setup_aoe`: `SetupAOE`, план `Cast summon_X`. intent_score сохраняется.

### Acceptance

См. [`ai_rework.md §5.4`](ai_rework.md#54-шаг-5-summon--acceptance). Дополнительно — regression test для legitimate buff-стэка (haste + armor на одном target'е) не должен триггерить новые penalty.

### Риск

Средний. Правка в `intent_score` затрагивает большой объём поведения. Необходим полный replay-corpus.

---

## Порядок и параллелизм

```
0 → 0.5 ──┐
           ├─→ 1 ─→ 1b ─→ 1c ─→ 2 (checkpoint) ─→ 2.5 ─→ 3 ─→ 4 ─→ 5
```

- **Последовательность обязательна.** Шаги 3–5 зависят от стабилизированного tempo (шаги 1/1b/1c) и schema v15 (шаг 2.5).
- **Параллельно можно:** пока 1 в измерении, готовить 2.5 (schema bump) как отдельный PR.
- **Не спешить с 4 и 5.** Между ними прогнать corpus минимум один раз, убедиться что шаг 3 стабилен.
- **Phase 7 — параллельная опция** (см. ниже). Может идти в own worktree пока текущая итерация завершается.

---

## Phase 7 (следующая итерация). Prefix + FutureValue scoring

**Статус**: предложение, не начато. Концептуально — замена фундамента plan scoring'а, не инкрементальная правка.

### Мотивация

Из анализа `baseline_20260422_step1b.txt`: beam search log'ит 3+ шаговые планы, но committed_decision — это только prefix (`CastInPlace` / `MoveAndCast` / `MoveOnly`). Post-prefix tail **физически не исполняется**, но смещает scoring → phantom-tail bias. Step-1c адресует это локально для intent_sum, но:

- `tempo_gain` всё ещё смотрит на `plan.final_pos` (включая phantom tail).
- `self_survival` — то же самое.
- Plateau от `pursuit_move_score` step-function (`0.8` для всего attack range) даёт кучу одинаково-оценённых планов, которые разрешаются через top-K RNG.

Architecturally правильное решение — отказаться от скоринга полного beam-плана и перейти на:

```
Score(plan) = PrefixScore(committed_prefix) + γ · FutureValue(committed_state)
```

где `FutureValue` — value-of-state оценка (cheap one-ply surrogate от committed_pos: best future position, best future attack, mobility), не зависящая от конкретного хвоста.

### Зачем отдельной итерацией

Это НЕ замена step-1c. Step-1c — 50 строк, точечно чинит один symptom. Phase 7 — замена pipeline:

- Перекалибровка всех `AXIS_FACTOR_WEIGHTS` под новую декомпозицию.
- Schema bump (raw_factors layout меняется).
- Перепроектирование `sanity_adjust_plans` + `apply_protect_self_mask` + `apply_killable_gate` под prefix-based signals.
- Все текущие тесты, пиннящие raw_factors значения, ломаются.
- Новый набор метрик: `committed_decision_quality` и т.п. (текущие `repeated_tile_rate` теряют смысл, если tail не скорится).

### Предварительный scope

- `future_value_from_committed_state(actor, committed_pos, snap, maps)` — использует существующие `evaluate_position` (position_eval.rs), `target_priority`, `score_action`. Bfs по reachable_next_turn от committed_pos; max/avg по future-position / future-attack / mobility.
- `PrefixScore` — то, что истинно после committed action: damage_now/kill_now/cc/heal из actual Cast-outcome + position-eval в committed_pos.
- `γ = 0.25` как starting point, λ_safe/press/mob/setup — перекалибровать через replay-corpus.
- Phase 6 решение об удалении position/risk/focus axes **не отменяется** — `evaluate_position` используется как internal helper в FutureValue, не как самостоятельная ось.

### Preconditions (что должно быть сделано ДО Phase 7)

1. Текущая итерация (step-1 → 1c → 2 → 3 → 4 → 5) завершена и задеплоена.
2. **Offline prototype**: написать `future_value_from_committed_state` как pure function, replay на corpus offline (без изменения production scoring), сравнить ranking с текущим. Если FutureValue даёт measurable differentiation в plateau-tied случаях — есть смысл. Если нет — revisit подход.
3. Замер plateau на corpus: сколько top-K entries имеют `max - min < ε`. Если <5% — Phase 7 имеет низкий приоритет, phantom-tail достаточно лечится через step-1c + точечные фиксы.

### Риск

Очень высокий. Затрагивает основной scoring pipeline, schema, все sanity-gates, все calibrated weights. В одном PR не делается — нужна разбивка минимум на 3-4 шага с своими checkpoint'ами.

### Документация

Детальный design-doc пишется отдельно (`docs/ai_scoring_prefix_plus_future.md`), когда итерация будет готова стартовать. В этом плане — только stub для tracking.

---

## Вне scope

- Полная канонизация факторов (следующая итерация).
- Marginal board value для summon'ов (технический долг).
- Trade economy, difficulty knobs, sanity pipeline calibration — изолированы.
- Intent selection (`select_intent`) — меняем только scoring, не выбор.
- Plateau-ties от step-function `pursuit_move_score` (0.8 flat в attack range) — отдельный bug, не scope текущей итерации. Возможно лечится через Phase 7 FutureValue differentiation или через smooth closing в `pursuit_move_score`. Пока зафиксировано в `logs/baseline_20260422_step1b.txt` как известный артефакт.

Детали — [`ai_rework.md §6`](ai_rework.md#6-что-вне-scope).
