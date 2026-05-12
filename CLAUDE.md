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

### 2. Думать о общем, а не целом

- Изучать варианты
- Подмечать противоречия и странные решения
- При возникновении сомнений или выбора всегда спрашивать, а не молча предполагать и делать
- предлагай рефторинги если видишь хороший варианта (даже если сложный)

### 3. Тесты
- тесты должны покрывать освновную логику и сложные краевые случаи, а не тестировать что 2=2
- тесты должны быть читаемыми
- тесты должны быть компактными
- тесты не должны дублировать друг друга
- тесты следует параметризовывать где это возможно для экономии и ясности

### 4. Документация
- всегда поддерживай актуальной, обновляй после изменений соответствующие разделы
- документация должны быть логически структурированной, иерархичной, логически связной
- разбивай файлы на части, если они становятся слишком большими или слишком про разное

---

## Tools
* делегируй задачи по правке кода агенту
* prefer Grep tool over bash grep
* prefer using ya tool ast-index is suitable
* see `.claude/rules/graphify.md` for knowledge-graph rules (main session + subagent guidance)
* see `.claude/rules/ast-index.md` for ast-index rules

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
| [AI](docs/ai/ai.md) | Обзор + ссылки на per-layer доки (decision-cycle, scoring, intent, critics, …) |
| [AI Replay](docs/ai/replay.md) | Оффлайн-реплей `logs/*.jsonl`: `cargo run --bin replay_ai_log -- <file>` — пересчёт скоров/sanity текущим кодом, поиск изменившихся решений |
| [AI Mining](docs/ai/mining.md) | Агрегированная статистика по логам: `cargo run --release --bin mine_ai_logs -- --dir logs/` — band coverage, agenda win-rate, continuation outcomes |
| [Content Guide](docs/content-guide.md) | Как добавлять способности, статусы, оружие, классы, встречи, сценарии |
| [Hex Grid](docs/hex-grid.md) | Координаты even-r, соседи, расстояние, pathfinding, правила движения |
| [World](docs/world.md) | Описание мира, фракции, система магии |

### Tests

- `tests/combat.rs` — интеграционные (validation 3 + effects 3)
- Юнит-тесты: `Vital`, `TurnQueue`, `DiceRng`, `hex_distance`, `find_path`

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

Rules:
- ALWAYS read graphify-out/GRAPH_REPORT.md before reading any source files, running grep/glob searches, or answering codebase questions. The graph is your primary map of the codebase.
- IF graphify-out/wiki/index.md EXISTS, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
- **When dispatching subagents** (Plan, Explore, general-purpose), include graphify guidance in the spawn prompt — see `.claude/rules/graphify.md` "Rules for Subagents" for ready-to-paste blocks. Subagents do NOT inherit these rules automatically.
