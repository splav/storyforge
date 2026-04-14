# Architecture

## State Machines

### AppState (primary)

```
Boot → Story ↔ Combat → MainMenu
```

| State | Description |
|-------|-------------|
| `Boot` | Default. `start_scenario` runs at Startup, transitions to first scene |
| `Story` | Story screen overlay (text + "Continue"). Input: Space/Enter/click |
| `Combat` | Hex grid combat. Sub-state `CombatPhase` active |
| `MainMenu` | End state (scenario complete or defeat) |
| `Overworld` | Reserved, not used |

### CombatPhase (sub-state, active only in `AppState::Combat`)

```
StartRound → AwaitCommand → Victory / Defeat
```

- `StartRound` — builds turn order (initiative d20 + DEX mod, round 1 only), transitions to AwaitCommand
- `AwaitCommand` — 11 systems with explicit `.after()` ordering (player_command ∥ enemy_ai, queue_enemy_popup ∥ advance_turn). Blocked by `combat_ready()` while animations or popups active
- `Victory` — all enemies dead. Space → advance scenario
- `Defeat` — all heroes dead. Показывает оверлей с двумя вариантами: **R / кнопка** → `RestartCombat` (перезапуск боя с сохранённой инициативой), **Esc** → MainMenu

## Module Map

```
src/
  app_state.rs      AppState + CombatPhase enums
  scenario/
    mod.rs          AdvanceScenario message, start_scenario, advance_scenario_system
    combat_scene.rs spawn_combat_scene, despawn_combatants, restart_combat_system
    input.rs        victory_input_system
  lib.rs            Re-exports all modules
  main.rs           App builder: resources, messages, system registration

  core/
    mod.rs          modifier(stat) = stat >> 1
    ids.rs          string_id!() macro → AbilityId, StatusId, WeaponId
    rng.rs          DiceRng (LCG), DiceExpr { count, sides, bonus }

  game/
    components.rs   ECS components: HexCell, Vital, CombatStats, Speed, ActionPoints, Mana, Rage, StatusEffects, ActiveCombatant (marker), UnitToken, etc.
    resources.rs    CombatContext (round, encounter, turn_ending), TurnQueue, GameDb (with validation), SelectionState, ScenarioState, HexPositions (+ generation counter), UiDirty/UiDirtyFlags
    combat_log.rs   CombatEvent enum (16 variants) + CombatLog resource + CombatEvent::format() method
    messages.rs     UseAbility, ValidatedAction, ApplyDamage, ApplyHeal, ApplyStatus, MoveUnit, EndTurn, RestartCombat
    bundles.rs      CombatantBundle, hero_bundle(), enemy_bundle()
    hex.rs          Grid constants, hex_distance, hex_neighbors, in_bounds
    pathfinding.rs  find_path (BFS), reachable_cells, reachable_with_paths (BFS + path reconstruction)

  content/
    abilities.rs    AbilityDef, EffectDef, TargetType + TOML loader
    statuses.rs     StatusDef + TOML loader
    weapons.rs      WeaponDef + TOML loader
    classes.rs      ClassDef + TOML loader
    encounters.rs   EncounterDef, EnemyDef + TOML loader
    scenarios.rs    ScenarioDef, SceneDef, PartyMemberDef + TOML loader

  combat/
    turn_order.rs   Initiative rolls, turn queue construction
    turn_start.rs   Mana +1 at turn start
    skip_dead.rs    Skip dead / stunned turns
    command_input.rs  Player keyboard input (1-5, M, Tab, Enter, E, Escape)
    enemy_ai.rs     AI: ability scoring, pathfinding, movement. CombatantQ (QueryData struct)
    movement.rs     MoveUnit processing, HexPositions updates, movement animation queueing
    enemy_popup.rs  PopupCursor + queue_enemy_popup: detects enemy ability use, queues popup
    validation.rs   UseAbility → ValidatedAction (resources, range, target alive)
    resolution.rs   Dice rolls, damage/heal/status emission, resource costs
    apply_effects.rs  Damage (armor), healing, rage gain, death marking
    advance_turn.rs  Status ticks, victory/defeat, queue advance, AP reset

  ui/
    mod.rs          UI marker components (HudPhase, TurnOrderCard*, DefeatOverlay, RestartButton, …)
    animation.rs    AnimationQueue, PendingAnim, MovePath, combat_ready(), process_animation_queue, animate_movement, EnemyActionPopup + popup UI
    combat_ui.rs    HUD: phase hint, ability panel, move button (all guarded by UiDirtyFlags); defeat overlay (setup/cleanup/input/hover)
    turn_order_ui.rs  Правая панель порядка ходов: spawn_turn_order_panel, update_turn_order, update_turn_order_hp
    hex_grid/       Hex grid module (render, input, visuals): rendering, hover, click, range/move highlighting, ui_dirty_bridge, UnitToken spawning
    log_ui.rs       Combat log display + scrollbar
    console_log.rs  CombatEvent → text (delegates to CombatEvent::format())
    story_ui.rs     Story screen: text overlay + continue button
```

## Data Files

```
assets/data/
  abilities.toml    Ability definitions (11 abilities)
  statuses.toml     Status effect definitions (5 statuses)
  weapons.toml      Weapon definitions (4 weapons)
  classes.toml      Player class definitions (3 classes)
  encounters.toml   Enemy encounter templates (2 encounters)
  scenarios.toml    Scenario definitions (1 demo scenario)
```

## UI Optimization: Dirty Flags

`ui_dirty_bridge` runs first in Combat UI. Compares resource fields via `Local<DirtyBridgePrev>` struct (not `Res::is_changed()` — avoids false positives and two-frame window). Sets bitflags in `UiDirty` resource. Each UI system checks its flag and early-returns if not dirty. First frame sets `UiDirtyFlags::all()` to initialize UI. Flags:

| Flag | Systems | Triggers |
|------|---------|----------|
| `OVERLAY` | update_hex_visuals (BFS recompute) | actor/ability/move_mode/positions/death |
| `HEX_FILL` | update_hex_visuals (cell colors) | actor/move_mode/target/positions/death |
| `LABELS` | update_hex_visuals (HP/mana text) | actor/positions/vitals/mana/rage |
| `ABILITY_PANEL` | update_ability_panel | actor/ability/mana/rage |
| `TURN_ORDER` | update_turn_order | actor/queue/vitals/death |
| `PHASE_HINT` | update_phase_hint | actor/ability/move_mode |
| `MOVE_BTN` | update_move_button | actor/move_mode |
| `TOOLTIP` | update_hex_tooltip | hover |
| `TOKENS` | update_token_positions | positions/death |

## Animation System

`AnimationQueue` (VecDeque<PendingAnim>) decouples visual animations from game logic. Game state updates instantly; visuals catch up via:
- `PendingAnim::Movement` — smooth token lerp along hex path (0.12s/step)
- `PendingAnim::Popup` — enemy action popup (dismissed by Space/Esc)

`combat_ready()` run condition blocks AwaitCommand chain while animations/popups are active.

## Dependencies

- `bevy 0.18` — ECS game engine
- `serde 1` + `toml 0.8` — TOML deserialization
- `bitflags 2` — UI dirty flags
- No external RNG (custom LCG in `core/rng.rs`)
