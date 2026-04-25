# Шаг 3 — Appraisal / Need layer: декомпозиция на сабшаги

Декомпозиция в стиле фаз 2a / step 4: коммит-на-сабшаг, gate на каждом.
Спецификация: `docs/ai_rework.md` §3 + `docs/ai_need_signals.md` (входная спецификация патологий и таргетов).

## Preamble

**Текущее состояние смысловой оценки.** «Насколько нужно действие» размазано по трём слоям:

- `intent.rs::select_intent` (`intent.rs:355`) — лестница условий с эвристическими порогами: `hp_pct < hp_panic && danger > danger_panic` для panic, `hp_pct < 0.4 && danger > 0.0` для ProtectSelf, `cluster_count > 0` для AOE, `pos_eval < repo_threshold` для Reposition. Все пороги читают raw `BattleSnapshot`/`InfluenceMaps`, без агрегации в нормализованный «срочность».
- `factors/*` — пересчитывают похожие факты под другим углом (offensive smoothness, survival quadratic, scarcity).
- Stickiness через `AiMemory.last_intent` уже работает, но как scalar bonus, не как need-signal.

**Mining-сводка** (`docs/ai_need_signals.md`, mining 36 логов / 761 решений):

| Патология | Сейчас | Таргет post-3 | Нужный сигнал |
|---|---|---|---|
| FocusTarget-switches с живой целью ≥50% HP | 7.3% (FT→FT) | ≤2.5% | `continue_commitment` (P1) |
| Panic на стабильном HP (\|Δ\|<5%) | 29% panic'ов | ≤10% | `self_preserve + recent_damage_taken` (P2) |
| Depth-0 при `actor_ap ≥ 1` | 6.2% всех | ≤1.0% | `reposition` (P3) |
| Reposition как chosen intent | 0.4% | 3–5% | `reposition` (P6) |
| `actor_hp_drop` divergence | 21.6% div'ов | ≤12% | `self_preserve` front-load (P5) |

**Natural seam appraisal ↔ consumers.** `pick_action` (`utility/mod.rs`) — единственное место, где у нас уже собран весь контекст: `world` (snap+maps+tuning), `memory`, `caster_ctx`. Producer `compute_need_signals(...)` зовётся **один раз** в начале `pick_action` после реcurrents, перед `select_intent`. Результат — поле `NeedSignals` в `ScoringCtx`, читаемое downstream.

**Что НЕ в scope step 3:**
- Bands+agenda+scorecard intent (это step 11, поверх need layer).
- Migration `compute_factors` на NeedSignals (откладывается до step 11 / 5 — там нужен terminal eval).
- Critics decomposition (step 10).
- Team blackboard для точного `damage_already_dealt_to_last_target` (step 13). До тех пор — эвристика «единственный атакующий по этой цели → max_hp - hp».
- Semantic AI tags для `has_defensive_ability` (step 9). До тех пор — whitelist способностей в `appraisal/`.

**Зафиксированные решения по 4 развилкам (см. обсуждение перед началом):**

1. **Response curves data-driven с самого начала** (1a). Минимальный `ResponseCurve` enum (`Logistic`, `LinearClamped`) в `AiTuning.curves` уже в 3.0. TOML balance, никакой захардкоженной математики.
2. **AiMemory расширение в 3.0** — bump `SCHEMA_VERSION` v19→v20, поля добавляются, `update_memory` пишет, никто не читает.
3. **Все 5 mineable need signals в первой волне** (3a) — `continue_commitment`, `self_preserve`, `finish_target`, `reposition`, `conserve_resource`. `rescue_ally` / `apply_cc` / `setup_aoe` остаются `0.0` в `NeedSignals` до второй итерации mining'а (`ai_need_signals.md:166`).
4. **Phased rewire `select_intent`** (4a) — точечная замена эвристик на need-сигналы при сохранении лестницы. Bands+agenda — отдельно в step 11.

**Природа gate'ов в step 3 — отличается от step 4.**

В step 4 golden 0/131 diff был жёстким gate: outcome vector — структурный рефакторинг, ничего не меняется. В step 3 **сами решения должны двигаться** — это и есть цель шага. Поэтому:

- **3.0–3.1** (scaffolding + producer без consumers): golden 0/131 diff, gate как в step 4.
- **3.2–3.5** (consumers): golden diff допустим, но **каждый diff** разбирается per-entry и соответствует ожидаемому сдвигу из mining'а. Если diff — деградация по таргетам, а не движение к ним: откат сабшага.
- **3.6** (cleanup): rebaseline golden (`logs/golden_pre_step4.jsonl` → `logs/golden_post_step3.jsonl`) для будущих шагов.

Реальные gate-метрики — mining-таблица из `ai_need_signals.md:184` и scenario harness. После 3.6 повторный прогон `mine_ai_logs` на новом наборе и сверка с таргетами.

## Сабшаги

### 3.0. Scaffolding: `ResponseCurve` + `appraisal/` + `AiMemory` v19→v20 ✓ DONE

**Scope.**

- Новый модуль `src/combat/ai/tuning/curves.rs` (либо inline в `tuning.rs`):
  ```rust
  pub enum ResponseCurve {
      Logistic { mid: f32, k: f32 },                 // ceiling = 1.0 implied; eval = 1/(1+exp(-k*(x-mid)))
      LinearClamped { x_lo: f32, x_hi: f32 },        // eval = clamp((x - x_lo)/(x_hi - x_lo), 0, 1)
  }
  impl ResponseCurve { pub fn eval(&self, x: f32) -> f32; }
  ```
  Никакого «exponential decay»/«power»/«piecewise» — на step 3 нужны ровно эти две формы (см. `ai_need_signals.md:155`). `ai_rework_plan.md:373` (фаза 2b) расширит `ResponseCurve` enum по запросу.

- Новый модуль `src/combat/ai/appraisal/mod.rs`:
  ```rust
  #[derive(Debug, Clone, Default, Serialize, Deserialize)]
  pub struct NeedSignals {
      pub self_preserve: f32,
      pub rescue_ally: f32,        // 0.0 в первой волне
      pub finish_target: f32,
      pub apply_cc: f32,           // 0.0 в первой волне
      pub setup_aoe: f32,          // 0.0 в первой волне
      pub reposition: f32,
      pub conserve_resource: f32,
      pub continue_commitment: f32,
  }
  pub fn compute_need_signals(...) -> NeedSignals { Default::default() } // stub в 3.0
  ```

- Расширение `AiTuning`:
  ```toml
  [curves]
  self_preserve_hp        = { kind = "logistic",       mid = 0.5, k = 8.0 }
  self_preserve_dmg_alpha = 0.6                                            # multiplier scalar, не curve
  continue_commitment_hp  = { kind = "logistic",       mid = 0.4, k = -10.0 }  # k<0 → высокий при hp ≥ mid
  finish_target_kill      = { kind = "logistic",       mid = 0.6, k = 6.0 }
  reposition_pos_gain     = { kind = "linear_clamped", x_lo = 0.05, x_hi = 0.5 }
  conserve_resource       = { kind = "logistic",       mid = 0.3, k = -10.0 }
  ```
  Стартовые параметры — best-guess, тюнятся в 3.6 после mining'а. Хардкод в коде запрещён.

- Расширение `AiMemory` (`intent.rs:200`):
  ```rust
  pub struct AiMemory {
      // existing fields...
      pub hp_ratio_at_last_turn: f32,           // default 1.0 при init
      pub last_turn_was_defensive: bool,
      pub turns_in_low_hp: u8,
      pub damage_dealt_to_last_target: i32,     // эвристика через max_hp - hp до team blackboard
  }
  ```
  `update_memory` (`intent.rs:637`) пишет все 4 поля. Зеркало в `AiMemorySnapshot` (`log.rs`) — `#[serde(default)]` для backward compat.

- `SCHEMA_VERSION` 19 → 20 в `log.rs` с записью в истории: «v19→v20: AiMemory расширен 4 полями (hp_ratio_at_last_turn, last_turn_was_defensive, turns_in_low_hp, damage_dealt_to_last_target) для appraisal/need layer».

- В `pick_action` (`utility/mod.rs`) — поле `need_signals: NeedSignals` в `ScoringCtx`, заполняется заглушкой `Default::default()` через stub из `appraisal/mod.rs`. Никто пока не читает.

**Gate.** `cargo test/clippy`, `ai_scenarios`, golden **0 / 131 diff**. Никто не читает, ничего не изменилось. Unit-тест `ResponseCurve::eval` на logistic/linear-clamped границах.

**Эстимейт:** 1.0 день.

**Изменения от плана при реализации:**
- `AiMemory` расширен на **3** поля (не 4) — `damage_dealt_to_last_target` дропнут, поскольку `max_hp - hp` of `last_target` тривиально вычисляется в 3.1 producer'е напрямую из снапшота. Убрали dead-code поле.
- `hp_ratio_at_last_turn: Option<f32>` (не `f32` с default 1.0) — `None` = первый ход, чтобы producer 3.1 мог различить «нет prior turn data» vs «было полное HP».
- Добавлено `Thresholds.low_hp_zone_threshold: f32` (default 0.4) — порог для счётчика `turns_in_low_hp`.

**Коммит:** `36c3d18`. **Golden-replay:** 0 / 131 diff.

### 3.1. Producer: `compute_need_signals` для всех 5 mineable

**Scope.**

Реализовать `compute_need_signals(active, snap, maps, memory, caster_ctx, tuning) -> NeedSignals`. Все 5 mineable считаются по формулам ниже, остальные 3 (`rescue_ally`, `apply_cc`, `setup_aoe`) остаются `0.0` с TODO-комментарием.

**Формулы (точные).** Все curves читаются из `tuning.curves` через `ResponseCurve::eval`.

**self_preserve.** `urgency_hp = curve(self_preserve_hp).eval(1.0 - active.hp_pct())`. Множитель за свежий урон: `dmg_mult = 1.0 + tuning.curves.self_preserve_dmg_alpha * recent_damage_taken`, где `recent_damage_taken = (memory.hp_ratio_at_last_turn - active.hp_pct()).max(0.0)`. Гасящий фактор: если `memory.last_turn_was_defensive && recent_damage_taken < 0.05` — `dmg_mult *= 0.5`. Итог: `signals.self_preserve = (urgency_hp * dmg_mult).clamp(0, 1)`.

**continue_commitment.** Если `memory.last_intent ∉ {FocusTarget, ApplyCC}` или `memory.last_target.is_none()` — `0.0`. Иначе:
- `last_target = memory.last_target.and_then(|id| snap.unit_by_id(id))`. Если `None` (умерла или ушла) — `0.0`.
- `last_target_hp = last_target.hp_pct()`. Если `last_target_hp <= 0.25` — `0.0` (финишер ≠ abandon, см. `ai_need_signals.md:24`).
- `reachable = active.pos.unsigned_distance_to(last_target.pos) <= active.speed + active.max_attack_range`. Если `false` — `0.0`.
- `signals.continue_commitment = curve(continue_commitment_hp).eval(last_target_hp)` — logistic с `k<0`, плавное плато 0.6–0.8 в hp-диапазоне 0.3–0.7.

**finish_target.** `killability_targets = enemies.filter(|e| active.threat >= e.eff_hp() && reachable)`. Для топ-1 (по убыванию `1.0 - hp_pct()`): `signals.finish_target = curve(finish_target_kill).eval(1.0 - target.hp_pct())`. Если `last_target` совпадает с killable и `target_damage_taken_since_last_turn > 0` — bonus +0.2 clamp 1.0.

**reposition.** Несколько входов:
- `best_position_improvement` = max(`evaluate_position(tile) - evaluate_position(active.pos)`) среди `tile ∈ reachable_tiles_within(active.pos, active.speed)`. Берём топ-3 (BFS limit 19 hex для speed=3 — см. hex-grid). Без полного BFS, чтобы не O(n²) — limit AP-budget 1.
- `engagement_gap = enemies.iter().all(|e| active.pos.unsigned_distance_to(e.pos) > active.max_attack_range)`.
- `has_ap = active.action_points >= 1`.
- `signals.reposition = curve(reposition_pos_gain).eval(best_position_improvement)`. Если `engagement_gap && has_ap` — `signals.reposition = max(signals.reposition, 0.5)` (idle AP boost, см. `ai_need_signals.md:67`).

**conserve_resource.** `mana_ratio = active.mana as f32 / active.max_mana.max(1) as f32`. `signals.conserve_resource = curve(conserve_resource).eval(mana_ratio)` — logistic `k<0`, высокий при low mana.

**Stub'ы для нереализованных** (`rescue_ally`, `apply_cc`, `setup_aoe`): `// TODO step 3 v2 — нужны input'ы из team blackboard / outcome vector` + `0.0`.

**Где звать.** В `pick_action` (`utility/mod.rs`) — после построения `world`/`memory` контекста, перед `select_intent`. Результат прокидывается через `ScoringCtx.need_signals: &NeedSignals` (по той же модели, что `tuning: &AiTuning`).

**Gate.**
- Unit-тесты per signal:
  - `self_preserve_zero_at_full_hp`, `self_preserve_max_at_low_hp_with_damage`, `self_preserve_dampened_after_defensive`.
  - `continue_commitment_zero_when_target_dead`, `continue_commitment_zero_when_target_low_hp` (финишер), `continue_commitment_high_when_alive_50pct`.
  - `finish_target_high_for_killable_low_hp`, `finish_target_zero_when_no_threat`.
  - `reposition_high_when_engagement_gap_and_has_ap`, `reposition_zero_when_engaged_and_position_good`.
  - `conserve_resource_high_at_low_mana`, `conserve_resource_low_at_full_mana`.
- Golden **0 / 131 diff** — никто не читает.

**Эстимейт:** 1.5 дня.

### 3.2. Consumer: `self_preserve` → `select_intent::ProtectSelf`

**Scope.**

Заменить две точки в `intent.rs::select_intent`:

**Точка 1 — panic override (`intent.rs:394`).**
```rust
// БЫЛО:
if hp_pct < hp_panic && danger > danger_panic { return ProtectSelf; }
// СТАНЕТ:
if need_signals.self_preserve >= tuning.thresholds.panic_self_preserve_threshold
   && danger > danger_panic
{ return ProtectSelf; }
```
`panic_self_preserve_threshold` — новое поле в `Thresholds`, default 0.85 (≈ старая комбинация `hp_pct < 0.20 && danger > 0.6` даёт `urgency_hp ≈ 0.86` при logistic).

**Точка 2 — soft ProtectSelf (`intent.rs:408`).**
```rust
// БЫЛО:
if hp_pct < 0.4 && danger > 0.0 {
    let urgency = (1.0 - hp_pct) * danger;
    consider(ProtectSelf, urgency, ...);
}
// СТАНЕТ:
if need_signals.self_preserve > 0.2 && danger > 0.0 {
    let urgency = need_signals.self_preserve * danger;
    consider(ProtectSelf, urgency, ...);
}
```

`IntentReason::PanicOverride` / `Urgency` — расширить полем `self_preserve: f32` для diagnostics.

**Gate.**
- `cargo test/clippy`, `ai_scenarios`. Сценарий `bell_crypt::p008_bell_bound_retreat_low_hp` (protect-self at 7% hp) должен остаться зелёным; `twisted_grove::p013_iskazhenny_last_stand_trade` — тоже.
- Golden diff допустим. Запустить `replay_ai_log --compare-golden logs/golden_pre_step4.jsonl logs/20260424T*.jsonl` и **per-entry разобрать** все diff'ы. Каждый diff:
  - либо panic'и на стабильном HP, которые исчезли (целевая патология P2) — записать в `commit message`,
  - либо новые ProtectSelf'ы при свежем уроне без low-hp (P5 «front-load») — тоже целевые,
  - либо неожиданные деградации (например, ProtectSelf исчез там, где раньше срабатывал по `hp < 0.4 && danger > 0`) — fix params.
- Если diff > ~15% от 131 → tunes curves в `ai_tuning.toml`, не код.

**Эстимейт:** 1.0 день.

### 3.3. Consumer: `continue_commitment` → stickiness в `select_intent::consider`

**Scope.**

В `intent.rs::select_intent::consider` (внутренняя closure, `intent.rs:367`):
```rust
// БЫЛО:
if memory.turns_committed < t.max_committed_turns
   && memory.last_intent == Some(intent.kind())
{
    s += t.stickiness_bonus;
    if same_target { s += t.target_stickiness_bonus; }
}
// СТАНЕТ:
if memory.turns_committed < t.max_committed_turns
   && memory.last_intent == Some(intent.kind())
{
    s += t.stickiness_bonus * need_signals.continue_commitment;
    if same_target { s += t.target_stickiness_bonus * need_signals.continue_commitment; }
}
```

Логика: текущий stickiness — flat scalar; новый — модулируется силой commitment-сигнала. Если старая цель умерла / ушла из досягаемости / на 1HP — `continue_commitment ≈ 0`, stickiness гасится. Жива и здорова — `continue_commitment ≈ 0.7`, stickiness работает почти как сейчас.

**Gate.**
- Сценарий `twisted_grove::p019_dvoynik_monotone_focus` (continuation на ту же цель) — должен **усилиться** или остаться. Если ослаб — fix curve.
- Mining-метрика `FT-switches с живой целью ≥50% HP` ожидается падение к ≤2.5%. Снять после 3.6.
- Golden diff допустим, per-entry анализ как в 3.2.

**Эстимейт:** 1.0 день.

### 3.4. Consumer: `reposition` → `select_intent::Reposition`

**Scope.**

В `intent.rs::select_intent` (`intent.rs:548`):
```rust
// БЫЛО:
let pos_eval = evaluate_position(active.pos, &active.role, tuning, maps);
let repo_threshold = difficulty.awareness_reposition_threshold();
if pos_eval < repo_threshold {
    let repo_score = 0.3 + (repo_threshold - pos_eval).min(1.5) * 0.4;
    consider(Reposition, repo_score, ...);
}
// СТАНЕТ:
if need_signals.reposition > 0.1 {
    let repo_score = 0.3 + need_signals.reposition * 0.7;
    consider(Reposition, repo_score, ...);
}
```

`pos_eval`/`repo_threshold` остаются вычисляться внутри `compute_need_signals` (это ингредиенты `best_position_improvement`).

**Ожидаемый сдвиг:** depth-0 при `actor_ap ≥ 1` падает с 6.2% к ≤1.0% (P3). Reposition вырастает с 0.4% до 3–5% (P6).

**Риск:** Reposition может начать «забивать» ProtectSelf в low-HP сценариях, потому что при low HP + engagement_gap оба сигнала высокие. Защита через `IntentReason::Urgency` приоритет — survival_quadratic в sanity всё ещё гасит non-defensive ProtectSelf при low HP.

**Gate.**
- Все 9 текущих сценариев `ai_scenarios` зелёные.
- Golden diff: новые Reposition entries вместо «empty plan + viability_fallback» — целевые. Проверить per-entry: ни одна из текущих выигрышных decisions не превратилась в Reposition. Если превратилась — curve params слишком агрессивные.

**Эстимейт:** 1.0 день.

### 3.5. Consumer: `finish_target` + `conserve_resource`

**Scope.**

**`finish_target`** в `intent.rs::select_intent` (`intent.rs:484`):
```rust
// БЫЛО:
if let Some(target) = killable {
    let kill_score = 1.2 + (1.0 - target.hp_pct()) * 0.3;
    consider(FocusTarget { target }, kill_score, IntentReason::Killable {...});
}
// СТАНЕТ:
if let Some(target) = killable {
    let kill_score = 1.2 + need_signals.finish_target * 0.3;
    consider(FocusTarget { target }, kill_score, IntentReason::Killable {...});
}
```

**`conserve_resource`** — penalty в `consider` для intent'ов с дорогими планами, и boost для cheap actions. Точка применения сложнее: scoring дорогих планов происходит в `factors`, не в `select_intent`. На step 3 ограничиваемся **soft-application через intent score**:

```rust
// В select_intent, после consider всех intent'ов:
if need_signals.conserve_resource > 0.5 {
    // Низкий resource pressure → cheap-friendly intents получают bonus
    // FocusTarget/ApplyCC c CAN_CHEAP_ATTACK → +0.1 * conserve_resource
    // ProtectSelf, Reposition (cheap actions) → +0.15 * conserve_resource
}
```

Жёсткий budget-aware scoring на factors level откладывается — это step 11 (scorecard).

**Gate.**
- Mining-метрика `viability_fallback` (P4) — ожидается частичное падение, потому что reposition (3.4) уже забрал часть кейсов; conserve_resource добивает оставшиеся.
- Golden diff: проверить что finish_target target ≠ старая цель (когда свежий урон выбил последний HP) — целевой случай. Иначе — curve переоценивает recent damage.

**Эстимейт:** 1.0 день.

### 3.6. Cleanup + повторный mining + rebaseline golden

**Scope.**

- Sync docstring §3 в `docs/ai_rework.md`: список входов и выходов NeedSignals совпадает с реальным API `compute_need_signals`. Помеченные в `ai_need_signals.md:174` правки внести.
- Mining: прогон `cargo run --release --bin mine_ai_logs -- --dir logs/` на новом наборе **post-step-3 plays** (минимум 5 свежих логов). Сверить с таргетами `ai_need_signals.md:184`. Закоммитить обновлённую таблицу с baseline → actual в `ai_need_signals.md`.
- Если метрики не сдвинулись или ушли не туда: вернуться к 3.2–3.5, тюнить curves в `ai_tuning.toml`. Не идти в step 5, пока gate не пройден.
- Rebaseline golden: `logs/golden_pre_step4.jsonl` → `logs/golden_post_step3.jsonl`. Удалить старый, закоммитить новый.
- Удалить TODO-комментарии в `appraisal/mod.rs` для `rescue_ally`/`apply_cc`/`setup_aoe` — заменить на ссылку «откладывается до второй итерации mining'а, см. `ai_need_signals.md:166`».
- Обновить `docs/ai_rework_plan.md` §«Волна 1 — обновлённая последовательность»: `3 ✓ → 5`.

**Gate.**
- Mining-таблица: каждая из 5 метрик из `ai_need_signals.md:184` либо достигла таргета, либо движется к нему (минимум 50% пути). Если меньше — не закрываем шаг.
- `cargo test/clippy`, `ai_scenarios`. Golden — новый baseline зафиксирован, последующие шаги стартуют с него.

**Эстимейт:** 0.5 дня.

## Итого

| # | Шаг | Эстимейт | Gate | Статус |
|---|---|---|---|---|
| 3.0 | scaffolding (`ResponseCurve` + `appraisal/` + AiMemory v19→v20) | 1.0 | golden 0/131 | **DONE** (`36c3d18`) |
| 3.1 | producer (5 mineable signals) | 1.5 | unit-tests + golden 0/131 | pending |
| 3.2 | consumer self_preserve | 1.0 | per-entry golden review + scenario harness | pending |
| 3.3 | consumer continue_commitment | 1.0 | per-entry golden review + monotone_focus | pending |
| 3.4 | consumer reposition | 1.0 | per-entry golden review + 9 scenarios | pending |
| 3.5 | consumer finish_target + conserve_resource | 1.0 | per-entry golden review | pending |
| 3.6 | cleanup + повторный mining + rebaseline golden | 0.5 | mining-метрики достигают таргетов | pending |

**Суммарно ~7 дней.**

## Зафиксированные решения

1. **Response curves** — `ResponseCurve { Logistic, LinearClamped }` с самого начала в `AiTuning.curves` (1a). Стартовый набор кривых (6 штук) тюнится по результатам mining'а в 3.6.
2. **AiMemory расширение** — bump v19→v20 в 3.0 вместе со scaffolding'ом. Поля пишутся в `update_memory`, читаются consumer'ами в 3.2+. `#[serde(default)]` на обеих сторонах для backward compat со старыми v19-логами.
3. **Scope первой волны** — все 5 mineable need signals (`self_preserve`, `continue_commitment`, `finish_target`, `reposition`, `conserve_resource`). `rescue_ally`/`apply_cc`/`setup_aoe` остаются `0.0` до второй итерации mining'а.
4. **Phased rewire** — точечные замены в `select_intent` при сохранении лестницы. Bands+agenda — step 11.
5. **Gate'ы 3.2–3.5** — golden diff допустим, но per-entry разбор обязателен. Если diff деградирует от mining-таргетов: tunes curves в TOML, не код. Real gate — mining-таблица в 3.6.
6. **Heuristic эвристики до зависимостей от других шагов:**
   - `damage_dealt_to_last_target` — `max_hp - hp` если `last_target` единственный enemy с потерями (без team blackboard, step 13).
   - `has_defensive_ability` — whitelist способностей в `appraisal/` (без semantic AI tags, step 9).

## Критические файлы

- `src/combat/ai/tuning.rs` — `ResponseCurve`, `Curves` секция, `AiTuning.curves`.
- `src/combat/ai/appraisal/mod.rs` — новый модуль, `NeedSignals` + `compute_need_signals`.
- `src/combat/ai/intent.rs` — `AiMemory` расширение (3.0), `select_intent` consumer (3.2–3.5).
- `src/combat/ai/log.rs` — `AiMemorySnapshot` зеркало (3.0), `SCHEMA_VERSION` v19→v20.
- `src/combat/ai/utility/mod.rs` — `pick_action` вызывает `compute_need_signals`, прокидывает в `ScoringCtx`.
- `src/combat/ai/scoring.rs` — `ScoringCtx` расширяется полем `need_signals: &NeedSignals`.
- `assets/data/ai_tuning.toml` — секция `[curves]`.
- `docs/ai_need_signals.md` — обновляется в 3.6 с baseline → actual метриками.
- `docs/ai_rework.md` — §3 docstring sync в 3.6.
