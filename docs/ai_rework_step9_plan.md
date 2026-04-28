# Шаг 9 — Semantic AI tags для способностей и статусов

Декомпозиция на 3 сабшага. Спецификация: `docs/ai_rework.md` §9.

## Preamble

### Текущее состояние (после step 8)

- `role::ability_vote` (`src/combat/ai/role.rs:240–296`) — структурная эвристика 5 if-else веток с magic-multiplers (`weight * 0.7 / 0.4 / 0.3`); единственный production classifier способностей в 5-axis space (tank/melee/ranged/control/support).
- `appraisal::compute_need_signals` (`src/combat/ai/appraisal/mod.rs:62`) — три need-сигнала прибиты к `0.0`: `rescue_ally`, `apply_cc`, `setup_aoe`. Не активируются: outcome facts не различают «новый CC» от «duration tick», «heal-rescue» от «heal-maintenance».
- `repair::classify_mismatch` (`src/combat/ai/repair/mod.rs:55–88`) — `"actor_status_changed" => Relevant` захардкожено. Не отличает HardCC-set от Dot-tick.
- Невыводимые семантики (Peel, Escape, ZoneControl, Setup, Cleanse) — нет ни shape-проверки, ни outcome facts, ни тегов; добавление способности с такой семантикой требует расширения consumer-кода.
- `BuffClass` (`src/content/statuses.rs:7–13`) — subkind для saturation-tracking (Haste/ArmorBuff/DamageUp/Shield), не AI semantic.
- Schema v29 (post step 8 clean break).

### Проблемы

1. **Production-classifier способностей — структурная эвристика** в `ability_vote`. Любое расширение требует нового if'а; пограничные случаи (taunt как Peel, knockback как peel) выпадают.
2. **Need signals rescue/cc/aoe — мёртвые стабы.** Step 5.5 и step 7.5 целенаправленно оставили хук на step 9.
3. **Repair severity на actor_status_changed — too coarse.** Burning duration tick и Stun set одинаково триггерят Relevant.
4. **Открытые семантики недоступны.** AI не различает peel vs damage, escape vs aggression — нет ни сигнала, ни хука.

### Что закрывает step 9

1. **AI-side classifier**: `derive_ability_tags(&AbilityDef) -> AbilityTagSet`, `derive_status_tags(&StatusDef) -> StatusTagSet`. Single source of truth — shape остаётся authoritative; теги = выведенная проекция, кэшированная.
2. **Tags реализуют 7 derived ability-семантик**: Offensive, Defensive, Rescue, Summon, Mobility, ApplyCC, Peel — все выводятся из существующих shape-полей.
3. **Tags реализуют 5 derived status-семантик**: HardCC, SoftCC, Dot, Buff, Cosmetic — все выводятся из существующих status-полей.
4. **`role::ability_vote` removed** из production-пути `infer_profile`. Чтение → `tag_cache`. Mapping `tag → role-axis bias` живёт в одном месте.
5. **`compute_need_signals` стабы активированы**: `rescue_ally`, `apply_cc` — producer'ы читают теги. `setup_aoe` остаётся 0.0 (механики Setup нет в shape; активация — отдельный future-step).
6. **`classify_mismatch` actor_status_changed** читает StatusTag диффа: `HardCC set` → Invalidating, `Dot tick` / `Buff tick` → Cosmetic, остальное → Relevant.
7. **Override-механизм** для пограничных случаев: `AbilityFile.ai_tags_override: Option<Vec<String>>` — replaces derived (не append). Initial usage: 0–2 случая.
8. **PlanAnnotation diagnostics**: `effective_ai_tags: Vec<Vec<AbilityTag>>` per Cast step (cache lookup, без consumer logic). Schema additive, без bump.

### Что НЕ в scope

- **Тег Setup** — нет механики (channel/marker/stage в shape). Когда механика появится, активируется derived. Сейчас setup_aoe need остаётся 0.0.
- **Тег Cleanse** — нет механики (нет remove-debuff effect).
- **Тег ZoneControl** — нет persistent hazard zones.
- **Тег Finisher** — это scoring property `outcome.kill_promised`, уже работает через terminal axis `secure_kill`. Не tag.
- **Тег Escape** — это intent context, не свойство механики. Активируется через `appraisal::self_preserve` модуляцию + Mobility tag в kit'е (не отдельный тег).
- **Тег `commitment_skill`** — нужен step 12 (mid-plan reflow для multi-turn модели). Без инфраструктуры — мёртвый класс.
- **Decomposition critics** (step 10).
- **Bands+agenda+scorecard** (step 11).
- **TOML schema rewrite** для весов role/need.

### Зафиксированные решения

| # | Решение | Альтернатива (отвергнутая) |
|---|---|---|
| 1 | Tags = AI-side classifier (`derive_*_tags`) над shape, не TOML data layer | Тегирование контента вручную (drift между shape и tags) |
| 2 | Closed enum для AbilityTag и StatusTag | String (опечатки, silent regression) |
| 3 | Set без приоритета (`AbilityTagSet`) | Priority list (PR'ы трудно ревьюить, primary редко однозначен) |
| 4 | Override через `ai_tags_override: Option<Vec<String>>` (replace, не append) | Append-семантика (двусмысленно: derived всё ещё активен?) |
| 5 | `BuffClass` остаётся как refinement в StatusTag::Buff (saturation), не сливается со StatusTag | Единый StatusKind enum (смешивает ortogональные dimensions) |
| 6 | Hint/Contract — свойство consumer'а, не тега. Документируется в consumer docs | Runtime-флаг на теге (нечего проверять — это convention) |
| 7 | tag → role/outcome mapping — explicit Rust code, не TOML | TOML override (premature; difficulty/encounter override — backlog) |
| 8 | `effective_ai_tags` per-step (не per-plan) | Per-plan агрегация (теряет инфо для plan'ов с несколькими casts) |
| 9 | Schema additive без bump (`#[serde(default)]` на новом поле) | Bump v29→v30 (нет deletion, не оправдано) |
| 10 | 7 ability tags + 5 status tags. Setup/Cleanse/ZoneControl/Finisher/Escape/commitment — out of scope (см. выше) | 12 ability tags из spec'а (5 — мёртвые классы / не теги) |

---

## Сабшаг 9.A — Tag layer + classifier (~1.5–2 дня)

**Scope.** Foundation: enums, classifier, cache, override-поле, effective_ai_tags writeback. **Поведение AI не меняется** — теги пишутся в annotation, ни один production consumer их пока не читает. Golden 0/N.

### Изменения

**1. Новый модуль `src/combat/ai/tags/`:**

```
tags/
  mod.rs        — AbilityTag, StatusTag enums; AbilityTagSet, StatusTagSet
  classify.rs   — derive_ability_tags(&AbilityDef) -> AbilityTagSet
                — derive_status_tags(&StatusDef) -> StatusTagSet
  cache.rs      — AbilityTagCache, StatusTagCache (Bevy Resources)
```

**2. Enum'ы:**

```rust
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AbilityTag {
    Offensive,    // damage-семантика
    Defensive,    // self/ally protection
    Rescue,       // heal/extract ally в danger
    Summon,       // призыв юнита
    Mobility,     // даёт движение
    ApplyCC,      // накладывает контроль
    Peel,         // taunt-redirect или знаковое перенаправление угрозы
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum StatusTag {
    HardCC,       // skips_turn
    SoftCC,       // causes_disadvantage / speed_bonus<0
    Dot,          // dot_dice / hp_percent_dot
    Buff,         // buff_class.is_some / armor_bonus>0
    Cosmetic,     // negative class — ничего из перечисленного
}
```

**3. Sets — bitset для compactness:**

```rust
pub struct AbilityTagSet(u8);  // 7 тегов → влезает в u8
pub struct StatusTagSet(u8);   // 5 тегов

impl AbilityTagSet {
    pub fn empty() -> Self;
    pub fn from_iter<I: IntoIterator<Item = AbilityTag>>(i: I) -> Self;
    pub fn contains(self, t: AbilityTag) -> bool;
    pub fn insert(&mut self, t: AbilityTag);
    pub fn iter(self) -> impl Iterator<Item = AbilityTag>;
}
// аналогично StatusTagSet
```

**4. Classifier `tags/classify.rs`:**

```rust
pub fn derive_ability_tags(def: &AbilityDef) -> AbilityTagSet {
    let mut s = AbilityTagSet::empty();

    // Offensive: any damage effect
    if matches!(def.effect, EffectDef::WeaponAttack | EffectDef::Damage{..} | EffectDef::SpellDamage{..}) {
        s.insert(AbilityTag::Offensive);
    }

    // Rescue: heal on ally
    if def.target_type == TargetType::SingleAlly && matches!(def.effect, EffectDef::Heal{..}) {
        s.insert(AbilityTag::Rescue);
    }

    // Defensive: self/ally protection через armor_bonus в applied statuses (требует status data)
    // → пройти по def.statuses.on=Self|Target=Ally + content lookup
    // Реализуется через `derive_with_status_ctx` (см. ниже).

    // Summon
    if matches!(def.effect, EffectDef::Summon{..}) { s.insert(AbilityTag::Summon); }

    // Mobility
    if matches!(def.effect, EffectDef::GrantMovement{..}) { s.insert(AbilityTag::Mobility); }

    // ApplyCC: any status[on=Target] с StatusTag::HardCC|SoftCC
    // → пересечение с status_tag_cache; реализуется через `derive_with_status_ctx`.

    // Peel: forces_targeting на target — taunt-redirect
    // → status data lookup.
    s
}

pub fn derive_status_tags(def: &StatusDef) -> StatusTagSet {
    let mut s = StatusTagSet::empty();
    if def.skips_turn { s.insert(StatusTag::HardCC); }
    if def.causes_disadvantage || def.speed_bonus < 0 { s.insert(StatusTag::SoftCC); }
    if def.dot_dice.is_some() || def.hp_percent_dot > 0 { s.insert(StatusTag::Dot); }
    if def.buff_class.is_some() || def.armor_bonus > 0 { s.insert(StatusTag::Buff); }
    if s.is_empty() { s.insert(StatusTag::Cosmetic); }
    s
}
```

**Контракт:** `derive_ability_tags` зависит от `StatusTagCache` (для Defensive/ApplyCC/Peel). Реализуется как двухпроходный builder: status'ы первыми, abilities — вторыми с уже готовым cache'ом.

**5. Cache как Bevy Resources `tags/cache.rs`:**

```rust
#[derive(Resource, Default)]
pub struct AbilityTagCache {
    map: HashMap<AbilityId, AbilityTagSet>,
}

#[derive(Resource, Default)]
pub struct StatusTagCache {
    map: HashMap<StatusId, StatusTagSet>,
}
```

Заполнение — Bevy startup system после загрузки `ContentDb`. Lookup: `cache.get(id) -> AbilityTagSet`.

**6. Override-поле в content:**

```rust
// AbilityFile (TOML deserialize)
#[serde(default)]
ai_tags_override: Option<Vec<String>>,

// AbilityDef (runtime)
ai_tags_override: Option<AbilityTagSet>,
```

Resolver: `effective_tags(def, cache) = def.ai_tags_override.unwrap_or_else(|| cache.get(def.id))`.

**7. PlanAnnotation extension:**

```rust
// outcome.rs — PlanAnnotation
#[serde(default)]
pub effective_ai_tags: Vec<AbilityTagSet>,  // per Cast step
```

Заполняется в planning/scorer.rs (или соответствующей stage) lookup'ом по ability_id каждой Cast step. Поведение не меняется — никто не читает.

**8. Tests `tags/classify_tests.rs`:**

- `derive_status_tags_for_each_existing_status` × 10 (по числу статусов в `assets/data/statuses.toml`).
- `derive_ability_tags_for_each_existing_ability` × N (~15) — pin expected tag set.
- `derive_ability_tags_taunt_has_peel` (использует forces_targeting через status lookup).
- `derive_ability_tags_paralyzing_shot_has_offensive_and_apply_cc`.
- `override_replaces_derived_not_appends`.
- `tag_set_iter_order_stable` (для детерминированной сериализации).
- `tag_set_serde_round_trip`.
- `effective_ai_tags_populated_per_cast_step_in_annotation`.

### Удаляется

- (Ничего в 9.A.) `ability_vote`, стабы appraisal, hardcoded classify_mismatch — остаются нетронутыми. Изъятие — в 9.B.

### Gate

- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Golden 0/N (поведение не меняется; диагностика additive).
- Schema v29 (без bump). v29 logs читаются: новое поле = empty по `#[serde(default)]`.
- Sanity: `tag_cache.iter().count() == content.abilities.iter().count()` — все способности классифицированы (классификация может быть пустой, но запись присутствует).

---

## Сабшаг 9.B — Hardcode removal через теги (~2.5 дня)

**Scope.** Replace 4 hardcode-сайта на tag-driven чтение. `ability_vote` уходит из production-пути; appraisal стабы (rescue_ally, apply_cc) активируются; classify_mismatch читает StatusTag.

### Изменения

**1. `role::infer_profile` (`role.rs:206–236`) → tag-driven:**

```rust
pub fn infer_profile(
    abilities: &[AbilityId],
    max_hp: i32,
    total_armor: i32,
    content: &ContentView,
    tag_cache: &AbilityTagCache,
) -> AxisProfile {
    let mut p = AxisProfile::default();
    for id in abilities {
        let Some(def) = content.abilities.get(id) else { continue };
        let cost: f32 = def.costs.iter().map(|c| c.amount as f32).sum();
        let weight = 1.0 + cost;
        let tags = tag_cache.get(id);

        // Tag → axis bias mapping table (explicit, в одном месте)
        let v = tag_axis_vote(tags, def, weight);  // [tank, melee, ranged, control, support]
        for i in 0..5 { p.axes[i] += v[i]; }
    }
    // Stat-based tank mass (как было)
    let eff_hp = (max_hp + total_armor * 2) as f32;
    p.tank += (eff_hp / 20.0).clamp(0.3, 2.0);
    if p.total() < 1e-6 { p.melee = 1.0; }
    p
}

fn tag_axis_vote(tags: AbilityTagSet, def: &AbilityDef, weight: f32) -> [f32; 5] {
    let mut v = [0.0; 5];
    if tags.contains(AbilityTag::Rescue)    { v[4] += weight; return v; }
    if tags.contains(AbilityTag::Summon)    { v[4] += weight*0.7; v[2] += weight*0.3; return v; }
    if tags.contains(AbilityTag::Defensive) { v[0] += weight; return v; }
    if tags.contains(AbilityTag::Offensive) {
        // melee/ranged split — единственное место, где shape ещё нужен (range/aoe/spell)
        let is_ranged = matches!(def.effect, EffectDef::SpellDamage{..})
            || def.aoe != AoEShape::None
            || def.range.min >= 2;
        if is_ranged { v[2] += weight } else { v[1] += weight };
        if tags.contains(AbilityTag::ApplyCC) { v[3] += weight*0.4; }
        return v;
    }
    if tags.contains(AbilityTag::ApplyCC) { v[3] += weight; return v; }
    if tags.contains(AbilityTag::Peel)    { v[0] += weight*0.7; v[4] += weight*0.3; return v; }
    if tags.contains(AbilityTag::Mobility){ v[1] += weight*0.3; return v; }
    v
}
```

**2. `compute_need_signals` activation (`appraisal/mod.rs:62`):**

```rust
let rescue_ally = compute_rescue_ally(active, snap, maps, tag_cache, tuning);
let apply_cc    = compute_apply_cc(active, snap, status_tag_cache, ability_tag_cache, tuning);
let setup_aoe   = 0.0;  // OUT OF SCOPE step 9 — нет Setup механики в shape
```

Producer skeleton (детали — в 9.B implementer):

- `compute_rescue_ally`: actor имеет ability с `Rescue`-тегом? + есть ally с low HP в reach или под threat? → curve.
- `compute_apply_cc`: actor имеет ability с `ApplyCC`-тегом? + есть target без StatusTag::HardCC + threat от него высокий? → curve.

Параметры curves — в `AiTuning.curves.{rescue_ally,apply_cc}`. Bump tuning fields, schema без bump (curve secondaries — additive).

**3. `repair::classify_mismatch` (`repair/mod.rs:55–88`):**

`actor_status_changed` нельзя классифицировать по string code — нужен diff между memory и current. Refactor:

```rust
// classify_mismatch теперь принимает контекст:
pub fn classify_mismatch(
    code: &'static str,
    ctx: &MismatchContext,  // delta+caches доступны
) -> ContinuationSeverity {
    match code {
        "actor_status_changed" => classify_status_change(&ctx.status_delta, ctx.status_tag_cache),
        // остальные ветки — без изменений
    }
}

fn classify_status_change(delta: &StatusDelta, cache: &StatusTagCache) -> ContinuationSeverity {
    for added in &delta.added {
        let tags = cache.get(added);
        if tags.contains(StatusTag::HardCC) { return ContinuationSeverity::Invalidating; }
        if tags.contains(StatusTag::SoftCC) { return ContinuationSeverity::Relevant; }
    }
    for removed in &delta.removed {
        let tags = cache.get(removed);
        if tags.contains(StatusTag::Buff) { return ContinuationSeverity::Relevant; }  // потеряли защиту
    }
    // Tick-only changes (Dot countdown, Buff countdown) → Cosmetic
    if delta.added.is_empty() && delta.removed.is_empty() {
        return ContinuationSeverity::Cosmetic;
    }
    ContinuationSeverity::Relevant
}
```

`StatusDelta` — добавляется в memory: помимо `actor_status_hash` хранить `actor_statuses_at_capture: Vec<StatusId>` (или snapshot от `PlanSnapshot`).

**4. Smoke test — ai_scenario без code change:**

`tests/scenarios/peel_via_taunt.toml` — actor с taunt, ally в threat. Assertion: actor выбирает taunt; intent=ProtectAlly или scoring продвинул taunt в top-1. **Без правки production кода** — only через теги.

### Удаляется

- `role::ability_vote` (`role.rs:240–296`) — переезжает в `tag_axis_vote` (compact, читает теги, не shape).
- `role::has_damage` (`role.rs:298–306`) — ushed by ability_vote; убирается вместе с ним.
- Захардкоженные `0.0` стабы для rescue_ally / apply_cc в `compute_need_signals`.
- `actor_status_changed` → Relevant прямой mapping в `classify_mismatch`.

### Acceptance gate

1. `cargo grep -F "ability_vote" src/combat/ai/` — 0 production-callers (только тесты + derive_default или удалено).
2. `compute_need_signals` body не содержит literal `0.0` для rescue_ally / apply_cc.
3. `classify_mismatch` body на actor_status_changed читает `StatusTagCache`.
4. Smoke test scenario passes без правки кода (только TOML).
5. `cargo test/clippy --all-targets/ai_scenarios` зелёные.
6. Golden — ожидаемые behavioral сдвиги (см. ниже).

### Ожидаемые сдвиги

- **Role inference** — должен совпадать с legacy `ability_vote` на ≥95% случаев (та же decision tree, переписанная через теги). Расхождения — для пограничных способностей с ApplyCC + Offensive.
- **Need signals** — `rescue_ally` / `apply_cc` теперь не 0.0 → ProtectAlly intent чаще активируется в danger-сценариях; ApplyCC интент чаще побеждает на target'ах без HardCC.
- **Repair severity** — burning duration ticks больше не Relevant → continuation_outcome shift: меньше voluntary abandon на burning ticks.

Mining gate в 9.C подтверждает направление сдвигов.

---

## Сабшаг 9.C — Calibration + scenarios + cleanup (~1–1.5 дня)

**Scope.** Добор сценариев, mining-калибровка curves для новых need signals, чистка legacy.

### Изменения

**1. Новые ai_scenarios:**

- `rescue_ally_via_heal_tag` — Tank + Support в команде, Support с low HP. Assertion: Tank prefers move-to-Support, Support использует heal на Tank или peel'ит.
- `apply_cc_skips_already_hardcc_target` — два врага, один stunned. Assertion: actor с ApplyCC-ability нацеливается на не-stunned.
- `actor_status_hardcc_invalidates_goal` — actor поймал stun посередине плана. Assertion: continuation_outcome = GoalAbandonedReactive.
- `actor_status_dot_tick_preserves_goal` — actor под burning, тик прошёл. Assertion: continuation_outcome = GoalPreserved* (Cosmetic severity).
- `peel_via_taunt` (повтор smoke из 9.B, но с golden) — taunt prefers when ally threatened.

**2. Mining-калибровка:**

- Rebuild v29 corpus после 9.B.
- Сверить distributions: `rescue_ally` percentile (новый сигнал — ожидаемо 5–15% на defensive-rich сценариях), `apply_cc` (5–10% на kit'ах с CC), continuation reactive/voluntary split (Cosmetic ticks больше не abandon).
- Подкрутить curves в `AiTuning.curves.{rescue_ally,apply_cc}` если distributions выглядят патологически (>40% активаций signal — too greedy; <2% — слишком cautious).

**3. Mining sections в `bin/mine_ai_logs.rs`:**

- `=== AI tags coverage ===` — какой % chosen plans использует override vs derived; tag distribution среди chosen (Offensive/Rescue/Peel/...).
- `=== Need signals (post-9.B) ===` — обновлённые distributions для rescue_ally / apply_cc.
- `=== Continuation severity (post-9.B) ===` — Cosmetic vs Relevant vs Invalidating split на actor_status_changed.

**4. Cleanup:**

- Удалить TODO/FIXME-метки на step 9 в `appraisal/mod.rs:61` и `repair/mod.rs:62`.
- Удалить mention'ы «`ability_vote` is the production classifier» из docs/ai.md (если есть).
- Обновить `docs/ai.md` секцию AI Roles: tag-driven inference; перечислить 7 + 5 тегов.
- Если override-разметка пуста — отметить в плане «derive покрывает 100% существующего контента»; иначе — список случаев.

### Acceptance gate

- Все 5 ai_scenarios зелёные.
- Mining v29 corpus baseline стабилен в expected directions.
- 0 references на `compute_factors` / `ability_vote` / `0.0 stub` в `git grep`.
- `docs/ai.md` отражает tag-layer; `docs/ai_rework.md` step 9 → DONE.

---

## Итого

| # | Сабшаг | Эстимейт | Gate |
|---|---|---|---|
| 9.A | Tag layer + classifier + cache + effective_ai_tags writeback | 1.5–2.0 | golden 0/N (additive), classify pin tests |
| 9.B | infer_profile / appraisal / classify_mismatch reads tags + smoke scenario | 2.0–2.5 | acceptance gate (4 hardcode-сайта закрыты), expected behavioral shifts |
| 9.C | Mining-калибровка + 5 scenarios + cleanup | 1.0–1.5 | mining baseline post-9.B, 0 stale refs |

**Суммарно ~5–6 дней.**

## Критические файлы

**Новые:**
- `src/combat/ai/tags/{mod,classify,cache}.rs`.
- `tests/scenarios/peel_via_taunt.toml` + 4 других в 9.C.

**Меняются:**
- `src/combat/ai/role.rs` — `infer_profile` сигнатура (tag_cache), `ability_vote` удалена, `tag_axis_vote` добавлена.
- `src/combat/ai/appraisal/mod.rs` — стабы заменены на producer'ы; `compute_need_signals` сигнатура (tag_cache).
- `src/combat/ai/repair/mod.rs` — `classify_mismatch` сигнатура (`MismatchContext`); `classify_status_change` добавлена.
- `src/combat/ai/intent.rs` — `PlanSnapshot` хранит `actor_statuses` для diff (или `AiMemory`).
- `src/combat/ai/outcome.rs` (или где живёт `PlanAnnotation`) — `effective_ai_tags` поле.
- `src/content/abilities.rs` — `AbilityFile/Record/Def.ai_tags_override`.
- `assets/data/ai_tuning.toml` — новые curves `rescue_ally`, `apply_cc`.
- `src/bin/mine_ai_logs.rs` — новые секции.
- `docs/ai.md` — обновление AI Roles.

**Не трогается (но зависит):**
- `src/content/{abilities,statuses}.rs` — shape остаётся authoritative; classifier читает.

## Что откладывается / Чего не делать

- **Setup / Cleanse / ZoneControl tags** — нет механики в shape. Активация — отдельные future-steps вместе с введением механик.
- **Finisher / Escape** — это outcome-property и intent-context соответственно; НЕ теги.
- **commitment_skill** — step 12.
- **Schema bump** — нет deletions; additive через `#[serde(default)]`.
- **TOML override на массовую перезапись** — проверить, если derive покрывает 100% — оставить override-механизм пустым. Авторам контента не предлагать override, пока не появится конкретный пограничный случай.
- **Tag-driven encounter overrides** — step 14.
- **Per-difficulty tag remap** — backlog.

## Открытые вопросы реализации

1. **Двухпроходный classify (statuses → abilities).** `derive_ability_tags` для Defensive/ApplyCC/Peel требует доступ к StatusTagCache. Либо: classifier строит сначала StatusTagCache, потом AbilityTagCache. Либо: AbilityTag derive принимает `&StatusTagLookup` параметром. Решение — implementer-уровень в 9.A.
2. **Передача cache в существующие consumers.** `infer_profile` сейчас принимает `&ContentView`; добавление `&AbilityTagCache` — каскад изменений по 5–10 callers (генератор плана, intent, scoring). Alternative: `ContentView` экспандируется до `WorldView` с tag_cache. Решение — implementer-уровень.
3. **`StatusDelta` для `classify_mismatch`.** PlanSnapshot хранит `actor_status_hash`, не сами статусы. Добавить `actor_statuses: Vec<StatusId>` в snapshot — ~minor schema bump (memory snapshot версия).
4. **Override deserialize ошибки.** Unknown tag string в TOML override — fail-loud panic vs warn-and-skip. Предлагаю panic: override — opt-in, разработчик контента знает, что делает.
5. **Bitset reps — manual u8 vs `bitflags!` macro.** `bitflags` крейт уже в зависимостях? Проверить и решить в 9.A.
