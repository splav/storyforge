# Engine state как единственный source of truth — план унификации

**Статус**: предварительный план. Запланирован после Stage C0 (Path E) + step 0
(унификация lossy reconstruct).

**Текущий HEAD**: коммит `3d7a14b refactor(ai): unify unit_snapshots_to_combat_state via UnitSnapshot::as_pair`.

## Цель

Engine `CombatState.Unit` — единственный runtime источник истины для gameplay-state
юнитов. AI добавляет immutable side-table `AiCache` для derived метрик. Никаких
параллельных копий, никаких sync-points, никаких lossy reconstructions.

### Желаемый эндпоинт

```rust
BattleSnapshot {
    state: CombatState,    // engine, единственное хранилище gameplay state
    cache: AiCache,        // AI-derived, snapshot-time-stable, read-only после построения
}

UnitView<'a> {             // read API
    state: &'a Unit,       // engine Unit поля через Deref
    cache: &'a UnitAiCache,// AI-derived поля через .cache.*
}

UnitSnapshot              // ТОЛЬКО deserializer для logs schema v36/v37
                          // не существует как runtime тип

SimState {                // AI's "what-if" симулятор
    state: CombatState,   // owned clone of engine state
    cache: AiCache,       // immutable, не обновляется в sim (все поля stable)
    actor: Entity,
}
                          // мутирует state.units напрямую,
                          // вызывает engine's pure mutation API
```

## Текущий блокер

`SimState` сейчас клонирует `BattleSnapshot.units: Vec<UnitSnapshot>` и мутирует
**UnitSnapshot**. Это блокирует удаление поля `units` (Stage D невозможен пока
sim не перейдёт на `state.units`).

## Шаги к эндпоинту

### Step 1 — C2-C5: callsite migration (mechanical)

Завершить миграцию reader-callsites с `snap.unit_snapshot(e)` (legacy) на
`snap.unit(e) -> Option<UnitView>`. После этого удалить
`BattleSnapshot::unit_snapshot()` accessor.

**Файлы** (~17 callsites после Path E):

| Файл | Сложность | Заметки |
|---|---|---|
| `src/combat/ai/scoring/factors/step/scarcity.rs:72` | low | local read |
| `src/combat/ai/plan/future_value.rs:190, 201, 250` | medium | передаёт `&UnitSnapshot` в closures |
| `src/combat/ai/plan/generator.rs:54` | medium | передаёт в `seed_partial_score` |
| `src/combat/ai/plan/sim.rs:74, 851` | high | возвращает `&UnitSnapshot` из метода |
| `src/combat/ai/plan/parity_tests.rs:633` | medium | test |
| `src/combat/ai/outcome/builder.rs:63, 439` | medium | передаёт в `estimate_kill_soon` |
| `src/combat/ai/orchestration/mod.rs:225` | **high** | передаёт в `appraisal_ctx`, `assign_band`, etc. — **`ScoringCtx.active` caskaдит** |
| `src/combat/ai/system.rs:147` | high | передаёт в `update_memory`, `process_action` |
| `src/combat/ai/adapt/select.rs:81` | medium | `plan_has_self_rescue` |
| `src/combat/ai/scoring/trade.rs:244` | medium | `unit_value` helper |
| `src/combat/ai/scoring/factors/aggregate.rs:334, 427, 495` | **high** | `.cloned()` для последующих мутаций — owned UnitSnapshot |
| `src/combat/ai/scoring/policy/tests.rs:80, 90` | medium | test, `.cloned()` |
| `src/bin/replay_ai_log.rs:616` | medium | `.cloned()` |
| `src/combat/ai/replay/pipeline.rs:218` | medium | `.cloned()` |

**Каскадные миграции сигнатур** (после которых аналогичные callsites становятся
тривиальными):
- `ScoringCtx.active: &UnitSnapshot` → `UnitView<'_>` (core scoring path)
- `appraisal_ctx::active` — то же
- `update_memory(active: &UnitSnapshot, ...)` — то же
- `assign_band(active: &UnitSnapshot, ...)` — то же
- `build_agenda(..., active: &UnitSnapshot, ...)` — то же
- `select_intent(..., actor: &UnitSnapshot, ...)` — то же

**Подэтапы**:
- **C2**: тривиальные local-read callsites (где `&UnitSnapshot` не передаётся дальше).
- **C3**: `ScoringCtx.active` → `UnitView` (главный каскад через ~10 функций scoring layer).
- **C4**: helpers вне scoring (`update_memory`, `unit_value`, `plan_has_self_rescue`, `process_action`).
- **C5**: удаление `BattleSnapshot::unit_snapshot()` accessor.

**Объём**: ~10-15 коммитов, mechanical. Каждый callsite предсказуем.

**Риск**: низкий. Сигнатура `unit()` возвращает UnitView, читается через Deref для
engine-полей и через `.cache.*` для AI-полей. Семантика идентична. Опасность —
случайный шифт через изменения порядка iteration (видели в Pass 3 step 1-2 коммите
685a4cb с rescue_ally/apply_cc). Проверять каждый коммит через `golden_smoke` +
`all_ai_scenarios_pass`.

### Step 2 — Phase E: SimState мутирует engine.Unit (большой)

**Это главный архитектурный шаг.** До этого мы переносим плитку. Здесь — реальное
сшивание двух миров.

#### Текущее состояние SimState

`src/combat/ai/plan/sim.rs`:
- `SimState { snapshot: BattleSnapshot, actor: Entity, ... }`
- При `apply_step(step)` клонирует и мутирует `snapshot.units[i]` через:
  - `UnitSnapshot::add_status(status, &cache)` — добавляет в `.statuses` + зовёт `refresh_aggregates`
  - `UnitSnapshot::remove_status(id, &cache)` — то же
  - Прямые присваивания `unit.hp -= damage`, `unit.action_points -= cost`, etc.
- `refresh_aggregates(&status_tags)` пересчитывает `speed/armor_bonus/damage_taken_bonus`
  на основе активных статусов.

#### Engine эквиваленты

`crates/combat_engine/src/effect.rs`:
- `apply_effect(state: &mut CombatState, effect: &EffectDef, target: UnitId, rng: &mut DiceRng) -> Vec<Event>`
- `apply_status(state: &mut CombatState, target: UnitId, status: ActiveStatus)`
- `remove_status(state: &mut CombatState, target: UnitId, status_id: &StatusId)`
- Engine обновляет `speed/armor_bonus/damage_taken_bonus` сам при status changes.

#### Ключевая развилка

**Вариант 2A — AI sim вызывает engine's effect.rs напрямую**.
- Pros: ноль дублирования логики. Buff/dot/cleanse/refresh — всё engine'ом.
- Cons: engine sim сейчас не pure: эмитит `Vec<Event>`, использует `DiceRng` для разброса.
  AI хочет deterministic (expected dice values) и без emissions.
- Требует: **extract pure mutation API в engine** — функции которые не эмитят и
  принимают expected-value mode.

**Вариант 2B — AI sim переписывает мутации на engine.Unit, не используя engine API**.
- Pros: малый scope. Просто `state.units[i].statuses.push(...)` вместо
  `UnitSnapshot::add_status`. AI остаётся independent.
- Cons: дублирование переезжает с одного слоя на другой. Любое будущее изменение
  status-mechanics в engine придётся повторять в AI sim.

**Без явного решения 2A vs 2B Phase E нельзя начинать.**

#### Подэтапы Phase E (для 2B варианта)

- **E1**: вынести `as_pair`-конверсию в момент построения BattleSnapshot —
  убедиться что `state.units[i]` всегда отражает то же что `units[i]` (parity
  test уже существует, `view_state_matches_unit_snapshot_basic_fields`).
- **E2**: переписать `SimState::apply_step` cast-ветку — мутировать
  `state.units` instead of `units`. Reuse engine's `apply_effect` логику или
  re-implement на CombatState.Unit.
- **E3**: то же для move (тривиально — set position).
- **E4**: то же для end_turn / advance_turn_queue (engine уже это делает).
- **E5**: удалить `UnitSnapshot::add_status`, `remove_status`, `refresh_aggregates`,
  `apply_status_change` — больше не нужны.
- **E6**: удалить `unit_snapshots_to_combat_state` (только для legacy logs нужно
  было; после E sim больше не зависит от `units` поля).

**Объём**: 5-10 коммитов, реальный refactor с риском поведенческих сдвигов
(потому что AI sim и engine sim могут расходиться в нюансах: порядок эффектов,
RNG handling, edge cases с death/revive).

**Риск**: high. Любой шифт всплывёт в `golden_smoke` baseline. Перед началом
Phase E **расширить golden corpus** дополнительными сценариями (текущих 8 мало).

### Step 3 — Stage D: drop duplicate fields (после E)

После Phase E `SimState` не зависит от `BattleSnapshot.units`. Поле можно удалить.

- **D1**: удалить `BattleSnapshot.units / by_entity / round` (round дублирует
  `state.round`).
- **D2**: упростить `BattleSnapshotRepr` до `{state, cache}` — два поля вместо
  четырёх.
- **D3**: удалить `rebuild_index` ветку для пересборки `state` из `units` (больше
  не нужна — state всегда есть).
- **D4**: удалить `unit_snapshots_to_combat_state` (использовалась только в той
  ветке).
- **D5**: удалить shim'ы `UnitSnapshot::is_stunned`, `forces_targeting` — больше
  никто не держит `&UnitSnapshot` runtime.
- **D6**: parallel-функции `compute_status_delta_engine` / `status_hash_engine`
  становятся единственными — переименовать обратно в `compute_status_delta` /
  `status_hash`, удалить ActiveStatusView варианты.

**Объём**: 3-5 коммитов, mechanical.

**Риск**: low. Все компиляционные проверки.

### Step 4 — Phase G (опционально): SCHEMA bump

`UnitSnapshot` сейчас — сериализуемый формат логов. После Stage D он —
single-purpose: десериализатор v36/v37 логов. Schema v38+ может писать
`state + cache` напрямую, без UnitSnapshot.

- **G1**: bump `SCHEMA_VERSION` to 38. Log writer сериализует `BattleSnapshot`
  через `state + cache`.
- **G2**: log reader для v36/v37 продолжает использовать UnitSnapshot::as_pair
  для конверсии (drop legacy migration по прецеденту d38c83c).
- **G3**: регенерация `tests/baselines/baseline_v38.jsonl` и
  `tests/ai_scenarios/snapshots/*/log.jsonl`.

**Объём**: 2-3 коммита + регенерация тестового корпуса.

**Риск**: medium — регенерация baselines требует проверки каждого diff'а как
делали для rescue_ally fix. Не "слепая" перегенерация.

**Можно отложить навсегда**: UnitSnapshot как legacy reader живёт бесплатно.
Phase G не блокирует ничего важного.

## Открытые вопросы (требуют решения ДО Phase E)

### Q1: Вариант 2A или 2B для Phase E?

**Vote: 2B пока, 2A после.** Engine'ный pure mutation API — это отдельный refactor
engine'а, выходит за scope унификации. Сделать 2B сейчас, оставить дверь
открытой для 2A в будущем (выделить trait-абстракцию над мутациями, AI sim
имплементирует свою версию, engine может в будущем имплементить ту же).

### Q2: UnitAiCache дублирует поля engine Unit (entity, abilities, caster_ctx)

После Phase E это становится видимым: `cache.abilities` vs `state.unit.abilities`,
`cache.caster_ctx` vs `state.unit.caster_context`, `cache.entity` vs `state.unit.id`.

**Решение**: cleanup-PR в конце цикла. Удалить из UnitAiCache поля что есть в Unit.
Через `.cache.*` остаются только реально AI-специфичные: threat, role, tags,
max_attack_range, aoo_expected_damage, damage_horizon, crit_fail_effect,
ai_tuning_override.

### Q3: Расширить ли golden corpus перед Phase E?

**Vote: да.** Текущие 8 ai_scenarios и 1 golden_smoke baseline ловят только
обнаруженные ранее ситуации (rescue_ally, taunt, etc.). Phase E может ввести
сдвиги в edge cases (death/revive ordering, status stacking, AoE friendly fire).

Перед началом Phase E: добавить 5-10 ai_scenarios покрывающих:
- Multi-status interactions (stack stun + dot + buff order).
- Death triggers (lethal damage clears statuses correctly).
- Reactions chain (multi-AoO + counterattack).
- Resource overflow / underflow (mana cap, rage cap).
- AoE with friendly fire + reservations.

## Self-critique проектного решения

Самокритика плана:

1. **Phase F ("cache refresh on mutations") выброшен.** UnitAiCache поля все
   snapshot-stable, refresh не нужен. Это было заблуждение в первой версии плана.

2. **Phase G объявлен орthogonal.** Bump schema — отдельная инициатива, не зависит
   от Phase E. Не блокирует cleanup.

3. **Path E (Stage C0) — спорное решение в ретроспективе.** Сделано до этого
   плана. IS_STUNNED/FORCES_TARGETING можно было оставить вычисляемыми методами
   на UnitSnapshot без split bitfield — но в текущем состоянии всё работает,
   откат не имеет смысла. После Stage D shim'ы на UnitSnapshot уйдут естественно.

4. **Главный риск плана — Q1 (Phase E variant).** Если выбрать 2A и engine не
   готов exposing pure API — застрянем. Если 2B — на годы остаётся дублирование
   логики мутаций на двух уровнях.

5. **Test corpus не покрывает достаточно сценариев для Phase E.** Q3 — приоритет.

## Чего не делаем в плане

- **Не унифицируем AI и engine RNG модели.** AI остаётся deterministic, engine
  остаётся random. Это семантическое решение, не дублирование.
- **Не объединяем AI и engine sim в один code path.** AI всегда будет работать с
  copy-on-write state и expected values. Engine — с authoritative state и
  real RNG.
- **Не трогаем UnitAiCache построение (cache::build).** Логика построения cache
  не дублирует engine.Unit, она самостоятельна.
- **Не переписываем engine.** Phase E не требует изменений в engine кроме
  возможной extract API (вариант 2A — отложен).

## Когда начинать

**C2-C5** — можно начать прямо сейчас. Mechanical. Не требует больших решений.

**Phase E** — после явного решения Q1, Q3 (test corpus expansion). Не
импровизировать.

**Stage D** — автоматически следует за Phase E.

**Phase G** — когда станет неудобно поддерживать legacy UnitSnapshot reader.
Возможно никогда.

## Связанные документы

- `docs/ai/rework/step_unisim*_plan.md` — предыдущие шаги unification (Phases 1-7).
- `docs/ai/rework/index.md` — общая навигация.
- `docs/ai/extension-checklist.md` § SCHEMA_VERSION bump — процедура для Phase G.
- Коммиты последовательности Phase D: `c4cf91e` → `25e0e40` → `6776d61` →
  `5fb3cf4` → `1ad092c` → `601dbfb` → `49ffdaf` → `685a4cb` → `3b24e99` (Stage B)
  → `6077310` (C1) → `54b77bb` (C0/Path E) → `3d7a14b` (step 0 unification).
