# CLAUDE.md

## Commands

```bash
cargo build          # компиляция
cargo run            # запуск (открывает окно Bevy)
cargo check          # быстрая проверка без линковки
cargo clippy         # линтер
cargo test           # юнит + интеграционные тесты
```

## Bevy 0.18

Messaging API: `#[derive(Message)]`, `MessageReader`, `MessageWriter`, `.add_message::<T>()`, `writer.write(msg)` — переименованный Event-механизм из 0.15.

## Overview

Bevy ECS тактическая RPG. Состояния: `AppState` (Boot → Story ↔ Combat → MainMenu), `CombatPhase` (StartRound → AwaitCommand → Victory/Defeat). Сценарии из TOML: чередование сюжетных экранов и боёв. Весь контент data-driven (`assets/data/*.toml`).

## Docs

| Документ | Содержание |
|----------|-----------|
| [Architecture](docs/architecture.md) | Модули, состояния, карта файлов, зависимости |
| [Combat Pipeline](docs/combat-pipeline.md) | 10 систем цепочки, поток сообщений, детали каждой системы |
| [Mechanics](docs/mechanics.md) | Характеристики, урон, лечение, мана/ярость, статусы, AI, инициатива |
| [Content Guide](docs/content-guide.md) | Как добавлять способности, статусы, оружие, классы, встречи, сценарии |
| [Hex Grid](docs/hex-grid.md) | Координаты even-r, соседи, расстояние, pathfinding, правила движения |

## Tests

- `tests/combat.rs` — интеграционные (validation 3 + effects 3)
- Юнит-тесты: `Vital`, `TurnQueue`, `DiceRng`, `hex_distance`, `find_path`
