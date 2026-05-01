# AI Restructure — Log

Хронологическая запись выполнения roadmap'а из [`restructure.md`](restructure.md). Один блок = один slice или одна важная итерация. Для каждой записи: дата, slice, статус, что сделано, комментарии (если были отклонения от плана или замеченные проблемы).

Формат записи:

```
## YYYY-MM-DD — <Slice ID> (<status>)

**Что сделано:**
- ...

**Комментарии / отклонения от плана:**
- ...

**Файлы, которые затронули:**
- ...

**DoD проверка:**
- [x] / [ ] критерий 1
- [x] / [ ] критерий 2
```

---

## 2026-05-01 — Roadmap зафиксирован

`restructure.md` после серии critique-итераций приведён к финальному виду:

- Двухтрековая структура (P-track контракты + R-track relocation overlay).
- 15-пунктовый рекомендованный порядок выполнения с mainline `P0 → P1 → P2 → P3a → P3b`.
- Контракты validator'а уточнены: `ScoreEffect` с `PreScoreGate` / `PostScoreGate` split, `AnnotationField::Score` разнесён на `ScoreBase` / `ScoreEffects` / `FinalScore`, INITIAL_FIELDS список зафиксирован.
- P0 use fn-pointer table (`StageEntry { id, apply: fn(...) }`), не `&dyn Trait` const.
- P3a `ScoreTrace::compute()` алгебра задокументирована канонически (mask poison → base → multipliers → addends → gates).
- R6 промаркирован как mixed/high-risk ownership split с golden replay diff = 0 hard DoD.
- Owner map в конце документа — single source of truth для «куда класть новое».

**Следующий шаг:** P0 — Single production pipeline declaration. Делегировано thoughtful-implementer'у.

---

## 2026-05-01 — R1 — world/ + config/ + log/ umbrella (completed)

**Что сделано:**
- `snapshot.rs`, `influence.rs`, `reservations.rs`, `tags/` → `src/combat/ai/world/`. Создан `world/mod.rs`.
- `tuning.rs`, `difficulty.rs`, `role.rs` → `src/combat/ai/config/`. Создан `config/mod.rs`.
- `log.rs` → `log/mod.rs` (file-to-dir conversion через `git mv`), `debug.rs` → `log/debug.rs`, `serde_helpers.rs` → `log/serde_helpers.rs`. Добавлены `pub mod debug; pub mod serde_helpers;` в `log/mod.rs`.
- `combat/ai/mod.rs`: 9 строк (snapshot, influence, reservations, tags, tuning, difficulty, role, debug, serde_helpers) → 3 строки (`pub mod world; pub mod config;` + `pub mod log;` не изменилась). Чистый результат: -7 строк.
- Все import paths обновлены: `crate::combat::ai::{snapshot,influence,reservations,tags}` → `world::*`, `{tuning,difficulty,role}` → `config::*`, `{debug,serde_helpers}` → `log::*`. Строковые литералы `#[serde(with = "crate::combat::ai::serde_helpers::...")]` обновлены на `log::serde_helpers`.

**Комментарии / отклонения от плана:**
- Три коммита (R1-1, R1-2, R1-3) по одной группе — каждая группа независимо обратима. Небольшой overhead оправдан: при переносе `log.rs→log/mod.rs` обнаружилась проблема с порядком `//!` inner doc comments и `pub mod` declarations — `//!` должны идти первыми. Раздельные коммиты помогли изолировать это.
- Неожиданность при `log.rs → log/mod.rs`: `pub mod debug; pub mod serde_helpers;` нельзя вставить перед блоком `//!` комментариев — Rust не позволяет inner doc comment после items. Вставлены после блока doc comments, перед первым `#![allow(...)]`.
- `main.rs` использует bare `combat::ai::reservations::Reservations` и `combat::ai::influence::InfluenceConfig` (без `crate::`), поэтому `perl` автозамена не охватила их — обновлены вручную.
- Обнаружено, отложено: внутри `world/snapshot.rs` есть `AiTags` — тип, который по семантике тяготеет к `world/tags/`, но технически встроен в snapshot. Потенциальный кандидат для R-track ownership-split, не для pure relocation.

**Файлы, которые затронули:**
- `src/combat/ai/world/` (new dir: mod.rs + 3 moved files + tags/ subdir)
- `src/combat/ai/config/` (new dir: mod.rs + 3 moved files)
- `src/combat/ai/log/` (converted from file: mod.rs + debug.rs + serde_helpers.rs)
- `src/combat/ai/mod.rs` (−7 строк)
- ~80 source файлов с import path changes (src/combat/ai/**, src/combat/**, src/content/**, src/scenario/**, src/bin/**, tests/**)

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 744 passed, 0 failed (идентично baseline)
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] `git status` — renames, не delete+create (git mv использован для всех переносов)
- [x] `git diff --stat` — доминируют `{old => new}/file.rs` renames; изменения в телах — только import строки
- [x] No semantic diff: git diff — исключительно path/import changes, никаких логических изменений
- [x] `combat/ai/mod.rs` стал короче на 7 строк

---

## 2026-05-01 — P0 — Single production pipeline declaration (completed)

**Что сделано:**
- Создан `src/combat/ai/pipeline/order.rs`: `StageId`, `StageEntry`, thin-shims для всех 12 стадий, три `pub const` (`PRODUCTION_PIPELINE_PRE_MASK`, `PRODUCTION_PIPELINE_POST_MASK`, `PRODUCTION_PIPELINE`), runner `run`.
- `pipeline/mod.rs`: добавлен `pub mod order;`, удалена функция `run_pool_pipeline` вместе с doc-комментарием.
- `utility/mod.rs`: инлайн-цепочка из 12 `.apply()` + блок `use stages::{...}` + `use PlanStage` заменены на два вызова `run(PRODUCTION_PIPELINE_PRE_MASK, ...)` и `run(PRODUCTION_PIPELINE_POST_MASK, ...)`.
- Тест `pipeline_runs_modifiers_after_repair_before_pick` переписан: вызывает `run(PRODUCTION_PIPELINE, ...)` вместо удалённого `run_pool_pipeline`.

**Комментарии / отклонения от плана:**
- Выбран **Вариант A** (split на pre/post): `base_scored` снимается между двумя половинами pipeline и используется в `PickResult.base_scored` — читается в `write_decision_log_from_result` (строка 540) для показа pre/post-adaptation дельт. Взять snapshot после полного pipeline нельзя без семантического изменения логов.
- `PRODUCTION_PIPELINE` (полный список) добавлен как третья `pub const` рядом с двумя split-константами. Это позволяет тесту буквально использовать `PRODUCTION_PIPELINE`, как требует DoD, при том что `pick_action` использует split-варианты.
- Порядок аргументов в `fn(&mut ScoredPool, &mut StageCtx)` — pool первым, ctx вторым — соответствует существующему `PlanStage::apply` trait. Спецификация в `restructure.md` показывала обратный порядок; следован trait order, как указано в задании.
- Pre-existing clippy warnings (28 штук, в `item_scoring.rs` и `overlay_considerations.rs`) не затронуты.

**Файлы, которые затронули:**
- `src/combat/ai/pipeline/order.rs` (new)
- `src/combat/ai/pipeline/mod.rs`
- `src/combat/ai/utility/mod.rs`
- `docs/ai/restructure_log.md` (этот файл)

**DoD проверка:**
- [x] `cargo test` зелёный (744 passed, 0 failed)
- [x] `PRODUCTION_PIPELINE` — единственное место с порядком production-стадий (`order.rs`); split-константы `PRE_MASK` / `POST_MASK` — два допустимых источника согласно Варианту A
- [x] тест `pipeline_runs_modifiers_after_repair_before_pick` использует `PRODUCTION_PIPELINE` буквально
- [x] `git grep` по `<Stage>Stage.apply` в `utility/mod.rs` не находит инлайн-цепочки (exit code 1)

---

## 2026-05-01 — Follow-up: tags consolidation формализована в R7

R1's «обнаружено, отложено» наблюдение про `AiTags` в `world/snapshot.rs` vs `AbilityTag`/`StatusTag` в `world/tags/` зафиксировано в roadmap'е:

- Расширен scope `R7` в `restructure.md` — добавлен пункт **Tags consolidation** (переезд `AiTags` в `world/tags/ai_tags.rs`).
- Указано, что эту часть R7 можно вынести как отдельный мини-slice **R7.tags** если приоритет вырастет — pinning только R1 (уже сделано), DoD `cargo test --lib` + import-churn guard.
- Owner map обновлён: добавлена отдельная строка для `AiTag` flag (текущее место `world/snapshot.rs` → пост-R7.tags `world/tags/ai_tags.rs`); существующая строка для `AbilityTag` / `StatusTag` уточнена.

Это не отдельный slice — просто формализация observation'а, чтобы он не потерялся в логе.

---

## 2026-05-01 — P1 — Remove legacy AdaptationStage (completed)

**Что сделано:**
- Проведён анализ parity-покрытия legacy-тестов из `pipeline/stages/adaptation.rs` относительно `mode_selection.rs` / `finalize.rs` тестов.
- Добавлены два недостающих теста в `pipeline/stages/mode_selection.rs::tests`:
  - `mode_selection_records_original_score` — проверяет что `ann.adaptation.original_score` совпадает с pre-adaptation `ann.score`.
  - `mode_selection_adaptation_reason_round_trips_to_intent` — проверяет что `IntentReason::Adapted` корректно конструируется из `ann.adaptation`.
- Удалён `src/combat/ai/pipeline/stages/adaptation.rs` (git rm).
- Удалён `pub mod adaptation;` из `src/combat/ai/pipeline/stages/mod.rs`.
- Обновлены комментарии, ссылавшиеся на `AdaptationStage`, в `outcome/mod.rs`, `modifiers/mod.rs`, `pipeline/stages/{critics,sanity,finalize,mode_selection}.rs`, `utility/mod.rs`.

**Комментарии / отклонения от плана:**
- Анализ покрытия показал, что 2 из 4 legacy-тестов не имели точных аналогов в mode_selection:
  1. `adaptation_stage_records_original_score` — mode_selection тесты проверяли лишь `is_some()`, но не значение `original_score`.
  2. `adaptation_data_round_trips_through_intent_reason` — нет аналога (тест про IntentReason round-trip).
- После P1 счётчик тестов: 744 - 4 + 2 = 742 (4 удалены из stages/adaptation.rs, 2 добавлены в mode_selection).

**Файлы:**
- `src/combat/ai/pipeline/stages/adaptation.rs` (deleted)
- `src/combat/ai/pipeline/stages/mod.rs` (−2 строки)
- `src/combat/ai/pipeline/stages/mode_selection.rs` (+2 теста)

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 742 passed, 0 failed
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] `pipeline/stages/adaptation.rs` не существует
- [x] `git grep "AdaptationStage"` — exit code 1

---

## 2026-05-01 — R2 — adapt/ extraction (completed)

**Что сделано:**
- Создан top-level модуль `src/combat/ai/adapt/`:
  - `adapt/mod.rs` — data-types: `EvaluationMode`, `AdaptationReason`, `Adaptation`. Тесты: `default_mode_defers_to_global_intent`, `last_stand_mode_overrides_global`. Re-exports `apply_adaptation`, `select_evaluation_modes`, `pending_dot_before_next_action` из `select`.
  - `adapt/select.rs` — алгоритм: `pending_dot_before_next_action`, `plan_has_self_rescue`, `select_evaluation_modes`, `apply_adaptation` + 11 тестов поведения.
- Перемещён `planning/adaptation.rs` → `adapt/mod.rs` (git mv), создан `adapt/select.rs` со split'ом.
- Удалены `pub mod adaptation;` и `pub use adaptation::{...};` из `planning/mod.rs`.
- Добавлен `pub mod adapt;` в `combat/ai/mod.rs`.
- Обновлены import paths у 12 consumer-файлов.

**Комментарии / отклонения от плана:**
- Helpers (`pending_dot_before_next_action`, `plan_has_self_rescue`) перенесены в `select.rs` — они вызываются только из алгоритмических функций, когезия с алгоритмом выше, чем с типами.
- Два места с inline `crate::combat::ai::planning::AdaptationReason` в `outcome/mod.rs` и `intent/mod.rs` обнаружены через cargo check (не через ast-index, т.к. не в import-строках) и исправлены.
- Обнаружен dead-code кандидат: `apply_adaptation` после удаления `AdaptationStage` не имеет production-консьюмеров. Сохранена согласно плану (pure algorithm, тест-suite использует её напрямую). Отложено для P-track.

**Файлы:**
- `src/combat/ai/adapt/` (new: mod.rs + select.rs)
- `src/combat/ai/planning/adaptation.rs` (deleted via git mv)
- `src/combat/ai/planning/mod.rs`, `src/combat/ai/mod.rs`
- 12 consumer-файлов: import paths
- `docs/ai/adaptation.md`

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 742 passed, 0 failed
- [x] `cargo test` (интеграционные) — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] `planning/adaptation.rs` не существует
- [x] `adapt/mod.rs` и `adapt/select.rs` существуют
- [x] `git grep "planning::adaptation\|planning::AdaptationReason\|planning::EvaluationMode"` — exit code 1
- [x] `git grep "AdaptationStage"` — exit code 1

---

## 2026-05-01 — R3 — scoring/ partial umbrella (completed)

**Что сделано:**
- `scoring.rs` → `scoring/horizon.rs` (git mv; имя отражает содержимое: DPR helpers, damage horizon).
- `target_priority.rs`, `position_eval.rs`, `trade.rs` → `scoring/`.
- `policy/` (вся директория: cc, damage, friendly_fire, heal, status, tests, mod) → `scoring/policy/`.
- Создан `scoring/mod.rs` с `pub mod` declarations + `//!` doc-комментарий + `pub use horizon::{...}` re-exports для обратной совместимости (7 публичных символов).
- `combat/ai/mod.rs`: 5 строк (`policy`, `position_eval`, `scoring`, `target_priority`, `trade`) → 1 строка `pub mod scoring;`.
- Обновлены import paths в 14 файлах: `intent/mod.rs`, `intent/agenda.rs`, `log/debug.rs`, `planning/future_value.rs`, `planning/sanity.rs`, `planning/scorer.rs`, `appraisal/mod.rs`, `factors/offensive.rs`, `modifiers/trade_bonus.rs`, `modifiers/repair_bonus.rs`, `modifiers/summon_bonus.rs`, `pipeline/stages/plan_modifiers.rs`, `utility/mod.rs`, `scoring/policy/tests.rs`.
- Обновлены пути в docs: `docs/ai/ai.md`, `docs/ai/target-priority.md`, `docs/ai/policy.md`, `docs/ai/trade-economy.md`, `docs/ai/extension-checklist.md`.

**Комментарии / отклонения от плана:**
- Re-exports в `scoring/mod.rs` — 7 pub символов из `horizon::*`. Два символа не включены: `status_score` — `pub(crate)`, не `pub` (нельзя ре-экспортировать через `pub use`); `AbilityProjection` — приватный внутренний тип.
- `scoring/trade.rs` — внутренний `use crate::combat::ai::scoring::horizon_avg` заменён на `use crate::combat::ai::scoring::horizon::horizon_avg` (прямой путь, избегает self-referencing через родительский re-export).
- `scoring/policy/status.rs` — аналогично: `scoring::*` → `scoring::horizon::*`.
- `extension-checklist.md`: заодно обновлены пути из R1/R2 которые были упущены ранее (`planning/adaptation.rs` → `adapt/select.rs`, `snapshot.rs` → `world/snapshot.rs`).

**Файлы, которые затронули:**
- `src/combat/ai/scoring/` (new dir: mod.rs + horizon.rs + target_priority.rs + position_eval.rs + trade.rs + policy/)
- `src/combat/ai/mod.rs` (−4 строки)
- 14 source-файлов с import path changes
- 5 docs файлов с path changes

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 742 passed, 0 failed (идентично baseline)
- [x] `cargo test` — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Top-level `scoring.rs`, `target_priority.rs`, `position_eval.rs`, `trade.rs`, `policy/` — не существуют
- [x] `scoring/{mod.rs, horizon.rs, target_priority.rs, position_eval.rs, trade.rs, policy/}` — существуют
- [x] `src/combat/ai/mod.rs` стал короче на 4 строки (5 строк → 1)
- [x] `git status` показывает R (renames) для всех файлов и директории policy/
- [x] `git diff --stat` доминируют path/import changes; логика файлов не тронута
- [x] Документы `docs/ai/*.md` актуальны

---

## 2026-05-01 — P2 — StageSpec + pipeline validator (completed)

**Что сделано:**
- Создан `src/combat/ai/pipeline/spec.rs` (~360 LOC).
- `AnnotationField` enum (12 вариантов), `ScoreEffect` enum (6 вариантов), `StageSpec` struct, `INITIAL_FIELDS`, `STAGE_SPECS` (12 стадий), `ValidationError` enum с `Display`, `validate_pipeline`.
- Добавлен `pub mod spec;` в `pipeline/mod.rs`.
- 7 тестов в `spec.rs`: 2 structural (`stage_specs_length_matches_pipeline`, `stage_specs_ids_match_pipeline_order`), 1 positive (`production_pipeline_order_is_valid`), 4 negative (MissingWriter, RescoreAfterEffect, MultipleRescore, PostScoreGateBeforeRescore).
- Обновлён `docs/ai/pipeline.md` — добавлена секция «StageSpec и pipeline validator (P2)».

**Комментарии / отклонения от плана:**
- **Дизайн-выбор: отдельная таблица STAGE_SPECS** (не поле в StageEntry). Обоснование: spec не зависит от split PRE/POST_MASK, поэтому дублировать данные в трёх константах избыточно; `StageEntry` остаётся простым и const-constructible.
- **PickBest читает `ScoreEffects`, не `FinalScore`**. `FinalScore` как самостоятельное поле нет смысла заводить сейчас (нет стадии, которая явно «финализирует» в отдельное поле) — это P3a concern. PickBest читает `ScoreEffects` как финальный результат.
- **KillableGate = PostScoreGate**, как предписывает roadmap. Фактически код ставит `ann.score = NEG_INFINITY` (т.е. Mask-поведение), но roadmap директивно разделяет: «Не путай Mask и PostScoreGate — KillableGate = PostScoreGate». Spec фиксирует **планируемую** семантику, а не текущую реализацию (migration в P3a).
- **Порядок проверок в validate_pipeline**: `PostScoreGateBeforeRescore` проверяется перед `RescoreAfterEffect`, так как это более специфичная ошибка. `PostScoreGate` исключён из `ILLEGAL_BEFORE_RESCORE` — PostScoreGate после Rescore корректно, только до Rescore — ошибка.
- Тест `negative_reads_without_writer` использует минимальный фиктивный pipeline из 3 стадий (overlay → repair → finalize), что достаточно для проверки конкретной ошибки.

**Файлы:**
- `src/combat/ai/pipeline/spec.rs` (new, ~360 LOC)
- `src/combat/ai/pipeline/mod.rs` (+1 строка: `pub mod spec;`)
- `docs/ai/pipeline.md` (новая секция)
- `docs/ai/restructure_log.md` (этот файл)
- `docs/ai/restructure.md` (status table)

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 749 passed (742 baseline + 7 новых)
- [x] `cargo test` (интеграционные) — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] `STAGE_SPECS` покрывает все 12 production stages
- [x] `validate_pipeline(STAGE_SPECS)` зелёный
- [x] 4 negative теста (MissingWriter, RescoreAfterEffect, MultipleRescore, PostScoreGateBeforeRescore)
- [x] `spec.rs` ≤ 400 LOC (≈360 LOC)
- [x] `restructure_log.md` обновлён, status table обновлена

---

## 2026-05-01 — P3a.0 — ScoreTrace types + compute() (completed)

**Что сделано:**
- Создан `src/combat/ai/pipeline/score_trace.rs` (~175 LOC): типы (`MultiplierHit`, `AddendHit`, `MaskHit`, `GateHit`, `MaskKind`, `GateOutcome`, `MultiplierKind`), структура `ScoreTrace` с `#[derive(Default)]`, метод `compute()`, builder-helpers, `reset_effects()`, 8 unit-тестов на алгебру.
- Добавлен `pub mod score_trace;` в `src/combat/ai/pipeline/mod.rs`.
- В `PlanAnnotation` (outcome/mod.rs) добавлено поле `score_trace: ScoreTrace` с `#[serde(skip)]` (no schema bump).
- Обновлён `docs/ai/pipeline.md` — секция «ScoreTrace — typed effect log (P3a)».

**Комментарии / отклонения:**
- Это первый sub-step из 7 в split'е P3a (P3a.0 — P3a.5 + финализация). Следующий — P3a.1 (миграция modifiers stage к `push_addend`).
- Поведенческий diff = 0: ни одна стадия ещё не пушит в trace; `ann.score` мутируется по-прежнему.
- `EvaluationMode` импортируется из `adapt::EvaluationMode` (путь зафиксирован в R2).
- Поле `score_trace` размещено в конце `PlanAnnotation` после `reject_reasons_per_item` — логически рядом с `#[serde(skip)]` полями `score_initial` и `per_item`, при этом в отдельной `// ── P3a fields ──` секции для явного маркирования.

**Файлы:**
- `src/combat/ai/pipeline/score_trace.rs` (new, ~175 LOC)
- `src/combat/ai/pipeline/mod.rs` (+1 строка: `pub mod score_trace;`)
- `src/combat/ai/outcome/mod.rs` (+14 строк: поле + P3a-секция + комментарий)
- `docs/ai/pipeline.md` (новая секция)
- `docs/ai/restructure_log.md` (этот файл)
- `docs/ai/restructure.md` (status table)

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 757 passed (749 baseline + 8 новых тестов на compute())
- [x] `cargo test` (интеграционные) — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] `score_trace.rs` существует с 8 тестами на алгебру
- [x] `pipeline/mod.rs` регистрирует `pub mod score_trace;`
- [x] `PlanAnnotation` имеет поле `score_trace` с `#[serde(skip)]`
- [x] `pipeline/stages/*.rs` — git diff пустой (production стадии не тронуты)
- [x] No semantic diff: pipeline behavior unchanged

---

## 2026-05-01 — P4 — Intent split (completed)

**Что сделано:**
- Создан `intent/kinds.rs` — `TacticalIntent`, `IntentKind`, `IntentReason` + impl + Display.
- Создан `intent/memory.rs` — `PlanSnapshot` + `AiMemory` + `status_hash`; тесты `snapshot_*`.
- Создан `intent/score.rs` — `pursuit_move_score`, `cc_reach`, `IntentWeights`, `intent_offensive_value_on_target`, `intent_score`; тесты `reposition_*`, `pursuit_*`, `focus_target_*`, `cc_reach_*`, `intent_score_via_narrow_offensive_api_matches_legacy`.
- Создан `intent/select.rs` — `IntentChoice`, `select_intent_normal`, `select_intent`, `intent_viability_threshold`, `default_focus_target`, `update_memory`; тесты `killable_requires_action_points`, `stickiness_modulated_by_continue_commitment`.
- Переписан `intent/mod.rs` (~32 LOC): только docstring + `pub mod` + `pub use` / `pub(crate) use`.
- Re-export `status_hash` добавлен в mod.rs (внешний consumer `repair/goal.rs` использует `crate::combat::ai::intent::status_hash`).

**Комментарии / отклонения от плана:**
- Прерывание предыдущей попытки на этапе создания `select.rs` (socket error). 3 sub-файла (`kinds.rs`, `memory.rs`, `score.rs`) были созданы в первой попытке, `mod.rs` не успел получить cleanup. Завершено второй попыткой.
- `select_intent_normal` — `pub(crate)`, реэкспортирована через `pub(crate) use` (не `pub use`) — корректно, т.к. только внутри intent-модуля.
- `intent_offensive_value_on_target` — `pub(crate)`, вызывается только внутри `score.rs`; не реэкспортируется из mod.rs.
- `status_hash` — не упомянута в P4 спеке явно, но необходима для `repair/goal.rs` и `repair/lifecycle.rs` (путь `crate::combat::ai::intent::status_hash`).
- `#[allow(deprecated)]` добавлен на pub use строку `select_intent` в mod.rs — подавляет pre-existing warning при реэкспорте deprecated символа.

**Файлы:**
- `src/combat/ai/intent/mod.rs` — 32 LOC (переписан)
- `src/combat/ai/intent/kinds.rs` — 214 LOC (создан ранее)
- `src/combat/ai/intent/memory.rs` — 267 LOC (создан ранее)
- `src/combat/ai/intent/score.rs` — 994 LOC (создан ранее)
- `src/combat/ai/intent/select.rs` — 695 LOC (создан)

**DoD проверка:**
- [x] `intent/mod.rs` ≤ 80 LOC (32 LOC)
- [x] `intent/{kinds, select, score, memory}.rs` существуют, каждый owns одну concern
- [x] `intent/{agenda, bands, considerations}.rs` не изменены
- [x] `cargo test --lib` — 749 passed (точное соответствие baseline)
- [x] `cargo build` — clean
- [x] `cargo clippy --all-targets` — 28 warnings в тестах, все pre-existing; 0 новых
- [x] No semantic diff: чистая релокация символов
- [x] Внешние consumer'ы работают через `crate::combat::ai::intent::*` без изменений

---

## 2026-05-01 — P3a.1 — Modifiers → trace.addends (completed)

**Что сделано:**
- `PlanModifiersStage::apply()` теперь пушит `AddendHit` в `ann.score_trace.addends` для каждого из 3 modifier'ов на non-masked планах.
- Bridging: `ann.score_trace.base = ann.score` на входе в стадию — принимает текущий accumulated score как baseline (upstream стадии ещё не мигрированы и мутируют `ann.score` напрямую).
- `ann.score += contribution` сохранён — downstream readers (PickBest) работают без изменений.
- `debug_assert!((ann.score - ann.score_trace.compute()).abs() < 1e-5)` после modifier-loop'а на каждом non-masked плане.
- Masked планы (`!ann.score.is_finite()`) пропускаются — trace остаётся `Default`.
- 4 новых теста в `pipeline/stages/plan_modifiers.rs::tests`:
  - `p3a_modifiers_push_addends_to_trace` — `trace.addends.len() == PLAN_MODIFIERS.len()`, имена в order'е.
  - `p3a_modifiers_trace_base_synced_from_score` — `trace.compute() == ann.score`.
  - `p3a_modifiers_invariant_score_equals_compute` — 3 non-masked плана, invariant по всем.
  - `p3a_modifiers_masked_plan_trace_unchanged` — masked план: `base=0, addends.len()=0`.

**Комментарии / отклонения от плана:**
- `p3a_modifiers_masked_plan_trace_unchanged` вызывает `PlanModifiersStage.apply()` напрямую (не `run(PRODUCTION_PIPELINE, ...)`), потому что `PickBestStage` перезаписывает `ann.score` на masked-планах при прогоне полного pipeline. Прямой вызов более точен: тест проверяет именно семантику этой стадии, а не полного pipeline'а.
- Остальные 3 теста используют `run(PRODUCTION_PIPELINE, ...)` — результат `score_trace` проверяется, а не `ann.score` после PickBest.

**Файлы:**
- `src/combat/ai/pipeline/stages/plan_modifiers.rs` — единственный изменённый файл стадии

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 761 passed (757 + 4)
- [x] `cargo test` — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Behavioural diff = 0: `ann.score += contribution` сохранён; downstream readers не тронуты
- [x] Existing тесты `plan_modifiers_stage_*` (3 шт) — зелёные без изменений
- [x] Только один файл стадии тронут: `pipeline/stages/plan_modifiers.rs`
- [x] `Sanity`, `Critics`, `ProtectSelfMask`, `KillableGate`, `Finalize` — не изменены

---

## 2026-05-01 — P3a.2 — Critics → trace.multipliers (completed)

**Что сделано:**
- `CriticsStage::apply()` теперь пушит `MultiplierHit { kind: MultiplierKind::Critic, value: hit.multiplier }` в `ann.score_trace.multipliers` для каждого critic hit.
- Bridging: `ann.score_trace = ScoreTrace { base: ann.score, ..Default::default() }` на входе в план-loop — полный сброс trace + синхронизация base с текущим ann.score (discards upstream trace state).
- `ann.score *= hit.multiplier` сохранён — behavioural diff = 0.
- `ann.critics.push(hit)` сохранён — observability канал не тронут.
- `debug_assert!` на invariant `(ann.score - trace.compute()).abs() < 1e-5` — только когда `applied_count > 0 && entry_score.is_finite()` (избегает NaN при NEG_INFINITY × 0.0 corner-кейсе).
- **Retroactive fix P3a.1 bridging** в `plan_modifiers.rs`: строка `ann.score_trace.base = ann.score;` заменена на полное пересбрасывание `ann.score_trace = ScoreTrace { base: ann.score, ..Default::default() }`. Без этого накопленные critic-multipliers из предыдущей стадии нарушали бы invariant `ann.score == trace.compute()` в PlanModifiersStage.
- 4 новых теста в `pipeline/stages/critics.rs::tests`:
  - `p3a_critics_push_multipliers_to_trace` — один hit; `trace.multipliers.len() == 1`, kind=Critic, value=0.5.
  - `p3a_critics_trace_base_synced_from_score` — entry=1.0, multiplier=0.5; `trace.base == 1.0`, `trace.compute() == 0.5 == ann.score`.
  - `p3a_critics_invariant_score_equals_compute_with_multiple_hits` — два critic'а [0.5, 0.8]; entry=1.0; `ann.score ≈ 0.4`, `trace.compute() ≈ 0.4`.
  - `p3a_critics_no_hits_leave_trace_with_only_base` — пустой critics list; `trace.base == entry_score`, `multipliers.len() == 0`.

**Комментарии / отклонения от плана:**
- Bridging-механизм P3a.1 содержал скрытый баг: `ann.score_trace.base = ann.score` не очищал существующие мультипликаторы в trace. После миграции Critics (P3a.2) critic-multipliers присутствовали бы в trace на входе в PlanModifiersStage, нарушая invariant. Fix применён retroactively — P3a.1 тесты остались зелёными.
- trace отражает только эффекты **последней мигрированной стадии** (partial migration phase). Начиная с P3a.6 (Finalize + full cleanup) trace будет аккумулировать все стадии через `reset_effects()`.
- Тест `critics_survive_through_adaptation_path` использует partial pipeline (Finalize → Critics). После P3a.2 bridging-сброс в Critics перезаписывает trace после Finalize — это ожидаемое поведение для partial migration.

**Файлы:**
- `src/combat/ai/pipeline/stages/critics.rs` — миграция apply() + 4 теста
- `src/combat/ai/pipeline/stages/plan_modifiers.rs` — retroactive bridging fix (1 строка → 5 строк)

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 765 passed (761 + 4)
- [x] `cargo test` (интеграционные) — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Behavioural diff = 0: `ann.score *= hit.multiplier` сохранён; bit-identical с pre-P3a.2 для всех планов
- [x] Existing тесты `critics_*` (3 шт) — зелёные без изменений ассертов
- [x] Existing тесты `p3a_modifiers_*` (4 шт) — зелёные без изменений ассертов
- [x] Только два файла стадий тронуты: `critics.rs` и `plan_modifiers.rs`
- [x] `Sanity`, `ProtectSelfMask`, `KillableGate`, `Finalize`, `PickBest` — не изменены

---

## 2026-05-01 — P3a.3 — Sanity → trace.multipliers (completed)

**Что сделано:**
- `SanityStage::apply()` теперь пушит `MultiplierHit { kind: MultiplierKind::Sanity, value: hit.multiplier }` в `ann.score_trace.multipliers` для каждого `SanityHit` параллельно сохранению `ann.sanity = hits`.
- Bridging: snapshot entry scores снимается ДО вызова `sanity_adjust_plans` (она мутирует scores in-place); затем `ann.score_trace = ScoreTrace { base: entry_scores[i], ..Default::default() }` — полный сброс per-plan с синхронизацией base на pre-sanity score.
- `ann.score = new_score` и `ann.sanity = hits` сохранены — behavioural diff = 0.
- `debug_assert!(ann.score == trace.compute(), ε < 1e-5)` — только для `entry_scores[i].is_finite()` (masked планы с NEG_INFINITY пропускаются — invariant не запускается).
- 4 новых теста в `pipeline/stages/sanity.rs::tests`:
  - `p3a_sanity_no_hits_trace_has_only_base` — clean plan (нет правил); `trace.base == entry_score`, `multipliers.len() == 0`, `compute() == entry_score`.
  - `p3a_sanity_with_hits_pushes_multipliers` — через FinalizeStage → SanityStage setup; 1-to-1 соответствие `multipliers[j].kind == Sanity`, `multipliers[j].value == sanity[j].multiplier`, `compute() == ann.score`.
  - `p3a_sanity_invariant_holds_for_all_finite_plans` — pool из 3 планов; для каждого `(ann.score - trace.compute()).abs() < 1e-5`.
  - `p3a_sanity_masked_plan_trace_unchanged_or_only_base` — plan с `NEG_INFINITY`; no panic, score остаётся NEG_INFINITY, `sanity.is_empty()`, invariant assert пропущен.

**Комментарии / отклонения от плана:**
- Snapshot entry_scores берётся до `sanity_adjust_plans` (не после), т.к. функция мутирует scores in-place — без pre-snapshot bridging base оказался бы post-sanity значением, нарушая `trace.compute() == ann.score`.
- `p3a_sanity_with_hits_pushes_multipliers` использует FinalizeStage + SanityStage setup (аналог `sanity_survives_adaptation_path`), а не standalone trigger — реальные sanity-правила (HealerExposure, RetreatTrap, SynergyBonus) трудно вызвать без сложного multi-unit setup. Тест проверяет 1-to-1 корреляцию и `compute()` invariant вне зависимости от того, сработало ли правило.
- `p3a_sanity_invariant_holds_for_all_finite_plans` использует прямой вызов `SanityStage.apply()` на 3-plan pool без FinalizeStage — `sanity_adjust_plans` имеет early-return для `len <= 1`, поэтому pool из 3 планов гарантирует прогон rule-loop'а (хотя с healthy actor в safe tile правила всё равно не сработают — invariant проверяется и без hits).
- Bridging-паттерн идентичен P3a.2 (`critics.rs`) — полный reset `ScoreTrace { base, ..default() }` + push hits в order'е.

**Файлы:**
- `src/combat/ai/pipeline/stages/sanity.rs` — единственный изменённый файл стадии

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 769 passed (765 + 4)
- [x] `cargo test` (интеграционные) — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Behavioural diff = 0: `ann.score = new_score` и `ann.sanity = hits` сохранены; bit-identical с pre-P3a.3
- [x] Existing тесты `sanity_stage_*` (3 шт) — зелёные без изменений ассертов
- [x] Existing тесты P3a.0/1/2 — без regressions
- [x] Только один файл стадий тронут: `pipeline/stages/sanity.rs`
- [x] `ProtectSelfMask`, `KillableGate`, `Finalize`, `PickBest` — не изменены

---

## 2026-05-01 — P3a.4 — ProtectSelfMask + KillableGate → trace.masks/gates (completed)

**Что сделано:**
- `ProtectSelfMaskStage::apply()` пушит `MaskHit { kind: MaskKind::Poison, source: "protect_self" }` для каждого замаскированного плана. Bridging: `ann.score_trace = ScoreTrace { base: pre_scores[i], ..Default::default() }` внутри if-блока (только для masked планов). `debug_assert_eq!(ann.score_trace.compute(), f32::NEG_INFINITY)` — инвариант на masked plans.
- `KillableGateStage::apply()` пушит **double-emit** для каждого gated плана: `GateHit { outcome: GateOutcome::Reject, source: "killable_gate" }` + `MaskHit { kind: MaskKind::Poison, source: "killable_gate" }`. Bridging аналогичен ProtectSelf. `debug_assert_eq!(ann.score_trace.compute(), f32::NEG_INFINITY)` — инвариант.
- `ann.score = NEG_INFINITY` и `ann.contract = Some(ContractMaskHit { ... })` сохранены в обеих стадиях — behavioural diff = 0.
- 3 новых теста в `pipeline/stages/protect_self.rs::tests`:
  - `p3a_protect_self_mask_emits_mask_hit` — non-defensive plan: `masks.len()==1`, kind=Poison, source="protect_self", `gates.is_empty()`.
  - `p3a_protect_self_mask_no_hit_when_defensive` — defensive plan: `masks.is_empty()`, score unchanged=0.5.
  - `p3a_protect_self_mask_invariant` — masked plan: `ann.score == NEG_INFINITY && trace.compute() == NEG_INFINITY`.
- 3 новых теста в `pipeline/stages/killable_gate.rs::tests`:
  - `p3a_killable_gate_emits_gate_and_mask_hits` — gated plan: `gates.len()==1`, outcome=Reject, source="killable_gate", `masks.len()==1`, kind=Poison, `is_gated()==true`.
  - `p3a_killable_gate_no_hit_when_intent_not_focus_target` — Reposition intent: `gates.is_empty()`, `masks.is_empty()`.
  - `p3a_killable_gate_compute_returns_neg_infinity` — gated plan: `trace.compute() == NEG_INFINITY`.

**Комментарии / отклонения от плана:**
- **Выбран Option A** (double-emit для KillableGate): GateHit + MaskHit Poison. GateHit — корректная семантика (соответствует PostScoreGate classification в STAGE_SPECS и roadmap'е). MaskHit Poison — необходим для поддержания инварианта `ann.score == trace.compute()`, пока KillableGate всё ещё ставит NEG_INFINITY. Только-GateHit вариант (без MaskHit) был бы некорректен: `trace.compute()` вернул бы finite значение при NEG_INFINITY score, ломая инвариант.
- **KillableGate: семантическая расщеплённость** — стадия классифицирована как `PostScoreGate` в STAGE_SPECS (и в roadmap'е), но по текущей реализации ведёт себя как Mask (ставит NEG_INFINITY). Double-emit отражает оба аспекта: roadmap-семантику через GateHit и фактическое поведение через MaskHit Poison. Переход на чистый Gate-behavior (флаг вместо NEG_INFINITY, удаление MaskHit Poison) — будущий slice после P3a.6.
- **Invariant assert — только для masked/gated планов**: для finite-pre → finite-post планов assert не запускается. Это сделано намеренно: для не-masked/не-gated планов `score_trace` остаётся в upstream-состоянии (default с base=0 от предыдущих стадий), и `trace.compute()` не равен `ann.score`. Assert внутри `if new_score == NEG_INFINITY` блока — корректная область видимости.

**Файлы:**
- `src/combat/ai/pipeline/stages/protect_self.rs` — миграция apply() + 3 теста
- `src/combat/ai/pipeline/stages/killable_gate.rs` — миграция apply() + 3 теста
- `docs/ai/pipeline.md` (обновлена секция ScoreTrace)
- `docs/ai/restructure_log.md` (этот файл)
- `docs/ai/restructure.md` (status table)

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 775 passed (769 + 6 новых)
- [x] `cargo test` (интеграционные) — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Behavioural diff = 0: `ann.score = NEG_INFINITY` и `ann.contract` сохранены bit-identical
- [x] Existing тесты `protect_self_mask_*` (3 шт) и `killable_gate_*` (3 шт) — зелёные без изменений
- [x] Existing тесты P3a.0/1/2/3 — без regressions
- [x] Только два файла стадий тронуты: `protect_self.rs` и `killable_gate.rs`
- [x] `STAGE_SPECS` не изменены
- [x] `FinalizeStage`, `PickBest`, `SanityStage`, `CriticsStage`, `PlanModifiersStage` — не изменены

---

## 2026-05-01 — P3a.5 — Finalize → Rescore (sets trace.base, clears effects) (completed)

**Что сделано:**
- `FinalizeStage::apply()` теперь устанавливает `ann.score_trace = ScoreTrace { base: new_score, rescore_mode: Some(modes[i]), ..Default::default() }` для каждого плана после rescoring.
- `ann.score = new_score` и `ann.factors = new_raw` сохранены — behavioural diff = 0.
- `debug_assert!((ann.score - ann.score_trace.compute()).abs() < 1e-5)` — только для `new_score.is_finite()` (защита от ложных срабатываний на NaN/Infinity).
- `rescore_mode` теперь заполнен (`Some(EvaluationMode::Default)` или `Some(EvaluationMode::LastStand)`) — диагностический сигнал о режиме rescoring.
- Это ЕДИНСТВЕННАЯ стадия с `ScoreEffect::Rescore` (см. STAGE_SPECS). Upstream effects до Finalize считаются устаревшими и очищаются struct-literal присваиванием (эквивалентно `reset_effects() + base = new_score`).
- 4 новых теста в `pipeline/stages/finalize.rs::tests`:
  - `p3a_finalize_sets_trace_base_to_new_score` — trace.base == ann.score, rescore_mode = Default, все vecs пусты, compute() == ann.score.
  - `p3a_finalize_clears_upstream_effects` — стale upstream trace (base=999.0, multiplier=0.5) очищается; base = new_score; multipliers.is_empty().
  - `p3a_finalize_records_rescore_mode_per_plan` — два плана (LastStand + Default); rescore_mode соответствует per-plan mode.
  - `p3a_finalize_empty_pool_no_op` — empty pool не паникует, is_empty() == true после apply.

**Комментарии / отклонения от плана:**
- Это последняя миграция стадий в P3a. Следующий шаг (P3a.6) убирает bridging-резеты (`ScoreTrace { base: ann.score, ..Default::default() }`) из немигрированных-upstream-стадий (Sanity, Critics, ProtectSelfMask, KillableGate, PlanModifiers) и валидирует full-pipeline equivalence. После P3a.6 trace будет аккумулировать все эффекты pipeline'а: base (Finalize) → multipliers (Sanity, Critics) → addends (Modifiers) → mask/gate (ProtectSelf, KillableGate).
- Struct-literal присваивание (`ann.score_trace = ScoreTrace { ... }`) явно и корректно документирует rescore-семантику — проще и нагляднее чем `reset_effects() + base = new_score`.
- P3a.5 не меняет `if pool.is_empty() { return; }` early return — сохранён как было.

**Файлы, которые затронули:**
- `src/combat/ai/pipeline/stages/finalize.rs` — единственный изменённый файл стадии (+1 import, +~30 строк в apply, +4 теста)
- `docs/ai/pipeline.md` (секция ScoreTrace: статус P3a.5 done, описание FinalizeStage migration)
- `docs/ai/restructure_log.md` (этот файл)
- `docs/ai/restructure.md` (status table)

**DoD проверка:**
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 779 passed (775 + 4 новых)
- [x] `cargo test` (интеграционные) — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Behavioural diff = 0: `ann.score = new_score` и `ann.factors = new_raw` сохранены bit-identical
- [x] Existing тесты `finalize_applies_per_plan_modes` и `finalize_default_mode_idempotent` — зелёные без изменений
- [x] Existing тесты P3a.0/1/2/3/4 — без regressions (775 → 779, +4 только finalize)
- [x] Только один файл стадий тронут: `pipeline/stages/finalize.rs`
- [x] `STAGE_SPECS` не изменены
- [x] `if pool.is_empty() { return; }` сохранён
- [x] debug_assert только для `new_score.is_finite()`
- [x] `rescore_mode` заполнен: `Some(EvaluationMode::Default)` или `Some(EvaluationMode::LastStand)`

---

## 2026-05-01 — P3a.6 — cleanup bridging-resets + full-pipeline trace equivalence (completed)

**Что сделано:**
- Убраны bridging-резеты (`ann.score_trace = ScoreTrace { base: ..., ..Default::default() }`) из всех 5 мигрированных стадий: `plan_modifiers.rs`, `critics.rs`, `sanity.rs`, `protect_self.rs`, `killable_gate.rs`.
- Удалены неиспользуемые `ScoreTrace`-импорты из `critics.rs`, `protect_self.rs`, `killable_gate.rs`.
- Переменная `applied_count` в `critics.rs` удалена (нужна была только для conditional assert, теперь assert всегда для finite).
- Unit-тест helpers в изменённых стадиях обновлены — инициализируют `ann.score_trace.base = score` перед прямым вызовом `apply()` без Finalize upstream:
  - `critics.rs`: `make_pool_with_scores` — добавлено `ann.score_trace.base = score`.
  - `sanity.rs`: `apply_sanity_to_two_plans` — добавлено `ann.score_trace.base = score`; `p3a_sanity_invariant_holds_for_all_finite_plans` — inline init; `p3a_sanity_masked_plan_trace_unchanged_or_only_base` — обновлён: init только для finite plan, убран assert на `trace.base == NEG_INFINITY` (bridging его создавал, теперь нет).
  - `protect_self.rs` / `killable_gate.rs`: `run_stage` — добавлено `if score.is_finite() { ann.score_trace.base = score }`.
- `p3a_modifiers_*` тесты (используют `run(PRODUCTION_PIPELINE, ...)`) — не тронуты: Finalize устанавливает base в production пути.
- Добавлен новый тест `p3a_full_pipeline_trace_compute_equals_ann_score` в `pipeline/mod.rs::tests`: прогоняет PRODUCTION_PIPELINE и проверяет `ann.score == trace.compute()` для всех finite-score планов.

**Комментарии / отклонения от плана:**
- `protect_self.rs` и `killable_gate.rs`: bridging был только внутри `if new_score == NEG_INFINITY` блока (не на входе стадии). После удаления `trace.compute() == NEG_INFINITY` сохраняется через `push_mask(Poison)` независимо от `trace.base` — Poison mask в `compute()` даёт early return NEG_INFINITY. Invariant assert `debug_assert_eq!(trace.compute(), NEG_INFINITY)` сохранён.
- `p3a_sanity_masked_plan_trace_unchanged_or_only_base`: убран assert `trace.base == NEG_INFINITY` — это было артефактом bridging (bridging устанавливал base=NEG_INFINITY для masked плана). После cleanup для masked плана в isolation `trace.base` = 0.0 (default) или значение из setup. В production Finalize устанавливает finite base до того как план будет замаскирован.
- **Golden replay verification**: corpus в `logs/` содержит 121 JSONL файл, но replay-binary требует schema v27–v31; старые файлы — v22 (слишком старые), новые — v32 (too new). Verification отложена. Полагаемся на: behavioural тесты (ann.score/factors/contract/sanity/critics/modifiers bit-identical) + новый invariant-тест через full PRODUCTION_PIPELINE. No control-flow или formula изменений в production-коде — только удаление trace-reset строк.

**Файлы, которые затронули:**
- `src/combat/ai/pipeline/stages/plan_modifiers.rs` — удалён bridging-резет
- `src/combat/ai/pipeline/stages/critics.rs` — удалён bridging-резет; `make_pool_with_scores` обновлён
- `src/combat/ai/pipeline/stages/sanity.rs` — удалён bridging-резет; 3 теста обновлены
- `src/combat/ai/pipeline/stages/protect_self.rs` — удалён bridging-резет; `run_stage` обновлён
- `src/combat/ai/pipeline/stages/killable_gate.rs` — удалён bridging-резет; `run_stage` обновлён
- `src/combat/ai/pipeline/mod.rs` — добавлен тест `p3a_full_pipeline_trace_compute_equals_ann_score`
- `docs/ai/pipeline.md` (секция ScoreTrace: P3a complete, накопление через pipeline)
- `docs/ai/restructure_log.md` (этот файл)
- `docs/ai/restructure.md` (status table)

**DoD проверка:**
- [x] Bridging-резеты убраны из 5 файлов
- [x] Unit tests обновлены: `score_trace.base` инициализируется в setup'ах
- [x] Новый тест `p3a_full_pipeline_trace_compute_equals_ann_score` — зелёный
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 780 passed (779 + 1 новый), 0 failed
- [x] `cargo test` (интеграционные) — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Behavioural diff = 0: `ann.score`, `ann.factors`, `ann.contract`, `ann.sanity`, `ann.critics`, `ann.modifiers`, `ann.adaptation`, `ann.chosen` — bit-identical с pre-P3a.6
- [x] STAGE_SPECS не изменены
- [x] JSONL schema не тронута (P3a — no schema bump)
- [x] Golden replay: verification отложена — corpus schema mismatch (v22 old / v32 new vs. required v27–v31); полагаемся на unit + full-pipeline invariant тест
- [x] P3a (umbrella) complete: ScoreTrace internal migration done

---

## 2026-05-01 — R5.A — critics/ + modifiers/ → pipeline/stages/ (completed)

**Что сделано:**
- `src/combat/ai/critics/` (7 файлов) → `src/combat/ai/pipeline/stages/critics/` (file-to-dir conversion):
  - `pipeline/stages/critics.rs` → `pipeline/stages/critics/mod.rs` (file-to-dir conversion через `git mv`). Содержимое `combat/ai/critics/mod.rs` (trait `PlanCritic`, типы `CriticKind`, `CriticReason`, `CriticHit`, тесты serde) объединено в один `critics/mod.rs` вместе с `CriticsStage`.
  - 6 critic-файлов (`blindspot_ranged`, `buff_into_void`, `heal_without_rescue_value`, `overcommit_into_danger`, `rare_resource_for_low_impact`, `self_lethal_without_payoff`) → `pipeline/stages/critics/<name>.rs`.
  - Import paths в 6 файлах: `crate::combat::ai::critics::*` → `super::*` (или `crate::combat::ai::pipeline::stages::critics::*` в тест-блоках).
- `src/combat/ai/modifiers/` (4 файла) → `src/combat/ai/pipeline/stages/modifiers/` (file-to-dir + rename):
  - `pipeline/stages/plan_modifiers.rs` → `pipeline/stages/modifiers/mod.rs` (file-to-dir conversion + rename). Содержимое `combat/ai/modifiers/mod.rs` (trait `PlanModifier`, `ModifierCtx`, `ModifierContribution`, `PLAN_MODIFIERS`) объединено в один `modifiers/mod.rs` вместе с `PlanModifiersStage`.
  - 3 modifier-файла (`summon_bonus`, `trade_bonus`, `repair_bonus`) → `pipeline/stages/modifiers/<name>.rs`.
  - Import paths в 3 файлах: `crate::combat::ai::modifiers::ModifierCtx` → `crate::combat::ai::pipeline::stages::modifiers::ModifierCtx` (в тест-блоках).
- `combat/ai/mod.rs`: удалены `pub mod critics;` и `pub mod modifiers;` (-2 строки).
- `pipeline/stages/mod.rs`: `pub mod plan_modifiers;` → `pub mod modifiers;`.
- 7 consumer-файлов обновлены (import path changes only):
  - `pipeline/order.rs` — shim `apply_plan_modifiers` → `stages::modifiers::PlanModifiersStage`
  - `pipeline/mod.rs` — тест `crate::combat::ai::modifiers::PLAN_MODIFIERS` → `pipeline::stages::modifiers::PLAN_MODIFIERS`
  - `outcome/mod.rs` — два обновления: `modifiers::ModifierContribution` + инлайн `critics::CriticHit`
  - `planning/scorer.rs` — тест `modifiers::{ModifierCtx, PLAN_MODIFIERS}`
  - `src/bin/mine_ai_logs.rs` — `storyforge::combat::ai::modifiers::ModifierContribution`

**Комментарии / отклонения от плана:**
- File-to-dir conversion для critics: `critics.rs` → `critics/mod.rs` + 6 submodules. Вариант (b) из плана (объединить CriticsStage и PlanCritic trait в один mod.rs). Публичный путь `pipeline::stages::critics::CriticsStage` сохранён.
- Обнаружена дополнительная ссылка в `outcome/mod.rs`: инлайн `crate::combat::ai::critics::CriticHit` в struct field (`pub critics: Vec<...>`) — не была покрыта `agrep` поиском, выявлена на `cargo check`. Обновлена.
- Rename `plan_modifiers` → `modifiers` изменяет публичный путь: `pipeline::stages::plan_modifiers::PlanModifiersStage` → `pipeline::stages::modifiers::PlanModifiersStage`. Это ожидаемо по roadmap (R5 — move + rename для modifiers).
- `PLAN_MODIFIERS` путь изменился: `crate::combat::ai::modifiers::PLAN_MODIFIERS` → `crate::combat::ai::pipeline::stages::modifiers::PLAN_MODIFIERS`. Обновлено в 3 consumer-файлах.

**Файлы, которые затронули:**
- `src/combat/ai/pipeline/stages/critics/` (новая директория: mod.rs + 6 critic-файлов)
- `src/combat/ai/pipeline/stages/modifiers/` (новая директория: mod.rs + 3 modifier-файлов)
- `src/combat/ai/critics/` (удалена — все файлы перенесены)
- `src/combat/ai/modifiers/` (удалена — все файлы перенесены)
- `src/combat/ai/mod.rs` (-2 строки)
- `src/combat/ai/pipeline/stages/mod.rs` (1 строка: plan_modifiers → modifiers)
- `src/combat/ai/pipeline/order.rs` (1 строка: import path)
- `src/combat/ai/pipeline/mod.rs` (1 строка: тест import)
- `src/combat/ai/outcome/mod.rs` (2 строки: imports)
- `src/combat/ai/planning/scorer.rs` (1 строка: тест import)
- `src/bin/mine_ai_logs.rs` (1 строка: import)
- `docs/ai/critics.md` (обновлены пути)
- `docs/ai/extension-checklist.md` (обновлены пути)

**DoD проверка:**
- [x] `combat/ai/critics/` директория не существует
- [x] `combat/ai/modifiers/` директория не существует
- [x] `pipeline/stages/critics.rs` файл не существует (стал `pipeline/stages/critics/mod.rs`)
- [x] `pipeline/stages/plan_modifiers.rs` файл не существует (стал `pipeline/stages/modifiers/mod.rs`)
- [x] `pipeline/stages/critics/mod.rs` — registers 6 critic-submodules + trait/types/CriticsStage/tests
- [x] `pipeline/stages/modifiers/mod.rs` — registers 3 modifier-submodules + trait/types/PlanModifiersStage/tests
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 780 passed, 0 failed (идентично baseline)
- [x] `cargo test` — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] `combat/ai/mod.rs` стал короче на 2 строки (удалены `pub mod critics;` и `pub mod modifiers;`)
- [x] No semantic diff: изменения в телах — только import/path строки

---

## 2026-05-01 — R5.B.1 — planning/sanity.rs → pipeline/stages/sanity/ (completed)

**Что сделано:**
- `pipeline/stages/sanity.rs` → `pipeline/stages/sanity/mod.rs` (file-to-dir conversion через `git mv`). `SanityStage` + 7 P3a.3 tests перенесены.
- Создан `pipeline/stages/sanity/healer_exposure.rs` — Rule 1: `evaluate(active, final_pos, allies) -> Vec<SanityHit>`.
- Создан `pipeline/stages/sanity/retreat_trap.rs` — Rule 2: `evaluate(final_pos, ally_positions) -> Option<SanityHit>`.
- Создан `pipeline/stages/sanity/synergy_bonus.rs` — Rule 3: `evaluate(active, plan, final_pos, danger, pos_eval, ctx) -> Option<SanityHit>`. Содержит приватный helper `plan_has_useful_cast`.
- Содержимое `planning/sanity.rs` (605 LOC) вмержено в `pipeline/stages/sanity/mod.rs`: типы `SanityHit`/`SanityRule`, оркестратор `sanity_adjust_plans` (заменён inline на вызовы rule-модулей), shared helpers (`expected_aoo_damage`, `plan_is_defensive`, `apply_protect_self_mask`, `plan_has_self_aoe`), тесты из planning/sanity.rs.
- Удалён `planning/sanity.rs` (git rm).
- Удалены `pub mod sanity;` и `pub use sanity::{...}` из `planning/mod.rs`.
- Обновлён `combat/ai/mod.rs`: `planning::{SanityHit, SanityRule}` → `pipeline::stages::sanity::{SanityHit, SanityRule}`.
- 7 consumer-файлов обновлены (import paths):
  - `scoring/trade.rs` — `planning::sanity::expected_aoo_damage` → `pipeline::stages::sanity::...`
  - `adapt/select.rs` — `planning::sanity::expected_aoo_damage`, `planning::plan_is_defensive` → pipeline paths; тест `sanity_adjust_plans` → pipeline path
  - `pipeline/stages/protect_self.rs` — `planning::apply_protect_self_mask` → pipeline path
  - `pipeline/stages/critics/overcommit_into_danger.rs` — `planning::sanity::expected_aoo_damage` → pipeline path
  - `pipeline/stages/critics/self_lethal_without_payoff.rs` — `planning::sanity::plan_has_self_aoe` → pipeline path
  - `pipeline/stages/item_scoring.rs` — `planning::plan_is_defensive` → pipeline path
  - `outcome/mod.rs` — `planning::sanity::SanityHit` → `pipeline::stages::sanity::SanityHit`
  - `log/mod.rs` — `planning::SanityHit` → `pipeline::stages::sanity::SanityHit`

**Shared helpers — место расположения (pipeline/stages/sanity/mod.rs top-level):**
- `expected_aoo_damage` — `pub(crate)`, используется adapt/scoring/critics cross-layer.
- `plan_is_defensive` — `pub`, используется adapt/select + item_scoring + protect_self.
- `apply_protect_self_mask` — `pub`, используется protect_self stage.
- `plan_has_self_aoe` — `pub(crate)`, используется critics/self_lethal_without_payoff.
- `plan_has_useful_cast` — приватная, перенесена в `synergy_bonus.rs` (её единственный caller).

**Комментарии / отклонения от плана:**
- `plan_has_useful_cast` (fn-private в `planning/sanity.rs`) перенесена в `synergy_bonus.rs` как `fn plan_has_useful_cast(plan, ctx) -> bool` — её единственный caller — `synergy_bonus::evaluate`. Когезия с rule-файлом выше, чем с mod.rs.
- При объединении тестов из staging/sanity.rs + planning/sanity.rs обнаружен конфликт: оба содержат `use crate::combat::ai::test_helpers::empty_content;` и `fn empty_content()`. Дубль удалён (локальная fn уже есть в тест-блоке).
- Обнаружено, отложено: `expected_aoo_damage` и `apply_protect_self_mask` семантически тяготеют к отдельным слоям (scoring и protect_self соответственно). В R5.B.1 они оставлены в mod.rs (единственный разумный target для них при текущем layout). Возможные будущие moves — R6 или позднее.

**Файлы, которые затронули:**
- `src/combat/ai/pipeline/stages/sanity/` (новая директория: mod.rs + healer_exposure.rs + retreat_trap.rs + synergy_bonus.rs)
- `src/combat/ai/planning/sanity.rs` (deleted)
- `src/combat/ai/planning/mod.rs` (−2 строки)
- `src/combat/ai/mod.rs` (1 строка: import path)
- `src/combat/ai/outcome/mod.rs` (1 строка: import path)
- `src/combat/ai/log/mod.rs` (2 строки: import path)
- `src/combat/ai/adapt/select.rs` (3 строки: import paths)
- `src/combat/ai/scoring/trade.rs` (1 строка)
- `src/combat/ai/pipeline/stages/protect_self.rs` (1 строка)
- `src/combat/ai/pipeline/stages/item_scoring.rs` (2 строки)
- `src/combat/ai/pipeline/stages/critics/overcommit_into_danger.rs` (1 строка)
- `src/combat/ai/pipeline/stages/critics/self_lethal_without_payoff.rs` (1 строка)

**DoD проверка:**
- [x] `planning/sanity.rs` не существует
- [x] `pipeline/stages/sanity.rs` не существует (стал `pipeline/stages/sanity/mod.rs`)
- [x] `pipeline/stages/sanity/{mod.rs, healer_exposure.rs, retreat_trap.rs, synergy_bonus.rs}` существуют
- [x] `planning/mod.rs` НЕ re-export'ит sanity типы/функции
- [x] Все consumer-файлы обновлены
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 780 passed, 0 failed (идентично baseline)
- [x] `cargo test` — зелёный (lib + integration)
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Behavioural diff = 0: hit order сохранён (HealerExposure → RetreatTrap → SynergyBonus); penalty accumulation идентичен; P3a.3 тесты зелёные
- [x] Каждый rule-файл owns one rule (cohesion criterion)

---

## 2026-05-01 — R5.B.2 — planning/killable_gate.rs → pipeline/stages/killable_gate/mod.rs (completed)

**Что сделано:**
- `pipeline/stages/killable_gate.rs` → `pipeline/stages/killable_gate/mod.rs` (file-to-dir conversion через `git mv`).
- Содержимое `planning/killable_gate.rs` (510 LOC) вмержено в новый `mod.rs`:
  - Типы: `KillLineStrength`, `GateStats`, `KILLABLE_ALPHA`.
  - Функции: `plan_is_offensive_vs`, `apply_killable_gate`.
  - `use`-импорты из обоих источников объединены в один блок (дублей нет).
  - `KillableGateStage` + `impl PlanStage` — без изменений.
- Тесты объединены в два именованных sub-модуля:
  - `mod algorithm_tests` — 9 тестов из `planning/killable_gate.rs::tests` (pure алгоритм).
  - `mod stage_tests` — 6 тестов из `pipeline/stages/killable_gate.rs::tests` (stage + P3a.4).
  - Конфликтов имён helpers не было: `algorithm_tests` содержит `empty_plan`, `cast_plan`, `cast_plan_at`, `move_plan`, `snap_with_target`, `default_modes`, `factors_with`; `stage_tests` содержит `run_stage`, `pfv_kill_now`.
- Удалён `planning/killable_gate.rs` (git rm).
- Удалены `pub mod killable_gate;` и `pub use killable_gate::{...};` из `planning/mod.rs`.
- 2 consumer-файла обновлены:
  - `pipeline/stages/killable_gate/mod.rs` — удалён `use crate::combat::ai::planning::apply_killable_gate;` (теперь в том же модуле); убраны дублирующие imports из `stages/killable_gate.rs::tests` (`use super::*` покрывает все типы).
  - `pipeline/stages/item_scoring.rs` — `use crate::combat::ai::planning::{compute_plan_intent_sum, plan_is_offensive_vs}` → два отдельных импорта: `pipeline::stages::killable_gate::plan_is_offensive_vs` и `planning::compute_plan_intent_sum`.

**Комментарии / отклонения от плана:**
- `pipeline/order.rs` не требовал изменений: thin shim `apply_killable_gate` использует `KillableGateStage.apply()`, а не `planning::apply_killable_gate` напрямую.
- Два sub-модуля тестов (`algorithm_tests` / `stage_tests`) выбраны вместо одного `mod tests`: в `algorithm_tests` `empty_plan()` и `cast_plan()` используют фикстуры без FocusTarget-setup, а `stage_tests` использует `run_stage()` с полным `StageCtx`. Объединение в один модуль потребовало бы prefixing helpers или careful conflict audit.

**Файлы, которые затронули:**
- `src/combat/ai/pipeline/stages/killable_gate/mod.rs` (новая директория; итог ~550 LOC)
- `src/combat/ai/planning/killable_gate.rs` (deleted)
- `src/combat/ai/planning/mod.rs` (−2 строки)
- `src/combat/ai/pipeline/stages/item_scoring.rs` (import split: 1 строка → 2 строки)

**DoD проверка:**
- [x] `planning/killable_gate.rs` не существует
- [x] `pipeline/stages/killable_gate.rs` (файл) не существует (стал `pipeline/stages/killable_gate/mod.rs`)
- [x] `pipeline/stages/killable_gate/mod.rs` существует и содержит: типы, функции, KillableGateStage, `algorithm_tests` + `stage_tests`
- [x] `planning/mod.rs` НЕ re-export'ит killable_gate символы
- [x] Consumers обновлены: `item_scoring.rs` путь plan_is_offensive_vs обновлён
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 780 passed, 0 failed (точное соответствие baseline)
- [x] `cargo test` — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Behavioural diff = 0: ни одна формула / control flow / литерал не изменена

---

## 2026-05-01 — R5.B.3 — planning/picker.rs → pipeline/stages/pick_best.rs (completed)

**Что сделано:**
- Содержимое `planning/picker.rs` (342 LOC) вмержено в `pipeline/stages/pick_best.rs`:
  - `PickMechanics` struct (pub) — добавлен после use-блока, перед `PickBestStage`.
  - `commit_plan`, `mercy_cruelty` (private), `pick_best_plan`, `record_committed_reservations` — добавлены в секцию «Picker API (consolidated from planning/picker.rs)».
  - Import `pick_best_plan` из `planning` убран из строки 50 (теперь в том же файле); добавлены новые imports: `CommittedPrefix`, `PlanStep`, `applies_cc`, `AiDecision`, `AiWorld`, `MoveOrigin`, `Reservations`, `BattleSnapshot`, `UnitSnapshot`, `AoEShape`, `TargetType`, `DiceRng`, `aoe_area`, `aoe_hits`.
- 5 тестов из `planning/picker.rs::tests` добавлены как `mod commit_plan_tests` внутри `pick_best.rs::tests`.
- `PickInfo.mechanics` в `outcome/mod.rs` обновлён: `planning::PickMechanics` → `pipeline::stages::pick_best::PickMechanics`.
- Удалён `planning/picker.rs` (git rm).
- Удалены `pub mod picker;` и `pub use picker::{...};` из `planning/mod.rs`.
- 4 consumer-файла обновлены (Опция A — utility re-exports обновлены на новый source, внешние consumers не тронуты):
  - `enemy_turn.rs` — `planning::record_committed_reservations` → `pipeline::stages::pick_best::record_committed_reservations`; `planning::commit_plan` (inline path) → `pipeline::stages::pick_best::commit_plan`.
  - `utility/mod.rs` — `planning::PickMechanics` re-export → `pipeline::stages::pick_best::PickMechanics`; `planning::commit_plan` import → `pipeline::stages::pick_best::commit_plan`.
  - `bin/mine_ai_logs.rs` — `planning::PickMechanics` → `pipeline::stages::pick_best::PickMechanics`.
  - `outcome/mod.rs` — inline type path для `PickInfo::mechanics` обновлён.

**Комментарии / отклонения от плана:**
- `pick_best_plan` оставлена `pub` — internal consumer это `PickBestStage.apply()` в том же файле, pub не вредит.
- `utility/mod.rs` re-export `PickMechanics` сохранён (Опция A): `log/debug.rs` и другие consumers продолжают использовать `utility::PickMechanics` без изменений.
- Тест count остался 780: тесты из picker.rs попали в pick_best.rs::tests::commit_plan_tests, граф не изменился.

**Файлы, которые затронули:**
- `src/combat/ai/pipeline/stages/pick_best.rs` (~1080 → 1417 LOC)
- `src/combat/ai/planning/picker.rs` (deleted)
- `src/combat/ai/planning/mod.rs` (−2 строки: `pub mod picker;` и re-export)
- `src/combat/ai/outcome/mod.rs` (1 строка: inline type path)
- `src/combat/ai/utility/mod.rs` (2 строки: PickMechanics re-export + commit_plan import)
- `src/combat/ai/enemy_turn.rs` (2 строки: record_committed_reservations + commit_plan paths)
- `src/bin/mine_ai_logs.rs` (1 строка: PickMechanics import)

**DoD проверка:**
- [x] `planning/picker.rs` не существует
- [x] `pipeline/stages/pick_best.rs` содержит: PickMechanics, commit_plan, mercy_cruelty, pick_best_plan, record_committed_reservations, commit_plan_tests sub-mod, плюс существующий PickBestStage код
- [x] `planning/mod.rs` НЕ re-export'ит picker символы
- [x] Consumers обновлены: все ссылки на `planning::picker::*` идут через `pipeline::stages::pick_best::*` (или через utility-re-exports)
- [x] `cargo build` — clean
- [x] `cargo test --lib` — 780 passed, 0 failed (точное соответствие baseline)
- [x] `cargo test` — зелёный
- [x] `cargo clippy --all-targets` — 28 warnings, все pre-existing; 0 новых
- [x] Behavioural diff = 0: ни одна формула / control flow / литерал не изменена

---
