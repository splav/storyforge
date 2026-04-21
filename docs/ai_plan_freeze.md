# AI Plan Freeze After Move

План: отключить реплан после Move-only шага (детерминированное изменение состояния, не должно менять решение), при этом **всегда строить shadow-план** и сравнивать — для диагностики случаев, где scorer был бы нестабилен.

Независим от [`ai_rework.md`](ai_rework.md) — тот про перестройку scoring-осей, этот про стабильность выполнения плана.

---

## Мотивация

В replay-логах наблюдается "вперёд–назад" паттерн: AI на тике N делает Move ближе к цели, на тике N+1 — Move обратно (пример: `Move→(6,0)→(6,1)→(6,0)` в разделе S1 из `ai_rework.md`).

Между тиками N и N+1:
- актор переместился по плану;
- в кубах рандома нет (scoring детерминирован по state);
- единственные допустимые изменения — AoO-урон, tick DoT, новые ауры, игрок что-то между ходами.

Если ни одно из этого не случилось, fresh-план должен продолжать исходный. Если продолжает не тот — scorer немонотонен под частичным исполнением плана. Это баг в скоринге, не в планировании. Нужно детектить.

---

## Дизайн

### StoredPlan в AiMemory

```rust
struct PlanSnapshot {
    actor_hp: i32,
    actor_rage: i32,
    actor_status_hash: u64,      // FNV из (status_id, rounds_remaining)
    target: Option<Entity>,
    target_hp: i32,
    target_pos: Hex,
    expected_actor_pos: Hex,     // куда должны были прийти
}

struct StoredPlan {
    steps: Vec<PlanStep>,
    step_index: usize,           // следующий к выполнению
    snapshot: PlanSnapshot,
    intent: IntentKind,
    cast_ability: Option<AbilityId>,
    cast_target: Option<Entity>,
    score: f32,
}

// в AiMemory:
last_plan: Option<StoredPlan>,
```

`IntentKind` хранится отдельно — независим от `AiMemory.last_intent` (тот тикается по `turns_committed`).

`TurnPlan.sim_snapshots` не копируем (тяжёлый, нужен только в процессе скоринга).

### Инвалидация

На следующем тике после Move проверяем `PlanSnapshot` против текущего состояния. Replan если:
1. `actor_hp` упал (AoO / DoT / reactive damage).
2. `actor_rage` изменился непредсказуемо (сейчас AoO +1 — достаточно сравнения, не идентичности; обычно ↑ с AoO).
3. `actor_status_hash` отличается (новый debuff/buff между ходами).
4. Целевая сущность мертва / отсутствует.
5. `target_hp` упал (союзник-AI добил).
6. `target_pos` сдвинулся.
7. `actor_pos != expected_actor_pos` (путь прервался — AoO truncation).
8. Сохранённый следующий шаг не валидируется под текущим состоянием (range, AP, мана/ярость).

При replan записывается `replan_reason` в log.

### Flow в run_ai_turn

```
1. build snapshot + maps
2. fresh_plan, fresh_decision = pick_action(...)   // всегда
3. if memory.last_plan.is_some() && settings.ai_freeze_plan_after_move:
     stored = memory.last_plan.take()
     if stored.snapshot.matches(current) && continuation_valid(stored, world):
         decision = continuation_from_stored(stored)
         used = "continuation"
     else:
         decision = fresh_decision
         used = "fresh_replan"
         replan_reason = Some(first_failed_check)
     log_plan_divergence(stored, fresh_plan, used, replan_reason)
   else:
     decision = fresh_decision
4. execute decision
5. if decision was Move:
     memory.last_plan = Some(StoredPlan::from(fresh_plan, step_index=1, current state))
   else:
     memory.last_plan = None
```

Важно: при continuation **сохраняем исходный `stored` с инкрементом `step_index`**, а не fresh_plan. Смысл freeze — выполнить план, который был построен до начала движения.

### pick_action API

Сейчас возвращает `(AiDecision, Option<DebugSnapshot>)`. Расширяем до `(AiDecision, Option<DebugSnapshot>, Option<TurnPlan>)` — полный выбранный план (чтобы сохранить/сравнить). `None` для fallback-ветки (нет планов).

### Logging

Новый event в `AiLogger`:
```rust
PlanDivergence {
    tick, actor,
    stored: { intent, ability, target, score },
    fresh:  { intent, ability, target, score },
    diff: { intent_changed, ability_changed, target_changed, score_delta },
    used: "continuation" | "fresh_replan",
    replan_reason: Option<String>,  // "actor_hp_drop" | "target_dead" | ...
}
```

Пишется **всегда** когда были и stored, и fresh — даже при валидной continuation (видим, на сколько fresh разошёлся бы, но не использован).

Overlay (debug): маркер ⚠ на токене актора при `ability_changed || target_changed || intent_changed` за последние 2 тика.

### Config

`GameSettings.ai_freeze_plan_after_move: bool` — default `true`. Выключение → старое поведение (реплан каждый тик, никаких StoredPlan).

---

## Фазы

### Phase 1. Инфраструктура

Низкий риск: добавить типы, поля, флаг — без изменения поведения.

- `PlanSnapshot`, `StoredPlan` в `src/combat/ai/intent.rs` рядом с `AiMemory`.
- `AiMemory.last_plan: Option<StoredPlan>` + сброс в `on_turn_start`/эквиваленте если есть.
- `GameSettings.ai_freeze_plan_after_move: bool = true`.
- Проброс `TurnPlan` из `pick_action` — расширить tuple-возврат.

**Проверка:** `cargo check`, существующие тесты не падают, поле `last_plan` никем не читается (пока).

**Файлы:** `src/combat/ai/intent.rs`, `src/combat/ai/utility/mod.rs`, `src/combat/ai/planning/picker.rs`, `src/content/settings.rs`.

### Phase 2. Continuation + инвалидация

Основная логика freeze.

- `PlanSnapshot::capture(actor, target, positions, combatants, statuses)`.
- `PlanSnapshot::matches(&self, current: &Self) -> Result<(), &'static str>` — возвращает причину расхождения либо `Ok`.
- `continuation_from_stored(stored, actor_pos, world) -> Option<AiDecision>` — валидирует следующий шаг (range/AP/resources) и строит decision.
- В `run_ai_turn` (src/combat/ai/enemy_turn.rs:79): ветка freeze до/после `pick_action`.
- Запись `last_plan` после Move-decision, очистка — после остальных.

**Проверка:** ручной replay одного из логов с "вперёд-назад" паттерном — ожидаем, что continuation сработает.

**Файлы:** `src/combat/ai/intent.rs`, `src/combat/ai/enemy_turn.rs`, `src/combat/ai/utility/mod.rs` (хелперы).

### Phase 3. Divergence logging

- Новый вариант `AiLogger` event (или расширение существующего — смотреть по структуре `log.rs`).
- Схема бампит `SCHEMA_VERSION`.
- `replay_ai_log` читает новое поле с `#[serde(default)]`.

**Проверка:** прогон одного боя, глазами смотреть divergence-записи в `logs/*.jsonl`.

**Файлы:** `src/combat/ai/log.rs`, `src/bin/replay_ai_log.rs`.

### Phase 4. Debug overlay

- Маркер на токене актора при divergence за последние 2 тика.
- Живёт рядом с существующим `AiDebugState`.

**Файлы:** `src/combat/ai/debug.rs`, `src/ui/ability_panel.rs` (или где рисуется AI overlay).

### Phase 5. Тесты

`tests/ai_replan.rs` (новый):

| Тест | Что проверяет |
|------|---------------|
| `snapshot_matches_unchanged_state` | Пустое состояние → `matches = Ok` |
| `snapshot_detects_actor_hp_drop` | AoO сымитирован → replan trigger |
| `snapshot_detects_new_status` | Statused ↑ → replan trigger |
| `snapshot_detects_target_death` | Target мёртв → replan |
| `continuation_rejects_invalid_range` | Следующий Cast вне range → `None`, fresh fallback |
| `freeze_prevents_oscillation` | Mock-сценарий где fresh дал бы другой cast, с флагом continuation выполняет сохранённый шаг |
| `freeze_disabled_allows_replan` | flag = false → старое поведение |

**Файлы:** `tests/ai_replan.rs`, `tests/common/mod.rs` (helpers).

---

## Что не делаем

- Не сохраняем `sim_snapshots` из `TurnPlan` — слишком тяжёлые, для continuation не нужны.
- Не меняем scoring — freeze лечит симптом, не причину. Divergence-логи дают материал для последующего расследования (см. ai_rework.md).
- Не трогаем `MoveAndCast` / `MoveThenCast` bundle — они атомарны в одном тике, не требуют freeze.
- Не применяем freeze к player/pact AI первоначально — только enemy. Можно расширить позже, если pact AI начнёт показывать тот же паттерн.

---

## Риски

### Застревание в невалидном плане

Если PlanSnapshot недостаточно полон, AI может "зависнуть" в continuation плохого плана. Защита:
- continuation_valid проверяет следующий шаг целиком (range, AP, resources, target alive).
- При любой валидационной ошибке — fresh replan.
- В логах виден `replan_reason`.

### Плохое взаимодействие с adaptation layer

Adaptation может переключить intent между тиками (например, HP упал → `ProtectSelf`). Это покрыто инвалидацией по `actor_hp` + status. Но если adaptation сработал без HP-изменения — не покрыто. Добавить в `PlanSnapshot` также `current_intent` и сравнивать.

### Кросс-ход персистентность

После EndTurn следующий ход того же актора — это новый цикл. `last_plan` должен очищаться на `advance_turn` либо автоматически — fresh decision не-Move → сброс. Проверить, что EndTurn-ветка сбрасывает `last_plan`.

---

## Что дальше

Запустить Phase 1, убедиться что всё собирается и тесты зелёные. Потом Phase 2 отдельным коммитом. Logging (Phase 3) можно сразу после Phase 2 — они образуют минимальный полезный slice (continuation работает + diagnostics есть). Overlay и тесты — следом.
