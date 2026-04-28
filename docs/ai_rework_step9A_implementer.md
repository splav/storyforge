# Implementer Plan — Сабшаг 9.A (Tag layer foundation)

**Source of truth:** `/Users/splav/personal/storyforge/docs/ai_rework_step9_plan.md` секция «Сабшаг 9.A» и «Зафиксированные решения». Этот документ — implementer-friendly декомпозиция; не дублирует spec — отсылает к нему.

**Working tree:** `/Users/splav/personal/storyforge` (clean post step-8 baseline, schema v29).

**Final-state file inventory after 9.A:**
- New: `src/combat/ai/tags/mod.rs`, `src/combat/ai/tags/classify.rs`, `src/combat/ai/tags/cache.rs` + `src/combat/ai/tags/classify_tests.rs` (или inline `#[cfg(test)] mod tests`).
- Heavily edited: `src/content/abilities.rs` (+`ai_tags_override`), `src/combat/ai/mod.rs` (+`pub mod tags`), `src/combat/ai/outcome/mod.rs` (+`PlanAnnotation.effective_ai_tags`), `src/combat/ai/utility/mod.rs` (writeback после первичного скоринга), `src/main.rs` или `src/scenario/mod.rs` (insert `AbilityTagCache`/`StatusTagCache` resources после `ActiveContent`).
- Untouched: `src/combat/ai/role.rs`, `src/combat/ai/appraisal/mod.rs`, `src/combat/ai/repair/mod.rs` — это всё 9.B; в 9.A читателей у тегов нет, кроме writeback'а в annotation.

---

## Зафиксированные решения и развилки

### Решение 1: Bitset реализация — `bitflags!` крейт

Проверка `Cargo.toml` показывает `bitflags = "2"` уже в зависимостях, и в `src/combat/ai/snapshot.rs:20` есть рабочий пример `bitflags::bitflags! { pub struct AiTags: u16 { ... } }`.

**Аргументы за `bitflags!`:**
- Готовый рабочий паттерн в кодбейзе → нулевой review-friction;
- Bitwise ops (`|`, `&`, `-`, `contains`) — бесплатно;
- `Debug` и `Eq`/`Hash` derive из коробки;
- `Serialize`/`Deserialize` для bitflags v2 — через `#[derive]` + `serde` feature OR через manual impl. Поскольку основной канал сериализации — внутренний (`PlanAnnotation.effective_ai_tags`), сериализуем как `Vec<&'static str>` через manual `Serialize`/`Deserialize` поверх bitflags (выводим список enabled flag-имён).

**Аргументы за manual `u8`:**
- Чуть-чуть проще понять для нового читателя;
- Полный контроль над JSON-формой.

**Рекомендация:** **`bitflags!`** — паттерн уже валидирован проектом, не нужно изобретать. Storage `u8` для AbilityTagSet (7 тегов), `u8` для StatusTagSet (5 тегов).

```rust
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct AbilityTagSet: u8 {
        const OFFENSIVE = 0b0000_0001;
        const DEFENSIVE = 0b0000_0010;
        const RESCUE    = 0b0000_0100;
        const SUMMON    = 0b0000_1000;
        const MOBILITY  = 0b0001_0000;
        const APPLY_CC  = 0b0010_0000;
        const PEEL      = 0b0100_0000;
    }
}
```

Custom `Serialize`/`Deserialize` (через manual `Serialize::serialize_seq`) пишет/читает `Vec<&'static str>` (имена в фиксированном порядке `iter()` обхода — для детерминированного диффа в логах). Тест `tag_set_iter_order_stable` пинит порядок.

### Решение 2: Двухпроходный classify (statuses → abilities)

`derive_ability_tags` для `Defensive`, `ApplyCC`, `Peel` требует доступ к `StatusTagCache` (нужно знать, какой `StatusTag` у статусов, который ability накладывает).

**Варианты:**

(a) **Параметр `&StatusTagLookup`.** Сигнатура `derive_ability_tags(def: &AbilityDef, statuses: &StatusTagLookup) -> AbilityTagSet`, где `StatusTagLookup` — trait с одним методом `get(&self, id: &StatusId) -> StatusTagSet`. Реализуется и для `&HashMap<StatusId, StatusTagSet>`, и для `&StatusTagCache`. Caller строит сначала status cache, потом передаёт его в classifier при построении ability cache.

(b) **Две фазы внутри builder'а.** Free functions: `derive_status_tags(def) -> StatusTagSet` сначала, потом цикл строит `StatusTagCache`, потом `derive_ability_tags(def, &status_cache) -> AbilityTagSet`. Очень похоже на (a) по факту.

(c) **`OnceCell<StatusTagCache>`.** Глобальный или thread-local `OnceCell` инициализируется при первом обращении. Hidden global state, плохо тестируется, не масштабируется на multi-scenario harness'ы.

**Рекомендация:** **(a) параметр `&StatusTagLookup` через trait.** Phantom-cost: trait abstraction нужна минимально, но даёт чистый юнит-тест pure-классификатора без построения `StatusTagCache`. Это эквивалент (b) с явной зависимостью в сигнатуре. Тестовый код передаёт `&HashMap<StatusId, StatusTagSet>` напрямую, production-код — `&StatusTagCache.map`.

Уточнённая сигнатура:

```rust
// classify.rs

pub trait StatusTagLookup {
    fn get_tags(&self, id: &StatusId) -> StatusTagSet;
}

impl StatusTagLookup for std::collections::HashMap<StatusId, StatusTagSet> {
    fn get_tags(&self, id: &StatusId) -> StatusTagSet {
        self.get(id).copied().unwrap_or_default()
    }
}

pub fn derive_status_tags(def: &StatusDef) -> StatusTagSet { ... }

pub fn derive_ability_tags<L: StatusTagLookup>(
    def: &AbilityDef,
    statuses: &L,
    status_defs: &HashMap<StatusId, StatusDef>,
) -> AbilityTagSet { ... }
```

`status_defs` нужен дополнительно для проверки `forces_targeting` (Peel) — `StatusTagSet` не выводит этот флаг (он не в 5 status tag вариантах). Compromise: `forces_targeting` остаётся «shape-проверка через `status_defs.get(id).map(|s| s.forces_targeting)`», в обход кэша.

### Решение 3: Cache как Resource vs Asset

В кодбейзе `ContentView` живёт через `ActiveContent(pub ContentView)` Resource (см. `src/content/content_view.rs:223`), вставляется в `src/scenario/mod.rs:58` через `commands.insert_resource(ActiveContent(scen.content.clone()))`.

**Рекомендация:** **Resource**, ровно по паттерну `ActiveContent`.

Cache builder вставляется рядом с `ActiveContent`:

```rust
// scenario/mod.rs:58 — после insert_resource(ActiveContent(...))
let content = &scen.content;
let (status_cache, ability_cache) = crate::combat::ai::tags::cache::build_caches(content);
commands.insert_resource(status_cache);
commands.insert_resource(ability_cache);
```

Не используется Bevy Asset / AssetLoader, потому что caches — derived data, а не загружаемый артефакт.

### Решение 4: `effective_ai_tags` writeback location

Writeback должен происходить **после** того как `PlanAnnotation` создан (в `ScoredPool::new`), но **до** того как любая стадия может потенциально читать его. В 9.A никто не читает, но мы фиксируем pattern сейчас.

После анализа `src/combat/ai/utility/mod.rs:273–282`:

```
278: let mut pool = ScoredPool::new(plans);
279: for (ann, (score, raw)) in pool.annotations.iter_mut().zip(initial_scored.into_iter().zip(initial_raw.into_iter())) {
280:     ann.score = score;
281:     ann.factors = raw;
282: }
```

**Точное место вставки:** `src/combat/ai/utility/mod.rs:282` (сразу после loop, заполняющего `score` и `factors`):

```rust
// Step 9.A: populate effective_ai_tags from cache lookup per Cast step.
// Diagnostic only — no consumer reads this in 9.A.
for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
    ann.effective_ai_tags = plan
        .steps
        .iter()
        .filter_map(|step| match step {
            crate::combat::ai::planning::types::PlanStep::Cast { ability, .. } => {
                Some(world.ability_tags.effective(ability, world.content))
            }
            _ => None,
        })
        .collect();
}
```

Здесь `world` — это `ScoringCtx.world: &AiWorld` (или соответствующая структура). Cache доступен через `world.ability_tags: &AbilityTagCache` — потребуется добавить поле в `AiWorld` (уточнено в commit 4 ниже).

**Альтернатива (отвергнута):** writeback внутри `score_plans_with_raw` или `compute_plan_factors`. Минусы: смешивает diagnostic с пайплайном scoring'а, усложняет тестирование. Лучше держать writeback отдельным шагом — это согласуется с тем как `ann.score` и `ann.factors` заливаются отдельным циклом.

### Решение 5: Override TOML deserialize errors — panic

`AbilityFile.ai_tags_override: Option<Vec<String>>` десериализуется в `Vec<String>`. Затем при сборке `AbilityDef.ai_tags_override: Option<AbilityTagSet>` каждый string мапится в `AbilityTag::from_name(s)`.

**Варианты при unknown string:**
- **panic** (как `parse_abilities` делает на unknown `target_type`): fail-loud.
- warn-and-skip: silently drop unknown tag, продолжить.

**Рекомендация:** **panic**. Ровно по паттерну существующего парсера `abilities.toml` (строки 289, 321, 330, 343, 354 в `src/content/abilities.rs` все panic'ат на unknown enum strings). Override — opt-in low-volume фича; разработчик контента, который пишет override, должен быть уверен, что он валиден. Silent skip создаёт «работает не как ожидается» баги.

```rust
fn parse_ability_tag(s: &str, ability_id: &str, path: &str) -> AbilityTag {
    AbilityTag::from_name(s)
        .unwrap_or_else(|| panic!("{path}: ability '{ability_id}' has unknown ai_tags_override entry '{s}'"))
}
```

---

## Декомпозиция на коммиты (4 commit'а)

Порядок:
1. **Commit 1** — Enums + Sets + classifier (pure functions, no Bevy, no Resource, no callers). Юнит-тесты.
2. **Commit 2** — `AbilityTagCache` / `StatusTagCache` Resources + builder. Plumbing into scenario startup. Sanity tests.
3. **Commit 3** — `ai_tags_override` поле в `AbilityFile`/`AbilityDef`/parser. Resolver. Override pin tests.
4. **Commit 4** — `PlanAnnotation.effective_ai_tags` field + writeback в `pick_action`. `AiWorld.ability_tags` access path. Integration test, golden 0/N.

Каждый commit — `cargo check` зелёный + соответствующие тесты.

---

## Commit 1 — Enums, sets, classifier (pure)

**Цель.** Vocabulary + classification logic. Никакой Bevy, никаких Resources, никаких касаний production-кода. Compile-only goal: classifier функции принимают `&AbilityDef` / `&StatusDef`, возвращают `AbilityTagSet` / `StatusTagSet`. Pin-тесты на каждую существующую ability и status.

**Файлы (создать).**

### `src/combat/ai/tags/mod.rs`

```rust
//! AI semantic tags for abilities and statuses (step 9.A).
//!
//! Tags are *derived* projections of ability/status shape, computed once at
//! content load time and cached. The classifier is pure: same shape → same
//! tags. See `docs/ai_rework_step9_plan.md` for the full spec.
//!
//! In 9.A nothing in production reads tags — they're written into
//! `PlanAnnotation.effective_ai_tags` for diagnostics only.
//! Consumers (role, appraisal, repair) come in 9.B.

pub mod cache;
pub mod classify;

pub use cache::{AbilityTagCache, StatusTagCache};
pub use classify::{derive_ability_tags, derive_status_tags, StatusTagLookup};

use serde::{Deserialize, Serialize};

// ── Ability tags ─────────────────────────────────────────────────────────────

/// Closed enum of derivable ability semantics. 7 variants — see `docs/ai_rework.md` §9.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbilityTag {
    Offensive,
    Defensive,
    Rescue,
    Summon,
    Mobility,
    ApplyCC,
    Peel,
}

impl AbilityTag {
    pub fn name(self) -> &'static str {
        match self {
            Self::Offensive => "offensive",
            Self::Defensive => "defensive",
            Self::Rescue    => "rescue",
            Self::Summon    => "summon",
            Self::Mobility  => "mobility",
            Self::ApplyCC   => "apply_cc",
            Self::Peel      => "peel",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "offensive" => Some(Self::Offensive),
            "defensive" => Some(Self::Defensive),
            "rescue"    => Some(Self::Rescue),
            "summon"    => Some(Self::Summon),
            "mobility"  => Some(Self::Mobility),
            "apply_cc"  => Some(Self::ApplyCC),
            "peel"      => Some(Self::Peel),
            _ => None,
        }
    }

    /// Iteration order = bitset write order = JSON list order.
    /// Pinned by `ability_tag_iter_order_is_stable`.
    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::Offensive, Self::Defensive, Self::Rescue, Self::Summon,
            Self::Mobility,  Self::ApplyCC,   Self::Peel,
        ].into_iter()
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct AbilityTagSet: u8 {
        const OFFENSIVE = 0b0000_0001;
        const DEFENSIVE = 0b0000_0010;
        const RESCUE    = 0b0000_0100;
        const SUMMON    = 0b0000_1000;
        const MOBILITY  = 0b0001_0000;
        const APPLY_CC  = 0b0010_0000;
        const PEEL      = 0b0100_0000;
    }
}

impl AbilityTagSet {
    pub fn from_iter_tags<I: IntoIterator<Item = AbilityTag>>(it: I) -> Self {
        let mut s = Self::empty();
        for t in it { s.insert_tag(t); }
        s
    }

    pub fn contains_tag(self, t: AbilityTag) -> bool {
        let bit = Self::tag_bit(t);
        self.contains(bit)
    }

    pub fn insert_tag(&mut self, t: AbilityTag) {
        self.insert(Self::tag_bit(t));
    }

    pub fn iter_tags(self) -> impl Iterator<Item = AbilityTag> {
        AbilityTag::iter().filter(move |&t| self.contains_tag(t))
    }

    fn tag_bit(t: AbilityTag) -> Self {
        match t {
            AbilityTag::Offensive => Self::OFFENSIVE,
            AbilityTag::Defensive => Self::DEFENSIVE,
            AbilityTag::Rescue    => Self::RESCUE,
            AbilityTag::Summon    => Self::SUMMON,
            AbilityTag::Mobility  => Self::MOBILITY,
            AbilityTag::ApplyCC   => Self::APPLY_CC,
            AbilityTag::Peel      => Self::PEEL,
        }
    }
}

// Manual Serialize/Deserialize as Vec<&'static str> in iter() order — keeps
// log diffs reviewable.
impl Serialize for AbilityTagSet {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let count = self.iter_tags().count();
        let mut seq = s.serialize_seq(Some(count))?;
        for t in self.iter_tags() {
            seq.serialize_element(t.name())?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for AbilityTagSet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let names: Vec<String> = Vec::deserialize(d)?;
        let mut set = Self::empty();
        for n in names {
            let t = AbilityTag::from_name(&n)
                .ok_or_else(|| serde::de::Error::custom(format!("unknown ability tag '{n}'")))?;
            set.insert_tag(t);
        }
        Ok(set)
    }
}

// ── Status tags ───────────────────────────────────────────────────────────────

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusTag {
    HardCC,
    SoftCC,
    Dot,
    Buff,
    Cosmetic,
}

impl StatusTag {
    pub fn name(self) -> &'static str { /* analogous */ }
    pub fn from_name(s: &str) -> Option<Self> { /* analogous */ }
    pub fn iter() -> impl Iterator<Item = Self> {
        [Self::HardCC, Self::SoftCC, Self::Dot, Self::Buff, Self::Cosmetic].into_iter()
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct StatusTagSet: u8 {
        const HARD_CC  = 0b0000_0001;
        const SOFT_CC  = 0b0000_0010;
        const DOT      = 0b0000_0100;
        const BUFF     = 0b0000_1000;
        const COSMETIC = 0b0001_0000;
    }
}

// ... аналогично AbilityTagSet: from_iter_tags / contains_tag / insert_tag
//     / iter_tags / tag_bit + manual Serialize/Deserialize via Vec<&str>.
```

### `src/combat/ai/tags/classify.rs`

```rust
//! Pure classifier: shape → tag set.
//!
//! `derive_status_tags` is one-pass (no dependencies).
//! `derive_ability_tags` requires a status-tag lookup (for Defensive / ApplyCC / Peel
//! which depend on what statuses the ability applies). Caller builds the
//! StatusTagCache first, then passes it into ability classification.

use std::collections::HashMap;

use crate::content::abilities::{AbilityDef, EffectDef, StatusOn, TargetType};
use crate::content::statuses::StatusDef;
use crate::core::StatusId;

use super::{AbilityTag, AbilityTagSet, StatusTag, StatusTagSet};

// ── StatusTagLookup ──────────────────────────────────────────────────────────

pub trait StatusTagLookup {
    fn get_tags(&self, id: &StatusId) -> StatusTagSet;
}

impl StatusTagLookup for HashMap<StatusId, StatusTagSet> {
    fn get_tags(&self, id: &StatusId) -> StatusTagSet {
        self.get(id).copied().unwrap_or_default()
    }
}

// ── Status classifier ────────────────────────────────────────────────────────

pub fn derive_status_tags(def: &StatusDef) -> StatusTagSet {
    let mut s = StatusTagSet::empty();
    if def.skips_turn {
        s.insert_tag(StatusTag::HardCC);
    }
    if def.causes_disadvantage || def.speed_bonus < 0 {
        s.insert_tag(StatusTag::SoftCC);
    }
    if def.dot_dice.is_some() || def.hp_percent_dot > 0 {
        s.insert_tag(StatusTag::Dot);
    }
    if def.buff_class.is_some() || def.armor_bonus > 0 {
        s.insert_tag(StatusTag::Buff);
    }
    if s.is_empty() {
        s.insert_tag(StatusTag::Cosmetic);
    }
    s
}

// ── Ability classifier ───────────────────────────────────────────────────────

pub fn derive_ability_tags<L: StatusTagLookup>(
    def: &AbilityDef,
    status_lookup: &L,
    status_defs: &HashMap<StatusId, StatusDef>,
) -> AbilityTagSet {
    let mut s = AbilityTagSet::empty();

    // Offensive: any direct damage effect.
    if matches!(
        def.effect,
        EffectDef::WeaponAttack | EffectDef::Damage { .. } | EffectDef::SpellDamage { .. }
    ) {
        s.insert_tag(AbilityTag::Offensive);
    }

    // Rescue: heal targeted at an ally.
    if def.target_type == TargetType::SingleAlly
        && matches!(def.effect, EffectDef::Heal { .. })
    {
        s.insert_tag(AbilityTag::Rescue);
    }

    // Summon
    if matches!(def.effect, EffectDef::Summon { .. }) {
        s.insert_tag(AbilityTag::Summon);
    }

    // Mobility
    if matches!(def.effect, EffectDef::GrantMovement { .. }) {
        s.insert_tag(AbilityTag::Mobility);
    }

    // Defensive: ability applies a Buff status to Self or to Ally.
    // Buff = StatusTag::Buff (armor_bonus > 0 or buff_class is_some).
    let applies_buff_to_protected = def.statuses.iter().any(|sa| {
        let is_protected_target = matches!(sa.on, StatusOn::MySelf)
            || (sa.on == StatusOn::Target && def.target_type == TargetType::SingleAlly)
            || (sa.on == StatusOn::Target && def.target_type == TargetType::Myself);
        is_protected_target && status_lookup.get_tags(&sa.status).contains_tag(StatusTag::Buff)
    });
    if applies_buff_to_protected {
        s.insert_tag(AbilityTag::Defensive);
    }

    // ApplyCC: any status applied to Target with HardCC or SoftCC tag,
    // when the ability targets an enemy (or ground AoE — ignored: enemies
    // hit by aoe are still "target" via StatusOn::Target).
    let applies_cc_to_enemy = def.statuses.iter().any(|sa| {
        if sa.on != StatusOn::Target { return false; }
        if !matches!(def.target_type, TargetType::SingleEnemy | TargetType::Ground) { return false; }
        let tags = status_lookup.get_tags(&sa.status);
        tags.contains_tag(StatusTag::HardCC) || tags.contains_tag(StatusTag::SoftCC)
    });
    if applies_cc_to_enemy {
        s.insert_tag(AbilityTag::ApplyCC);
    }

    // Peel: ability applies a status with `forces_targeting = true` to
    // an ally / self (taunt-redirect), OR applies HardCC to enemy that targets
    // ally — но второе уже покрывается ApplyCC, так что Peel — узко на
    // forces_targeting.
    let applies_taunt = def.statuses.iter().any(|sa| {
        let to_ally_or_self = matches!(sa.on, StatusOn::MySelf)
            || (sa.on == StatusOn::Target && def.target_type == TargetType::SingleAlly)
            || (sa.on == StatusOn::Target && def.target_type == TargetType::Myself);
        to_ally_or_self
            && status_defs.get(&sa.status).map_or(false, |sd| sd.forces_targeting)
    });
    if applies_taunt {
        s.insert_tag(AbilityTag::Peel);
    }

    s
}
```

**Замечание по `taunt`:** ability `taunt` в `assets/data/abilities.toml:24` накладывает `defending` (на target=self → `StatusOn::Target` при `target_type=Myself`) и `taunted` (на self → `StatusOn::MySelf`). `defending` имеет `armor_bonus=4` (Buff), `taunted` имеет `forces_targeting=true`. Значит:
- **Defensive** ✓ через `defending`.
- **Peel** ✓ через `taunted`.

### `src/combat/ai/tags/cache.rs`

```rust
//! AbilityTagCache / StatusTagCache as Bevy Resources.
//!
//! Built once at scenario load via `build_caches(content)`, stored as
//! Resources beside `ActiveContent`. Lookup is HashMap O(1).

use std::collections::HashMap;

use bevy::prelude::*;

use crate::content::content_view::ContentView;
use crate::core::{AbilityId, StatusId};

use super::classify::{derive_ability_tags, derive_status_tags};
use super::{AbilityTagSet, StatusTagSet};

#[derive(Resource, Default, Debug, Clone)]
pub struct StatusTagCache {
    pub map: HashMap<StatusId, StatusTagSet>,
}

impl StatusTagCache {
    pub fn get(&self, id: &StatusId) -> StatusTagSet {
        self.map.get(id).copied().unwrap_or_default()
    }
}

#[derive(Resource, Default, Debug, Clone)]
pub struct AbilityTagCache {
    pub map: HashMap<AbilityId, AbilityTagSet>,
}

impl AbilityTagCache {
    /// Raw derived tags (without override).
    pub fn get(&self, id: &AbilityId) -> AbilityTagSet {
        self.map.get(id).copied().unwrap_or_default()
    }

    /// Effective tags = override if present, else derived.
    /// Resolver — central place for replace-not-append semantics.
    pub fn effective(
        &self,
        id: &AbilityId,
        content: &ContentView,
    ) -> AbilityTagSet {
        if let Some(def) = content.abilities.get(id) {
            if let Some(ovr) = def.ai_tags_override {
                return ovr;
            }
        }
        self.get(id)
    }
}

/// Build both caches from a content view. StatusTagCache first
/// (no deps), then AbilityTagCache (uses StatusTagCache).
pub fn build_caches(content: &ContentView) -> (StatusTagCache, AbilityTagCache) {
    let mut status_map: HashMap<StatusId, StatusTagSet> = HashMap::new();
    for (id, def) in &content.statuses {
        status_map.insert(id.clone(), derive_status_tags(def));
    }

    let mut ability_map: HashMap<AbilityId, AbilityTagSet> = HashMap::new();
    for (id, def) in &content.abilities {
        let tags = derive_ability_tags(def, &status_map, &content.statuses);
        ability_map.insert(id.clone(), tags);
    }

    (
        StatusTagCache { map: status_map },
        AbilityTagCache { map: ability_map },
    )
}
```

### `src/combat/ai/mod.rs`

Добавить `pub mod tags;` рядом с `pub mod factors;`.

**Тесты commit 1** (живут inline в `tags/mod.rs::tests` и `tags/classify.rs::tests`).

#### Pin tests — derive_status_tags (10 statuses из `assets/data/statuses.toml`)

Каждый тест строит `StatusDef` руками (или загружает через `ContentView::load_global_for_tests().statuses`) и пинит ожидаемый bitset:

| Status id | Поля shape | Ожидаемый StatusTagSet |
|---|---|---|
| `defending` | armor_bonus=4, buff_class=ArmorBuff | `BUFF` |
| `taunted` | forces_targeting=true | `COSMETIC` (forces_targeting не отображается в 5 status tags) |
| `stunned` | skips_turn=true | `HARD_CC` |
| `burning` | damage_taken_bonus=1 | `COSMETIC` (damage_taken_bonus не в 5 tags) |
| `paralyzed` | skips_turn=true | `HARD_CC` |
| `poisoned` | dot_count=1, dot_sides=4 | `DOT` |
| `broken_faith` | blocks_mana_abilities=true | `COSMETIC` (blocks_mana не в 5 tags) |
| `exhaustion` | speed_bonus=-1, hp_percent_dot=5 | `SOFT_CC \| DOT` |
| `pact_control` | ai_controlled=true | `COSMETIC` |
| `disoriented` | causes_disadvantage=true | `SOFT_CC` |

Один тест на статус, имя `derive_status_tags_for_<id>`. Acceptance: все 10 тестов зелёные.

**Замечание:** `taunted` мапится на `Cosmetic` потому что `forces_targeting` — не один из 5 status tags. Это OK: Peel-тег у ability считывается через `status_defs.get(...).forces_targeting`, минуя `StatusTagSet`. Это чётко отделяет «AI-side semantic» (5 tags) от «raw shape флага» (forces_targeting).

#### Pin tests — derive_ability_tags (15 abilities из `assets/data/abilities.toml`)

Каждый тест собирает `AbilityDef` (или берёт его из `ContentView::load_global_for_tests().abilities`), готовит `status_lookup` через `build_status_map_for_tests()`, вызывает classifier, пинит ожидаемый bitset:

| Ability id | Shape (key fields) | Ожидаемый AbilityTagSet |
|---|---|---|
| `move` | effect=ToggleMoveMode, target=Myself | `empty()` |
| `rest` | effect=RestoreResources, target=Myself | `empty()` |
| `melee_attack` | effect=WeaponAttack, target=SingleEnemy | `OFFENSIVE` |
| `taunt` | target=Myself, statuses=[defending on=target dur=1, taunted on=self dur=1] | `DEFENSIVE \| PEEL` |
| `fireball` | effect=SpellDamage, target=Ground, aoe=Circle | `OFFENSIVE` |
| `thunderstrike` | effect=SpellDamage, target=Ground, aoe=Circle | `OFFENSIVE` |
| `heal` | effect=Heal, target=SingleAlly | `RESCUE` |
| `flash` | effect=SpellDamage, target=SingleEnemy | `OFFENSIVE` |
| `burn` | target=SingleEnemy, statuses=[burning on=target] | `empty()` (burning=Cosmetic, нет CC) |
| `spark` | effect=SpellDamage, target=SingleEnemy | `OFFENSIVE` |
| `stun` | target=SingleEnemy, statuses=[stunned on=target dur=1] | `APPLY_CC` |
| `backstab` | effect=Damage, target=SingleEnemy, statuses=[poisoned] | `OFFENSIVE` (poisoned=Dot, не CC) |
| `rush` | effect=GrantMovement, target=Myself | `MOBILITY` |
| `field_medic` | effect=Heal, target=SingleAlly | `RESCUE` |
| `bow_shot` | effect=Damage, target=SingleEnemy, range.min=2 | `OFFENSIVE` |
| `paralyzing_shot` | effect=Damage, target=SingleEnemy, statuses=[paralyzed on=target] | `OFFENSIVE \| APPLY_CC` |
| `poison_shot` | effect=Damage, target=SingleEnemy, statuses=[poisoned on=target] | `OFFENSIVE` |
| `summon_storm_spirit` | effect=Summon, target=Myself | `SUMMON` |

(18 strok вместо 15 потому что в `abilities.toml` все 18 abilities, посчитал ровно.)

Acceptance: каждый ability получил ровно ожидаемый set. Один тест на ability, имя `derive_ability_tags_for_<id>`.

Дополнительно:

- `derive_ability_tags_taunt_has_peel_via_taunted_status`: спец-проверка что `taunt` получает `PEEL` именно через `taunted.forces_targeting`, а не через что-то ещё. Build `status_defs` без `taunted.forces_targeting`, ожидаем `DEFENSIVE` без PEEL.
- `derive_ability_tags_paralyzing_shot_has_offensive_and_apply_cc`: явная double-tag проверка.

#### Прочие тесты commit 1

- `ability_tag_set_iter_order_is_stable` — `[Offensive, Defensive, Rescue, Summon, Mobility, ApplyCC, Peel]` order.
- `status_tag_set_iter_order_is_stable` — `[HardCC, SoftCC, Dot, Buff, Cosmetic]`.
- `ability_tag_set_serde_round_trip_named_list` — `AbilityTagSet::all() ↔ JSON ["offensive","defensive",...]`.
- `status_tag_set_serde_round_trip_named_list` — same for status.
- `ability_tag_set_unknown_string_in_deserialize_errors` — `["bogus_tag"]` → custom error.
- `derive_status_tags_unknown_buff_class_treated_as_buff_via_armor` — defensive: `defending` (armor_bonus>0, buff_class=ArmorBuff) — `BUFF`.
- `classify_is_pure_no_io` — call twice, identical result (paranoia, structural).

**Gate (commit 1):**
- `cargo build --all-targets` зелёный.
- `cargo clippy --all-targets -- -D warnings` зелёный.
- ~30 новых тестов зелёные, остальные не тронуты.
- 0 references on tags из production-кода (только tests).

**Объём (estimate):** ~350 LOC code + ~400 LOC tests.

---

## Commit 2 — Resources + builder + scenario plumbing

**Цель.** `AbilityTagCache` и `StatusTagCache` живут как Bevy Resources, заполняются вместе с `ActiveContent`. Sanity tests на coverage.

**Файлы (изменить).**

- `src/scenario/mod.rs:58` — после `commands.insert_resource(ActiveContent(scen.content.clone()));` добавить:
  ```rust
  use crate::combat::ai::tags::cache::build_caches;
  let (status_tags, ability_tags) = build_caches(&scen.content);
  commands.insert_resource(status_tags);
  commands.insert_resource(ability_tags);
  ```
- `src/main.rs` — если есть централизованная регистрация `app.init_resource::<ActiveContent>()` или подобное, добавить `app.init_resource::<AbilityTagCache>().init_resource::<StatusTagCache>()`. (Уточнить чтением `main.rs`. По умолчанию — `Resource, Default` достаточно через ECS, явная регистрация не обязательна, но lint-friendly.)

**Файлы (тронуть для тестов).**

- `src/combat/ai/test_helpers.rs` — добавить `pub fn empty_caches() -> (StatusTagCache, AbilityTagCache)` для unit-тестов consumers (используется только в Commit 4).

**Тесты commit 2:**

- `build_caches_global_content_covers_all_abilities` — `let content = ContentView::load_global_for_tests(); let (sc, ac) = build_caches(&content); assert_eq!(ac.map.len(), content.abilities.len()); assert_eq!(sc.map.len(), content.statuses.len());` — поверка sanity invariant из spec («все способности классифицированы»).
- `build_caches_status_first_then_abilities_dependency_satisfied` — paranoid: ability `taunt` должен получать `PEEL` через cache. Если бы порядок был обратный, `taunt`'s status lookup был бы пустым и Peel не выставлялся.
- `build_caches_is_idempotent` — позвать дважды, hashmap-equal результаты.

**Gate (commit 2):**
- `cargo build --all-targets` зелёный.
- В тестах commit 1 + commit 2 — все зелёные.
- При запуске `cargo run --bin storyforge` (если есть smoke run): startup без panic.

**Объём:** ~80 LOC code + ~80 LOC tests + ~10 LOC scenario plumbing.

---

## Commit 3 — Override field в TOML/AbilityDef + resolver

**Цель.** TOML schema additive: `ai_tags_override: Option<Vec<String>>`. `AbilityDef.ai_tags_override: Option<AbilityTagSet>`. Replace-not-append семантика. Override pin tests.

**Файлы (изменить).**

- `src/content/abilities.rs`:

  - Добавить импорт `use crate::combat::ai::tags::{AbilityTag, AbilityTagSet};` внутри `parse_abilities`. **Альтернатива** (предпочтительно): не импортировать `combat::ai` из `content` (правило: content layer не зависит от AI layer). Тогда `AbilityDef.ai_tags_override` хранится как `Option<Vec<String>>` (raw), а конверсия в `AbilityTagSet` происходит на стадии `build_caches` (в `cache.rs`). Это чище architecturally.

  - **Рекомендация:** хранить override как `Option<Vec<String>>` в `AbilityDef`. `cache::effective` парсит на лету; парсер делает только структурную валидацию. Парсинг строки → enum переезжает в `cache.rs` (panic если unknown).

  - `AbilityFile.AbilityRecord` (строка ~211):
    ```rust
    #[derive(Deserialize)]
    struct AbilityRecord {
        // ... existing fields
        #[serde(default)]
        ai_tags_override: Option<Vec<String>>,
    }
    ```

  - `AbilityDef` (строка ~66):
    ```rust
    pub struct AbilityDef {
        // ... existing fields
        /// Optional override for AI semantic tags (replaces derived, not appends).
        /// Empty Vec means "explicitly empty tag set". `None` means "use derived".
        /// Validation of tag-name strings happens in `tags::cache::build_caches`.
        pub ai_tags_override: Option<Vec<String>>,
    }
    ```

  - `parse_abilities` map (строка ~365): добавить `ai_tags_override: r.ai_tags_override` в собираемый `AbilityDef`.

- `src/combat/ai/tags/cache.rs::AbilityTagCache::effective`: уточнить парсинг override:
  ```rust
  pub fn effective(&self, id: &AbilityId, content: &ContentView) -> AbilityTagSet {
      if let Some(def) = content.abilities.get(id) {
          if let Some(names) = &def.ai_tags_override {
              return parse_override(names, &def.id);
          }
      }
      self.get(id)
  }

  fn parse_override(names: &[String], ability_id: &AbilityId) -> AbilityTagSet {
      let mut s = AbilityTagSet::empty();
      for n in names {
          let t = AbilityTag::from_name(n).unwrap_or_else(|| {
              panic!(
                  "ability '{}': unknown ai_tags_override entry '{}' (known: \
                   offensive defensive rescue summon mobility apply_cc peel)",
                  ability_id, n
              )
          });
          s.insert_tag(t);
      }
      s
  }
  ```

  **Альтернатива:** парсить override один раз в `build_caches` (затратнее по памяти — добавить отдельную map `override_map: HashMap<AbilityId, AbilityTagSet>`), вместо того чтобы парсить при каждом lookup. Lookup частый (per Cast step per plan), парсинг строки в enum — медленный. **Решение:** parse в `build_caches` единожды, хранить `override_map` рядом с `map`. Ленивый panic (на load time) — fail-fast.

  Уточнённый `AbilityTagCache`:
  ```rust
  #[derive(Resource, Default, Debug, Clone)]
  pub struct AbilityTagCache {
      pub map: HashMap<AbilityId, AbilityTagSet>,           // derived
      pub override_map: HashMap<AbilityId, AbilityTagSet>,  // explicit overrides
  }

  impl AbilityTagCache {
      pub fn get(&self, id: &AbilityId) -> AbilityTagSet {
          self.map.get(id).copied().unwrap_or_default()
      }
      pub fn effective(&self, id: &AbilityId) -> AbilityTagSet {
          self.override_map.get(id).copied()
              .unwrap_or_else(|| self.get(id))
      }
  }
  ```

  `build_caches` теперь parses override panic'ами — content load is fail-fast on bad TOML. Производительность: lookup — две HashMap-проверки.

**Тесты commit 3:**

- `override_replaces_derived_not_appends` — построить `AbilityDef` для `melee_attack` (derived = `OFFENSIVE`), задать `ai_tags_override = Some(vec!["defensive"])`, ожидать `DEFENSIVE` (без OFFENSIVE).
- `override_empty_vec_results_in_empty_tag_set` — `Some(vec![])` → `AbilityTagSet::empty()` (явное «нет тегов»).
- `override_none_uses_derived` — `None` → derived tags.
- `override_unknown_tag_panics` — `Some(vec!["bogus_tag"])` → `build_caches` panics. Test через `#[should_panic(expected = "unknown ai_tags_override")]`.
- `override_multi_tag_combines` — `Some(vec!["offensive", "peel"])` → `OFFENSIVE | PEEL`.
- `parse_abilities_default_override_is_none` — TOML без `ai_tags_override` поля → `None` (additive через `#[serde(default)]`).
- `parse_abilities_with_override_field_round_trip` — TOML с `ai_tags_override = ["mobility"]` → `Some(vec!["mobility".to_string()])`.

**Gate (commit 3):**
- `cargo build --all-targets` зелёный.
- Все pre-existing TOML файлы (`assets/data/abilities.toml` и любые campaign/scenario overrides) парсятся без ошибок (никто не использует override → все `None`).
- 7 новых override-тестов зелёные.
- Sanity: `assets/data/abilities.toml` не содержит `ai_tags_override` ни в одном record (spec говорит «Initial usage: 0–2 случая», для 9.A — 0).

**Объём:** ~50 LOC content/abilities.rs + ~30 LOC cache.rs + ~120 LOC tests.

---

## Commit 4 — `effective_ai_tags` writeback + `AiWorld` access path

**Цель.** Pipeline пишет per-Cast effective tags в `PlanAnnotation`. Никто не читает в 9.A. Schema additive (no version bump).

**Файлы (изменить).**

- `src/combat/ai/outcome/mod.rs` — добавить поле:
  ```rust
  // PlanAnnotation, после `pub modifiers: Vec<ModifierContribution>,`
  /// Step 9.A: per-Cast-step effective AI tags (cache lookup with override).
  /// Length = number of Cast steps in the plan; Move steps contribute nothing.
  /// Diagnostic only — no consumer reads this in 9.A. Activated by 9.B.
  /// Schema-additive via `#[serde(default)]`; v29 logs without this field
  /// deserialize as empty vec.
  #[serde(default)]
  pub effective_ai_tags: Vec<AbilityTagSet>,
  ```
  + `use crate::combat::ai::tags::AbilityTagSet;` в импортах.

- `src/combat/ai/utility/mod.rs` — нужен доступ к `AbilityTagCache` и `ContentView` из `pick_action`. Найти `AiWorld` definition (вероятно в `src/combat/ai/mod.rs` или `src/combat/ai/utility/mod.rs`) и добавить поле `pub ability_tags: &'a AbilityTagCache`. Cascade — все callers `AiWorld { ... }` literal'ов (вероятно 5–10) должны получить `ability_tags` от Bevy системы (`Res<AbilityTagCache>` параметр).

  **Альтернатива:** Прокинуть cache отдельно — добавить параметр `ability_tags: &AbilityTagCache` в `pick_action`. Менее инвазивно, но requires добавление параметра во все callsites `pick_action` (включая enemy_turn.rs).

  **Рекомендация:** Расширить `AiWorld` — это устоявшийся container для read-only AI inputs (содержит content, tuning, difficulty). Расширение Aiworld полностью изолировано от per-call API.

  - Уточнённое место: при дальнейшем чтении файла `src/combat/ai/utility/mod.rs:1-100` найти `pub struct AiWorld<'a>`. Добавить `pub ability_tags: &'a AbilityTagCache`. Поле `world.ability_tags` доступно из `ScoringCtx` (так как `ScoringCtx.world: &AiWorld`).

  - `src/combat/ai/enemy_turn.rs` — Bevy system, который вызывает `pick_action`. Добавить параметр `ability_tags: Res<AbilityTagCache>` и передать `&ability_tags` в `AiWorld { ... }`.

- `src/combat/ai/utility/mod.rs:282` — после `ann.factors = raw;` цикла:
  ```rust
  // Step 9.A: populate effective_ai_tags per Cast step (diagnostic).
  for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
      ann.effective_ai_tags = plan
          .steps
          .iter()
          .filter_map(|step| match step {
              crate::combat::ai::planning::types::PlanStep::Cast { ability, .. } => {
                  Some(world.ability_tags.effective(ability))
              }
              _ => None,
          })
          .collect();
  }
  ```

  **Точно: `src/combat/ai/utility/mod.rs:282–283`** — вставка между closing `}` цикла score/factors и следующим блоком `let mut stage_ctx = StageCtx::new(...)`. Это **после** того как `ScoredPool::new(plans)` создал annotations и **до** того как pipeline stages запустились.

**Тесты commit 4:**

- `pick_action_populates_effective_ai_tags_per_cast_step` — integration test: создать actor с `melee_attack` ability и `taunt` ability, plan с Cast(melee_attack) → Cast(taunt). После `pick_action` → `pool.annotations[*].effective_ai_tags == vec![OFFENSIVE, DEFENSIVE | PEEL]` (или для plan с двумя casts).
- `pick_action_move_only_plan_has_empty_effective_ai_tags` — plan только Move steps → `effective_ai_tags.is_empty()`.
- `pick_action_override_propagates_to_annotation` — actor имеет ability с `ai_tags_override = Some(vec!["mobility"])` → annotation для plan c этой ability показывает `MOBILITY` (а не derived).
- `plan_annotation_default_effective_ai_tags_empty` — `PlanAnnotation::default().effective_ai_tags.is_empty()`.
- `plan_annotation_serde_v29_log_without_effective_ai_tags_deserialises` — взять JSON v29 log без `effective_ai_tags` поля → парсится как empty vec (forward compat). Этот тест защищает schema additive контракт.
- `plan_annotation_serde_round_trip_with_effective_ai_tags` — построить annotation с `vec![OFFENSIVE, RESCUE]`, serialize → deserialize → equal.

**Gate (commit 4):**
- `cargo test --all-targets` зелёный (включая новые + все pre-existing).
- `cargo clippy --all-targets -- -D warnings` зелёный.
- `cargo run --bin ai_scenarios` — output identical to pre-9.A baseline (поведение не изменилось; только diagnostic-поле добавилось в JSON).
- Golden 0/N: производственные scoring-формулы не тронуты, scores bit-exact.
- Schema verification: pre-9.A v29 logs (saved corpus) deserialise без ошибок (не получают `effective_ai_tags` → empty vec).
- Mining проверка: `cargo run --bin mine_ai_logs -- <fresh post-9.A v29 corpus>` — не падает, не показывает unexpected стат-сдвигов.
- `git grep "effective_ai_tags"` returns hits только в `outcome/mod.rs`, `utility/mod.rs`, и в test-файлах — никто из production consumers не читает.

**Объём:** ~10 LOC outcome/mod.rs + ~15 LOC utility/mod.rs + ~5 LOC AiWorld + ~10 LOC enemy_turn.rs + ~150 LOC tests.

---

## Тестовый чек-лист (полный 9.A)

| # | Test name | File |
|---|---|---|
| 1 | `derive_status_tags_for_defending` | `tags/classify.rs::tests` |
| 2 | `derive_status_tags_for_taunted` | same |
| 3 | `derive_status_tags_for_stunned` | same |
| 4 | `derive_status_tags_for_burning` | same |
| 5 | `derive_status_tags_for_paralyzed` | same |
| 6 | `derive_status_tags_for_poisoned` | same |
| 7 | `derive_status_tags_for_broken_faith` | same |
| 8 | `derive_status_tags_for_exhaustion` | same |
| 9 | `derive_status_tags_for_pact_control` | same |
| 10 | `derive_status_tags_for_disoriented` | same |
| 11 | `derive_ability_tags_for_move` | `tags/classify.rs::tests` |
| 12 | `derive_ability_tags_for_rest` | same |
| 13 | `derive_ability_tags_for_melee_attack` | same |
| 14 | `derive_ability_tags_for_taunt` | same |
| 15 | `derive_ability_tags_for_fireball` | same |
| 16 | `derive_ability_tags_for_thunderstrike` | same |
| 17 | `derive_ability_tags_for_heal` | same |
| 18 | `derive_ability_tags_for_flash` | same |
| 19 | `derive_ability_tags_for_burn` | same |
| 20 | `derive_ability_tags_for_spark` | same |
| 21 | `derive_ability_tags_for_stun` | same |
| 22 | `derive_ability_tags_for_backstab` | same |
| 23 | `derive_ability_tags_for_rush` | same |
| 24 | `derive_ability_tags_for_field_medic` | same |
| 25 | `derive_ability_tags_for_bow_shot` | same |
| 26 | `derive_ability_tags_for_paralyzing_shot` | same |
| 27 | `derive_ability_tags_for_poison_shot` | same |
| 28 | `derive_ability_tags_for_summon_storm_spirit` | same |
| 29 | `derive_ability_tags_taunt_has_peel_via_taunted_status` | same |
| 30 | `derive_ability_tags_paralyzing_shot_has_offensive_and_apply_cc` | same |
| 31 | `ability_tag_set_iter_order_is_stable` | `tags/mod.rs::tests` |
| 32 | `status_tag_set_iter_order_is_stable` | same |
| 33 | `ability_tag_set_serde_round_trip_named_list` | same |
| 34 | `status_tag_set_serde_round_trip_named_list` | same |
| 35 | `ability_tag_set_unknown_string_in_deserialize_errors` | same |
| 36 | `classify_is_pure_no_io` | `tags/classify.rs::tests` |
| 37 | `build_caches_global_content_covers_all_abilities` | `tags/cache.rs::tests` |
| 38 | `build_caches_status_first_then_abilities_dependency_satisfied` | same |
| 39 | `build_caches_is_idempotent` | same |
| 40 | `override_replaces_derived_not_appends` | `tags/cache.rs::tests` |
| 41 | `override_empty_vec_results_in_empty_tag_set` | same |
| 42 | `override_none_uses_derived` | same |
| 43 | `override_unknown_tag_panics` (`#[should_panic]`) | same |
| 44 | `override_multi_tag_combines` | same |
| 45 | `parse_abilities_default_override_is_none` | `content/abilities.rs::tests` |
| 46 | `parse_abilities_with_override_field_round_trip` | same |
| 47 | `pick_action_populates_effective_ai_tags_per_cast_step` | `utility/mod.rs::tests` |
| 48 | `pick_action_move_only_plan_has_empty_effective_ai_tags` | same |
| 49 | `pick_action_override_propagates_to_annotation` | same |
| 50 | `plan_annotation_default_effective_ai_tags_empty` | `outcome/mod.rs::tests` |
| 51 | `plan_annotation_serde_v29_log_without_effective_ai_tags_deserialises` | same |
| 52 | `plan_annotation_serde_round_trip_with_effective_ai_tags` | same |

**Existing tests that MUST still pass unchanged (regression guard):**
- `cargo run --bin ai_scenarios` — все scenario logs bit-equal с pre-9.A baseline (новое поле present, но empty/expected — golden diff допустим только в новом поле `effective_ai_tags`).
- `factors/*` тесты неизменны.
- `pipeline/stages/*` тесты неизменны.
- `role::infer_profile` тесты неизменны (consumers не тронуты в 9.A).

---

## Migration order и Cargo check инварианты

| Step | Commit | `cargo check` |
|------|--------|---------------|
| 1 | Commit 1 (enums + classifier) | зелёный |
| 2 | Commit 2 (Resources + scenario plumbing) | зелёный (Resources — additive) |
| 3 | Commit 3 (override field в AbilityDef) | зелёный (поле additive с `#[serde(default)]`) |
| 4 | Commit 4 (writeback + AiWorld) | зелёный (AiWorld расширяется, callers Bevy systems получают новый Resource) |

Каждый commit отдельный, разных размеров; commit 4 — самый интегрирующий и самый рискованный (cascade на enemy_turn.rs system signature).

---

## Risk register

| Risk | Likelihood | Severity | Mitigation |
|------|-----------|----------|------------|
| **Cascade на `AiWorld` callers (commit 4)** — поле `ability_tags` добавляется, все literal-конструкторы должны быть обновлены | Medium | Medium | `cargo check` после добавления — компилятор покажет каждый callsite. Tests `test_helpers::make_test_ctx` нужно обновить (`empty_caches()` helper). |
| **`status_lookup` API drift** между manual HashMap и Resource StatusTagCache | Low | Low | Trait abstraction (`StatusTagLookup`) на обоих имплементациях; integration test через `build_caches_*`. |
| **Override panic на load — content broken irrecoverably** | Low | High | Тесты pin'ят что override пустой во всём `assets/data/`. `should_panic` тест проверяет error message. Spec говорит «0–2 случая» — низкая вероятность регрессии. |
| **Schema bit-equal break** — добавление `effective_ai_tags` в JSON может ломать старые v29 readers | Low | Medium | `#[serde(default)]` обеспечивает forward-read. Pin test `plan_annotation_serde_v29_log_without_effective_ai_tags_deserialises`. |
| **`taunted` status имеет `Cosmetic` тег** — может казаться странным, что taunt-effect не виден в `StatusTag` enum | Medium | Low | Документировать в комментарии: `forces_targeting` — shape флаг, не AI semantic; ability-side через Peel tag (читая `status_defs.forces_targeting`, не StatusTagSet). |
| **`burning` damage-only DoT vs Cosmetic** — `burning` имеет `damage_taken_bonus=1` (vulnerability), не `dot_dice` | Medium | Low | По spec rule, `Dot` = `dot_dice.is_some() \|\| hp_percent_dot > 0`. `damage_taken_bonus` — отдельная shape feature (vulnerability), не маппится в 5 status tags. Pin test `derive_status_tags_for_burning` явно ожидает `Cosmetic`. Это дизайн-решение: vulnerability — это amplifier, не сам DoT. |
| **`AbilityTagCache` lifetime в `AiWorld<'a>`** — borrow check на Resource'ах | Medium | Low | `Res<'_, AbilityTagCache>` в Bevy дереферится в `&AbilityTagCache` через `&res`. Стандартный паттерн, работает в `enemy_turn.rs` для других resources. |
| **Bevy `AbilityTagCache` Default impl** не строит cache (тривиальный default) — production rely on scenario init | Low | Medium | Sanity test `pick_action_populates_effective_ai_tags_per_cast_step` использует ручной build_caches; production-test через `cargo run --bin ai_scenarios` с реальным scenario init. |
| **`bitflags` v2 Serialize feature flag** — может потребовать `bitflags = { version = "2", features = ["serde"] }` | Low | Low | Решено: используем manual `Serialize`/`Deserialize` через `Vec<&str>` (reviewable diffs). Не зависим от bitflags serde feature. |
| **TOML `ai_tags_override` deserialize неожиданное поле в campaign/scenario layer** — campaign/scenario layer override ability, без указания tags-override → `None`, OK | Low | Low | `#[serde(default)]` обеспечивает additive. Pin test `parse_abilities_default_override_is_none`. |

---

## Объём изменений (estimate)

| File | LOC delta | Type |
|------|-----------|------|
| `src/combat/ai/tags/mod.rs` | +250 | new |
| `src/combat/ai/tags/classify.rs` | +180 | new |
| `src/combat/ai/tags/cache.rs` | +120 | new |
| (tests inline в выше файлах) | +500 | new |
| `src/combat/ai/mod.rs` | +1 | edit |
| `src/content/abilities.rs` | +20 | edit |
| `src/combat/ai/outcome/mod.rs` | +10 | edit |
| `src/combat/ai/utility/mod.rs` | +25 | edit (writeback + AiWorld field) |
| `src/combat/ai/enemy_turn.rs` | +5 | edit (Bevy system signature) |
| `src/combat/ai/test_helpers.rs` | +30 | edit (empty_caches helper) |
| `src/scenario/mod.rs` | +5 | edit (resource init) |
| **Total** | **~1145 LOC** | |

---

## Финальный gate 9.A

После всех 4 commits:

1. `cargo test --all-targets` зелёный, включая 52 новых теста.
2. `cargo clippy --all-targets -- -D warnings` зелёный.
3. `cargo build --all-targets --release` зелёный.
4. `cargo run --bin ai_scenarios` — выходные logs:
   - Schema v29 (без bump).
   - Каждый `PlanAnnotation` имеет поле `effective_ai_tags` (массив массивов tag-имён, по одному per Cast step).
   - Все остальные numeric поля (factors, terminal, score) — bit-equal с pre-9.A baseline.
5. v29 corpus, сохранённый pre-9.A: deserialise через post-9.A code → success, `effective_ai_tags` = `[]` для всех plans (forward compat).
6. Sanity: `tag_cache.map.iter().count() == content.abilities.iter().count()` — verified в `build_caches_global_content_covers_all_abilities`.
7. **Поведение AI не изменилось.** Никто не читает `effective_ai_tags`; `infer_profile`, `compute_need_signals`, `classify_mismatch` остаются нетронутыми.
8. Mining baseline reproduction: post-9.A v29 corpus mined → identical metrics с pre-9.A baseline (только в JSON-форме новое diagnostic-поле, не влияющее на агрегаты).

---

## Таблица коммитов

| # | Title | Estimate (h) | Files | Tests |
|---|---|---|---|---|
| 1 | Tag enums + bitset wrappers + pure classifier | 4–5 | 3 new files (`tags/{mod,classify,cache}.rs`) | 36 (10 status pins + 18 ability pins + 8 misc) |
| 2 | AbilityTagCache/StatusTagCache Resources + scenario init | 1.5 | edit `scenario/mod.rs`, optional `main.rs` | 3 (coverage + dependency order + idempotence) |
| 3 | `ai_tags_override` field в content + resolver | 2 | edit `abilities.rs`, `cache.rs` | 7 (override semantics + parse) |
| 4 | `PlanAnnotation.effective_ai_tags` + writeback + `AiWorld.ability_tags` | 3–4 | edit `outcome/mod.rs`, `utility/mod.rs`, `enemy_turn.rs`, `test_helpers.rs` | 6 (writeback + serde compat) |
| **Total** | | **10.5–12.5** (~1.5–2 рабочих дня, согласуется с эстимейтом spec'а 1.5–2.0) | | **52 new tests** |

---

### Critical Files for Implementation

- `/Users/splav/personal/storyforge/src/combat/ai/tags/mod.rs` — vocabulary + bitset wrappers (new).
- `/Users/splav/personal/storyforge/src/combat/ai/tags/classify.rs` — pure shape→tags functions (new).
- `/Users/splav/personal/storyforge/src/combat/ai/tags/cache.rs` — Resources + builder + override resolver (new).
- `/Users/splav/personal/storyforge/src/content/abilities.rs` — `AbilityFile/Record/Def.ai_tags_override` field plumbing.
- `/Users/splav/personal/storyforge/src/combat/ai/utility/mod.rs` — `effective_ai_tags` writeback (line ~282), `AiWorld.ability_tags` access path.
- `/Users/splav/personal/storyforge/src/combat/ai/outcome/mod.rs` — `PlanAnnotation.effective_ai_tags: Vec<AbilityTagSet>` field (additive, `#[serde(default)]`).

---
