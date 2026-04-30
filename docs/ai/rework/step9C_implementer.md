# Implementer Plan — Сабшаг 9.C (Calibration + scenarios + cleanup)

**Source of truth:** `/Users/splav/personal/storyforge/docs/ai_rework_step9_plan.md` секция «Сабшаг 9.C». Этот документ — implementer-friendly декомпозиция; стилистический образец 9.B (`docs/ai_rework_step9B_implementer.md`).

**Prereq:** 9.B landed (commits `3651ba0..c749e74`). Working tree clean. Schema v30. ai_scenarios 15/15 passing.

**Scope.** Добор pin-фикстур под tag-driven branches, mining sections под новые сигналы, mining-driven sanity curves, чистка legacy comments + docs.

**Поведение AI не меняется.** Это observability + scenario coverage + cleanup. Calibration может изменить curve coefficients, но обнаруживается только если distributions патологические — ожидаем no-op для большинства метрик.

---

## Зафиксированные решения

| # | Развилка | Решение | Аргумент |
|---|---|---|---|
| 1 | `rescue_ally_via_heal_tag` — новый scenario или используем 9.B smoke | **Используем `rescue_via_heal_in_threat`** (создан в 9.B.4) и убираем дублирование из плана 9.C | Smoke уже pin'ит rescue_ally pathway; новый файл = duplicate. |
| 2 | `peel_via_taunt` scenario — pin или drop | **Пытаемся найти в v30 corpus**; если actor с taunt в kit'е реально побеждает в scoring через role-shift — pin; если нет — backlog с пометкой «требует Peel-aware terminal axis в future step» | В 9.B Peel живёт только в tag_axis_vote (role bias); прямого consumer'а в scoring/intent нет. Real Peel test может оказаться слабым. |
| 3 | Mining-калибровка curves | **Verify-only first**: запустить `mine_ai_logs` с новыми секциями, посмотреть distributions, **корректировать только если выходят за expected bounds** (5–25% rescue, 5–20% apply_cc) | Цифры в плане 9.B PROVISIONAL, но не arbitrarily. Mining как verification, не automatic tuning. |
| 4 | Schema bump для mining sections | **Без bump** — секции читают v30 как есть, добавляются новые aggregations, schema стабильна | Mining = read-only over corpus. |
| 5 | `docs/ai.md` обновление | **Целиком пересмотреть AI Roles секцию** — tag-driven inference, 7+5+1 tag dictionary (AbilityTag + StatusTag + Compulsion), pointer на `tags/classify.rs` | В 9.A/9.B docs/ai.md не трогали — за две стадии накопилось расхождение. |

---

## Декомпозиция на коммиты (3 коммита)

### Commit 1 — 4 новых ai_scenarios под tag-driven branches (~0.5 дня)

**Scope.** Добор фикстур на untested pathways. Существующие 15 сценариев — про общее поведение; здесь pin'им конкретно tag-driven сигналы.

**Сценарии:**

1. **`apply_cc_skips_already_hardcc_target`** — actor с stun-likes в kit'е, два врага: один уже under HardCC, другой healthy threat. Assertion: actor таргетит non-stunned.
   - Discovery: scan v30 logs на `actor_tick` events где `actor.abilities` ∩ ApplyCC-tagged != ∅, в snapshot есть enemy с status `stunned`/`paralyzed` И другой без; AI выбирает unstunned target.
   - Если не находится — slice synthesizing from existing combat (одна и та же scene, два plan_id'а).

2. **`actor_status_hardcc_invalidates_goal`** — actor поймал stun посередине плана; continuation должна быть `GoalAbandonedReactive { source: ... }`.
   - Discovery: scan на `plan_divergence` events с `continuation_outcome.kind == "goal_abandoned_reactive"` И severity invalidating И reason связанный с `actor_status_changed`. Через `jq` или mining filter.

3. **`actor_status_dot_tick_preserves_goal`** — actor под burning/poisoned, tick прошёл; continuation остаётся `GoalPreservedInTransit`.
   - Discovery: actor с DOT status, plan_id где status_hash изменился (tick), но added/removed sets empty → severity Cosmetic → goal preserved.

4. **`peel_via_taunt`** — actor с taunt в kit'е делает taunt в ситуации где ally threatened.
   - Discovery: scan на actor с `taunt` в abilities, в reach до threatened ally, AI выбирает `cast/taunt`.
   - **Если не находится** — drop с TODO «requires Peel-aware terminal axis (backlog)».

**Реализация:**

- Использовать существующие 6 v30-логов в `tests/ai_scenarios/snapshots/`.
- Можно создать новые scenario folders из slice'ов.
- Каждый — pin минимально: `decision_kind` + `cast_ability` (или `intent_kind`) + что-то specific (target_already_hardcc, status diff).

**Acceptance commit 1:**
- `cargo test --test ai_scenarios` зелёный (15 → 18 или 19, в зависимости от peel_via_taunt).
- Каждый новый scenario использует **реальный v30 log slice** (synthesis запрещён).
- Если peel_via_taunt не находится — вернуться с TODO и продолжить.

**Объём:** 4 новых scenario folder'а (или 3 + drop), ~80 LOC overlay'ев total.

---

### Commit 2 — Mining sections (~0.5 дня)

**Scope.** Добавить 3 новые секции в `src/bin/mine_ai_logs.rs` для observability над tag-driven сигналами.

**Изменения:**

1. **`=== AI tags coverage ===`** секция:
   - Per-tag distribution среди chosen plans: % planов где Cast step имел Offensive/Defensive/Rescue/Summon/Mobility/ApplyCC/Peel.
   - Override usage: какой % chosen abilities имеет `ai_tags_override = Some(_)` в content (статически из `ContentDb` либо динамически из `effective_ai_tags` slice).
   - StatusTag coverage среди applied statuses: HardCC/SoftCC/Dot/Buff/Compulsion/Cosmetic %.

2. **`=== Need signals (post-9.B) ===`** секция:
   - Distributions для `rescue_ally` и `apply_cc` (mean, p50, p90, p99, % > 0.1).
   - Старые need signals (`self_preserve`, `continue_commitment`, и т.д.) — оставить (могут уже быть mined; сверить).
   - `setup_aoe` — pin как always-zero (verify gate).

3. **`=== Continuation severity (post-9.B) ===`** секция:
   - Per-severity counts на `actor_status_changed` events: Cosmetic / Relevant / Invalidating.
   - Cross-tab: severity × StatusTag (HardCC set → Invalidating, Dot tick → Cosmetic, etc.).
   - Goal continuation rate: % preserved vs abandoned per severity.

**Реализация:**

- Расширить `Aggregate` struct с новыми полями.
- В `process_event`/`process_continuation` инкрементить.
- В `main` print три новых секции после существующих.

**Тесты:**
- Unit: новые counters накапливаются на synthetic events.
- E2E: запустить `cargo run --bin mine_ai_logs -- tests/ai_scenarios/snapshots/road_bridge/log.jsonl` — секции выдаются без panic.

**Acceptance commit 2:**
- 3 новые секции выводятся в `mine_ai_logs` output.
- Sanity: `setup_aoe` строго 0 во всём corpus (regression catch).
- Существующие mining-секции не сломаны.

**Объём:** ~150 LOC в `bin/mine_ai_logs.rs`, ~30 LOC tests.

---

### Commit 3 — Calibration verify + cleanup + docs (~0.25–0.5 дня)

**Scope.** Mining-driven sanity check на provisional curves; cleanup legacy TODO/FIXME; docs/ai.md обновление; финализация шага.

**Подшаги:**

**3.1 Mining verification:**
- Запустить `cargo run --bin mine_ai_logs -- logs/20260428T*.jsonl` (все 6 v30-логов).
- Проверить bounds для `rescue_ally` и `apply_cc`:
  - **Expected:** `rescue_ally > 0.1` в 5–25% chosen plans на kit'ах с heal/field_medic.
  - **Expected:** `apply_cc > 0.1` в 5–20% chosen plans на kit'ах с stun-likes.
  - **Out-of-bounds → корректировка:** если >25% — поднять Logistic mid; <5% — опустить.
- Curve-update коммит **только если** наблюдаем pathological distributions; иначе verify-passed.

**3.2 Cleanup TODO/FIXME:**
- `appraisal/mod.rs:61` (комментарий «Activation requires step 9 (semantic AI tags / appraisal rules)») — **удалить**.
- `repair/mod.rs:62` (комментарий «could become Invalidating after step 9 adds semantic tags») — **удалить или переписать**: status_changed теперь читает StatusTag через ctx.status_tags.
- Glob search `rg "step 9|ability_vote|0\.0 stub" src/` — нулевые residual references.
- Проверить `compute_factors` references — должны быть 0 уже после step 8.

**3.3 Docs update:**
- `docs/ai.md` секция AI Roles: переписать на tag-driven inference. Список 7 ability tags + 5 status tags + Compulsion (6-й status tag из 9.B). Pointer на `src/combat/ai/tags/classify.rs`.
- `docs/ai_rework.md` — пометить step 9 как **DONE** (как было сделано для step 8 в `d1b28cc`). Список backlog'а:
  - Setup / Cleanse / ZoneControl tags — out of scope (нет механики).
  - Finisher / Escape — не теги (outcome property / intent context).
  - commitment_skill — step 12.
  - Peel-aware terminal axis — backlog (если peel_via_taunt drop'нулся в commit 1).

**Acceptance commit 3:**
- Mining-секции на реальном corpus показывают bounds.
- 0 references на step-9 TODO/FIXME в src/.
- `docs/ai.md` отражает tag layer.
- `docs/ai_rework.md` step 9 → DONE с backlog.
- Final gate: `cargo test --all-targets` зелёный, `cargo clippy --all-targets -- -D warnings` 0 warnings.

**Объём:** ~10 LOC code edits (cleanup + maybe curve adjust), ~80 LOC docs.

---

## Acceptance gate 9.C (полный)

1. ai_scenarios 15+3 (или 15+4 если peel) passing на v30 corpus.
2. `mine_ai_logs` показывает три новые секции без panic.
3. Distributions для `rescue_ally`/`apply_cc` в bounds (5–25% / 5–20% активаций) ИЛИ corrected through curve update.
4. `setup_aoe` распределение pin'ed на 0.
5. 0 step-9 TODO/FIXME в production code (`rg "step 9|FIXME.*tag|TODO.*tag" src/`).
6. `docs/ai.md` AI Roles секция отражает tag-driven inference.
7. `docs/ai_rework.md` step 9 → DONE.
8. `cargo test --all-targets` + `cargo clippy --all-targets -- -D warnings` зелёные.

---

## Финальная таблица коммитов

| # | Title | Estimate | Files | New tests/scenarios |
|---|---|---|---|---|
| 1 | 3–4 новых ai_scenarios (apply_cc_skips, hardcc_invalidates, dot_tick_preserves, peel_via_taunt try) | 0.5 дня | `tests/ai_scenarios/snapshots/<new>/*` | 3–4 scenarios |
| 2 | Mining sections (AI tags coverage / Need signals post-9.B / Continuation severity post-9.B) | 0.5 дня | `bin/mine_ai_logs.rs` + tests | ~6 unit |
| 3 | Mining verify + cleanup TODO/FIXME + docs/ai.md + docs/ai_rework.md DONE | 0.25–0.5 дня | `appraisal/mod.rs`, `repair/mod.rs`, `docs/ai.md`, `docs/ai_rework.md` | 0 |
| **Total** | | **~1.25–1.5 дня** (соответствует spec'овскому 1.0–1.5) | | **~10 new** |

---

## Что откладывается (backlog после 9.C)

- **Peel-aware terminal axis или critic** — если peel_via_taunt не pin'ится в commit 1, потребует Peel-aware scoring consumer (отдельный future step).
- **Setup / Cleanse / ZoneControl mechanics** — нет shape-полей; будут добавлены вместе с механиками (channel/marker/hazard zones).
- **commitment_skill tag** — step 12 (mid-plan reflow).
- **Override TOML**-разметка — сейчас 0 cases. Если в 9.C/после mining'а появится конкретный пограничный случай — добавится отдельным content-PR.

---

### Critical Files for Implementation

- `/Users/splav/personal/storyforge/src/bin/mine_ai_logs.rs` — commit 2, 3 новые mining секции.
- `/Users/splav/personal/storyforge/src/combat/ai/appraisal/mod.rs` — commit 3, удаление step-9 TODO.
- `/Users/splav/personal/storyforge/src/combat/ai/repair/mod.rs` — commit 3, удаление step-9 TODO.
- `/Users/splav/personal/storyforge/docs/ai.md` — commit 3, AI Roles секция.
- `/Users/splav/personal/storyforge/docs/ai_rework.md` — commit 3, step 9 → DONE.
- `/Users/splav/personal/storyforge/tests/ai_scenarios/snapshots/<new>/*` — commit 1, 3–4 новых scenario folders.
