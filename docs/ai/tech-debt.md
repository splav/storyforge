# AI Tech Debt — Followup Roadmap

Проблемы организации и логики, обнаруженные в процессе пост-restructure-аудита (см. git history `ai/P0..ai/R-late`). Не требуют немедленного исправления — реструктуризация дала чистый layout, и эти пункты можно закрывать инкрементально, отдельными PR.

Принцип: каждая запись описывает **что не так**, **почему это болит**, и **как закрыть**. Каждый пункт несёт meta-строку **`Effort / Risk / Priority / Blocked by`**, чтобы быстро понять, можно ли брать сейчас.

---

## TL;DR — гибридный план

Половина пунктов roadmap'a (A2, B1, B2, B4, E1, плюс симптомы вроде `f32_finite`, `debug_assert_eq`, `score_initial`) имеет **общий архитектурный корень** — pipeline построен на shared mutable state с runtime-validated контрактами. Но atomic refactor «закрыть всё одним умным slice'ом» опасен: если decision drift появится, источник (drive-loop / trace algebra / KillableGate semantics) не локализовать.

Поэтому план — **узкий behavior-preserving R8-lite** + отдельные slice'ы для semantic changes. Каждая фаза имеет понятный DoD и не смешивает архитектуру с поведением.

```
Phase 0 ✅ ── Quick wins (≤ полдня каждый, no risk) ──────────────────────
       A1  rename finalize_scores                                   done
       A3  relocate apply_protect_self_mask + expected_aoo_damage   done
       C4  canonical AiTags path sweep                              done
       C1  remove ScoringCtx Copy                                   done

Phase 1 ✅ ── Verification foundation ────────────────────────────────────
       C2  capture fresh v34 baseline corpus + replay compare CLI   done
       E1  verify current ScoreTrace algebra against golden replay  done
       (no harness dependency — corpus + CLI достаточно)

Phase T ✅ ── TestStageHarness (independent, но до R8-lite) ──────────────
       B4+D1  harness с trace.base init и общим setup            done
       (ортогонален Phase 1; делать до R8-lite, потому что R8-lite потребует
        миграции stage tests — дешевле, если harness уже есть)

Phase 2 ── R8-lite / Score Effect Engine ─────────────────────────────────
       Behavior-preserving архитектурный refactor:
       - стадии score-effect family возвращают StageEffect values
       - drive-loop — sole writer score_trace + legacy observability
       - B1-soft: приватизация ТОЛЬКО score/effect-owned полей
       - B2 writer-dedup (но НЕ schema cleanup)
       DoD: golden replay diff = 0, no schema bump.

Phase 3 ── A2 KillableGate semantic fix ──────────────────────────────────
       true PostScoreGate (без NEG_INFINITY)
       PickBest получает SelectionKey { rejected, masked, score }
       Это behavioral PR, golden diff ≠ 0 ожидается, decisions review вручную.

Phase 4 ── B2 schema observability cleanup ───────────────────────────────
       4a: mining/replay читают только score_trace_log
       4b: schema bump v35, удаление legacy fields из JSONL

Phase 5 ── Opportunistic ─────────────────────────────────────────────────
       B3  cross-stage helpers doc
       C3  pub(crate) use cleanup
       C5  AiTags split (state vs capabilities) — реактивно
       B1-rest (приватизация остальных annotation полей, если ещё болит)

Skip  ── D2 (preventive process — рефлекс, не задача)
```

### Жёсткие границы Phase 2 (R8-lite)

Самая важная часть плана — что R8-lite **не имеет права делать**:

```
R8-lite is behavior-preserving.
It may:
  - centralize writes through drive-loop
  - privatize score/effect-owned fields (score, score_trace, modifiers,
    sanity, critics, contract) and gate them through pub(crate) helpers
  - introduce StageEffect return API for score-effect family stages

It must NOT:
  - change KillableGate semantics (Mask-like emission stays for now)
  - remove or change JSONL legacy observability fields
  - change PickBest ranking logic
  - apply effect-trait abstraction to non-score-effect stages
    (ModeSelection / Finalize / ItemScoring / OverlayConsiderations /
     PickBest / RepairAffinity / Viability stay as-is)
```

DoD: `replay_ai_log --compare-golden baseline.jsonl` returns diff = 0.

### Зависимости фаз

```
Phase 0 → Phase 1 → Phase 2 → Phase 3 → Phase 4
              ↓         ↑
            Phase T ────┘  (рекомендован до Phase 2)

Phase 5 — opportunistic, в любое время
```

Phase 1 (C2+E1) — обязательная пред-условие любых архитектурных изменений: без replay guard любой drift невидим. TestStageHarness (Phase T) ортогонален Phase 1 и может идти параллельно, но до Phase 2.

### Что закрывает каждая фаза

| Пункт | Закрывается |
|---|---|
| A1, A3, C1, C4 | ✅ Phase 0 (a14fb2f) |
| C2, E1 | ✅ Phase 1 |
| B4, D1 | ✅ Phase T (TestStageHarness) |
| B1-soft (только score/effect fields) | Phase 2 |
| B2 writer-dedup | Phase 2 |
| `f32_finite` workaround, `debug_assert_eq` invariants, `score_initial` в annotation, pre_scores buffer dance | Phase 2 (собирается централизованно) |
| A2 + SelectionKey | Phase 3 |
| B2 schema cleanup | Phase 4 |
| B3, C3, C5, B1-rest | Phase 5 (opportunistic) |
| D2 | skip (preventive process) |

---

## A. Семантические рассогласования (имена ↔ смысл)

### A1. `finalize_scores` — имя лжёт

**Status:** ✅ closed (`ai/Phase0`, a14fb2f).
**Effort:** ≤ полдня · **Risk:** низкий · **Phase:** 0 · **Blocked by:** —

**Где:** `src/combat/ai/scoring/factors/aggregate.rs::finalize_scores`.

**Проблема.** Функция называется «finalize», но используется тремя разными путями:
- Initial scoring: `score_plans_with_raw → finalize_scores`.
- Rescore: `rescore_with_per_plan_modes → finalize_scores`.
- Future value lookahead: `plan/future_value.rs::finalize_scores`.

Это **primitive «свернуть raw factors в score»**, а не «финализация» в смысле `FinalizeStage`. Имя пересекается с `FinalizeStage` и сбивает при чтении pipeline кода.

**Закрытие.** Переименовать в `aggregate_factors_to_score` (или `combine_raw_factors`). Pure rename: ~30 callsites/комментариев. PR ≤ 100 LOC.

---

### A3. `apply_protect_self_mask` и `expected_aoo_damage` — wrong neighbourhood

**Status:** ✅ closed (`ai/Phase0`, a14fb2f). `apply_protect_self_mask` → `pipeline/stages/protect_self.rs` (now `pub(super)`); `expected_aoo_damage` → `scoring/horizon.rs` (sole consumer of damage helpers).
**Effort:** ≤ полдня · **Risk:** низкий · **Phase:** 0 · **Blocked by:** —

**Где:** `src/combat/ai/pipeline/stages/sanity/mod.rs`.

**Проблема.**
- `apply_protect_self_mask` — единственный consumer этой функции — `pipeline/stages/protect_self.rs` (плюс регистрация в `pipeline/order.rs`).
- `expected_aoo_damage` — общий snapshot-fact, используется в `adapt/select.rs`, `scoring/trade.rs`, `pipeline/stages/critics/overcommit_into_danger.rs` — не специфичен для sanity.

**Закрытие.** Pure relocation:
- `apply_protect_self_mask` → консолидировать в `pipeline/stages/protect_self.rs`.
- `expected_aoo_damage` → вынести в `world/snapshot.rs` или `scoring/horizon.rs` (где живут другие damage-helpers).

Scope ~ 50–150 LOC.

---

### A2. `KillableGate` — design ↔ implementation mismatch

**Effort:** 200–400 LOC · **Risk:** средний (поведенческий drift) · **Phase:** 3 · **Blocked by:** Phase 1 (verification), Phase 2 (R8-lite даёт architecture, на которой A2 строится).

> **Важно:** A2 — это behavioral change (Mask-like → true Gate). Делается **отдельным slice'ом после R8-lite**, не внутри. Иначе drift между «новой архитектурой» и «новой semantics» неотличим.

**Где:** `src/combat/ai/pipeline/stages/killable_gate/mod.rs`, `src/combat/ai/pipeline/spec.rs::STAGE_SPECS` (строки 209-219).

**Проблема.** В `STAGE_SPECS` стадия маркирована как `ScoreEffect::PostScoreGate` (семантика «помечает план через флаг, не зануляет score»). Фактический код ставит `scores[i] = f32::NEG_INFINITY` — это `Mask`. P3a.4 обходит это эмиссией double hit (`GateHit { Reject } + MaskHit { Poison }`) для сохранения `compute()` invariant'а — см. строки 225-235.

**Почему болит.** Двойная эмиссия — workaround. Реальный Gate-flow (план остаётся в pool, PickBest читает `is_gated()` и понижает приоритет на ранге, а не зануляет score) не реализован. Spec лжёт о семантике стадии — любой новый PostScoreGate стейдж, написанный «по образцу», унаследует эту же ложь.

**Закрытие (Phase 3, после R8-lite):**
1. KillableGateStage пушит только `GateHit { Reject }` (не Mask).
2. `ann.score` НЕ ставится в NEG_INFINITY — план остаётся со своим score.
3. **PickBest получает `SelectionKey { rejected: bool, masked: bool, score: f32 }`** — rejected/masked становятся first-class в ranking, а не «надо помнить проверить». Это требование критично: без него можно случайно начать выбирать gated/masked планы по finite score.
4. Behavioural diff ≠ 0 ожидается; нужен ручной review changed decisions через golden replay.

Риск drift'а реальный: на gated плане сейчас все downstream stages видят `-∞` и пропускают; после изменения они будут запускать модификаторы и могут сместить итоговый ranking. Поэтому A2 идёт строго после R8-lite (architecture стабильна) и Phase 1 (corpus есть).

**Связанные симптомы (collateral, исчезают вместе с A2):**
- `f32_finite` serde adapter в `outcome/mod.rs:268` существует **только потому что** stages пишут `NEG_INFINITY` в `ann.score`. JSON не умеет non-finite floats; adapter маппит `NEG_INFINITY → f32::MIN` на write. Когда mask/gate семантика хранится в `score_trace.masks/gates` и `SelectionKey`, `ann.score` финитен и adapter не нужен.
- `debug_assert_eq!(ann.score_trace.compute(), f32::NEG_INFINITY)` в `killable_gate/mod.rs:235` — ad-hoc проверка инварианта double-emit. В release-сборке проверка молчит. После R8-lite (Phase 2) инвариант становится type-enforced (drive-loop не может выпустить inconsistent trace), а сам double-emit исчезает в Phase 3.

---

## B. Архитектурные слабости (типовая дисциплина)

### B4. Test-vs-production divergence по trace.base

**Status:** ✅ closed (`ai/PhaseT`). `StageTestHarness` + `PoolBuilder` в `src/combat/ai/test_helpers.rs`; 22 stage-test файла мигрированы на 5-секционный template. `PoolBuilder::trace_base_eq_score()` — explicit fix B4. Critics с зависимостью от `content.abilities` (`BuffIntoVoid`, `HealWithoutRescueValue`, `RareResourceForLowImpact`) сохранили inline setup (harness вызывает `empty_content()` — несовместимо), но в 5-секционном формате.
**Effort:** 1 день (~200 LOC harness + миграция ~30 тестов) · **Risk:** низкий · **Phase:** T (TestStageHarness, independent) · **Blocked by:** —

> Phase T ортогонален Phase 1, но рекомендован **до Phase 2 (R8-lite)** — иначе миграция stage tests внутри R8-lite будет дороже.

**Проблема.** В production `FinalizeStage` устанавливает `score_trace.base = new_score`. Все downstream стадии полагаются на это. В тестах же стадия запускается в изоляции: тест должен **вручную** инициализировать `trace.base = entry_score` перед apply (см., например, `pipeline/stages/finalize.rs:319-323` — тест injects `ScoreTrace { base: 999.0, ..Default::default() }`). Если забыть — `compute()` invariant ломается, тест проходит ложно.

**Почему сейчас:** каждый новый stage-test платит этот налог. Дешевле создать harness один раз.

**Закрытие.** Test harness:
```rust
TestStageHarness::new(actor)
    .with_intent(TacticalIntent::Reposition)
    .with_initial_scores(&[1.0, 0.5])  // также инициализирует trace.base
    .run(SanityStage)
```
Setup тратит 1 строку на test вместо 6-7. Покрывает D1 (boilerplate) и B4 (trace.base init) одним harness'ом.

---

### B2. Параллельная observability state

**Phase:** разделено на B2-writer-dedup (Phase 2 / R8-lite) и B2-schema-cleanup (Phase 4).

- **B2-writer-dedup** — drive-loop становится sole writer обоих каналов одновременно. Drift невозможен по конструкции, потому что обновление trace и legacy field — атомарная операция в одной точке. Effort: входит в R8-lite, отдельной стоимости нет. **Blocked by:** —.
- **B2-schema-cleanup** — удаление legacy fields из JSONL. Effort: 1-2 дня + миграция mining cuts. **Blocked by:** Phase 1 (corpus), Phase 2 (writer-dedup), и стабилизация mining на `score_trace_log` как source of truth.

**Где:** `PlanAnnotation` имеет одновременно:
- `score_trace_log: Option<ScoreTraceLog>` (P3b — typed effect log).
- `modifiers: Vec<ModifierContribution>`, `sanity: Vec<SanityHit>`, `critics: Vec<CriticHit>`, `adaptation: Option<AdaptationData>`, `contract: Option<ContractMaskHit>` (legacy per-effect channels).

**Проблема.** Они кодируют **одну и ту же информацию** на разных уровнях. После P3b каждая стадия пишет в оба канала. Каналы могут drift'ить (pre-existing observability ловит то, что score_trace не ловит, и наоборот). Mining/replay tooling обращается к разным источникам.

**Почему болит.** Дубликат state требует синхронизации. Любой новый score-effect нужно writeable и в trace, и в legacy field. Drift — class бесшумных багов. Каждый refactor pipeline платит налог двойной поддержки.

**Закрытие — два шага:**

**Phase 2 (R8-lite, writer-dedup):**
- Drive-loop пишет `score_trace` и legacy fields одновременно из одного источника (`StageEffect`).
- Стадии больше не могут писать в каналы напрямую — закрыто visibility.
- Drift между каналами становится невозможен по конструкции, без удаления legacy.

**Phase 4 (schema cleanup):**
- 4a: mining/replay переключаются на `score_trace_log` как source of truth, legacy fields всё ещё сериализуются для backward-compat.
- 4b: schema bump v35, legacy fields удаляются из JSONL. Требует свежего corpus (Phase 1) и стабилизированного mining.

Зачем разделять: удалять legacy из JSONL до того, как mining реально считывает trace_log, — рискованно (потеря diagnostics при неполном покрытии). Phase 4a даёт консистентность через dual-write + read-from-trace, и только потом 4b убирает duplicate.

---

### B1. `PlanAnnotation` — flat bag из ~30 полей

**Phase:** разделено на B1-soft (часть Phase 2 / R8-lite, ограниченный scope) и B1-rest (Phase 5, opportunistic).

- **B1-soft внутри R8-lite:** приватизация **только score/effect-owned** полей (`score`, `score_trace`, `modifiers`, `sanity`, `critics`, `contract`) через `pub(crate)` + writer API только для drive-loop. Это естественное следствие writer-dedup в Phase 2.
- **B1-rest:** остальные ~24 поля (`outcomes`, `terminal`, `repair_affinity`, `viability`, `pick`, `effective_ai_tags`, `agenda_item`, `considerations_per_item`, `reject_reasons_per_item`, `score_initial`, `per_item`, …). Если после R8-lite ещё болит — Phase 5, opportunistic, по мере касания. Hard-вариант с per-stage trait'ами не оправдан без явных регрессий.

**Где:** `src/combat/ai/outcome/mod.rs::PlanAnnotation` (строки 224-358).

**Проблема.** Каждая стадия пишет свой slice (sanity, critics, modifiers, contract, adaptation, score_trace, agenda_item, considerations_per_item, ...). Структура открыта для записи всем; типовой системы «стадия X может писать только Y» нет. `StageSpec` validator в `pipeline/spec.rs` ловит эти контракты в runtime-тесте, но структура остаётся незащищённой.

**Почему болит.** Slow-burn: регрессии «стадия по ошибке записывает не своё поле» поймаются только тестом, и только если конкретно этот тест есть. Refactor приводит к утечкам ответственности.

**Закрытие B1-soft (Phase 2):** `pub(crate)` на score/effect-owned поля + методы на `PlanAnnotation`:
```rust
impl PlanAnnotation {
    pub(crate) fn apply_stage_effects(&mut self, effects: &[StageEffect]) { ... }
    pub(crate) fn set_score_base(&mut self, base: f32) { ... }
    pub(crate) fn final_score(&self) -> f32 { self.score }
}
```
Стадии не имеют write-доступа к score/effect полям, только читают через геттеры или передают `&PlanAnnotation` в `compute()`. Drive-loop — единственный, кто вызывает `apply_stage_effects`.

**Закрытие B1-rest (Phase 5, opportunistic):** остальные поля приватизируются по мере касания, при следующих рефакторингах layer'ов (terminal, repair, agenda).

**Связанные симптомы (тот же flat-bag паттерн):**
- `score_initial: f32` (`outcome/mod.rs:309`) — `#[serde(skip)]`, intermediate variable для PickBest composition. Логически принадлежит локальному scope PickBest, не annotation. После Phase 2 уезжает из `PlanAnnotation` (drive-loop хранит локально или PickBest читает из `score_trace.base`).
- `KillableGateStage::apply` (`pipeline/stages/killable_gate/mod.rs:181-238`) — паттерн «сделать `pre_scores: Vec<f32>` копию, скопировать в `scores`, мутировать, write back в annotations + копировать в contract field». Stage API заставляет это делать руками, потому что нет typed effect-возврата. Phase 2 убирает копирование (стадия возвращает `Hit`, drive-loop применяет атомарно).

---

### B3. Cross-stage helpers в factor leaves

**Effort:** ~1 час (doc-only) · **Risk:** ниже нуля · **Phase:** 5 (opportunistic) · **Blocked by:** —

**Где:** `compute_plan_self_survival`, `compute_plan_tempo_gain` остаются `pub` после P5.

**Проблема (мягче чем казалось при первом аудите).** Из реального usage:
- `compute_plan_tempo_gain` — единственный live cross-stage call: `pipeline/stages/item_scoring.rs:165`.
- `compute_plan_self_survival` — вне своего leaf'а используется фактически только в тестах.

Так что pain в основном про **первого**: factor leaf экспортирует свой `compute_*` для использования другой стадией.

**Почему болит.** Идея factor leaves: «один factor = один self-contained файл». Реальность: некоторые factors имеют дополнительные cross-stage entry points для предварительной оценки. `pub` визибельность это не нарушает, но размывает model.

**Закрытие.** Документировать в `scoring/factors/mod.rs` отдельный «pre-step factor evaluation API» как явный интерфейс. Trivial cleanup при следующем касании этого слоя.

---

## C. Инфраструктурные пробелы

### C4. `AiTags` re-export через `world/snapshot.rs`

**Status:** ✅ closed (`ai/Phase0`, a14fb2f). 20 consumer'ов мигрировали на `world::tags::AiTags`, re-export удалён.
**Effort:** ~1 час · **Risk:** нулевой · **Phase:** 0 · **Blocked by:** —

**Где:** R7-3 переместил `AiTags` в `world/tags/ai_tags.rs`, но оставил `pub use crate::combat::ai::world::tags::AiTags;` в `snapshot.rs:21` для backward-compat.

**Проблема.** Re-export — мост; consumers продолжают писать `world::snapshot::AiTags` (10+ файлов: `orchestration/fallback.rs`, `intent/select.rs`, `intent/score.rs`, `world/influence.rs`, `pipeline/stages/sanity/healer_exposure.rs`, `log/debug.rs`, `test_helpers.rs`, etc). Canonical path размывается.

**Закрытие.** Sweep consumers на canonical path `world::tags::AiTags`, удалить re-export. Trivial — `sed` + `cargo check`.

---

### C1. `ScoringCtx` стал `Copy` ради тестов

**Status:** ✅ closed (`ai/Phase0`, a14fb2f). Удаление оказалось zero-impact — все ~40 callsite'ов уже передают `&ScoringCtx`; `Copy` в R6 был добавлен превентивно и нигде не работал.
**Effort:** ~30 callsites, полдня · **Risk:** низкий · **Phase:** 0 · **Blocked by:** —

**Где:** `src/combat/ai/orchestration/mod.rs:165` — `#[derive(Clone, Copy)]` на `ScoringCtx`.

**Проблема.** В R6 добавлен `Copy` чтобы aggregate-тесты могли передавать ctx нескольким функциям. ScoringCtx — context struct из ссылок (`&AiWorld`, `&BattleSnapshot`, etc). Copy «безопасно» в том смысле что копирование ссылок — no-op. Но Copy на context-type приглашает к неявному копированию там, где должна быть передача `&ScoringCtx`.

**Почему болит.** Code smell. Lifetime-баги легче возникают и сложнее отлавливаются.

**Закрытие.** Удалить `Copy`, сохранить `Clone`. Тесты обновить на explicit `ctx.clone()` или передачу `&ctx`. Scope ~ 30 callsites.

---

### C2. Schema migration tooling отсутствует

**Status:** ✅ closed (`ai/Phase1`). Принято решение про continuous re-capture (минимальный вариант). Baseline зафиксирован в `tests/baselines/baseline_v34.jsonl` (114 records из `tests/ai_scenarios/snapshots/*/log.jsonl`); CI smoke в `tests/golden_smoke.rs`; recapture-чеклист в [extension-checklist.md § SCHEMA_VERSION bump](extension-checklist.md). Полный migration tool — не нужен (см. ниже).
**Effort:** continuous re-capture — полдня; full migration tool — несколько дней · **Risk:** низкий (continuous вариант) · **Phase:** 1 · **Blocked by:** —

**Проблема.** Текущая `SCHEMA_VERSION = 34` (`log/mod.rs:182`); `parse_actor_tick` hard-reject'ит corpus с `schema_version < SCHEMA_VERSION - 1` = 33 (`log/mod.rs:1226`). За реструктуризацию schema выросла на ≥12 bump'ов. Каждый bump делает старый corpus мёртвым — миграции нет.

**Почему болит.** На самых рискованных slice'ах (P3a.6, R6, P5+R4) `golden replay diff = 0` был **обязательным DoD**, но фактически недоступным — corpus оставался стар, нужен v33+. Самый сильный behavioural-equivalence guard был unavailable; полагались на test suite (слабее).

**Закрытие — recommendation:** **continuous re-capture + checklist** (минимальный вариант, Phase 1):
1. Захватить v34-baseline corpus сейчас, до следующего изменения.
2. Добавить в `extension-checklist.md` пункт «после schema bump'а пере-захватить golden corpus».
3. CI smoke: `replay_ai_log --compare-golden` against baseline после любого изменения в pipeline.

**Полный migration tool** (несколько дней) имеет смысл только если корпус начнёт быть source-of-truth для исторических scenarios — пока что continuous re-capture дешевле и закрывает блокировку для E1/A2/B2.

---

### C3. `pub(crate)` → `pub` промоушены ради re-exports

**Effort:** ~1 час cleanup · **Risk:** нулевой · **Phase:** 5 (opportunistic) · **Blocked by:** —

**Где:** `build_summon_dpr_cache` и подобные. R6 потребовал поднять видимость для `pub use` в module re-exports.

**Проблема.** Rust не различает «public API модуля внутри крейта» от «public API крейта наружу». Ради цепочки `pub use` приходится поднимать визибельность сверх необходимого.

**Почему болит.** Ослабление encapsulation. Внешний код может случайно начать использовать символ, не предназначенный для широкого употребления.

**Закрытие.** Минор: использовать `pub(crate) use` где возможно вместо `pub use`. Не везде Rust позволит цепочку, но в большинстве случаев да. Делать opportunistically при следующем касании affected модулей.

---

### C5. `AiTags` смешивает unit state и unit capabilities

**Effort:** ~полдня (если делать) · **Risk:** низкий · **Phase:** 5 (opportunistic, реактивно) · **Blocked by:** —

**Где:** `src/combat/ai/world/tags/ai_tags.rs` (определение); заполнение в `src/combat/ai/world/snapshot.rs:490-560` (`compute_unit_tags`-подобный код).

**Проблема.** Один `AiTags` bitflags несёт две разные природы флагов:
- **Unit state (runtime):** `LOW_HP`, `IS_STUNNED`, `FORCES_TARGETING` — производные от текущего состояния юнита (HP, статусы).
- **Unit capabilities (content-derived):** `CAN_HEAL`, `CAN_CC`, `HAS_AOE`, `MELEE_ONLY`, `RANGED` — производные от содержимого юнита (его способностей).

Они живут в одном bitset, конструируются в одном месте, проверяются одинаково. Но семантически — это разные домены. Capabilities стабильны на бой; state меняется каждый ход.

**Почему болит.** Пока что *не очень болит* — флагов мало, они работают. Но если добавить новые tag-кандидаты (`CAN_REVIVE`, `IS_SUMMON`, `IS_BURNING`), решение «куда их класть» неоднозначно, и путаница накапливается.

**Закрытие.** Расщепить на `UnitStateTags` (LOW_HP, IS_STUNNED, FORCES_TARGETING) и `UnitCapabilityTags` (CAN_HEAL, CAN_CC, HAS_AOE, MELEE_ONLY, RANGED). Capabilities считаются раз при построении snapshot'a; state — каждый snapshot bump.

**Когда делать:** реактивно — при следующей попытке добавить tag, который не вписывается в имеющуюся природу. До тех пор preventive split не оправдан.

---

## D. Тестовая дисциплина

### D1. Test setup boilerplate

**Status:** ✅ closed вместе с B4 (`ai/PhaseT`).
**Cливается с B4** — `TestStageHarness` покрывает оба case'а (setup + trace.base init). Не отдельная задача.

### D2. Pre-existing parity tests могут таиться

**Priority:** Skip (preventive process, не задача).

**Урок реструктуризации.** До рефакторинга жили «duality» состояния (legacy code + новая ветвь под условным флагом или с parity-тестами). При R-track'е находили: `factors/saturation.rs` legacy + `factors/step/saturation.rs` leaf, `pipeline/stages/adaptation.rs` legacy + ModeSelection/Finalize new. Каждый из них — pre-existing tech debt, замаскированный комментарием «legacy reference».

**Practice — preventive:** при появлении подобных дубликатов фиксировать их сразу в этот документ как кандидата на cleanup, не оставлять «временно». Сама по себе задача в roadmap не идёт.

---

## E. Инвариант-pain

### E1. ScoreTrace algebra ↔ pre-P3a поведение — никогда полностью не verified

**Status:** ✅ closed (`ai/Phase1`) по варианту "закрепить v34 как baseline". Pre-P3a archive отсутствует (старый corpus в `logs/` — v26, ниже MIN_SUPPORTED=33), сравнить буквально не с чем; вместо этого baseline `tests/baselines/baseline_v34.jsonl` фиксирует current state как ground truth. `golden_baseline_zero_diff` integration test ловит **будущие** push/apply-order drifts. Если архив pre-P3a когда-либо найдётся — investigation отдельным slice'ом.
**Effort:** полдня verification + N для investigation если drift найдётся · **Risk:** невидимый класс багов · **Phase:** 1 · **Blocked by:** C2 (corpus).

> **Важно:** E1 — это просто прогон existing replay против fresh corpus. Закрывается в Phase 1 одновременно с C2, не требует архитектурных изменений. Если drift найдётся — investigation отдельно. Phase 2 (R8-lite) делает алгебру централизованной (одна функция в drive-loop), что предотвращает **будущие** drift'ы по push-order, но pre-existing drift проверяется в Phase 1.

**Проблема.** `ScoreTrace::compute()` (mask poison → base → multipliers → addends) построен так, чтобы давать identical результат с ad-hoc математикой `ann.score *= multiplier; ann.score += contribution; ann.score = NEG_INFINITY`. Логически это та же формула; на практике порядок операций над f32 может отличаться (e.g. push-order vs apply-order). Roadmap требовал ε = 1e-5 на decisions через golden replay corpus.

**Реальность.** Corpus был несовместим (см. C2). `compute()` алгебра тестировалась в изоляции (8 unit-тестов в `score_trace.rs`). Decision-equivalence на реальных корпусах **не verified**.

**Риск.** Если есть behavioural drift — мы про него не знаем. Может быть нулевой; может быть ε ~ 10⁻⁴ на конкретных pos/neg-multiplier комбинациях. Это самый скрытый класс багов в pipeline после P3a/P3b.

**Закрытие.**
1. Восстановить v34 baseline corpus (см. C2).
2. Запустить `replay_ai_log --capture-golden` для baseline.
3. Если есть pre-P3a archive — сравнить decision-equivalence напрямую; если нет — закрепить v34 как новую baseline и идти дальше.
4. Если decision drift — investigate (likely f32 commutativity issue в порядке push/apply), fix, re-verify.

Без E1 пункт A2 рискует поведенческим изменением, которое некому поймать.

---

## F. R8-lite — Score Effect Engine (Phase 2)

**Effort:** ~1 неделя · **Risk:** средний (затрагивает score-effect family) · **Phase:** 2 · **Blocked by:** Phase 1 (corpus + replay guard), Phase T (TestStageHarness — рекомендуется).

**Что это.** Узкий, **behavior-preserving** архитектурный refactor: централизация writes в pipeline через `StageEffect` API. Не umbrella «всё в одном» — это отдельная стадия плана, после которой ещё идут A2, B2-cleanup, и opportunistic уборка.

### Корневая проблема

Pipeline сейчас работает по принципу **shared mutable state + runtime-validated contracts**:

- Каждая стадия видит `ScoredPool`/`PlanAnnotation` целиком, мутирует напрямую (`ann.score *= mul`, `ann.score = NEG_INFINITY`, `ann.score_trace.push_*`).
- Контракты «какая стадия что пишет» живут в `STAGE_SPECS` как runtime таблица, проверяются тестом.
- Observability (score_trace + legacy modifiers/sanity/critics) обновляется каждой стадией дважды — синхронизация на совести стадии.

Из этого следует **B1** (annotation = свалка), **B2-writer-dedup** (drift между каналами), плюс симптомы: `f32_finite` workaround, `debug_assert_eq` в killable_gate, `score_initial` в annotation, pre_scores buffer dance.

### Что входит в R8-lite (DoD: golden replay diff = 0)

#### 1. Stage Effect API — только для score-effect family

```rust
enum StageEffect {
    Multiplier(MultiplierHit),
    Addend(AddendHit),
    Mask(MaskHit),
    Gate(GateHit),
    // плюс observability-only: SanityHit, CriticHit (read-only side-channel)
}
```

Стадия возвращает `Vec<StageEffect>` (или `Option<StageEffect>` если по факту один):
```rust
fn compute_effects(&self, ctx: &StageCtx, plan: &TurnPlan, ann: &PlanAnnotation)
    -> Vec<StageEffect>;
```

**Применяется только к score-effect family:**
- Modifiers (Addend)
- Critics (Multiplier)
- Sanity (Multiplier + observability)
- ProtectSelfMask (Mask)
- KillableGate (Mask, **сохраняем текущее поведение**, см. ниже)
- ContractMask (Mask)

**НЕ применяется** к: ModeSelection, Finalize, ItemScoring, OverlayConsiderations, PickBest, RepairAffinity, Viability — у них другая природа (выбор mode'а, агрегация, agenda evaluation, picking). Они остаются как есть.

#### 2. Drive-loop — sole writer

Новый код (~150 LOC) — единственное место, которое:
- Устанавливает `score_trace.base = ann.score` на старте каждой группы стадий.
- Перебирает стадии в каноническом порядке (тот же, что сейчас).
- Аккумулирует `score_trace` из возвращённых `StageEffect`'ов.
- **Одновременно** обновляет legacy fields (`modifiers`, `sanity`, `critics`, `contract`) из тех же `StageEffect`'ов — это закрывает B2-writer-dedup без удаления самих fields.
- В конце применяет `ann.score = score_trace.compute()`.

#### 3. B1-soft, ограниченный scope

Приватизируются **только score/effect-owned** поля `PlanAnnotation`:
```rust
pub struct PlanAnnotation {
    pub(crate) score: f32,
    pub(crate) score_trace: ScoreTrace,
    pub(crate) modifiers: Vec<ModifierContribution>,
    pub(crate) sanity: Vec<SanityHit>,
    pub(crate) critics: Vec<CriticHit>,
    pub(crate) contract: Option<ContractMaskHit>,
    // остальные поля — пока как есть (Phase 5, opportunistic)
    pub outcomes: Vec<ActionOutcomeEstimate>,
    pub terminal: FactorTerminalScore,
    // ...
}

impl PlanAnnotation {
    pub(crate) fn apply_stage_effects(&mut self, effects: &[StageEffect]) { ... }
    pub(crate) fn set_score_base(&mut self, base: f32) { ... }
    pub fn final_score(&self) -> f32 { self.score }
}
```

Вне модуля `pipeline` (включая тесты `pipeline::*` через `pub(crate)`) score/effect поля невидимы для записи.

### Жёсткие границы Phase 2 — что R8-lite НЕ делает

```
R8-lite is behavior-preserving.

It MAY:
  - centralize writes through drive-loop
  - privatize score/effect-owned fields
  - introduce StageEffect return API for score-effect family stages
  - remove f32_finite workaround IF и только если KillableGate перестанет
    эмитить NEG_INFINITY (см. ниже — но это требует Phase 3, поэтому
    в Phase 2 f32_finite остаётся)

It MUST NOT:
  - change KillableGate semantics (Mask emission stays — engine просто
    позволяет Mask hit)
  - remove or rename JSONL legacy observability fields
  - change PickBest ranking logic
  - apply effect-trait abstraction to non-score-effect stages
  - change schema_version
```

DoD: `replay_ai_log --compare-golden baseline.jsonl` returns diff = 0 на baseline'е, захваченном в Phase 1.

### Что закрывается по конструкции в R8-lite

| Пункт | Как |
|---|---|
| **B1-soft** (score/effect fields) | `pub(crate)` + writer methods, drive-loop — единственный writer |
| **B2-writer-dedup** | Drive-loop пишет оба канала из одного `StageEffect`. Drift невозможен. |
| `score_initial` в annotation | Уезжает в local scope drive-loop / PickBest |
| `pre_scores` buffer dance в KillableGate | Drive-loop применяет `Mask` атомарно, стадия возвращает `MaskHit` |
| Push-order brittleness в score_trace | Алгебра — одна функция в drive-loop, push-order = canonical apply-order |

**Что НЕ закрывается в Phase 2** (нужны последующие фазы):
- A2 (KillableGate Mask → true Gate) — Phase 3
- B2-schema-cleanup (удаление legacy из JSONL) — Phase 4
- `f32_finite` workaround, `debug_assert_eq` инвариант — пока KillableGate эмитит Mask, они нужны; уйдут в Phase 3
- Pre-existing ScoreTrace drift на старом corpus — проверяется в Phase 1, не в Phase 2

### Почему именно эти границы

- **Не смешивать architecture с KillableGate semantics.** Если drift в decisions появится после комбинированного PR, неотличимо: виноват drive-loop, trace algebra, или новая Gate. Раздельно — каждый PR имеет понятный DoD.
- **Не трогать schema в Phase 2.** Drive-loop dual-writes — корректная страховка. Удалять legacy fields имеет смысл только когда mining реально читает trace_log (Phase 4a).
- **Не абстрагировать non-score-effect стадии.** ModeSelection / PickBest / etc. имеют другую природу; effect-trait для них — over-abstraction.
- **B1 ограничен.** Приватизировать ~30 полей сразу = большой PR без понятного DoD. Score/effect поля — естественная единица: они про эффекты в pipeline, и они единственные, кому R8-lite даёт владельца (drive-loop).

### Когда брать

- Если ai/pipeline активно растёт (новые score effects, новые critics) — **выгоднее R8-lite сейчас**: новые стадии будут писать одну функцию `compute_effects` вместо ритуала.
- Если pipeline в режиме maintenance — может подождать.
- **Обязательное предусловие**: Phase 0 (расчищенная семантика) и Phase 1 (corpus + replay guard). Без replay guard'а DoD «diff = 0» непроверяем.

---

## Когда обновлять этот документ

- При обнаружении новой структурной проблемы — добавлять в соответствующую группу (A/B/C/D/E) с meta-строкой `Effort/Risk/Phase/Blocked by`.
- При закрытии пункта — удалять (или помечать `~~strikethrough~~` со ссылкой на коммит). При закрытии целой фазы — обновить TL;DR и таблицу «Что закрывает каждая фаза».
- Раз в квартал — review плана: иногда Phase 0/5 пункты копятся, иногда зависимости между фазами становятся неточными (например, появляется требование к Phase 2, которого изначально не было).
