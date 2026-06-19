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
id           = "ranged_shot"
name         = "Выстрел из лука"
target_type  = "single_enemy"
effect       = "weapon_attack"
ranged       = true          # uses ranged_dice channel + dex_mod
power        = 0.5           # optional; scales the weapon roll (default 1.0)
range        = 5
min_range    = 1
requires_los = true       # blocked by obstacles from [[encounters.obstacles]]
```

Notes:
- LOS check is **skipped** for melee abilities (`range.max == 1`).
- LOS check is **skipped** when `requires_los = false` (the default).
- Obstacles are declared per-encounter via `[[encounters.obstacles]]` (see below).

### `power` — масштаб «оружейной» части (default `1.0`)

Один множитель на способность; задаётся полем `power`. Масштабирует только
переменную, «оружейную» часть урона/лечения, а модификатор стата (`STR`/`INT`)
добавляется всегда полностью:

- `weapon_attack`: `round(roll × power) + mod`.
- `spell_damage` / `heal` / DoT-бейк: `roll + INT_mod + round(power × spell_power)`.

`power < 1` — ослабленная версия (напр. `paralyzing_shot`, `burn` = 0.5: добавляют
статус-эффект ценой урона), `power > 1` — усиленная. Пропуск поля = `1.0`
(нейтрально). См. [mechanics.md](mechanics.md#damage).

### Effect Types

| Effect | Dice | Stat | Mitigation | Notes |
|---|:---:|:---:|:---:|---|
| `weapon_attack` | weapon | +STR/+DEX | armor | `ranged=true` → ranged dice + DEX. `power` scales the roll. |
| `damage` | Yes | +STR | armor | Fixed dice (no weapon); `power` ignored (always 1.0). |
| `spell_damage` | Yes | +INT +`power`×spell_power | **magic_resist** | Magic — armor does NOT apply. |
| `heal` | Yes | +INT +`power`×spell_power | N/A | Capped at max_hp |
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

### Target tags (`requires_tags` / `excludes_tags`)

Restrict which units an ability may target by their **creature tags** (see [Creature tags](#creature-tags) below). A target is legal only when it carries **all** of `requires_tags` and **none** of `excludes_tags`.

```toml
[[abilities]]
id            = "shepherd_heal"
name          = "Латание симбионта"
target_type   = "single_ally"
effect        = "heal"
dice_count    = 1
dice_sides    = 5
costs         = [{ resource = "mana", amount = 1 }]
cost_ap       = 1
range         = 3
requires_tags = ["symbiote"]      # may only heal units tagged `symbiote`
# excludes_tags = ["incorporeal"]  # …and never units tagged `incorporeal`
```

- **Enforced in legality** for `single_enemy` and `single_ally` targets only (`check_legality` in `crates/combat_engine/src/legality.rs`); `myself` / `ground` ignore the predicate. A failing target yields `IllegalReason::WrongTargetTags` and the cast is refused — for both player and AI (the AI inherits the gate for free through the shared legality path).
- **Both default to empty** → no restriction. Omit when you don't need tag-gating.
- **Content-only.** `requires_tags` / `excludes_tags` are *not* serialized into the engine wire/trace — they live on the content `AbilityDef` and are consulted at legality time.
- **When to use:** a healer that only patches one creature type (ch3 `shepherd_heal` → `symbiote`), a weapon that bites only the corporeal, an exorcism that targets only the possessed.

## Statuses (`statuses.toml`)

```toml
[[statuses]]
id                    = "burning"
name                  = "Ожог"
armor_bonus           = 0        # +armor vs physical (negative reduces)
magic_resist_bonus    = 0        # +magic_resist vs spell_damage/DoT (negative reduces)
skips_turn            = false    # unit can't act
forces_targeting      = false    # enemies must attack this unit
dot_count             = 0        # DoT dice count (with dot_sides)
dot_sides             = 0
blocks_mana_abilities = false    # can't cast mana abilities
speed_bonus           = 0        # modifies movement (clamped to 0+; speed=0 means immobile)
hp_percent_dot        = 0        # % of max_hp as DoT per tick
ai_controlled         = false    # hero acts under AI control (pact_control)
causes_disadvantage   = false    # ALL carrier's rolls are at disadvantage (disoriented)
heal_per_tick         = 0        # HoT: heals N HP per tick over the holder's turns
```

### Heal-over-time (`heal_per_tick`)

The healing mirror of damage DoT (`dot_count`/`dot_sides`/`hp_percent_dot`). A status with `heal_per_tick = N` restores **N HP per tick** over the holder's own turns, clamped to `max_hp`. The amount is **fixed** (not INT/spell-power-scaled) — like a damage DoT. Healing can never kill, phase-trigger, or grant rage.

```toml
[[statuses]]
id            = "vital_infusion"
name          = "Вливание жизни"
heal_per_tick = 4                # +4 HP per turn → +8 over a 2-turn duration
```

Apply it like any other status — via an ability's `statuses = [{ id = "vital_infusion", on = "target", duration = 2 }]`. Real example: Орен's «Вливание жизни» (ch3) — a `single_ally`, 2-mana cast that lays a 2-turn `vital_infusion` for +8 total, the efficiency counterpart to a burst `heal`.

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
id     = "plate_armor"
name   = "Латная кираса"
armor  = 2
weight = "heavy"      # light (default) | medium | heavy
# optional stat bonuses
# mana = 1            # flat bonus added to the wearer's mana pool at spawn (casters only)
```

Each armor piece declares a `weight` class: `"light"` (cloth/padded, default), `"medium"` (leather/mail), or `"heavy"` (plate/iron).

The optional `mana` field grants a flat bonus to the wearer's mana pool at spawn. It only augments an **existing** pool — gear never creates a mana pool for a non-caster (a class with `mana_max = 0` receives no `Mana` component regardless of armor). Only the three armor slots (chest/legs/feet) contribute to this bonus; weapons are intentionally excluded. The field is optional — omitting it defaults to `"light"`. Weight is used as a **camp-screen-only passive gate**: a hero may only equip medium or heavy armor if their class lists that weight in `armor_proficiencies` (see below). Light armor is always allowed regardless of class. This gate is enforced only in the camp UI — combat systems never consult it.

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
ability_ids          = ["melee_attack", "ranged_shot"]
mana_max             = 0
rage_max             = 0
energy_max           = 6
armor_proficiencies  = ["medium"]   # optional; lists medium/heavy weights the class is trained in
```

`armor_proficiencies` is an optional list of armor weight classes the hero is trained to wear. Light armor is always free and should **not** be listed. An empty list (or omitting the field) means the class can only wear light armor. This is a passive gate enforced only on the camp screen — it is not checked during combat.

### Battle figurines (`sprite`)

`sprite` is an optional asset path **relative to `assets/images/`** that renders a
figurine as a child of the unit's battlefield token circle. Absent → the unit keeps
the plain colored-circle token (no figure).

Resolution per spawn path (first non-`None` wins):

| Unit kind        | Precedence                                                            |
|------------------|-----------------------------------------------------------------------|
| Class hero       | party-member `sprite` override → class `sprite` (pattern)             |
| Template member  | party-member `sprite` override → template `sprite` (literal)          |
| Encounter enemy  | enemy `sprite` (literal) → template `sprite` (literal)                |
| Summon           | template `sprite` (literal)                                           |

Any `sprite` (class pattern, literal override, template, enemy) may contain two
placeholders:

- **`{race}`** ← the unit's race id (e.g. `human`), substituted **at spawn**.
- **`{facing}`** ← screen orientation (`right` / `left`), substituted **dynamically
  at render time**. Facing is a runtime state: a unit starts facing the nearest
  opposing-party member and (planned) turns toward its last interaction. So both
  files must exist; the engine swaps between them as the unit turns.

A path without a placeholder is used verbatim. The class `sprite` is the
race-parametrised default (`units/warrior_{race}_{facing}.png`); the other three
are overrides — they may use `{facing}` (and `{race}`) too.

```toml
# classes.toml — race + facing parametrised default
sprite = "units/warrior_{race}_{facing}.png"

# party member / enemy / unit_template — override (facing still applies)
sprite = "units/oren_{facing}.png"
```

**Asset spec.** PNG, RGBA with transparency, 256×256, the figure's feet at the
bottom-center of the canvas. **Two files per figure** — one facing right, one
facing left — each **drawn separately, not mirrored**: the scene light is fixed
top-left in screen space, so a horizontal flip would light the wrong side. A unit
turns at runtime, so both orientations are needed for nearly every figure.
Symmetric art (no clear left/right) may omit `{facing}` and ship a single file.
Naming convention: `images/units/<class>_<race>_<facing>.png` for class
patterns; `images/units/<id>_<facing>.png` for per-unit overrides
(`<facing>` ∈ `right`, `left`).

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
sprite      = "units/stormborn_echo_{facing}.png"   # optional figurine override; see Classes → Battle figurines
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

When `template` is set, scalar fields (`name`, `race`, `speed`, `ability_ids`, `faction`, `path`, `sprite`) can be overridden individually; blocks (`stats`, `equipment`, `resources`) are **all-or-nothing** — include the whole block to override, omit to inherit. `hex_col` / `hex_row` are always required. A `sprite` override is an asset path (the `{facing}` placeholder still applies; see [Battle figurines](#battle-figurines-sprite)); absent → inherits the template's `sprite`.

```toml
[[encounters.enemies]]
template = "stormborn_echo"      # scenario chars first, then campaign templates
name     = "Старшина"            # override leaf
hex_col  = 5
hex_row  = 3
```

### Creature tags

`tags = ["..."]` on a `[[encounters.enemies]]` entry attaches **creature tags** to that roster unit (e.g. species/body/life axes — `symbiote`, `corporeal`, `living`, `incorporeal`, `aberration`, …). Tags are a flat, additive set; the axes are documentation grouping, not enforced. They're consumed by:

- ability target predicates — `requires_tags` / `excludes_tags` (see [Target tags](#target-tags-requires_tags--excludes_tags) above);
- aura `affects_tags` (see above);
- phase tag-swaps (see below).

Tags live on the **encounter enemy**, not the template — the same template can be tagged differently per encounter. Default is empty (most enemies need no tags).

```toml
[[encounters.enemies]]
template = "combat_symbiote"
tags     = ["symbiote", "corporeal", "living"]   # makes shepherd_heal legal on it
hex_col  = 5
hex_row  = 3
```

Real example: ch3 `combat_symbiote` carries `["symbiote","corporeal","living"]` so the shepherd's `requires_tags = ["symbiote"]` heal can target it.

### Phases

Boss transforms when a trigger fires. At most one phase per frame; pending phases fire in declaration order. **In-place mutation**: same entity, same turn position, `VictoryTarget` preserved — so `kill_target` means "kill through all phases".

**Lethal hits vs. `heal_to_full`.** A phase preempts death **only** when `heal_to_full = true` — the refill reverses an otherwise-lethal hit, so the boss survives into the new phase. With a **non-healing** phase (`heal_to_full` absent/false, e.g. the flee+deadline combo below), a lethal blow does both: the boss **enters the phase and then dies in the same step**. So the phase's `victory_override` / `turn_limit` are applied first, then the death is evaluated against the new win-condition — one-shotting the boss past the threshold wins immediately via the override's `kill_target`. (Emitting only the phase would strand the boss at 0 HP and the fight would stall; this is the Kolm one-shot fix.)

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

#### Phase tags

A phase may also rewrite the unit's [creature tags](#creature-tags) on entry. `tags = ["..."]` on a `[[encounters.enemies.phases]]` **replaces** the unit's entire tag set when the phase fires; **omitting** `tags` keeps the current tags unchanged (it does not clear them).

```toml
[[encounters.enemies.phases]]
hp_below_pct = 33
template     = "container_phase3"
tags         = ["aberration", "incorporeal"]   # REPLACES the tag set; drops `symbiote`
flavor       = "Оболочка пала..."
```

Real example: the ch3 Container's phase-3 swap replaces `{symbiote, corporeal}` with `{aberration, incorporeal}`. Dropping `symbiote` means the accumulator aura (`affects_tags = ["symbiote"]`) no longer matches the boss, so its feed cuts off automatically — see [Aura `affects_tags`](#aura-affects_tags). (Phase-2, which omits `tags`, keeps `{symbiote, corporeal}` so the aura still feeds it.)

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

#### Aura `affects_tags`

Optionally narrow the aura to targets carrying **all** of `affects_tags` (in addition to the `affects` team filter). An **empty / omitted** list ⇒ no tag filter (every team-matching unit in range is affected).

```toml
# ch3 accumulator: a buff aura that only feeds units tagged `symbiote`.
aura = { status = "accumulator_field", radius = 2, affects = "allies", affects_tags = ["symbiote"] }
```

Real example: the ch3 `accumulator` ("живая батарея") sits behind the boss and projects an `armor_bonus` aura with `affects_tags = ["symbiote"]` — so it only feeds the Container while the Container still carries the `symbiote` tag. When the Container's phase-3 swap drops `symbiote` (see [Phase tags](#phase-tags) below), the aura stops seeing it and the buff falls off — a lore-native way to cut the feed "no matter what". Killing the accumulator removes the source and the aura lapses the same way.

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

- **`story`** — dialogue. `lines` is a list of `{speaker, text, requires_flag?, excludes_flag?}`. Player advances line-by-line (Space / Enter / button); previous lines stay on screen. Line visibility is gated by flags (see [Per-line flag gating](#per-line-flag-gating-requires_flag--excludes_flag) below).
  - `party_add` / `party_remove` apply when the player advances past the last line.
  - **If `lines = []`** (or omitted), the scene is **invisible** — `advance_scenario` skips past it. Use this idiom for a pure party-change beat without dialogue.

- **`combat`** — fight. `encounter` refers to this scenario's `encounters.toml`. On the outcome, campaign flags are recorded (see **Combat-outcome flags** below); `requires_flag` on future dialogue lines checks against the persisted flag set.

- **`choice`** — a `story`-style prompt (`lines`) plus an `options` list. Each option is `{ label, set_flag }`; picking one writes `set_flag` into the persistent campaign flag set and advances. This is the **branching primitive**: downstream `requires_flag` / `excludes_flag` gates (on scenes or lines) read those flags. Real example: ch3 `theo_fate` (`theo_surrendered` / `theo_fight`) and the 3-way `kasian_choice` (`kasian_accept` / `kasian_refuse` / `kasian_strike`).

```toml
[[scenes]]
type = "choice"
lines = [ { speaker = "Тэо", text = "Опустите оружие." } ]
options = [
  { label = "Опустить оружие — выслушать Тэо", set_flag = "theo_surrendered" },
  { label = "Прорываться к Тэо с боем",        set_flag = "theo_fight" },
]
```

### Per-line flag gating (`requires_flag` / `excludes_flag`)

A `[[scenes]]` `story`/`choice` `lines` entry can carry both flag gates. A line is **shown** iff:

- `requires_flag` is unset **or** its flag is present, **and**
- `excludes_flag` is unset **or** its flag is absent.

`requires_flag` = "show only after this happened"; `excludes_flag` = its `else`-branch companion ("show only when this did *not* happen") — useful for narrating an outcome without a dedicated positive flag.

```toml
lines = [
  # boat survived → praise; boat lost → the "else" line (no positive flag needed)
  { speaker = "Венн", text = "...полным составом да с сухой кормой.", requires_flag = "boat_saved" },
  { speaker = "Венн", text = "Корыто в щепки расшибли. Но уцелели.",  excludes_flag = "boat_saved" },
  # Marken possessed = Theo killed but Marken NOT killed
  { speaker = "Kael", text = "Оставил его там... с этой тварью под кожей.",
    requires_flag = "theo_killed", excludes_flag = "marken_killed" },
]
```

### Scene-level branching (`requires_flag`)

`requires_flag = "flag"` on a **whole scene** (`type = "story" | "combat" | "choice"`) skips that scene — exactly like an empty/invisible beat — when the flag is **absent** from the campaign flag set. This is how a branch gates entire beats: skip a combat bout, or gate one of several mutually-exclusive epilogue scenes.

```toml
# ch3: this combat bout only plays in the "fight" branch.
[[scenes]]
type          = "combat"
encounter     = "ch3_theo"
requires_flag = "theo_fight"

# ch3: one of three mutually-exclusive epilogues, gated by the kasian_choice flag.
[[scenes]]
type          = "story"
requires_flag = "kasian_accept"
lines = [ { speaker = "Lyra", text = "...Мы примем его условия. Пока." } ]
```

> **Contract — skipping a combat scene discards its flags.** A skipped `Combat`
> scene never writes its `on_victory_flags` or encounter `objectives` flags. So a
> branch that *replaces* a fight (e.g. "negotiate instead of fighting Тэо") must
> set any downstream-needed flag itself — via the `Choice` option's `set_flag` or
> a `Story` scene — because the skipped bout won't. See
> [docs/architecture.md](architecture.md) (Scenario Scene Flow) for the flag-flow
> internals.

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

Either shape accepts an optional `sprite` field — a figurine path that **overrides** the class pattern or template default for this one member (the `{facing}` placeholder still applies; see [Battle figurines](#battle-figurines-sprite)).

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

### Persistent statuses (`status_ops`)

A story scene can apply or remove a **persistent status** on a named party member —
e.g. a wound set narratively when it happens and carried into later fights. These fold
across scenes exactly like party membership (derived from `scene_index`, so they add **no
save state**) and are re-applied at every combat spawn until a later scene removes them.

```toml
[[scenes]]
type = "story"
lines = [ { speaker = "Орен", text = "Я перевязал, но рана останется." } ]

# Single ORDERED list — ops apply in declaration order when folded.
[[scenes.status_ops]]
op = "add"
unit_name = "Aldric"      # must be in the party at this scene
status_id = "injured"     # must exist in statuses.toml

# A later scene can soften the wound, or even turn it into a buff:
# [[scenes.status_ops]]
# op = "remove"  ... unit_name = "Aldric"  status_id = "injured"
# [[scenes.status_ops]]
# op = "add"     ... unit_name = "Aldric"  status_id = "injured_minor"
```

- **Ordered, not two lists.** `status_ops` is one ordered list of `add`/`remove`; it folds
  in declaration order across scenes, so `add X … remove X … add Y` composes exactly as
  written. Adds dedupe (a status appears at most once per unit).
- **Permanent-per-bout.** Each `add` grants a `PERMANENT`-duration status, re-derived and
  re-applied at every combat spawn until a later scene removes it — it never ticks away
  mid-fight. A one-bout debuff is just `add` in the scene before the fight + `remove` in the
  scene after.
- **Status content.** `status_id` is a regular `statuses.toml` entry; a stat condition like
  `injured` is just `armor_bonus`/`speed_bonus` (no engine code). It stacks with combat
  statuses (e.g. `defending`) through the normal aggregate sum, and is visible to the AI.
- **Validated at load.** `unit_name` must be in the party after that scene and `status_id`
  must exist — a typo fails loudly instead of silently doing nothing.

### Derived state (no runtime storage)

- **Active party** at scene N = starting `[[party]]` + all `party_add` / `party_remove` from story scenes 0..N-1, folded in order. Save files only store `scene_index`; the party is re-derived on load.
- **Persistent statuses** on party members at scene N = fold of all `status_ops` over story scenes 0..N-1, in declaration order. Derived (no save state), re-applied at each combat spawn — same as the party itself.
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
