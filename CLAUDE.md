# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build          # компиляция
cargo run            # запуск (открывает окно Bevy)
cargo check          # быстрая проверка без линковки
cargo clippy         # линтер
```

## Architecture

Bevy 0.15, ECS. Два уровня состояний:

- `AppState` (`app_state.rs`) — `Boot | MainMenu | Overworld | Combat`
- `CombatPhase` (`app_state.rs`) — SubState активен только при `AppState::Combat`:
  `StartRound → AwaitCommand → ResolveAction → Cleanup → Victory/Defeat`

### Боевой pipeline (один ход)

```
player_command_system   (AwaitCommand)  →  UseAbility event
validate_action_system  (ResolveAction) →  пропускает невалидные, форвардит валидные
resolve_action_system   (ResolveAction) →  кубики → ApplyDamage / ApplyStatus / EndTurn events
cleanup_system          (Cleanup)       →  применяет урон/статусы, переводит ход / завершает бой
```

### Модули

| Путь | Что делает |
|------|-----------|
| `core/` | `AbilityId`, `StatusId`, `DiceRng` (LCG, без внешних зависимостей), `DiceExpr` |
| `game/components.rs` | ECS-компоненты: `Combatant`, `Stats`, `Vital`, `ActionPoints`, `StatusEffects` |
| `game/resources.rs` | `CombatContext`, `TurnQueue`, `CombatLog`, `GameDb`, `SelectionState` |
| `game/messages.rs` | Events: `StartCombat`, `UseAbility`, `ApplyDamage`, `ApplyStatus`, `EndTurn` |
| `game/bundles.rs` | `CombatantBundle`, хелперы `party_member_bundle` / `enemy_bundle` |
| `combat/` | Системы боя по одной на файл (turn_order, command_input, validation, resolution, cleanup) |
| `content/` | `AbilityDef`, `StatusDef`, `ClassDef` + дефолтный контент; ядро боя их не знает напрямую |
| `ui/` | Заглушки HUD и лога (временно `println!`, под замену Bevy UI) |

### Добавление контента

Новая способность — только `content/abilities.rs`: добавить `AbilityId`-константу, `AbilityDef` в `default_abilities()`. Боевые системы читают через `GameDb` из ресурса.

Новый статус — аналогично `content/statuses.rs`.

### Bevy версия

Используется 0.18. Messaging API: `#[derive(Message)]`, `MessageReader`, `MessageWriter`, `.add_message::<T>()`, `writer.write(msg)` — это переименованный Event-механизм из 0.15 (`Event`/`EventReader`/`EventWriter`/`send`).
