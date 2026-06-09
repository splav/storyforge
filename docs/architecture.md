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
- `AwaitCommand` — 12 systems with explicit `.after()` ordering (player_command ∥ enemy_ai, queue_enemy_popup ∥ advance_turn, + pact_ai). Blocked by `combat_ready()` while animations or popups active
- `Victory` — all enemies dead. Space → advance scenario
- `Defeat` — all heroes dead. Показывает оверлей с двумя вариантами: **R / кнопка** → `RestartCombat` (перезапуск боя с сохранённой инициативой), **Esc** → MainMenu

## Module Map

```
src/
  app_state.rs      AppState + CombatPhase enums
  scenario/
    mod.rs          AdvanceScenario message, start_scenario, advance_scenario_system
    combat_scene.rs spawn_combat_scene, despawn_combatants, restart_combat_system
    init_fight.rs   ECS-free builder of the initial engine CombatState (headless tooling; see note below)
    input.rs        victory_input_system
  lib.rs            Re-exports all modules
  main.rs           App builder: resources, messages, UI/меню системы; combat-цепочка подключается через `CombatPipelinePlugin`

  core/
    mod.rs          modifier(stat), ResourceKind enum (Hp, Mana, Rage, Energy)
    ids.rs          string_id!() macro → AbilityId, StatusId, WeaponId
    rng.rs          DiceRng (LCG), DiceExpr { count, sides, bonus }

  game/
    components.rs   ECS components: HexCell, Vital, CombatStats, Speed, ActionPoints, Mana, Rage, Energy, StatusEffects (with dot_per_tick), ActiveCombatant (marker), UnitToken, etc.
    resources.rs    CombatContext, CombatObjective, TurnQueue, GameDb (metadata only: scenarios + campaigns + per-scenario content validation), SelectionState, ScenarioState, CampaignState, HexPositions, UiDirty/UiDirtyFlags
    combat_log.rs   CombatEvent enum (18 variants: +EnergyChanged, +PoisonTick, +PoisonCleansed) + CombatLog resource + CombatEvent::format()
    messages.rs     UseAbility, ValidatedAction, ApplyDamage, ApplyHeal, ApplyStatus, MoveUnit, EndTurn, RestartCombat
    bundles.rs      CombatantBundle, hero_bundle(), enemy_bundle()
    hex.rs          Grid constants, hex_distance, hex_neighbors, in_bounds
    pathfinding.rs  find_path (BFS), reachable_cells, reachable_with_paths (BFS + path reconstruction)

  content/
    content_view.rs ContentView (all overridable content) + ActiveContent resource + ContentView::load_layered (global→campaign→scenario merge by id) + effective_stats/equipment_armor helpers
    abilities.rs    AbilityDef, EffectDef (incl. Summon), ResourceCost, TargetType, AoEShape, CasterContext + parse_abilities
    statuses.rs     StatusDef (incl. causes_disadvantage) + parse_statuses
    weapons.rs      WeaponDef, HandType + parse_weapons
    armor.rs        ArmorDef, ArmorSlot + parse_armor (chest/legs/feet)
    classes.rs      ClassDef + parse_classes
    unit_templates.rs UnitTemplateDef + nested stats/equipment/resources blocks + parse_unit_templates
    encounters.rs   EncounterDef, EnemyDef, PhaseDef, AuraDef, VictoryCondition + load_encounters_from_str
    scenarios.rs    ScenarioDef (holds content: ContentView + encounters), SceneDef (Story+Combat+Choice, each with requires_flag gate), PartyMemberDef, parse_scenario_body, active_party, active_flags
    campaigns.rs    CampaignDef + directory-walking loader that builds per-scenario ContentView via load_layered
    races.rs        RaceDef, FactionDef, PathDef, CritFailEffect + parse_races

  combat/         См. [`docs/combat/`](combat/) — engine/bridge/pipeline/lifecycle документация.

  persistence/
    mod.rs          PersistencePlugin, PersistencePaths resource, detect_paths()
    paths.rs        AppPaths (config/data/cache/state via `directories::ProjectDirs`)
    settings_repo.rs  load_layered / save for user settings (config_dir/settings.toml)
    save_repo.rs    slot profiles V1 (data_dir/saves/slot_N.toml): load/save/delete/record_progress/clear_campaign

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

## Content Resolution (layered)

Game rules live in `ContentView` — a flat bag of HashMaps per content type (abilities, statuses, weapons, armor, classes, unit_templates, races, factions, paths). At load time `ContentView::load_layered(campaign_dir, scenario_dir)` reads every overridable file at three layers (global / campaign / scenario), merging **by id** with scenario winning over campaign over global.

Each `ScenarioDef` gets its own merged `content: ContentView` stored at load. On scenario entry `scenario/mod.rs::enter_scenario_at` publishes it via `commands.insert_resource(ActiveContent(scen.content.clone()))`.

Combat systems read content exclusively via `Res<ActiveContent>` — `GameDb` holds only metadata (`scenarios`, `campaigns`, `campaign_order`) and runs validation per-scenario against that scenario's merged view. There is no "global abilities map" at runtime: every lookup is scoped to the currently-active scenario.

Tests that don't enter a scenario construct a `ContentView::load_global_for_tests()` (global layer only) and wrap it in `ActiveContent`.

## Scenario Scene Flow

`ScenarioDef.scenes` is a flat `Vec<SceneDef>` (Story / Combat / Choice). `advance_scenario_system` increments `ScenarioState.scene_index` by 1 then calls `skip_skipped`, which walks forward past any scenes that should be auto-skipped, returning `None` if all remaining scenes are skipped (→ scenario finish). `enter_scenario_at` (save-load reentry) calls the same helper.

### Skip reasons

A scene is skipped when **either** condition holds:

| Condition | Mechanism |
|-----------|-----------|
| `is_invisible()` | `Story` scene with `lines = []` — silent party-change beat |
| `requires_flag` absent | `SceneDef` has `requires_flag: Some("flag")` and `"flag" ∉ CampaignState.flags` |

The two reasons compose: an invisible scene is always skipped regardless of its `requires_flag`.

### `requires_flag` semantics

- Declared per `[[scenes]]` entry in `scenario.toml` as `requires_flag = "flag_name"` (optional; omitting it means always play).
- Applies to all three variant types: `Story`, `Combat`, `Choice`.
- When the flag is absent at skip-resolution time, the scene is treated as if it doesn't exist. Execution resumes at the next non-skipped scene.
- **Combat scene contract**: skipping a `Combat` scene discards its `on_victory_flags` and encounter objectives — those flags are never written to `CampaignState`. Any downstream scene that needs a flag from a skippable fight must receive it via the branching `Choice` option or a dedicated `Story` scene instead.
- Non-campaign scenarios (no `CampaignState`) treat flags as empty: all `requires_flag`-gated scenes are skipped.

Individual `DialogueLine` entries within a scene also support `requires_flag` (show line only when flag present) and `excludes_flag` (show line only when flag absent — the "else" branch companion).

### All-gated tail

If `skip_skipped` returns `None` (all remaining scenes gated/invisible), the scenario finishes gracefully — same path as reaching the last scene normally. `enter_scenario_at` does the same (no panic) to handle save-load that lands on an all-gated tail.

## `init_fight` — ECS-free combat bootstrap

`src/scenario/init_fight.rs` builds the initial engine `CombatState` purely from data — `(ContentView, ScenarioDef, scene_index, EncounterDef, rng_seed, UnitId-assigner)` — with **no ECS access**. It is the Bevy-free counterpart to the live `spawn_combatants` + `from_ecs` bootstrap, and `tests/init_fight_equivalence.rs` asserts the two produce a field-equivalent `CombatState` when fed the same UnitIds and seed.

**Status: a building block, not yet wired into the live game.** The running game still bootstraps combat through `spawn_combatants` + `from_ecs`; the cutover was deliberately not done. `init_fight` exists as the foundation for **headless tooling** (e.g. a future balance-sim that runs fights without a window). The id schemes need not match: UnitIds are recorded in engine traces and read back on replay, so a headless simulator can mint dense `0..N` ids while the game uses its own — the trace is the contract, not a shared id space.

## Data Files

```
assets/data/
  abilities.toml        Ability definitions
  statuses.toml         Status effect definitions (incl. disoriented — causes_disadvantage)
  classes.toml          Player class definitions (warrior/mage/ranger)
  magic_schools.toml    Magic school domains + methods
  races.toml            Races + factions + paths
  settings.toml         Default user settings
  equipment/
    weapons.toml        Weapon definitions
    chest.toml / legs.toml / feet.toml  Armor pieces
  campaigns/
    <campaign_id>/                     # folder name = id
      campaign.toml                    # name, description, scenarios = [...]
      # Any overridable file optional at this layer. Examples:
      # unit_templates.toml / abilities.toml / statuses.toml / ...
      <scenario_id>/                   # folder name = id
        scenario.toml                  # party, scenes (no id in body)
        encounters.toml                # this scenario's encounters
        # Any overridable file optional at this layer too:
        # unit_templates.toml / abilities.toml / statuses.toml / ...
```

All overridable files (abilities, statuses, classes, weapons, armor, unit_templates, races) can appear at global / campaign / scenario level. At load time the campaign loader builds a merged `ContentView` per scenario. See `docs/content-guide.md` for schemas.

See `docs/content-guide.md` for TOML schemas (scenes, encounters, templates, phases, auras).

## UI Optimization: Dirty Flags

`ui_dirty_bridge` runs first in Combat UI. Compares resource fields via `Local<DirtyBridgePrev>` struct (not `Res::is_changed()` — avoids false positives and two-frame window). Sets bitflags in `UiDirty` resource. Each UI system checks its flag and early-returns if not dirty. First frame sets `UiDirtyFlags::all()` to initialize UI. Flags:

| Flag | Systems | Triggers |
|------|---------|----------|
| `OVERLAY` | update_hex_visuals (BFS recompute) | actor/ability/move_mode/positions/death |
| `HEX_FILL` | update_hex_visuals (cell colors) | actor/move_mode/target/positions/death |
| `LABELS` | update_hex_visuals (HP/mana/energy text) | actor/positions/vitals/mana/rage/energy |
| `ABILITY_PANEL` | update_ability_panel | actor/ability/mana/rage/energy |
| `TURN_ORDER` | update_turn_order | actor/queue/vitals/death |
| `PHASE_HINT` | update_phase_hint | actor/ability/move_mode |
| `MOVE_BTN` | update_move_button | actor/move_mode |
| `TOOLTIP` | update_hex_tooltip | hover |
| `TOKENS` | update_token_positions | positions/death |

## Persistence

- Пути определяются через `directories::ProjectDirs` (qualifier=`com`, org/app=`Storyforge`) один раз в `detect_paths()` на старте; при неудаче persistence выключается.
- **Settings** — `config_dir/settings.toml`. Layered load: при parse-ошибке файл переименовывается в `.bak`, используются bundled defaults.
- **Dev start-chapter** — `[debug] start_scenario` (→ `GameSettings.dev_start_scenario`) in `assets/data/settings.toml`. When built with the cargo `dev` feature, a **fresh** campaign starts at that scenario id instead of the first chapter (e.g. `start_scenario = "ch3"` jumps straight into chapter 3). Empty string = normal start. **Ignored in release builds** — it only takes effect under `--features dev`, so it can't ship a wrong entry point to players.
- **Saves** — `data_dir/saves/slot_{1..SLOT_COUNT}.toml`, формат versioned (`SaveSlotFile::V1`). Slot = профиль пользователя: `last_campaign` + `HashMap<campaign_id, CampaignProgress>`. Parse-ошибка → backup в `.toml.bak`, slot считается отсутствующим.
- **Логирование**: `info!` на успешную загрузку/сохранение/удаление (путь + summary), `warn!` на ошибки чтения/парсинга и неудачный backup-rename. Стартовая строка `persistence paths: …` подтверждает, что хранилище активно.
- Callers: `main.rs` (settings at startup), `ui/settings_ui.rs` (settings save, slot select/delete), `ui/main_menu_ui.rs` + `ui/modal.rs` (new game / continue / resume), `scenario/mod.rs` (`record_progress` on scene advance, `clear_campaign` on scenario finish).

## Dependencies

- `bevy 0.18` — ECS game engine
- `serde 1` + `toml 0.8` — TOML deserialization
- `bitflags 2` — UI dirty flags
- `directories` — кроссплатформенные user dirs для persistence
- No external RNG (custom LCG in `core/rng.rs`)
