# Bridge turn-lifecycle unification — план

**Статус**: ✅ выполнен (2026-05-21). Все фазы (B0–B5) + B-prime закоммичены.
См. § «Hand-off / completion» внизу для commit hash'ей и список выловленных
по пути bug-классов.

**Стартовый HEAD**: `a3915de fix(combat): restore ActiveCombatant lifecycle on turn handoffs`.
**Финальный HEAD**: `5ffd874 fix(engine): pop enemy_phases[0] inside Effect::EnterPhase apply`.

**Связь с другими планами**: parallel-ось к [engine_state_unification.md](engine_state_unification.md). Та работа — про AI side (`BattleSnapshot.units` → `UnitView`). Эта — про bridge (Bevy ECS ↔ engine) и владение turn lifecycle. Не блокируют друг друга.

## Цель

Engine — **единственный владелец turn lifecycle**: refill AP/MP, regen ресурсов, tick статусов. Bridge — чистый translator событий + read-only projection в ECS. ECS-сторона перестаёт быть источником истины на границе раунда.

## Текущее состояние (baseline)

### Три точки мутации engine state, одна точка projection

| Фаза кадра | Мутирует engine | Пишет в ECS |
|---|---|---|
| `OnEnter(AwaitCommand)` (каждый раунд) | `init_state_from_ecs` — затирает engine из ECS | — |
| `CombatStep::TurnStart` | `engine_turn_start_system` → `start_actor_turn` (refill AP/MP, status tick) | — |
| `CombatStep::Command` | — | — (читает ECS) |
| `CombatStep::Execute` | `process_action_system` → `step()` | `project_state_to_ecs` |

Refill в TurnStart и projection в Execute разнесены через Command-шаг. `player_command_system` / `enemy_ai_system` читают **ECS**, видят значения с конца прошлого хода юнита (типично AP=0, MP=0 если ресурсы выгрызены), срабатывает auto-end check ([src/combat/command_input.rs:62](../../../src/combat/command_input.rs#L62), [src/combat/ai/system.rs:168](../../../src/combat/ai/system.rs#L168)) → silent EndTurn без какого-либо лога.

`init_state_from_ecs` ([src/combat/engine_bridge.rs:1435](../../../src/combat/engine_bridge.rs#L1435)) запускается каждый раз при входе в `AwaitCommand` — то есть на каждом round wrap. Это перетирает engine state значениями из ECS, делая engine-source-of-truth фиктивным на границах раундов.

### Симптомы в продакшене

Из лога пользователя (2026-05-21):
- В **раунде 2+** игрок входит в ход — `start_actor_turn` рефилит engine — но `player_command_system` читает ECS, видит AP=0 / MP=0 → `ActionInput::EndTurn` без логирования. Юзер не может ходить, в логе только `▶ Ход: Aldric / ○ Aldric завершил ход`.
- Аналогично enemy AI: в раунде 2 `Зверокров Налётчик` (потративший весь budget в раунде 1) сразу заканчивает ход, AI debug-блок даже не печатается (skip-path early return).
- Двойной "статус спал" в одном ходу (Aldric раунд 2 — `Защита/Провокация` спадают, потом re-apply'атся, потом снова сразу спадают) — побочка того, что `init_state_from_ecs` импортирует stale статусы из ECS, а `start_actor_turn` тикает их повторно.

### Архитектурный smell

Два writer'а engine state (`engine.step` и `engine.start_actor_turn`), но только `step` channel'ит изменения через event stream в bridge. Turn-start refill — скрытый side-channel. Любая future engine-mutating система, добавленная вне Execute-шага, повторит ту же ошибку класса.

## Целевая архитектура

```
Action::EndTurn → engine.step()
                    ↓
                 TurnEnded → AdvanceTurn cascade → [TurnSkipped]* → TurnStarted
                    ↓                                                   ↓
                 события                                          start_actor_turn (refill + tick)
                    ↓                                                   ↓
                 ManaRegenerated / EnergyRegenerated / StatusExpired / DotApplied / …
                    ↓
                 process_action_system → translate_end_turn_events
                    ↓
                 project_state_to_ecs  ← один кадр, один канал, ECS всегда в sync на границе
```

- Engine `step(EndTurn)` сам эмитит весь turn-lifecycle stream нового актора.
- Bridge `engine_turn_start_system` исчезает (его задача переехала внутрь engine).
- `init_state_from_ecs` срабатывает **один раз** за бой; engine state эволюционирует без re-import'а из ECS.
- `Local<Option<Entity>>` change-detection хак исчезает.

## Phase plan

### B0 — Engine: `start_actor_turn` внутрь cascade

**Файл**: `crates/combat_engine/src/step.rs`.

В `step_inner` после блока emit'а `TurnStarted` для `EndTurn`-пути:

```rust
// Текущий код (lines 698-705):
if matches!(&action, Action::EndTurn { .. }) {
    if let Some(next_actor) = state.turn_queue.current() {
        events.push(Event::TurnStarted { actor: next_actor });
    }
}
```

→

```rust
if matches!(&action, Action::EndTurn { .. }) {
    if let Some(next_actor) = state.turn_queue.current() {
        events.push(Event::TurnStarted { actor: next_actor });
        // Refill AP/MP, regen mana/energy, tick statuses for the incoming actor.
        // Was previously done by bridge's engine_turn_start_system; absorbed here
        // so the full turn-lifecycle flows through one event stream.
        events.extend(state.start_actor_turn(next_actor, content));
    }
}
```

`start_actor_turn` (`crates/combat_engine/src/state.rs:360`) уже эмитит `ManaRegenerated` / `EnergyRegenerated` + tick events. Дополнительно: refill AP=max_ap / MP=speed silently (без событий) — это нам и нужно для projection.

**Корнер-кейс**: если cascade застрял на budget-limit (все стан/мертвы, `turn_advance_budget` истёк), `turn_queue.current()` может оказаться dead. `start_actor_turn` сам проверяет `if u.is_alive()` перед refill (`state.rs:373`), tick тоже корректно работает для dead (sirota DoT). Безопасно.

**Тесты engine**: где-то могут быть ассерты на точное количество событий после `step(EndTurn)`. Найти, обновить ожидания. Ожидаемый scope: `crates/combat_engine/tests/end_turn.rs`, `crates/combat_engine/src/step.rs#tests`, `crates/combat_engine/tests/parity.rs`. Не пытаться сохранить старое поведение — добавляем events намеренно.

**Exit**: engine-тесты зелёные, новые ассерты ловят refill events.

---

### B1 — Bridge: удалить `engine_turn_start_system`

**Файлы**: `src/combat/engine_bridge.rs`, `src/combat/pipeline.rs`.

Удалить полностью функцию `engine_turn_start_system` (`engine_bridge.rs:648-698`) и её регистрацию в pipeline (`pipeline.rs:62-65`). `CombatStep::TurnStart` set остаётся пустым — пусть будет stable hook на будущее (как сделан `advance_turn_system`).

Перенести трансляцию refill events в `translate_tick_events` или в новую ветку `translate_end_turn_events`:

```rust
Event::ManaRegenerated { unit, current, max } => {
    if let Some(ent) = id_map.get_entity(*unit) {
        log.push(CombatEvent::ManaChanged { actor: ent, current: *current, max: *max });
    }
}
Event::EnergyRegenerated { unit, current, max } => {
    if let Some(ent) = id_map.get_entity(*unit) {
        log.push(CombatEvent::EnergyChanged { actor: ent, current: *current, max: *max });
    }
}
```

Проверить что `translate_tick_events` уже эти варианты не обрабатывает (иначе будет дубль-лог). Если не обрабатывает — добавляем туда, чтобы покрыть оба пути (поток через end-turn cascade и потенциальные другие источники).

**Корнер-кейс**: round 1 first actor. Сейчас `engine_turn_start_system` фирит для них тоже, делая `start_actor_turn` для первого актора в самом начале боя. После B1 этого не происходит. Варианты:
- (a) `CombatantBundle` уже даёт max AP / max MP — игрок начинает с полными ресурсами. Status tick для первого актора в раунде 1 — no-op (статусов нет, если только сценарий не препроставил pre-applied buffs).
- (b) Добавить one-shot вызов `state.start_actor_turn(first_actor, content)` в конец `init_state_from_ecs` (там есть `state` и `content`, есть `actor` через `turn_queue.current()` после `set_turn_queue`). События стрима — расшарить через events:Local или просто записать в `CombatLog` напрямую.

**Решение по корнер-кейсу**: вариант (b). Это симметрично — каждый ход актора начинается через `start_actor_turn`, включая первый. Pre-applied статусы будут тикать корректно (хотя сценариев с такой механикой пока нет, инвариант сохраняем).

Делать через расширение `init_state_from_ecs` или через отдельный `engine_start_first_turn_system` — на выбор имплементатора; разница вкусовая. Если отдельной системой — поставить её OnEnter(AwaitCommand) ПОСЛЕ `init_state_from_ecs` в chain.

**Exit**: `engine_turn_start_system` удалена; AP/MP/regen events корректно появляются в `CombatLog` через bridge translator; round 1 first actor получает полный turn-start treatment.

---

### B2 — `init_state_from_ecs` только один раз за бой

**Файл**: `src/combat/engine_bridge.rs:1435`.

Добавить `Local<bool>` guard:

```rust
pub fn init_state_from_ecs(
    // existing params...
    mut already_initialized: Local<bool>,
) {
    if *already_initialized { return; }
    *already_initialized = true;
    // ...existing body...
}
```

Также добавить sentinel-reset когда выходим из `AppState::Combat` обратно (после Victory/Defeat). Если сейчас при следующем бою `Local<bool>` сохраняется между сеансами — нужно сбрасывать. Способ: дополнительная система OnExit(AppState::Combat) которая фирит сигнал... либо `Local<bool>` привязать к `CombatContext.encounter` — если encounter сменился, сбрасываем. Проверить как Bevy ведёт себя с `Local<T>` между state-переходами; если переживает — нужен явный reset.

**Альтернатива (если Local<bool> не подходит)**: переместить `init_state_from_ecs` из `OnEnter(AwaitCommand)` в `OnEnter(StartRound)` под guard `ctx.round == 1`, т.е. отвязать от каждого round wrap. Семантически чище.

**Exit**: `init_state_from_ecs` запускается ровно один раз на бой. Engine state на round 2+ не перетирается; статусы applied во время боя сохраняются без round-trip через ECS.

---

### B3 — Регрессионные тесты

**Файл**: `tests/combat/handoff.rs` (расширить тот, что добавлен в `a3915de`).

Тесты:

1. **`player_can_act_on_round_2_after_exhausting_resources`**
   - Spawn hero (Player) + enemy (Enemy).
   - Hero ходит в раунде 1, тратит AP=0, MP=0 (cast + полный move).
   - Enemy завершает ход.
   - Тикаем app до начала раунда 2 hero-хода.
   - **Assert**: hero's `ActionPoints.action_points == max_ap`, `movement_points > 0` ДО того, как `player_command_system` побежит в Command. Не должно быть `ActionInput::EndTurn` для hero в очереди сообщений на момент Command-старта.
   - **Assert**: hero's `ActiveCombatant` присутствует.

2. **`enemy_ai_does_not_skip_round_2_after_exhausting_resources`**
   - Аналогично, но для enemy. Проверяем что `enemy_ai_system` НЕ хитает early-return по no-AP/no-MP в начале раунда 2.

3. **`status_does_not_tick_twice_per_turn`**
   - Apply статус с `rounds_remaining = 2` на актора.
   - Сделать round wrap.
   - Тикнуть app до начала следующего хода актора.
   - **Assert**: статус остался с `rounds_remaining = 1` (тикнулся один раз), а не expired (тикнулся дважды).

4. **`first_actor_round_1_gets_start_actor_turn_treatment`**
   - Spawn unit с pre-applied статусом (rounds_remaining = 1).
   - Combat start.
   - **Assert**: на первом ходу первого актора статус expired (один tick прошёл). Это валидирует корнер-кейс B1.

**Scaffold**: использовать `movement_app` / `spawn_at` / `test_hero` / `test_enemy` как в существующих handoff-тестах. См. `tests/combat/aoo.rs` для расширенного builder pattern.

**Exit**: 4 новых теста зелёные; вся `cargo test --test combat` зелёная.

---

### B5 — Death of current actor = involuntary turn end

**Цель**: устранить класс зависаний "current actor died mid-action, turn never advances". Engine — единственный владелец turn lifecycle, включая involuntary endings.

**Симптом**: AI пишет `Move` для актора, во время движения провоцируется AoO, AoO убивает актора. `step(Move)` возвращается успешно, но `turn_queue.current()` всё ещё указывает на труп. Никто не пишет `ActionInput::EndTurn` для мёртвого — `enemy_ai_system` и `player_command_system` молча возвращают на `c.vital.is_alive() == false`. Игра зависает.

Текущий workaround — auto-end в Cast-обработчике bridge'а ([engine_bridge.rs:858](../../../src/combat/engine_bridge.rs)) когда `AP=0 && MP=0`. Это:
1. Локальный фикс только для Cast, Move не покрыт.
2. Условие неполное — dead-актор может иметь AP/MP > 0 (если умер до payCost).
3. Дублирует логику между bridge и engine.

**Engine changes**:

1. **`crates/combat_engine/src/effect.rs`** — `apply_effect` для `Effect::Death`:

   ```rust
   Effect::Death { unit } => {
       // ... existing status cleanup + hp=0 ...

       let mut derived: Vec<Effect> = statuses_to_clean
           .into_iter()
           .map(|status| Effect::RemoveStatus { target: *unit, status })
           .collect();

       let mut ctx = ApplyCtx::default();

       // If dying actor was the current turn-holder, force-end their turn.
       // Cascade emits TurnEnded for them + AdvanceTurn to settle on next alive.
       // step_inner's "current changed" check below picks up TurnStarted +
       // start_actor_turn for the new actor.
       if state.turn_queue.current() == Some(*unit) {
           ctx.turn_skip_events.push(Event::TurnEnded { actor: *unit });
           derived.push(Effect::AdvanceTurn);
       }

       (derived, ctx)
   }
   ```

   Reuse `ctx.turn_skip_events` (already drained into main events by step.rs:597) — не нужно новое поле в ApplyCtx.

2. **`crates/combat_engine/src/step.rs`** — `step_inner` TurnStarted emission. Снять gating на `Action::EndTurn`, эмитить когда `turn_queue.current()` изменился за step:

   ```rust
   // Текущий код (после Phase 1):
   if matches!(&action, Action::EndTurn { .. }) {
       if let Some(next_actor) = state.turn_queue.current() {
           events.push(Event::TurnStarted { actor: next_actor });
           events.extend(state.start_actor_turn(next_actor, content));
       }
   }
   ```

   →

   ```rust
   // Capture initial current at top of step_inner:
   let initial_current = state.turn_queue.current();

   // ... existing logic, including pump loop ...

   // After pump loop: emit TurnStarted + refill whenever current changed.
   // Covers Action::EndTurn (legitimate ending) AND death-mid-action
   // (Effect::Death of current actor cascades to AdvanceTurn).
   let final_current = state.turn_queue.current();
   if initial_current != final_current {
       if let Some(next_actor) = final_current {
           events.push(Event::TurnStarted { actor: next_actor });
           events.extend(state.start_actor_turn(next_actor, content));
       }
   }
   ```

   Перенос `let initial_current = ...` в самое начало `step_inner` (после декларации events/effect_queue, до pre-validate).

**Bridge changes**:

3. **`src/combat/engine_bridge.rs`** — `translate_move_events` и `translate_cast_events` сейчас игнорируют `TurnEnded`/`TurnStarted`/`TurnSkipped`/`RoundStarted` в "no-op pins for exhaustiveness". После B5 эти события могут появиться в Move/Cast stream'ах (death-mid-action кейс). Решение:

   Extract из `translate_end_turn_events` общий helper `translate_turn_lifecycle_events(events, id_map, commands, log, next_phase)` обрабатывающий 4 turn-related variants + `AuraStatusGained`/`Lost`. Вызвать из всех трёх translator'ов. Move/Cast стримы редко содержат эти события (только в edge-кейсах death-mid-action и phase transitions), но обработка должна быть корректной.

   Альтернатива — inline duplicate logic в каждый translator. Хуже, но проще. Pick whichever — depends on what looks cleaner after reading the code.

4. **Удалить устаревший workaround в Cast handler ([engine_bridge.rs:858](../../../src/combat/engine_bridge.rs))**: блок "End turn only when both AP and MP are exhausted, and the ability isn't GrantMovement" больше не нужен. После B5 engine сам эмитит AdvanceTurn когда current actor умер. Auto-end на исчерпание ресурсов — отдельный invariant (turn автоматически НЕ заканчивается если actor жив но resourceless); если хотите сохранить — оставить, но обновить комментарий что dead-case теперь покрыт engine'ом.

   **Решение**: сохраняем auto-end на ресурсный exhaust (отдельная UX feature — "Cast spent everything, end turn for me"). После B5 одно условие (`AP=0 && MP=0`) станет subset более общего engine-handling'а (`!alive`), не пересекаясь semantically. Никаких изменений в Cast handler не требуется кроме обновления комментария.

**Tests engine**:

5. `crates/combat_engine/tests/effect.rs` или новый файл — тест `death_of_current_actor_derives_advance_turn`:
   - State с двумя alive юнитами, queue [A, B], current = A.
   - `apply_effect(state, Effect::Death { unit: A }, content)`.
   - Assert: `derived` содержит `Effect::AdvanceTurn`; `ctx.turn_skip_events` содержит `Event::TurnEnded { actor: A }`.

6. `crates/combat_engine/tests/step.rs` — тест `current_actor_dies_mid_move_via_aoo_settles_on_next`:
   - State с heroA (player, has melee AoO), enemyB (current). enemyB attempts move that triggers AoO. AoO kills B.
   - `step(state, Action::Move { actor: B, path }, &mut rng, &content)`.
   - Assert: events stream содержит `UnitDied { B }` → `TurnEnded { B }` → (potentially `TurnSkipped` for any dead in queue) → `TurnStarted { A }` (или next alive).
   - Assert: `state.turn_queue.current() == Some(A)` после step.

7. Существующие тесты на event-counts после `step(EndTurn)` — пройдут без изменений (TurnStarted emission уже срабатывает для EndTurn-пути; gating change не меняет поведение для нормального flow).

**Не делаем**:
- Не трогаем engine's existing `start_actor_turn` или `start_round`.
- Не вводим "involuntary end reason" в events stream — `Event::TurnEnded` нейтрален к причине; различение voluntary vs involuntary — задача UI/log layer, не engine.

**Exit**: новый engine test зелёный + bridge корректно обрабатывает турн-события в Move/Cast стримах + user scenario (AI moves into AoO that kills them) больше не виснет.

---

### B4 — Cleanup и docs

- Обновить `docs/engine-architecture.md` § turn lifecycle ownership — отметить что refill теперь происходит в engine `step()`.
- Обновить `docs/combat-pipeline.md` § per-system schedule — отметить что `CombatStep::TurnStart` пустой (но оставлен как hook).
- Удалить TODO/комментарии в `src/combat/engine_bridge.rs` про `engine_turn_start_system` если такие найдутся.
- `CombatStateRes` doc comment ("Phase D-step-2: engine state cloned into BattleSnapshot.state at build time") — оставить как есть, относится к AI-side.

**Exit**: doc consistency, no dangling references.

---

## Зависимости

```
B0 (engine) ── B1 (bridge) ── B2 (init guard) ── B5 (death) ── B3 (tests) ── B4 (docs)
```

B5 добавлен после первого playthrough — закрывает класс "current actor died mid-action, turn never advances". Логически продолжение B0+B1 (engine — sole owner of turn lifecycle, включая involuntary endings), потому идёт перед регрессионными тестами B3 (чтобы B3 покрывал и этот класс через тест из §B5 5–6, плюс bridge-level integration test "AI moves into lethal AoO, combat continues").

Возможны параллельно: B0 и B2 (разные слои), но B1 зависит от B0. Чище делать линейно — каждый коммит атомарен и зелёный.

## Откатимость

Каждая фаза — отдельный коммит. Откат любого коммита оставляет рабочее состояние (с известными багами, которые мы фиксим). Особенно важно для B0 — engine change touches много мест.

## Risks & mitigations

| ID | Риск | Mitigation |
|---|---|---|
| BR1 | Engine-тесты падают массово на B0 из-за изменившегося event count | Ожидаемо. Прогнать `cargo test -p combat_engine`, обновить ассерты по списку. Не пытаться сохранить старое event count — это и есть цель change'а. |
| BR2 | `Local<bool>` в B2 не сбрасывается между боями → второй бой не получит init | Проверить поведение Bevy. Если переживает — переехать на `OnEnter(StartRound) if round == 1` (альтернатива в B2). |
| BR3 | Дубль-логирование Mana/Energy events если перенос в `translate_tick_events` пересечётся с существующим handler'ом | Найти все callsites `translate_tick_events`, убедиться что эти варианты ранее не обрабатывались. `engine_turn_start_system` был единственным emitter'ом — `translate_tick_events` не должна была их видеть. |
| BR4 | Round 1 first actor lose status tick если выбрать вариант (a) корнер-кейса B1 | Идём вариантом (b) — явный `start_actor_turn` через `init_state_from_ecs` или отдельную one-shot систему. |
| BR5 | Замедление test suite от добавления интеграционных тестов | 4 теста с минимальным scaffold'ом — пренебрежимо. |
| BR6 | Скрытая зависимость кого-то на `engine_turn_start_system` (например, debug overlay читает `last_active`) | grep для `engine_turn_start_system` и `last_active` перед удалением. Аудит — раз. |

## Не делаем (out of scope)

- **Не сворачиваем `init_state_from_ecs` в OnEnter(AppState::Combat)** — там ECS TurnQueue ещё не построена (build_turn_order не отработала). Оставляем хук на AwaitCommand с guard'ом.
- **Не трогаем AI-side** — `BattleSnapshot.units`, `UnitSnapshot`, `unit_snapshots_to_combat_state` — это план [engine_state_unification.md](engine_state_unification.md) (U0–U7).
- **Не унифицируем `project_state_to_ecs`** — оставляем как есть в Execute. После B0 этого достаточно: все engine mutations в кадре уже происходят до Execute (через `process_action_system`), projection их подбирает.
- **Не убираем `build_turn_order`** — он всё ещё нужен для initiative roll на раунде 1 и для управления ECS TurnQueue (используется не только в bridge — `tests/combat/aoo.rs` его трогает напрямую).

## Критические файлы

- `crates/combat_engine/src/step.rs` — `step_inner`, TurnStarted emission point (B0).
- `crates/combat_engine/src/state.rs` — `start_actor_turn` (B0 reference, не меняем).
- `crates/combat_engine/src/effect.rs` — `skip_or_settle_current`, `Effect::AdvanceTurn`, `Effect::BumpRound` (B0 reference).
- `src/combat/engine_bridge.rs` — `engine_turn_start_system` (удалить B1), `translate_end_turn_events` / `translate_tick_events` (расширить B1), `init_state_from_ecs` (guard B2).
- `src/combat/pipeline.rs` — schedule registration (B1).
- `tests/combat/handoff.rs` — regression tests (B3).
- `docs/engine-architecture.md`, `docs/combat-pipeline.md` — docs (B4).

## Что меняется в наблюдаемом поведении

После B0+B1:
- Поток событий из `step(EndTurn)` становится длиннее: добавляются ManaRegenerated / EnergyRegenerated / DotApplied / StatusExpired для нового актора. Логи боя станут чуть подробнее на границах ходов (но это были события, которые и раньше эмитились, просто из другой системы).
- AI replay/mining workflow: если в логах хранятся подсчёты событий per-tick, они изменятся. Возможна регенерация `tests/baselines/baseline_v36.jsonl` через `cargo run --release --bin replay_ai_log`.

После B2:
- На round 2+ engine state НЕ перетирается ECS-копией. Статусы applied во время боя сохраняются без round-trip. Это устраняет "двойной tick" симптом.

## Exit criteria для всего плана

- `cargo check --tests` clean. ✅
- `cargo test` зелёный (включая регрессионные тесты в `tests/combat/handoff.rs`). ✅
- В user-репро (раунд 2 после exhaust) игрок может ходить. ✅
- Лог боя: одна `▶ Ход: X` + одна `○ X завершил ход` на ход, без silent skip'ов. ✅
- `engine_turn_start_system` отсутствует в коде. ✅
- `init_state_from_ecs` запускается один раз за бой (guard `ctx.round < 2`). ✅

---

## Hand-off / completion (2026-05-21)

### Phases as actually shipped

| Phase | Commit(s) | Notes |
|---|---|---|
| Pre-work (mid-round + first-actor-dead) | `a3915de` | ActiveCombatant lifecycle on TurnEnded/TurnSkipped + build_turn_order picks first-alive |
| B0+B1 | `faaaded` | Engine cascade owns `start_actor_turn`; `engine_turn_start_system` deleted; `engine_start_first_turn_system` added for round-1 init |
| B2 | `ebde94e` → `f5bfc80` → `1b522d3` | init_state_from_ecs once-per-combat. First version used encounter-identity guard (regressed second-combat); second tried `ctx.round == 1` (regressed `bridge_smoke` tests calling init with `ctx.round = 0`); final settled on `ctx.round < 2` covering both production and test paths |
| B5 | `4879934` | `Effect::Death` of current actor derives `AdvanceTurn`; `step_inner` emits `TurnStarted` whenever `(current, round)` changed (not only on `Action::EndTurn`) |
| Engine mirror teardown | `80ae900` → `0e09215` | First version inline in `despawn_combatants`/`restart_combat_system`; refactored to `reset_engine_mirrors_on_exit_combat` + `reset_engine_mirrors_on_restart` owned by `CombatPipelinePlugin` (layering fix) |
| B-prime (NEW — added after first playthrough panic) | `1b522d3` + `eb3ee8a` + `ef45aea` | `BattleSnapshot.uid_to_entity: HashMap<UnitId, Entity>` replaces `Entity::from_bits(u.id.0)` shortcut in 9 callsites (7 in snapshot lookup methods + `UnitView::entity` + `target_selection_score`). Required to fix panic on summoned units whose synthetic UnitId bits aren't valid Bevy Entity bits. |
| Engine `enemy_phases` pop | `5ffd874` | `Effect::EnterPhase` apply pops `unit.enemy_phases[0]`. Was never popped → same phase re-fired on every damage-below-threshold → boss healed-to-full forever instead of dying |
| B3 + B4 (this commit) | _this commit_ | 5 new regression tests in `tests/combat/handoff.rs` + docs sync |

### Bugs found and fixed during execution

The plan as originally written anticipated 3 fixes (B0+B1, B2, B5). Actual
execution surfaced 7 distinct bug classes — each got its own commit:

1. **ActiveCombatant lifecycle** (mid-round handoff + dead-first-actor at round wrap) — pre-Phase-1 latent from the 4e "ECS deletion sweep". Caused silent freezes after the first player turn or after a round wrap onto a dead first-initiative actor.
2. **Frame-ordering at round boundary** (B0+B1) — engine refilled AP/MP in `TurnStart`, but ECS lagged one frame; Command-step systems read stale ECS and auto-ended silently.
3. **Double tick on round wrap** (B2) — `init_state_from_ecs` ran every `OnEnter(AwaitCommand)`, re-importing stale ECS statuses; then `start_actor_turn` ticked them a second time.
4. **Death-mid-action hang** (B5) — AoO kills the mover during their own `Move` action; engine `step()` returns successfully but no system writes EndTurn for the dead actor → silent freeze.
5. **Multi-combat-session pollution** (engine mirror teardown) — `despawn_combatants` cleared ECS resources but not `CombatStateRes` / `UnitIdMap` / `PendingPhaseTransitions`; next combat's `StartRound → project_state_to_ecs` projected stale combat-1 unit positions into freshly-cleared `HexPositions` → collision panic.
6. **Synthetic UnitId `from_bits` panic** (B-prime) — engine allocates `UnitId(SYNTHETIC_UID_BASE | counter)` for `Effect::Spawn`; bits aren't valid Bevy Entity. `BattleSnapshot` lookups used `Entity::from_bits(u.id.0)` shortcut → panic on AI tick after any summon. Fixed by introducing an explicit `uid_to_entity` map populated from `UnitIdMap`.
7. **Engine `enemy_phases` never popped** — `check_phase_trigger` peeked at `enemy_phases.first()` but engine never consumed the entry; bridge popped only ECS-side. Boss with `heal_to_full` phase healed to full on every damage-below-threshold infinitely.

### Long-term direction

The invariant established is: **"Engine UnitId and Bevy Entity are distinct
namespaces, always translated via an explicit map"**. `from_bits` shortcuts
across the namespace boundary are gone from production code.

Next steps (out of scope for this plan, lined up in adjacent work):

- **Bevy component hooks** for automatic `id_map` maintenance — `on_add<Combatant>` / `on_remove<Combatant>` to eliminate manual `id_map.insert/clear/from_ecs` callsites. Smaller follow-up.
- **engine_state_unification U5–U7** ([adjacent plan](engine_state_unification.md)) — drop `UnitSnapshot` / `BattleSnapshot.units`; cache keyed by `UnitId` directly. Removes one direction of the translation map.
- **Engine spawn allocator** (deferred) — alternative architectural option where engine asks bridge for an Entity bits when spawning, eliminating synthetic UIDs entirely. Doesn't help AI sim (which can't spawn real Bevy entities) → kept as theoretical option, not pursued.
