# Шаг 10 — `PlanCritic` набор (декомпозиция sanity): декомпозиция на сабшаги

Декомпозиция в стиле step 7/8/9: коммит-на-сабшаг, gate на каждом.
Спецификация: `docs/ai_rework.md` §10 (строки 309-324).

## Preamble

### Текущее состояние

`src/combat/ai/planning/sanity.rs` — 732 строки, монолитный `sanity_adjust_plans` (`:85-232`). Внутри один цикл по плану с 7 inline-правилами через `SanityRule` enum (`:30-45`):

| # | Rule | Логика | Источник | Маппинг (см. развилку 1) |
|---|---|---|---|---|
| 1 | `Survival` | low-HP × `worst_path_danger`² | `tuning.thresholds.low_hp_factor` | → `OvercommitIntoDanger` |
| 2 | `HealerExposure` | non-healer уходит от unguarded healer | inline 0.5 | → residual |
| 3 | `LosBlindspot` | RANGED закончил без LoS на enemies | inline 0.3 | → `BlindspotRanged` |
| 4 | `RetreatTrap` | <2 open neighbours на final_pos | inline 0.5 | → residual |
| 5 | `SelfAoe` | friendly-fire AoE накрывает caster'а | inline 0.5 | → `SelfLethalWithoutPayoff` |
| 6 | `AoOBleed` | Move провоцирует AoO с reactions | `tuning.thresholds.aoo_penalty_k` | → `OvercommitIntoDanger` |
| 7 | `SynergyBonus` | retreat → safer + useful cast | inline 1.1 | → residual (это бонус, не штраф) |

Pipeline-обёртка — `SanityStage` (`pipeline/stages/sanity.rs`, 148 строк). Простая: считывает scores из `pool.annotations[i].score`, прогоняет через `sanity_adjust_plans`, пишет hits в `pool.annotations[i].sanity`.

`PlanAnnotation.sanity: Vec<SanityHit>` (`outcome/mod.rs:173`) — плоский массив `{rule: SanityRule, multiplier: f32}`. Hit = «сработавшая проверка»; правило с пустым vec означает «не сработало». Сериализуется в schema v30 как часть `actor_tick`.

Также в `sanity.rs` живут: `apply_protect_self_mask` (`:337-359` — hard mask, не soft penalty), `expected_aoo_damage` (`:246-282` — helper, используется адаптацией), `plan_has_self_aoe` (`:284-297`), `plan_has_useful_cast` (`:299-311`), `plan_is_defensive` (`:319-321`).

### Проблемы текущей схемы

1. **Семантика штрафа теряется в `(rule, multiplier)`** — нет structured reason'а; debug через regrep.
2. **Один 150-строчный цикл** смешивает 3-строчные правила (`LosBlindspot`) с 35-строчными (`AoOBleed`); per-rule unit-тест невозможен без mocking всего setup'а (`breakdown_reports_survival_and_aoo_bleed`, `:651`).
3. **Расширение — туго.** Новые правила из мастер-плана (`BuffIntoVoid`, `RareResourceForLowImpact`, `HealWithoutRescueValue`) пришлось бы вписывать в тот же монолит, обращаясь к 4 слоям (`outcome` / `terminal` / `policy` / snapshot).
4. **Generic vs targeted смешано** — `HealerExposure` / `RetreatTrap` / `SynergyBonus` это generic эвристики, а `Survival` / `AoOBleed` / `LosBlindspot` — targeted error-class checks. Стратегии тестирования и тюнинга разные.

### Что закрывает шаг 10

1. **Trait `PlanCritic`** в `src/combat/ai/critics/`: `evaluate(plan, annotation, ctx) -> Option<CriticHit>`. Каждый critic — отдельный файл и unit-тесты.
2. **6 critics первой волны** (1 из 7 мастер-плана отложен — см. развилку 7), 3 кластерами по сабшагам.
3. **`PlanAnnotation.critics: Vec<CriticHit>`** — structured log; `sanity: Vec<SanityHit>` shrink'ается до 3 residual правил.
4. **`CriticsStage`** в `pipeline/stages/critics.rs` — один stage, диспатч в `critics/`. Запускается после `SanityStage` (residual), до `AdaptationStage`.
5. **Schema v30→v31 atomic в финальном сабшаге** — clean break.
6. **`apply_protect_self_mask` / `expected_aoo_damage` / `plan_is_defensive`** остаются в sanity.rs — hard mask + helpers, не critics.

### Что НЕ в scope шага 10

- **Bands/agenda/scorecard (step 11)** — critics не участвуют в band selection; `Flag`-only (observability без штрафа) тоже step 11.
- **Mid-plan reflow (step 12)** — critics post-scoring one-shot.
- **Adaptation switches от critics** — adaptation остаётся separate (master plan invariant 2).
- **TOML configuration критиков** — composition в коде (step 7 invariant).
- **`ZoneOverlapWaste`** — backlog на step 17 (нет zone-абилок в content).

## Зафиксированные решения по развилкам

**1. Mapping SanityRule → critics — частичное.** 4 → critics, 3 → residual sanity.

- **OvercommitIntoDanger** = `Survival` + `AoOBleed` объединены (один класс «перевыставление навстречу урону»).
- **SelfLethalWithoutPayoff** ⊃ `SelfAoe` (SelfAoe — частный случай). Шире: sum self-damage / sum payoff (kills + ally rescues).
- **BlindspotRanged** = `LosBlindspot` 1:1.
- **HealerExposure** / **RetreatTrap** / **SynergyBonus** — generic, не critics из мастер-плана; остаются как residual в `sanity.rs`.

**Альтернатива (1b):** растворить residual в `PlanModifiersStage`. Отвергнута: PlanModifiers — для post-composition bonuses (summon/trade/repair), не для general penalties.

**2. Schema bump v30→v31 atomic в 10.4 (clean break).** `PlanAnnotation.critics` добавляется; `sanity` shrink'ается до 3 residual rules; SanityRule enum теряет 4 варианта. v30 logs дают `LogError::UnsupportedSchema`. По аналогии с шагом 8.

**Альтернатива (2b):** `#[serde(default)]` migration. Отвергнута: SanityRule enum в любом случае ломается.

**3. `CriticHit` — struct, `Pass` = отсутствие entry, `Flag` — out of scope.**

```rust
pub struct CriticHit { pub critic: CriticKind, pub multiplier: f32, pub reason: CriticReason }
```

`evaluate -> Option<CriticHit>`: `None` = pass. `Flag(reason)` (observability без штрафа) — step 11 territory.

**Альтернатива (3b):** enum `CriticResult { Pass, Penalize(...), Flag(...) }`. Отвергнута: Pass не сериализуется, Flag не в scope.

**4. Один `CriticsStage` с `Vec<Box<dyn PlanCritic>>`, composition в коде.**

`CriticsStage::first_wave()` — hardcoded 6 критиков. Per step 7 invariant: pipeline composition в коде, не в TOML.

**Альтернатива (4b):** stage per critic. Отвергнута: 6 stages для симметричных проверок — overengineering.
**Альтернатива (4c):** static dispatch через enum (как `StepFactor` в step 8). Отвергнута: критики ожидаются добавляться часто (вторая волна, encounter-specific) — trait устойчивее.

**5. Multiplicative penalty (`score *= multiplier`).** Сохраняем существующую sanity-семантику; multiplicative invariant к scale-tuning'у. Per-critic floor по аналогии с `survival_floor`.

**Альтернатива (5b):** additive. Отвергнута: ломает scale invariance.

**6. Порядок critics — fixed code-order:** defensive → positioning → resource/value (наиболее «фатальные» — раньше).

**7. `ZoneOverlapWaste` → backlog на step 17.** В content нет zone-абилок (channel/marker/hazard); critic был бы no-op до step 17. Stub без сценария — мёртвый код.

**8. `critics/` — отдельная директория** (зеркалит `factors/`/`policy/`/`tags/`). `CriticsStage` живёт в `pipeline/stages/critics.rs` и только диспатчит — domain rules отделены от pipeline-инфраструктуры.

## Природа gate'ов

Step 10 — рефактор + расширение. Behavioral invariants по сабшагам:

- **10.0** (scaffolding): golden 0/N. Никто из critics не активен.
- **10.1-10.3** (extraction + new critics): golden может расходиться **в пределах допустимой дельты**. Расхождение — на тех планах, где новый critic активируется (например, `HealWithoutRescueValue` на бесполезных хилах). Per-сабшаг review золотых diffs: каждое расхождение должно быть атрибутируемо к новому critic'у. Допустимо ≤15% planов на каждый сабшаг.
- **10.4** (cleanup + schema bump): v31 round-trip 0/N на rebuilt corpus. SanityRule enum shrink — старые варианты исчезают. mining-секция G1 (critics coverage) воспроизводится.

Per-критик unit-тесты в `critics/<name>.rs::tests`:
- `*_fires_on_canonical_case` — minimal сетап с явно ожидаемым hit.
- `*_passes_on_clean_plan` — нет hit при отсутствии триггера.
- `*_severity_scales_with_input` — multiplier монотонен по входному сигналу.

ai_scenarios fixture per critic (созданные в 10.1-10.3) — assertions на `critics[].critic == <kind>`.

## Сабшаги

### 10.0. Scaffolding: trait + types + empty `CriticsStage`

**Scope.**

`src/combat/ai/critics/mod.rs` — trait + types:

```rust
pub trait PlanCritic: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(&self, plan: &TurnPlan, ann: &PlanAnnotation, ctx: &ScoringCtx) -> Option<CriticHit>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticKind { OvercommitIntoDanger, SelfLethalWithoutPayoff, BlindspotRanged,
    BuffIntoVoid, RareResourceForLowImpact, HealWithoutRescueValue }

pub struct CriticHit { pub critic: CriticKind, pub multiplier: f32, pub reason: CriticReason }

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CriticReason { /* per-critic варианты заполняются в 10.1-10.3 */ }
```

`PlanAnnotation.critics: Vec<CriticHit>` (`#[serde(default)]`). `pipeline/stages/critics.rs::CriticsStage` — диспатч-цикл по `Vec<Box<dyn PlanCritic>>`. Регистрация `CriticsStage::first_wave()` (пустой vec) в `pick_action` после `SanityStage`.

**Юнит-тесты:** `critics_stage_no_op_when_empty`, `plan_annotation_critics_default_empty`, `critics_stage_writes_hit_and_multiplies_score` (mock critic).

**Gate.** `cargo test/clippy/build/ai_scenarios` зелёные. Golden **0/N**.

**Эстимейт:** 0.5 дня.

---

### 10.1. Defensive cluster: `OvercommitIntoDanger` + `SelfLethalWithoutPayoff`

**Scope.**

`critics/overcommit_into_danger.rs`:
- Источники: `Survival` rule (low_hp × max_path_danger²) + `AoOBleed` rule (`expected_aoo_damage` / actor.hp).
- Логика: combined penalty = max из двух (не произведение — это уже multiplicative invariant'а cluster'а).
- `CriticReason::OvercommitIntoDanger { source: SurvivalPath | AooBleed, ratio: f32 }`.

`critics/self_lethal_without_payoff.rs`:
- Источники: `plan_has_self_aoe` + sum of `outcome.self_damage` across steps + sum payoff (`outcome.enemy_damage` + `outcome.p_kill_now`).
- Логика: если self_damage > 0.3 × actor.max_hp И payoff < threshold — penalty proportional to (self_damage / max_hp).
- `CriticReason::SelfLethalWithoutPayoff { self_dmg_ratio: f32, payoff_estimate: f32 }`.

В `sanity_adjust_plans` отключить `Survival`/`AoOBleed`/`SelfAoe` ветки — заменены critic'ами. Удалить соответствующие SanityRule варианты ОТЛОЖЕНО до 10.4 (атомарный schema bump).

`expected_aoo_damage` и `plan_has_self_aoe` остаются в `sanity.rs` как `pub(crate)` helpers — используются и адаптацией, и новыми critics.

Ai_scenarios:
- `overcommit_into_danger_low_hp_corridor` — low-HP актор пытается пройти через danger>0.5 коридор.
- `self_lethal_without_payoff_self_aoe` — self-AoE с минимальными попаданиями по врагам.

**Юнит-тесты:** 6 (3 на критик).

**Gate.** Golden review per-entry. Допустимо ≤15% diff'ов, каждый атрибутируется к новому critic'у.

**Эстимейт:** 1.5 дня.

---

### 10.2. Positioning cluster: `BlindspotRanged`

**Scope.**

`critics/blindspot_ranged.rs`:
- Источник: текущий `LosBlindspot` rule, перенос 1:1.
- Логика: actor.tags.contains(RANGED) && нет enemies в LoS из final_pos.
- `CriticReason::BlindspotRanged { enemies_visible: 0 }`.

В `sanity_adjust_plans` отключить `LosBlindspot` ветку (удаление варианта — в 10.4).

ai_scenarios: `blindspot_ranged_los_broken_by_ally`.

**Юнит-тесты:** 3.

**Gate.** Golden ≤5% diff'ов (LosBlindspot был precise; перенос должен быть byte-identical в большинстве случаев).

**Эстимейт:** 0.5 дня.

---

### 10.3. Resource/value cluster: `BuffIntoVoid` + `RareResourceForLowImpact` + `HealWithoutRescueValue`

**Scope.** Три новых critic'а — нет input'а из существующего sanity.

`critics/buff_into_void.rs`:
- Логика: Cast step с status на ally, у которого тот же status уже активен (или будет от другой плановой ability'и). Источник: `ann.outcomes[i].status_turns_applied` + lookup `target.statuses` через snapshot.
- `CriticReason::BuffIntoVoid { ability: String, target_already_buffed: bool }`.

`critics/rare_resource_for_low_impact.rs`:
- Логика: Cast с mana_cost ≥ threshold (например ≥30) и outcome.enemy_damage < 0.5 × ability.expected_damage. «Дорогой каст с низкой эффективностью».
- `CriticReason::RareResourceForLowImpact { ability: String, cost: u8, impact_ratio: f32 }`.

`critics/heal_without_rescue_value.rs`:
- Логика: heal-cast на ally, у которого hp_pct > 0.7 И `ctx.maps.danger.get(ally.pos) < 0.3`. «Лечим того, кто не нуждается».
- Источник: outcome.healing_done + target snapshot.
- `CriticReason::HealWithoutRescueValue { target_hp_pct: f32, target_danger: f32 }`.

ai_scenarios:
- `buff_into_void_haste_on_already_hasted_ally`.
- `rare_resource_for_low_impact_fireball_on_single_low_hp`.
- `heal_without_rescue_value_full_hp_safe_ally`.

**Юнит-тесты:** 9.

**Gate.** Golden review. Ожидаемо ≤15% diff'ов — это новые критики, они должны fire'ить на старых corpus'ах.

**Эстимейт:** 2.0 дня.

---

### 10.4. Cleanup + schema v30→v31 + mining

**Scope.**

- Удалить из `SanityRule` варианты `Survival`/`AoOBleed`/`LosBlindspot`/`SelfAoe`.
- `sanity_adjust_plans` shrink'ается до 3 правил: HealerExposure, RetreatTrap, SynergyBonus. Целевой объём `sanity.rs` после shrink: ~250 строк.
- Schema v30→v31: `PlanAnnotation.critics` сериализуется. v30 logs дают `LogError::UnsupportedSchema` (clean break).
- `bin/replay_ai_log.rs` / `bin/mine_ai_logs.rs` обновляются: parsing v31 only.
- Mining-секция **G1** (critics coverage) в `mine_ai_logs.rs`: per-critic frequency, distribution multiplier, attribution to chosen plans.
- Документация: `docs/ai.md` секция «Critics layer» (новая, аналогично «Tag layer» step 9).

**Юнит-тесты.** Round-trip v31, mining G1 voiceprint.

**Gate.**
- `cargo test --lib` все зелёные.
- v31 round-trip 0/N на rebuilt corpus.
- mining G1 показывает все 6 critics с non-zero coverage на rebuilt corpus.

**Эстимейт:** 1.0 день (schema + mining + docs).

---

## Итого

| # | Шаг | Эстимейт | Gate |
|---|---|---|---|
| 10.0 | Scaffolding (trait + types + empty CriticsStage) | 0.5 | golden 0/N, no behavior |
| 10.1 | Defensive cluster (OvercommitIntoDanger + SelfLethalWithoutPayoff) | 1.5 | golden ≤15%, per-entry attributed |
| 10.2 | Positioning cluster (BlindspotRanged) | 0.5 | golden ≤5% (1:1 port) |
| 10.3 | Resource/value cluster (BuffIntoVoid + RareResourceForLowImpact + HealWithoutRescueValue) | 2.0 | golden ≤15%, per-entry attributed |
| 10.4 | Cleanup + schema v30→v31 + mining G1 | 1.0 | v31 round-trip 0/N, mining G1 voiceprint |

**Суммарно ~5.5 дня.**

## Критические файлы

- `src/combat/ai/critics/mod.rs` (новый) — trait + types.
- `src/combat/ai/critics/{overcommit_into_danger, self_lethal_without_payoff, blindspot_ranged, buff_into_void, rare_resource_for_low_impact, heal_without_rescue_value}.rs` (новые).
- `src/combat/ai/pipeline/stages/critics.rs` (новый) — CriticsStage.
- `src/combat/ai/pipeline/mod.rs` — registration в pick_action.
- `src/combat/ai/outcome/mod.rs` — `PlanAnnotation.critics` field.
- `src/combat/ai/planning/sanity.rs` — shrink до ~250 строк (residual + helpers + mask).
- `src/combat/ai/log.rs` — schema v30→v31 в 10.4.
- `src/bin/{replay_ai_log,mine_ai_logs}.rs` — v31 parsing + mining G1.
- `tests/ai_scenarios/snapshots/critic_*` — 6 новых fixtures.
- `docs/ai.md` — секция Critics layer.

## Что откладывается

- **`ZoneOverlapWaste`** — backlog на step 17 (geometry).
- **`Flag`-only critics** (observability без штрафа) — backlog на step 11 (band/agenda).
- **Adaptation hooks для critics** — backlog (master plan invariant 2: critics ≠ adaptation).
- **TOML thresholds + веса** — захардкожено в первой волне; миграция в `AiTuning` — backlog после mining-калибровки.
- **Вторая волна critics** (encounter-specific, anti-meta) — backlog на step 14.

## Чего не делать в шаге 10

- **Не трогать `apply_protect_self_mask`** — hard mask за рамками critics (master plan invariant 4).
- **Не удалять `SanityRule` enum** — shrink до 3 residual вариантов, не remove.
- **Не консолидировать residual sanity в один critic** — содержательно не related.
- **Не делать critics зависящими друг от друга** — каждый читает только plan + annotation + ctx; order не семантичен (modulo sanity-режим accumulation).
- **Не делать critics async/parallel** — sync sequential по дизайну (master plan).
