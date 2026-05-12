# Snapshot & Tags

*Источник: `src/combat/ai/snapshot.rs`, `src/combat/ai/tags/`.*

## BattleSnapshot

`BattleSnapshot` — чистый снимок без Bevy-зависимостей (кроме Entity).

### UnitSnapshot

- Позиция, HP / max_hp, armor.
- Агрегаты `armor_bonus` / `damage_taken_bonus` (снимаются в build-time, обновляются через `refresh_status_aggregates` при status-mutation в sim).
- Ресурсы (mana / rage / energy).
- Speed (base + status_bonus на snapshot-time).
- Список способностей.
- **`statuses: Vec<ActiveStatusView>`** — mirror `StatusEffects` component (`id`, `rounds_remaining`, `dot_per_tick`).
- threat, `AiTags`, `max_attack_range`, `aoo_expected_damage`, `summoner`.

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
