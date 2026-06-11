use bevy::ecs::query::QueryData;
use bevy::prelude::*;
use combat_engine::{AbilityId, ArmorId, StatusId, WeaponId};

#[derive(Component, Default)]
pub struct Combatant;

/// Магический путь персонажа (определяет эффект крит. неудачи).
#[derive(Component, Debug, Clone)]
pub struct CombatPath(pub String);

/// Marker: the combatant whose turn it is right now.
#[derive(Component)]
pub struct ActiveCombatant;

/// Starting hex grid position assigned at spawn.
#[derive(Component, Clone, Copy)]
pub struct StartingHexPos(pub hexx::Hex);

/// Inserted when hp reaches 0. Skips the unit's turn and prevents acting.
#[derive(Component, Default)]
pub struct Dead;

/// Marker: this combatant is the KillTarget for the current encounter.
/// When it dies, combat ends in victory. `marker_color` drives a ring under its token.
#[derive(Component, Clone, Copy)]
pub struct VictoryTarget {
    pub marker_color: [f32; 3],
}

/// Marker: this combatant is a KeepAlive target for the current encounter.
/// If it dies, combat ends in defeat. `marker_color` drives a ring under its token.
/// Set by `spawn_combatants` when any `VictoryCondition::KeepAlive` (at any depth
/// inside `AllOf`) names this unit.
///
/// AI reads `AiTags::OPPONENT_OBJECTIVE` (set from this component in `build_snapshot`)
/// to prioritize killing this unit.
#[derive(Component, Clone, Copy)]
pub struct KeepAliveTarget {
    pub marker_color: [f32; 3],
}

/// Pending phase transformations for an enemy, in declaration order.
/// Each entry is applied at most once when its trigger fires, then removed.
#[derive(Component, Debug, Clone)]
pub struct EnemyPhases {
    pub pending: Vec<crate::content::encounters::PhaseDef>,
}

/// Passive aura: while this unit is alive, every target matching `affects`
/// within `radius` hexes gets `status` (re-)applied at TurnStart with duration=1.
/// Removed automatically when the source dies or the target leaves range.
#[derive(Component, Debug, Clone)]
pub struct AuraSource {
    pub status: StatusId,
    pub radius: u32,
    pub affects: crate::content::encounters::AuraAffects,
    /// Tags the target must carry for this aura to apply.  Empty ⇒ no filter.
    pub affects_tags: std::collections::BTreeSet<combat_engine::TagId>,
}

/// Creature tags for this unit (e.g. `"undead"`, `"beast"`, `"living"`).
///
/// Populated at spawn from content (`EnemyDef.tags`, `UnitTemplate.tags`).
/// The bootstrap carry-in loop copies these into the engine `Unit.tags` field
/// so legality and aura predicates see them.  Absent component ⇒ empty tags.
#[derive(Component, Debug, Clone, Default)]
pub struct Tags(pub std::collections::BTreeSet<combat_engine::TagId>);

/// Marker pointing at the summoner that brought this unit into the encounter.
/// Used only for `max_active` caps — summons outlive their summoner by default.
#[derive(Component, Clone, Copy, Debug)]
pub struct SummonedBy(pub Entity);

#[derive(Component, Default)]
pub struct PartyMember;

#[derive(Component, Default)]
pub struct Enemy;

/// Overrides the AI evaluation regime for this unit each turn.
/// Written by `apply_phase_ecs_writes` when a boss phase carrying
/// `ai_behavior` fires. Intentionally game-layer only — stores the
/// content enum `AiBehaviorKind` rather than the AI-internal `EvaluationMode`
/// so that the game/components layer does not import `combat::ai`.
#[derive(Component, Clone, Copy, Debug)]
pub struct AiBehaviorOverride {
    pub kind: crate::content::encounters::AiBehaviorKind,
}

/// ECS marker carrying the unit-template id for template-based combatants.
/// Set by `spawn_combatants` for party members spawned from a `UnitTemplate`
/// (e.g. non-acting NPCs via `party_add { template = "..." }`).
/// Read by `from_ecs` so the engine `Unit` can carry `template_id`.
#[derive(Component, Debug, Clone)]
pub struct TemplateRef(pub String);

pub use combat_engine::state::Team;

#[derive(Component)]
pub struct Faction(pub Team);

/// The core combat stats (base values, without equipment bonuses).
#[derive(Component, Clone, Debug, Default)]
pub struct CombatStats {
    pub max_hp: i32,
    pub strength: i32,  // melee attack/damage bonus
    pub dexterity: i32, // initiative bonus
    pub constitution: i32,
    pub intelligence: i32, // boosts spell damage and healing
    pub wisdom: i32,
    pub charisma: i32,
}

impl std::ops::AddAssign<&CombatStats> for CombatStats {
    fn add_assign(&mut self, rhs: &CombatStats) {
        self.max_hp += rhs.max_hp;
        self.strength += rhs.strength;
        self.dexterity += rhs.dexterity;
        self.constitution += rhs.constitution;
        self.intelligence += rhs.intelligence;
        self.wisdom += rhs.wisdom;
        self.charisma += rhs.charisma;
    }
}

#[derive(Component)]
pub struct Vital {
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32,        // reduces incoming physical damage
    pub magic_resist: i32, // reduces incoming magic damage
}

impl Vital {
    pub fn new(stats: &CombatStats, armor: i32, magic_resist: i32) -> Self {
        Self {
            hp: stats.max_hp,
            max_hp: stats.max_hp,
            armor,
            magic_resist,
        }
    }

    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }
}

/// How many hex cells the unit can move per turn.
#[derive(Component, Clone, Copy, Debug)]
pub struct Speed(pub i32);

/// Marker inserted by GrantMovement abilities (e.g. Rush) so the UI can
/// auto-enter move mode for the actor. Removed after the first move or at
/// turn end. The actual bonus distance is added directly to
/// `ActionPoints.movement_points` when the ability resolves.
#[derive(Component)]
pub struct BonusMovement;

/// Reactions available per round (attacks of opportunity, etc).
/// Refilled to `max` at the start of each round.
#[derive(Component, Clone, Copy, Debug)]
pub struct Reactions {
    pub remaining: u8,
    pub max: u8,
}

impl Default for Reactions {
    fn default() -> Self {
        // remaining starts at 0 to match engine `Unit { reactions_left: 0, reactions_max: 1 }`
        // at spawn (crates/combat_engine/src/effect.rs Effect::Spawn). The engine refills
        // reactions_left = reactions_max on round wrap via `start_round`, then
        // `project_state_to_ecs` mirrors that back into ECS. A unit cannot AoO in the
        // round it was spawned in.
        Self {
            remaining: 0,
            max: 1,
        }
    }
}

#[derive(Component)]
pub struct Initiative(pub i32);

#[derive(Component)]
pub struct ActionPoints {
    /// Current AP pool. Refilled to `max_ap` at turn start.
    pub action_points: i32,
    /// Base AP pool per turn.
    pub max_ap: i32,
    /// Remaining movement points for the current turn. Refilled at TurnStart.
    pub movement_points: i32,
}

impl ActionPoints {
    /// True while the unit still has movement budget this turn.
    pub fn can_move(&self) -> bool {
        self.movement_points > 0
    }

    /// True if the actor can afford an ability costing `cost_ap` AP.
    pub fn can_act_for(&self, cost_ap: i32) -> bool {
        self.action_points >= cost_ap
    }
}

#[derive(Component, Default)]
pub struct Abilities(pub Vec<AbilityId>);

/// Ярость — накапливается при ударах и получении урона.
/// Присутствует только у персонажей с этой механикой (воин).
#[derive(Component, Debug, Clone)]
pub struct Rage {
    pub current: i32,
    pub max: i32,
}

/// Мана — расходуется на заклинания, восстанавливается на 1 в конце каждого хода.
/// Присутствует только у персонажей с этой механикой (маг).
#[derive(Component, Debug, Clone)]
pub struct Mana {
    pub current: i32,
    pub max: i32,
}

impl Mana {
    pub fn new(max: i32) -> Self {
        Self { current: max, max }
    }

    /// Восстановить amount маны (не выше max). Возвращает новое значение.
    pub fn restore(&mut self, amount: i32) -> i32 {
        self.current = (self.current + amount).min(self.max);
        self.current
    }

    /// Потратить ману. Возвращает false если недостаточно.
    pub fn spend(&mut self, amount: i32) -> bool {
        if self.current < amount {
            return false;
        }
        self.current -= amount;
        true
    }
}

impl Rage {
    pub fn new(max: i32) -> Self {
        Self { current: 0, max }
    }

    /// Прибавить 1 ярость (не выше max). Возвращает новое значение.
    pub fn gain(&mut self) -> i32 {
        self.current = (self.current + 1).min(self.max);
        self.current
    }

    /// Потратить ярость. Возвращает false если недостаточно.
    pub fn spend(&mut self, amount: i32) -> bool {
        if self.current < amount {
            return false;
        }
        self.current -= amount;
        true
    }
}

/// Энергия — расходуется на немагические способности, восстанавливается на 1 в начало хода.
/// Присутствует только у персонажей с этой механикой (следопыт).
#[derive(Component, Debug, Clone)]
pub struct Energy {
    pub current: i32,
    pub max: i32,
}

impl Energy {
    pub fn new(max: i32) -> Self {
        Self { current: max, max }
    }

    pub fn restore(&mut self, amount: i32) -> i32 {
        self.current = (self.current + amount).min(self.max);
        self.current
    }

    pub fn spend(&mut self, amount: i32) -> bool {
        if self.current < amount {
            return false;
        }
        self.current -= amount;
        true
    }
}

/// Visual token circle entity linked to a combatant.
#[derive(Component)]
pub struct UnitToken(pub Entity);

/// All equipment slots for this combatant.
#[derive(Component, Clone, Debug)]
pub struct Equipment {
    pub main_hand: Option<WeaponId>,
    pub off_hand: Option<WeaponId>,
    pub chest: ArmorId,
    pub legs: ArmorId,
    pub feet: ArmorId,
}

#[derive(Component, Default)]
pub struct StatusEffects(pub Vec<ActiveStatus>);

#[derive(Debug, Clone)]
pub struct ActiveStatus {
    pub id: StatusId,
    pub rounds_remaining: u32,
    /// Entity whose EndTurn ticks this counter down.
    /// `None` when the status was applied by an environment object (trap/hazard)
    /// with no unit applier.
    pub applier: Option<Entity>,
    /// Урон за тик (яд). 0 = нет DoT. Уменьшается исцелением.
    pub dot_per_tick: i32,
}

// ── Query data structs ──────────────────────────────────────────────────────

/// Hex grid display: labels, cell colors, tooltip.
#[derive(QueryData)]
pub struct HexCombatantQ {
    pub entity: Entity,
    pub name: &'static Name,
    pub vital: &'static Vital,
    pub faction: &'static Faction,
    pub mana: Option<&'static Mana>,
    pub rage: Option<&'static Rage>,
    pub energy: Option<&'static Energy>,
    pub is_dead: Has<Dead>,
}

/// Enemy AI: full combatant data for scoring, pathfinding, ability selection.
/// Optional combat-kit fields (`abilities`, `speed`, `ap`, `stats`, `equipment`)
/// default in `build_snapshot` so a minimal combatant (Faction+Vital only) is
/// still visible to the AI — parity with engine `from_ecs`. Non-acting NPCs are
/// a valid config; defaulting here is intentional, not a bug, so NO warn.
#[derive(QueryData)]
pub struct AiCombatantQ {
    pub entity: Entity,
    pub faction: &'static Faction,
    pub abilities: Option<&'static Abilities>,
    pub vital: &'static Vital,
    pub speed: Option<&'static Speed>,
    pub ap: Option<&'static ActionPoints>,
    pub stats: Option<&'static CombatStats>,
    pub equipment: Option<&'static Equipment>,
    pub mana: Option<&'static Mana>,
    pub rage: Option<&'static Rage>,
    pub energy: Option<&'static Energy>,
    pub combat_path: Option<&'static CombatPath>,
    pub summoned_by: Option<&'static SummonedBy>,
    pub reactions: Option<&'static Reactions>,
    pub ai_behavior_override: Option<&'static AiBehaviorOverride>,
}

/// Player command input: ability selection, target cycling.
#[derive(QueryData)]
pub struct PlayerCombatantQ {
    pub entity: Entity,
    pub vital: &'static Vital,
    pub faction: &'static Faction,
    pub abilities: &'static Abilities,
    pub ap: &'static ActionPoints,
    pub has_bonus_move: Has<BonusMovement>,
}

/// Validation: actor resource/status data for ability cost checks.
#[derive(QueryData)]
pub struct ValidationActorQ {
    pub vital: &'static Vital,
    pub faction: &'static Faction,
    pub ap: &'static ActionPoints,
    pub abilities: &'static Abilities,
    pub rage: Option<&'static Rage>,
    pub mana: Option<&'static Mana>,
    pub energy: Option<&'static Energy>,
    pub statuses: Option<&'static StatusEffects>,
}

/// Validation: combatant-level data for target inspection **and** taunter
/// scan. Named-type query data so borrowing `&Query<..>` stays variance-
/// friendly (unlike the bare `&Vital` form, which makes the `D` parameter
/// invariant over its internal lifetime and breaks thin adapter structs
/// that hold `&Query`).
///
/// Includes `faction` + `statuses` so the adapter can answer "is this an
/// opposing-team taunter?" without an extra query — both team-safety and
/// taunt-enforcement resolve against the same fetch.
#[derive(QueryData)]
pub struct ValidationTargetQ {
    pub entity: Entity,
    pub vital: &'static Vital,
    pub faction: &'static Faction,
    pub statuses: Option<&'static StatusEffects>,
    pub tags: Option<&'static Tags>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vital(hp: i32, max_hp: i32) -> Vital {
        Vital {
            hp,
            max_hp,
            armor: 0,
            magic_resist: 0,
        }
    }

    #[test]
    fn is_alive_false_at_zero_hp() {
        assert!(!vital(0, 10).is_alive());
    }

    #[test]
    fn is_alive_true_above_zero() {
        assert!(vital(1, 10).is_alive());
    }
}
