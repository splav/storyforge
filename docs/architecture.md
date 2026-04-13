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
- `AwaitCommand` — 10 chained systems execute: input → AI → movement → validation → resolution → effects → turn advance
- `Victory` — all enemies dead. Space → advance scenario
- `Defeat` — all heroes dead. Space → MainMenu

## Module Map

```
src/
  app_state.rs      AppState + CombatPhase enums
  scenario.rs       Scenario runner: start, spawn, despawn, advance, victory/defeat input
  lib.rs            Re-exports all modules
  main.rs           App builder: resources, messages, system registration

  core/
    mod.rs          modifier(stat) = stat >> 1
    ids.rs          string_id!() macro → AbilityId, StatusId, WeaponId
    rng.rs          DiceRng (LCG), DiceExpr { count, sides, bonus }

  game/
    components.rs   ECS components: Vital, CombatStats, Speed, ActionPoints, Mana, Rage, StatusEffects, etc.
    resources.rs    CombatContext, TurnQueue, CombatLog, GameDb, SelectionState, ScenarioState, HexPositions
    messages.rs     UseAbility, ValidatedAction, ApplyDamage, ApplyHeal, ApplyStatus, MoveUnit, EndTurn, etc.
    bundles.rs      CombatantBundle, warrior_bundle(), enemy_bundle()
    hex.rs          Grid constants, hex_distance, hex_neighbors, in_bounds
    pathfinding.rs  find_path (BFS), reachable_cells (flood fill)

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
    enemy_ai.rs     AI: ability scoring, pathfinding, movement
    movement.rs     MoveUnit processing, HexPositions/HexOccupant updates
    validation.rs   UseAbility → ValidatedAction (resources, range, target alive)
    resolution.rs   Dice rolls, damage/heal/status emission, resource costs
    apply_effects.rs  Damage (armor), healing, rage gain, death marking
    advance_turn.rs  Status ticks, victory/defeat, queue advance, AP reset

  ui/
    mod.rs          UI marker components
    combat_ui.rs    HUD: phase hint, turn order, ability panel, move button
    hex_grid.rs     Hex grid rendering, hover, click, range/move highlighting
    log_ui.rs       Combat log display + scrollbar
    console_log.rs  CombatEvent → text formatting (Russian)
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

## Dependencies

- `bevy 0.18` — ECS game engine
- `serde 1` + `toml 0.8` — TOML deserialization
- No external RNG (custom LCG in `core/rng.rs`)
