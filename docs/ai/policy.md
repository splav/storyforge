# Policy формулы

*Источник: `src/combat/ai/scoring/policy/` (5 sub-modules: `damage`, `heal`, `friendly_fire`, `status`, `cc`).*

HP-эквивалентные оценки fact-полей. Живут в `src/combat/ai/policy/` как named pure functions — единственный source of truth для «как мы оцениваем этот факт». `compute_score_core` и `score_action` удалены в step 4.12; вместо них — policy sub-modules. Применяются в `compute_offensive` и `future_value::λ_attack`.

## Damage (`policy::damage::value`)

```
raw = max(0, expected - armor + damage_taken_bonus)
progress = min(raw / target.hp, 1.0)
score = raw × (0.5 + 0.5 × progress)
```

## Heal (`policy::heal::value`)

```
delta_pct = min(expected, missing_hp) / max_hp
horizon_sum = max(Σ damage_horizon, threat)
urgency = 1.0 + max(hp_missing, min(danger/hp, 1.0))  # capped at 2.0
score = delta_pct × horizon_sum × urgency
```

Urgency (включает `hp_missing` и `danger_at_target`) вычисляется в policy, не в outcome populator'е — `outcome.hp_restored` — чистый raw fact.

## Status Effects (`policy::status::*`, `policy::cc::value`)

```
skips_turn      → +threat × duration
damage_taken_δ  → +|delta| × duration
armor_δ         → +|delta| × duration
dot_dice        → +dice.expected() × duration
hp_percent_dot  → +ceil(max_hp × pct / 100) × duration
silence         → +threat × 0.5 × duration
speed_penalty   → +|bonus| × duration
```

`policy::cc::value(cc_turns_applied, vulnerability_applied, armor_shred_applied)` aggregates.

## AoE и Friendly Fire (`policy::friendly_fire::penalty`)

Per-entity policy application из `outcome.enemy_damage_per_entity` / `ally_damage_per_entity`:

```
penalty = raw_dmg × (1 + raw_dmg / max_hp)   # super-linear, escalates with damage ratio
```

## Critical Failure Adjustment

- Miss: `score × (1 - crit_chance)`
- ManaOverload: `score - crit_chance × mana_cost`
- CircuitBreach: `score × (1 - crit_chance) - crit_chance × mana_cost × 0.5`

Применяется через `factors::adjustments::crit_fail_adjusted` в `compute_offensive`.
