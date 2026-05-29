# CLAUDE.md

## Guidelines

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

### 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If something is unclear, stop. Name what's confusing. Ask.
- If you see legacy or smelling code:
   * if you are touching it anyway - propose best way to improve and ask
   * if not - write about it in report after finishing task
- Always use planner to make an architecture plan for implementer
- Always show your architecture plan to a critic agent

### 2. Думать обо всей архитектуре в целом

- Изучать варианты
- Подмечать противоречия и странные решения
- При возникновении сомнений или выбора всегда спрашивать, а не молча предполагать и делать
- предлагай рефторинги если видишь хороший вариант (даже если сложный)

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

Dev-цикл идёт через фичу `dev`, которая включает `bevy/dynamic_linking`
(Bevy линкуется как dylib → инкрементальная линковка ~3× быстрее,
бинарь 9.5 MB вместо 146 MB). Релиз и бенчмарки запускать **без** этой
фичи — нужен статический линк.

```bash
# Dev cycle (фича dev = bevy/dynamic_linking)
cargo build  --features dev      # сборка
cargo run    --features dev      # запуск (открывает окно Bevy)
cargo check  --features dev      # быстрая проверка без линковки
cargo clippy --features dev      # линтер

# Тесты — через cargo-nextest (3× быстрее warm, 10× при filter)
cargo nextest run --workspace --features dev               # ВСЕ тесты воркспейса: ~1332 (~4 s warm)
cargo nextest run --features dev                            # только storyforge pkg: ~1300 (см. note ниже)
cargo nextest run --features dev -E 'test(/combat::ai/)'    # только AI tests (~2.6 s warm)
cargo nextest run --features dev -E 'test(=foo)'            # один конкретный тест
cargo test        --features dev --doc                      # doc-tests (nextest их не умеет)

# Релиз / бенчмарки — БЕЗ --features dev
cargo build --release
cargo bench
```

> **Важно про охват тестов.** `dev` — фича пакета `storyforge`, поэтому
> `cargo nextest run --features dev` **без** `--workspace` собирает test-таргеты
> только этого пакета: интеграционные тесты движка из `tests/combat_engine/*.rs`
> (подключены через `tests/combat_engine.rs`) проходят, а inline `#[cfg(test)]`
> внутри `crates/combat_engine/src/*.rs` — **нет**.
>
> Канон для полного прогона — `cargo nextest run --workspace --features dev`:
> `--features dev` применяется к `storyforge` (где фича объявлена) и игнорируется
> для членов воркспейса без неё (`combat_engine`, `combat_ai`) — ошибки не будет,
> а inline-тесты движка попадут в прогон. Используй эту команду в CI / перед
> коммитом.
>
> Не клади новые engine-тесты в `crates/combat_engine/tests/` (такой каталог
> намеренно удалён): он собирается только под `-p combat_engine`, поэтому при
> прогоне через `storyforge` молча исключается. Публичные интеграционные тесты —
> в `tests/combat_engine/`; white-box тесты приватных внутренностей — inline в src.

Установка nextest: `cargo install cargo-nextest --locked`.

Удобные алиасы можно положить в **личный** `.cargo/config.toml` (он не
в git'е — там же при желании lld linker config для своей машины):

```toml
[alias]
b  = "build --features dev"
r  = "run --features dev"
c  = "check --features dev"
t  = "test --features dev"
nt  = "nextest run --features dev"
nta = "nextest run --features dev -E 'test(/combat::ai/)'"
```

### Build performance notes

- **Cold build**: ~6 min wall (~3300 s CPU на 8 ядрах). Доминируют
  Bevy/wgpu/naga: bevy_render 169 s, bevy_pbr 153 s, bevy_ui 124 s.
  Свой `storyforge` lib — 39 s (30-я строчка). 
- **Incremental edit** в AI: 5–11 s в зависимости от impact'а
  (leaf factor: ~5 s; heavy type вроде `UnitSnapshot`: ~11 s).
- **Test cycle warm**: 4 s через `cargo nextest run`, 12 s через `cargo test`.
- **Test cycle filter** (AI only, warm): 2.6 s через nextest, 25 s через `cargo test` —
  cargo test пересобирает test binaries даже при substring-фильтре.
- Workspace: `crates/combat_engine` (pure-Rust), `crates/combat_ai` (skeleton,
  заготовка для будущего выноса AI-слоя — пока неиспользуется).
- Quick room для cold build: урезать дефолтные фичи Bevy (`bevy = { default-features = false, ... }`) — экономит 2-3 мин (отключить `bevy_pbr`, `bevy_anti_alias`, `bevy_post_process`, `bevy_audio`, лишние image формат).

### Bevy 0.18

Messaging API: `#[derive(Message)]`, `MessageReader`, `MessageWriter`, `.add_message::<T>()`, `writer.write(msg)` — переименованный Event-механизм из 0.15.

### Overview

Bevy ECS тактическая RPG. Состояния: `AppState` (Boot → Story ↔ Combat → MainMenu), `CombatPhase` (StartRound → AwaitCommand → Victory/Defeat). Сценарии из TOML: чередование сюжетных экранов и боёв. Весь контент data-driven (`assets/data/*.toml`).

### Docs

| Документ | Содержание |
|----------|-----------|
| [Architecture](docs/architecture.md) | Top-level state machines, module map, content resolution, UI dirty flags, persistence |
| [Combat — Engine](docs/combat/engine.md) | Pure engine: `step` API, `ContentView`, `Unit`, determinism contract |
| [Combat — Bridge](docs/combat/bridge.md) | `engine_bridge.rs` — ECS projection, system schedule, event translators |
| [Combat — Pipeline](docs/combat/pipeline.md) | Chain, EndTurn ownership, edge cases, animation blocking |
| [Combat — Lifecycle](docs/combat/lifecycle.md) | Combat start/end, bootstrap, restart, dynamic spawn |
| [Mechanics](docs/mechanics.md) | Характеристики, урон, лечение, мана/ярость, статусы, инициатива |
| [AI](docs/ai/ai.md) | Обзор + ссылки на per-layer доки (decision-cycle, scoring, intent, critics, …) |
| [AI Replay](docs/ai/replay.md) | Оффлайн-реплей `logs/*.jsonl`: `cargo run --bin replay_ai_log -- <file>` — пересчёт скоров/sanity текущим кодом, поиск изменившихся решений |
| [AI Mining](docs/ai/mining.md) | Агрегированная статистика по логам: `cargo run --release --bin mine_ai_logs -- --dir logs/` — band coverage, agenda win-rate, continuation outcomes |
| [Content Guide](docs/content-guide.md) | Как добавлять способности, статусы, оружие, классы, встречи, сценарии |
| [Hex Grid](docs/hex-grid.md) | Координаты even-r, соседи, расстояние, pathfinding, правила движения |
| [World](docs/world.md) | Описание мира, фракции, система магии |

### Tests

Полный гайд — [docs/testing.md](docs/testing.md) (слои, расположение хелперов,
нейминг, мутационное тестирование, decision-tree). Кратко:

- **Полный прогон:** `cargo nextest run --workspace --features dev` (~1332).
  Без `--workspace` inline-тесты крейта `combat_engine` молча не запускаются.
- `tests/combat_engine/*.rs` — engine + bridge (и Bevy-free pure-engine: replay,
  serde, rng_count, purity, …), один бинарь через `tests/combat_engine.rs`.
- `tests/combat/*.rs` — full-app сценарии. `tests/common/` — общие хелперы.
- Inline `#[cfg(test)]` — white-box тесты приватных внутренностей крейта.

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

Rules:
- ALWAYS read graphify-out/GRAPH_REPORT.md before reading any source files, running grep/glob searches, or answering codebase questions. The graph is your primary map of the codebase.
- IF graphify-out/wiki/index.md EXISTS, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
- **When dispatching subagents** (Plan, Explore, general-purpose), include graphify guidance in the spawn prompt — see `.claude/rules/graphify.md` "Rules for Subagents" for ready-to-paste blocks. Subagents do NOT inherit these rules automatically.
