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
