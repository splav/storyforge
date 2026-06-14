//! Outcome builder — constructs `ActionOutcomeEstimate` from sim steps or
//! hypothetical (def + target) inputs.
//!
//! Two primary entry points:
//! - [`from_sim_step`] — used by `generator`'s beam search after each
//!   `apply_step`; has access to sim result + pre-step snapshot.
//! - [`hypothetical`] — used by `future_value::λ_attack` and
//!   `picker::record_committed_reservations` where no sim step has been
//!   executed; derives outcome from ability def + target alone.
//!
//! All private helpers live here; `outcome::mod.rs` re-exports the public API.

use crate::combat::ai::orchestration::AiWorld;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::plan::types::PlanStep;
use crate::combat::ai::scoring::status_applications;
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::content::abilities::{AbilityDef, AoEShape, CasterContext, EffectCalcExt};
use crate::content::content_view::ActiveContentData;
use crate::content::races::CritFailEffect;
use crate::game::components::Team;
use bevy::prelude::Entity;
use combat_engine::ResourceKind;

// ---------------------------------------------------------------------------
// Primary public API
// ---------------------------------------------------------------------------

/// Builds `ActionOutcomeEstimate` from a sim step result.
///
/// Used by generator's beam search after each `apply_step`. Populates fact
/// fields only — no policy weighting.
///
/// Uses the pre-step snapshot for target reads so killed targets (hp→0 in
/// `outcome.killed`) are still visible via their pre-death stats.
///
/// `caster_tile` is the actor's position before this step — needed to compute
/// the AoE blast area for multi-target p_kill_soon and status aggregation.
#[allow(clippy::too_many_arguments)]
pub fn from_sim_step(
    step: &PlanStep,
    outcome: &crate::combat::ai::plan::types::StepOutcome,
    step_damage: f32,
    pre_snap: &BattleSnapshot,
    caster: &CasterContext,
    _crit_fail_effect: &CritFailEffect,
    ctx: &AiWorld,
    maps: &InfluenceMaps,
    caster_tile: crate::game::hex::Hex,
    actor_unit_team: Team,
    actor_entity: Entity,
) -> ActionOutcomeEstimate {
    match step {
        PlanStep::Cast {
            ability,
            target,
            target_pos,
        } => {
            let content = ctx.content;
            let Some(def) = content.abilities.get(ability) else {
                return ActionOutcomeEstimate {
                    enemy_damage: step_damage,
                    ..Default::default()
                };
            };
            let target_unit = pre_snap.unit(*target).map(|v| v.state);

            // ── Kill facts ──
            let p_kill_now = if outcome.killed.is_empty() { 0.0 } else { 1.0 };

            // ── p_kill_soon ──
            let p_kill_soon = if def.aoe == AoEShape::None {
                target_unit.map_or(0.0, |t| estimate_kill_soon(def, t, caster, content))
            } else {
                aoe_p_kill_soon(
                    def,
                    *target_pos,
                    caster_tile,
                    actor_unit_team,
                    pre_snap,
                    caster,
                    content,
                )
            };

            // ── Damage facts ──
            let dmg_facts = build_damage_facts(
                def,
                *target_pos,
                *target,
                caster_tile,
                actor_unit_team,
                actor_entity,
                pre_snap,
                caster,
                step_damage,
            );

            // ── Status facts ──
            let status_facts = build_status_facts(
                def,
                *target,
                *target_pos,
                caster_tile,
                actor_unit_team,
                pre_snap,
                content,
            );

            // ── Support facts ──
            let hp_restored = target_unit.map_or(0.0, |t| estimate_hp_restored(def, t, caster));

            // ── Resource facts ──
            let res_facts = split_resource_costs(def);

            ActionOutcomeEstimate {
                enemy_damage: dmg_facts.enemy_damage,
                enemy_damage_per_entity: dmg_facts.enemy_damage_per_entity,
                ally_damage: dmg_facts.ally_damage,
                ally_damage_per_entity: dmg_facts.ally_damage_per_entity,
                self_damage: dmg_facts.self_damage,
                p_kill_now,
                p_kill_soon,
                cc_turns_applied: status_facts.cc_turns_applied,
                vulnerability_applied: status_facts.vulnerability_applied,
                armor_shred_applied: status_facts.armor_shred_applied,
                hp_restored,
                path_max_danger: 0.0,
                mp_spent: 0,
                ap_spent: res_facts.ap_spent,
                mana_spent: res_facts.mana_spent,
                rage_spent: res_facts.rage_spent,
                other_resource_spent: res_facts.other_resource_spent,
            }
        }
        PlanStep::Move { path } => {
            let path_max_danger = step_path_danger(step, maps);
            // Optimistic full-path estimate. The engine may truncate a move mid-path
            // on an interrupt — an AoO provoked by leaving an enemy's reach, or a
            // hidden hazard the unit self-reveals while moving (the planner can't see
            // hidden env; snapshot.rs strips unrevealed). The AI self-corrects by
            // re-planning next frame, so mp_spent may overestimate spent movement for
            // such truncated moves.
            let mp_spent = path.len() as i32;

            ActionOutcomeEstimate {
                path_max_danger,
                mp_spent,
                ..Default::default()
            }
        }
    }
}

/// Builds `ActionOutcomeEstimate` without sim — for consumers without sim context
/// (`future_value::λ_attack`, `picker::record_committed_reservations`).
///
/// First-class parallel API to [`from_sim_step`]. Same outcome shape; precision
/// is hypothetical (no sim verification — all fields derived from ability def +
/// target). Fact fields only — no policy weighting.
///
/// Populates:
/// - `enemy_damage` — raw post-armor damage for single-target (formula-derived);
///   0 for heal / status-only / GrantMovement.
/// - `p_kill_now` / `p_kill_soon` — kill detection via same formula as from_sim_step.
/// - `cc_turns_applied` / `vulnerability_applied` / `armor_shred_applied` —
///   status facts from ability def (single-target only; AoE not applicable here
///   since callers have no area context).
/// - `hp_restored` — raw clamped heal for heal abilities.
/// - Resource fields — from `split_resource_costs`.
pub fn hypothetical(
    def: &AbilityDef,
    target: &combat_engine::state::Unit,
    caster: &CasterContext,
    content: &ActiveContentData,
) -> ActionOutcomeEstimate {
    // ── Damage fact ──
    let enemy_damage = if let Some(calc) = def.effect.calc(caster) {
        if calc.is_heal {
            0.0
        } else {
            let mit = if calc.pierces_armor {
                0.0
            } else {
                combat_engine::mitigation(
                    target.runtime.armor,
                    target.armor_bonus,
                    target.runtime.magic_resist,
                    calc.magic,
                )
            };
            (calc.expected() - mit + target.damage_taken_bonus as f32).max(0.0)
        }
    } else {
        0.0
    };

    // ── Kill facts ──
    let p_kill_now = if enemy_damage >= target.hp().max(1) as f32 {
        1.0
    } else {
        0.0
    };
    let p_kill_soon = if p_kill_now == 0.0 {
        estimate_kill_soon(def, target, caster, content)
    } else {
        0.0
    };

    // ── Status facts (single-target) ──
    let mut cc_turns_applied = 0.0f32;
    let mut vulnerability_applied = 0.0f32;
    let mut armor_shred_applied = 0.0f32;
    for (sd, dur) in status_applications(def, content) {
        if sd.skips_turn {
            cc_turns_applied += dur;
        }
        if sd.bonuses.damage_taken_bonus != 0 {
            vulnerability_applied += sd.bonuses.damage_taken_bonus as f32 * dur;
        }
        if sd.bonuses.armor_bonus != 0 {
            armor_shred_applied += sd.bonuses.armor_bonus as f32 * dur;
        }
    }

    // ── Support facts ──
    let hp_restored = estimate_hp_restored(def, target, caster);

    // ── Resource facts ──
    let res_facts = split_resource_costs(def);

    ActionOutcomeEstimate {
        enemy_damage,
        p_kill_now,
        p_kill_soon,
        cc_turns_applied,
        vulnerability_applied,
        armor_shred_applied,
        hp_restored,
        ap_spent: res_facts.ap_spent,
        mana_spent: res_facts.mana_spent,
        rage_spent: res_facts.rage_spent,
        other_resource_spent: res_facts.other_resource_spent,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Extraction helpers (step 4.2)
// ---------------------------------------------------------------------------

/// `p_kill_soon` component of `ActionOutcomeEstimate`.
///
/// Returns `1.0` if `def`'s direct damage won't kill `target` now but the
/// accumulated DoT (pending on target + newly applied by this ability) will.
/// Returns `0.0` otherwise (including when direct damage already kills — that
/// case is covered by `p_kill_now` via sim's `StepOutcome.killed`).
pub fn estimate_kill_soon(
    def: &AbilityDef,
    target: &combat_engine::state::Unit,
    caster: &CasterContext,
    content: &ActiveContentData,
) -> f32 {
    let Some(calc) = def.effect.calc(caster) else {
        return 0.0;
    };
    let mit = if calc.pierces_armor {
        0.0
    } else {
        combat_engine::mitigation(
            target.runtime.armor,
            target.armor_bonus,
            target.runtime.magic_resist,
            calc.magic,
        )
    };
    let net = calc.expected().round() - mit + target.damage_taken_bonus as f32;
    // kill_now case — no kill_soon when net already kills
    if net >= target.hp() as f32 {
        return 0.0;
    }
    let pending_dot = already_pending_dot(target);
    let new_dot = dot_tick_sum_for_ability(def, target, content);
    if net + pending_dot + new_dot >= target.hp() as f32 {
        1.0
    } else {
        0.0
    }
}

/// Max danger value along the path tiles of a single Move step.
/// Returns `0.0` for Cast steps.
///
/// Shared helper for `exposure_delta` in the outcome estimate. Uses only the
/// current step's path (not the whole plan) so each step's annotation is
/// independent.
pub fn step_path_danger(step: &PlanStep, maps: &InfluenceMaps) -> f32 {
    let PlanStep::Move { path } = step else {
        return 0.0;
    };
    path.iter()
        .map(|&h| maps.danger.get(h))
        .fold(0.0f32, f32::max)
}

// ---------------------------------------------------------------------------
// Private helpers for estimate_kill_soon
// ---------------------------------------------------------------------------

fn already_pending_dot(target: &combat_engine::state::Unit) -> f32 {
    target
        .statuses
        .iter()
        .map(|s| s.dot_per_tick.max(0) as f32 * s.rounds_remaining as f32)
        .sum()
}

fn dot_tick_sum_for_ability(
    def: &AbilityDef,
    target: &combat_engine::state::Unit,
    content: &ActiveContentData,
) -> f32 {
    status_applications(def, content)
        .map(|(sd, dur)| {
            let per_tick = sd.dot_dice.as_ref().map(|d| d.expected()).unwrap_or(0.0)
                + sd.hp_percent_dot as f32 / 100.0 * target.max_hp() as f32;
            per_tick * dur
        })
        .filter(|&v| v > 0.0)
        .sum()
}

// ---------------------------------------------------------------------------
// Fact-vector helpers
// ---------------------------------------------------------------------------

/// Damage facts split by relation to the actor (enemy / ally / self).
///
/// For single-target casts `enemy_damage_per_entity` is left empty — the
/// caller stores `enemy_damage` directly. For AoE casts the vector is
/// populated with one entry per enemy in the blast area, enabling step-10
/// critics to inspect per-target impact.
pub(crate) struct DamageFacts {
    pub enemy_damage: f32,
    pub enemy_damage_per_entity: Vec<(Entity, f32)>,
    pub ally_damage: f32,
    pub ally_damage_per_entity: Vec<(Entity, f32)>,
    pub self_damage: f32,
}

/// Build `DamageFacts` for a single Cast step by walking the AoE area.
///
/// For AoE casts: resolves the blast area, computes expected net damage for
/// each unit in that area, and separates enemies / allies / self. For
/// single-target casts (AoE == None): uses `sim_damage` from the step
/// outcome as the single enemy damage fact; `_per_entity` vecs stay empty.
///
/// `sim_damage` is the raw `StepOutcome.damage` value passed from the
/// generator — used as a reference for single-target facts so they agree
/// with the sim rather than re-deriving independently.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_damage_facts(
    def: &AbilityDef,
    target_pos: crate::game::hex::Hex,
    target: Entity,
    caster_tile: crate::game::hex::Hex,
    actor_team: crate::game::components::Team,
    actor_entity: Entity,
    pre_snap: &BattleSnapshot,
    caster: &CasterContext,
    sim_damage: f32,
) -> DamageFacts {
    use crate::combat::ai::scoring::factors::aoe_area;
    use crate::content::abilities::AoEShape;

    if def.aoe == AoEShape::None {
        // Single-target: use sim_damage directly.
        let raw = sim_damage;
        return DamageFacts {
            enemy_damage: raw,
            enemy_damage_per_entity: vec![],
            ally_damage: 0.0,
            ally_damage_per_entity: vec![],
            self_damage: 0.0,
        };
    }

    // AoE: walk the blast area.
    let area = aoe_area(def, target_pos, caster_tile);

    let Some(calc) = def.effect.calc(caster) else {
        return DamageFacts {
            enemy_damage: sim_damage,
            enemy_damage_per_entity: vec![],
            ally_damage: 0.0,
            ally_damage_per_entity: vec![],
            self_damage: 0.0,
        };
    };

    // For each unit in the area, compute expected net damage (post-armor).
    let net_damage_for = |unit: &combat_engine::state::Unit| -> f32 {
        let mit = if calc.pierces_armor {
            0.0
        } else {
            combat_engine::mitigation(
                unit.runtime.armor,
                unit.armor_bonus,
                unit.runtime.magic_resist,
                calc.magic,
            )
        };
        (calc.expected() - mit + unit.damage_taken_bonus as f32).max(0.0)
    };

    let mut enemy_damage = 0.0f32;
    let mut enemy_damage_per_entity: Vec<(Entity, f32)> = vec![];
    let mut ally_damage = 0.0f32;
    let mut ally_damage_per_entity: Vec<(Entity, f32)> = vec![];
    let mut self_damage = 0.0f32;

    // Iterate all live units (enemies + allies + self) via UnitView.
    let opponent_team = crate::combat::ai::world::snapshot::opponent_team(actor_team);
    for view in pre_snap
        .enemies_of(actor_team)
        .chain(pre_snap.allies_of(actor_team))
        .chain(pre_snap.unit(actor_entity).into_iter())
        .filter(|v| area.contains(&v.state.pos))
    {
        let entity = view.entity();
        let dmg = net_damage_for(view.state);
        if entity == actor_entity {
            self_damage += dmg;
        } else if view.state.team != opponent_team {
            ally_damage += dmg;
            ally_damage_per_entity.push((entity, dmg));
        } else {
            enemy_damage += dmg;
            enemy_damage_per_entity.push((entity, dmg));
        }
    }

    // Fallback: if no enemies found in area but sim reported damage, use sim value
    // (e.g. target was killed between area snap and sim application).
    if enemy_damage == 0.0 && sim_damage > 0.0 && enemy_damage_per_entity.is_empty() {
        enemy_damage = sim_damage;
        enemy_damage_per_entity.push((target, sim_damage));
    }

    DamageFacts {
        enemy_damage,
        enemy_damage_per_entity,
        ally_damage,
        ally_damage_per_entity,
        self_damage,
    }
}

/// `p_kill_soon` for AoE: 1.0 if any enemy in the area can be finished
/// by direct + pending DoT + new DoT from this ability.
pub(crate) fn aoe_p_kill_soon(
    def: &AbilityDef,
    target_pos: crate::game::hex::Hex,
    caster_tile: crate::game::hex::Hex,
    actor_team: crate::game::components::Team,
    pre_snap: &BattleSnapshot,
    caster: &CasterContext,
    content: &ActiveContentData,
) -> f32 {
    use crate::combat::ai::scoring::factors::aoe_area;
    let area = aoe_area(def, target_pos, caster_tile);
    let any = pre_snap
        .enemies_of(actor_team)
        .filter(|v| v.is_alive() && area.contains(&v.state.pos))
        .any(|v| estimate_kill_soon(def, v.state, caster, content) > 0.0);
    if any {
        1.0
    } else {
        0.0
    }
}

/// Aggregate status facts over enemies hit by this ability.
///
/// Walks the ability's status applications once and accumulates:
/// - `cc_turns_applied`: Σ skips_turn × duration per enemy hit.
/// - `vulnerability_applied`: Σ damage_taken_bonus × duration per enemy hit.
/// - `armor_shred_applied`: Σ armor_bonus × duration per enemy hit
///   (negative armor_bonus = shred, but stored as-is for consumer to interpret).
pub(crate) struct StatusFacts {
    pub cc_turns_applied: f32,
    pub vulnerability_applied: f32,
    pub armor_shred_applied: f32,
}

pub(crate) fn build_status_facts(
    def: &AbilityDef,
    target: Entity,
    target_pos: crate::game::hex::Hex,
    caster_tile: crate::game::hex::Hex,
    actor_team: crate::game::components::Team,
    pre_snap: &BattleSnapshot,
    content: &ActiveContentData,
) -> StatusFacts {
    use crate::combat::ai::scoring::factors::aoe_area;
    use crate::content::abilities::AoEShape;

    // Collect enemies that will receive status applications.
    let enemy_targets: Vec<UnitView<'_>> = if def.aoe == AoEShape::None {
        pre_snap.unit(target).into_iter().collect()
    } else {
        let area = aoe_area(def, target_pos, caster_tile);
        pre_snap
            .enemies_of(actor_team)
            .filter(|v| v.is_alive() && area.contains(&v.state.pos))
            .collect()
    };

    let n = enemy_targets.len() as f32;
    if n == 0.0 {
        return StatusFacts {
            cc_turns_applied: 0.0,
            vulnerability_applied: 0.0,
            armor_shred_applied: 0.0,
        };
    }

    let mut cc_turns = 0.0f32;
    let mut vuln = 0.0f32;
    let mut shred = 0.0f32;

    for (sd, dur) in status_applications(def, content) {
        if sd.skips_turn {
            cc_turns += dur * n;
        }
        if sd.bonuses.damage_taken_bonus != 0 {
            vuln += sd.bonuses.damage_taken_bonus as f32 * dur * n;
        }
        if sd.bonuses.armor_bonus != 0 {
            shred += sd.bonuses.armor_bonus as f32 * dur * n;
        }
    }

    StatusFacts {
        cc_turns_applied: cc_turns,
        vulnerability_applied: vuln,
        armor_shred_applied: shred,
    }
}

/// Raw HP restored by a heal ability, clamped to the target's missing HP.
///
/// Returns 0.0 for non-heal abilities or full-HP targets.
/// This is a pure fact (no policy weighting) — `policy::heal::value` applies
/// urgency / horizon on top of this value.
pub(crate) fn estimate_hp_restored(
    def: &AbilityDef,
    target: &combat_engine::state::Unit,
    caster: &CasterContext,
) -> f32 {
    let Some(calc) = def.effect.calc(caster) else {
        return 0.0;
    };
    if !calc.is_heal {
        return 0.0;
    }
    let missing = (target.max_hp() - target.hp()) as f32;
    if missing <= 0.0 {
        return 0.0;
    }
    calc.expected().min(missing)
}

/// Resource facts split by kind.
pub(crate) struct ResourceFacts {
    pub ap_spent: i32,
    pub mana_spent: i32,
    pub rage_spent: i32,
    pub other_resource_spent: i32,
}

/// Split resource costs of an ability by `ResourceKind`.
///
/// `ap_spent` is taken from `def.cost_ap`; other costs come from `def.costs`.
pub(crate) fn split_resource_costs(def: &AbilityDef) -> ResourceFacts {
    let mut mana = 0i32;
    let mut rage = 0i32;
    let mut other = 0i32;
    for cost in &def.costs {
        match cost.resource {
            ResourceKind::Mana => mana += cost.amount,
            ResourceKind::Rage => rage += cost.amount,
            _ => other += cost.amount,
        }
    }
    ResourceFacts {
        ap_spent: def.cost_ap,
        mana_spent: mana,
        rage_spent: rage,
        other_resource_spent: other,
    }
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
