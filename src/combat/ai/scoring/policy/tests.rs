//! Property tests for `combat::ai::policy` — verify that policy functions
//! satisfy key invariants (monotonicity, non-negativity, range bounds).
//!
//! After step 4.12 `compute_score_core` is gone; these tests shift from
//! bit-identical parity tests to **invariant** tests:
//! 1. **Monotonicity**: higher raw damage → higher value (for same target HP).
//! 2. **Non-negativity**: value ≥ 0 for any non-negative inputs.
//! 3. **Range**: damage value ≤ raw (policy never amplifies beyond raw).
//! 4. **Formula round-trips**: verify known-good formula derivations for
//!    `damage::value`, `friendly_fire::penalty`.

use crate::combat::ai::scoring::policy;
use crate::combat::ai::world::snapshot::UnitSnapshot;
use crate::content::abilities::{AbilityDef, CasterContext, EffectCalcExt, EffectDef};
use crate::content::content_view::ContentView;
use crate::core::DiceExpr;
use crate::game::components::Team;
use crate::game::hex::hex_from_offset;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Compute the policy score for `(def, target)` directly via policy functions.
fn via_policy(
    def: &AbilityDef,
    target: &UnitSnapshot,
    ctx: &CasterContext,
    content: &ContentView,
    danger_at_target: f32,
) -> f32 {
    let Some(calc) = def.effect.calc(ctx) else {
        return if matches!(def.effect, EffectDef::GrantMovement { .. }) {
            0.0
        } else {
            policy::status::value(def, target, content)
        };
    };

    let expected = calc.expected();

    let dmg_score = if calc.is_heal {
        let missing = (target.max_hp - target.hp) as f32;
        if missing <= 0.0 {
            return 0.0;
        }
        let effective = expected.min(missing);
        let horizon_sum: f32 = target.damage_horizon.iter().sum::<f32>().max(target.threat);
        policy::heal::value(effective, target.max_hp, target.hp, danger_at_target, horizon_sum)
    } else {
        let mitigation = if calc.pierces_armor {
            0.0
        } else {
            (target.armor + target.armor_bonus) as f32
        };
        let raw = (expected - mitigation + target.damage_taken_bonus as f32).max(0.0);
        let progress = (raw / target.hp.max(1) as f32).min(1.0);
        policy::damage::value(raw, progress)
    };

    dmg_score + policy::status::value(def, target, content)
}

// ── Scenario-based invariant tests ────────────────────────────────────────────

/// Extract all `(ability_def, target_snapshot, caster_ctx)` triples from an
/// `ActorTickEvent` JSONL line.
fn extract_cast_triples_from_line(
    line: &str,
    content: &ContentView,
) -> Vec<(AbilityDef, UnitSnapshot, CasterContext)> {
    use crate::combat::ai::log::ActorTickEvent;
    use crate::combat::ai::plan::types::PlanStep;

    let Ok(event) = serde_json::from_str::<ActorTickEvent>(line) else {
        return vec![];
    };

    let Some(actor_entity) = bevy::prelude::Entity::try_from_bits(event.actor_id) else {
        return vec![];
    };
    let Some(actor_snap) = event.snapshot.unit(actor_entity).cloned() else {
        return vec![];
    };
    let caster_ctx = actor_snap.caster_ctx.clone();

    let mut triples = Vec::new();
    for plan in &event.plans {
        for step in &plan.steps {
            let PlanStep::Cast { ability, target, .. } = step else { continue };
            let Some(def) = content.abilities.get(ability).cloned() else { continue };
            let Some(target_snap) = event.snapshot.unit(*target).cloned() else { continue };
            triples.push((def, target_snap, caster_ctx.clone()));
        }
    }
    triples
}

/// Parse all JSONL files from `tests/ai_scenarios/snapshots/` and collect
/// `(ability_def, target_snapshot, caster_ctx)` triples.
fn collect_scenario_triples(content: &ContentView) -> Vec<(AbilityDef, UnitSnapshot, CasterContext)> {
    use std::io::BufRead;
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let snapshots_dir = manifest.join("tests/ai_scenarios/snapshots");

    let mut all_triples = Vec::new();

    let Ok(groups) = std::fs::read_dir(&snapshots_dir) else { return all_triples; };
    for entry in groups.flatten() {
        let group_dir = entry.path();
        if !group_dir.is_dir() { continue; }
        let Ok(files) = std::fs::read_dir(&group_dir) else { continue; };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            let Ok(file) = std::fs::File::open(&p) else { continue; };
            let reader = std::io::BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                if line.trim().is_empty() { continue; }
                let triples = extract_cast_triples_from_line(&line, content);
                all_triples.extend(triples);
            }
        }
    }
    all_triples
}

/// Verify that `via_policy` gives non-negative, finite values for all triples
/// from scenario fixtures.
#[test]
fn policy_non_negative_for_all_scenario_fixtures() {
    let content = ContentView::load_global_for_tests();
    let triples = collect_scenario_triples(&content);

    let n = triples.len();
    assert!(n > 0, "no Cast triples found in ai_scenarios fixtures — check fixture paths");

    for (def, target, ctx) in &triples {
        let score = via_policy(def, target, ctx, &content, 0.0);
        assert!(
            score.is_finite() && score >= 0.0,
            "policy score must be non-negative and finite for ability={:?}: got {score}",
            def.id,
        );
    }
}

// ── Random-input invariant tests ──────────────────────────────────────────────

/// Minimal deterministic LCG PRNG — avoids any external dependency.
struct Lcg(u64);
impl Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) ^ (self.0 >> 17)) as u32
    }
    fn next_f32(&mut self) -> f32 {
        (self.next_u32() as f64 / u32::MAX as f64) as f32
    }
    fn next_range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next_u32() % ((hi - lo + 1) as u32)) as i32
    }
}

fn random_target(rng: &mut Lcg) -> UnitSnapshot {
    use crate::combat::ai::config::role::AxisProfile;
    use crate::combat::ai::world::tags::AiTags;
    let hp = rng.next_range(1, 100);
    let max_hp = hp + rng.next_range(0, 50);
    let threat = rng.next_range(1, 20) as f32;
    let horizon_len = rng.next_range(0, 5) as usize;
    let damage_horizon = (0..horizon_len).map(|_| rng.next_range(1, 15) as f32).collect();
    UnitSnapshot {
        entity: bevy::prelude::Entity::from_raw_u32(rng.next_u32()).unwrap_or(
            bevy::prelude::Entity::from_raw_u32(1).expect("always valid")
        ),
        team: if rng.next_u32().is_multiple_of(2) { Team::Player } else { Team::Enemy },
        role: AxisProfile { tank: 0.5, melee: 0.5, ..Default::default() },
        pos: hex_from_offset(rng.next_range(0, 5), rng.next_range(0, 5)),
        hp,
        max_hp,
        armor: rng.next_range(0, 5),
        armor_bonus: rng.next_range(-2, 2),
        damage_taken_bonus: rng.next_range(-2, 4),
        action_points: 1,
        max_ap: 1,
        movement_points: 3,
        base_speed: 3,
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
        damage_horizon,
        ai_tuning_override: None,
    }
}

fn random_caster_ctx(rng: &mut Lcg) -> CasterContext {
    const SIDES: [u32; 4] = [4, 6, 8, 10];
    let has_weapon = rng.next_u32().is_multiple_of(2);
    CasterContext {
        str_mod: rng.next_range(-2, 5),
        int_mod: rng.next_range(-2, 5),
        spell_power: rng.next_range(0, 8),
        weapon_dice: if has_weapon {
            Some(DiceExpr {
                count: rng.next_range(1, 3) as u32,
                sides: SIDES[rng.next_u32() as usize % 4],
                bonus: rng.next_range(-1, 3),
            })
        } else {
            None
        },
    }
}

/// 1000 random triples: policy score is non-negative and finite.
#[test]
fn policy_non_negative_for_random_inputs() {
    let content = ContentView::load_global_for_tests();

    let abilities: Vec<&AbilityDef> = content
        .abilities
        .values()
        .filter(|def| !matches!(def.effect, EffectDef::GrantMovement { .. }))
        .collect();

    assert!(!abilities.is_empty(), "no abilities found — check ContentView::load_global_for_tests");

    let mut rng = Lcg(42);
    let n = 1000usize;

    for i in 0..n {
        let ability_idx = rng.next_u32() as usize % abilities.len();
        let def = abilities[ability_idx];
        let target = random_target(&mut rng);
        let ctx = random_caster_ctx(&mut rng);
        let danger = rng.next_f32() * 50.0;

        let score = via_policy(def, &target, &ctx, &content, danger);
        assert!(
            score.is_finite() && score >= 0.0,
            "random triple {i}: policy score must be non-negative and finite for ability={:?}: got {score}",
            def.id,
        );
    }
}

/// `damage::value` monotone in raw for fixed target HP.
#[test]
fn damage_value_monotone_in_raw() {
    let target_hp = 20;
    let raws = [0.0f32, 1.0, 5.0, 10.0, 20.0, 40.0];
    let mut prev = f32::NEG_INFINITY;
    for &raw in &raws {
        let progress = (raw / target_hp.max(1) as f32).min(1.0);
        let v = policy::damage::value(raw, progress);
        assert!(v >= prev, "damage::value not monotone at raw={raw}: {v} < {prev}");
        prev = v;
    }
}

/// `damage::value` is non-negative for any non-negative raw / progress.
#[test]
fn damage_value_non_negative() {
    let cases = [(0.0f32, 0.0f32), (0.0, 1.0), (10.0, 0.0), (10.0, 0.5), (10.0, 1.0)];
    for (raw, progress) in cases {
        let v = policy::damage::value(raw, progress);
        assert!(v >= 0.0, "damage::value({raw}, {progress}) = {v} < 0");
    }
}

// ── Friendly-fire round-trip ──────────────────────────────────────────────────

/// Verify that `policy::friendly_fire::penalty(raw, max_hp)` matches the
/// formula that `factors::offensive::friendly_fire_penalty` used to inline.
#[test]
fn friendly_fire_penalty_formula_matches_old_inline() {
    // Old formula: raw × (1 + raw / max_hp)
    let cases = [(0.0f32, 100i32), (5.0, 100), (100.0, 100), (10.0, 50), (0.1, 200)];
    for (raw, max_hp) in cases {
        let expected = raw * (1.0 + raw / max_hp.max(1) as f32);
        let actual = policy::friendly_fire::penalty(raw, max_hp);
        assert!(
            (expected - actual).abs() < 1e-6,
            "friendly_fire::penalty({raw}, {max_hp}): expected={expected} actual={actual}"
        );
    }
}

/// `damage::value` round-trip against the old inline formula.
#[test]
fn damage_value_formula_matches_old_inline() {
    // Old formula: raw × (0.5 + 0.5 × progress)
    let cases = [(0.0f32, 0.0f32), (10.0, 0.5), (10.0, 1.0), (5.0, 0.25), (20.0, 0.8)];
    for (raw, progress) in cases {
        let expected = raw * (0.5 + 0.5 * progress);
        let actual = policy::damage::value(raw, progress);
        assert!(
            (expected - actual).abs() < 1e-6,
            "damage::value({raw}, {progress}): expected={expected} actual={actual}"
        );
    }
}
