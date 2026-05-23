# Combat Pipeline

> **Post-unisim note (2026-05-18):** the "System Details" section below names
> several systems that were absorbed into the engine and deleted during
> Phases 3–5 (`apply_auras_system`, `movement_system`, `validate_action_system`,
> `resolve_action_system`, `apply_effects_system`, `tick_status_effects_system`,
> `phase_transition_system`, `skip_dead_turn_system`). The actual current
> pipeline is two systems: `process_action_system` (consumes `ActionInput`,
> calls engine `step()`, translates events) and `project_state_to_ecs`
> (writes engine state back to ECS), chained with `apply_phase_transitions_system`
> and `flush_pending_ai_log_system`. See
> [`engine-architecture.md`](engine-architecture.md) §3 for the canonical
> schedule.
>
> **Post-V3-bootstrap note (2026-05-23):** `engine_turn_start_system`,
> `engine_start_first_turn_system`, and `init_state_from_ecs` have all been
> deleted. Their responsibilities are now consolidated in
> `bootstrap_combat_state`, which runs at the end of the
> `CombatPhase::StartRound` chain (after `build_turn_order`) and is one-shot
> per encounter via an `units().is_empty()` idempotency guard. It calls
> `from_ecs` (content-aware: recomputes status-derived aggregates), populates
> per-unit fields (`caster_context`, `aoo_dice`, `auras`, `enemy_phases`),
> sets the turn queue, and primes the first actor. `write_engine_trace_init_system`
> is chained immediately after bootstrap in the same StartRound chain.
> `reset_engine_mirrors_on_exit_combat` + `reset_engine_mirrors_on_restart`
> clear engine-side resources at combat end. `CombatStep::TurnStart` set is
> empty. See [`engine-architecture.md`](engine-architecture.md) §3 for the
> canonical schedule.

## System Chain

Systems in `CombatPhase::AwaitCommand`, grouped by `CombatStep` sets, gated by `combat_ready()` (no active animations or popups). Ordered via `.chain()` within sets, sets themselves chained: `TurnStart → Command → Execute → Finalize`.

Регистрация — в `combat::pipeline::CombatPipelinePlugin` (`src/combat/pipeline.rs`): plugin инкапсулирует `configure_sets` и `add_systems` для StartRound и всех четырёх `CombatStep`. `main.rs` подключает его одной строкой `.add_plugins(CombatPipelinePlugin)`.

`CombatPhase::StartRound` chain (runs once per round before AwaitCommand):
`project_state_to_ecs → assign_hex_positions → build_turn_order → bootstrap_combat_state → write_engine_trace_init_system`. Bootstrap is idempotent — only the first pass populates the engine state.

```
TurnStart: turn_start → tick_status_effects → skip_dead → skip_stunned → apply_auras
Command:   pact_ai → player_command ∥ enemy_ai
Execute:   movement → validate → resolve → apply_effects → apply_spawn → phase_transition
Finalize:  queue_enemy_popup ∥ advance_turn
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
                    phase_transition_system
                    (HpBelowPct → revive / retrain; ловит как урон
                    из apply_effects, так и DoT-смерть от тика
                    предыдущего TurnStart)
                              │
                    queue_enemy_popup
                    (if enemy used ability → PendingAnim::Popup)
                              │
                         EndTurn { actor }
                              │
                    advance_turn_system
                    (apply new statuses, victory/defeat, next actor, reset AP)

--- следующий кадр (новый активный) ---

                    tick_status_effects_system (TurnStart)
                    (однократно при смене ActiveCombatant:
                    тикает все статусы, где applier == active;
                    DoT-урон → Dead; ход наложения не тикается,
                    так как свежий статус попадает в effects только
                    в advance_turn того же кадра — позже, чем тик)
```

## System Details

### turn_start_system
Fires once per turn (when `ActiveCombatant` entity differs from `Local<Option<Entity>>`). Restores +1 mana and +1 energy to current actor.

### skip_dead_turn_system / skip_stunned_turn_system
Dead actor → immediate EndTurn. Stunned actor (`skips_turn` status) → `ap.action = false`, `ap.movement = false`, EndTurn.

### apply_auras_system
Runs in `TurnStart` after skip_stunned. For each alive `AuraSource`, applies its status (with `duration = 1`, `applier = source`) to every entity in range matching `affects`. Removes aura-applied statuses from targets whose source died or who left the radius. Never stomps a same-id status applied by a non-aura means (ability cast survives, aura re-covers after it expires). Known limitation: there's a 1-turn lag when a unit enters a radius mid-turn — disadvantage kicks in starting the next turn.

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
Only for `Team::Enemy`. Runs `run_ai_turn` (shared with `pact_ai_system`). Each tick:
1. Builds fresh `BattleSnapshot` + influence maps.
2. Always calls `pick_action` (beam search over intent → plan pool → scoring → pick).
3. **Plan freeze** (`ai_freeze_plan_after_move = true`, default): if the previous tick was a `MoveOnly` decision, the stored plan is validated against current world state. If valid, continues with `step[step_index]` from the stored plan instead of following the fresh plan. This suppresses the "move forward → replan backward" oscillation caused by non-monotonic scoring under partial plan execution.
4. Invalidation (forces fresh replan): actor HP dropped (AoO), actor rage changed, actor statuses changed, target dead/moved/HP-dropped, actor not at expected position.
5. After any Move decision, stores the full plan in `AiMemory.last_plan` for next-tick continuation.
6. No EndTurn after Move — the next tick continues or ends the turn naturally.

`pick_action` always runs regardless of freeze to produce the shadow plan for divergence logging. When logging is enabled, a `plan_divergence` JSONL entry records stored vs fresh: intent/ability/target diffs and score delta.

Respects `forces_targeting` (taunt).

### movement_system
Processes `MoveUnit` messages. Validates: actor is active, has movement, path ≤ speed + BonusMovement, destination empty and in bounds. Updates `HexPositions` (increments `generation` counter for precise UI change detection). Removes `BonusMovement` after use. Pushes `PendingAnim::Movement` to `AnimationQueue` for smooth token animation.

Walks the path step by step, firing **Attacks of Opportunity** for each enemy that was adjacent to the previous hex but not to the next. Each provoker fires at most once per round (see `Reactions` in mechanics.md), applies weapon-attack damage with normal armor mitigation, and triggers the +1 rage gain on both sides. If AoO damage drops the mover's HP to 0, the remaining path is truncated (actor ends on the step where it died, gets `Dead` inserted).

### queue_enemy_popup
Scans new `CombatLog` events (via `PopupCursor` cursor). Emits `PendingAnim::Popup` for:
- **Enemy `AbilityUsed`** — collects following result events (DamageResult, HealResult, StatusApplied, UnitDied) into one popup.
- **`PhaseEntered`** — self-contained popup with `prev_name`, "Новая фаза: next_name", and optional `flavor` narrative line.

Player dismisses with Space/Esc.

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

Subtracts rage/mana costs. Applies status effects. Sends EndTurn only when both AP and MP pools are exhausted — leftover movement after a cast lets the actor spend remaining MP (player can move or press E; AI re-plans next tick).

### apply_effects_system
- **Damage**: `max(1, raw - armor - armor_bonus + damage_taken_bonus)`. Piercing ignores armor/armor_bonus. Grants +1 rage to both source and target.
- **Heal**: amount capped at max_hp.
- **Death**: HP ≤ 0 → insert `Dead` component.

### apply_spawn_system
Processes `SpawnUnit` messages emitted by `resolve_action_system` for `EffectDef::Summon` abilities. Resolves the template via the scenario's `characters` + current campaign's `unit_templates` (scenario wins). Blocked states logged as `SummonBlocked`:
- Template id not found in either pool.
- No free hex within the summon search radius.
- Caster's concurrent-summon count ≥ `max_active`.

On success: spawns the combatant entity with full bundle + `SummonedBy(caster)`, inserts into `HexPositions`, spawns a `UnitToken` (the normal `assign_hex_positions` path only runs at StartRound and is intentionally skipped here). New unit joins the turn queue at the next StartRound with `Initiative(0)`.

### tick_status_effects_system
Runs in `TurnStart` **after** `turn_start_system` and **before** `skip_dead_turn_system` / `apply_auras_system`. Срабатывает один раз при смене `ActiveCombatant` (`Local<Option<Entity>>`-паттерн): тикает все статусы, где `applier == active`. DoT/percent-DoT наносят урон напрямую (лог `PoisonTick`), на HP≤0 ставит `Dead`, истёкшие удаляет с логом `StatusExpired`.

Почему тик на **TurnStart повесившего**, а не на его же EndTurn:

* Падение HP от DoT оказывается в самом начале кадра — `phase_transition_system` в `Execute` того же кадра успевает оживить фазированного босса до `check_combat_end` в `advance_turn_system` (`Finalize`). Без этого порядка умерший от яда босс-мишень схлопывал бой в `Victory` до фазового перехода.
* Семантика «длительность в ходах повесившего» сохраняется: новые статусы попадают в `StatusEffects` только в `advance_turn_system` (Finalize), то есть **позже** тика этого же кадра. Первый тик свежеприменённого статуса происходит на **следующем** TurnStart повесившего, а не в ходе наложения.
* Ставится **до** `apply_auras`, чтобы стек-листы свежих аура-статусов (`rounds=1`, applier=source) не тикались в тот же кадр, в котором были выставлены.

### phase_transition_system
Runs in `Execute` **after** `apply_effects_system` and **before** `advance_turn_system`. For each enemy with pending `EnemyPhases`: if the first phase's trigger fires (`HpBelowPct`), the phase is applied in-place (new `CombatStats`, `Abilities`, `AxisProfile`, optional heal-to-full, name rename, `Dead` removal). Emits `CombatEvent::PhaseEntered { prev_name, next_name, flavor }` which `queue_enemy_popup` turns into a popup. The `VictoryTarget` marker is NOT removed on transition — a `kill_target` boss must be defeated through all phases.

### advance_turn_system
1. Apply pending new statuses (from ApplyStatus messages); skip Dead targets. Runs every frame so statuses resolved mid-turn (player casts, then moves) take effect immediately, not only at EndTurn.
2. Check victory via `CombatObjective`: `AllEnemiesDead` (no living enemy) or `KillTarget` (no living entity with `VictoryTarget`); defeat when no players alive → `CombatPhase::Victory/Defeat`
3. Advance queue index, skipping Dead entities; orphaned statuses applied by Dead units still tick during the scan so they expire on schedule
4. Recheck victory (orphan ticks may have killed the last remaining unit)
5. Insert `ActiveCombatant` on next actor, reset `ap.action = true`, `ap.movement = true`; wrapped queue → `CombatPhase::StartRound`

DoT-тик активного бойца живёт в `tick_status_effects_system` (TurnStart); `advance_turn` его не применяет, а только тикает осиротевшие статусы от мёртвых в queue-скане (шаг 3).

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
