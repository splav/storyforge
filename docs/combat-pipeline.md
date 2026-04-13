# Combat Pipeline

## System Chain

All 10 systems run `.chain()` in `CombatPhase::AwaitCommand` — strict sequential order each frame:

```
turn_start → skip_dead → skip_stunned → player_command → enemy_ai → movement → validate → resolve → apply_effects → advance_turn
```

## Message Flow (One Turn)

```
                    player_command / enemy_ai
                              │
                         UseAbility { actor, ability, target }
                              │
                    validate_action_system
                    (checks: AP, resources, range, target alive)
                              │
                     ValidatedAction { actor, ability, target }
                              │
                    resolve_action_system
                    (rolls dice, subtracts costs, ap.action = false)
                              │
               ┌──────────────┼──────────────┐
        ApplyDamage     ApplyHeal      ApplyStatus
               │              │              │
               └──────────────┼──────────────┘
                              │
                    apply_effects_system
                    (armor, rage +1, death check)
                              │
                         EndTurn { actor }
                              │
                    advance_turn_system
                    (tick statuses, victory/defeat, next actor, reset AP)
```

## System Details

### turn_start_system
Fires once per turn (when `ctx.active != ctx.last_active`). Restores +1 mana to current actor.

### skip_dead_turn_system / skip_stunned_turn_system
Dead actor → immediate EndTurn. Stunned actor (`skips_turn` status) → `ap.action = false`, `ap.movement = false`, EndTurn.

### player_command_system
Only for `Team::Player`. Handles:
- **1-5**: select ability slot (clears move_mode)
- **M**: toggle move mode (clears selected_ability)
- **Tab**: cycle targets (enemies for SingleEnemy, allies for SingleAlly)
- **Enter**: confirm ability use → `UseAbility`
- **E**: end turn manually → `EndTurn`
- **Escape**: cancel move mode
- Auto-enters move_mode when `BonusMovement` is present
- Auto-ends turn when both `action` and `movement` are false

### enemy_ai_system
Only for `Team::Enemy`. Smart ability selection:
1. Scores each affordable ability × target pair
2. Heal allies below 60% HP → high priority
3. SpellDamage > Damage > WeaponAttack
4. Status with `skips_turn` → very high priority
5. If target in range → use ability
6. If not in range → pathfind, move, then use if reachable
7. If can't reach → approach closest, end turn

Respects `forces_targeting` (taunt).

### movement_system
Processes `MoveUnit` messages. Validates: actor is active, has movement, path ≤ speed (or BonusMovement), destination empty and in bounds. Updates `HexPositions` + `HexOccupant`. Removes `BonusMovement` after use.

### validate_action_system
Checks: actor is active, alive, has `ap.action`, ability in list, rage/mana affordable, target alive, target in range (`hex_distance ≤ ability.range`, skipped for range == 0).

### resolve_action_system
Per effect type:
- **WeaponAttack**: weapon dice + STR mod → ApplyDamage (physical)
- **Damage**: ability dice + STR mod → ApplyDamage (physical)
- **SpellDamage**: ability dice + INT mod + spell_power → ApplyDamage (pierces armor)
- **Heal**: ability dice + INT mod + spell_power → ApplyHeal
- **None**: statuses only
- **GrantMovement**: resets `ap.movement = true`, inserts `BonusMovement(distance)`, does NOT send EndTurn

Subtracts rage/mana costs. Applies status effects. Sends EndTurn (except GrantMovement).

### apply_effects_system
- **Damage**: raw - armor - status armor_bonus (min 1), then + damage_taken_bonus. Piercing ignores armor/armor_bonus. Grants +1 rage to both source and target.
- **Heal**: amount capped at max_hp.
- **Death**: HP ≤ 0 → insert `Dead` component.

### advance_turn_system
1. Tick existing statuses: decrement `rounds_remaining` for statuses applied by the current actor
2. Apply pending new statuses (from ApplyStatus messages)
3. Check victory (all enemies dead) / defeat (all players dead)
4. Advance queue index; if wrapped → StartRound
5. Set next actor active, reset `ap.action = true`, `ap.movement = true`

## Status Duration

- Duration ticks on **applier's** EndTurn (not target's)
- At ability use: stored as `duration + 1` to compensate tick in same frame
- Duration 1 = active until end of applier's next turn
