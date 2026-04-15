# Content Guide

All game content is data-driven via TOML files in `assets/data/`. No code changes needed to add content.

## Abilities (`abilities.toml`)

```toml
[[abilities]]
id            = "fireball"
name          = "Огненный шар"
magic_domains = ["aether", "form"]  # optional; see magic-schools.md
magic_method  = "destruction"       # optional; see magic-schools.md
target_type   = "single_enemy"      # single_enemy | single_ally | myself
effect        = "spell_damage"      # weapon_attack | damage | spell_damage | heal | none | grant_movement
dice_count    = 2
dice_sides    = 3
costs         = [{ resource = "mana", amount = 5 }]  # resource: hp | mana | rage | energy
range         = 5                   # hex steps; 0 = no range check (for myself)
distance      = 0                   # only for grant_movement
statuses      = [                   # optional
    { id = "burning", on = "target", duration = 2 },
]
```

### Effect Types

| Effect | Dice Required | Stat Bonus | Armor | Notes |
|--------|:---:|:---:|:---:|-------|
| `weapon_attack` | No (uses weapon) | +STR | Reduced | |
| `damage` | Yes | +STR | Reduced | |
| `spell_damage` | Yes | +INT +spell_power | **Pierced** | |
| `heal` | Yes | +INT +spell_power | N/A | Capped at max_hp |
| `none` | No | N/A | N/A | Status-only |
| `grant_movement` | No | N/A | N/A | Requires `distance`, does NOT end turn |

### Target Types
- `single_enemy` — one living enemy
- `single_ally` — one living ally (including self)
- `myself` — always targets self (auto-targeted in UI)

## Statuses (`statuses.toml`)

```toml
[[statuses]]
id                    = "burning"
name                  = "Ожог"
armor_bonus           = 0        # default 0; adds to armor (negative = reduce)
damage_taken_bonus    = 1        # default 0; extra damage on all hits
skips_turn            = false    # default false; unit can't act
forces_targeting      = false    # default false; enemies must attack this unit
dot_count             = 1        # optional; DoT dice count (requires dot_sides)
dot_sides             = 4        # optional; DoT dice sides
blocks_mana_abilities = false    # default false; can't use mana abilities (crit fail: broken_faith)
speed_bonus           = 0        # default 0; modifies movement speed
hp_percent_dot        = 0        # default 0; % of max_hp as DoT per tick (crit fail: exhaustion)
ai_controlled         = false    # default false; hero acts under AI control (crit fail: pact_control)
```

## Weapons (`equipment/weapons.toml`)

```toml
[[weapons]]
id          = "staff"
name        = "Посох"
hand        = "two_handed"       # main_hand | off_hand | two_handed
dice_count  = 1
dice_sides  = 6
spell_power = 1                  # default 0; added to spell_damage and heal
# optional stat bonuses (default 0): armor, max_hp, strength, dexterity, constitution, intelligence, wisdom, charisma
```

## Armor (`equipment/chest.toml`, `legs.toml`, `feet.toml`)

```toml
[[items]]
id    = "plate_armor"
name  = "Латная кираса"
armor = 2                        # physical damage reduction
# optional stat bonuses (default 0): max_hp, strength, dexterity, constitution, intelligence, wisdom, charisma
```

## Classes (`classes.toml`)

```toml
[[classes]]
id           = "ranger"
name         = "Следопыт"
max_hp       = 14
strength     = 2
dexterity    = 6
constitution = 2
intelligence = 0
wisdom       = 4
charisma     = 0
speed        = 5
main_hand    = "dagger"          # references equipment/weapons.toml
off_hand     = null              # optional; second weapon
chest        = "leather_vest"    # references equipment/chest.toml
legs         = "leather_pants"   # references equipment/legs.toml
feet         = "leather_boots"   # references equipment/feet.toml
ability_ids  = ["melee_attack", "bow_shot", "paralyzing_shot", "field_medic"]
mana_max     = 0                 # default 0 (no mana)
rage_max     = 0                 # default 0 (no rage)
energy_max   = 6                 # default 0 (no energy)
```

## Encounters (`encounters.toml`)

```toml
[[encounters]]
id   = "orc_camp"
name = "Лагерь орков"

[[encounters.enemies]]
name         = "Orc Mage"
race         = "orc"             # references races.toml
faction      = null              # optional; references races.toml factions
path         = null              # optional; references races.toml paths (determines crit fail)
max_hp       = 14
strength     = 0
dexterity    = -2
constitution = 2
intelligence = 4
wisdom       = 2
charisma     = -2
speed        = 4
main_hand    = "staff"           # references equipment/weapons.toml
off_hand     = null              # optional
chest        = "mage_robe"       # references equipment/chest.toml
legs         = "cloth_pants"     # references equipment/legs.toml
feet         = "cloth_shoes"     # references equipment/feet.toml
ability_ids  = ["melee_attack", "fireball", "heal"]
mana_max     = 8                 # default 0
rage_max     = 0                 # default 0
energy_max   = 0                 # default 0
hex_col      = 6
hex_row      = 4
```

## Scenarios (`scenarios.toml`)

```toml
[[scenarios]]
id   = "demo"
name = "Засада гоблинов"

# Party (same for all combats in this scenario)
[[scenarios.party]]
name    = "Aldric"
race    = "human"                # references races.toml
faction = "aurum"                # optional; references races.toml factions
path    = null                   # optional; references races.toml paths (determines crit fail)
class   = "warrior"              # references classes.toml id
hex_col = 1
hex_row = 2

# Scenes (play in order)
[[scenarios.scenes]]
type = "story"
text = "Отряд пробирается через лес..."

[[scenarios.scenes]]
type      = "combat"
encounter = "goblin_patrol"      # references encounters.toml id

[[scenarios.scenes]]
type = "story"
text = "Конец."
```

Scene types: `story` (requires `text`) or `combat` (requires `encounter`).
