# Game Mechanics

## Stats

6 D&D-style stats, range -5..10. Used via `modifier(stat) = stat >> 1` (floor divide by 2).

| Stat | Modifier Range | Used For |
|------|---------------|----------|
| Strength | -3..+5 | Melee/physical damage bonus |
| Dexterity | -3..+5 | Initiative (d20 + DEX mod) |
| Constitution | -3..+5 | (reserved) |
| Intelligence | -3..+5 | Spell damage + healing bonus |
| Wisdom | -3..+5 | (reserved) |
| Charisma | -3..+5 | (reserved) |

## Damage

### Physical (weapon_attack, damage)
```
raw = dice_roll + STR_mod
actual = max(1, raw - armor - armor_bonus) + damage_taken_bonus
```

### Magical (spell_damage)
```
raw = dice_roll + INT_mod + spell_power
actual = raw + damage_taken_bonus
```
Pierces armor: ignores `armor` and `armor_bonus`.

## Healing
```
amount = dice_roll + INT_mod + spell_power
hp = min(hp + amount, max_hp)
```

## Resources

### Mana
- Start: `current = max` (full)
- Restore: +1 at start of owner's turn (`turn_start_system`)
- Spend: deducted on ability use (`resolve_action_system`)

### Rage
- Start: `current = 0`
- Gain: +1 for dealing damage, +1 for receiving damage (`apply_effects_system`)
- Spend: deducted on ability use

### Action Points
- `action: bool` — can use an ability this turn
- `movement: bool` — can move this turn
- Both reset to `true` at turn start (`advance_turn_system`)
- Movement and action are independent (can do both per turn)

## Speed & Movement
- `Speed(i32)` — max hex cells per movement action
- `BonusMovement(i32)` — temporary override from GrantMovement abilities (e.g. Rush)
- Passability: can walk through allies, blocked by enemies
- Landing: must end on empty cell (no stacking)

## Statuses

| Field | Effect |
|-------|--------|
| `armor_bonus` | Reduces physical damage (stacks with armor) |
| `damage_taken_bonus` | Increases all incoming damage |
| `skips_turn` | Unit cannot act or move |
| `forces_targeting` | Enemies must target this unit (taunt) |

### Duration
- Ticks on **applier's** EndTurn, not target's
- Internal: stored as `duration + 1` at application time
- Duration 1 = active until end of applier's next turn

## Enemy AI

Ability scoring (higher = chosen first):
1. **Heal** ally below 60% HP: score = missing_hp * 10 + 50
2. **SpellDamage**: dice_value + 20 (pierces armor)
3. **Status with skips_turn**: +40
4. **Damage**: dice_value + 5
5. **WeaponAttack**: 8 (baseline melee)
6. **Status with damage_taken_bonus**: +15
7. **GrantMovement**: 0 (enemies skip)

Targeting: respects `forces_targeting` (taunt). If no target in range → pathfind and move. If still unreachable → approach closest, end turn.

## Initiative
- Round 1: `d20 + DEX_modifier` per combatant
- Subsequent rounds: reuse same order
- Higher total acts first
