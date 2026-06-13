# Snapshot & Tags

*Источник: `src/combat/ai/world/snapshot.rs`, `src/combat/ai/world/cache.rs`, `src/combat/ai/world/tags/`.*

## BattleSnapshot

`BattleSnapshot` — снимок боя без Bevy-зависимостей (кроме `Entity` в кэше). Состоит из двух половин (с schema v38 сериализуются только они):

- **`state: combat_engine::CombatState`** — авторитетное игровое состояние (юниты, позиции, пулы, статусы). Источник истины по геймплею.
- **`cache: AiCache`** — AI-производные метрики per-unit (`UnitAiCache`): `threat`, `damage_horizon`, `role`, `AiTags`, `max_attack_range`, `aoo_expected_damage`, `caster_ctx`, `forced_mode`, `entity`. Строится в `build_snapshot`.

Плюс serde-skip индексы `uid_to_entity` / `entity_to_uid` (rebuild при десериализации) — единственный мост через namespace-границу engine `UnitId` ↔ Bevy `Entity` (нужен для саммонов, чьи синтетические UnitId не равны `entity.to_bits()`).

### UnitView

`UnitView { state: &Unit, cache: &UnitAiCache }` — borrowed-композиция двух половин, отдаётся `BattleSnapshot::unit(entity)`. `Deref` на engine `Unit`, поэтому геймплейные чтения (`hp()`, `pos`, `armor`, `statuses`) идут напрямую, а AI-метрики — через `.cache` (`view.cache.threat`). Передаётся по значению (две ссылки, 16 байт). Помимо Deref несёт хелперы: `is_alive`, `eff_hp`/`eff_max_hp`, `hp_pct`, `killability`, `resource_amount`, `can_afford`, `is_stunned`/`forces_targeting` (вычисляются из текущих статусов через `StatusTagCache` — никогда не устаревают).

Отдельного `UnitSnapshot`-зеркала больше нет (удалено вместе с контрактом `refresh_aggregates`): production и тесты читают engine `Unit` через `UnitView`, агрегаты (`armor_bonus`/`speed`/`damage_taken_bonus`) пересчитывает движок через `Effect::RefreshAggregates`. Тест-фикстуры строятся билдером `test_helpers::UnitBuilder` → `UnitFixture` → `fixture_to_pair` → `(Unit, UnitAiCache)`.

### AiTags (bitflags)

```
LOW_HP | CAN_HEAL | CAN_CC | HAS_AOE | IS_STUNNED | FORCES_TARGETING | RANGED | MELEE_ONLY
```

## AI Semantic Tags

Семантические теги (`src/combat/ai/tags/classify.rs` — single source of truth) дополняют битфлаги более высокоуровневой классификацией способностей и статусов. Строятся один раз при загрузке контента; кешируются в `AbilityTagCache` / `StatusTagCache` (`tags/cache.rs`).

### AbilityTag (7 значений)

| Тег | Условие (из `derive_ability_tags`) |
|---|---|
| `Offensive` | WeaponAttack / Damage / SpellDamage effect |
| `Defensive` | Применяет Buff-статус на self или союзника |
| `Rescue` | Heal effect + target = SingleAlly |
| `Summon` | Summon effect |
| `Mobility` | GrantMovement effect |
| `ApplyCC` | Применяет HardCC или SoftCC на врага |
| `Peel` | Применяет `forces_targeting` статус на self или союзника |

Override: поле `ai_tags_override` в TOML заменяет derived tags целиком (replace-not-append). Используется для пограничных случаев, где shape не передаёт семантику.

### StatusTag (6 значений)

| Тег | Условие (из `derive_status_tags`) |
|---|---|
| `HardCC` | `skips_turn = true` |
| `SoftCC` | `causes_disadvantage` или `speed_bonus < 0` |
| `Dot` | `dot_dice` задан или `hp_percent_dot > 0` |
| `Buff` | `buff_class` задан или `armor_bonus > 0` |
| `Compulsion` | `forces_targeting = true` (параллельно с другими тегами) |
| `Cosmetic` | fallback — ни один другой тег не выставлен |

### Как теги влияют на поведение AI

- **Инференс профиля** (`role.rs::infer_profile`) — `tag_axis_vote` использует AbilityTag для голосования за оси.
- **Need signals** (`appraisal/mod.rs`) — `rescue_ally` читает Rescue-tagged abilities; `apply_cc` читает ApplyCC-tagged abilities + target с HardCC для подавления сигнала.
- **Goal-preserving repair** (`repair/mod.rs::classify_status_change`) — StatusTag определяет severity при `actor_status_changed`: HardCC/Compulsion set → Invalidating; Buff removed/SoftCC set → Relevant; пустой tick → Cosmetic.
