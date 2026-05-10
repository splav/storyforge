# Шаг 12 — Mid-plan reflow derived stats: декомпозиция на сабшаги

Декомпозиция в стиле step 6/7/8/9/10/11: коммит-на-сабшаг, gate на каждом.
Спецификация: `docs/ai/rework/index.md` §12.

## Preamble

### Текущее состояние

Симуляция многошагового плана живёт в `src/combat/ai/plan/sim.rs::SimState`:

- `SimState::from_snapshot(snap, actor)` клонирует `BattleSnapshot` (`src/combat/ai/plan/sim.rs:54`).
- `apply_step(step, ...)` диспатчит на `apply_move` / `apply_cast` (`:90`).
- `apply_move` мутирует `pos` и `movement_points` (`:112`). **Не записывает AoO damage** на actor'а.
- `apply_cast` платит `pay_costs`, считает `compute_ability_outcome`, вызывает `apply_primary` (мутирует target HP) и `apply_statuses` (модифицирует target.statuses) (`:126`).
- `apply_primary` (`:188`) применяет primary effect — Damage / Heal / Summon / etc. **Не мутирует rage** на attacker'е (drift #3). **Уже мутирует actor.hp при self-AoE friendly fire** (target = actor case), что является существующим источником outcome.self_damage > 0 в редких случаях.
- `apply_statuses` (`:314`) добавляет/удаляет статусы на target'е. **Не пересчитывает derived stats** (drift #speed, drift #status).

`UnitSnapshot` (`world/snapshot.rs`) имеет в одной плоскости и base, и derived поля:

```rust
pub struct UnitSnapshot {
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32,            // Base
    pub armor_bonus: i32,       // Derived from statuses
    pub damage_taken_bonus: i32, // Derived from statuses
    pub speed: i32,             // ← "Base + status speed_bonus" (агрегированный, single field)
    pub movement_points: i32,
    pub max_attack_range: u32,  // ← Constant (max range of any offensive ability)
    pub reactions_left: i32,    // Derived: starts max=1, decrements per AoO
    pub aoo_expected_damage: Option<f32>,  // Derived from weapon (constant per unit currently)
    pub statuses: Vec<ActiveStatusView>,    // Source of derived bonuses
    pub rage: Option<(i32, i32)>,           // current/max
    // ...
}
```

`expected_aoo_damage` (`scoring/horizon.rs:299`) — функция-helper, считает суммарный AoO damage для плана через scan path'ов. Возвращает float total. **Per-step propagation отсутствует** — функция возвращает «итог за весь план», не «AoO damage на step k».

Mining D1 на v35 corpus: `self_damage count=0 (never non-zero in corpus)` — outcome.self_damage всегда 0 в логах. AoO не достигает outcomes.

### Real combat reference (verified в pre-plan check)

`combat/resolution.rs::resolve_action_system` + `combat/apply_effects.rs::apply_effects_system`:

- **Damage→Status order**: `resolve_action_system` emit'ит `ApplyDamage` events ДО `ApplyStatus` events. `apply_effects_system` обрабатывает damage с **текущим** (pre-this-cast) armor/vuln. Status apply отдельно через `apply_status_system` дальше в pipeline. Внутри одного cast'а **damage сначала, status hooks потом** — sim mirror'ит этот order (см. §2 решение).

- **Rage gain rule** (apply_effects.rs:117-129):
  ```rust
  for (target, source, ...) in &damages {
      for actor in [source, target] { rage.gain(); }
  }
  ```
  **+1 attacker И +1 defender per damage event**. Для 3-target AoE attacker получает +3, каждый defender +1. См. §7 — мой первоначальный план («attacker +1 per cast») был **wrong**, исправлено.

### Проблемы текущей схемы

1. **Drift #speed** — статус `Haste` (+2 speed) применяется в `apply_statuses` к `target.statuses`, но `target.speed` (агрегат) не пересчитывается. Pathing на следующем step плана читает старый `speed`. Симметрично для `Slow`/`Root` на enemy: actor reach остаётся завышенным.

2. **Drift #3 (rage)** — `apply_primary` в Damage mode не делает rage gain. Real-pipeline это делает per damage event; sim — нет. Multi-step plan с rage-gated abilities видит rage как «застывшее на старте».

3. **Drift #status (armor/vuln/CC)** — `armor_bonus`, `damage_taken_bonus`, hard/soft CC tags на target меняются от status apply, но `apply_statuses` не пересчитывает derived поля. `apply_primary` на следующем step берёт старый armor.

4. **Forward-model death blindness (AoO suicide)** — observed bug в playtest 2026-05-09 road_bridge: actor HP=1, выбирает Move plan с `Move|Move|Cast`. AoO на первом Move = 11.5 dmg. Real actor умирает, Cast не выполняется. **Sim считает все 3 step outcomes как будто actor выживает** → план получает Cast damage credit → score высокий. ExpectedSelfLethal adaptation триггерит LastStand mode который ещё больше boost'ит offensive payoff.

5. **AoO damage не propagate'ится в outcome.self_damage** — `expected_aoo_damage` существует, но используется только критиком на whole-plan-level. `apply_move` outcome не записывает self_damage. Любой self-damage-aware downstream (SelfLethalWithoutPayoff critic, OvercommitIntoDanger AoOBleed) работает на synthetic data.

### Что закрывает шаг 12

1. **Sim-state split**: `UnitSnapshot.base_speed` (runtime-only field, `#[serde(skip)]`); derived поля пересчитываются явным `refresh_aggregates()` после каждой мутации statuses. Закрывает drift #speed.

2. **Status reflow** (один точечный change в `refresh_aggregates`) — пересчитывает `speed`, `armor_bonus`, `damage_taken_bonus`, AiTags из active statuses. Закрывает drift #speed + drift #status одним механизмом.

3. **`apply_primary` Damage мутирует rage** обоих сторон **per damage event** (real-rule mirror). Закрывает drift #3.

4. **Privatized `statuses` field**: `add_status` / `remove_status` методы auto-call `refresh_aggregates`. Класс багов «забыл refresh после мутации» исключён by construction.

5. **AoO propagation в `outcome.self_damage`** — `apply_move` теперь:
   - Сканирует path для each enemy с `reactions_left > 0` и `aoo_expected_damage.is_some()`.
   - Если path leaves adjacency → AoO triggered → `outcome.self_damage += final_damage(...)`.
   - Decrements `enemy.reactions_left` в sim snapshot (per-plan reaction budget).
   - Mutates `actor.hp -= aoo_damage`.

6. **Mid-plan death truncation**: generator проверяет `sim.actor.hp <= 0` после apply_step → terminate plan branch. Закрывает forward-model death blindness.

7. **Parity harness** (новый `tests/parity.rs`) — sim canonical scenarios identical с real combat (в пределах ε для dice variance).

### Что НЕ в scope шага 12

- **B3 closure** (Adaptation rescore wipes Critics) — отдельная архитектурная задача, не блокируется и не блокирует step 12. Step 12 закрывает AoO suicide через truncation (без B3); B3 закрывает orthogonal class «adaptation rescore стирает penalty layers».
- **TeamTasks blackboard** (step 13) — координация команды, отдельный layer.
- **Encounter scripts** (step 14), **Telegraphing** (step 15).
- **Step 12 не реализует sim для не-AoO reactions** (counterspell, retaliate) — current content не имеет таких.
- **Не меняем `expected_aoo_damage` whole-plan функцию** — она остаётся для критика OvercommitIntoDanger как pre-sim heuristic. После step 12 critic может также читать `outcome.self_damage` напрямую, но обе сигнатуры сосуществуют.
- **Не trying to recalibrate `axis_*_weights`** — calibration follow-up после measurement v35-rebuilt corpus.
- **Не trying to fix non-deterministic dice** — sim использует ExpectedValue (mean), real использует RngDice. Parity tests calibrate within ε.
- **Schema bump v35→v36** в финальном сабшаге (12.4) atomic. `base_speed` сериализуется явно.

## Зафиксированные решения по развилкам

### 1. Derived state: explicit struct vs in-place fields в `UnitSnapshot`

**Выбор: in-place fields + `refresh_aggregates()` метод**, отметив комментариями в struct definition какие поля derived.

Альтернатива (1b): новая struct `DerivedStats` как поле `UnitSnapshot.derived: DerivedStats`. Отвергнута: ломает 200+ callsites которые читают `unit.speed` / `unit.armor_bonus` напрямую.

Альтернатива (1c): generic accessor `unit.speed()` с lazy computation. Отвергнута: hidden allocation/recompute, плохо для hot path (factor formulas).

**Реализация**:

```rust
pub struct UnitSnapshot {
    // ── Base (immutable after snapshot construction) ─────────
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32,
    /// Base move budget without status modifiers. Runtime-only field
    /// (#[serde(skip)]); reconstructed at load as
    /// `base_speed = speed - sum(active speed bonuses from statuses)`.
    #[serde(skip)]
    pub base_speed: i32,

    // ── Derived (recomputed by refresh_aggregates) ───────────
    pub speed: i32,                         // = base_speed + speed_bonus_from_statuses
    pub armor_bonus: i32,                   // = sum(status.armor_amount)
    pub damage_taken_bonus: i32,            // = sum(status.vuln_amount)
    pub reactions_left: i32,                // base 1, decrements after AoO
    pub aoo_expected_damage: Option<f32>,   // derived from weapon (constant per unit)
    pub tags: AiTags,                        // recomputed from statuses+capabilities
    // ...

    // ── Source of derived state (privatized) ─────────────────
    pub(crate) statuses: Vec<ActiveStatusView>,
}

impl UnitSnapshot {
    /// Add a status and refresh derived aggregates atomically.
    pub fn add_status(&mut self, s: ActiveStatusView, status_tags: &StatusTagCache) {
        self.statuses.push(s);
        self.refresh_aggregates(status_tags);
    }

    /// Remove status by id and refresh.
    pub fn remove_status(&mut self, id: &StatusId, status_tags: &StatusTagCache) -> bool {
        let before = self.statuses.len();
        self.statuses.retain(|s| &s.id != id);
        let changed = self.statuses.len() != before;
        if changed { self.refresh_aggregates(status_tags); }
        changed
    }

    /// Read-only access to active statuses.
    pub fn statuses(&self) -> &[ActiveStatusView] { &self.statuses }

    /// Recompute all derived fields from base + active statuses.
    pub fn refresh_aggregates(&mut self, status_tags: &StatusTagCache) {
        // Logic added incrementally in 12.1.
    }
}
```

### 2. Privatized `statuses` field — invariant safety

**Выбор: `pub statuses` → `pub(crate) statuses` + `add_status`/`remove_status`/`statuses()` API.** Auto-call `refresh_aggregates` после мутации.

Это исключает class-of-bugs «забыл refresh после `unit.statuses.push(...)`».

Migration: ~10-15 callsites в snapshot construction + sim. Все они проходят через `add_status`. Read-only callsites (mining, factor formulas) переходят на `unit.statuses()` (returns `&[ActiveStatusView]`).

Альтернатива (2b): leave `pub`, manual discipline. Отвергнута: silent drift inevitable.

### 3. `refresh_aggregates` location и frequency

**Выбор: `UnitSnapshot::refresh_aggregates(&mut self, status_tags)` метод**, вызывается автоматически из `add_status`/`remove_status`. В sim также manual call в `apply_statuses` после bulk изменений, и в `apply_primary` если damage снимает статус.

**Order в sim** (verified от real combat — см. Real combat reference выше):
```
apply_cast {
    pay_costs;
    apply_primary;      // Damage/Heal — читает derived (старый armor) ← real combat order
    apply_statuses;     // Apply status, then refresh_aggregates per affected unit
}
```

Внутри одного Cast primary применяется до status apply — verified от real combat в `resolution.rs` + `apply_effects.rs`.

### 4. AoO propagation: где и как

**Выбор: per-step AoO в `apply_move`**, использует тот же helper что `expected_aoo_damage` (общий algorithmic core).

Декомпозиция:
- Extract AoO scan core в `scoring/horizon.rs::scan_aoo_hits_for_step(prev_pos, path, enemies) -> Vec<AooHit>`. Pure function. `AooHit { enemy_idx, raw_damage }`.
- `expected_aoo_damage(active, plan, enemies)` — оборачивает scan: суммирует raw_damage для всех Move steps плана + applies mitigation (existing whole-plan signature, для критика).
- `apply_move` использует scan на текущем step:
  - Получает hits per-step.
  - Применяет each hit к actor.hp в sim snapshot (`final_damage_f32(raw, mitigation, vuln, false)`).
  - Decrements enemy.reactions_left (≥0).
  - Записывает `outcome.self_damage = sum(applied_damages)`.

`reactions_left` decrement важен: enemy с reactions=1 не должен AoO-ить дважды на разных Move steps одного плана.

**Borrow checker note**: `apply_move` мутирует и `actor.hp` (через `actor_unit_mut`), и `enemy.reactions_left` (через другой `unit_mut(enemy)`). Two concurrent `&mut UnitSnapshot` borrow — Rust будет ругаться. Pre-collect pattern:
```rust
// Phase 1: scan with immutable borrow.
let hits = scan_aoo_hits_for_step(actor_pos, path, &snap.units);
// Phase 2: mutate sequentially.
for hit in hits {
    if let Some(actor) = self.actor_unit_mut() {
        let mitigation = actor.armor + actor.armor_bonus;
        let vuln = actor.damage_taken_bonus;
        let applied = final_damage_f32(hit.raw_damage, mitigation, vuln, false);
        actor.hp = (actor.hp - applied as i32).max(0);
        outcome.self_damage += applied;
    }
    if let Some(enemy) = self.snapshot.units.get_mut(hit.enemy_idx) {
        enemy.reactions_left = (enemy.reactions_left - 1).max(0);
    }
}
```

### 5. Mid-plan death: hard truncation vs marker

**Выбор: hard truncation в generator**, плюс safety guard в sim.

Generator (`plan/generator.rs::enumerate_next_steps`):
- После `apply_step` проверяет `if sim.actor_unit().map(|a| a.hp <= 0).unwrap_or(true) { return; }` — branch terminates, no further steps appended.

Sim safety (`plan/sim.rs::apply_step`):
- Если на entry `actor.hp <= 0` — return `StepOutcome::default()` (safety net; не должно достигаться при правильном generator'е).

Альтернатива (5b): soft marker `outcome.actor_dead_at_end: bool`. Отвергнута: усложняет factor formulas (каждый должен respect'ить marker), легко забыть в новом factor.

Альтернатива (5c): mask plan через KillableGate-like. Отвергнута: KillableGate про target killability, иной concept.

### 6. AoO в `outcome.self_damage`: per-step (existing semantics)

`outcome[k].self_damage` = self damage taken на step k. Cumulative считают консьюмеры через `plan.outcomes.iter().map(|o| o.self_damage).sum()`. Существующие критики так и делают.

После step 12, outcome.self_damage записывается из **двух источников**:
- AoO в Move steps (новое в 12.2).
- Self-AoE friendly fire в Cast steps (existing — `apply_primary` уже мутирует actor.hp когда target=actor в AoE).

### 7. `base_speed` — serialized + schema bump v35→v36

**Выбор: `pub base_speed: i32` сериализуется явно.** Schema bump v35→v36 atomic в финальном сабшаге (12.4). Consumers видят explicit field. Symmetric с прошлыми step-bumps (step 5/7/8/11 все делали schema bump на финальном сабшаге).

На v35 logs `base_speed` отсутствует → deserialise через `#[serde(default = "..." )]` со значением = `speed` field. Post-load reconstructor в `parse_actor_tick` корректирует: `base_speed = speed - sum(active.speed_bonus)` если статусы есть.

Альтернатива (7b): `#[serde(skip)] pub base_speed`, runtime-only. Отвергнута: consumers (mining, replay) не видят explicit field; debugging трудно.

### 8. Rage mutation rule (corrected from real combat)

**Real rule** (apply_effects.rs:117-129): per damage event, +1 для source И target. AoE с N targets → attacker получает +N rage, каждый defender +1.

**Sim implementation в `apply_primary` Damage arm**:
```rust
PrimaryEffect::Damage { affected_units } => {
    for hit in affected_units {
        // Existing: damage mitigation + HP mutation.
        let target = self.unit_mut(hit.entity);
        target.hp -= mitigated;

        // NEW: rage gain (drift #3) — per hit для attacker AND defender.
        if let Some((cur, max)) = target.rage {
            target.rage = Some(((cur + 1).min(max), max));
        }
        if let Some(actor) = self.actor_unit_mut() {
            if let Some((cur, max)) = actor.rage {
                actor.rage = Some(((cur + 1).min(max), max));
            }
        }
    }
}
```

(Borrow checker: pre-collect hit.entity values, then mutate sequentially, как в §4 AoO.)

### 9. Replay tool — clean break на v35-rebuilt corpus

**Выбор: rebuild golden replay corpus после step 12.** Replay assertions переписаны под new sim outcomes. Symmetric с прошлыми step-боундами (step 5/7/8/9/10/11 все делали golden rebuild).

Альтернатива (9b): `legacy_no_aoo_propagation: bool` flag в sim для backward-compat replay'а v34/v35 logs. Отвергнута: branch добавляет permanent code path; clean break проще.

### 10. Sub-step order: status reflow → AoO

**Выбор: 12.1 status reflow первым, 12.2 AoO вторым.** Это значит AoO mitigation корректен с момента введения (12.2 читает refresh'нутый armor_bonus).

Цена: AoO suicide bug закрывается на ~1 день позже. Acceptable — playtest продолжает функционировать; bug observable но closed properly when 12.2 ships.

Альтернатива (10b): AoO first (12.1 AoO, 12.2 status). Mitigation slightly off в edge cases с mid-plan armor buffs. Отвергнута user'ом — correctness over speed.

### 11. Параллельная работа со step 12 — что НЕ менять

Step 12 — sim correctness fix. **Не trying to fix B3** (Adaptation rescore wipes Critics) внутри step 12. После step 12, AoO suicide bug закрыт через truncation (план с death на step 1 имеет `outcomes[0].self_damage = 11.5`, остальные steps не существуют — Cast damage не учитывается). B3 остаётся в backlog как orthogonal task.

## Природа gate'ов

Step 12 — behavior change: sim становится «правильнее», но в существующих ai_scenarios golden tests могут появиться decision drift'ы. Behavioral invariants по сабшагам:

- **12.0** (scaffolding): `cargo test --lib` зелёный. Существующие ai_scenarios — без изменений.
- **12.1, 12.2, 12.3** (per-drift fixes): per-drift ai_scenarios должны pass'ить (новые fixtures, expected = post-fix behaviour). Existing scenarios могут drift — каждое расхождение review'ится и attribute'ится.
  - **Drift threshold gate**: если > **30% existing ai_scenarios** drift'ят на одном сабшаге → pause, review per-fixture, attribute или revert change.
- **12.4** (cleanup): all gates pass. Performance regression `pick_action` ≤ +20% от pre-step12 baseline.

Parity harness (`tests/parity.rs`) распределена по сабшагам — каждый сабшаг добавляет per-drift parity test как часть DoD, не batch'ом в конце.

## Сабшаги

### 12.0. Scaffolding: parity harness + base_speed + statuses privatize + refresh_aggregates skeleton

**Scope.**

`src/combat/ai/world/snapshot.rs`:
- Добавить `pub base_speed: i32` после `pub speed: i32` с `#[serde(default = "default_base_speed")]` (fallback returns 0; post-load reconstructor поднимает до правильного значения).
- Конструкторы (`build_snapshot`, тесты) инициализируют `base_speed = speed - sum(active.speed_bonus)` либо `= speed` если нет статусов.
- `UnitBuilder::build()` — `base_speed: speed_at_build_time`.
- На load v35 logs (`base_speed=0` default): post-process в `parse_actor_tick` или `build_snapshot` reconstructor: `base_speed = speed - sum(speed bonuses from active statuses)`. На load v36+ logs: `base_speed` читается явно.
- Privatize `pub statuses` → `pub(crate) statuses`.
- Add API: `add_status(s, status_tags)`, `remove_status(id, status_tags) -> bool`, `statuses() -> &[ActiveStatusView]`.
- Метод `refresh_aggregates(&mut self, status_tags: &StatusTagCache)` на `UnitSnapshot` — empty body на 12.0, placeholder.

`tests/parity.rs` (новый файл):
- Skeleton harness: `fn run_parity_scenario(name: &str, scenario: ScenarioFn) -> ParityReport` — runs both real combat + AI sim against same scenario, returns diff.
- Empty `ParityReport { hp_drift, pos_drift, statuses_drift, rage_drift, speed_drift }` — все нулевые на 12.0.
- 1 sentinel test: `parity_no_op_scenario_zero_drift` — empty scenario passes trivially.

Migration: callsites use `unit.statuses` for read → `unit.statuses()`. Callsites push'ат → `unit.add_status(..., &cache)`. Compile-only change.

**Юнит-тесты:**
- `base_speed_reconstructed_from_speed_minus_status_bonus`: snapshot with active Haste(+2), `speed=5` → `base_speed=3`.
- `add_status_calls_refresh_aggregates`: mock cache, push status → verify refresh called (через flag в test cache).
- `remove_status_returns_true_when_removed`: returns false on non-existent id.
- `statuses_accessor_returns_immutable_slice`.
- `parity_no_op_scenario_zero_drift` (sentinel).

**Что НЕ делать в 12.0:**
- Не трогать `apply_statuses` / `apply_primary` mid-plan logic.
- `refresh_aggregates` — empty body (логика в 12.1).

**Gate.** Все existing tests pass (~838). Behaviour invariant: `ai_scenarios` golden 0/N drift.

**Эстимейт:** 1 день.

---

### 12.1. Status reflow (drift #speed + drift #status)

**Scope.**

`src/combat/ai/world/snapshot.rs::refresh_aggregates`:
- Сканирует `self.statuses`, использует `status_tags` для семантики.
- `speed_bonus = sum(status.speed_modifier from speed-buff/debuff statuses)`.
- `armor_bonus = sum(status.armor_amount from armor-buff statuses)`.
- `damage_taken_bonus = sum(status.vuln_amount from vulnerability statuses)`.
- `tags = StatusTagCache::compute_unit_tags(...)` — Hard/Soft CC, FORCES_TARGETING, Compulsion etc.
- `aoo_expected_damage` — TODO comment: «derived from weapon, currently constant; if weapon-status interaction (Disarm, Empower Strike) появится — пересчитывать здесь». Сейчас не трогаем.
- `self.speed = self.base_speed + speed_bonus`.

`src/combat/ai/plan/sim.rs::apply_statuses`:
- После всех `target.statuses.push(...)` / removes — `target.refresh_aggregates(status_tags)` (либо use `add_status`/`remove_status` для auto-refresh).

`src/combat/ai/plan/sim.rs::apply_primary` Damage:
- При expire статуса из-за damage (если такая логика есть в real, например Bleed cleansed by burst heal — verify в combat code) — call refresh.
- Pass `status_tags` в sim apply functions: extend `SimState` с `status_tags: &'a StatusTagCache` reference.

**Юнит-тесты:**
- `apply_haste_increases_speed`: actor base_speed=3, apply Haste(+2) → speed=5.
- `apply_slow_decreases_speed`: target base_speed=3, apply Slow(-1) → speed=2.
- `expire_haste_restores_speed`: actor with Haste at expiry → speed=base.
- `multiple_speed_statuses_stack`: Haste+Bless → speed = base + sum.
- `apply_armor_buff_reduces_subsequent_damage`: target hit raw=10, pre-buff damage=8. Apply ArmorBuff(+2). Next hit raw=10 → damage=6.
- `apply_vulnerability_increases_damage`: Cast(Vuln) → subsequent hit damage_taken_bonus.
- `hard_cc_status_updates_tags`: apply Stun → target.tags has IS_STUNNED bit.

**Parity tests** (в `tests/parity.rs`):
- `parity_haste_speed_real_vs_sim`: real combat + sim same Haste cast → both yield speed=base+2 после.
- `parity_armor_buff_mitigation_real_vs_sim`: same scenario, hit damage identical.

**ai_scenarios:**
- `self_haste_then_move_then_cast`: actor casts Haste self, Move uses extended range, Cast on target reachable only post-haste.
- `armor_buff_self_then_tank`: actor casts armor buff, tanks subsequent enemy attack. Plan score reflects reduced damage.
- `vuln_target_then_burst`: actor applies Vuln, then heavy damage hit. Score reflects amplified damage.

**Gate.**
- ai_scenarios pass; existing — drift acceptable, attributed; **drift threshold ≤30%**.
- Parity tests pass within ε.

**Эстимейт:** 2 дня.

---

### 12.2. AoO propagation + mid-plan death truncation (closes AoO suicide)

**Scope.**

`src/combat/ai/scoring/horizon.rs`:
- Extract `scan_aoo_hits_for_step(actor_start_pos, path, enemies) -> Vec<AooHit>`. Pure function.
- `AooHit { enemy_idx: usize, raw_damage: f32 }`. enemy_idx vs entity для borrow-friendly mutation.
- `expected_aoo_damage(...)` — оборачивает scan + applies mitigation (existing whole-plan helper остаётся для критика).

`src/combat/ai/plan/sim.rs::apply_move`:
- Pre-collect `hits = scan_aoo_hits_for_step(...)` (immutable borrow phase).
- Sequentially mutate (см. borrow note в §4):
  - actor: apply mitigated damage to `hp`, accumulate to `outcome.self_damage`. **Mitigation использует refresh'нутый armor_bonus от 12.1.**
  - enemy[hit.enemy_idx]: decrement `reactions_left`.

`src/combat/ai/plan/generator.rs::enumerate_next_steps`:
- После apply_step: `if sim.actor_unit().map(|a| a.hp <= 0).unwrap_or(true) { return; }` — branch terminates.

**Юнит-тесты (sim):**
- `apply_move_records_aoo_self_damage`: actor adjacent to enemy с aoo_expected_damage=Some(5), reactions_left=1. Move out of adjacency → outcome.self_damage = mitigated(5).
- `apply_move_decrements_enemy_reactions`: same setup → enemy.reactions_left = 0.
- `apply_move_no_aoo_when_already_used_reaction`: reactions_left=0 → no self_damage, no decrement.
- `apply_move_kills_actor_with_lethal_aoo`: actor hp=1, AoO=10 → actor.hp=0, outcome.self_damage=10.
- `apply_move_aoo_mitigated_by_status_armor_buff`: actor casts armor buff, then provokes AoO; mitigation includes buff amount. **Verifies 12.1 + 12.2 integration.**

**Юнит-тесты (generator):**
- `enumerate_terminates_when_actor_dies_mid_plan`: actor hp=1 + enemy AoO=10 + multi-step plan. Generated plan length = 1; no Cast/Move-2 extensions.
- `single_step_lethal_move_still_recorded`: lethal Move plan существует в pool с outcome.self_damage > hp.

**Parity tests:**
- `parity_aoo_real_vs_sim`: real combat вход в AoO + sim same → identical damage.
- `parity_aoo_decrements_reactions_real_vs_sim`: enemy reactions consumed identically.

**ai_scenarios:**
- `aoo_suicide_truncation`: actor at hp=1 adjacent to taunter. Pool **не должен** содержать Move-out-of-adjacency план длиннее 1 step. Если есть — chosen plan не выбирает его (через критики или low score).

**Gate.**
- Mining на post-step12 corpus показывает D1 `self_damage > 0` для AoO-провоцирующих сценариев.
- Decision drift в существующих ai_scenarios — review attributed; **drift threshold ≤30%**.
- Parity tests pass.

**Эстимейт:** 2-3 дня (с учётом drift review).

---

### 12.3. Rage mutation в apply_primary (drift #3)

**Scope.**

`src/combat/ai/plan/sim.rs::apply_primary` Damage arm:
- Pre-collect hit entities.
- Per damage event: `+1` для attacker AND target rage (clamped to max).
- AoE с N targets → attacker получает N total rage gain (real-rule mirror).

(Verified rule в Real combat reference выше: apply_effects.rs:117-129.)

**Юнит-тесты:**
- `apply_damage_grants_rage_to_attacker_per_hit`: caster rage (5/10), 1-target Damage → rage (6/10).
- `apply_damage_grants_rage_to_defender_per_hit`: target rage (3/10), Damage hit → (4/10).
- `aoe_damage_grants_rage_per_target_to_attacker`: 3-target AoE, attacker rage (5/10) → (8/10). **Каждый defender +1**.
- `rage_caps_at_max`: rage (10/10) + damage → still (10/10).

**Parity tests:**
- `parity_rage_real_vs_sim`: same Damage cast в real + sim → identical rage values на attacker и defender.
- `parity_rage_aoe_real_vs_sim`: 3-target AoE → attacker +3, each defender +1 в обоих.

**ai_scenarios:**
- `rage_gated_ability_after_basic_attack`: actor rage_cost=3 ability, начало round rage=2. Plan: Basic Attack (gain +1 → 3), then Rage Ability — должен быть viable в pool.

**Gate.**
- Parity tests pass.
- ai_scenarios pass; drift threshold ≤30%.

**Эстимейт:** 1 день.

---

### 12.4. Cleanup + schema v35→v36 + docs + clean-break replay corpus

**Scope.**

**Schema bump v35→v36** (atomic):
- `SCHEMA_VERSION` 35 → 36 в `src/combat/ai/log/mod.rs`.
- `base_speed` сериализуется в JSONL явно. v35 logs deserialise через `#[serde(default)]` + post-load reconstructor.
- `bin/replay_ai_log.rs` / `bin/mine_ai_logs.rs` обновляются на v36 parsing (`MIN_SUPPORTED` сдвигается).
- v35 logs либо clean-break отвергаются (как было с v32/v33 в прошлых bumps), либо acceptable через `#[serde(default)]` reconstructor — выбрать consistent с прошлой практикой (v34 принимался как fallback на v35; здесь делаем то же).
- Update parse_actor_tick docstring + LogError::UnsupportedSchema sample.

**Replay golden corpus** (clean break):
- Drop pre-step12 v35 corpus in `tests/baselines/baseline_v34.jsonl` (если он там).
- Capture fresh post-step12 v36 corpus.
- Replay assertions обновлены под new sim outcomes.

**Documentation**:
- Mining D1 documentation update: `self_damage > 0` теперь expected для AoO-проvокирующих сценариев.
- `docs/ai/ai.md` — секция «Mid-plan reflow» (новая): base_speed/derived split, refresh_aggregates contract, AoO propagation, rage rule.
- `docs/ai/tech-debt.md` — закрыть «drift #speed», «drift #3», «forward-model death» (strikethrough со ссылкой на step 12 commit).

**Cleanup**: TODO комментарии для drift'ов из старого кода, любые workarounds.

**Performance gate:**
- Benchmark `pick_action` на canonical scenario (e.g. `road_bridge` round 1 turn). Compare vs pre-step12 baseline.
- **Threshold: ≤ +20% time regression**. Beam search overhead от refresh_aggregates + AoO scan per Move acceptable до 20%.
- Если > +20% — investigate, optimize hotpath (likely refresh_aggregates allocation in tight loop).

**Юнит-тесты:**
- Mining D1 voiceprint включает self_damage scenarios.
- Schema round-trip: v36 log → parse → v36 (identity).
- v35 backward-compat: v35 log → parse → `base_speed` reconstructed correctly.

**Gate.**
- `cargo test --lib` все зелёные.
- v36 round-trip 0/N на rebuilt corpus.
- v35 logs deserialise корректно (reconstructor работает).
- Replay golden corpus rebuilt, replay assertions pass.
- Mining D1 показывает self_damage count > 0 на post-step12 playtest corpus.
- Existing ai_scenarios passing (с обновлёнными expected.toml где drift attributed).
- Performance regression ≤ +20%.

**Эстимейт:** 1.5 дня (с учётом schema bump work).

---

## Итого

| # | Шаг | Эстимейт | Gate | Закрывает |
|---|---|---|---|---|
| 12.0 | Scaffolding (parity harness + base_speed + statuses privatize + refresh_aggregates skeleton) | 1d | golden 0/N, no behavior | — |
| 12.1 | **Status reflow (speed/armor/vuln/CC merged)** | 2d | speed/armor scenarios + parity | drift #speed + drift #status |
| 12.2 | **AoO + mid-plan death truncation** | 2-3d | aoo_suicide_truncation + parity | observed AoO suicide bug |
| 12.3 | Rage mutation в apply_primary | 1d | rage_gated + parity | drift #3 |
| 12.4 | Cleanup + schema v35→v36 + docs + clean-break replay corpus + perf gate | 1.5d | v36 round-trip 0/N + replay golden 0/N + perf ≤+20% | schema, docs, replay |

**Суммарно ~7.5-8.5 дней (realistic 10-12 дней с учётом ai_scenarios drift review).**

## Критические файлы

- `src/combat/ai/world/snapshot.rs` — `UnitSnapshot.base_speed` field, `refresh_aggregates`/`add_status`/`remove_status`/`statuses()` API.
- `src/combat/ai/plan/sim.rs` — `apply_move` (AoO), `apply_primary` (rage), `apply_statuses` (refresh).
- `src/combat/ai/plan/generator.rs::enumerate_next_steps` — death truncation guard.
- `src/combat/ai/scoring/horizon.rs` — `scan_aoo_hits_for_step` extract.
- `tests/parity.rs` (новый) — real vs sim parity harness.
- `tests/ai_scenarios/snapshots/{aoo_suicide_truncation,self_haste_then_move_then_cast,armor_buff_self_then_tank,vuln_target_then_burst,rage_gated_ability_after_basic_attack}/` — fixtures.
- `bin/replay_ai_log.rs` — golden corpus regenerate.
- `docs/ai/ai.md`, `docs/ai/tech-debt.md` — docs sync.

**В 12.4 затрагиваем** для schema bump: `src/combat/ai/log/mod.rs` (SCHEMA_VERSION 35→36 + parse_actor_tick docstring), `bin/replay_ai_log.rs` + `bin/mine_ai_logs.rs` (v36 parsing).

## Стратегия миграции тестов

### Тесты которые **становятся obsolete** (ищем в процессе)

- Тесты pinning «outcome.self_damage всегда 0» — если найдены, удалить.
- Тесты pinning «speed после Haste не меняется» — если найдены, удалить.
- Тесты `unit.statuses.push(...)` напрямую — переписать на `add_status(..., &cache)` (compile-time fix через privatization).

### Тесты которые **остаются** (в основном)

- `plan/sim.rs::tests::*` (existing apply_move / apply_cast / apply_primary / apply_statuses) — base behaviour preserved.
- `plan/generator.rs::tests::*` — большинство pass'ит. Тесты на dedup, ai_policy_ok, beam_pruning — independent.
- `outcome/builder.rs::tests::*` — outcome shape не меняется (только population).
- Critic tests — synthetic plan setups; работают с напрямую заданными outcomes. Step 12 не трогает.
- H1c.bis mining tests — independent layer.

### Тесты которые **нужно обновить**

- **Tests reading `unit.speed`** напрямую с assumption «speed unchanged» — найти и убедиться используют корректное поле.
- **ai_scenarios fixtures** — некоторые drift'нут. Per-fixture review:
  - `focus_target_melee_basic` — простой, drift unlikely.
  - `continuation_*` — verify не зависит от broken sim.
  - `bell_crypt`, `road_bridge` (если есть в fixtures) — могут drift.

  Per drift'у: если новый decision align'ится с design intent — обновить `expected.toml`. Если хуже — investigate как regression.

### Тесты которые **нужно добавить** (per sub-step)

Списки см. в каждом сабшаге выше. Total: ~25 unit tests + 6 parity tests + 5 ai_scenarios fixtures.

### Test runner expectations

После полного step 12:
- `cargo test --lib` — должно быть ~860+ tests (был 797 + ~25 sim/snapshot + ~5 generator + ~6 parity).
- `cargo test --bin mine_ai_logs` — без изменений (38).
- `cargo test --test ai_scenarios` — все pass; некоторые existing fixtures с обновлёнными `expected.toml`.
- `cargo test --test parity` (новый) — pass.

## Что откладывается

- **B3 closure** — orthogonal architectural task. Step 12 не блокирует и не requires.
- **Step 13 TeamTasks** — следующий шаг по master plan.
- **Recalibration of `axis_*_weights`** — calibration follow-up если playtest на post-step12 corpus покажет AI «слишком умный».
- **`expected_aoo_damage` removal** — функция остаётся как whole-plan helper для критика и adapt. После step 12 дублирует scan, но callers не упрощаются легко.
- **Reaction model для не-AoO reactions** — counterspell, retaliate. Когда content получит — расширять scan и apply_move.
- **`aoo_expected_damage` status-dependency** — TODO в `refresh_aggregates`. Когда content получит Disarm / Empower Strike — пересчитывать.

## Чего не делать в шаге 12

- **Не менять real combat pipeline** — sim сближается с real, не наоборот.
- **Не сливать с B3** — раздельные slice'ы.
- **Не делать speculative reactions** — counterspell/retaliate не существуют в content.
- **Не trying to fix non-deterministic dice** — sim ExpectedValue, real RngDice. Parity tests calibrate within ε.
- **Не трогать AoE crit/miss механики** — `crit_failed=false` hardcoded в sim, matches «sim never crit-fails» contract.
- **Не пытаться зафиксировать `axis_*_weights`** — после step 12 leverage formulas будут давать другие значения. Calibration — потом.
- **Не bump'ать schema** — `base_speed` runtime-only.

## Связь с master plan

Step 12 — конец Wave 3 первая половина. После закрытия:
- Wave 3 step 13 (TeamTasks blackboard) — следующий по плану.
- B3 closure — opportunistic, можно перед или после 13.
- Wave 4 (encounter scripts / telegraphing / quirks / geometry) — после Wave 3.

После step 12 forward-model становится корректной — это разблокирует:
- Step 13 (TeamTasks): team plans теперь reliable.
- Step 15 (Telegraphing): telegraph based on accurate sim outcomes.
- Step 17 (Geometry signals): geometry effects properly propagate.
