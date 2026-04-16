# CLAUDE.md

## Guidelines

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

### 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

### 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

### 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

### 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.


## Project

### Commands

```bash
cargo build          # компиляция
cargo run            # запуск (открывает окно Bevy)
cargo check          # быстрая проверка без линковки
cargo clippy         # линтер
cargo test           # юнит + интеграционные тесты
```

### Bevy 0.18

Messaging API: `#[derive(Message)]`, `MessageReader`, `MessageWriter`, `.add_message::<T>()`, `writer.write(msg)` — переименованный Event-механизм из 0.15.

### Overview

Bevy ECS тактическая RPG. Состояния: `AppState` (Boot → Story ↔ Combat → MainMenu), `CombatPhase` (StartRound → AwaitCommand → Victory/Defeat). Сценарии из TOML: чередование сюжетных экранов и боёв. Весь контент data-driven (`assets/data/*.toml`).

### Docs

| Документ | Содержание |
|----------|-----------|
| [Architecture](docs/architecture.md) | Модули, состояния, карта файлов, зависимости |
| [Combat Pipeline](docs/combat-pipeline.md) | 10 систем цепочки, поток сообщений, детали каждой системы |
| [Mechanics](docs/mechanics.md) | Характеристики, урон, лечение, мана/ярость, статусы, инициатива |
| [AI](docs/ai.md) | Роли, скоринг, сложность, snapshot, карты влияния, debug overlay |
| [Content Guide](docs/content-guide.md) | Как добавлять способности, статусы, оружие, классы, встречи, сценарии |
| [Hex Grid](docs/hex-grid.md) | Координаты even-r, соседи, расстояние, pathfinding, правила движения |
| [World](docs/world.md) | Описание мира, фракции, система магии |

### Tests

- `tests/combat.rs` — интеграционные (validation 3 + effects 3)
- Юнит-тесты: `Vital`, `TurnQueue`, `DiceRng`, `hex_distance`, `find_path`
