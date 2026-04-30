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
