use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
use crate::combat::ai::world::snapshot::UnitView;
use crate::content::abilities::{AbilityDef, CasterContext, EffectCalcExt, TargetType};
use crate::content::content_view::ActiveContentData;
use crate::content::statuses::StatusDef;
use crate::game::components::Abilities;
use crate::game::hex::Hex;
use combat_engine::final_damage_f32;
use combat_engine::ResourceKind;

/// True if the ability applies any status that skips the target's turn
/// (stun, paralyse, sleep…). Single source of truth for "is this CC?".
pub fn applies_cc(def: &AbilityDef, content: &ActiveContentData) -> bool {
    def.statuses.iter().any(|sa| {
        content
            .statuses
            .get(&sa.status)
            .is_some_and(|sd| sd.skips_turn)
    })
}

/// Sum of projected damage denied to `target` by `def`'s stun-class statuses.
/// Shared by the `cc` factor and the `scarcity` swing-justification branch so
/// their stun formulas can't drift apart. Non-stun effects (vulnerability, armor
/// shred, DoT) are excluded — each caller folds those in with its own weighting.
pub fn stun_denial_value(
    def: &AbilityDef,
    target: crate::combat::ai::world::snapshot::UnitView<'_>,
    content: &ActiveContentData,
) -> f32 {
    status_applications(def, content)
        .filter(|(sd, _)| sd.skips_turn)
        .map(|(_, d)| horizon_window_sum_raw(&target.cache.damage_horizon, target.cache.threat, d))
        .sum()
}

/// Yield `(StatusDef, duration_rounds)` pairs for every status application on
/// `def`, resolving ids against `content`. Skips ids not in the registry.
///
/// Single source of truth for the iterate-statuses-and-lookup pattern shared by
/// `status_score` and `factors::offensive::status_cc_value`; only the iteration
/// shape is shared — each caller keeps its own value-accumulating closure.
pub fn status_applications<'a, 'c: 'a>(
    def: &'a AbilityDef,
    content: &'c ActiveContentData,
) -> impl Iterator<Item = (&'c StatusDef, f32)> + 'a {
    def.statuses.iter().filter_map(move |sa| {
        content
            .statuses
            .get(&sa.status)
            .map(|sd| (sd, sa.duration_rounds as f32))
    })
}

/// Damage denied by removing the target's actions for `duration` rounds: sum of
/// the first `ceil(duration)` rounds of its damage horizon. Falls back to
/// `threat × duration` when the horizon is empty (legacy logs, fixtures).
///
/// Used by `status_score` skips_turn/blocks_mana branches and the CC factor.
pub fn horizon_window_sum(
    target: crate::combat::ai::world::snapshot::UnitView<'_>,
    duration: f32,
) -> f32 {
    horizon_window_sum_raw(&target.cache.damage_horizon, target.cache.threat, duration)
}

/// Raw implementation shared by `horizon_window_sum` (takes `UnitView`)
/// and `stun_denial_value` (takes `UnitView` whose AI metrics live in `cache`).
fn horizon_window_sum_raw(damage_horizon: &[f32], threat: f32, duration: f32) -> f32 {
    if damage_horizon.is_empty() {
        return threat * duration.max(0.0);
    }
    let n = (duration.ceil() as usize).min(damage_horizon.len());
    damage_horizon.iter().take(n).sum()
}

/// Average projected damage per round across the full horizon.
/// Falls back to `target.threat` when the horizon is empty.
///
/// Used where the call site needs a single per-round scalar (scarcity
/// bonus, intent CC weight) and wants the DPR-equivalent rather than a
/// duration-specific sum.
pub fn horizon_avg(target: UnitView<'_>) -> f32 {
    if target.cache.damage_horizon.is_empty() {
        return target.cache.threat;
    }
    let n = target.cache.damage_horizon.len() as f32;
    target.cache.damage_horizon.iter().sum::<f32>() / n.max(1.0)
}

/// Best single-target expected damage from one ability (before armor).
/// Used to value stuns/kills: controlling a high-damage target is worth more.
/// Does NOT capture AoE, healing, or utility — it's a damage-only estimate.
pub fn estimate_st_damage(
    ctx: &CasterContext,
    abilities: &Abilities,
    content: &ActiveContentData,
) -> f32 {
    abilities
        .0
        .iter()
        .filter_map(|id| content.abilities.get(id))
        // SingleEnemy = direct attack; Ground = area-landed attack at a cell.
        // Both produce per-cast damage; the `effect.calc` filter below
        // drops non-damaging variants (teleport/spawn Ground spells), so
        // the blanket include here is safe.
        .filter(|def| {
            matches!(
                def.target_type,
                TargetType::SingleEnemy | TargetType::Ground
            )
        })
        .filter_map(|def| def.effect.calc(ctx, def.engine.power()))
        .map(|calc| calc.expected().max(0.0))
        .fold(0.0f32, f32::max)
}

/// Project expected single-target damage over the next `rounds` rounds,
/// accounting for AP budget and resource (mana/rage/energy/HP) depletion.
/// Returns a `Vec<f32>` of length `rounds`; index `i` is round `i+1`.
///
/// **Greedy** over damage-per-AP: each round spends the AP budget starting with
/// the best-per-AP castable ability. **Regen**: each subsequent round adds +1 to
/// mana/rage/energy, capped at max (conservative lower bound — real sustain may
/// be higher via AoE / multi-hit rage triggers).
///
/// Replaces `UnitSnapshot.threat` (peak single-cast) for CC/heal/stun consumers,
/// since peak over-weighted resource-limited burst casters.
#[allow(clippy::too_many_arguments)]
pub fn estimate_damage_horizon(
    caster: &CasterContext,
    abilities: &Abilities,
    content: &ActiveContentData,
    max_ap_per_round: i32,
    mana: Option<(i32, i32)>, // (current, max)
    rage: Option<(i32, i32)>,
    energy: Option<(i32, i32)>,
    hp: i32,
    rounds: u32,
) -> Vec<f32> {
    // Per damaging SingleEnemy/Ground ability: (expected, cost_ap, costs, dpa).
    // 0-AP abilities are filtered — they'd spin the inner loop forever.
    struct AbilityProjection<'a> {
        expected: f32,
        cost_ap: i32,
        costs: &'a [crate::content::abilities::ResourceCost],
        dpa: f32, // damage-per-AP, for sort
    }
    let projections: Vec<AbilityProjection> = abilities
        .0
        .iter()
        .filter_map(|id| content.abilities.get(id))
        .filter(|def| {
            matches!(
                def.target_type,
                TargetType::SingleEnemy | TargetType::Ground
            )
        })
        .filter(|def| def.cost_ap > 0)
        .filter_map(|def| {
            let calc = def.effect.calc(caster, def.engine.power())?;
            let expected = calc.expected().max(0.0);
            if expected <= 0.0 {
                return None;
            }
            Some(AbilityProjection {
                expected,
                cost_ap: def.cost_ap,
                costs: &def.costs,
                dpa: expected / def.cost_ap as f32,
            })
        })
        .collect();

    if projections.is_empty() {
        return vec![0.0; rounds as usize];
    }

    let mut sorted: Vec<&AbilityProjection> = projections.iter().collect();
    sorted.sort_by(|a, b| b.dpa.total_cmp(&a.dpa));

    let (mut pool_mana, max_mana) = mana.unwrap_or((0, 0));
    let (mut pool_rage, max_rage) = rage.unwrap_or((0, 0));
    let (mut pool_energy, max_energy) = energy.unwrap_or((0, 0));
    let mut pool_hp = hp;

    let mut out = Vec::with_capacity(rounds as usize);
    for round in 0..rounds {
        // Start-of-round regen: +1 to each pool, capped at max. Round 0 uses
        // current pools as-is (engine already regen'd this turn).
        if round > 0 {
            if max_mana > 0 {
                pool_mana = (pool_mana + 1).min(max_mana);
            }
            if max_rage > 0 {
                pool_rage = (pool_rage + 1).min(max_rage);
            }
            if max_energy > 0 {
                pool_energy = (pool_energy + 1).min(max_energy);
            }
        }

        let mut ap_left = max_ap_per_round;
        let mut round_damage = 0.0f32;
        'ap: loop {
            for ability in &sorted {
                if ability.cost_ap > ap_left {
                    continue;
                }
                // Check every resource cost affordable.
                let affordable = ability.costs.iter().all(|c| match c.resource {
                    ResourceKind::Mana => pool_mana >= c.amount,
                    ResourceKind::Rage => pool_rage >= c.amount,
                    ResourceKind::Energy => pool_energy >= c.amount,
                    ResourceKind::Hp => pool_hp > c.amount, // strict: can't self-kill
                });
                if !affordable {
                    continue;
                }
                // Cast this one.
                round_damage += ability.expected;
                ap_left -= ability.cost_ap;
                for c in ability.costs {
                    match c.resource {
                        ResourceKind::Mana => pool_mana -= c.amount,
                        ResourceKind::Rage => pool_rage -= c.amount,
                        ResourceKind::Energy => pool_energy -= c.amount,
                        ResourceKind::Hp => pool_hp -= c.amount,
                    }
                }
                continue 'ap;
            }
            break; // nothing castable this round
        }
        out.push(round_damage);
    }
    out
}

// ── Internals ──────────────────────────────────────────────────────────────

/// One AoO hit triggered by a single Move step.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AooHit {
    /// Index into the `enemies` slice passed to `scan_aoo_hits_for_step`.
    pub enemy_idx: usize,
    /// Pre-mitigation expected damage (from `enemy.aoo_expected_damage`).
    pub raw_damage: f32,
}

/// Scan a single Move-step path for adjacency-leaving transitions vs each
/// enemy with `reactions_left > 0` and `aoo_expected_damage = Some(_)`.
/// `start_pos` is the actor's position **before** this step.
///
/// Returns one hit per provoked enemy (each enemy AoO's at most once per
/// step, even if the path leaves and re-enters adjacency).
pub(crate) fn scan_aoo_hits_for_step(
    start_pos: Hex,
    path: &[Hex],
    enemies: &[UnitView<'_>],
) -> Vec<AooHit> {
    let mut hits = Vec::new();
    for (enemy_idx, e) in enemies.iter().enumerate() {
        if e.reactions_left <= 0 {
            continue;
        }
        let Some(raw) = e.cache.aoo_expected_damage else {
            continue;
        };
        // Walk the path: detect first transition that leaves adjacency with
        // this enemy (prev adjacent, next not). Each enemy triggers at most
        // once per step.
        let mut prev = start_pos;
        for &h in path {
            if prev.unsigned_distance_to(e.pos) == 1 && h.unsigned_distance_to(e.pos) != 1 {
                hits.push(AooHit {
                    enemy_idx,
                    raw_damage: raw,
                });
                break;
            }
            prev = h;
        }
    }
    hits
}

/// Sum of expected AoO damage the plan would take, accrued against the actor's
/// armor. One AoO per enemy per plan (`aoo_used` tracks spent reactions). 0.0
/// when no provokers fire — fast path for non-adjacent moves.
///
/// Shared with the adaptation layer: the `ExpectedSelfLethal` trigger compares
/// this against `active.hp`, and `sanity_adjust_plans`' non-lethal penalty reads
/// the same number — kept here so they can't diverge.
pub(crate) fn expected_aoo_damage(
    active: UnitView<'_>,
    plan: &TurnPlan,
    enemies: &[UnitView<'_>],
) -> f32 {
    let mut total = 0.0f32;
    let mitigation = active.effective_armor() as f32;
    // Track which enemies have already spent their reaction this plan.
    let mut aoo_used = vec![false; enemies.len()];
    let mut prev_pos = active.pos;
    for step in &plan.steps {
        let PlanStep::Move { path } = step else {
            continue;
        };
        let hits = scan_aoo_hits_for_step(prev_pos, path, enemies);
        for hit in hits {
            if !aoo_used[hit.enemy_idx] {
                aoo_used[hit.enemy_idx] = true;
                total += final_damage_f32(hit.raw_damage, mitigation, false);
            }
        }
        // Advance actor position to end of this Move step.
        if let Some(&last) = path.last() {
            prev_pos = last;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, ResourceCost, StatusApplication, StatusOn,
    };
    use combat_engine::{AbilityId, DiceExpr, StatusId};
    use std::collections::HashMap;

    fn content_with(abs: Vec<AbilityDef>) -> ActiveContentData {
        let mut map: HashMap<AbilityId, AbilityDef> = HashMap::new();
        for d in abs {
            map.insert(d.id.clone(), d);
        }
        ActiveContentData {
            abilities: map,
            keyed_abilities: Vec::new(),
            statuses: HashMap::new(),
            weapons: HashMap::new(),
            armor: HashMap::new(),
            classes: HashMap::new(),
            unit_templates: HashMap::new(),
            races: HashMap::new(),
            factions: HashMap::new(),
            paths: HashMap::new(),
            ..ActiveContentData::default()
        }
    }

    fn weapon_attack_def(id: &str, cost_ap: i32, dice: DiceExpr) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 1 },
                effect: EffectDef::Damage { dice },
                costs: Vec::new(),
                cost_ap,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
                power: None,
            },
        }
    }

    fn mana_spell_def(id: &str, cost_ap: i32, dice: DiceExpr, mana_cost: i32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 5 },
                effect: EffectDef::Damage { dice },
                costs: vec![ResourceCost {
                    resource: ResourceKind::Mana,
                    amount: mana_cost,
                }],
                cost_ap,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
                power: None,
            },
        }
    }

    const ZERO: CasterContext = CasterContext {
        str_mod: 0,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };

    /// Free-attack warrior: one ability, no cost, cost_ap=1. Horizon over 5
    /// rounds with max_ap=1 should be a flat plateau of `expected` per round.
    #[test]
    fn horizon_free_attack_plateaus() {
        let melee = weapon_attack_def("melee", 1, DiceExpr::new(1, 6, 0));
        let ev = melee
            .effect
            .calc(&ZERO, melee.engine.power())
            .unwrap()
            .expected();
        let content = content_with(vec![melee.clone()]);
        let abilities = Abilities(vec![melee.id.clone()]);

        let h = estimate_damage_horizon(&ZERO, &abilities, &content, 1, None, None, None, 20, 5);
        assert_eq!(h.len(), 5);
        for (i, v) in h.iter().enumerate() {
            assert!((v - ev).abs() < 0.01, "round {i}: {v}, expected {ev}");
        }
    }

    /// Mana-limited burst mage: 1d10 spell @ 5 mana, 10 mana pool → 2 casts
    /// total across 5 rounds. Horizon front-loads, then drops to 0.
    #[test]
    fn horizon_exhausts_resource_pool() {
        let spell = mana_spell_def("bolt", 1, DiceExpr::new(1, 10, 0), 5);
        let ev = spell
            .effect
            .calc(&ZERO, spell.engine.power())
            .unwrap()
            .expected();
        let content = content_with(vec![spell.clone()]);
        let abilities = Abilities(vec![spell.id.clone()]);

        let h = estimate_damage_horizon(
            &ZERO,
            &abilities,
            &content,
            1,
            Some((10, 10)),
            None,
            None,
            20,
            5,
        );
        // First two rounds fire the spell, remaining rounds have no damage.
        assert!((h[0] - ev).abs() < 0.01, "round 0: {}", h[0]);
        assert!((h[1] - ev).abs() < 0.01, "round 1: {}", h[1]);
        assert_eq!(h[2], 0.0, "pool exhausted");
        assert_eq!(h[3], 0.0);
        assert_eq!(h[4], 0.0);
    }

    /// Mix: high-DPA mana spell (1d10 @ 5 mana, cost_ap 1) + free melee
    /// fallback (1d4 @ 0 mana, cost_ap 1). With 1 AP/round and 10 mana,
    /// greedy prefers the spell → 2 rounds of spell + 3 rounds of fallback.
    #[test]
    fn horizon_greedy_falls_back_after_resource_drain() {
        let spell = mana_spell_def("bolt", 1, DiceExpr::new(1, 10, 0), 5);
        let melee = weapon_attack_def("slap", 1, DiceExpr::new(1, 4, 0));
        let spell_ev = spell
            .effect
            .calc(&ZERO, spell.engine.power())
            .unwrap()
            .expected();
        let melee_ev = melee
            .effect
            .calc(&ZERO, melee.engine.power())
            .unwrap()
            .expected();
        let content = content_with(vec![spell.clone(), melee.clone()]);
        let abilities = Abilities(vec![spell.id.clone(), melee.id.clone()]);

        let h = estimate_damage_horizon(
            &ZERO,
            &abilities,
            &content,
            1,
            Some((10, 10)),
            None,
            None,
            20,
            5,
        );
        assert!((h[0] - spell_ev).abs() < 0.01);
        assert!((h[1] - spell_ev).abs() < 0.01);
        assert!(
            (h[2] - melee_ev).abs() < 0.01,
            "fell back to melee: {}",
            h[2]
        );
        assert!((h[3] - melee_ev).abs() < 0.01);
        assert!((h[4] - melee_ev).abs() < 0.01);
    }

    /// No damaging abilities → all-zeros Vec of correct length (never None),
    /// so summing call sites need no division-by-zero / Option handling.
    #[test]
    fn horizon_empty_for_non_damaging_actor() {
        let ping = AbilityDef {
            id: AbilityId::from("inspire"),
            name: "inspire".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                // not SingleEnemy
                target_type: TargetType::SingleAlly,
                range: AbilityRange { min: 0, max: 2 },
                effect: EffectDef::None,
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: vec![StatusApplication {
                    status: StatusId::from("inspired"),
                    duration_rounds: 2,
                    on: StatusOn::Target,
                }],
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
                power: None,
            },
        };
        let content = content_with(vec![ping.clone()]);
        let abilities = Abilities(vec![ping.id.clone()]);

        let h = estimate_damage_horizon(&ZERO, &abilities, &content, 2, None, None, None, 20, 5);
        assert_eq!(h.len(), 5);
        for v in h {
            assert_eq!(v, 0.0);
        }
    }

    /// Stun-value scoring now reads `damage_horizon` (DPR-correct) instead
    /// of `threat` (peak) — a burst mage who's already spent their pool
    /// should score lower to CC than a sustained fighter with the same
    /// `threat`. Pins the key user-visible effect of #6-B.
    #[test]
    fn stun_value_devalues_resource_starved_target() {
        use crate::combat::ai::test_helpers::UnitBuilder;
        use crate::combat::ai::world::snapshot::UnitView;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;

        fn make_pair(
            id: u32,
            threat: f32,
            horizon: Vec<f32>,
        ) -> (
            combat_engine::state::Unit,
            crate::combat::ai::world::cache::UnitAiCache,
        ) {
            UnitBuilder::new(id, Team::Player, hex_from_offset(0, 0))
                .threat(threat)
                .damage_horizon(horizon)
                .build_pair()
        }

        // Both targets have the SAME peak (`threat = 10`). The sustained
        // fighter keeps hitting every round; the burst mage fired twice
        // and ran out of mana.
        let (su, sc) = make_pair(1, 10.0, vec![10.0, 10.0, 10.0, 10.0, 10.0]);
        let (bu, bc) = make_pair(2, 10.0, vec![10.0, 10.0, 0.0, 0.0, 0.0]);
        let sustained = UnitView {
            state: &su,
            cache: &sc,
        };
        let burst = UnitView {
            state: &bu,
            cache: &bc,
        };

        // horizon_window_sum over a 3-round stun:
        let sustained_stun = horizon_window_sum(sustained, 3.0);
        let burst_stun = horizon_window_sum(burst, 3.0);
        assert!(
            burst_stun < sustained_stun,
            "burst mage stun value {burst_stun} must be < sustained {sustained_stun}",
        );
        assert!(
            (sustained_stun - 30.0).abs() < 0.01,
            "sustained: 3 rounds × 10 = 30, got {sustained_stun}",
        );
        assert!(
            (burst_stun - 20.0).abs() < 0.01,
            "burst: rounds 0+1 fire, round 2 empty = 20, got {burst_stun}",
        );
    }

    /// Regen must unlock an extra cast when the pool would otherwise bottom
    /// out. Cheap 2-mana spell, 2-mana pool: without regen → 1 cast total;
    /// with regen (+1/round) → casts on rounds 0, 2, 4 → 3 casts over 5
    /// rounds. Pins the lower-bound regen model.
    #[test]
    fn horizon_regen_unlocks_extra_casts() {
        let spell = mana_spell_def("zap", 1, DiceExpr::new(1, 4, 0), 2);
        let ev = spell
            .effect
            .calc(&ZERO, spell.engine.power())
            .unwrap()
            .expected();
        let content = content_with(vec![spell.clone()]);
        let abilities = Abilities(vec![spell.id.clone()]);

        // Start pool = cost exactly; max = 10 so regen isn't capped at cost.
        let h = estimate_damage_horizon(
            &ZERO,
            &abilities,
            &content,
            1,
            Some((2, 10)),
            None,
            None,
            20,
            5,
        );
        // Round 0: pool=2 → cast, pool=0.
        // Round 1: +1 → pool=1, can't cast.
        // Round 2: +1 → pool=2, cast, pool=0.
        // Round 3: +1 → pool=1, can't.
        // Round 4: +1 → pool=2, cast.
        let cast_sum = h.iter().filter(|&&d| d > 0.0).count();
        assert_eq!(
            cast_sum, 3,
            "regen should allow 3 casts in 5 rounds, horizon={:?}",
            h
        );
        for i in [0, 2, 4] {
            assert!((h[i] - ev).abs() < 0.01, "round {i}: {}", h[i]);
        }
        for i in [1, 3] {
            assert_eq!(h[i], 0.0, "round {i} should have no cast, got {}", h[i]);
        }
    }
}
