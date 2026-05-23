# Engine Bootstrap V3 — Dynamic Spawn Full Fix Research (3.7-R)

**Статус**: research завершён. Plan-агент discovered что **2 ship-content summons сломаны сегодня**: `storm_spirit` (demo_campaign) и `forest_morok` (bell_under_veil). Full fix их оживит — это bug fix, не regression, но **balance demo сценариев изменится**.

## 1. Текущий `UnitTemplate`

**Path:** `/Users/splav/personal/storyforge/crates/combat_engine/src/content.rs:184-192`

```rust
#[derive(Debug, Clone, Copy)]
pub struct UnitTemplate {
    pub max_hp: i32,
    pub armor: i32,
    pub base_speed: i32,
    pub max_ap: i32,
    pub mana_max: i32,
    pub energy_max: i32,
    pub rage_max: i32,
}
```

`Copy + Clone + Debug` — без serde derive.

**Callsites:**
- `crates/combat_engine/src/toml_content_view.rs:561-569` — `convert_template()` engine-side TOML loader.
- `src/combat/engine_bridge.rs:282-302` — `EcsContentView::unit_template()` bridge-side impl.
- `tests/combat_engine/effect.rs:1011-1021` — `test_template()` helper.
- `tests/combat_engine/effect.rs:54-57` — `StubContent::with_template()`.

**Trait method:** `ContentView::unit_template(&self, id: &str) -> Option<UnitTemplate>` (content.rs:305). `None` → `SpawnBlockedReason::TemplateMissing`.

## 2. Текущий `Effect::Spawn`

**Path:** `crates/combat_engine/src/effect.rs:693-777` (внутри `apply_effect`, arm `Effect::Spawn { summoner, template_id, max_active }`).

Из template копируются: `hp/max_hp/armor/base_speed/speed/action_points/max_ap/movement_points/rage/mana/energy`.

**Hardcoded defaults (THE BUG):**

```rust
armor_bonus: 0,
damage_taken_bonus: 0,
reactions_left: 0,
reactions_max: 1,
statuses: Vec::new(),
caster_context: crate::content::CasterContext::default(),  // BUG
aoo_dice: None,                                            // BUG
auras: Vec::new(),                                         // BUG
enemy_phases: Vec::new(),                                  // OK для summon
```

`team` производный: `summoner_team`. Action variant: `Effect::Spawn { summoner: UnitId, template_id: String, max_active: Option<u32> }`. Через `content.unit_template(template_id)` (line 694). Если `None` — `SpawnBlockedReason::TemplateMissing`. Никакого fallback'а.

## 3. Spawn-abilities в content

**2 summon-ability сейчас в шипатом content'е, оба сломаны:**

1. `assets/data/abilities.toml:195-200` — `summon_storm_spirit`, used by `stormborn_echo` template (demo_campaign). storm_spirit имеет `ability_ids = ["melee_attack"]` — **сломан** (нет caster_context.weapon_dice/str_mod, не делает AoO).

2. `assets/data/campaigns/bell_under_veil/abilities.toml:17-23` — `summon_forest_morok`, used by `anchor_of_grove` (Bell Under Veil ch1). Тот же class.

**Вывод:** NOT нулевой кейс — fix оживит оба. **Balance demo и BUV изменится** — это bug fix, но регенерация golden replays и manual playtests нужны.

## 4. `UnitTemplate` в content TOML — два loader'а

### Bridge-side (Bevy-tied), authoritative
- **Struct:** `UnitTemplateDef` (Rust runtime), `TemplateRecord` (TOML serde) в `src/content/unit_templates.rs`.
- **Files:** layered — `assets/data/unit_templates.toml` (global), `assets/data/campaigns/<name>/unit_templates.toml`, `assets/data/campaigns/<name>/<scenario>/unit_templates.toml`.
- **Templates ships:** demo (`stormborn_echo`, `storm_spirit`), bell_under_veil (~10+ включая `forest_morok`, `anchor_of_grove`, `crypt_servant`).

`UnitTemplateDef` уже carries: `id, name, race, faction, path, speed, stats: CombatStats, equipment: EquipmentBlock, resources: ResourcesBlock, ability_ids, ai_tuning_override`.

**Не имеет:** `aura`, `phases` (они на `EnemyDef`, не на template — design choice: template = stat sheet, encounter = per-instance overrides).

### Engine-side (pure-Rust), для replay tooling
- **Loader:** `crates/combat_engine/src/toml_content_view.rs` (TemplateRecord, TemplateFile, EquipmentRecord, ResourcesRecord, StatsRecord).
- **Не использует Bevy** — дублирует только нужные поля.

**Important:** оба loader'а должны быть extended синхронно — `toml_content_view_parity` test сверяет.

## 5. Engine structs для extension

### `CasterContext` (content.rs:36-43)
```rust
pub struct CasterContext {
    pub str_mod: i32,
    pub int_mod: i32,
    pub spell_power: i32,
    pub weapon_dice: Option<DiceExpr>,
    pub crit_fail_outcome: CritFailOutcome,
}
```
`Default + Clone + PartialEq + Serialize + Deserialize`.

### `AuraDef` (content.rs:216-223)
```rust
pub struct AuraDef {
    pub radius: u32,
    pub status_id: StatusId,
    pub applies_to: TeamRelation,
}
```
`Clone + PartialEq + Serialize + Deserialize`. Нет Default — Vec<AuraDef> через empty vec.

### `PhaseEntry` (content.rs:266-274)
```rust
pub struct PhaseEntry {
    pub pct: i32,
    pub new_max_hp: i32,
    pub heal_to_full: bool,
}
```
`Clone + PartialEq + Eq + Default + Serialize + Deserialize`.

### `DiceExpr` (dice.rs:21-26)
```rust
pub struct DiceExpr {
    pub count: u32,
    pub sides: u32,
    pub bonus: i32,
}
```
`Copy + Clone + PartialEq + Serialize + Deserialize`.

### Производные vs статичные

- **caster_context** — производится из ECS `Equipment + CombatStats + CombatPath`. Bridge logic: engine_bridge.rs:1486 onwards (bootstrap).
- **aoo_dice** — производится из `Equipment + CombatStats.strength + Abilities` (filter `WeaponAttack && range.max == 1`).
- **auras** — статично в TOML (но на `EnemyDef`, не на `UnitTemplate` сейчас).
- **enemy_phases** — статично, декларируется на `EnemyDef`.

## 6. Bridge `spawn_ecs_entity_from_engine_unit` (engine_bridge.rs:485-569)

ECS components ставящиеся при spawn:
- `Name`, `enemy_bundle` (= Combatant, Faction, CombatStats, Vital, Speed, Initiative, ActionPoints {ap=1, mp=speed}, Abilities, StatusEffects::default, Equipment, Reactions::default).
- `AxisProfile`, `AiMemory::default`, `SummonedBy`, `Faction` (overrides), conditionally `Rage/Mana/Energy`, `CombatPath`.

**НЕ ставит:** `AuraSource`, `EnemyPhases`, `StartingHexPos`, `VictoryTarget`.

### Развилка ECS-side sync

Кто читает `AuraSource` / `EnemyPhases` ВНЕ engine_bridge во время боя?

- **`AuraSource`:** только `bootstrap_combat_state` (one-shot). Больше никто.
- **`EnemyPhases`:** `apply_phase_ecs_writes` (читает `.pending[phase_idx]` для Name/stats/flavor change при phase trigger) + bootstrap. Critical для boss-summons.
- **`CombatStats/Equipment/Abilities`:** ставятся через enemy_bundle.

**Рекомендация: вариант (a) синхронизированно, MVP path.**

Обоснование:
1. После V3 engine authoritative. Но `apply_phase_ecs_writes` до сих пор читает ECS-side `EnemyPhases.pending`. Boss-summons (если когда-нибудь добавим) нуждаются в sync.
2. `AuraSource` — bootstrap читает только один раз. В принципе можно не ставить. Но best-practice — данные consistent.
3. **MVP:** `UnitTemplateDef` сейчас НЕ имеет `aura`/`phases` поля → engine `Unit.auras/enemy_phases` для summons остаются пустыми. Это покрывает 100% сегодняшнего content'а (storm_spirit, forest_morok не имеют ни aura, ни phases).
4. **Future-proof:** добавить engine `UnitTemplate.auras: Vec<AuraDef>` + `enemy_phases: Vec<PhaseEntry>` (по умолчанию empty), но не extend'ить bridge `UnitTemplateDef` TOML schema до явного use case.

## 7. Existing Effect::Spawn tests

Все в `tests/combat_engine/effect.rs`:
- `spawn_creates_unit_with_correct_template_stats` (1023-1054) — checks scalars, **не проверяет** caster_context/aoo_dice/auras/enemy_phases. Pre-existing gap.
- `spawn_blocked_when_template_missing` (1057-1072).
- `spawn_blocked_at_max_active_cap` (1074-1095).
- `spawn_blocked_when_no_free_position` (1097-1125).
- `spawn_blocked_by_corpse_tombstone` (1134-1164).
- `spawn_synthetic_uid_above_bevy_bit_range` (1166-1180).
- `effect_to_event_emits_unit_spawned_on_success` (1182-1208).

Test infra: `StubContent` (line 18-58) с `with_template(id, tpl)` — нужно расширить.

Related: `tests/combat_engine/cast.rs` имеет много `cast_*` тестов которые читают `caster_context` — нужно убедиться, что fix не сломает их (он не должен — изменения изолированы в Spawn arm и template loader).

## 8. Implementable план full fix

### A. Extend `UnitTemplate` (content.rs:184)

Drop `Copy`, add fields:

```rust
#[derive(Debug, Clone)]
pub struct UnitTemplate {
    pub max_hp: i32,
    pub armor: i32,
    pub base_speed: i32,
    pub max_ap: i32,
    pub mana_max: i32,
    pub energy_max: i32,
    pub rage_max: i32,
    // NEW
    pub caster_context: crate::content::CasterContext,
    pub aoo_dice: Option<crate::dice::DiceExpr>,
    pub auras: Vec<crate::content::AuraDef>,
    pub enemy_phases: Vec<crate::content::PhaseEntry>,
}
```

Trait method return value стал owned `UnitTemplate` (Clone, не Copy) — 2 callsite'а move'ят сразу, OK.

### B. Engine `Effect::Spawn` (effect.rs:743-768)

Заменить hardcoded defaults на template-carried:

```rust
caster_context: template.caster_context.clone(),
aoo_dice: template.aoo_dice,
auras: template.auras.clone(),
enemy_phases: template.enemy_phases.clone(),
```

### C. Engine-side TOML loader (toml_content_view.rs)

Расширить `convert_template`:
1. Параметризовать дополнительно `abilities: &HashMap<AbilityId, AbilityDef>` для AoO detection.
2. Lookup weapon dice через `WeaponRecord` — **WeaponRecord сейчас не имеет `dice_count`/`dice_sides`**, расширить.
3. Compute `str_mod = modifier(stats.strength)`, `int_mod = modifier(stats.intelligence)`, `spell_power`.
4. Build `CasterContext { str_mod, int_mod, spell_power, weapon_dice, crit_fail_outcome }`.
5. Compute `aoo_dice` если template имеет melee `WeaponAttack`.
6. `auras`, `enemy_phases` — пустые для engine-side TOML loader (engine TOML слой не имеет encounters; не критичны в replay tooling).

### D. Bridge `EcsContentView::unit_template` (engine_bridge.rs:282-303)

Заменить упрощённое построение на полное — mirror логику `bootstrap_combat_state` (lines 1490-1525). Recommended: вынести вспомогательный helper `build_engine_template_from_def(tpl, active_content) -> UnitTemplate`, который вызывается из обоих мест.

```rust
fn unit_template(&self, id: &str) -> Option<combat_engine::UnitTemplate> {
    let tpl = self.active_content.unit_templates.get(id)?;
    // ... build equipment, effective stats, armor ...
    // build CasterContext (same as bootstrap)
    // build aoo_dice (same as bootstrap)
    Some(combat_engine::UnitTemplate {
        max_hp: effective.max_hp,
        armor,
        base_speed: tpl.speed,
        max_ap: 1,
        mana_max: tpl.resources.mana_max,
        energy_max: tpl.resources.energy_max,
        rage_max: tpl.resources.rage_max,
        caster_context: engine_ctx,
        aoo_dice,
        auras: Vec::new(),         // UnitTemplateDef нет aura поля
        enemy_phases: Vec::new(),  // UnitTemplateDef нет phases поля
    })
}
```

### E. Bridge `spawn_ecs_entity_from_engine_unit`

**MVP:** не меняем — auras/phases на template нет, ECS-side данные не нужны.

**Future (если включаем aura/phases в template):** insert `AuraSource` if Some(aura), `EnemyPhases { pending: phases.clone() }` if !empty.

### F. TOML schema (`unit_templates.toml`)

**MVP: no changes** — caster_context и aoo_dice производимые из stats + equipment + abilities, не нужны в TOML.

### G. Tests

Минимум:
1. `spawn_unit_carries_caster_context_from_template` — assert spawned `caster_context.str_mod`, `weapon_dice` non-zero для melee template.
2. `spawn_unit_carries_aoo_dice_for_melee_template` — assert `aoo_dice.is_some()` для шаблона с melee weapon attack.
3. `spawn_unit_has_none_aoo_for_ranged_template` — `aoo_dice.is_none()` для ranged.
4. `spawn_unit_empty_auras_when_template_has_none` — sanity.
5. `toml_content_view_parity` — должен по-прежнему pass (после WeaponRecord extension).
6. Update `test_template()` helper и `StubContent::with_template`.

### H. Files to touch

**Engine:**
- `crates/combat_engine/src/content.rs` — UnitTemplate struct.
- `crates/combat_engine/src/effect.rs` — Effect::Spawn arm.
- `crates/combat_engine/src/toml_content_view.rs` — convert_template, TemplateRecord, WeaponRecord extension.

**Bridge:**
- `src/combat/engine_bridge.rs` — EcsContentView::unit_template, optional shared helper.

**Tests:**
- `tests/combat_engine/effect.rs` — test_template, make_unit, новые тесты.
- Возможно `tests/toml_content_view_parity.rs` (verify still passes).

## 9. Топ-3 риска

### Risk 1: Engine TOML parity test ломается
`tests/toml_content_view_parity.rs` сверяет engine-side TomlContentView и bridge-side ActiveContent. Если bridge добавит populated `caster_context`/`aoo_dice` в UnitTemplate, а engine loader не сможет точно reconstruct'ить (нет access к weapons.toml `dice_count`/`dice_sides`), parity fail.

**Mitigation:** Расширить `WeaponRecord` в `toml_content_view.rs` чтобы тянул `dice_count`, `dice_sides`. Это fix-в-fix'е — pure-Rust loader был неполным изначально.

### Risk 2: Балансовый shift в шипатых сценариях (storm_spirit, forest_morok)
Эти summons сейчас сломаны но "работают" — caster_context=zero значит весь damage расчёт даёт ~0 (или division-by-None). После fix'а они станут полноценными units → нанесут значимый урон → меняют balance demo и BUV.

**Mitigation:** Считать **bug fix**. Запустить golden combat replay перед merge. Регенерировать fixtures с warning'ом playtester'ам. Сообщить user'у заранее.

### Risk 3: `UnitTemplate` теряет `Copy` ломает API consumers
Сегодня `UnitTemplate: Copy` — return из `ContentView::unit_template` copy. Vec<AuraDef> в struct делает Clone-only.

**Mitigation:** Изменить trait method на owned return; callsite'ы (2 шт.) move'ят template сразу, OK.

## 10. Открытые вопросы

1. **WeaponRecord engine-side extension?** Сейчас тянет только `armor + max_hp`. Если pure-Rust loader должен populate'ить `caster_context.weapon_dice`, нужны `dice_count + dice_sides`. **Recommendation:** расширить — parity test остаётся sharp.

2. **`spell_power` source.** Сейчас `CasterContext::new(stats, eq, weapons)` вычисляет — implementer должен проверить логику в `src/content/abilities.rs:355`.

3. **`crit_fail_outcome` через CombatPath.** Bootstrap делает lookup `combat_path → paths → crit_fail_effect`. UnitTemplateDef имеет `path: Option<String>`. Implementer должен повторить map в `EcsContentView::unit_template`.

4. **Reactions defaults inconsistency.** ECS `Reactions::default()` = remaining=1/max=1; engine = `reactions_left: 0, reactions_max: 1`. Вне scope, но flag implementer'у.

5. **`aura`/`phases` в `UnitTemplateDef` сегодня?** MVP — нет (никто не использует). **Recommendation:** добавить engine-side fields в `UnitTemplate` (Vec по умолчанию empty), но bridge populates empty; UnitTemplateDef TOML extension отложить до явного use case.

6. **Bridge-level integration test summon?** `effect_to_event_emits_unit_spawned_on_success` — unit-level. Bridge-level summon test в `bridge_smoke.rs` — стоит проверить.

## Critical Files for Implementation

- `/Users/splav/personal/storyforge/crates/combat_engine/src/content.rs:184` — UnitTemplate struct.
- `/Users/splav/personal/storyforge/crates/combat_engine/src/effect.rs:693-777` — Effect::Spawn arm.
- `/Users/splav/personal/storyforge/src/combat/engine_bridge.rs:282-303` — EcsContentView::unit_template; mirror logic from line ~1480-1556 (bootstrap_combat_state).
- `/Users/splav/personal/storyforge/crates/combat_engine/src/toml_content_view.rs:534-572` — convert_template + WeaponRecord/TemplateRecord extension.
- `/Users/splav/personal/storyforge/tests/combat_engine/effect.rs:1208+` — new spawn assertion tests + extended test_template.
