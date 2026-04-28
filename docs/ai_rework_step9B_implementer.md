# Implementer Plan — Сабшаг 9.B (Hardcode removal через теги)

**Source of truth:** `/Users/splav/personal/storyforge/docs/ai_rework_step9_plan.md` секция «Сабшаг 9.B». Этот документ — компактная implementer-friendly декомпозиция; стилистический образец 9.A (`docs/ai_rework_step9A_implementer.md`) урезан вдвое: без построчных pin-таблиц, без exhaustive risk-регистра.

**Prereq:** 9.A landed (commits `0570445..5cae0f7`). Verified via `rg AbilityTagCache src/`:
- `src/combat/ai/tags/{mod,classify,cache}.rs` присутствуют.
- `AiWorld.ability_tags: &AbilityTagCache` (см. `utility/mod.rs:140`).
- `PlanAnnotation.effective_ai_tags: Vec<AbilityTagSet>` writeback в `utility/mod.rs:287–295`.
- `AbilityDef.ai_tags_override`, override-resolver `cache.effective(...)`.
- `test_helpers::empty_caches()` — для unit-тестов consumers.

**Working tree:** clean, schema v29 без bump.

---

## Зафиксированные решения

| # | Развилка | Альтернативы | Решение | Аргумент |
|---|---|---|---|---|
| 1 | Cascade `infer_profile(&AbilityTagCache)` | (a) param-injection (b) `WorldView` extension (c) global `OnceCell` | **(a)** — пробросить `&AbilityTagCache` параметром во все production call-sites + 7 тестов | 4 callsite-а вне AI-модуля (`combat/phases.rs`, `combat/spawn.rs`, `scenario/combat_scene.rs:60,86`) — Bevy systems и helper'ы, у всех `Res<AbilityTagCache>` достижим. `WorldView`-extension — overkill для одной функции. `OnceCell` — глобальное состояние, ломает scenario-isolation. |
| 2 | Status diff в `classify_mismatch` | (a) расширить `PlanSnapshot.actor_statuses: Vec<StatusId>` (b) `StatusDelta` через `MismatchContext` (c) caller-computed delta | **(a) + (b) + shared helper** — `PlanSnapshot` и `StoredGoalContext` оба хранят `actor_statuses_at_capture: Vec<StatusId>`; единый `compute_status_delta` помощник в `repair/mod.rs`; classifier принимает `MismatchContext { delta, status_tags }`. `actor_status_hash` сохраняется как fast-path | Один источник истины для diff-логики; mirror-logic между snapshot/goal не разойдётся. |
| 3 | Producer формулы для `rescue_ally`/`apply_cc` | Inline / отдельный модуль | **inline в `appraisal/mod.rs`** рядом с другими producer'ами (`compute_self_preserve` etc.) | Стилевое единообразие — все producer'ы там; ~30–50 строк каждый. |
| 4 | Куда мапится `tag_axis_vote` | Inline в `role.rs` / отдельный файл `tags/role_axis.rs` | **inline в `role.rs`** | Compact (~35 LOC), один call-site (`infer_profile`). Spec явно говорит «mapping живёт в одном месте». |
| 5 | Tuning curves | Reuse существующие / добавить `rescue_ally` + `apply_cc` curves | **добавить две новые `Curve`-записи** + пометить **PROVISIONAL** до 9.C | Producer'ы не должны hard-кодить shape; mining-калибровка в 9.C обязательна. Сейчас числа — placeholder. |
| 6 | Schema versioning | Additive `#[serde(default)]` в v29 / bump v30 | **bump v30** с явной миграцией (`v29 → v30`: `actor_statuses_at_store: Vec::new()`) | Two-flavor v29 (с/без поля по same name) скрывает реальное изменение формата. Bump = honest signal. |
| 7 | Сигнатура `compute_need_signals` | 8-параметровая / `&AppraisalCtx` группирующий объект | **`&AppraisalCtx<'a>`** | Паттерн `ScoringCtx`/`StageCtx` уже в кодбейзе. 8 параметров нечитаемы. |
| 8 | `taunted` / `forces_targeting` semantics | Cosmetic (как в 9.A) / `Invalidating` через новый StatusTag | **расширить StatusTag шестым вариантом `Compulsion`** (derived из `forces_targeting=true`); `Compulsion` set на actor'е → `Invalidating` | Семантически наложение taunted насильственно меняет цель → не Cosmetic. Закрывает open question 5 из 9.A до начала 9.B-consumer работы. Включается в commit 0. |
| 9 | Smoke scenario тип | (a) Peel pathway через role-shift (b) honest rescue_ally pathway | **(b)** — actor с heal в kit'е + ally в danger → ProtectAlly. Имя `rescue_via_heal_in_threat` | Peel-тег в 9.B живёт ТОЛЬКО в role-axis (commit 1); нет consumer'а Peel в scoring/intent. Сценарий «taunt побеждает heal через role-shift» — слишком тонкая цепочка для smoke. Real Peel-test переезжает в 9.C, когда добавится Peel-aware terminal axis. |
| 10 | Numeric acceptance gate для behavioral shifts | Ручная проверка / числовые критерии | **числовые критерии** (см. секцию «Acceptance gate 9.B» ниже) | Без чисел нет «готов/не готов». Mining baseline даёт нижнюю/верхнюю границы expected distributions. |

---

## Декомпозиция на коммиты (5 коммитов)

Порядок строго sequential — каждый ломает либо роль, либо need-сигнал, либо severity. `cargo check` зелёный после каждого.

### Commit 0 — Foundation patches (StatusTag::Compulsion + shared utilities)

**Scope.** Закрыть две архитектурные дыры **до** consumer-работы:
1. Расширить `StatusTag` шестым вариантом `Compulsion` (derived из `forces_targeting=true`) — закрывает open question 5 из 9.A.
2. Ввести `AppraisalCtx<'a>` — группирующая структура для будущей сигнатуры `compute_need_signals` (commit 2).
3. Ввести pure helper `compute_status_delta(stored: &[StatusId], current: &[ActiveStatusView]) -> StatusDelta` в `repair/mod.rs` — будет shared между PlanSnapshot и StoredGoalContext (commit 3).

**Ключевые изменения:**

```rust
// src/combat/ai/tags/mod.rs
pub enum StatusTag {
    HardCC,
    SoftCC,
    Dot,
    Buff,
    Compulsion,    // <— NEW: forces_targeting (taunted)
    Cosmetic,
}

// src/combat/ai/tags/classify.rs::derive_status_tags
if def.forces_targeting { s.insert(StatusTag::Compulsion); }
// Compulsion живёт параллельно с другими тегами; не вытесняет Cosmetic-fallback,
// если статус ТОЛЬКО forces_targeting, то Compulsion ставится, Cosmetic — нет.

// src/combat/ai/appraisal/mod.rs (только тип, использование — в commit 2)
pub struct AppraisalCtx<'a> {
    pub active: &'a UnitSnapshot,
    pub snap: &'a BattleSnapshot,
    pub maps: &'a InfluenceMaps,
    pub memory: &'a AiMemory,
    pub tuning: &'a AiTuning,
    pub ability_tags: &'a AbilityTagCache,
    pub status_tags: &'a StatusTagCache,
    pub content: &'a ContentView,
}

// src/combat/ai/repair/mod.rs
pub struct StatusDelta {
    pub added: Vec<StatusId>,
    pub removed: Vec<StatusId>,
}

pub fn compute_status_delta(
    stored: &[StatusId],
    current: &[ActiveStatusView],
) -> StatusDelta { ... }
```

**Schema bump v29 → v30** (для consistency — applied здесь, fields поэтапно добавляются в commit 3):

```rust
// src/combat/ai/log.rs
pub const SCHEMA_VERSION: u32 = 30;

// Migration: v29 logs дают LogError::UnsupportedSchema (clean break, как в step 7→8).
// Поля `actor_statuses_at_store` появляются в v30 corpus.
```

**Альтернатива (отвергнутая):** `#[serde(default)]` в v29 — создаёт «two-flavor v29» (corpus до 9.B и после имеют разный shape под одним номером). Bump v30 — honest.

**Удаляется:** ничего.

**Тесты:**

- `derive_status_tags_taunted_has_compulsion` — pin (taunted → `Compulsion | Cosmetic` или просто `Compulsion`, см. ниже).
- `derive_status_tags_compulsion_is_set_for_forces_targeting` — generic.
- `compute_status_delta_added_diff` / `_removed_diff` / `_pure_tick_empty` — 3 теста.
- `appraisal_ctx_for_test_helper_constructs` — sanity helper-конструктор.
- Schema migration: `log_v29_yields_unsupported_schema` (mirror commit 4 в step 8.A.2).

**Acceptance commit 0:**
- `StatusTag::Compulsion` существует; `taunted` классифицируется через него.
- `compute_status_delta` — pure, нет side effects.
- `AppraisalCtx` определён, не используется (commit 2 wires up).
- `SCHEMA_VERSION = 30`.
- `cargo check --all-targets` зелёный.

**Объём:** ~30 LOC `tags/` delta, ~50 LOC `repair/mod.rs` delta, ~25 LOC `appraisal/mod.rs` (struct only), ~80 LOC тестов, schema bump.

**Note про `taunted`:** в 9.A classifier ставит `Cosmetic` как fallback, если ни один из других тегов не сработал. С добавлением `Compulsion`: `taunted` → `{Compulsion}` (без Cosmetic). Pin-test 9.A на `taunted` придётся обновить (~1 строка).

---

### Commit 1 — `role::infer_profile` tag-driven

**Scope.** Заменить структурный `ability_vote` на `tag_axis_vote(tags, def, weight)`. Удалить `ability_vote` и `has_damage`. Расширить сигнатуру `infer_profile`. Обновить 3 production call-sites + 7 тестов в `role.rs`.

**Ключевые сигнатуры:**

```rust
// src/combat/ai/role.rs
pub fn infer_profile(
    abilities: &[AbilityId],
    max_hp: i32,
    total_armor: i32,
    content: &ContentView,
    tag_cache: &AbilityTagCache,   // <— NEW
) -> AxisProfile { ... }

fn tag_axis_vote(
    tags: AbilityTagSet,
    def: &AbilityDef,
    weight: f32,
) -> [f32; 5] {
    let mut v = [0.0; 5];
    if tags.contains_tag(AbilityTag::Rescue)    { v[4] += weight; return v; }
    if tags.contains_tag(AbilityTag::Summon)    { v[4] += weight*0.7; v[2] += weight*0.3; return v; }
    if tags.contains_tag(AbilityTag::Defensive) && !tags.contains_tag(AbilityTag::Offensive) {
        v[0] += weight; return v;
    }
    if tags.contains_tag(AbilityTag::Offensive) {
        // melee/ranged split — единственное место, где shape ещё нужен.
        let is_ranged = matches!(def.effect, EffectDef::SpellDamage{..})
            || def.aoe != AoEShape::None
            || def.range.min >= 2;
        if is_ranged { v[2] += weight } else { v[1] += weight };
        if tags.contains_tag(AbilityTag::ApplyCC) { v[3] += weight*0.4; }
        return v;
    }
    if tags.contains_tag(AbilityTag::ApplyCC) { v[3] += weight; return v; }
    if tags.contains_tag(AbilityTag::Peel)    { v[0] += weight*0.7; v[4] += weight*0.3; return v; }
    if tags.contains_tag(AbilityTag::Mobility){ v[1] += weight*0.3; return v; }
    v
}
```

**Tag→axis pin-map (центральная documentation, не таблица per-ability):**

| Primary tag (priority order) | Axis vote |
|---|---|
| Rescue | Support 1.0 |
| Summon | Support 0.7 + Ranged 0.3 |
| Defensive (без Offensive) | Tank 1.0 |
| Offensive ranged (`SpellDamage` / aoe / `range.min ≥ 2`) | Ranged 1.0 (+ Control 0.4 если +ApplyCC) |
| Offensive melee | Melee 1.0 (+ Control 0.4 если +ApplyCC) |
| ApplyCC (без Offensive) | Control 1.0 |
| Peel | Tank 0.7 + Support 0.3 |
| Mobility-only | Melee 0.3 |
| `empty()` | 0 (только stat-based tank floor добавится позже) |

**Удаляется:** `role::ability_vote` (lines 240–296), `role::has_damage` (lines 298–306).

**Cascade на call-sites:**

- `src/combat/phases.rs:72` — Bevy system; добавить `Res<AbilityTagCache>` параметр, передать `&tag_cache`.
- `src/combat/spawn.rs:107` — system context; same.
- `src/scenario/combat_scene.rs:60, 86` — функция `build_combat_scene(...)` принимает `&ContentView`; добавить `&AbilityTagCache` параметр; cascade на её caller (scenario startup).

**Тесты:**

- Все 7 существующих role-тестов (`infer_kael_is_ranged`, `infer_aldric_is_control_tank` etc.) обновить: добавить `let (_, ac) = build_caches(&db); let p = infer_profile(..., &db, &ac);`. Ожидаемые dominant axes должны остаться **те же** — это и есть «9.B сохраняет parity с legacy ability_vote».
- Один новый тест: `tag_axis_vote_parity_with_legacy_per_ability` — для каждой из 18 abilities в `assets/data/abilities.toml` собрать `[f32; 5]` через `tag_axis_vote(tag_cache.effective(...), def, 1.0)` и сравнить с pre-9.B `ability_vote(def)` (записать old function inline в тесте). Ожидание: **identical для ≥17 из 18**; единственное допустимое расхождение — `taunt` (теперь `Defensive | Peel`, axis vote изменится: legacy → Tank 1.0 (Myself self-buff branch); new → Tank 1.0 (Defensive branch). Identical).
  - Если расхождение появляется — pin его как expected delta в тесте, документировать в comment.
- `infer_profile_uses_override_when_set` — actor с ability `melee_attack` + override `[support]` → AxisProfile dominant = Support.

**Acceptance commit 1:**
- `git grep -n "fn ability_vote\|fn has_damage" src/` → 0 hits.
- `git grep -n "infer_profile" src/` → все callers передают 5 параметров.
- 7 + 2 новых role-теста зелёные.
- `cargo check --all-targets` зелёный.

**Объём:** ~80 LOC role.rs delta, ~60 LOC тестов, ~30 LOC cascade-edits.

---

### Commit 2 — `compute_need_signals`: `rescue_ally` + `apply_cc` producers

**Scope.** Активировать два из трёх stub'ов. `setup_aoe` остаётся `0.0` (нет Setup-механики в shape — см. spec §9 «Что НЕ в scope»).

**Ключевые сигнатуры:**

```rust
// src/combat/ai/appraisal/mod.rs
pub fn compute_need_signals(ctx: &AppraisalCtx<'_>) -> NeedSignals {
    NeedSignals {
        self_preserve:       compute_self_preserve(ctx),
        continue_commitment: compute_continue_commitment(ctx),
        finish_target:       compute_finish_target(ctx),
        reposition:          compute_reposition(ctx),
        conserve_resource:   compute_conserve_resource(ctx),
        rescue_ally:         compute_rescue_ally(ctx),
        apply_cc:            compute_apply_cc(ctx),
        setup_aoe:           0.0,  // No Setup mechanic in shape — see plan §9.B scope.
    }
}

fn compute_rescue_ally(ctx: &AppraisalCtx<'_>) -> f32 {
    // 1. Gate: actor has any ability with Rescue tag?
    let has_rescue_kit = ctx.active.abilities.iter().any(|id| {
        ctx.content.abilities.get(id).map_or(false, |def| {
            ctx.ability_tags.effective(id, def).contains_tag(AbilityTag::Rescue)
        })
    });
    if !has_rescue_kit { return 0.0; }

    // 2. Find ally in danger within reach budget.
    let reach = (ctx.active.speed.max(0) as u32).saturating_add(ctx.active.max_attack_range);
    let allies_in_danger: f32 = ctx.snap.units.iter()
        .filter(|a| a.team == ctx.active.team && a.entity != ctx.active.entity)
        .filter(|a| ctx.active.pos.unsigned_distance_to(a.pos) <= reach)
        .map(|a| {
            let hp_low = (1.0 - a.hp_pct()).clamp(0.0, 1.0);
            let threat_to_ally = ally_threat_proxy(a, ctx.snap);
            hp_low * threat_to_ally
        })
        .fold(0.0_f32, f32::max);

    ctx.tuning.curves.rescue_ally.eval(allies_in_danger)
}

/// `threat_to_ally`: max DPR среди врагов в attack range от ally,
/// нормализованный на ~10 (DPR ceiling середины игры). Reuses
/// `scoring::horizon_avg` для consistency со scoring layer.
fn ally_threat_proxy(ally: &UnitSnapshot, snap: &BattleSnapshot) -> f32 {
    snap.units.iter()
        .filter(|e| e.team != ally.team)
        .filter(|e| e.pos.unsigned_distance_to(ally.pos) <= e.max_attack_range)
        .map(|e| crate::combat::ai::scoring::horizon_avg(e))
        .fold(0.0_f32, f32::max)
        / 10.0
}

fn compute_apply_cc(ctx: &AppraisalCtx<'_>) -> f32 {
    let has_cc_kit = ctx.active.abilities.iter().any(|id| {
        ctx.content.abilities.get(id).map_or(false, |def| {
            ctx.ability_tags.effective(id, def).contains_tag(AbilityTag::ApplyCC)
        })
    });
    if !has_cc_kit { return 0.0; }

    let reach = (ctx.active.speed.max(0) as u32).saturating_add(ctx.active.max_attack_range);
    let cc_target_score = ctx.snap.units.iter()
        .filter(|e| e.team != ctx.active.team)
        .filter(|e| ctx.active.pos.unsigned_distance_to(e.pos) <= reach)
        .filter(|e| !target_already_hardcc(e, ctx.status_tags))
        .map(|e| crate::combat::ai::scoring::horizon_avg(e))
        .fold(0.0_f32, f32::max);

    // LinearClamped — explicit borders [0, 10] DPR; устойчивее к смене shape horizon_avg.
    ctx.tuning.curves.apply_cc.eval(cc_target_score)
}

fn target_already_hardcc(unit: &UnitSnapshot, cache: &StatusTagCache) -> bool {
    unit.statuses.iter().any(|st| cache.get(&st.id).contains_tag(StatusTag::HardCC))
}
```

**Tuning curves — PROVISIONAL до 9.C calibration:**

```rust
// src/combat/ai/tuning.rs::Curves
// PROVISIONAL — recalibrate in step 9.C using v30 mining corpus.
pub rescue_ally: ResponseCurve,    // input: ally_hp_low × threat_to_ally ∈ [0, 1]
pub apply_cc:    ResponseCurve,    // input: best unstunned target horizon_avg ∈ [0, 10]+

// Default()
rescue_ally: ResponseCurve::Logistic     { mid: 0.4, k: 8.0 },        // PROVISIONAL
apply_cc:    ResponseCurve::LinearClamped { x_lo: 2.0, x_hi: 10.0 },  // PROVISIONAL
```

`assets/data/ai_tuning.toml` получает раздел `[curves.rescue_ally]` / `[curves.apply_cc]` с pointer-комментарием на 9.C.

**Удаляется:** строки 56–64 в `appraisal/mod.rs` (literal `0.0` и комментарий «step 5.5 note»). `setup_aoe = 0.0` остаётся с обновлённым комментарием: «Setup механика отсутствует в shape — активируется в будущем step при введении channel/marker effect'ов».

**Cascade на single caller:**

`src/combat/ai/utility/mod.rs:235` — `compute_need_signals(...)` строит `AppraisalCtx` из `world` полей и вызывает. **Note:** в 9.A в `AiWorld` уже есть `ability_tags`; для `status_tags` — добавить аналогичное поле `pub status_tags: &'a StatusTagCache`. Cascade на `enemy_turn.rs` (Bevy system) аналогично 9.A — через `Res<StatusTagCache>`. Тестовый helper — `AppraisalCtx::for_test(...)` строится из `test_helpers::empty_caches()`.

**Тесты:**

Для каждого producer'а — 4 unit-теста (gate-on, gate-off, monotonic, edge):

| Test | Условие | Ожидание |
|---|---|---|
| `rescue_ally_zero_when_no_rescue_kit` | Actor без heal abilities | `signal == 0.0` |
| `rescue_ally_zero_when_no_allies_in_danger` | Heal в kit'е, allies full HP | `signal < 0.05` |
| `rescue_ally_high_when_ally_low_hp_threatened` | Heal в kit'е, ally 20% HP, enemy adjacent | `signal > 0.6` |
| `rescue_ally_uses_override_for_kit_check` | Ability с `ai_tags_override = ["rescue"]` | gate активен |
| `apply_cc_zero_when_no_cc_kit` | Actor без stun-likes | `signal == 0.0` |
| `apply_cc_zero_when_target_already_hardcc` | Stun в kit'е, единственный enemy уже stunned | `signal < 0.05` |
| `apply_cc_high_when_unstunned_threat_in_reach` | Stun в kit'е, healthy threat-target в reach | `signal > 0.5` |
| `apply_cc_zero_when_no_enemies_in_reach` | Stun в kit'е, enemies слишком далеко | `signal == 0.0` |

Плюс integration test:
- `compute_need_signals_no_setup_aoe_remains_zero` — explicit pin: `setup_aoe == 0.0` независимо от kit/aoe-shape.
- Старый тест `compute_need_signals_stubs_are_strictly_zero` (line 612) **переименовать** в `compute_need_signals_setup_aoe_remains_zero_after_9b` и удалить assertions для `rescue_ally`/`apply_cc`. Это intended behavioral shift.

**Acceptance commit 2:**
- `rg "rescue_ally = 0\.0\|apply_cc = 0\.0" src/combat/ai/appraisal/` → 0 hits (literals удалены).
- `setup_aoe = 0.0` присутствует с обновлённым комментарием.
- 8 + 1 новых producer-теста зелёные.
- `cargo check --all-targets` зелёный.

**Объём:** ~120 LOC appraisal delta, ~150 LOC тестов, ~10 LOC tuning, ~5 LOC AiWorld/utility cascade.

---

### Commit 3 — `repair::classify_mismatch` reads `StatusTagCache`

**Scope.** `actor_status_changed` ветка перестаёт быть hardcoded `Relevant`. Classifier видит **что именно** изменилось через diff и берёт severity по StatusTag максимума.

**Ключевые сигнатуры:**

`StatusDelta` и `compute_status_delta` уже существуют (commit 0). Здесь — wiring и priorities.

```rust
// src/combat/ai/intent.rs::PlanSnapshot
pub struct PlanSnapshot {
    // ... existing fields
    pub actor_status_hash: u64,       // fast-path, остаётся
    pub actor_statuses_at_capture: Vec<StatusId>,   // <— NEW
    // ... target fields
}

// src/combat/ai/repair/mod.rs
pub struct MismatchContext<'a> {
    pub status_delta: Option<&'a StatusDelta>,   // None when reason_code != actor_status_changed
    pub status_tags: &'a StatusTagCache,
}

pub fn classify_mismatch(
    code: &'static str,
    ctx: &MismatchContext<'_>,
) -> ContinuationSeverity {
    match code {
        "actor_rage_changed" => ContinuationSeverity::Cosmetic,
        "actor_status_changed" => ctx.status_delta
            .map(|d| classify_status_change(d, ctx.status_tags))
            .unwrap_or(ContinuationSeverity::Relevant),  // safe fallback if delta missing
        "actor_hp_drop" => ContinuationSeverity::Relevant,
        "actor_pos_mismatch" | "target_gone" | "target_entity_changed" => ContinuationSeverity::Invalidating,
        "target_hp_drop" | "target_moved" => ContinuationSeverity::Relevant,
        _ => ContinuationSeverity::Invalidating,
    }
}

fn classify_status_change(delta: &StatusDelta, cache: &StatusTagCache) -> ContinuationSeverity {
    // Priority order: HardCC/Compulsion set > Buff lost > SoftCC set > Dot tick > pure tick.
    for added in &delta.added {
        let tags = cache.get(added);
        if tags.contains_tag(StatusTag::HardCC) || tags.contains_tag(StatusTag::Compulsion) {
            return ContinuationSeverity::Invalidating;
        }
    }
    for removed in &delta.removed {
        if cache.get(removed).contains_tag(StatusTag::Buff) {
            return ContinuationSeverity::Relevant;   // потеря защиты
        }
    }
    for added in &delta.added {
        if cache.get(added).contains_tag(StatusTag::SoftCC) {
            return ContinuationSeverity::Relevant;
        }
    }
    // Pure tick (counter changed, set unchanged) → Cosmetic.
    if delta.added.is_empty() && delta.removed.is_empty() {
        return ContinuationSeverity::Cosmetic;
    }
    ContinuationSeverity::Relevant
}
```

**`PlanSnapshot::mismatch` рефакторинг:**

`mismatch()` сохраняет signature `Option<&'static str>` для backward compat. Добавляется wrapper:

```rust
impl PlanSnapshot {
    pub fn mismatch_with_delta(
        &self,
        actor: &UnitSnapshot,
        target: Option<&UnitSnapshot>,
    ) -> Option<(&'static str, Option<StatusDelta>)> {
        let code = self.mismatch(actor, target)?;
        let delta = if code == "actor_status_changed" {
            // Shared helper из commit 0 — единый источник diff-логики.
            Some(crate::combat::ai::repair::compute_status_delta(
                &self.actor_statuses_at_capture,
                &actor.statuses,
            ))
        } else { None };
        Some((code, delta))
    }
}
```

**`StoredGoalContext::check_continuation`:** аналогично — добавить `actor_statuses_at_store: Vec<StatusId>` поле, использовать **тот же** `compute_status_delta` helper. Никакого mirror'а — один утильный метод, два call-site'а.

**Cascade callers `classify_mismatch(code)` → `classify_mismatch(code, &ctx)`:**

- `src/combat/ai/intent.rs:177` (`PlanSnapshot::check_continuation`) — добавить `&StatusTagCache` параметр.
- `src/combat/ai/repair/goal.rs:175,182,189,196,205,212,218,224` — 8 call-sites; параметризовать через `&MismatchContext`.
- `src/combat/ai/repair/lifecycle.rs:25,73` — Bevy systems; пробросить `Res<StatusTagCache>`.
- `src/combat/ai/log.rs:957` — log emission; same.
- `src/combat/ai/pipeline/stages/repair_affinity.rs:29` — pipeline stage; пробросить cache через ScoringCtx.
- `src/combat/ai/repair/mod.rs:302,312,322` — testы; обновить через `MismatchContext::for_test()` helper.

**Schema:** v30 (bump в commit 0). `StoredGoalContext.actor_statuses_at_store: Vec<StatusId>` — поле v30 формата. v29 logs дают `LogError::UnsupportedSchema` (clean break, паттерн step 7→8). Pin test: `actor_tick_v29_load_yields_unsupported_schema_error`. `actor_tick_v30_round_trip` подтверждает форму.

**Тесты:**

| Test | Условие | Ожидание |
|---|---|---|
| `classify_status_change_hardcc_set_invalidates` | delta.added = [stunned] | `Invalidating` |
| `classify_status_change_softcc_set_relevant` | delta.added = [disoriented] | `Relevant` |
| `classify_status_change_buff_removed_relevant` | delta.removed = [defending] | `Relevant` |
| `classify_status_change_dot_added_relevant` | delta.added = [poisoned] | `Relevant` |
| `classify_status_change_pure_tick_cosmetic` | delta.added/removed empty (hash differs but no set change) | `Cosmetic` |
| `classify_mismatch_legacy_codes_unchanged` | Все 8 не-`actor_status_changed` кодов | severity такой же как pre-9.B |
| `plan_snapshot_actor_statuses_serde_default_empty` | v29 log без поля | `Vec::new()` |

Плюс existing test `classify_all_existing_codes_have_explicit_severity` (line 289) обновить: для `actor_status_changed` теперь принимается `MismatchContext` — тест assert'ит «без delta → Relevant fallback» и «с HardCC delta → Invalidating».

**Acceptance commit 3:**
- `rg "actor_status_changed.*Relevant" src/combat/ai/repair/mod.rs` → 0 hits (literal mapping убран).
- `classify_mismatch` body на `actor_status_changed` обращается к `ctx.status_tags`.
- 7 новых severity-тестов зелёные; legacy-тесты обновлены и зелёные.
- `cargo check --all-targets` зелёный.

**Объём:** ~80 LOC repair/mod.rs delta, ~30 LOC PlanSnapshot delta, ~50 LOC StoredGoalContext delta, ~120 LOC тестов, ~30 LOC cascade callers.

---

### Commit 4 — Smoke scenario `rescue_via_heal_in_threat` + acceptance gate green

**Scope.** Один real-log scenario доказывает rescue_ally activation pathway end-to-end: actor с heal-ability в kit'е + ally под threat → ProtectAlly intent побеждает FocusTarget. Production-код не правится.

**Почему не Peel:** в 9.B тег `Peel` живёт ТОЛЬКО в `tag_axis_vote` (role-axis bias). Нет consumer'а Peel в scoring/intent/terminal-eval. Сценарий «taunt побеждает heal через role-shift» — слишком тонкая цепочка для smoke. Real Peel-test переезжает в 9.C, когда terminal axis или critic с Peel-awareness будет добавлен.

**Сценарий:**

Playtest где support-юнит (`heal` в kit'е → AbilityTag::Rescue) находится в reach до ally на ≤30% HP, рядом с врагом. Capture:

```
tests/ai_scenarios/snapshots/rescue_via_heal_in_threat/
  log.jsonl
  p<plan_id>_support_heals_threatened_ally.expected.toml
```

Overlay format:

```toml
[scope]
plan_id = <id_from_log>

[[expectations]]
decision_kind = ["CastInPlace", "MoveAndCast"]
cast_ability = ["heal", "field_medic"]
intent_kind = ["ProtectAlly"]
# Pathway:
#   - ally HP < 30% + враг adjacent → ally_threat_proxy > 0
#   - actor имеет Rescue tag в kit → has_rescue_kit = true
#   - rescue_ally signal активирован → ProtectAlly intent score boost
#   - Pre-9.B этот сценарий выбирал FocusTarget или ProtectSelf (rescue_ally = 0).
```

**Гарантия:** assertion проверяет _только_ activation pathway. Если scenario фейлится — это либо producer broken, либо ally_threat_proxy под-калиброван (и calibration в 9.C это должна закрыть).

**Acceptance gate 9.B (полный):**

**Структурные (binary):**
1. `rg "fn ability_vote\|fn has_damage" src/combat/ai/role.rs` → 0 hits.
2. `rg "rescue_ally = 0\.0\|apply_cc = 0\.0" src/combat/ai/appraisal/` → 0 hits.
3. `classify_mismatch` body для `actor_status_changed` reads `ctx.status_tags`.
4. `cargo test --all-targets` зелёный (~30 новых тестов 9.B + smoke scenario).
5. `cargo clippy --all-targets -- -D warnings` зелёный.
6. Schema v30. v29 logs дают `LogError::UnsupportedSchema`.
7. Smoke scenario `rescue_via_heal_in_threat` зелёный.
8. `StatusTag::Compulsion` существует и активен на `taunted`.

**Числовые (mining gate на v30 corpus):**

После rebuild post-9.B corpus (~5 playtest'ов, ≥500 actor turns):

| Метрика | Bound | Что значит, если выходит |
|---|---|---|
| `rescue_ally > 0.1` в chosen plans | **5–25%** на defensive-rich kit'ах (Aldric/Lyra) | <5%: producer не ловит сцены; >25%: overfit |
| `apply_cc > 0.1` в chosen plans | **5–20%** на CC-rich kit'ах (Aldric stun) | <5%: gate broken; >20%: ApplyCC dominates |
| `actor_status_changed → Cosmetic` (на pure ticks burning/poison) | **≥40%** среди actor_status_changed events | <40%: StatusDelta path не работает или Compulsion mismatch |
| `actor_status_changed → Invalidating` (на HardCC/Compulsion set) | **≥80%** when set events occur | <80%: priority order broken |
| ai_scenarios overlay updates | **≤4 из 13** (intended shifts) | >4: что-то регрессирует, не intended; investigate |

**Ожидаемые behavioral shifts:**

| Сценарий | Pre-9.B | Post-9.B | Причина |
|---|---|---|---|
| ally low HP + actor с heal | `ProtectSelf` или `FocusTarget` | `ProtectAlly` | `rescue_ally > 0` → activated need signal |
| target healthy + actor с stun | `FocusTarget` | `ApplyCC` | `apply_cc > 0` → ApplyCC intent boost |
| burning duration tick на actor | `Relevant` → voluntary abandon возможен | `Cosmetic` → preserved | StatusDelta видит pure-tick |
| stun set на actor | `Relevant` → preserve возможен | `Invalidating` → reactive abandon | HardCC visible |
| taunted set на actor | `Relevant` (legacy) | `Invalidating` | Compulsion visible |

Snapshots `continuation_actor_hp_drop_relevant`, `continuation_cosmetic_rage_tick_no_replan` остаются pin'ed (не status changes). `continuation_setup_aoe_two_ticks` — `setup_aoe = 0.0` остаётся; пин держится.

**Acceptance commit 4:**
- Scenario `peel_via_taunt` создан и зелёный.
- 12 existing scenarios зелёные (с возможным локальным fixup overlay'ев, но **не production-кода**).
- Если какой-то scenario фейлится из-за legitimate behavioral shift — overlay updated и shift описан в commit log.

**Объём:** ~50 LOC scenario fixture (overlay + log slice), 0 production code.

---

## Тестовый план (компактно)

| # | Test category | Count | Файл |
|---|---|---|---|
| 0a | StatusTag::Compulsion derive + taunted pin update | 2 | `tags/classify.rs::tests` |
| 0b | `compute_status_delta` (added/removed/pure-tick) | 3 | `repair/mod.rs::tests` |
| 0c | Schema v30 round-trip + v29 unsupported | 2 | `log.rs::tests` |
| 1a | role parity legacy `ability_vote` vs `tag_axis_vote` | 1 (multi-row) | `role.rs::tests` |
| 1b | role real-unit dominant axes (existing, signature update) | 7 | `role.rs::tests` |
| 1c | role override propagation | 1 | `role.rs::tests` |
| 2a | `rescue_ally` producer (gate-on/off/monotonic/edge/override-empty) | 5 | `appraisal/mod.rs::tests` |
| 2b | `apply_cc` producer (gate-on/off/monotonic/edge) | 4 | `appraisal/mod.rs::tests` |
| 2c | `setup_aoe` стабильно 0.0 post-9.B | 1 | `appraisal/mod.rs::tests` |
| 3a | `classify_status_change` per StatusTag (HardCC/Compulsion/SoftCC/Buff-loss/Dot/pure-tick) | 6 | `repair/mod.rs::tests` |
| 3b | `classify_mismatch` legacy codes unchanged | 1 | `repair/mod.rs::tests` |
| 3c | StoredGoalContext shared helper integration | 1 | `repair/goal.rs::tests` |
| 4 | Smoke scenario `rescue_via_heal_in_threat` | 1 | `tests/ai_scenarios/snapshots/rescue_via_heal_in_threat/` |
| **Total new** | | **~35 тестов** | |

**Не делать pin'ов на каждую ability/status separately** — это уже сделано в 9.A (52 pins). Здесь pin'им только **mappings**: tag→axis (table выше), StatusTag→Severity (6 cases выше).

---

## Open questions

1. **`rescue_ally` formula refinement.** Initial `(1 - hp_pct) × ally_threat_proxy` через max DPR от врагов в attack range до ally. 9.C mining покажет, нужна ли `enemies_targeting_ally` heuristic (учёт того, кого враг сейчас собирается атаковать). Сейчас — minimal viable.
2. **`apply_cc` normalization.** Перешли на `LinearClamped { x_lo: 2.0, x_hi: 10.0 }` — explicit borders менее хрупки чем magic `/10.0`. 9.C mining уточнит границы.
3. **Override-empty gate behaviour.** `ability_tags.effective(id, def)` — если override = `Some(vec![])`, тег не считается. Intended (replace-not-append). Тест `rescue_ally_zero_when_override_empties_kit` фиксирует.
4. **`taunted` Compulsion priority.** Compulsion идёт в одном priority-блоке с HardCC (Invalidating). Альтернатива — отдельный приоритет (Compulsion=Relevant) для случая когда taunt от ally себе же (например, redirect на актёра — это не нарушение цели). Сейчас выбран max-strict вариант. Если mining показывает overcautious abandon — пересмотр в 9.C.
5. **`compute_status_delta` allocation.** Vec для added/removed — fresh per-call. В hot loop (`mismatch_with_delta` вызывается каждый tick от continuation check) может стоить. Bench в 9.C; альтернатива — `SmallVec<[StatusId; 4]>` если actually shows up.

---

## Risk register

| Risk | L | S | Mitigation |
|---|---|---|---|
| Cascade `infer_profile` касается ≥4 файлов вне AI-модуля | M | M | `cargo check` после сигнатуры показывает каждый callsite. 4 cs-та (`combat/phases.rs`, `combat/spawn.rs`, `scenario/combat_scene.rs:60+86`); все имеют `Res<AbilityTagCache>` доступ. |
| `compute_status_delta` allocates Vec per-tick | L | L | Vec'ы статусов — ≤5 элементов; linear scan через `iter().any()` дешевле HashSet. Optimize только при bench. |
| Schema v30 corpus invalidation для пользовательских v29 logs | L | M | Clean break как в step 7→8; `actor_tick_v29_load_yields_unsupported_schema_error` тест pin'ит ошибку. Mining baseline rebuild требуется в 9.C. |
| Behavioral shifts ломают много existing ai_scenarios | M | M | Acceptance gate ограничивает ≤4 overlay-update'а из 13. >4 → investigate. Shifts — intended; overlay update OK, production code — нет. |
| Smoke scenario не находится в существующих logs | M | L | Создать playtest с support-юнитом (heal в kit'е) + ally на 30% HP + adjacent enemy. Если не получается за 1 час — переезжает в 9.C, gate 7 снимается. |
| Per-tick AI cost +5–15% от двух новых producer'ов | L | L | Каждый producer: 1 cache-lookup на ability × O(allies/enemies в reach). Раньше 0. Если profile показывает >20% — investigate (cache override-resolution в hot loop). |
| Provisional curves дают patологические distributions | M | M | Числовые gates (5–25% rescue_ally activation) ловят. Если выходим — корректировка midpoint/k в той же коммитной серии 9.B, без откладывания на 9.C. |

---

## Финальная таблица коммитов

| # | Title | Estimate | Files | New tests |
|---|---|---|---|---|
| 0 | Foundation: StatusTag::Compulsion + AppraisalCtx + StatusDelta helper + schema v30 | 0.5 дня | `tags/{mod,classify}.rs`, `appraisal/mod.rs`, `repair/mod.rs`, `log.rs` | 6 |
| 1 | role::infer_profile tag-driven (`ability_vote`/`has_damage` removed) | 0.5–0.75 дня | `role.rs`, `combat/phases.rs`, `combat/spawn.rs`, `scenario/combat_scene.rs` | 9 (7 updated + 2 new) |
| 2 | compute_need_signals: rescue_ally + apply_cc producers via AppraisalCtx | 1.0 дня | `appraisal/mod.rs`, `tuning.rs`, `utility/mod.rs`, `enemy_turn.rs`, `assets/data/ai_tuning.toml` | 9 |
| 3 | classify_mismatch reads StatusTagCache + actor_statuses fields v30 | 1.0 дня | `repair/mod.rs`, `intent.rs`, `repair/goal.rs`, `repair/lifecycle.rs`, `pipeline/stages/repair_affinity.rs`, `log.rs` | 8 |
| 4 | Smoke scenario `rescue_via_heal_in_threat` + overlay updates | 0.5 дня | `tests/ai_scenarios/snapshots/rescue_via_heal_in_threat/*` (+ ≤4 existing overlay'я) | 1 + ≤4 updates |
| **Total** | | **~3.5 дня** (spec оценивал 2.0–2.5; верхняя граница оправдана scope-патчем StatusTag и shared helpers) | | **~33 new** |

---

## Что НЕ делается в 9.B

- `setup_aoe` activation — out of scope (нет Setup-механики; в spec явно).
- 5 mining scenarios (`rescue_ally_via_heal_tag`, etc.) — это 9.C.
- Mining калибровка curves (`rescue_ally`, `apply_cc` coefficients) — 9.C, после rebuild v29 corpus.
- Documentation update в `docs/ai.md` («tag-driven inference») — 9.C cleanup.
- Удаление `compute_factors` references — уже сделано в pre-9 шагах (выходит за scope).

---

### Critical Files for Implementation

- `/Users/splav/personal/storyforge/src/combat/ai/tags/{mod,classify}.rs` — commit 0: `StatusTag::Compulsion` + `forces_targeting` derive.
- `/Users/splav/personal/storyforge/src/combat/ai/role.rs` — commit 1: `infer_profile` сигнатура (`+&AbilityTagCache`), `tag_axis_vote` inline, `ability_vote`/`has_damage` removed.
- `/Users/splav/personal/storyforge/src/combat/ai/appraisal/mod.rs` — commit 0: `AppraisalCtx<'a>` struct; commit 2: `compute_rescue_ally`/`compute_apply_cc` через `AppraisalCtx`.
- `/Users/splav/personal/storyforge/src/combat/ai/repair/mod.rs` — commit 0: `StatusDelta` + `compute_status_delta` shared; commit 3: `MismatchContext`, `classify_mismatch(+&ctx)`, `classify_status_change`.
- `/Users/splav/personal/storyforge/src/combat/ai/intent.rs` — commit 3: `PlanSnapshot.actor_statuses_at_capture`, `mismatch_with_delta`.
- `/Users/splav/personal/storyforge/src/combat/ai/repair/goal.rs` — commit 3: `StoredGoalContext.actor_statuses_at_store`, использует shared `compute_status_delta`.
- `/Users/splav/personal/storyforge/src/combat/ai/log.rs` — commit 0: `SCHEMA_VERSION = 30`.
