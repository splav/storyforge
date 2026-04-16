# Combat Pipeline

## System Chain

12 systems in `CombatPhase::AwaitCommand`, gated by `combat_ready()` (no active animations or popups). Ordered via `.after()` — parallel where independent:

```
turn_start → skip_dead → skip_stunned ─┬→ pact_ai ─→ player_command ─┬→ movement → validate → resolve → apply_effects ─┬→ queue_enemy_popup
                                        └→ enemy_ai ─────────────────┘                                                  └→ advance_turn
```

`pact_ai` обрабатывает героев под статусом `pact_control` (крит-провал пакта) — они действуют как AI, стоит до `player_command` чтобы перехватить ход.

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
Fires once per turn (when `ActiveCombatant` entity differs from `Local<Option<Entity>>`). Restores +1 mana and +1 energy to current actor.

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
- Auto-ends turn when both `action` and `movement` are false (guarded by `turn_ending` flag)

Mouse input (via `hex_click_target`):
- **Click occupied hex**: select target
- **Double-click occupied hex**: UseAbility on target
- **Click empty hex in move_mode**: move there
- **Double-click empty hex**: move there (without entering move_mode)

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
Checks: actor is active, alive, has `ap.action`, ability in list, rage/mana affordable, target alive, target in range (`hex_distance ≤ ability.range`, skipped for range == 0). If validation rejects the ability, sends `EndTurn` to prevent infinite loops (e.g. AI picks MoveAndCast but target ends up out of range after movement).

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
- **Damage**: `max(1, raw - armor - armor_bonus + damage_taken_bonus)`. Piercing ignores armor/armor_bonus. Grants +1 rage to both source and target.
- **Heal**: amount capped at max_hp.
- **Death**: HP ≤ 0 → insert `Dead` component.

### advance_turn_system
1. Tick existing statuses: decrement `rounds_remaining` for statuses applied by the current actor
2. Apply pending new statuses (from ApplyStatus messages)
3. Check victory (all enemies dead) → `CombatPhase::Victory` / defeat (all players dead) → `CombatPhase::Defeat`
4. Advance queue index; if wrapped → StartRound
5. Insert `ActiveCombatant` on next actor, reset `ap.action = true`, `ap.movement = true`

## Animation & Pipeline Blocking

Game logic updates instantly (HexPositions, damage, statuses). Visual animations run independently and block the pipeline via `combat_ready()`:

```
combat_ready() = AnimationQueue.is_empty() && no MovePath && no EnemyActionPopup
```

When false, the AwaitCommand chain doesn't run. Animation systems (process_animation_queue, animate_movement, enemy_popup_input) run every frame regardless.

Flow: movement_system pushes `PendingAnim::Movement` → `process_animation_queue` pops it → inserts `MovePath` on token → `animate_movement` lerps at 0.12s/step → removes `MovePath` when done → `combat_ready()` becomes true → chain resumes.

Enemy popup: `queue_enemy_popup` pushes `PendingAnim::Popup` → spawned as UI overlay → player presses Space/Esc → despawned → chain resumes.

## EndTurn Ownership

EndTurn отправляется из нескольких систем. Дублирование предотвращается через `CombatContext.turn_ending: bool` — мгновенно виден всем системам в цепочке (в отличие от Commands-based компонентов, которые применяются отложенно). Активный комбатант определяется маркерным компонентом `ActiveCombatant` на сущности (не полем ресурса).

| Scenario | Who sends EndTurn | Sets turn_ending |
|----------|-------------------|-----------------|
| Dead actor | `skip_dead_turn_system` | yes |
| Stunned actor | `skip_stunned_turn_system` | yes |
| Ability used (non-GrantMovement) | `resolve_action_system` | yes |
| Player presses E | `player_command_system` | no (unique) |
| Player: both AP spent | `player_command_system` | no (guarded by `!ctx.turn_ending`) |
| Enemy: both AP spent | `enemy_ai_system` | no (guarded by `!ctx.turn_ending`) |
| Enemy: move-only / approach | `enemy_ai_system` | no (unique) |

**Lifecycle:** системы-источники ставят `ctx.turn_ending = true` → последующие системы проверяют флаг и не дублируют EndTurn → `advance_turn` сбрасывает `ctx.turn_ending = false` и переносит `ActiveCombatant` на следующего → `spawn_combat_scene`, `start_combat_system` и `restart_combat_system` сбрасывают при входе в новый бой.

**Rule:** AI и player системы НЕ отправляют EndTurn когда шлют UseAbility — `resolve_action_system` это делает. AI и player проверяют `turn_ending` перед auto-end.

## Status Duration

- Duration ticks on **applier's** EndTurn (not target's)
- New statuses applied AFTER existing ones are ticked (no +1 hack needed)
- Duration 1 = active until end of applier's next turn
- `advance_turn` пропускает Dead-таргеты при наложении новых статусов

## Known Edge Cases

### Порядок movement → validation (намеренный)

Movement стоит до validation в цепочке **намеренно**: AI считает позицию назначения и пишет MoveUnit + UseAbility в одном кадре. movement обновляет HexPositions, затем validation проверяет range с новой позиции. Если переставить — range проверится со старой позиции и reject'нет валидную атаку.

### skip_stunned ставит AP = false (необходимо)

`skip_stunned` обнуляет `ap.action` и `ap.movement` перед EndTurn. Без этого `enemy_ai` (для врагов) или `player_command` (для игроков) попытаются действовать. Обнуление AP — дополнительная защита помимо `ctx.turn_ending`.

### Самоубийство через способность

Если способность наносит урон кастеру, `apply_effects` пометит Dead, `advance_turn` корректно определит победу/поражение. Работает благодаря порядку систем в цепочке.

### Все враги умирают в один ход

`advance_turn` проверяет `enemies_alive` / `players_alive` после каждого EndTurn. Если последний враг умер — фаза Victory немедленно, `return` из цикла.

## Перезапуск боя (Defeat → Restart)

При получении сообщения `RestartCombat` система `restart_combat_system` выполняет **полный despawn + respawn** без перебрасывания инициативы:

1. Сохраняет инициативу всех комбатантов по имени в `PresetInitiative`
2. Despawn всех комбатантов, токенов, попапов
3. Spawn свежих комбатантов через `spawn_combatants`
4. `reset_combat_state`: `ctx.round = 0`, `ctx.turn_ending = false`, очистка `CombatLog`, `AnimationQueue`, курсоров. `ActiveCombatant` снимается при despawn сущностей
5. Переход в `CombatPhase::StartRound` → `build_turn_order` делает `round += 1` (= 1), видит непустой `PresetInitiative` → восстанавливает сохранённые значения вместо бросков

**Инициатива:** значения сохраняются в `PresetInitiative` (HashMap<String, i32>) по имени, а не на сущностях (сущности пересоздаются). `build_turn_order` при `first_round && !preset.is_empty()` берёт значения из preset и очищает его.

## Тесты

| Тест | Что проверяет |
|------|--------------|
| `valid_use_ability_emits_validated_action` | Валидная UseAbility → ValidatedAction |
| `wrong_actor_use_ability_is_rejected` | Чужой актор → reject |
| `no_action_point_use_ability_is_rejected` | ap.action=false → reject |
| `apply_damage_reduces_hp` | Урон уменьшает HP (armor=0) |
| `killing_all_enemies_sets_victory_phase` | Все враги мертвы → Victory |
| `killing_all_heroes_sets_defeat_phase` | Все герои мертвы → Defeat |
| `turn_ending_flag_cleared_on_advance` | EndTurn продвигает ход, turn_ending сбрасывается |
| `stunned_unit_skips_turn_and_stun_expires` | Стан пропускает ход, тикается на EndTurn наложившего |
| `stunned_enemy_no_duplicate_end_turn` | Оглушённый враг не проскакивает следующий ход |
