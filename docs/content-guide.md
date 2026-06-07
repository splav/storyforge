# Content Guide

All game content is data-driven via TOML files in `assets/data/`. No code changes needed to add content.

## Layered Content

Every content file can exist at **three layers** — global, campaign, scenario. At load time they're merged **by id** with **scenario winning over campaign winning over global**. Records are replaced **wholesale**: redefining an ability at scenario level replaces the campaign/global record for that id; you can't merge individual fields.

Each scenario gets its own merged view stored in `ScenarioDef.content` and exposed at runtime as the `ActiveContent` Bevy resource. Combat systems read from `ActiveContent`; `GameDb` holds only metadata (campaigns + scenarios).

### Overridable content types

| File                         | Contents                                  |
|------------------------------|-------------------------------------------|
| `abilities.toml`             | Ability definitions                        |
| `statuses.toml`              | Status effect definitions                  |
| `classes.toml`               | Player class definitions                   |
| `unit_templates.toml`        | Reusable combat stat-block templates       |
| `races.toml`                 | Races + factions + paths (single file)     |
| `equipment/weapons.toml`     | Weapons                                    |
| `equipment/chest.toml`       | Chest armor                                |
| `equipment/legs.toml`        | Leg armor                                  |
| `equipment/feet.toml`        | Footwear                                   |

### Non-overridable / scenario-owned

| File                         | Where                                      |
|------------------------------|--------------------------------------------|
| `magic_schools.toml`         | Global only (flavor docs; unused at runtime) |
| `settings.toml`              | Global only (user preferences)             |
| `campaign.toml`              | Per-campaign (metadata, not content)       |
| `scenario.toml`              | Per-scenario (party + scenes; metadata)    |
| `encounters.toml`            | Per-scenario (scoped to that scenario)     |

## Directory Layout

```
assets/data/
  # Global layer (all overridable files listed above)
  abilities.toml / classes.toml / statuses.toml / ...
  unit_templates.toml / races.toml / equipment/...
  magic_schools.toml / settings.toml
  campaigns/
    <campaign_id>/              # folder name IS the id
      campaign.toml             # name, description, scenarios = [...] order
      # (any overridable file, optional — overrides global for this campaign)
      <scenario_id>/            # folder name IS the id
        scenario.toml           # name, party, scenes (no id in body)
        encounters.toml         # this scenario's encounters
        # (any overridable file, optional — overrides campaign + global
        #  for this scenario only)
```

IDs come from folder names. `campaign.toml` lists scenario folders in play order.

### Override example

```
campaigns/bell_under_veil/
├── campaign.toml
├── abilities.toml              # "fireball" here replaces the global one
│                                for every scenario in this campaign
└── crypt/
    ├── scenario.toml
    ├── encounters.toml
    └── statuses.toml           # "disoriented" here replaces both
                                # global and campaign versions, in this
                                # scenario only
```

## Abilities (`abilities.toml`)

```toml
[[abilities]]
id            = "fireball"
name          = "Огненный шар"
magic_domains = ["aether", "form"]  # optional; see magic-schools.md
magic_method  = "destruction"       # optional
target_type   = "single_enemy"      # single_enemy | single_ally | myself | ground
effect        = "spell_damage"      # see table below
dice_count    = 2
dice_sides    = 3
costs         = [{ resource = "mana", amount = 5 }]
range         = 5
statuses      = [{ id = "burning", on = "target", duration = 2 }]
```

### Line-of-Sight requirement

Set `requires_los = true` on ranged abilities (range > 1) that must have unobstructed
line-of-sight to their target. If LOS is blocked by an obstacle hex, `check_legality`
returns `Err(NoLineOfSight)` and the cast is refused — for both player and AI.

```toml
[[abilities]]
id           = "bow_shot"
name         = "Выстрел из лука"
target_type  = "single_enemy"
effect       = "damage"
dice_count   = 1
dice_sides   = 8
range        = 5
min_range    = 1
requires_los = true       # blocked by obstacles from [[encounters.obstacles]]
```

Notes:
- LOS check is **skipped** for melee abilities (`range.max == 1`).
- LOS check is **skipped** when `requires_los = false` (the default).
- Obstacles are declared per-encounter via `[[encounters.obstacles]]` (see below).

### Effect Types

| Effect | Dice | Stat | Armor | Notes |
|---|:---:|:---:|:---:|---|
| `weapon_attack` | weapon | +STR | Reduced | |
| `damage` | Yes | +STR | Reduced | |
| `spell_damage` | Yes | +INT +spell_power | **Pierced** | |
| `heal` | Yes | +INT +spell_power | N/A | Capped at max_hp |
| `none` | — | — | — | Status-only |
| `grant_movement` | — | — | — | Requires `distance`, doesn't end turn |
| `restore_resources` | — | — | — | `rest`: +1 HP/mana/rage/energy |
| `summon` | — | — | — | Instantiates a unit template at a free hex near the caster. Requires `summon_template`; optional `summon_max_active`. See below. |

### Summon ability

```toml
[[abilities]]
id                = "summon_storm_spirit"
name              = "Призыв духа бури"
target_type       = "myself"
effect            = "summon"
summon_template   = "storm_spirit"   # must resolve via scenario chars + campaign templates at use site
summon_max_active = 2                # optional cap on concurrent summons from one caster
range             = 0
costs             = [{ resource = "mana", amount = 3 }]
```

Spawn rules:
- Template is looked up in the scenario's merged `ActiveContent.unit_templates` (scenario override > campaign override > global). If missing → `SummonBlocked` in log; ability fires but no unit spawns (resource cost is still paid, turn ends).
- Landing hex: nearest empty cell within a small radius of the summoner. If none free → `SummonBlocked`.
- `max_active` cap counts currently-alive summons tagged with `SummonedBy(caster)`. Reached → `SummonBlocked`.
- Spawned unit joins the turn queue at the **next** `StartRound` (`build_turn_order` sees the new combatant). Acts with `Initiative(0)` — last in order by default.
- Summons inherit their summoner's team. Death of the summoner does NOT remove summons (they outlive their caster).

### Target Types

`single_enemy` | `single_ally` | `myself` | `ground`

- `single_enemy` / `single_ally`: player picks an entity on the matching team. Required alive.
- `myself`: self-cast; no target selection.
- `ground`: player picks a **cell** (position-based). No entity target — `target` is a sentinel, the effect uses `target_pos`. Typically paired with `aoe = "circle" | "line"`; `aoe = "none"` is legal but only meaningful for position-only effects (teleport, summon-at-cell).

## Statuses (`statuses.toml`)

```toml
[[statuses]]
id                    = "burning"
name                  = "Ожог"
armor_bonus           = 0        # +armor (negative reduces)
damage_taken_bonus    = 0        # extra damage on all hits
skips_turn            = false    # unit can't act
forces_targeting      = false    # enemies must attack this unit
dot_count             = 0        # DoT dice count (with dot_sides)
dot_sides             = 0
blocks_mana_abilities = false    # can't cast mana abilities
speed_bonus           = 0        # modifies movement (clamped to 0+; speed=0 means immobile)
hp_percent_dot        = 0        # % of max_hp as DoT per tick
ai_controlled         = false    # hero acts under AI control (pact_control)
causes_disadvantage   = false    # ALL carrier's rolls are at disadvantage (disoriented)
```

## Weapons / Armor

```toml
# weapons.toml
[[weapons]]
id          = "staff"
name        = "Посох"
hand        = "two_handed"       # main_hand | off_hand | two_handed
dice_count  = 1
dice_sides  = 6
spell_power = 1
# optional stat bonuses: armor, max_hp, strength, dexterity, constitution, intelligence, wisdom, charisma
```

```toml
# chest.toml / legs.toml / feet.toml
[[items]]
id    = "plate_armor"
name  = "Латная кираса"
armor = 2
# optional stat bonuses
```

## Classes (`classes.toml`)

```toml
[[classes]]
id           = "ranger"
name         = "Следопыт"
max_hp       = 14
strength     = 2
dexterity    = 6
# constitution, intelligence, wisdom, charisma ...
speed        = 5
main_hand    = "dagger"
off_hand     = null              # optional; second weapon
chest        = "leather_vest"
legs         = "leather_pants"
feet         = "leather_boots"
ability_ids  = ["melee_attack", "bow_shot"]
mana_max     = 0
rage_max     = 0
energy_max   = 6
```

## Unit Templates

Reusable combat stat blocks referenced by encounters, phases, and summon abilities.

Like every overridable content type, `unit_templates.toml` can live at global / campaign / scenario level. The scenario's merged view (scenario > campaign > global by id) is what combat reads from.

```toml
[[unit_templates]]
id    = "stormborn_echo"
name  = "Бурешаман"
race  = "stormborn"
faction = "..."                  # optional
path    = "heritage"             # optional (determines crit fail)
speed = 3

stats     = { max_hp = 30, strength = 4, dexterity = -2, constitution = 8, intelligence = 6, wisdom = 6, charisma = 0 }
equipment = { main_hand = "staff", chest = "chainmail", legs = "plate_greaves", feet = "iron_boots" }
resources = { mana = 8 }         # optional; defaults to {mana=0, rage=0, energy=0}

ability_ids = ["melee_attack", "thunderstrike", "heal"]
```

AI-роль не задаётся в контенте — `AxisProfile` (tank/melee/ranged/control/support) выводится из набора способностей, HP и брони через `infer_profile` при спауне юнита.

### Initial statuses (permanent statuses applied at every spawn)

Optional `initial_statuses` lists status ids that are applied to every unit spawned from this template, with `combat_engine::PERMANENT_DURATION` (sentinel — `ExpireStatus` guards and never decrements). Used for **non-acting allies** like the wounded magister in ch2 bой 2: a permanent `stunned` makes the engine skip every one of his turns through the standard `skip_stunned_turn_system`, while still letting party AI heal him and `keep_alive` victory track his HP.

```toml
[[unit_templates]]
id    = "wounded_magister"
name  = "Магистр"
race  = "human"
# ... stats / equipment as usual ...
initial_statuses = ["stunned"]   # always spawns stunned, turn auto-skips
```

Applies to all spawn paths: bootstrap (party / enemy entities) and mid-combat `Effect::Spawn` (summons). Each status starts with no source-applier semantics — the unit itself is recorded as `applier`.

Pair with the **temporary party member** pattern in Scenario → `party_add` below to wire an NPC-style ally into combat through the standard party flow without ad-hoc encounter sections.

## Encounters (`encounters.toml` inside a scenario folder)

```toml
[[encounters]]
id   = "stormborn_camp"
name = "Стоянка грозорождённых"

# Optional. Default = all_enemies_dead. `marker_color` drives the red ring
# drawn under the target's token; RGB in 0..1.
victory = { type = "kill_target", enemy_name = "Старшина", marker_color = [0.90, 0.15, 0.15] }

[[encounters.enemies]]
name        = "Воин"
race        = "stormborn"
speed       = 3
stats       = { max_hp = 18, strength = 8, dexterity = -2, constitution = 5, intelligence = -4, wisdom = -2, charisma = -4 }
equipment   = { main_hand = "long_sword", chest = "plate_armor", legs = "leather_pants", feet = "leather_boots" }
ability_ids = ["melee_attack"]
hex_col     = 6
hex_row     = 2
```

### Enemy via template

When `template` is set, scalar fields (`name`, `race`, `speed`, `ability_ids`, `faction`, `path`) can be overridden individually; blocks (`stats`, `equipment`, `resources`) are **all-or-nothing** — include the whole block to override, omit to inherit. `hex_col` / `hex_row` are always required.

```toml
[[encounters.enemies]]
template = "stormborn_echo"      # scenario chars first, then campaign templates
name     = "Старшина"            # override leaf
hex_col  = 5
hex_row  = 3
```

### Phases

Boss transforms when a trigger fires. At most one phase per frame; pending phases fire in declaration order. **In-place mutation**: same entity, same turn position, `VictoryTarget` preserved — so `kill_target` means "kill through all phases".

```toml
[[encounters.enemies.phases]]
hp_below_pct = 1                 # fires once at HP ≤ 1% of max
template     = "stormborn_echo"  # inherit stats/abilities from template
heal_to_full = true              # refill HP after transform, removes Dead
# name, stats, ability_ids — individual overrides on top of template
flavor       = "Старшина падает — но буря в его крови не даёт ему умереть..."
```

Без template поля `name`/`stats`/`ability_ids` задаются напрямую (всё необязательное — что не указано, остаётся от текущего состояния босса).

`flavor` — сюжетная строка. Показывается в попапе перехода и в combat log.

#### Phase overrides: victory, deadline, AI behaviour

A phase can do more than restat the boss — it can rewrite the fight's win condition, impose a round deadline, and flip the unit into a non-standard AI regime. All three are optional and independent.

```toml
[[encounters.enemies.phases]]
hp_below_pct    = 30
flavor          = "Босс бросает оружие и пытается бежать."
ai_behavior     = "flee"                    # AI regime override (see below)
turn_limit      = 3                          # rounds from THIS phase firing
victory_override = { type = "kill_target", enemy_name = "Тибор Колм", marker_color = [0.9, 0.15, 0.15] }
```

- **`victory_override`** — replaces the encounter's `victory` condition the moment this phase fires. Same record format as the top-level `victory` field (`kill_target` / `keep_alive` / `all_enemies_dead` / `all_of`). Use it to shift the goal mid-fight, e.g. from "kill everyone" to "finish the fleeing boss". The override is total — the prior condition (including any `keep_alive` clauses) no longer applies once it activates.
- **`turn_limit`** — a **round-based** deadline counted from the round the phase activates. If `turn_limit` rounds elapse and the override's target is still alive, the fight is **lost** (the boss "escaped"). Pair it with a `kill_target` `victory_override` so "catch them in N rounds" is enforceable. Counting is per round (full initiative cycle), not per actor-turn.
- **`ai_behavior`** — forces the unit's AI evaluation regime for the rest of the fight. Currently the only value is `"flee"`: each turn the unit moves to **maximise distance from the nearest enemy**, **all offensive casts are suppressed**, and **self-heal / self-buff are allowed** (a fleeing boss may still try to patch itself up). When cornered (no move increases distance) it simply ends its turn. The unit does not retaliate even if it could land a lethal hit — by design (see [docs/ai/adaptation.md](ai/adaptation.md), `EvaluationMode::Flee`).

Canonical combo (ch2 boss, "Колм"): at low HP the boss enters a phase that sets `ai_behavior = "flee"` + `turn_limit` + a `kill_target` `victory_override` on itself — the party must run it down and kill it before it gets away.

### Aura

Passive radius effect. While the source is alive, targets in range matching `affects` get the status re-applied every TurnStart. Removed when source dies or target leaves range. Uses `duration = 1` under the hood; ability-applied statuses of the same id are NOT stomped.

```toml
aura = { status = "disoriented", radius = 2, affects = "enemies" }
# affects: enemies | allies | all (default: enemies)
```

### Immobility

`speed = 0` → enemy doesn't move. AI plans from its starting tile only; `movement_system` rejects any `MoveUnit` on such an actor. Status `speed_bonus` is clamped to 0+, so debuffs can't move an immobile unit either. **Note:** a positive speed status (e.g. haste) would allow movement — keep objective-anchor units free from such buffs.

### Obstacles (`[[encounters.obstacles]]`)

Static impassable tiles — boxes, rubble, walls, etc. Block both **movement pass-through** and **stopping** for all units. Also block LOS for abilities with `requires_los = true`.

Each entry needs only a hex position. The encounter can have zero or more obstacles.

```toml
[[encounters.obstacles]]
hex_col = 5
hex_row = 3

[[encounters.obstacles]]
hex_col = 5
hex_row = 4

[[encounters.obstacles]]
hex_col = 5
hex_row = 5
```

Internally, these are stored in `CombatState.blocked_hexes` (a `HashSet<Hex>`) on combat bootstrap. They persist for the entire encounter and are cleared automatically on encounter exit or restart.

### Victory (`victory`)

```toml
# Default — all enemies dead.
# (omit the field entirely or set explicitly)
victory = { type = "all_enemies_dead" }

# Kill one specific enemy (may have multiple enemies alive).
victory = { type = "kill_target", enemy_name = "Старшина", marker_color = [0.90, 0.15, 0.15] }

# Protect a named unit — combat is lost immediately if the unit dies.
# `target_name` must match an enemy in this encounter OR a party member
# (regular hero or template-based NPC ally added via `party_add`).
# Validated at scenario load — a typo fails fast.
# This is a leaf condition — must be combined via all_of to also produce a win.
victory = { type = "keep_alive", target_name = "Магистр", marker_color = [0.3, 0.6, 1.0] }

# Conjunction — all sub-conditions must hold simultaneously.
# Defeat short-circuits: the first sub-condition that returns defeat ends the fight.
# Victory fires when every sub-condition has resolved to true.
victory = { type = "all_of", conditions = [
    { type = "all_enemies_dead" },
    { type = "keep_alive", target_name = "Магистр", marker_color = [0.3, 0.6, 1.0] },
] }
```

`all_of` nests arbitrarily.

A boss **phase** can replace the active victory condition mid-fight via `victory_override` (optionally with a round `turn_limit`) — see [Phase overrides](#phase-overrides-victory-deadline-ai-behaviour) above.

## Scenario (`scenario.toml` inside a scenario folder)

The scenario file does NOT contain its id — folder name is the id.

```toml
name = "Тропа через пограничье"

# Starting party
[[party]]
name    = "Aldric"
race    = "human"
faction = "aurum"        # optional
path    = "heritage"     # optional (determines crit fail)
class   = "warrior"
hex_col = 1
hex_row = 2

# Scenes play in order
[[scenes]]
type = "story"
lines = [
  { speaker = "Рассказчик", text = "Отряд пробирается через тёмный лес." },
  { speaker = "Kael", text = "Они бежали от чего-то хуже.", requires_flag = "beastblood_routed" },
]
# Optional side-effects applied when the player advances past this scene:
[[scenes.party_add]]
name = "Kael"
race = "human"
class = "ranger"
hex_col = 0
hex_row = 3
# [[scenes.party_remove]] — names to drop

[[scenes]]
type             = "combat"
encounter        = "beastblood_raid"  # looks up THIS scenario's encounters.toml
location         = "hills"            # optional; selects assets/images/battle_backgrounds/<location>.png
on_victory_flags = ["beastblood_routed"]
```

### Scene types

- **`story`** — dialogue. `lines` is a list of `{speaker, text, requires_flag?}`. Lines with `requires_flag` only show if that flag was set by an earlier victory. Player advances line-by-line (Space / Enter / button); previous lines stay on screen.
  - `party_add` / `party_remove` apply when the player advances past the last line.
  - **If `lines = []`** (or omitted), the scene is **invisible** — `advance_scenario` skips past it. Use this idiom for a pure party-change beat without dialogue.

- **`combat`** — fight. `encounter` refers to this scenario's `encounters.toml`. On the outcome, campaign flags are recorded (see **Combat-outcome flags** below); `requires_flag` on future dialogue lines checks against the persisted flag set.

### Combat-outcome flags (two mechanisms)

A combat can record campaign flags when it ends. There are **two deliberate mechanisms** for
two different intents — pick by whether the flag is **conditional**:

| | `on_victory_flags` | `objectives` |
|---|---|---|
| Lives on | the **scene** (`scenario.toml`) | the **encounter** (`encounters.toml`) |
| Fires when | combat is **won** (any victory) | a per-objective **condition holds in the final state** — on victory **or** a proceed-defeat |
| Conditional? | no — unconditional marker | yes — evaluated against who is alive at the end |
| Use for | "bout N cleared" narrative markers | secondary goals ("the boat survived", "the novice lived"), incl. lose-but-proceed bonuses |

```toml
# scenario.toml — unconditional marker, set on any victory
[[scenes]]
type = "combat"
encounter = "harbor_landing"
on_victory_flags = ["reached_island"]

# encounters.toml — conditional secondary objective + lose-but-proceed
[[encounters]]
id = "harbor_landing"
on_defeat = "proceed"            # losing still advances the scenario (default: "retry")
[[encounters.objectives]]
id = "boat_saved"                # flag recorded iff the condition below holds at combat end
hidden = true                    # not shown in the HUD goal line
[encounters.objectives.condition]
type = "keep_alive"
target_name = "Лодка"
```

- **`on_defeat`** (`retry` default | `proceed`): `proceed` shows a "Продолжить" button on the
  defeat overlay and advances the scenario instead of restarting. **Objectives are still
  evaluated on a proceed-defeat** — a flag can be earned in a bout you lost.
- An objective's `condition` is any `victory`-type table (`keep_alive`, `kill_target`,
  `all_enemies_dead`, `all_of`). It is a **positive predicate on the final state**, without
  the "protected unit died → instant defeat" short-circuit that `victory` carries.
- **Invariant:** a unit that must merely *survive* the bout (e.g. the boat) goes in
  `objectives`, **never** in the encounter's `victory` — in `victory` its death is an instant
  defeat. Victory = "enemies dead"; survival = a separate objective.
- Both mechanisms write into the same persisted `CampaignState.flags` set (idempotent inserts),
  so they compose freely. They are complementary, not redundant: `objectives` is strictly more
  expressive, so if a *conditional victory-only* flag is ever needed, `on_victory_flags` can be
  folded into `objectives` as parse-time sugar (a `won`-style condition) — not done today.

### Party members: class-based vs template-based

A `[[party]]` or `party_add` entry can be one of two shapes:

- **Class-based hero** (regular playable character) — provides `class = "warrior"`. The hero gets full stats / equipment / abilities from the class, owns its own turn, is player-controllable.

- **Template-based NPC ally** (non-acting or pre-statted unit) — provides `template = "wounded_magister"` instead of `class`. The unit is spawned from a `[[unit_templates]]` entry (stats, equipment, abilities, plus any `initial_statuses` like permanent `stunned`). Lives in `CombatState.units` as a full party member, but if its template carries permanent stun the engine auto-skips its turns via the standard `skip_stunned_turn_system`. Still healable by party AI; `keep_alive` victory tracks its HP.

```toml
# Story scene that introduces a wounded NPC ally before combat.
[[scenes]]
type = "story"
lines = [
  { speaker = "Рассказчик", text = "Перед вами лежит без сознания Магистр." },
]
[[scenes.party_add]]
name     = "Магистр"
template = "wounded_magister"    # template path — not class
hex_col  = 6
hex_row  = 4
# `class` omitted; `race` / `faction` / `path` inferred from template.

# Combat scene with keep_alive on the temporary ally.
[[scenes]]
type      = "combat"
encounter = "shrine_defence"

# Subsequent story scene removes the NPC from the party once the bout is over.
[[scenes]]
type = "story"
lines = []                       # invisible, pure party-change beat
[[scenes.party_remove]]
name = "Магистр"
```

This pattern fully replaces the legacy `[[encounters.npcs]]` section — no per-encounter NPC wiring; every unit goes through the unified party flow.

### Derived state (no runtime storage)

- **Active party** at scene N = starting `[[party]]` + all `party_add` / `party_remove` from story scenes 0..N-1, folded in order. Save files only store `scene_index`; the party is re-derived on load.
- **Flags are persisted, not derived.** Combat outcomes write into `CampaignState.flags`
  (saved in `CampaignProgress.flags`, restored on load) via the two mechanisms above. Earlier
  builds re-scanned `on_victory_flags` from all prior combat scenes each frame; that derivation
  (`active_flags`) was removed in favor of the persistent set.

## Campaign (`campaign.toml`)

```toml
# id = folder name (e.g. "demo_campaign"). Not repeated in the file.
name        = "Тропа через пограничье"
description = "Демо-кампания"
scenarios   = ["demo"]    # order of scenario folders to play through
```

## Template Resolution Order

All content lookups during a scenario go through `ActiveContent`, which is the merged `ContentView` for that scenario. The merge order, by id:

1. Scenario layer (`campaigns/<c>/<s>/*.toml`) wins.
2. Campaign layer (`campaigns/<c>/*.toml`) next.
3. Global layer (`assets/data/*.toml`) base.

So an encounter enemy's `template = "morok"` resolves to:
- the scenario's `unit_templates.toml` `morok` if present, else
- the campaign's `unit_templates.toml` `morok` if present, else
- the global `unit_templates.toml` `morok`, else panic at load time.

Cross-scenario references are not allowed (each scenario has its own scope).

## Validation

`GameDb::default()` validates every scenario's **merged** content view at startup and panics on broken references. Checks:

- Every ability, class, and unit template in a scenario's view references only ids that exist in that same view (no dangling refs).
- Campaign `scenarios = [...]` folders exist and parse cleanly.
- Scenario `party` + `party_add` members reference real races / factions / paths / classes.
- Scene `encounter_id` resolves inside the scenario's own `encounters.toml`.
- Encounter `phases[*].template`, `aura.status`, `victory.enemy_name` resolve (and uniqueness where required).
- Party hex positions don't collide with enemy hex positions at each combat scene (using the computed active party).

An authoring bug fails loudly at startup rather than at runtime.
