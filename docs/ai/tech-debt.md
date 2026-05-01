# AI Tech Debt — Followup Roadmap

Проблемы организации и логики, обнаруженные в процессе пост-restructure-аудита (см. git history `ai/P0..ai/R-late`). Не требуют немедленного исправления — реструктуризация дала чистый layout, и эти пункты можно закрывать инкрементально, отдельными PR.

Принцип: каждая запись описывает **что не так**, **почему это болит**, и **как закрыть** (с примерным scope'ом). Группы по убыванию воздействия.

---

## A. Семантические рассогласования (имена ↔ смысл)

### A1. `finalize_scores` — имя лжёт

**Где:** `src/combat/ai/scoring/factors/aggregate.rs::finalize_scores`.

**Проблема.** Функция называется «finalize», но используется тремя разными путями:
- Initial scoring: `score_plans_with_raw → finalize_scores`.
- Rescore: `rescore_with_per_plan_modes → finalize_scores`.
- Future value lookahead: `plan/future_value.rs::finalize_scores`.

Это **primitive «свернуть raw factors в score»**, а не «финализация» в смысле `FinalizeStage`.

**Закрытие.** Переименовать в `aggregate_factors_to_score` или `combine_raw_factors`. Pure relocation, ~3-4 callsites. PR ≤ 100 LOC.

### A2. `KillableGate` — design ↔ implementation mismatch

**Где:** `src/combat/ai/pipeline/stages/killable_gate/mod.rs`, `src/combat/ai/pipeline/spec.rs::STAGE_SPECS`.

**Проблема.** В `STAGE_SPECS` стадия маркирована как `ScoreEffect::PostScoreGate` (семантика «помечает план через флаг, не зануляет score»). Фактический код ставит `ann.score = NEG_INFINITY` — это `Mask`. P3a.4 обходит это эмиссией double hit (`GateHit { Reject } + MaskHit { Poison }`) для сохранения `compute()` invariant'а.

**Почему болит.** Двойная эмиссия — workaround. Реальный Gate-flow (план остаётся в pool, PickBest читает `is_gated()` и понижает приоритет на ранге, а не зануляет score) не реализован.

**Закрытие.** Slice уровня P-track:
1. KillableGateStage пушит только `GateHit { Reject }` (не Mask).
2. `ann.score` НЕ ставится в NEG_INFINITY — план остаётся со своим score.
3. PickBestStage учитывает `is_gated()` через penalty или фильтрацию (но не early-skip — gated план может быть единственным подходящим).
4. Behavioural diff = 0 verifier через golden corpus (если получится оживить, см. C2).

Scope ~ 200-400 LOC, риск средний (поведенческий drift возможен — на gated плане сейчас все pipeline-стадии downstream видят `-∞` и пропускают, после изменения они будут запускать модификаторы).

### A3. `apply_protect_self_mask` и `expected_aoo_damage` — wrong neighbourhood

**Где:** `src/combat/ai/pipeline/stages/sanity/mod.rs`.

**Проблема.** `apply_protect_self_mask` — единственный consumer этой функции — `pipeline/stages/protect_self.rs`. `expected_aoo_damage` — общий snapshot-fact, используется adapt/scoring/critics layers, не специфичен для sanity.

**Закрытие.** Pure relocation:
- `apply_protect_self_mask` → консолидировать в `pipeline/stages/protect_self.rs`.
- `expected_aoo_damage` → вынести в `world/snapshot.rs` или `scoring/horizon.rs` (где живут другие damage-helpers).

Scope ~ 50-150 LOC.

---

## B. Архитектурные слабости (типовая дисциплина)

### B1. `PlanAnnotation` — flat bag из ~20 полей

**Где:** `src/combat/ai/outcome/mod.rs::PlanAnnotation`.

**Проблема.** Каждая стадия пишет свой slice (sanity, critics, modifiers, contract, adaptation, score_trace, agenda_item, considerations_per_item, ...). Структура открыта для записи всем; типовой системы «стадия X может писать только Y» нет. `StageSpec` validator в `pipeline/spec.rs` ловит эти контракты в runtime-тесте, но структура остаётся незащищённой.

**Почему болит.** Регрессии «стадия по ошибке записывает не своё поле» поймаются только тестом, и только если конкретно этот тест есть. Refactor приводит к утечкам ответственности.

**Закрытие — варианты:**
- **Hard:** sealed annotation с per-stage trait'ами (`SanityWriter`, `CriticsWriter`, etc) + private fields. Большой refactor, ~2-3 дня.
- **Soft:** `#[non_exhaustive]` + helpers `set_sanity(&mut self, hits)` etc, чтобы code review хотя бы видел «эта функция трогает sanity». Меньший scope, ~1 день.

### B2. Параллельная observability state

**Где:** `PlanAnnotation` имеет одновременно:
- `score_trace_log: Option<ScoreTraceLog>` (P3b — typed effect log).
- `modifiers: Vec<ModifierContribution>`, `sanity: Vec<SanityHit>`, `critics: Vec<CriticHit>`, `adaptation: Option<AdaptationData>`, `contract: Option<ContractMaskHit>` (legacy per-effect channels).

**Проблема.** Они кодируют **одну и ту же информацию** на разных уровнях. После P3b каждая стадия пишет в оба канала. Каналы могут drift'ить (pre-existing observability ловит то, что score_trace не ловит, и наоборот). Mining/replay tooling обращается к разным источникам.

**Почему болит.** Дубликат state требует синхронизации. Любой новый score-effect нужно writeable и в trace, и в legacy field. Drift — class бесшумных багов.

**Закрытие.**
1. Schema bump (v35): убрать legacy fields из JSONL output (но оставить runtime для backward-compat consumers если есть).
2. Mining/replay полностью мигрируют на `score_trace_log` (sweep по mining classes E/G/A).
3. После миграции legacy-fields удаляются.

Scope: ~1-2 дня + миграция corpus.

### B3. Cross-stage helpers в factor leaves

**Где:** `compute_plan_self_survival`, `compute_plan_tempo_gain` остаются `pub` после P5 — потому что используются `scoring/factors/aggregate.rs` и `pipeline/stages/item_scoring.rs`, не только своим leaf'ом.

**Проблема.** Идея factor leaves: «один factor = один self-contained файл». Реальность: некоторые factors имеют дополнительные cross-stage entry points для предварительной оценки. `pub` визибельность это не нарушает, но размывает model.

**Закрытие.** Документировать в `appraisal/` или `scoring/factors/mod.rs` отдельный «pre-step factor evaluation API» как явный интерфейс. Либо — вынести cross-stage helpers в общий модуль (`scoring/preview.rs` или подобное).

### B4. Test-vs-production divergence по trace.base

**Проблема.** В production `FinalizeStage` устанавливает `score_trace.base = new_score`. Все downstream стадии полагаются на это. В тестах же стадия запускается в изоляции: тест должен **вручную** инициализировать `trace.base = entry_score` перед apply. Если забыть — `compute()` invariant ломается, тест проходит ложно.

**Закрытие.** Test harness:
```rust
TestStageHarness::new(actor)
    .with_intent(TacticalIntent::Reposition)
    .with_initial_scores(&[1.0, 0.5])  // также инициализирует trace.base
    .run(SanityStage)
```
Setup трачивает 1 строку на test вместо 6-7. Scope ~200 LOC harness + миграция ~30 тестов. PR ≤ 1 день.

---

## C. Инфраструктурные пробелы

### C1. `ScoringCtx` стал `Copy` ради тестов

**Где:** `src/combat/ai/orchestration/mod.rs::ScoringCtx`.

**Проблема.** В R6 добавлен `#[derive(Clone, Copy)]` чтобы aggregate-тесты могли передавать ctx нескольким функциям. ScoringCtx — context struct из ссылок (`&AiWorld`, `&BattleSnapshot`, etc). Copy «безопасно» в том смысле что копирование ссылок — no-op. Но Copy на context-type приглашает к неявному копированию там, где должна быть передача `&ScoringCtx`.

**Почему болит.** Code smell. Lifetime-баги легче возникают и сложнее отлавливаются.

**Закрытие.** Удалить `Copy`, сохранить `Clone`. Тесты обновить на explicit `ctx.clone()` или передачу `&ctx`. Scope ~ 30 callsites.

### C2. Schema migration tooling отсутствует

**Проблема.** За реструктуризацию прошли v22 → v34 (≥12 schema bump'ов). Validator hard-reject'ит corpus с `schema_version < SCHEMA_VERSION - 1`. Каждый bump делает старый corpus мёртвым — миграции нет.

**Почему болит.** На самых рискованных slice'ах (P3a.6, R6, P5+R4) `golden replay diff = 0` был **обязательным DoD**, но фактически недоступным — corpus был v22, нужен v33+. Самый сильный behavioural-equivalence guard был unavailable; полагались на test suite (слабее).

**Закрытие — варианты:**
- **Migration tool:** `replay_ai_log --migrate-to v34 <old.jsonl>` который пропускает события через upgrade-stages. Каждый schema bump добавляет new migrator. Scope: значительный (несколько дней) — нужна цепь миграторов.
- **Continuous re-capture:** после каждого schema bump'а сразу запускать `--capture-golden` на свежем corpus. Дешевле, но требует discipline.

Рекомендация — **continuous re-capture** + добавить в `extension-checklist.md` пункт «после schema bump'а пере-захватить golden corpus».

### C3. `pub(crate)` → `pub` промоушены ради re-exports

**Где:** `build_summon_dpr_cache` и подобные. R6 потребовал поднять видимость для `pub use` в module re-exports.

**Проблема.** Rust не различает «public API модуля внутри крейта» от «public API крейта наружу». Ради цепочки `pub use` приходится поднимать визибельность сверх необходимого.

**Почему болит.** Ослабление encapsulation. Внешний код может случайно начать использовать символ, не предназначенный для широкого употребления.

**Закрытие.** Минор: использовать `pub(crate) use` где возможно вместо `pub use`. Не везде Rust позволит цепочку, но в большинстве случаев да. Scope ~ 1 час cleanup.

### C4. `AiTags` re-export через `world/snapshot.rs`

**Где:** R7-3 переместил `AiTags` в `world/tags/ai_tags.rs`, но оставил `pub use crate::combat::ai::world::tags::AiTags;` в `snapshot.rs` для backward-compat.

**Проблема.** Re-export — мост; consumers продолжают писать `world::snapshot::AiTags`. Canonical path размывается.

**Закрытие.** Sweep consumers на canonical path `world::tags::AiTags`, удалить re-export. Trivial, ~1 час.

---

## D. Тестовая дисциплина

### D1. Test setup boilerplate

**Проблема.** Каждый stage-test строит ритуал из ~7 строк (UnitBuilder → BattleSnapshot → empty_maps → Reservations → make_test_ctx → make_scoring_ctx → StageCtx::new). Дублируется в десятках тестов.

**Закрытие.** См. B4 — test harness покрывает оба case'а (setup + trace.base init).

### D2. Pre-existing parity tests могут таиться

**Урок реструктуризации.** До рефакторинга жили «duality» состояния (legacy code + новая ветвь под условным флагом или с parity-тестами). При R-track'е находили: `factors/saturation.rs` legacy + `factors/step/saturation.rs` leaf, `pipeline/stages/adaptation.rs` legacy + ModeSelection/Finalize new. Каждый из них — pre-existing tech debt, замаскированный комментарием «legacy reference».

**Закрытие — preventive:** при появлении подобных дубликатов фиксировать их сразу в этот документ как кандидата на cleanup, не оставлять «временно».

---

## E. Инвариант-pain (open question)

### E1. ScoreTrace algebra ↔ pre-P3a поведение — никогда полностью не verified

**Проблема.** `ScoreTrace::compute()` (mask poison → base → multipliers → addends) построен так, чтобы давать identical результат с ad-hoc математикой `ann.score *= multiplier; ann.score += contribution; ann.score = NEG_INFINITY`. Логически это та же формула; на практике порядок операций над f32 может отличаться (e.g. push-order vs apply-order). Roadmap требовал ε = 1e-5 на decisions через golden replay corpus.

**Реальность.** Corpus был несовместим (см. C2). `compute()` алгебра тестировалась в изоляции (8 unit-тестов в `score_trace.rs`). Decision-equivalence на реальных корпусах **не verified**.

**Риск.** Если есть behavioural drift — мы про него не знаем. Может быть нулевой; может быть ε ~ 10⁻⁴ на конкретных pos/neg-multiplier комбинациях.

**Закрытие.**
- Восстановить golden corpus (см. C2).
- Запустить `--compare-golden` на pre-P3a и post-P3b данных.
- Если decision drift — investigate (likely f32 commutativity issue в порядке push/apply), fix, re-verify.

Scope зависит от того, что найдётся. Может быть 0 (clean), может быть half-day investigation.

---

## Приоритизация

**Done first (low effort, high clarity gain):**
- A1 (rename finalize_scores).
- A3 (relocate apply_protect_self_mask + expected_aoo_damage).
- C4 (canonical AiTags path).
- C1 (remove ScoringCtx Copy).

**Medium-effort, medium-impact:**
- B4 + D1 (test harness).
- B3 (cross-stage factor entry doc).
- A2 (KillableGate true Gate semantics) — **требует** golden corpus (E1/C2).

**High-effort или требует другой работы:**
- B1 (typed annotation per-stage).
- B2 (deduplicate observability) — schema bump.
- C2 (schema migration tooling) — фундаментальная инфраструктура.
- E1 (verify ScoreTrace algebra) — блокер для A2.

**Не делать без явной причины:**
- D2 (preventive process — рефлекс, не задача).

---

## Когда обновлять этот документ

- При обнаружении новой структурной проблемы — добавлять в соответствующую группу.
- При закрытии пункта — удалять (или помечать `~~strikethrough~~` со ссылкой на коммит).
- Раз в квартал — review приоритизации; иногда low-effort пункты копятся, иногда high-effort блокируют другую работу.
