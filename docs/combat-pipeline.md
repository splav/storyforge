# Combat Pipeline

## System Chain

11 systems run `.chain()` in `CombatPhase::AwaitCommand`, gated by `combat_ready()` (no active animations or popups):

```
turn_start → skip_dead → skip_stunned → player_command → enemy_ai → movement → validate → resolve → apply_effects → queue_enemy_popup → advance_turn
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
                    queue_enemy_popup
                    (if enemy used ability → PendingAnim::Popup)
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
- **M**: toggle move mode (preserves selected_ability)
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
Processes `MoveUnit` messages. Validates: actor is active, has movement, path ≤ speed (or BonusMovement), destination empty and in bounds. Updates `HexPositions` (increments `generation` counter for precise UI change detection). Removes `BonusMovement` after use. Pushes `PendingAnim::Movement` to `AnimationQueue` for smooth token animation.

### queue_enemy_popup
Scans new `CombatLog` events (via `PopupCursor` cursor). When an enemy's `AbilityUsed` is found, collects associated result events (DamageResult, HealResult, StatusApplied, UnitDied) and pushes `PendingAnim::Popup` to `AnimationQueue`. Player dismissed with Space/Esc.

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

## Animation & Pipeline Blocking

Game logic updates instantly (HexPositions, damage, statuses). Visual animations run independently and block the pipeline via `combat_ready()`:

```
combat_ready() = AnimationQueue.is_empty() && no MovePath && no EnemyActionPopup
```

When false, the AwaitCommand chain doesn't run. Animation systems (process_animation_queue, animate_movement, enemy_popup_input) run every frame regardless.

Flow: movement_system pushes `PendingAnim::Movement` → `process_animation_queue` pops it → inserts `MovePath` on token → `animate_movement` lerps at 0.12s/step → removes `MovePath` when done → `combat_ready()` becomes true → chain resumes.

Enemy popup: `queue_enemy_popup` pushes `PendingAnim::Popup` → spawned as UI overlay → player presses Space/Esc → despawned → chain resumes.

## EndTurn Ownership

`EndTurn` is sent from exactly one place per turn:

| Scenario | Who sends EndTurn |
|----------|-------------------|
| Ability used (any effect except GrantMovement) | `resolve_action_system` (line 182) |
| GrantMovement ability | NOT sent — turn continues for bonus movement |
| Enemy AI: move only, no ability in range | `enemy_ai_system` |
| Enemy AI: both resources spent, no action taken | `enemy_ai_system` |
| Player presses E (manual end) | `player_command_system` |
| Player: both action + movement spent | `player_command_system` (auto) |
| Dead/stunned actor | `skip_dead_turn_system` / `skip_stunned_turn_system` |

**Rule:** AI and player systems must NOT send EndTurn when they also send UseAbility — `resolve_action_system` handles that.

## Status Duration

- Duration ticks on **applier's** EndTurn (not target's)
- At ability use: stored as `duration + 1` to compensate tick in same frame
- Duration 1 = active until end of applier's next turn

## Known Weaknesses & Edge Cases

### EndTurn из множества мест (архитектурная проблема)

EndTurn отправляется из 5 систем. `advance_turn` дедуплицирует по actor через HashSet — это пластырь, а не решение. Причина: при оглушении `skip_stunned` ставит `ap = false/false` и шлёт EndTurn, но `enemy_ai` видит `!ap.action && !ap.movement` и шлёт второй. Dedup спасает, но скрывает баги.

**Идеальный рефакторинг:** один EndTurn writer. Например: отдельная система `auto_end_turn` в конце цепочки, которая проверяет "были ли потрачены все ресурсы или пришёл EndTurn от команды" и отправляет единственный EndTurn. Все остальные системы ставят флаг "хочу завершить ход" вместо прямого EndTurn.

### Статус на мёртвого юнита

`apply_effects_system` может пометить цель Dead, а `resolve_action_system` (раньше в кадре) уже отправил `ApplyStatus` для той же цели. `advance_turn` применит статус к трупу. Безвредно пока нет механики воскрешения, но если появится — статусы на трупах станут багом.

### Порядок movement → validation (намеренный)

Movement стоит до validation в цепочке **намеренно**: AI считает позицию назначения и пишет MoveUnit + UseAbility в одном кадре. movement обновляет HexPositions, затем validation проверяет range с новой позиции. Если переставить — range проверится со старой позиции и reject'нет валидную атаку.

### skip_stunned ставит AP = false (необходимо)

`skip_stunned` обнуляет `ap.action` и `ap.movement` перед EndTurn. Без этого `enemy_ai` (для врагов) или `player_command` (для игроков) попытаются действовать: AI найдёт способности, player_command выведет UI. Обнуление AP — способ сказать остальным системам "не трогай".

### Самоубийство через способность

Если способность наносит урон кастеру (или AoE задевает союзников), `apply_effects` пометит Dead, `advance_turn` корректно определит победу/поражение. Работает благодаря тому, что apply_effects идёт после resolution в цепочке.

### Все враги умирают в один ход

`advance_turn` проверяет `enemies_alive` / `players_alive` после каждого EndTurn. Если последний враг умер — фаза Victory выставляется немедленно, дальнейшие EndTurn не обрабатываются (`return` в цикле).

## Тесты

| Тест | Что проверяет |
|------|--------------|
| `valid_use_ability_emits_validated_action` | Валидная UseAbility → ValidatedAction |
| `wrong_actor_use_ability_is_rejected` | Чужой актор → reject |
| `no_action_point_use_ability_is_rejected` | ap.action=false → reject |
| `apply_damage_reduces_hp` | Урон уменьшает HP (armor=0) |
| `killing_all_enemies_sets_victory_phase` | Все враги мертвы → Victory |
| `killing_all_heroes_sets_defeat_phase` | Все герои мертвы → Defeat |
| `duplicate_end_turn_is_deduplicated` | Два EndTurn одного actor → один advance |
| `stunned_unit_skips_turn_and_stun_expires` | Стан пропускает ход, тикается на EndTurn наложившего |
| `stunned_enemy_no_duplicate_end_turn` | Оглушённый враг не проскакивает следующий ход |
