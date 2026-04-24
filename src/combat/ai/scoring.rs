use crate::content::content_view::ContentView;
use crate::combat::ai::snapshot::UnitSnapshot;
use crate::content::abilities::{AbilityDef, CasterContext, TargetType};
use crate::content::statuses::StatusDef;
use crate::core::ResourceKind;
use crate::game::components::Abilities;

/// True if the ability applies any status that skips the target's turn
/// (stun, paralyse, sleep…). Single source of truth for "is this CC?".
pub fn applies_cc(def: &AbilityDef, content: &ContentView) -> bool {
    def.statuses
        .iter()
        .any(|sa| content.statuses.get(&sa.status).is_some_and(|sd| sd.skips_turn))
}

/// Sum of projected damage denied to `target` by `def`'s stun-class statuses.
/// Reads `target.damage_horizon` via `horizon_window_sum`; the `cc` offensive
/// factor and the `scarcity` swing-justification branch both read this so
/// their stun formulas cannot drift apart. Non-stun effects (vulnerability,
/// armor shred, DoT) are intentionally *not* included — each caller folds
/// those in with its own weighting.
pub fn stun_denial_value(def: &AbilityDef, target: &UnitSnapshot, content: &ContentView) -> f32 {
    status_applications(def, content)
        .filter(|(sd, _)| sd.skips_turn)
        .map(|(_, d)| horizon_window_sum(target, d))
        .sum()
}

/// Yield `(StatusDef, duration_rounds)` pairs for every status application
/// attached to `def`, resolving the id against `content`. Silently skips
/// applications whose id isn't in the content registry.
///
/// Single source of truth for the iterate-def-statuses-and-lookup-StatusDef
/// pattern used by both `status_score` (full HP-equivalent contribution)
/// and `factors::offensive::status_cc_value` (CC-denial subset). Each
/// caller keeps its own value-accumulating closure — semantics differ
/// (`.abs()` on damage_taken_bonus vs. positive-only gate), so only the
/// iteration shape is shared.
pub fn status_applications<'a, 'c: 'a>(
    def: &'a AbilityDef,
    content: &'c ContentView,
) -> impl Iterator<Item = (&'c StatusDef, f32)> + 'a {
    def.statuses.iter().filter_map(move |sa| {
        content
            .statuses
            .get(&sa.status)
            .map(|sd| (sd, sa.duration_rounds as f32))
    })
}


/// Sum of projected damage over the first `duration` rounds of the
/// target's damage horizon. Rounds-up fractional durations (a
/// stun-for-2.5-rounds touches 3 rounds). Falls back to `target.threat
/// × duration` when the horizon is empty (legacy logs, uninitialised
/// fixtures) so CC / stun / blocks-mana formulas stay continuous with
/// the pre-#6 behaviour.
///
/// Caller semantics: "how much of the target's projected damage would
/// be denied if we removed their actions for `duration` rounds". Used
/// by `status_score` skips_turn / blocks_mana branches and the
/// CC-factor valuation in `factors::offensive`.
pub fn horizon_window_sum(target: &UnitSnapshot, duration: f32) -> f32 {
    if target.damage_horizon.is_empty() {
        return target.threat * duration.max(0.0);
    }
    let n = (duration.ceil() as usize).min(target.damage_horizon.len());
    target.damage_horizon.iter().take(n).sum()
}

/// Average projected damage per round across the full horizon.
/// Falls back to `target.threat` when the horizon is empty.
///
/// Used where the call site needs a single per-round scalar (scarcity
/// bonus, intent CC weight) and wants the DPR-equivalent rather than a
/// duration-specific sum.
pub fn horizon_avg(target: &UnitSnapshot) -> f32 {
    if target.damage_horizon.is_empty() {
        return target.threat;
    }
    let n = target.damage_horizon.len() as f32;
    target.damage_horizon.iter().sum::<f32>() / n.max(1.0)
}

/// Best single-target expected damage from one ability (before armor).
/// Used to value stuns/kills: controlling a high-damage target is worth more.
/// Does NOT capture AoE, healing, or utility — it's a damage-only estimate.
pub fn estimate_st_damage(ctx: &CasterContext, abilities: &Abilities, content: &ContentView) -> f32 {
    abilities
        .0
        .iter()
        .filter_map(|id| content.abilities.get(id))
        // SingleEnemy = direct attack; Ground = area-landed attack at a cell.
        // Both produce per-cast damage; the `effect.calc` filter below
        // drops non-damaging variants (teleport/spawn Ground spells), so
        // the blanket include here is safe.
        .filter(|def| matches!(def.target_type, TargetType::SingleEnemy | TargetType::Ground))
        .filter_map(|def| def.effect.calc(ctx))
        .map(|calc| calc.expected().max(0.0))
        .fold(0.0f32, f32::max)
}

/// Project expected single-target damage output over the next `rounds`
/// rounds, accounting for AP budget and resource (mana/rage/energy/HP)
/// depletion. Returns a `Vec<f32>` of length `rounds`; index `i` is the
/// expected damage dealt on future round `i+1`.
///
/// **Greedy** over damage-per-AP: each round the actor spends its AP
/// budget starting with the best-per-AP castable ability.
/// **Regeneration**: at the start of each subsequent round we add +1 to
/// mana / rage / energy pools, capped at their max. Matches the live
/// model — `turn_start_system` restores +1 mana + +1 energy per own-turn,
/// and rage gains ≥ +1/round from the steady damage flow in active
/// combat. Conservative lower bound; real sustain may be higher (AoE
/// damage, multi-hit rage triggers).
///
/// Sustained fighters (free melee) produce a flat horizon; burst casters
/// front-load and drop to regen-limited cadence once the pool dries.
///
/// Used by the CC / heal / stun consumers that previously read
/// `UnitSnapshot.threat` (peak single-cast damage); peak over-weighted
/// resource-limited burst casters. See `UnitSnapshot.damage_horizon`
/// for storage and consumers.
#[allow(clippy::too_many_arguments)]
pub fn estimate_damage_horizon(
    caster: &CasterContext,
    abilities: &Abilities,
    content: &ContentView,
    max_ap_per_round: i32,
    mana: Option<(i32, i32)>,    // (current, max)
    rage: Option<(i32, i32)>,
    energy: Option<(i32, i32)>,
    hp: i32,
    rounds: u32,
) -> Vec<f32> {
    // Precompute: for each damaging SingleEnemy/AoE ability, the triple
    // (expected_damage, cost_ap, cost_per_resource). Abilities that can't
    // deal damage or cost 0 AP are filtered — 0-AP would create an
    // infinite inner loop.
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
        .filter(|def| matches!(
            def.target_type,
            TargetType::SingleEnemy | TargetType::Ground
        ))
        .filter(|def| def.cost_ap > 0)
        .filter_map(|def| {
            let calc = def.effect.calc(caster)?;
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
        // Start-of-round regen: +1 to each tracked pool, capped at max.
        // Round 0 uses the unit's current pools as-is (today's turn already
        // regen'd by `turn_start_system`); subsequent rounds model the own-
        // turn restoration + in-combat rage trickle.
        if round > 0 {
            if max_mana > 0 { pool_mana = (pool_mana + 1).min(max_mana); }
            if max_rage > 0 { pool_rage = (pool_rage + 1).min(max_rage); }
            if max_energy > 0 { pool_energy = (pool_energy + 1).min(max_energy); }
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

/// Aggregate HP-equivalent value of the statuses `def` applies to `target`.
/// Crate-visible — consumed by `outcome::compute_score_core` (the central
/// HP-equivalent formula).
pub(crate) fn status_score(
    def: &AbilityDef,
    target: &UnitSnapshot,
    content: &ContentView,
) -> f32 {
    // HP-equivalent scoring — counts BOTH signs of damage_taken_bonus /
    // armor_bonus (.abs()) because a buff on an ally and a debuff on an
    // enemy are both "value to the caster". For CC-denial scoring see
    // `factors::offensive::status_cc_value`.
    status_applications(def, content)
        .map(|(sd, d)| {
            let mut total = 0.0f32;
            // Stun: deny target's projected damage over `d` rounds.
            // `horizon_window_sum` reads damage_horizon (DPR-correct);
            // falls back to `threat × d` on empty horizon (old logs).
            if sd.skips_turn {
                total += horizon_window_sum(target, d);
            }
            // Vulnerability: extra damage taken per hit for d rounds.
            if sd.damage_taken_bonus != 0 {
                total += sd.damage_taken_bonus.abs() as f32 * d;
            }
            // Armor delta: negative = shred on enemy, positive = buff on ally.
            if sd.armor_bonus != 0 {
                total += sd.armor_bonus.abs() as f32 * d;
            }
            // DoT: expected tick damage × duration.
            if let Some(ref dice) = sd.dot_dice {
                total += dice.expected() * d;
            }
            // %HP DoT (e.g. exhaustion).
            if sd.hp_percent_dot > 0 {
                let tick_dmg = (target.max_hp as f32 * sd.hp_percent_dot as f32 / 100.0).ceil();
                total += tick_dmg * d;
            }
            // Silence (blocks mana abilities): partial stun — target can
            // still basic-attack, so worth ~half the projected denial.
            if sd.blocks_mana_abilities {
                total += 0.5 * horizon_window_sum(target, d);
            }
            // Speed penalty: reduces tactical options.
            if sd.speed_bonus < 0 {
                total += (-sd.speed_bonus) as f32 * d;
            }
            total
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, ResourceCost, StatusApplication, StatusOn,
    };
    use crate::core::{AbilityId, DiceExpr, StatusId};
    use std::collections::HashMap;

    fn content_with(abs: Vec<AbilityDef>) -> ContentView {
        let mut map: HashMap<AbilityId, AbilityDef> = HashMap::new();
        for d in abs {
            map.insert(d.id.clone(), d);
        }
        ContentView {
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
            ..ContentView::default()
        }
    }

    fn weapon_attack_def(id: &str, cost_ap: i32, dice: DiceExpr) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice },
            costs: Vec::new(),
            cost_ap,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    fn mana_spell_def(id: &str, cost_ap: i32, dice: DiceExpr, mana_cost: i32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::Damage { dice },
            costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: mana_cost }],
            cost_ap,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    const ZERO: CasterContext = CasterContext {
        str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None,
    };

    /// Free-attack warrior: one ability, no cost, cost_ap=1. Horizon over 5
    /// rounds with max_ap=1 should be a flat plateau of `expected` per round.
    #[test]
    fn horizon_free_attack_plateaus() {
        let melee = weapon_attack_def("melee", 1, DiceExpr::new(1, 6, 0));
        let ev = melee.effect.calc(&ZERO).unwrap().expected();
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
        let ev = spell.effect.calc(&ZERO).unwrap().expected();
        let content = content_with(vec![spell.clone()]);
        let abilities = Abilities(vec![spell.id.clone()]);

        let h = estimate_damage_horizon(&ZERO, &abilities, &content, 1, Some((10, 10)), None, None, 20, 5);
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
        let spell_ev = spell.effect.calc(&ZERO).unwrap().expected();
        let melee_ev = melee.effect.calc(&ZERO).unwrap().expected();
        let content = content_with(vec![spell.clone(), melee.clone()]);
        let abilities = Abilities(vec![spell.id.clone(), melee.id.clone()]);

        let h = estimate_damage_horizon(&ZERO, &abilities, &content, 1, Some((10, 10)), None, None, 20, 5);
        assert!((h[0] - spell_ev).abs() < 0.01);
        assert!((h[1] - spell_ev).abs() < 0.01);
        assert!((h[2] - melee_ev).abs() < 0.01, "fell back to melee: {}", h[2]);
        assert!((h[3] - melee_ev).abs() < 0.01);
        assert!((h[4] - melee_ev).abs() < 0.01);
    }

    /// Unit with no damaging abilities (support / healer only) yields all
    /// zeros — useful guard for later call sites that sum over the horizon
    /// and don't want division-by-zero or Option handling.
    ///
    /// Also pins the non-damaging case: `estimate_damage_horizon` never
    /// returns None, always a Vec of correct length.
    #[test]
    fn horizon_empty_for_non_damaging_actor() {
        let ping = AbilityDef {
            id: AbilityId::from("inspire"),
            name: "inspire".into(),
            target_type: TargetType::SingleAlly,   // not SingleEnemy
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
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
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
        use crate::combat::ai::role::AxisProfile;
        use crate::combat::ai::snapshot::AiTags;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;

        fn make_target(
            id: u32, threat: f32, horizon: Vec<f32>,
        ) -> UnitSnapshot {
            UnitSnapshot {
                entity: bevy::prelude::Entity::from_raw_u32(id).unwrap(),
                team: Team::Player,
                role: AxisProfile { tank: 0.5, melee: 0.5, ..Default::default() },
                pos: hex_from_offset(0, 0),
                hp: 20,
                max_hp: 20,
                armor: 0,
                armor_bonus: 0,
                damage_taken_bonus: 0,
                action_points: 1,
                max_ap: 1,
                movement_points: 3,
                speed: 3,
                mana: None,
                rage: None,
                energy: None,
                abilities: Vec::new(),
                threat,
                tags: AiTags::empty(),
                max_attack_range: 1,
                summoner: None,
                reactions_left: 0,
                aoo_expected_damage: None,
                statuses: Vec::new(),
                caster_ctx: Default::default(),
                crit_fail_effect: Default::default(),
                damage_horizon: horizon,
                ai_tuning_override: None,
            }
        }

        // Both targets have the SAME peak (`threat = 10`). The sustained
        // fighter keeps hitting every round; the burst mage fired twice
        // and ran out of mana.
        let sustained = make_target(1, 10.0, vec![10.0, 10.0, 10.0, 10.0, 10.0]);
        let burst = make_target(2, 10.0, vec![10.0, 10.0, 0.0, 0.0, 0.0]);

        // horizon_window_sum over a 3-round stun:
        let sustained_stun = horizon_window_sum(&sustained, 3.0);
        let burst_stun = horizon_window_sum(&burst, 3.0);
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
        let ev = spell.effect.calc(&ZERO).unwrap().expected();
        let content = content_with(vec![spell.clone()]);
        let abilities = Abilities(vec![spell.id.clone()]);

        // Start pool = cost exactly; max = 10 so regen isn't capped at cost.
        let h = estimate_damage_horizon(
            &ZERO, &abilities, &content, 1, Some((2, 10)), None, None, 20, 5,
        );
        // Round 0: pool=2 → cast, pool=0.
        // Round 1: +1 → pool=1, can't cast.
        // Round 2: +1 → pool=2, cast, pool=0.
        // Round 3: +1 → pool=1, can't.
        // Round 4: +1 → pool=2, cast.
        let cast_sum = h.iter().filter(|&&d| d > 0.0).count();
        assert_eq!(cast_sum, 3, "regen should allow 3 casts in 5 rounds, horizon={:?}", h);
        for i in [0, 2, 4] {
            assert!((h[i] - ev).abs() < 0.01, "round {i}: {}", h[i]);
        }
        for i in [1, 3] {
            assert_eq!(h[i], 0.0, "round {i} should have no cast, got {}", h[i]);
        }
    }
}
