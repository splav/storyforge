# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build          # компиляция
cargo run            # запуск (открывает окно Bevy)
cargo check          # быстрая проверка без линковки
cargo clippy         # линтер
cargo test           # юнит + интеграционные тесты
```

## Bevy версия

Используется 0.18. Messaging API: `#[derive(Message)]`, `MessageReader`, `MessageWriter`, `.add_message::<T>()`, `writer.write(msg)` — это переименованный Event-механизм из 0.15 (`Event`/`EventReader`/`EventWriter`/`send`).

## Architecture

Bevy ECS. Два уровня состояний:

- `AppState` (`app_state.rs`) — `Boot | MainMenu | Overworld | Combat`
- `CombatPhase` (`app_state.rs`) — SubState активен только при `AppState::Combat`:
  `StartRound → AwaitCommand → Victory/Defeat`

### Боевой pipeline

Все 8 систем зарегистрированы `.chain()` в `CombatPhase::AwaitCommand` и выполняются последовательно каждый кадр:

```
turn_start_system       →  восстановление ресурсов (мана +1) при смене активного актора
skip_dead_turn_system   →  мёртвый → мгновенный EndTurn
skip_stunned_turn_system→  оглушён → ap.action=false + EndTurn
player_command_system   →  клавиатура (1-5/Tab/Enter) → UseAbility
enemy_ai_system         →  случайная способность + цель → UseAbility (приоритет провокации)
validate_action_system  →  UseAbility → ValidatedAction (проверяет ресурсы, AP, цель)
resolve_action_system   →  ValidatedAction → кубики → ApplyDamage / ApplyHeal / ApplyStatus / EndTurn
cleanup_system          →  применяет урон/лечение/статусы, ярость, тикает длительности, проверяет победу/поражение, передаёт ход
```

`StartRound` → `build_turn_order` (инициатива d20 + мод. ловкости, только в 1-м раунде) → `AwaitCommand`.

### Поток сообщений (один ход)

```
UseAbility → ValidatedAction → ApplyDamage/ApplyHeal/ApplyStatus → EndTurn
```

Все сообщения собираются и применяются в `cleanup_system` в том же кадре.

### Модули

| Путь | Что делает |
|------|-----------|
| `core/ids.rs` | Макрос `string_id!()` → `AbilityId`, `StatusId`, `WeaponId` |
| `core/rng.rs` | `DiceRng` (LCG, без внешних зависимостей), `DiceExpr` |
| `core/mod.rs` | `modifier(stat) = stat >> 1` — floor(stat/2), диапазон характеристик -5..10 → модификаторы -3..+5 |
| `game/components.rs` | ECS-компоненты: `Combatant`, `CombatStats` (6 характеристик D&D), `Vital`, `ActionPoints`, `StatusEffects`, `Rage`, `Mana` |
| `game/resources.rs` | `CombatContext`, `TurnQueue`, `CombatLog` + `CombatEvent` (enum), `GameDb`, `SelectionState` |
| `game/messages.rs` | Messages: `StartCombat`, `UseAbility`, `ValidatedAction`, `ApplyDamage`, `ApplyHeal`, `ApplyStatus`, `EndTurn` |
| `game/bundles.rs` | `CombatantBundle`, хелперы `warrior_bundle` / `enemy_bundle` |
| `combat/turn_order.rs` | `build_turn_order` — инициатива, порядок хода |
| `combat/turn_start.rs` | `turn_start_system` — ресурсы в начале хода (мана +1) |
| `combat/skip_dead.rs` | `skip_dead_turn_system`, `skip_stunned_turn_system` |
| `combat/command_input.rs` | `player_command_system` — ввод игрока |
| `combat/enemy_ai.rs` | `enemy_ai_system` — ИИ врагов (приоритет провокации) |
| `combat/validation.rs` | `validate_action_system` — проверка ресурсов, AP, цели |
| `combat/resolution.rs` | `resolve_action_system` — броски, эффекты |
| `combat/cleanup.rs` | `cleanup_system` — применение эффектов, тик статусов, смерть, переход хода |
| `content/` | TOML-загрузчики: `AbilityDef`, `StatusDef`, `WeaponDef`, `ClassDef`, `EncounterDef` |
| `ui/combat_ui.rs` | HUD: фаза, порядок хода, список бойцов, панель способностей |
| `ui/console_log.rs` | `fmt_event` — форматирование `CombatEvent` в строку (рус.), `print_log_system` |
| `ui/log_ui.rs` | `update_log` — последние 6 событий в HUD |

### Ключевые механики

- **Характеристики**: 6 стат D&D (strength..charisma), диапазон -5..10, используются через `modifier(stat)`.
- **Урон**: физический (weapon_attack/damage) — с учётом брони; магический (spell_damage) — `pierces_armor: true`.
- **Статусы**: `armor_bonus` (физ.), `damage_taken_bonus` (уязвимость ко всему), `skips_turn`.
- **Длительность статусов**: тикает по EndTurn **наложившего** (`ActiveStatus.applier`). При наложении `duration + 1` чтобы компенсировать тик в том же кадре. Duration=1 = активен до конца следующего хода наложившего.
- **Ярость** (Rage): +1 за нанесённый/полученный урон, начальная 0, тратится на способности.
- **Мана** (Mana): +1 в начале хода владельца (`turn_start_system`), начальная = max, тратится на заклинания.
- **Провокация** (taunted): враги атакуют только провокатора (`enemy_ai_system`).

### Добавление контента

Весь контент — в TOML-файлах `assets/data/`. Код не содержит хардкода конкретных способностей/статусов (кроме `"taunted"` в `enemy_ai.rs`).

- **Новая способность**: добавить запись в `abilities.toml`. Поля: `id`, `name`, `target_type` (single_enemy/single_ally/myself), `effect` (weapon_attack/damage/spell_damage/heal/none), `dice_count/dice_sides`, `rage_cost/mana_cost`, `statuses = [{id, on, duration}]`.
- **Новый статус**: добавить в `statuses.toml`. Поля: `id`, `name`, `armor_bonus`, `damage_taken_bonus`, `skips_turn`.
- **Новый класс**: добавить в `classes.toml`. 6 характеристик, weapon_id, ability_ids, rage_max/mana_max.
- **Новая встреча**: добавить в `encounters.toml`.

### Тесты

- `tests/combat.rs` — интеграционные тесты: validation (3), cleanup (3). Минимальный Bevy-app с целевыми системами.
- Юнит-тесты: `Vital` (components.rs), `TurnQueue` (resources.rs), `DiceRng` (rng.rs).
