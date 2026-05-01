# Target Priority, Position & Influence

*Источники: `src/combat/ai/scoring/target_priority.rs`, `src/combat/ai/scoring/position_eval.rs`, `src/combat/ai/world/influence.rs`.*

## Target Priority (`scoring/target_priority.rs`)

| Фактор | Вес | Формула |
|--------|-----|---------|
| Threat | 0.20 | `target.threat / max_threat` |
| Killability | 0.20 | `1 − eff_hp / eff_max_hp` |
| Threat density | 0.20 | `(threat / eff_hp) / max_density` |
| Vulnerability | 0.15 | `+0.3` если LOW_HP, `+0.2` если damage_taken_bonus > 0 |
| Proximity | 0.15 | `1 / (1 + distance)` |
| Role value | 0.10 | Support=1.0, Control=0.8, Ranged=0.7, Melee=0.5, Tank=0.3 |

`eff_hp = hp + armor + armor_bonus`.

## Position Evaluation (`scoring/position_eval.rs`)

Линейная комбинация 3 карт влияния с весами по профилю. Escape (derived) не включён. Веса живут в `AiTuning.tables.axis_position_weights` (step 2.5, data-driven).

| Карта | Tank | Melee | Ranged | Control | Support |
|-------|------|-------|--------|---------|---------|
| danger | −1.0 | −0.9 | −1.8 | −1.5 | −2.5 |
| ally_support | 0.7 | 0.4 | 0.7 | 0.8 | 1.3 |
| opportunity | 0.9 | 1.5 | 1.0 | 0.8 | 0.5 |

## Influence Maps (`influence.rs`)

Все карты ∈ `[0, 1]` кроме escape (`[-1, +1]`, derived = `ally_support − danger`). Реcурс `InfluenceConfig` — параметры λ.

### Danger Map

Для каждого врага BFS по speed → достижимые тайлы + `hex_circle(max_attack_range)` → `danger += enemy.threat`. Норм: `/ Σ(enemy.threat)`.

### Ally Support Map

`support_weight(ally) × exp(-dist / λ)`, λ=2.5. Healer ×2.0, melee ×1.5, базовый =1.0.

### Opportunity Map

`target_value × exp(-dist / λ)`, λ=3.0. `target_value = 0.7 × (1 − hp%) + 0.3 × (threat / max_threat)`.

### Escape Map

Derived. Используется только в `pick_top_move_tiles` и debug overlay.
