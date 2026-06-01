# storyforge

Пошаговая тактическая RPG на Rust + Bevy 0.18 (ECS). Бой на гексагональной
сетке, data-driven контент (весь в `assets/data/*.toml`), сценарии-кампании
с чередованием сюжетных экранов и боёв, и отдельный детерминированный
боевой движок с записываемыми трейсами для AI-реплея.

## Запуск

Дев-цикл идёт через фичу `dev` (включает `bevy/dynamic_linking` — линковка
~3× быстрее). Релиз и бенчмарки — **без** неё (нужен статический линк).

```bash
cargo run   --features dev     # запуск (открывает окно Bevy)
cargo check --features dev     # быстрая проверка без линковки
cargo clippy --features dev    # линтер

cargo build --release          # релизная сборка (без dev)
cargo bench                    # бенчмарки (без dev)
```

## Тесты

```bash
cargo nextest run --workspace --features dev          # вся suite — канон
cargo nextest run --features dev -E 'test(/combat::ai/)'  # только AI
cargo test --features dev --doc                       # doc-тесты
```

> Без `--workspace` inline-тесты крейта `combat_engine` молча не запускаются.
> Установка раннера: `cargo install cargo-nextest --locked`. Полный гайд —
> [docs/testing.md](docs/testing.md).

## Состояния

```
AppState: Boot → MainMenu → Story ↔ Combat → Overworld
                                  └── CombatPhase (SubState):
                                        StartRound → AwaitCommand → Victory / Defeat
```

Сценарии (TOML) чередуют сюжетные экраны и боевые встречи. Управление в бою —
в основном мышью (выбор способности на панели, клик по гексу-цели/клетке,
`Enter` — подтвердить); актуальные подсказки выводятся в HUD. Ходы врагов
ведёт AI-слой автоматически.

## Структура

Cargo-воркспейс: корневой пакет `storyforge` + два крейта.

| Крейт / каталог | Содержимое |
|---|---|
| `crates/combat_engine` | **Чистый** боевой движок (без Bevy): `step` API, `CombatState`, `Unit`, `ContentView`, кости, контракт детерминизма, трейсы |
| `crates/combat_ai` | Заготовка под будущий вынос AI-слоя (пока не используется) |
| `src/combat/` | Боевой слой над движком: `engine_bridge.rs` (ECS-проекция), pipeline, AI (`ai/`), очередь ходов |
| `src/content/` | Загрузка и резолв data-driven контента из TOML |
| `src/scenario/` | Сборка боёв и сюжетных сцен из кампаний |
| `src/game/` | ECS-компоненты, ресурсы, сообщения, гекс-геометрия, pathfinding |
| `src/ui/` | HUD: панель способностей, очередь ходов, лог, гекс-сетка |
| `src/persistence/` | Сохранение/загрузка |
| `src/bin/` | Оффлайн-инструменты: `replay_ai_log`, `replay_engine_trace`, `replay_diff`, `mine_ai_logs` |

## Контент (data-driven)

Весь геймплейный контент — в `assets/data/*.toml`: способности, статусы, оружие,
классы, шаблоны юнитов, встречи и сценарии кампаний. Как добавлять — см.
[docs/content-guide.md](docs/content-guide.md).

Боевые механики включают: характеристики и урон, ману/ярость/энергию, статусы и
ауры, инициативу и призывы, преграды на гексах с проверкой линии видимости (LOS),
ловушки на местности с **по-командной видимостью и владением** (команда-владелец
видит свои ловушки сразу; чужая — только раскрытые ей), фазы юнитов со сменой
цели победы и дедлайнами, поведение бегства у AI.

## Документация

| Документ | О чём |
|---|---|
| [Architecture](docs/architecture.md) | Машины состояний, карта модулей, резолв контента, persistence |
| [Combat — Engine](docs/combat/engine.md) | Чистый движок: `step`, `ContentView`, `Unit`, детерминизм |
| [Combat — Bridge](docs/combat/bridge.md) | `engine_bridge.rs`: ECS-проекция, расписание систем |
| [Combat — Pipeline](docs/combat/pipeline.md) | Цепочка хода, владение EndTurn, краевые случаи |
| [Combat — Lifecycle](docs/combat/lifecycle.md) | Старт/конец боя, bootstrap, рестарт, динамический спавн |
| [Mechanics](docs/mechanics.md) | Характеристики, урон, лечение, ресурсы, статусы, инициатива |
| [AI](docs/ai/ai.md) | Обзор AI + ссылки на per-layer доки |
| [AI Replay](docs/ai/replay.md) | Оффлайн-реплей логов `logs/*.jsonl` |
| [AI Mining](docs/ai/mining.md) | Агрегированная статистика по логам |
| [Content Guide](docs/content-guide.md) | Как добавлять способности, статусы, оружие, классы, встречи, сценарии |
| [Hex Grid](docs/hex-grid.md) | Координаты even-r, соседи, расстояние, pathfinding |
| [World](docs/world.md) | Мир, фракции, система магии |
| [Testing](docs/testing.md) | Слои тестов, расположение хелперов, нейминг |
