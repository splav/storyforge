# AI Restructure — Roadmap

Генеральный план структурной чистки `src/combat/ai/`. Артефакт двухстадийной критики архитектуры и согласования приоритетов. Цель — зафиксировать **контракты поведения pipeline до структурной чистки**: сначала закрыть systemic risks (порядок стадий, дрейф между production и тестами), потом распиливать файлы.

## Что это

Не план одной итерации. Два трека:

- **P-track (контракты + миграция логики)** — P0, P1, P2, P3a, P3b, P4, P5, P6, P7 в порядке убывания системного риска. Mainline `P0 → P1 → P2 → P3a → P3b` последовательная.
- **R-track (relocation overlay — структурная чистка top-level)** — R1…R7. Не отдельный сезон, а sub-slice'ы, привязанные к P-slice'ам. Цель — не делать import-churn по одним файлам дважды.

Каждый Slice — отдельная ветка/PR. Между Slice'ами — обязательный sweep легаси (см. Cross-cutting).

## Контекст: какие проблемы план закрывает

Краткое summary текущих болей (детальное обоснование — в conversation log, который привёл к этому документу):

1. **Два pipeline'а в коде.** `pipeline/mod.rs::run_pool_pipeline` (test-only legacy с порядком ≠ production) и инлайн-цепочка в `utility/mod.rs:404-419` (текущий step 11.4 порядок). Pipeline tests дают ложное чувство безопасности.
2. **Legacy `AdaptationStage`.** После split на `ModeSelection + Finalize` (step 11.0) старый монолитный stage остался в `pipeline/stages/adaptation.rs` (223 строки) как parity reference.
3. **Контракты порядка стадий — комментарии, не типы.** «OverlayConsiderations runs AFTER RepairAffinity», «Critics после Finalize» — программа компилируется в любом порядке.
4. **Score-mutation механизмы не унифицированы.** Шесть способов трогать `ann.score` (multiplier, addend, rescore, mask, gate, raw write) без общего интерфейса.
5. **Полу-завершённые рефакторы.** `factors/step/*`, `factors/plan/*` leaves существуют параллельно с `factors/saturation.rs`, `factors/scarcity.rs` и т.п.; `scoring.rs` (548 LOC) пересекается с `policy/*`.
6. **God-modules.** `intent/mod.rs` 2143, `planning/scorer.rs` 2377, `appraisal/mod.rs` 1084.
7. **Семантические долги.** `TacticalIntent::LastStand` никогда не выбирается `select_intent`; `IntentReason::Adapted` оборачивает причину intent-а в причину адаптации; `unit_value` (actor-agnostic) vs `target_priority` (relative) без формальной границы.

## Принципы

1. **Контракты до структуры.** Зафиксировать поведение pipeline'а (один источник правды, типизированные effects) до распилки файлов. Иначе красиво распилили — а главный риск (порядок стадий, дрейф score mutation) остался на комментариях.
2. **Минимальный шаг.** `PRODUCTION_PIPELINE: &[StageId]` лучше, чем сразу declarative DAG. Декларативный движок — когда от него уже что-то зависит.
3. **Унификация интерфейса ≠ слияние модулей.** Sanity и Critics получат общий effect-shape, но физически останутся отдельными — у них разная история, разные тесты, разные log-поля.
4. **Schema-touching изменения отдельным сезоном.** Всё, что трогает JSONL (LastStand split, IntentReason::Adapted, schema bump) — после структурного цикла; не смешивать с pipeline refactor.
5. **Post-slice sweep обязателен.** Три из шести наших проблем — один и тот же мета-баг «добавили новое, не удалили старое». Процедурное правило (см. Cross-cutting) предотвращает четвёртый случай.

## Рекомендованный порядок выполнения

Один пункт = один PR (либо P-slice, либо R-slice, либо естественная пара, которую разумно сделать одним движением). Детали каждого — в секциях ниже («Slice'ы» для P, «Relocation overlay» для R).

1. **P0** — Single pipeline declaration. С этого начинается всё; на нём держится остальное.

2. **R1** — `world/` + `config/` + `log/` umbrella. Pure relocation, parallel-safe. Идеально сразу после P0, чтобы быстро очистить top-level до того, как туда полезут новые файлы из P2 (StageSpec) и P3a (ScoreTrace).

3. **R3** — `scoring/` partial umbrella: `target_priority`, `position_eval`, `trade`, `policy/`, `scoring.rs → horizon.rs`. **Без `factors/`** — те ждут пары P5/R4. Pure relocation.

4. **P1 + R2** — remove legacy `AdaptationStage` + extract `planning/adaptation.rs → adapt/`. Естественная пара: оба про «убрать adaptation из неправильных мест».

5. **P2** — StageSpec + pipeline validator. Закрывает дыру «контракт порядка стадий — комментарий, не тип».

6. **P4** — Intent split (`kinds.rs` / `select.rs` / `score.rs` / `memory.rs`). Параллельно-безопасный slot между pipeline-фазами; делается тут чтобы позже не конкурировать за `intent/mod.rs` ни с кем.

7. **P3a** — ScoreTrace internal migration. Mainline; самый сильный архитектурный shift. Без schema bump.

8. **R5** — `pipeline/stages/` absorbs `critics/`, `modifiers/`, `planning/sanity.rs`, `planning/picker.rs`, `planning/killable_gate.rs`. **Hard requirement: после P3a**, иначе двойной import-churn по одним файлам.

9. **P5 + R4** — Factor refactor finalization + `factors/ → scoring/factors/`. Естественная пара: P5 финализирует содержимое (leaf owns implementation, плоские файлы удалены), R4 переносит уже стабильное состояние под `scoring/`.

10. **R6** — `planning/ → plan/` cleanup, включая ownership-split `planning/scorer.rs (2377 LOC)` на `pipeline/stages/finalize.rs` + `scoring/factors/aggregate.rs`. Самый рискованный move в R-track'е; требует завершённых R2, R5, P3a.

11. **P3b** — Expose ScoreTrace to JSONL/mining. Schema bump v33.

12. **P7** — Semantic cleanup: `LastStand` отдельный enum, `IntentReason::Adapted` → отдельный лог-блок, `target_priority` naming. Если готов одновременно с P3b — объединить в один schema release (v33 включает оба); если позже — отдельный bump v34.

13. **P6** — Replay tooling split (executor → `bin/`, assertion DSL остаётся в lib). Parallel-safe от любой точки; помещён сюда как явно low-priority hygiene.

14. **R7** *(optional)* — `memory/` extraction + `appraisal/` split. Может вообще не делаться, если AiMemory и goal lifecycle стабильны без grow-pains.

15. **R-late** *(optional)* — `utility/ → orchestration/`, `enemy_turn.rs → system.rs`. Косметика. Когда удобно или никогда.

**Mainline критический путь:** 1 → 4 → 5 → 7 → 11 (P0 → P1 → P2 → P3a → P3b). Всё остальное — либо pinned к mainline-узлу, либо parallel-safe от какой-то точки mainline'а.

## Slice'ы

### P0 — Single production pipeline declaration

**Цель.** Production и tests идут через один runner с одним списком стадий.

**Действия:**

- Завести `pub const PRODUCTION_PIPELINE` в `pipeline/mod.rs` (или отдельный `pipeline/order.rs`) через **fn-pointer-таблицу** (не `&dyn Trait` const, потому что `&'static dyn PlanStage` в `const` упирается в Rust object-safety / lifetime / Sync нюансы — fn pointer надёжнее):

  ```rust
  pub struct StageEntry {
      pub id: StageId,
      pub apply: fn(&mut StageCtx, &mut ScoredPool),
  }

  pub const PRODUCTION_PIPELINE: &[StageEntry] = &[
      StageEntry { id: StageId::Viability,        apply: apply_viability },
      StageEntry { id: StageId::ItemScoring,      apply: apply_item_scoring },
      StageEntry { id: StageId::ModeSelection,    apply: apply_mode_selection },
      StageEntry { id: StageId::Finalize,         apply: apply_finalize },
      // ...
  ];
  ```

  Тонкие `apply_*` функции (по одной на stage) делают `StageInstance.apply(ctx, pool)` — это thin shim. Можно оставить `PlanStage` trait для тестов и mock-stage'ей, но **production-список — через fn pointers**, не через trait objects в const.

  Контракт инвариантен: **dispatch — таблица, не match**.

- Добавить enum `StageId` (один на каждую существующую стадию).
- Runner — функция, принимающая список + `StageCtx` + `ScoredPool`. Внутри — итерация по таблице, вызов `entry.apply(ctx, pool)`. **Один dispatch point, ноль match-веток на 12 стадий.**
- `utility/mod.rs::pick_action` вызывает runner с `PRODUCTION_PIPELINE`.
- Существующий `run_pool_pipeline` либо удаляется, либо превращается в alias `run(PRODUCTION_PIPELINE, ...)`.
- Тесты pipeline-уровня вызывают тот же runner с тем же или явно отличным списком (для negative tests).

**Не делать в этом Slice:**

- Никаких StageSpec / reads-writes / score_effect полей. Только таблица и runner.
- Никакой validator-логики поверх — это P2.
- Никакого центрального match'а на StageId. Если он появится в runner'е — это сигнал, что dispatch построен неправильно.

**Definition of done.**

- `cargo test` зелёный.
- В коде ровно одно место, где зашит порядок production-стадий.
- В тесте `pipeline_runs_modifiers_after_repair_before_pick` (или его наследнике) фигурирует `PRODUCTION_PIPELINE` буквально.
- `git grep` по списку имён стадий в `utility/mod.rs:404-419` не находит инлайн-цепочки.

---

### P1 — Remove legacy `AdaptationStage`

**Цель.** В архитектуре остаются только `ModeSelectionStage` + `FinalizeStage`. Опасный historical artifact удалён.

**Действия:**

- Перенести parity-проверки из `pipeline/stages/adaptation.rs::tests` в `mode_selection.rs::tests` и `finalize.rs::tests` (они и так там частично есть — проверить покрытие).
- Удалить `pipeline/stages/adaptation.rs` целиком.
- В `pipeline/mod.rs` убрать импорт `adaptation::AdaptationStage` из `run_pool_pipeline` (если P0 уже свёл к `PRODUCTION_PIPELINE` — просто изъять `StageId::Adaptation`).
- `planning/adaptation.rs` оставить **как есть**: pure-algorithm module (data-types `EvaluationMode`, `AdaptationReason`; функции `apply_adaptation`, `select_evaluation_modes`). Не переносим логику внутрь стадий — это отдельный (более рискованный) refactor.
- Обновить `docs/ai/adaptation.md`: убрать упоминание legacy stage как «parity reference».

**Definition of done.**

- `pipeline/stages/adaptation.rs` не существует.
- `cargo test` зелёный.
- Доки актуальны.

---

### P2 — StageSpec + pipeline validator (slices C+D consolidated)

**Цель.** Ошибки порядка стадий и нарушения mutation semantics — test failures, а не комментарии.

Бывшие Slice C (reads/writes) и Slice D (score_effect kind) сливаются в один: spec без `score_effect` неполон (ловит «Overlay перед Repair», не ловит «Multiplier после Rescore»). Делать в две итерации = переписать validator второй раз.

**Структура:**

```rust
struct StageSpec {
    id: StageId,
    reads: &'static [AnnotationField],
    writes: &'static [AnnotationField],
    score_effect: Option<ScoreEffect>,
}

enum AnnotationField {
    RawFactors,
    Outcomes,
    Plan,
    SnapshotFacts,
    InitialScoreFacts,

    // Score разделён на семантические фазы — иначе validator
    // не различает «читаю baseline», «читаю промежуточный», «читаю final».
    ScoreBase,        // результат Finalize/Rescore
    ScoreEffects,     // multipliers/addends/masks/gates применены
    FinalScore,       // cached ann.score / trace.compute()

    RepairAffinity,
    PerItem,
    Eligibility,
    EvaluationMode,
}

enum ScoreEffect {
    PreScoreGate,    // Viability: фильтрует/помечает ДО Finalize, не трогает score
    Rescore,         // Finalize: устанавливает ScoreBase
    Multiplier,      // sanity, critics
    Addend,          // modifiers
    Mask,            // protect_self: -∞ poison
    PostScoreGate,   // killable_gate: работает после score-effects
}
```

**Почему `Gate` разделён на Pre/Post.** Старая формулировка одного `Gate` создавала противоречие в правиле порядка: `Viability` (gate) ДО `Finalize` (rescore) — нормально, а `KillableGate` (gate) ПОСЛЕ `Finalize` + multipliers — тоже нормально. Один тип `Gate` оба случая не выражает. Разделение фиксирует семантику в типе.

**Почему `Score` разделён на 3 поля.** Это контестируемый ресурс: разные стадии читают score в разных «фазах» (baseline, после multipliers, finalized). Если оставить один `AnnotationField::Score`, validator не отловит классический баг «стадия X читает финал, а должна — baseline».

**Initial fields.** На старте pipeline'а (до запуска первой стадии) в `PlanAnnotation` уже населены факты, которые validator должен признать как «доступны без writer-stage»:

```rust
const INITIAL_FIELDS: &[AnnotationField] = &[
    AnnotationField::RawFactors,       // populated by score_plans_with_raw
    AnnotationField::Outcomes,         // populated by sim::apply_step + outcome::builder
    AnnotationField::Plan,              // TurnPlan steps + final_pos
    AnnotationField::SnapshotFacts,     // BattleSnapshot, влияющие карты
    AnnotationField::InitialScoreFacts, // base score из finalize_scores baseline pass
];
```

Validator считает stage's reads допустимыми если они либо в `INITIAL_FIELDS`, либо были `writes` какой-то более ранней стадии в списке.

**Действия:**

- Завести `StageSpec` рядом со списком стадий.
- Заполнить spec для каждой стадии (12 стадий — ≤ день).
- Validator-функция `validate_pipeline(&[StageEntry]) -> Result<(), Error>`:
  - **reads-writes:** для каждой стадии проверить, что все её reads либо в `INITIAL_FIELDS`, либо есть в `writes` какой-то предыдущей стадии в списке.
  - **score_effect ordering** — два инварианта в одном правиле:
    - `Rescore` может идти после `PreScoreGate`, но **не может** идти после `Multiplier` / `Addend` / `Mask` / `PostScoreGate` (иначе пишем `ScoreBase` поверх уже применённых effects — исторический баг 11.0).
    - В production pipeline допустима ровно одна стадия со `score_effect: Rescore` (сейчас `FinalizeStage`).
    - `PostScoreGate` обязан идти после `Rescore` (иначе он гейтит то, что ещё не существует).
    - Это не «Rescore должен быть первым» — `Viability` (Gate) ДО `Finalize` (Rescore) допустимо. Это «Rescore не может перезаписать применённые effects».
- Тест `production_pipeline_order_is_valid` вызывает validator на `PRODUCTION_PIPELINE`.

**Не делать:**

- Compile-time / lint-time проверки. Runtime-валидатор в тесте достаточен.
- Полный AST полей `PlanAnnotation`. Coarse `AnnotationField` enum — норма.

**Definition of done.**

- Validator компилируется и зелёный на production-pipeline.
- Намеренная инверсия двух стадий в тесте (как negative case) — даёт `Err(...)`.
- Spec покрывает все 12 production-стадий.

---

### P3a — `ScoreTrace` internal migration (no schema bump)

**Цель.** Стадии не *мутируют* `ann.score`, а *добавляют typed effects* в trace. `ann.score` становится cached final score (derived).

Внутренний refactor; **JSONL и mining не меняются**. Это behavior-preserving cleanup, разделённый со schema-touch'ем.

**Структура:**

```rust
struct ScoreTrace {
    base: f32,                       // ScoreBase: результат Finalize/Rescore
    rescore_mode: Option<EvaluationMode>,
    multipliers: Vec<MultiplierHit>, // sanity, critics
    addends: Vec<AddendHit>,         // modifiers (summon/trade/repair_bonus)
    masks: Vec<MaskHit>,             // protect_self
    gates: Vec<GateHit>,             // killable_gate (post-score)
}
```

**Канонический порядок применения в `compute()`** — обязателен и фиксируется в коде:

```
1. Если есть Mask с poison → return -∞ (early exit).
2. score = base
3. score *= ∏ multipliers      // sanity penalties + critics multipliers, в порядке push'а
4. score += Σ addends           // modifiers (additive)
5. Если есть Gate с reject → mark plan as gated (не убираем из pool, но pick_best видит флаг).
6. return score
```

Это **не произвольный выбор** — это **сохранение текущей семантики** pipeline'а до-P3a:
- multipliers (sanity → critics) применяются к baseline ДО modifiers — currently это так в порядке Sanity → Critics → ... → PlanModifiers.
- masks применяются раньше других effects — currently `ProtectSelfMaskStage` ставит `-∞` до модификаторов.
- gates маркируют, не зануляют — currently `KillableGateStage` влияет на `pick_best` через флаги.

**Эквивалентность с pre-P3a — обязательное условие P3a.** Если новая алгебра в `compute()` даёт другой результат для тех же effects — это behaviour change, не refactor. Ловится golden replay (см. DoD).

**Бонусы (доступны сразу после P3a, не нужно ждать P3b):**

- Инвариант «multiplier после rescore нелегален» — на `trace.compute()`, а не только на pipeline-валидаторе. Двойная защита.
- Реордер стадий становится механически безопасным: порядок effects в trace не равен порядку их применения; compute разруливает по kind.

**Migration plan.**

Стадии переключаются по одной с `ann.score *= m` на `trace.push_multiplier(MultiplierHit { kind, value })`. `ann.score` остаётся как cached `trace.compute()` результат — **не удаляется** (читатели не меняются, JSONL не двигается).

Порядок миграции (от простого к сложному):

1. Modifiers (3 файла, чисто addend) — proof of concept.
2. Critics (6 файлов, multiplier) — масштаб.
3. Sanity (residual, 3 правила) — тривиально после critics.
4. ProtectSelfMask, KillableGate (mask/gate).
5. Finalize (rescore — самый интересный кейс, переписывает базовый score через `base = rescored_value; multipliers.clear();`).

**Definition of done.**

- Все 6 score-mutation механизмов пишут в `ScoreTrace`.
- `ann.score` остаётся как `trace.compute()` cache. Поле живо, JSONL прежний.
- Существующие тесты зелёные без изменений (черный ящик).
- **Никакого schema bump.** Schema number прежний.
- **Equivalence с pre-P3a:** golden replay corpus diff = 0 для решений (chosen plan + JSONL fields). Score significant figures: float-equivalent в пределах ε = 1e-5; любой больший дрейф — investigate (это сигнал, что `compute()` изменил алгебру).
- **Не «byte-identical».** Если push order или `compute()` меняют floating-point order операций — bit-exact не достижим, но решения должны совпадать. Bit-identical допустимо требовать только если P3a сохраняет точный порядок операций; если нет — ε = 1e-5 plus decision-equivalence.

---

### P3b — Expose `ScoreTrace` to JSONL / mining

**Цель.** Дать external tooling (mining, replay, manual debug) полный breakdown score'а.

**Действия:**

- Добавить в JSONL annotation поле `score_trace: ScoreTraceLog` (mirror runtime trace для serialization).
- Schema bump (вероятно v33).
- `mine_ai_logs` — обновить classes E (modifier contributions) / G (critics coverage) / A (adaptation freq) под новый источник истины. Существующие cross-tabs упрощаются: вместо склейки `ann.sanity + ann.critics + ann.adaptation` руками — единый sweep по `score_trace`.
- `replay_ai_log` — обновить verbose output, чтобы декодировать новый блок.

**Definition of done.**

- Schema v33 published.
- `mine_ai_logs` зелёный на свежем corpus, классы E/G/A читают `score_trace`.
- `replay_ai_log` корректно отображает trace.

**Risk.** Schema bump координация с P7. Если P7 (`LastStand`-сплит, `IntentReason::Adapted`-сплит) идёт в том же сезоне — бампать одной версией (v33 включает оба). Иначе — отдельным релизом v33 → v34.

---

### P4 — Intent split

**Цель.** `intent/mod.rs` (2143 строки) распилен по семантическим concern'ам.

**Структура:**

```
intent/kinds.rs    — TacticalIntent, IntentKind, IntentReason
intent/select.rs   — select_intent + default_focus_target
intent/score.rs    — intent_score + pursuit_move_score + offensive filters
intent/memory.rs   — AiMemory (или объединить с repair/lifecycle.rs)
intent/mod.rs      — re-exports + module docstring
```

`bands.rs`, `agenda.rs`, `considerations.rs` остаются как есть — у них уже хорошая когезия.

**Не делать:**

- Никаких semantic-изменений. Только перенос. `IntentReason::Adapted` остаётся как есть (это P7).
- Никаких типовых изменений. Public API одинаковый.

**Definition of done.**

- `cargo test` зелёный.
- `git diff --stat` показывает преимущественно перемещение, не переписывание.
- **Cohesion criterion:** каждый sub-файл owns одну concern (типы / select / score / memory). Размер — побочный сигнал, не цель: искусственное дробление по 800 строк не лучше монолита, если concerns остаются перемешаны.

---

### P5 — Factor refactor finalization

**Цель.** Один источник правды на factor.

**Решение.** Option 1 — leaf-модули владеют реализацией. `factors/step/saturation.rs`, `factors/plan/self_survival.rs` и т.п. содержат полную логику. Плоские `factors/saturation.rs`, `factors/scarcity.rs`, `factors/survival.rs`, `factors/tempo.rs` удаляются.

Registry-метаданные (`NAME`, `SIGNED`, `compute`) уже полезны — оставляем uniform shape.

**Действия по каждому фактору:**

1. Перенести тело из `factors/<name>.rs` внутрь `factors/step/<name>.rs` или `factors/plan/<name>.rs`.
2. Удалить старый плоский файл.
3. Обновить imports у consumer'ов (`factors/offensive.rs`, `planning/scorer.rs`).
4. Удалить `plan_factor_compute_matches_legacy_self_survival`-тип тесты — параллельной ветки больше нет.

**Behavioural invariant.**

**Это pure relocation refactor.** Все factor outputs должны быть byte/float-equivalent до и после переноса. Никаких «заодно поправлю тут формулу» — формульные изменения отдельным slice'ом, не смешивать.

Проверка: golden replay corpus до и после P5 даёт diff = 0.

**Definition of done.**

- `factors/saturation.rs`, `factors/scarcity.rs`, `factors/survival.rs`, `factors/tempo.rs` не существуют.
- Все факторы — leaf-файлы внутри `step/` или `plan/`.
- Extension Checklist в `docs/ai/extension-checklist.md` обновлён.
- Golden replay corpus diff = 0.

---

### P6 — Replay tooling split

**Цель.** Runtime AI не зависит от replay-executor'а.

**Граница (не «replay = bin only»):**

- `combat/ai/log.rs` — log types. Остаётся в библиотеке (где сейчас).
- `combat/ai/replay_assertion.rs` — assertion DSL. **Остаётся в библиотеке** (нужен тестам AI behavior, например ai_scenarios harness).
- `combat/ai/replay.rs` — executor + парсер CLI args. **Выносится в `src/bin/replay_ai_log.rs`** или в `src/replay/` sub-module бинарника.

**Definition of done.**

- `combat/ai/replay.rs` не существует или содержит только pure helpers без CLI/IO.
- `cargo build --bin replay_ai_log` собирается.

**Scope guard.** Если split разрастается за пределы простого file move + cleanup imports (например, обнаруживается что executor завязан на runtime AI types через Bevy world references) — **остановиться и отложить**. P6 не должен превращаться в pretext для крупного зависимостного refactor'а.

---

### P7 — Semantic cleanup

**Цель.** Закрыть семантические долги, требующие schema-touch.

**Объекты:**

1. **`TacticalIntent::LastStand` → `EvaluationMode`-only.** Выделить в отдельный enum. `intent_score` для LastStand — отдельная функция `evaluate_last_stand`. Затрагивает scorer, logs, mining.
2. **`IntentReason::Adapted` → отдельный лог-блок.** `IntentReason` отвечает «почему `select_intent` выбрал X». Adaptation reason пишется параллельно как `EvaluationModeReason`. В JSONL — два поля вместо одного wrapped.
3. **`unit_value` vs `target_priority` naming.** Переименовать `target_priority` → `target_selection_score` (relative ranking) или `unit_value` → `target_intrinsic_value`. Документировать «нельзя сравнивать напрямую».

**Условие запуска.** Только после P0–P3 стабилизации. Schema bump (вероятно v34) — отдельным релизом, не в одном PR с pipeline-работой.

---

## Relocation overlay (R-track)

Параллельный track «структурная чистка top-level». Сейчас `src/combat/ai/` — alphabet soup: 19 файлов и 12 директорий в одной плоскости, и flow принятия решения из них не читается.

R-track не отдельный сезон. Каждый R-slice пинится к P-slice'у — либо идёт в одном PR, либо отдельным PR сразу после, чтобы не плодить второй параллельный roadmap и не делать import-churn по одним путям дважды.

### Принципы R-track

1. **R-slice ≠ standalone сезон.** Привязан к соответствующему P-slice'у. Не «structural reorg после roadmap'а» — встраивание в существующие фазы.
2. **Pure relocation vs ownership split — разные категории риска.**
   - **Pure relocation:** `git mv` + import path updates. Низкий риск. Пример: `tuning.rs → config/tuning.rs`.
   - **Ownership split:** разнесение god-модуля по concern'ам с переносом частей в разные target-модули. Высокий риск, требует контрактов. Пример: `planning/scorer.rs (2377 LOC) → pipeline/stages/finalize.rs + scoring/factors/aggregate.rs`.
3. **DoD калиброван по риску.**
   - Pure relocation: `cargo test --lib` зелёный + `git diff --stat` доминирует move-строками.
   - Ownership split: golden replay corpus diff = 0 + `cargo test` зелёный + `mining`/`replay` smoke-tests.
4. **Import-churn guard для всех R-slice'ов.** PR description должен явно подтверждать: «No semantic diff: git diff — преимущественно path/import changes». Любое не-import изменение логики в R-PR требует явного объяснения. Это гард против «заодно поправлю формулу пока тут».
5. **Replay boundary неприкосновенна.** Появление `combat/ai/replay/` как dir в библиотеке не должно стать предлогом вернуть CLI executor обратно. Граница: `combat/ai/replay/assertion.rs` (DSL для тестов) — да; executor и CLI parser — `bin/replay_ai_log.rs`.

### Target layout (reference)

Финальное состояние, к которому ведёт R-track. Не план — карта.

```
src/combat/ai/
├── mod.rs
├── system.rs              # бывший enemy_turn.rs
│
├── world/                 # input view (read-only)
│   ├── snapshot.rs
│   ├── influence.rs
│   ├── reservations.rs
│   └── tags/
│
├── config/                # static knowledge & tuning
│   ├── tuning.rs
│   ├── difficulty.rs
│   └── role.rs
│
├── memory/                # between-tick state (R7, optional)
│   ├── ai_memory.rs
│   └── goal/
│
├── appraisal/             # need signals (R7 split internals, optional)
├── intent/                # P4 split: kinds/select/score + bands/agenda/considerations
├── plan/                  # был planning/, очищенный
│   ├── types.rs
│   ├── generator.rs
│   ├── reach.rs
│   ├── sim.rs
│   └── future_value.rs
│
├── outcome/               # как сейчас
├── scoring/               # ВСЕ "facts → numbers"
│   ├── horizon.rs         # был scoring.rs (DPR, horizon helpers)
│   ├── policy/
│   ├── factors/
│   ├── target_priority.rs
│   ├── position_eval.rs
│   └── trade.rs
│
├── adapt/                 # бывший planning/adaptation.rs
├── repair/                # только affinity (goal data в memory/goal)
│
├── pipeline/              # absorbs sanity/critics/modifiers
│   ├── mod.rs / order.rs / spec.rs / ctx.rs / score_trace.rs
│   └── stages/
│       ├── viability.rs / item_scoring.rs / mode_selection.rs / finalize.rs
│       ├── sanity/        # бывший planning/sanity.rs + правила
│       ├── critics/       # бывший top-level critics/
│       ├── modifiers/     # бывший top-level modifiers/
│       ├── protect_self.rs / killable_gate.rs / repair_affinity.rs
│       ├── overlay_considerations.rs
│       └── pick_best.rs   # включает бывший planning/picker.rs
│
├── orchestration/         # бывший utility/ (R-late, опционально)
│
├── log/                   # log + debug + serde_helpers
└── replay/                # assertion DSL только; executor → bin/
```

### R-sub-slice'ы

#### R1 — `world/` + `config/` + `log/` umbrella

**Категория.** Pure relocation.
**Привязка.** Parallel-safe в любой момент. Идеально — сразу после P0, чтобы быстро почистить top-level до того, как туда полезут StageSpec / score_trace новые файлы.

**Действия:**

- `snapshot.rs`, `influence.rs`, `reservations.rs`, `tags/` → `world/`.
- `tuning.rs`, `difficulty.rs`, `role.rs` → `config/`.
- `log.rs`, `debug.rs`, `serde_helpers.rs` → `log/`.

**DoD.** `cargo test --lib` зелёный. Никаких behavioural diffs.

#### R2 — `adapt/` extraction

**Категория.** Pure relocation (data-types) + минимальный rewire.
**Привязка.** Пара с P1 — один PR или сразу после.

**Действия:**

- `planning/adaptation.rs` (1004 LOC: data-types + apply_adaptation + select_evaluation_modes) → `adapt/mod.rs` + `adapt/select.rs`.
- Импорты в `pipeline/stages/mode_selection.rs` и `finalize.rs` обновляются на `adapt::*`.

**DoD.** `cargo test --lib` зелёный. `pipeline/stages/adaptation.rs` (legacy) удалён в P1, остатки не появляются.

#### R3 — `scoring/` partial umbrella (без `factors/`)

**Категория.** Pure relocation.
**Привязка.** Безопасно после P0. **Не ждёт** P5 — `factors/` подъедет позже как R4.

**Действия:**

- `target_priority.rs`, `position_eval.rs`, `trade.rs` → `scoring/`.
- `policy/` → `scoring/policy/`.
- `scoring.rs` (top-level helpers) → `scoring/horizon.rs` (rename отражает реальное содержимое: DPR, damage horizon helpers).

**DoD.** `cargo test --lib` зелёный. Импорты обновлены, нет dead `pub use` в `mod.rs`.

#### R4 — `factors/` → `scoring/factors/`

**Категория.** Pure relocation.
**Привязка.** **С/после P5.** P5 завершает factor-leaf consolidation; R4 переносит уже стабильное состояние внутрь `scoring/`. Делать R4 до P5 = два import-churn'а подряд.

**Действия:**

- `factors/` → `scoring/factors/`.
- Импорты у consumer'ов (`planning/scorer.rs` или его наследников после P3a, `factors/offensive.rs`) обновляются.

**DoD.** Golden replay corpus diff = 0 (P5 это требует, R4 наследует) + `cargo test` зелёный.

#### R5 — `pipeline/stages/` absorbs sanity/critics/modifiers/picker/killable_gate

**Категория.** Mixed: physical move (critics, modifiers) + ownership reorganization (sanity, picker).
**Привязка.** **Только после P3a.** Если переехать раньше — P3a будет менять effect-mechanism в этих же файлах = два churn'а подряд по одним путям.

**Действия:**

- `critics/` → `pipeline/stages/critics/` (pure move).
- `modifiers/` → `pipeline/stages/modifiers/` (pure move).
- `planning/sanity.rs` → `pipeline/stages/sanity/{stage.rs, healer_exposure.rs, retreat_trap.rs, synergy_bonus.rs}` (split).
- `planning/killable_gate.rs` → `pipeline/stages/killable_gate.rs` (pure move).
- `planning/picker.rs::commit_plan, pick_best_plan` → `pipeline/stages/pick_best.rs` (consolidated; merge с уже существующим `pipeline/stages/pick_best.rs`).

**DoD.** Golden replay corpus diff = 0 + `cargo test` зелёный + `mining`/`replay` smoke-tests на свежем corpus.

#### R6 — `planning/` → `plan/` cleanup + `scorer.rs` ownership-split

**Категория.** **Mixed / high-risk ownership split.** Не pure relocation: включает разнесение `planning/scorer.rs` (2377 LOC) на минимум два разных target-модуля. `git mv` — только финальный rename `planning/` → `plan/`; всё, что внутри scorer.rs — отдельный refactor с риском behavioural drift.

**Привязка.** Самый последний из R-slice'ов перед schema bump'ами. Блокеры:

- **P3a** — контракт «что куда мутирует» через `ScoreTrace` зафиксирован.
- **R2** — adaptation уже уехала в `adapt/`, не требует трогать planning/.
- **R5** — sanity / critics / modifiers / picker / killable_gate уехали в `pipeline/stages/`.
- **P5 + R4** — factors финализированы и переехали в `scoring/factors/`. Без этого `scorer.rs::compute_plan_factors_sans_intent` некуда переносить — target `scoring/factors/aggregate.rs` ещё не создан.

То есть R6 идёт **после {P3a ∧ R2 ∧ R5 ∧ P5 ∧ R4}** одновременно. Раньше = либо вслепую, либо с конфликтом.

**Действия:**

- `planning/scorer.rs` ownership-split:
  - Финализация (rescore) → `pipeline/stages/finalize.rs`.
  - Per-step factor compute / pool normalization → `scoring/factors/aggregate.rs` (или `scoring/factors/normalize.rs`).
  - Role-composition + terminal aggregation → `scoring/factors/compose.rs` (если естественно отделяется).
- `planning/` после уезда scorer.rs содержит `types.rs`, `generator.rs`, `reach.rs`, `sim.rs`, `future_value.rs`, `parity_tests.rs` — переименовать → `plan/`.

**DoD.**

- **Golden replay diff = 0** (hard requirement, как для всех ownership-split-моментов).
- `cargo test` зелёный.
- `mining`/`replay` smoke-tests на свежем corpus.
- `git diff --stat` ясно показывает: `planning/` → `plan/` — pure rename; `scorer.rs` ownership-split — diff с разнесением, но **без новых формул** (если новая формула нужна — отдельный slice).

**Rollback rule.** Если golden diff не = 0 и причина не объяснена в PR — revert. Не складывать «follow-up fix» в тот же PR. Это самый рискованный refactor; пусть будет два PR (sometimes split + verification, sometimes split + revert) вместо одного, который оставляет неисследованный дрейф.

#### R7 — `memory/` + `appraisal/` split + tags consolidation (optional)

**Категория.** Mixed; **deferred** до post-P7 или вообще опциональный.

**Действия (если делается):**

- `intent/mod.rs::AiMemory` (last_intent + last_goal) + `repair/goal.rs` + `repair/lifecycle.rs` → `memory/{ai_memory.rs, goal/}`.
- `repair/affinity.rs` остаётся (читает `memory::goal`).
- `appraisal/mod.rs` (1084 LOC) → `appraisal/{mod.rs (NeedSignals + entry), self_preserve.rs, rescue_ally.rs, apply_cc.rs, …}`.
- **Tags consolidation:** `AiTags` (bitflags) сейчас живёт в `world/snapshot.rs`, а `AbilityTag` / `StatusTag` — в `world/tags/`. Семантически три типа тегов тяготеют к одному модулю. Действие: переехать `AiTags` в `world/tags/ai_tags.rs`, чтобы `world/tags/` стал single source of truth для всех tag-семантик. Обнаружено в R1 при переносе `snapshot.rs → world/snapshot.rs` (см. лог R1 «обнаружено, отложено»).

**DoD.** Golden replay diff = 0 + cohesion criterion (один concern на файл) + `cargo test` зелёный.

**Когда не делать.** Если AiMemory и goal lifecycle стабильны, никаких grow-pains нет — этот slice опускается. R7 — желаемое-но-не-обязательное.

**Можно ли split'нуть tags consolidation отдельно?** Да — это самая маленькая часть R7 (один тип, ограниченный набор consumer'ов). Если приоритет tags consolidation вырастает раньше, чем у `memory/` или `appraisal/` split'ов — её можно вынести в отдельный мини-slice (условно «R7.tags»). Минимальный pinning: только safe после R1 (перемещение `world/`-зонтика, уже сделано). DoD: `cargo test --lib` + import-churn guard. Это допустимый исход — не обязательно ждать целого R7.

### R-late (низкий приоритет)

- `utility/` → `orchestration/`. Чистая косметика. Не пинить к P0 — там и так много происходит. Делать когда удобно или не делать.
- `enemy_turn.rs` → `system.rs`. То же.

## Cross-cutting

### Rollback rule для ownership-heavy slice'ов

Применяется к: P3a, P5, R5, R6 (всё, что трогает runtime stage/scoring/factor logic). Не применяется к: pure relocation (R1, R2, R3, R4) и P-only structural slice'ам (P0, P1, P2, P4, P6).

**Правило.** Если golden replay diff ≠ 0 (или ε > 1e-5 по решению, не score) **и причина не объяснена в PR description** — revert или split. **Не складывать follow-up fix в тот же PR.** Investigate-explain-or-revert; не «debug-as-you-go».

Зачем. Эти slice'ы по природе содержат поведенческий риск. Если drift возник — это сигнал, что есть скрытая семантика, которую refactor не сохранил. Засовывание fix'а в тот же PR смешивает «relocation» и «behavioural change», и через год blame не различит, что случилось.

### Post-Slice sweep

После каждого Slice — обязательный sweep «что должно было умереть».

**Чек-лист (5–15 минут):**

1. Имена legacy API, которые этот Slice заменил — `git grep` показывает только own-tests / docs.
2. Файлы, которые этот Slice заменил — удалены, не оставлены «на всякий случай».
3. Комментарии вида `// step N replaced this with M` — либо удалены (если ясно из истории git), либо превращены в проверяемый код (например, `#[deprecated]` с ссылкой на замену).
4. Доки в `docs/ai/*` обновлены — не описывают удалённое как актуальное.

**Зачем.** Три из шести наших исходных проблем (factor leaf-флэт duality, AdaptationStage legacy, scoring.rs остатки) — **один и тот же мета-баг**: «добавили новое, не удалили старое». Roadmap решает три случая поштучно. Sweep предотвращает четвёртый.

## Что не делать в этом roadmap'е

Эксплицитный negative scope — чтобы не расширять при коммите:

- **Не делать сразу compile-time dependency framework.** Runtime-валидатор в тесте достаточен.
- **Не сливать физически Sanity и Critics** в один модуль. Общий interface (через `ScoreTrace`) — да; общая директория — нет. Они переезжают **рядом** под `pipeline/stages/` (R5), не **в один файл**.
- **Не выносить LastStand из TacticalIntent в P0–P5.** Это P7, schema-touch.
- **Не переписывать одновременно** `scoring.rs` / `policy/` / `factors/`. Granularity — один Slice = один тематический рефактор.
- **Не трогать JSONL migration в Slice'ах с pipeline refactor.** Schema bumps — отдельным релизом.
- **Не делать R-track как самостоятельный сезон после P-track'а.** Это была первая итерация плана (под именем «P8»). Заменена на R-overlay по причине: standalone structural reorg создаёт второй параллельный roadmap, конфликты по одним файлам с активными P-slice'ами и теряет связь между relocation'ом и контрактами. R-slice'ы привязываются к P-slice'ам, не складываются в хвост.
- **Не пытаться двигать `planning/scorer.rs` (R6 ownership-split) до завершения P3a.** Контракты «что куда мутирует» должны быть зафиксированы в типах (`ScoreTrace`) ДО переноса логики между модулями. Иначе перенос сделан вслепую и golden replay не выполнит роль guard'а.

## Status tracking

P-track (контракты + миграция логики) и R-track (relocation) в одной таблице, с указанием pairing.

| Slice | Track | Status | Pinned to / Blocked by | Notes |
|---|---|---|---|---|
| P0 — Single pipeline | P | done | — | 2026-05-01. `order.rs` + split PRE/POST_MASK runner. |
| R1 — world/config/log umbrella | R | done | parallel-safe (после P0) | 2026-05-01. Pure relocation. 3 коммита. |
| P1 — Remove legacy AdaptationStage | P | done | P0 | 2026-05-01. +2 parity tests in mode_selection; 4 legacy deleted. |
| R2 — adapt/ extraction | R | done | пара с P1 | 2026-05-01. planning/adaptation.rs → adapt/{mod,select}.rs. |
| P2 — StageSpec + validator | P | done | P0 | 2026-05-01. spec.rs + validate_pipeline + 7 tests. |
| R3 — scoring/ partial umbrella | R | done | после P0; не ждёт P5 | 2026-05-01. Pure relocation; без `factors/`. |
| P3a — ScoreTrace internal migration | P | done | P2 | 2026-05-01. All 7 sub-steps complete. Trace accumulates through pipeline; behavioural diff = 0; no schema bump. |
| ↳ P3a.0 — ScoreTrace types + compute() | P | done | P2 | 2026-05-01. types-only, behavioural diff = 0. |
| ↳ P3a.1 — Modifiers → trace.addends | P | done | P3a.0 | 2026-05-01. PlanModifiersStage emits AddendHits. Bridging fixed in P3a.2. debug_assert invariant. |
| ↳ P3a.2 — Critics → trace.multipliers | P | done | P3a.1 | 2026-05-01. CriticsStage emits MultiplierHits. Retroactive bridging fix in P3a.1 (full reset). |
| ↳ P3a.3 — Sanity → trace.multipliers | P | done | P3a.2 | 2026-05-01. SanityStage emits MultiplierHits with kind=Sanity. 4 new tests. |
| ↳ P3a.4 — Mask + Gate stages | P | done | P3a.3 | 2026-05-01. ProtectSelf→MaskHit Poison; KillableGate→GateHit+MaskHit double-emit. 6 new tests. |
| ↳ P3a.5 — Finalize → Rescore | P | done | P3a.4 | 2026-05-01. FinalizeStage sets trace.base=new_score, rescore_mode=Some(mode), clears effects. 4 new tests. |
| ↳ P3a.6 — cleanup bridging-resets | P | done | P3a.5 | 2026-05-01. Bridging-резеты удалены из 5 стадий. Trace аккумулирует от Finalize::base. 1 new full-pipeline invariant test. 780 passed. |
| R5 — pipeline/stages absorbs | R | done | **только после P3a** | Mixed (move + split). A: done 2026-05-01 (critics/ + modifiers/ relocated). B.1: done 2026-05-01 (sanity split). B.2: done 2026-05-01 (killable_gate merge). B.3: done 2026-05-01 (picker merge). R5 complete. |
| P5 — Factor refactor | P | done | **НЕ параллелить с P3a** | 2026-05-01. Leaf consolidation. −4 flat files, −2 parity tests. 780→778. |
| R4 — factors → scoring/factors/ | R | done | с/после P5 | 2026-05-01. Pure relocation. scoring/ umbrella теперь полное. |
| P3b — Expose ScoreTrace to JSONL | P | pending | P3a; schema bump | |
| R6 — planning/ → plan/ cleanup + scorer.rs split | R | pending | **после P3a + R2 + R5 + P5 + R4** | Mixed / high-risk ownership split. Hard requirement: golden replay diff = 0. |
| P4 — Intent split | P | done | parallel-safe от P0 | 2026-05-01. kinds/select/score/memory. mod.rs → 32 LOC. |
| P6 — Replay split | P | pending | parallel-safe | См. boundary в R-track principles. |
| P7 — Semantic cleanup | P | pending | P3b (schema coordination) | |
| R7 — memory/ + appraisal split | R | pending | optional, post-P7 | Может вообще не делаться. |
| R-late — utility/→orchestration/, enemy_turn→system | R | pending | косметика | Когда удобно или никогда. |

**Mainline P-order.** `P0 → P1 → P2 → P3a → P3b`. Критический путь, без обхода.

**Pairing:**

- **P1 + R2** — естественная пара (убрать adaptation legacy + вытащить data-types из planning).
- **P3a → R5** — обязательная последовательность (P3a меняет effects в файлах, которые R5 двигает).
- **P5 → R4** — обязательная последовательность (P5 финализирует factors, R4 переносит финализированное).
- **P0 → R1, P0 → R3** — R-slice'ы могут идти параллельно P-цепочке (pure relocation, не конфликтуют).

**Parallelism rules:**

- **R1 (world/config/log)** parallel-safe от P0 включительно — pure path moves.
- **R3 (scoring/ partial)** parallel-safe от P0 включительно — без `factors/`, не конфликтует с P5.
- **P4 (Intent split)** parallel-safe от P0 — не касается pipeline / scoring.
- **P6 (Replay split)** parallel-safe — касается только `replay.rs` / `bin`.
- **R7** опциональный, ждёт post-P7.
- **R-late** не приоритизируется.

**Coordination notes:**

- P7 ждёт P3b (schema bump). Если P7 готов раньше — можно объединить в один schema release (v33 включает оба); если позже — отдельный bump v34.
- R6 `scorer.rs` ownership-split — единственный action в R-track'е, который требует golden replay diff = 0 как hard DoD. Все остальные R-slice'ы — `cargo test --lib` достаточно.

## Owner map (после реструктуризации)

После того как R-track довезён хотя бы до R5, контрибьютор не должен гадать «куда положить новое». Эта таблица — single source of truth для разместительной логики.

| Что добавляешь | Куда кладёшь | Почему |
|---|---|---|
| Новый factor | `scoring/factors/step/` или `scoring/factors/plan/` или `scoring/factors/terminal/` | Один factor = один leaf-файл с `NAME`, `SIGNED`, `compute`. |
| Новый pipeline stage | `pipeline/stages/<name>.rs` + регистрация в `pipeline/order.rs::PRODUCTION_PIPELINE` + `StageSpec` в `pipeline/spec.rs` | Stage реализует через `apply_<name>` fn-pointer + spec, не trait object. |
| Новый critic | `pipeline/stages/critics/<name>.rs` + регистрация в `CriticsStage::first_wave` | Multi-instance stage (несколько критиков под одной stage обёрткой). |
| Новый score modifier | `pipeline/stages/modifiers/<name>.rs` | Additive plan-level бонус. |
| Новое sanity-правило | `pipeline/stages/sanity/<name>.rs` | Multiplicative penalty. Если правило ложится на critic-семантику — лучше сделать critic, не sanity. |
| Новый need signal | `appraisal/<name>.rs` (после R7) или функция в `appraisal/mod.rs` (до R7) | Поле в `NeedSignals` + producer-функция. |
| Новый outcome fact | `outcome/mod.rs` (поле в `ActionOutcomeEstimate`) + `outcome/builder.rs` (populate) | Только raw facts, без value judgement. |
| Новая HP-equivalent value function | `scoring/policy/<name>.rs` или существующий `policy::*` модуль | Pure function `fn(facts, context) -> f32`. |
| Новый input в snapshot | `world/snapshot.rs` или `world/influence.rs` | Read-only world view. |
| Новый `AiTag` flag (bitflag) | `world/snapshot.rs` (текущее) → после R7.tags `world/tags/ai_tags.rs` | До tags consolidation `AiTags` живёт рядом с `UnitSnapshot`. После R7.tags — все три tag-типа в `world/tags/`. |
| Новый `AbilityTag` / `StatusTag` (семантический тэг) | `world/tags/classify.rs` + `world/tags/cache.rs` | Single source of truth — classify.rs. |
| Новая константа тюнинга | `config/tuning.rs` (`Thresholds` / `Tables` / `Difficulty`) + `assets/data/ai_tuning.toml` | Data-driven, не const в коде. |
| Новый difficulty knob | `config/difficulty.rs` + lerp endpoints в `tuning.toml` | Data-driven. |
| Новый AdaptationReason | `adapt/mod.rs` (variant) + `adapt/select.rs` (триггер) | Не в planning, не в pipeline/stages. |
| Новый goal kind | `memory/goal/context.rs` (после R7) или `repair/goal.rs` (до R7) | Memory concern. |
| Новый replay assertion | `replay/assertion.rs` (DSL остаётся в lib) | Executor — `bin/replay_ai_log.rs`, не библиотека. |
| Новый mining metric | `src/bin/mine_ai_logs.rs` | Tooling, не runtime. |

Если ни одна строка не подходит — это сигнал, что концепт не вписывается в текущие слои, и нужен design-обсуждение, а не «положу куда-нибудь».
