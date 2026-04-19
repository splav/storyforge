//! Pure simulation of plan steps against a cloned battle snapshot.
//!
//! Mirrors `combat/resolution.rs` effects in expected-value HP: dice rolls
//! collapse to their mean, armor/vulnerability apply as in `scoring::score_action`,
//! targets whose HP drops to 0 are removed so subsequent steps see them gone.
//! Does **not** run the real Bevy message pipeline — this is a deterministic
//! offline predictor used by the planner for scoring candidate sequences.

use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::content::abilities::{
    AbilityDef, AoEShape, CasterContext, EffectDef, StatusOn, TargetType,
};
use crate::content::content_view::ContentView;
use crate::core::{ResourceKind, StatusId};
use crate::game::components::Team;
use crate::game::hex::{hex_circle, hex_line, Hex};
use bevy::prelude::Entity;

use super::types::{PlanStep, StepOutcome};

/// Mutable working copy of a snapshot. Steps mutate `snapshot` in place;
/// derived fields like `threat`, `max_attack_range` and some tags are *not*
/// recomputed — treat them as stale on the simulated state.
pub struct SimState {
    pub snapshot: BattleSnapshot,
    pub actor: Entity,
}

impl SimState {
    pub fn from_snapshot(snap: &BattleSnapshot, actor: Entity) -> Self {
        Self {
            snapshot: snap.clone(),
            actor,
        }
    }

    pub fn actor_unit(&self) -> Option<&UnitSnapshot> {
        self.snapshot.unit(self.actor)
    }

    fn actor_unit_mut(&mut self) -> Option<&mut UnitSnapshot> {
        let actor = self.actor;
        self.snapshot.units.iter_mut().find(|u| u.entity == actor)
    }

    fn unit_mut(&mut self, entity: Entity) -> Option<&mut UnitSnapshot> {
        self.snapshot.units.iter_mut().find(|u| u.entity == entity)
    }

    /// Apply one plan step to the simulated state, returning per-step effects.
    pub fn apply_step(
        &mut self,
        step: &PlanStep,
        caster_ctx: &CasterContext,
        content: &ContentView,
    ) -> StepOutcome {
        match step {
            PlanStep::Move { path } => self.apply_move(path),
            PlanStep::Cast {
                ability,
                target,
                target_pos,
            } => {
                let Some(def) = content.abilities.get(ability) else {
                    return StepOutcome::default();
                };
                self.apply_cast(def, *target, *target_pos, caster_ctx, content)
            }
        }
    }

    fn apply_move(&mut self, path: &[Hex]) -> StepOutcome {
        if let Some(&dest) = path.last() {
            let cost = path.len() as i32;
            if let Some(u) = self.actor_unit_mut() {
                u.pos = dest;
                u.movement_points = (u.movement_points - cost).max(0);
            }
        }
        StepOutcome {
            moved: true,
            ..Default::default()
        }
    }

    fn apply_cast(
        &mut self,
        def: &AbilityDef,
        target: Entity,
        target_pos: Hex,
        caster_ctx: &CasterContext,
        content: &ContentView,
    ) -> StepOutcome {
        let mut outcome = StepOutcome::default();

        // Snapshot read-only bits needed before mutating self.
        let Some(actor_unit) = self.actor_unit() else {
            return outcome;
        };
        let actor_pos = actor_unit.pos;
        let actor_team = actor_unit.team;

        // Pay AP + resource costs on the actor.
        pay_costs(self.actor_unit_mut(), def);

        // GrantMovement adds to the actor's MP pool and returns — no targets.
        if let EffectDef::GrantMovement { distance } = &def.effect {
            if let Some(a) = self.actor_unit_mut() {
                a.movement_points += *distance;
            }
            return outcome;
        }

        // RestoreResources tops up the actor's HP and all present resources
        // by 1 each. No targets outside the actor.
        if matches!(def.effect, EffectDef::RestoreResources) {
            if let Some(a) = self.actor_unit_mut() {
                a.hp = (a.hp + 1).min(a.max_hp);
                if let Some((cur, max)) = a.mana {
                    a.mana = Some(((cur + 1).min(max), max));
                }
                if let Some((cur, max)) = a.rage {
                    a.rage = Some(((cur + 1).min(max), max));
                }
                if let Some((cur, max)) = a.energy {
                    a.energy = Some(((cur + 1).min(max), max));
                }
            }
            return outcome;
        }

        // Summon and ToggleMoveMode: cost paid, effect out of sim scope.
        if matches!(
            def.effect,
            EffectDef::Summon { .. } | EffectDef::ToggleMoveMode
        ) {
            return outcome;
        }

        let primary = match def.target_type {
            TargetType::Myself => self.actor,
            _ => target,
        };

        let affected: Vec<Entity> = match def.aoe {
            AoEShape::None => vec![primary],
            AoEShape::Circle { radius } => {
                let cells = hex_circle(target_pos, radius);
                collect_aoe(&self.snapshot, &cells, self.actor, actor_team, def.friendly_fire)
            }
            AoEShape::Line { length } => {
                let cells = hex_line(actor_pos, target_pos, length);
                collect_aoe(&self.snapshot, &cells, self.actor, actor_team, def.friendly_fire)
            }
        };

        outcome.hits = affected.len() as u32;

        // Apply direct damage / heal per affected target. Dead units are
        // skipped defensively (shouldn't happen since pruning runs at step end).
        let calc = def.effect.calc(caster_ctx);
        for ent in &affected {
            let ent = *ent;
            if self.snapshot.unit(ent).is_none_or(|u| u.hp <= 0) {
                continue;
            }
            let Some(ref c) = calc else { continue };
            if c.is_heal {
                if let Some(u) = self.unit_mut(ent) {
                    let missing = (u.max_hp - u.hp).max(0) as f32;
                    let effective = c.expected().min(missing).max(0.0);
                    u.hp = (u.hp as f32 + effective).min(u.max_hp as f32) as i32;
                    outcome.heal += effective;
                }
            } else if let Some(u) = self.unit_mut(ent) {
                let mitigation = if c.pierces_armor {
                    0.0
                } else {
                    (u.armor + u.armor_bonus) as f32
                };
                let raw = (c.expected() - mitigation + u.damage_taken_bonus as f32).max(0.0);
                u.hp = (u.hp as f32 - raw).max(0.0) as i32;
                outcome.damage += raw;
                if u.hp == 0 {
                    outcome.killed.push(ent);
                }
            }
        }

        // Apply status effects — each (entity, status_id) pair at most once,
        // matching the game's retain-then-push semantics in `advance_turn`.
        apply_statuses(self, def, &affected, content, &mut outcome);

        // Prune killed units so the next step sees them absent.
        if !outcome.killed.is_empty() {
            let dead: std::collections::HashSet<Entity> =
                outcome.killed.iter().copied().collect();
            self.snapshot.units.retain(|u| !dead.contains(&u.entity));
        }

        outcome
    }
}

fn pay_costs(actor: Option<&mut UnitSnapshot>, def: &AbilityDef) {
    let Some(a) = actor else { return };
    a.action_points = (a.action_points - def.cost_ap).max(0);
    for cost in &def.costs {
        match cost.resource {
            ResourceKind::Hp => {
                a.hp = (a.hp - cost.amount).max(0);
            }
            ResourceKind::Mana => {
                if let Some((cur, max)) = a.mana {
                    a.mana = Some(((cur - cost.amount).max(0), max));
                }
            }
            ResourceKind::Rage => {
                if let Some((cur, max)) = a.rage {
                    a.rage = Some(((cur - cost.amount).max(0), max));
                }
            }
            ResourceKind::Energy => {
                if let Some((cur, max)) = a.energy {
                    a.energy = Some(((cur - cost.amount).max(0), max));
                }
            }
        }
    }
}

fn collect_aoe(
    snap: &BattleSnapshot,
    cells: &[Hex],
    actor: Entity,
    actor_team: Team,
    friendly_fire: bool,
) -> Vec<Entity> {
    let mut out = Vec::new();
    for &cell in cells {
        let Some(u) = snap.unit_at(cell) else { continue };
        if u.entity == actor {
            if friendly_fire {
                out.push(u.entity);
            }
            continue;
        }
        if !friendly_fire && u.team == actor_team {
            continue;
        }
        out.push(u.entity);
    }
    out
}

fn apply_statuses(
    sim: &mut SimState,
    def: &AbilityDef,
    affected: &[Entity],
    content: &ContentView,
    outcome: &mut StepOutcome,
) {
    // Unique (target, status_id) pairs — duplicate applications collapse to one
    // (the game's retain-then-push replace-in-place behaviour).
    let mut applications: std::collections::HashSet<(Entity, StatusId)> =
        std::collections::HashSet::new();
    for sa in &def.statuses {
        match sa.on {
            StatusOn::MySelf => {
                applications.insert((sim.actor, sa.status.clone()));
            }
            StatusOn::Target => {
                for &ent in affected {
                    applications.insert((ent, sa.status.clone()));
                }
            }
        }
    }

    for (ent, status_id) in &applications {
        let skips_turn = content
            .statuses
            .get(status_id)
            .is_some_and(|sd| sd.skips_turn);
        if skips_turn {
            outcome.stunned.push(*ent);
            if let Some(u) = sim.unit_mut(*ent) {
                u.tags |= AiTags::IS_STUNNED;
            }
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, StatusApplication, TargetType,
    };
    use crate::core::{AbilityId, DiceExpr, StatusId};
    use crate::game::hex::hex_from_offset;
    use std::collections::HashMap;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    fn unit(id: u32, team: Team, pos: Hex, hp: i32, armor: i32) -> UnitSnapshot {
        UnitSnapshot {
            entity: ent(id),
            team,
            role: AxisProfile::from(AiRole::Bruiser),
            pos,
            hp,
            max_hp: 20,
            armor,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: 1,
            max_ap: 1,
            movement_points: 3,
            speed: 3,
            mana: Some((5, 10)),
            rage: None,
            energy: None,
            abilities: vec![],
            threat: 5.0,
            tags: AiTags::empty(),
            max_attack_range: 1,
            summoner: None,
            reactions_left: 0,
            aoo_expected_damage: None,
        }
    }

    fn snap(units: Vec<UnitSnapshot>, active: Entity) -> BattleSnapshot {
        BattleSnapshot { units, active_unit: active, round: 1 }
    }

    fn ctx(str_mod: i32, int_mod: i32) -> CasterContext {
        CasterContext { str_mod, int_mod, spell_power: 0, weapon_dice: None }
    }

    fn empty_content() -> ContentView {
        ContentView {
            abilities: HashMap::new(),
            keyed_abilities: Vec::new(),
            statuses: HashMap::new(),
            weapons: HashMap::new(),
            armor: HashMap::new(),
            classes: HashMap::new(),
            unit_templates: HashMap::new(),
            races: HashMap::new(),
            factions: HashMap::new(),
            paths: HashMap::new(),
        }
    }

    fn ability(
        id: &str,
        effect: EffectDef,
        target_type: TargetType,
        range: u32,
    ) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.to_string(),
            target_type,
            range: AbilityRange { min: 0, max: range },
            effect,
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    // ── damage / armor / kill ───────────────────────────────────────────────

    #[test]
    fn damage_subtracts_armor_and_decrements_hp() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 2);
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        // 1d6 (expected 3.5) + str_mod(4) = 7.5. armor 2 → raw 5.5 → hp: 20-5=15.
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target], actor_id), actor_id);
        let step = PlanStep::Cast {
            ability: def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        };
        let outcome = sim.apply_step(&step, &ctx(4, 0), &content);

        let t = sim.snapshot.unit(target_id).unwrap();
        // 20 - 5.5 = 14.5, truncated to 14 via `as i32`.
        assert_eq!(t.hp, 14, "expected 7.5 - armor 2 = 5.5 raw, 20-5.5 trunc = 14, got hp={}", t.hp);
        assert!((outcome.damage - 5.5).abs() < 0.01, "raw damage {}", outcome.damage);
        assert_eq!(outcome.hits, 1);
        assert!(outcome.killed.is_empty());
    }

    #[test]
    fn lethal_damage_removes_unit_and_records_kill() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 3, 0);
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        // 1d6 + str_mod(4) = 7.5 damage vs 3 hp → lethal.
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target], actor_id), actor_id);
        let step = PlanStep::Cast {
            ability: def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        };
        let outcome = sim.apply_step(&step, &ctx(4, 0), &content);

        assert_eq!(outcome.killed, vec![target_id]);
        assert!(sim.snapshot.unit(target_id).is_none(), "killed unit should be pruned");
    }

    // ── heal ───────────────────────────────────────────────────────────────

    #[test]
    fn heal_caps_at_missing_hp() {
        let actor = unit(1, Team::Player, hex_from_offset(0, 0), 20, 0);
        let ally = unit(2, Team::Player, hex_from_offset(1, 0), 15, 0);
        let actor_id = actor.entity;
        let ally_id = ally.entity;

        let mut content = empty_content();
        // Heal 3d6 (expected 10.5) but target is missing only 5.
        let def = ability(
            "cure",
            EffectDef::Heal { dice: DiceExpr::new(3, 6, 0) },
            TargetType::SingleAlly,
            2,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, ally], actor_id), actor_id);
        let step = PlanStep::Cast {
            ability: def.id.clone(),
            target: ally_id,
            target_pos: hex_from_offset(1, 0),
        };
        let outcome = sim.apply_step(&step, &ctx(0, 2), &content);

        let a = sim.snapshot.unit(ally_id).unwrap();
        assert_eq!(a.hp, 20, "heal must clamp to max_hp");
        assert!((outcome.heal - 5.0).abs() < 0.01, "effective heal {}", outcome.heal);
    }

    // ── resource / AP / MP accounting ───────────────────────────────────────

    #[test]
    fn cast_decrements_ap_and_pays_mana() {
        let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        actor.action_points = 2;
        actor.max_ap = 2;
        let actor_id = actor.entity;
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 0);
        let target_id = target.entity;

        let mut content = empty_content();
        let mut def = ability(
            "bolt",
            EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
            TargetType::SingleEnemy,
            3,
        );
        def.cost_ap = 1;
        def.costs = vec![crate::content::abilities::ResourceCost {
            resource: ResourceKind::Mana,
            amount: 3,
        }];
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target], actor_id), actor_id);
        sim.apply_step(
            &PlanStep::Cast {
                ability: def.id.clone(),
                target: target_id,
                target_pos: hex_from_offset(1, 0),
            },
            &ctx(0, 2),
            &content,
        );

        let a = sim.snapshot.unit(actor_id).unwrap();
        assert_eq!(a.action_points, 1, "AP drops from 2 to 1");
        assert_eq!(a.mana, Some((2, 10)), "mana 5 - 3 = 2");
    }

    #[test]
    fn move_step_updates_pos_and_drains_mp() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let actor_id = actor.entity;
        let target = hex_from_offset(2, 0);

        let content = empty_content();
        let mut sim = SimState::from_snapshot(&snap(vec![actor], actor_id), actor_id);
        let outcome = sim.apply_step(
            &PlanStep::Move { path: vec![hex_from_offset(1, 0), target] },
            &ctx(0, 0),
            &content,
        );

        assert!(outcome.moved);
        let a = sim.snapshot.unit(actor_id).unwrap();
        assert_eq!(a.pos, target);
        assert_eq!(a.movement_points, 1, "speed 3 - path 2 = 1");
    }

    // ── stun status ─────────────────────────────────────────────────────────

    #[test]
    fn stun_status_is_recorded_in_outcome_and_tags() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 0);
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();

        use crate::content::statuses::StatusDef;
        let stun_def = StatusDef {
            id: StatusId::from("stunned"),
            name: "Stunned".to_string(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: true,
            forces_targeting: false,
            dot_dice: None,
            blocks_mana_abilities: false,
            speed_bonus: 0,
            hp_percent_dot: 0,
            ai_controlled: false,
            causes_disadvantage: false,
        };
        content.statuses.insert(StatusId::from("stunned"), stun_def);

        let mut def = ability(
            "shock",
            EffectDef::None,
            TargetType::SingleEnemy,
            2,
        );
        def.statuses = vec![StatusApplication {
            status: StatusId::from("stunned"),
            duration_rounds: 1,
            on: StatusOn::Target,
        }];
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target], actor_id), actor_id);
        let outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: def.id.clone(),
                target: target_id,
                target_pos: hex_from_offset(1, 0),
            },
            &ctx(0, 0),
            &content,
        );

        assert_eq!(outcome.stunned, vec![target_id]);
        let t = sim.snapshot.unit(target_id).unwrap();
        assert!(t.tags.contains(AiTags::IS_STUNNED));
    }

    // ── AoE ─────────────────────────────────────────────────────────────────

    #[test]
    fn aoe_circle_hits_all_enemies_in_radius() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let t1 = unit(2, Team::Player, hex_from_offset(3, 0), 20, 0);
        let t2 = unit(3, Team::Player, hex_from_offset(4, 0), 20, 0);
        let actor_id = actor.entity;
        let t1_id = t1.entity;
        let t2_id = t2.entity;

        let mut content = empty_content();
        let mut def = ability(
            "blast",
            EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
            TargetType::SingleEnemy,
            5,
        );
        def.aoe = AoEShape::Circle { radius: 1 };
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, t1, t2], actor_id),
            actor_id,
        );
        let outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: def.id.clone(),
                target: t1_id,
                target_pos: hex_from_offset(3, 0),
            },
            &ctx(0, 0),
            &content,
        );

        assert_eq!(outcome.hits, 2, "radius-1 centered at (3,0) covers both (3,0) and (4,0)");
        assert!(sim.snapshot.unit(t1_id).unwrap().hp < 20);
        assert!(sim.snapshot.unit(t2_id).unwrap().hp < 20);
    }

    // ── GrantMovement ───────────────────────────────────────────────────────

    #[test]
    fn grant_movement_adds_mp_and_pays_ap() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let actor_id = actor.entity;

        let mut content = empty_content();
        let def = ability(
            "rush",
            EffectDef::GrantMovement { distance: 4 },
            TargetType::Myself,
            0,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor], actor_id), actor_id);
        let outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: def.id.clone(),
                target: actor_id,
                target_pos: hex_from_offset(0, 0),
            },
            &ctx(0, 0),
            &content,
        );

        let a = sim.snapshot.unit(actor_id).unwrap();
        assert_eq!(a.movement_points, 7, "3 base + 4 granted");
        assert_eq!(a.action_points, 0, "still costs 1 AP");
        assert_eq!(outcome.hits, 0, "GrantMovement has no targets");
    }
}

