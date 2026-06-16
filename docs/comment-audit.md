# Аудит длинных комментариев

Прогон по `src/`, `crates/`, `tests/`. «Длинный» = ≥ 4 подряд идущих строк комментария.

- Было: **1224** блока ≥ 4 строк (~9896 строк). Разбивка: `///` — 781, `//` — 256, `//!` — 177, mixed — 10.
- Стало: **1014** блоков (~6827 строк). **−31% строк**, 210 блоков ужались ниже порога 4 строк.

Политика (CLAUDE.md §5): комментарий содержит только неочевидное + мотив выбора подхода;
минимум информации, только дополняющая код. Цель — резать пересказ кода и process-историю,
сохранять «почему» и невидимые из кода инварианты.

Свип сделан 12 агентами по непересекающимся батчам файлов; правки comments-only,
`cargo check --workspace --features dev` зелёный, `cargo fmt` прогнан. Intra-doc
links не регрессировали: прогон с `-D rustdoc::broken_intra_doc_links` ловит только
pre-existing шум (нотация индексации `[i]`/`[k]`/`[Hp]` в прозе + давняя в репе
привычка ссылаться `[`private_item`]`) — в обычной сборке проекта этот флаг не включён.

---

## 1. Типовые причины, почему комментарий разрастается

| # | Паттерн | Чем лечится | Примеры |
|---|---------|-------------|---------|
| P1 | **Process / phase narration** — «MVP2 Phase 1 вводит…, Phase 2 добавит…, step 11.4» | Убрать целиком: это история разработки, а не свойство кода. Живёт в git/трекере. | `trade.rs:3-5`, `pick_best.rs` («step 7.4 / 11.4», «step 11.4»), `trade.rs:25` «tracked for Phase 2c» |
| P2 | **Пересказ соседнего кода** — комментарий построчно описывает то, что тут же написано | Удалить или сжать до одной строки-якоря. | `step.rs:1092-1099` (re-narration слайда), `trade.rs:248-251` |
| P3 | **Тройное дублирование одного факта** — module-doc + doc на const + doc на функции повторяют одно | Оставить в одном каноничном месте (обычно у определения), в остальных — ссылка `[[..]]`. | «no floor» в `trade.rs` module-doc inv.3 + `UNIT_VALUE_FLOOR` + `unit_value` doc |
| P4 | **Формула/таблица продублирована прозой** — ASCII-формула, а затем абзац, пересказывающий её словами | Оставить формулу, выкинуть прозу-пересказ. | `pick_best.rs:11-44` (формула 16-27, потом «Asymmetry» 34-44 повторяет) |
| P5 | **Разжёванная многословность** — 1 мысль на 5 предложений | Сжать до 1-2 предложений сути. | `step.rs:1-33`, `trade.rs:210-236` |
| P6 | **Reachability / «когда это вообще срабатывает»** — абзац про то, при каких фазах ветка достижима | Полезно как «почему», но сжимать до 1 строки. | `step.rs:13-18` (Phase 2+ reachability) |

## 2. Найденные неочевидности — и можно ли упростить сам код

Здесь фиксируются места, где длинный комментарий — *симптом* того, что код мог бы быть яснее.

| Файл / место | Что неочевидно (почему нужен комментарий) | Можно ли упростить код |
|---|---|---|
| `trade.rs` `unit_value` / `UNIT_VALUE_FLOOR` | Floor применяется **не** внутри `unit_value`, а в знаменателе на стороне вызова. Разнесённость — источник тройного дублирования комментария. | Да: ввести `unit_value_denom(self) = unit_value(self).max(UNIT_VALUE_FLOOR)` — инвариант «floor только в знаменателе» станет кодом, а не комментарием в 3 местах. |
| `pick_best.rs` per-item composition | Формула «swap intent+tempo колонок» живёт в `finalize_scores` и тут — комментарий явно предупреждает что это «тот же additive space». Риск дрейфа двух копий. | Да: вынести общий `compose_score(factors, stats, weights)` и звать из обоих мест → формула в коде один раз, комментарий-предупреждение не нужен. |
| `step.rs` strict-failure non-actor branch | Достижимость ветки зависит от типа экшена (только AoE-burst, не Move) — из кода ветки не видно. | Нет дешёвого упрощения: это инвариант движка. Оставлена 1 строка «почему». |
| `step.rs` slide-off-occupied | Расхождение контракта: pre-validate разрешает pass-through, а interrupt останавливает мовера досрочно → стек. | Частично: вынести в `slide_to_first_free(path, ...)` — имя несёт намерение, тело-цикл уедет из горячего `step`. |
| `log/mod.rs` schema-version | 194-строчный нарратив миграций v1→v49 жил в прозе, т.к. `SCHEMA_VERSION` — голый `u32` без machine-readable записи изменений; `MIN_SUPPORTED` лежит за 1300 строк. | Перенести версии в CHANGELOG/`#[doc]`-таблицу и сколокировать `SCHEMA_VERSION` рядом с `MIN_SUPPORTED`, чтобы supported-range читался в одном месте. |
| `outcome::PlanAnnotation` (pipeline) `outcomes` | Поле «мёртвое во время pipeline» (источник истины — `plan.annotation.outcomes`), но публично доступно → 2 критика несут одинаковый warning-комментарий. | Сделать поле недоступным из `ScoringCtx`/pipeline-аннотации (отдельный тип или private) — повторяющийся комментарий-предупреждение исчезнет. |
| `world/snapshot.rs` id-maps | 3 метода (`new`/`rebuild_index`/`new_with_id_map`) переизобретают shortcut `UnitId == entity.to_bits()` с почти одинаковым `filter_map` и каждый просит комментарий про summon-кейс. | Извлечь `fn derive_id_maps(state, cache) -> (…, …)` с единственным doc-комментом; три call-site без объяснений. |
| `state.rs` pump-дублирование | `pump_advance_turn` дублирует drain-дисциплину `step_inner`; комментарий просит «держать в синхроне руками» — реальный divergence-hazard без shared helper/теста. | Вынести общий budget-bounded pump в один helper для обоих, либо parity-тест. |
| `state.rs` `Unit.pools` инварианты | «Some для всех живых» (Ap/Mp) vs «Some iff resource-механика» (Mana/Rage) только в прозе; `Unit::new` debug-asserts лишь `pools[Hp]`. | Расширить `debug_assert` на `pools[Ap]`/`pools[Mp]` для живых — инвариант станет machine-checked. |
| `pipeline/order.rs` тройной список стадий | `PRODUCTION_PIPELINE` переписывает все `StageEntry` из `PRE_MASK`+`POST_MASK` — третья ручная копия, тихо дрейфует. | Собирать `PRODUCTION_PIPELINE` конкатенацией двух slice (const concat) или parity-тест `== PRE ++ POST`. |
| `effect.rs` `apply_effect` без своего doc | Rustdoc публичного `apply_effect` случайно прилип к приватному `phase_or_death` выше → entry-point без доки, helper с вводящим в заблуждение lead-in. | Уже починено по ходу: вернул отдельный doc на `apply_effect`, обрезал `phase_or_death`. |
| `intent/bands.rs` нумерация шагов | Маркеры `1./3./4.` в `assign_band` пропускают `2.` (удалённая ветка ForcedTargeting) — читается как «потерян case». | Перенумеровать 1/2/3 (или убрать префиксы). |
| `outcome/mod.rs` legacy-mirror поля | Doc ссылается на удалённые `modifiers/sanity/critics/contract` как на поля структуры; выживают только через serde ignore-unknown. | Явно написать, что это deserialize-only, не члены структуры. |
| `action.rs` `EndTurn` semantics | Старый коммент звал Phase-3 ветку no-op, но `EndTurn` теперь двигает очередь/RoundPhase (см. `step.rs` AdvanceTurn cascade). | Подтвердить актуальную семантику автором (правка comment-only уже выровняла текст). |
| `replay_diff.rs` `_init_a` | Параметр прокинут, подчёркнут как unused, но doc говорит «we only use init_a … for the header». | Убрать `_init_a` или реально печатать header. |
| stale-имена `v28`/`v29` | `read_v29_events`, `golden_from_v28_event` — имена кодируют мёртвые версии схемы; out-of-scope для comments-only. | Переименовать в version-neutral отдельным изменением. |
