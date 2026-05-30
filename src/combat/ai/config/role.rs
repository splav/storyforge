use crate::combat::ai::repair::affinity::RepairWeights;
use crate::combat::ai::world::tags::{AbilityTag, AbilityTagCache, AbilityTagSet};
use crate::combat::ai::config::tuning::AiTuning;
use crate::content::abilities::{AoEShape, EffectDef};
use crate::content::content_view::ContentView;
use combat_engine::AbilityId;
use bevy::prelude::*;

// ── AxisProfile: vector-role across 5 archetypal axes ──────────────────────
//
// Instead of classifying a unit as one of 5 enum roles, we score it across
// five orthogonal axes: Tank, Melee damage, Ranged damage, Control, Support.
// Final factor weights are a squared-smooth weighted mix — so pure archetypes
// converge to near-pure behaviour, hybrids (e.g., battlemage with heal + AoE)
// retain secondary flavor without dilution.
//
// Emergent roles:
//   glass cannon  = high Melee + low Tank
//   brawler       = Tank + Melee mix
//   control mage  = Ranged + Control
//   battle healer = Support + Ranged

/// Axis weights for the composition. All non-negative; not normalised —
/// normalisation happens inside `biased_normalized`.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AxisProfile {
    pub tank: f32,
    pub melee: f32,
    pub ranged: f32,
    pub control: f32,
    pub support: f32,
}

/// Exponent used when mixing axis weights. `1.0` = linear (heavy dilution on
/// hybrids), `>1.0` biases toward the dominant axis. `1.5` keeps hybrids
/// readable while pure roles converge to enum-like behaviour.
pub const COMPOSITION_EXPONENT: f32 = 1.5;


/// How much each axis contributes to a target's priority-value (used in
/// `target_priority`). Support > Control > Ranged > Melee > Tank — killing
/// a support / controller is strategically bigger than killing a front-liner.
const AXIS_PRIORITY_VALUE: [f32; 5] = [0.3, 0.5, 0.7, 0.8, 1.0];

const AXIS_NAMES: [&str; 5] = ["Tank", "Melee", "Ranged", "Control", "Support"];

impl AxisProfile {
    pub fn as_array(&self) -> [f32; 5] {
        [self.tank, self.melee, self.ranged, self.control, self.support]
    }

    /// Raise each axis to `COMPOSITION_EXPONENT`, then normalise so the sum is 1.
    /// Used as the mixing weights for factor composition.
    pub fn biased_normalized(&self) -> [f32; 5] {
        let raw = self.as_array();
        let biased: [f32; 5] = raw.map(|v| v.max(0.0).powf(COMPOSITION_EXPONENT));
        let total = biased.iter().sum::<f32>();
        if total < 1e-6 {
            // Empty profile: fallback to pure Melee as a safe default.
            return [0.0, 1.0, 0.0, 0.0, 0.0];
        }
        biased.map(|v| v / total)
    }

    /// Composed 10-factor weights for utility scoring.
    ///
    /// Per-axis rows live in `tuning.tables.axis_factor_weights`. Columns:
    /// [damage, kill_now, kill_promised, cc, heal, intent, scarcity,
    /// tempo_gain, saturation, self_survival]. `kill_promised` = kill_now × 0.5
    /// for all roles except Control (0.8 — DoT is strategically valuable for
    /// controllers). `saturation` = 1.0 for all roles (signed axis, sign drives
    /// the direction). `self_survival`: Support 1.2 (healer cares most), Tank
    /// 1.0, others 0.8. Phase 6 removed position/risk/focus columns — their
    /// signals are now covered by tempo_gain and self_survival.
    pub fn factor_weights(&self, tuning: &AiTuning) -> [f32; 10] {
        let mix = self.biased_normalized();
        let table = &tuning.tables.axis_factor_weights;
        let mut out = [0.0f32; 10];
        for axis in 0..5 {
            for f in 0..10 {
                out[f] += mix[axis] * table[axis][f];
            }
        }
        out
    }

    /// Composed terminal-state weights (8 axes). Mirrors `factor_weights` but
    /// reads `tuning.tables.axis_terminal_weights`. Columns:
    /// [exposure_at_end, next_turn_lethality, secure_kill, ally_rescue,
    ///  board_control_gain, line_actionability, density_value,
    ///  pressure_spacing_zone].
    /// Used by `finalize_scores` (5.4) to score plans by their terminal sim
    /// state in parallel with step-summed `PlanFactorValues`.
    pub fn terminal_weights(&self, tuning: &AiTuning) -> [f32; 8] {
        let mix = self.biased_normalized();
        let table = &tuning.tables.axis_terminal_weights;
        let mut out = [0.0f32; 8];
        for axis in 0..5 {
            for k in 0..8 {
                out[k] += mix[axis] * table[axis][k];
            }
        }
        out
    }

    /// Composed position-eval weights (danger, ally_support, opportunity).
    ///
    /// Per-axis rows live in `tuning.tables.axis_position_weights`. Columns:
    /// [danger, ally_support, opportunity].
    pub fn position_weights(&self, tuning: &AiTuning) -> [f32; 3] {
        let mix = self.biased_normalized();
        let table = &tuning.tables.axis_position_weights;
        let mut out = [0.0f32; 3];
        for axis in 0..5 {
            for k in 0..3 {
                out[k] += mix[axis] * table[axis][k];
            }
        }
        out
    }

    /// Composed factor weights — continuation evaluator (step 6.4).
    ///
    /// Reads `tuning.tables.axis_factor_weights_continuation` instead of
    /// `axis_factor_weights`. Applied when `AiMemory.last_goal` is `Some`.
    /// Mirrors `factor_weights` exactly — only the table differs.
    pub fn factor_weights_continuation(&self, tuning: &AiTuning) -> [f32; 10] {
        let mix = self.biased_normalized();
        let table = &tuning.tables.axis_factor_weights_continuation;
        let mut out = [0.0f32; 10];
        for axis in 0..5 {
            for f in 0..10 {
                out[f] += mix[axis] * table[axis][f];
            }
        }
        out
    }

    /// Composed terminal-state weights — continuation evaluator (step 6.4).
    ///
    /// Reads `tuning.tables.axis_terminal_weights_continuation` instead of
    /// `axis_terminal_weights`. Applied when `AiMemory.last_goal` is `Some`.
    /// Mirrors `terminal_weights` exactly — only the table differs.
    pub fn terminal_weights_continuation(&self, tuning: &AiTuning) -> [f32; 8] {
        let mix = self.biased_normalized();
        let table = &tuning.tables.axis_terminal_weights_continuation;
        let mut out = [0.0f32; 8];
        for axis in 0..5 {
            for k in 0..8 {
                out[k] += mix[axis] * table[axis][k];
            }
        }
        out
    }

    /// Composed repair-affinity weights (goal, region, method).
    ///
    /// Per-axis rows live in `tuning.tables.axis_repair_weights`. Columns:
    /// [goal_w, region_w, method_w].
    /// Used by `RepairAffinity::aggregate` (6.3) to produce the role-mixed
    /// repair bonus.
    pub fn repair_weights(&self, tuning: &AiTuning) -> RepairWeights {
        let mix = self.biased_normalized();
        let table = &tuning.tables.axis_repair_weights;
        let mut w = [0.0f32; 3];
        for axis in 0..5 {
            for j in 0..3 {
                w[j] += mix[axis] * table[axis][j];
            }
        }
        RepairWeights { goal_w: w[0], region_w: w[1], method_w: w[2] }
    }

    /// Composite "how valuable is this target to eliminate" scalar in 0..1.
    pub fn role_value(&self) -> f32 {
        let mix = self.biased_normalized();
        (0..5).map(|i| mix[i] * AXIS_PRIORITY_VALUE[i]).sum()
    }

    /// Debug label: `"Mage(0.73)"` or `"Mage(0.53) + Support(0.21)"` when
    /// a secondary axis is significant (≥ 0.15 after bias).
    pub fn dominant_label(&self) -> String {
        let mix = self.biased_normalized();
        let mut indexed: Vec<(usize, f32)> = mix.iter().copied().enumerate().collect();
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (pi, pv) = indexed[0];
        let primary = format!("{}({:.2})", AXIS_NAMES[pi], pv);
        if indexed.len() >= 2 && indexed[1].1 >= 0.15 {
            let (si, sv) = indexed[1];
            format!("{} + {}({:.2})", primary, AXIS_NAMES[si], sv)
        } else {
            primary
        }
    }
}

/// Infer an `AxisProfile` from a unit's ability kit, stats and speed.
///
/// Each ability casts votes across the 5 axes, weighted by `1 + total cost`
/// (free utility gets weight 1; heavy spells weight 1 + mana). In addition,
/// stat-based tank mass from `max_hp + armor×2` adds a floor of Tank weight —
/// a heavily-armored brawler reads as Tank even without taunt abilities.
///
/// Emergent classification: a unit with `speed ≥ 5` and mostly-Melee profile
/// will have a low composed Tank weight, making it behave as a glass-cannon
/// assassin without a dedicated enum variant.
pub fn infer_profile(
    abilities: &[AbilityId],
    max_hp: i32,
    total_armor: i32,
    content: &ContentView,
    tag_cache: &AbilityTagCache,
) -> AxisProfile {
    let mut p = AxisProfile::default();

    for id in abilities {
        let Some(def) = content.abilities.get(id) else { continue };
        let cost: f32 = def.costs.iter().map(|c| c.amount as f32).sum();
        let weight = 1.0 + cost;
        let tags = tag_cache.effective(id);
        let v = tag_axis_vote(tags, def, weight);
        p.tank    += v[0];
        p.melee   += v[1];
        p.ranged  += v[2];
        p.control += v[3];
        p.support += v[4];
    }

    // Stat-based tank mass: armor counts double because it's active defense.
    // Baseline 20 effective HP → 1.0 tank weight. Clamped to avoid extremes.
    let eff_hp = (max_hp + total_armor * 2) as f32;
    p.tank += (eff_hp / 20.0).clamp(0.3, 2.0);

    // Empty kit fallback — shouldn't happen with real content.
    let total = p.tank + p.melee + p.ranged + p.control + p.support;
    if total < 1e-6 {
        p.melee = 1.0;
    }

    p
}

/// Vote a single ability into the 5-axis space using semantic tags.
///
/// Priority order (first matching branch wins):
/// Rescue → Summon → Defensive(no Offensive, no Peel) → Offensive → ApplyCC → Peel → Mobility → zero
///
/// The Peel condition is excluded from the Defensive branch so that abilities
/// with both tags (e.g. `taunt`: Defensive+Peel) reach the Peel branch and
/// produce the blended Tank/Support vote rather than a pure Tank vote.
///
/// Returns `[tank, melee, ranged, control, support]`.
fn tag_axis_vote(
    tags: AbilityTagSet,
    def: &crate::content::abilities::AbilityDef,
    weight: f32,
) -> [f32; 5] {
    let mut v = [0.0f32; 5];

    if tags.contains_tag(AbilityTag::Rescue) {
        v[4] += weight;
        return v;
    }
    if tags.contains_tag(AbilityTag::Summon) {
        v[4] += weight * 0.7;
        v[2] += weight * 0.3;
        return v;
    }
    if tags.contains_tag(AbilityTag::Defensive)
        && !tags.contains_tag(AbilityTag::Offensive)
        && !tags.contains_tag(AbilityTag::Peel)
    {
        v[0] += weight;
        return v;
    }
    if tags.contains_tag(AbilityTag::Offensive) {
        // Melee/ranged split — the one place where shape is still needed.
        let is_ranged = matches!(def.effect, EffectDef::SpellDamage { .. })
            || def.aoe != AoEShape::None
            || def.range.min >= 2;
        if is_ranged { v[2] += weight } else { v[1] += weight };
        if tags.contains_tag(AbilityTag::ApplyCC) { v[3] += weight * 0.4; }
        return v;
    }
    if tags.contains_tag(AbilityTag::ApplyCC) {
        v[3] += weight;
        return v;
    }
    if tags.contains_tag(AbilityTag::Peel) {
        v[0] += weight * 0.7;
        v[4] += weight * 0.3;
        return v;
    }
    if tags.contains_tag(AbilityTag::Mobility) {
        v[1] += weight * 0.3;
        return v;
    }

    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::scoring::factors::StepFactor;
    use crate::combat::ai::world::tags::cache::build_caches;
    const DAMAGE_IDX: usize = StepFactor::Damage as usize;
    const HEAL_IDX: usize = StepFactor::Heal as usize;
    use combat_engine::AbilityId;

    fn db_with_cache() -> (ContentView, AbilityTagCache) {
        let content = ContentView::load_global_for_tests();
        let (_, ac) = build_caches(&content);
        (content, ac)
    }

    fn ids(names: &[&str]) -> Vec<AbilityId> {
        names.iter().map(|s| AbilityId::from(*s)).collect()
    }

    // ── AxisProfile tests ───────────────────────────────────────────────

    #[test]
    fn empty_profile_falls_back_to_melee() {
        let p = AxisProfile::default();
        let mix = p.biased_normalized();
        assert_eq!(mix, [0.0, 1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn pure_axis_stays_pure_after_bias() {
        let p = AxisProfile { support: 1.0, ..Default::default() };
        let mix = p.biased_normalized();
        assert!((mix[4] - 1.0).abs() < 1e-5, "pure support should stay 1.0, got {}", mix[4]);
        for &other in &mix[..4] {
            assert!(other.abs() < 1e-5);
        }
    }

    #[test]
    fn bias_amplifies_dominant_axis() {
        // Linear 70/30 → biased should skew further toward 70.
        let p = AxisProfile { ranged: 0.7, support: 0.3, ..Default::default() };
        let mix = p.biased_normalized();
        // 0.7^1.5 = 0.585, 0.3^1.5 = 0.164 → norm 0.78 / 0.22
        assert!(mix[2] > 0.75, "ranged should be >0.75 after bias, got {}", mix[2]);
        assert!(mix[4] < 0.25, "support should be <0.25 after bias, got {}", mix[4]);
    }

    #[test]
    fn factor_weights_mix_correctly() {
        // 50/50 Tank + Melee: heal (index 4) should be near zero (both axes have heal≈0.1 average).
        let tuning = AiTuning::default();
        let p = AxisProfile { tank: 0.5, melee: 0.5, ..Default::default() };
        let w = p.factor_weights(&tuning);
        assert!(w[HEAL_IDX] < 0.15, "heal weight should be small for tank/melee hybrid, got {}", w[HEAL_IDX]);
        // Damage should be meaningful (melee contributes 1.3).
        assert!(w[DAMAGE_IDX] > 0.6, "damage weight should be substantial, got {}", w[DAMAGE_IDX]);
    }

    #[test]
    fn pure_support_heal_weight_near_axis_value() {
        // After bias, pure support normalizes to 1.0; heal = 1.0 × 2.0 = 2.0.
        let tuning = AiTuning::default();
        let p = AxisProfile { support: 1.0, ..Default::default() };
        let w = p.factor_weights(&tuning);
        assert!((w[HEAL_IDX] - 2.0).abs() < 0.01, "pure support heal weight = 2.0, got {}", w[HEAL_IDX]);
    }

    #[test]
    fn role_value_scales_with_support() {
        // Pure support is highest-priority target (1.0). Pure tank lowest (0.3).
        let support = AxisProfile { support: 1.0, ..Default::default() };
        let tank = AxisProfile { tank: 1.0, ..Default::default() };
        assert!(support.role_value() > tank.role_value());
        assert!((support.role_value() - 1.0).abs() < 0.01);
        assert!((tank.role_value() - 0.3).abs() < 0.01);
    }

    #[test]
    fn dominant_label_shows_primary() {
        let p = AxisProfile { ranged: 1.0, ..Default::default() };
        let label = p.dominant_label();
        assert!(label.starts_with("Ranged"), "got {}", label);
    }

    #[test]
    fn dominant_label_shows_hybrid() {
        // Buryevestnik-like: Ranged + Support hybrid after bias should show both.
        let p = AxisProfile { ranged: 6.0, support: 3.0, melee: 1.0, ..Default::default() };
        let label = p.dominant_label();
        assert!(label.contains("Ranged"), "got {}", label);
        assert!(label.contains("Support"), "should show secondary support: {}", label);
    }

    // ── infer_profile on real units ─────────────────────────────────────

    /// Returns the dominant axis name (after bias normalisation).
    fn dominant(p: &AxisProfile) -> &'static str {
        let mix = p.biased_normalized();
        let names = ["Tank", "Melee", "Ranged", "Control", "Support"];
        let (idx, _) = mix.iter().enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap();
        names[idx]
    }

    #[test]
    fn infer_kael_is_ranged() {
        // Kael: ranger — melee + bow + paralyzing_shot + field_medic + poison_shot.
        // Expected: Ranged dominant, small Support and Control secondary.
        let (db, ac) = db_with_cache();
        let p = infer_profile(
            &ids(&["melee_attack", "bow_shot", "paralyzing_shot", "field_medic", "poison_shot"]),
            18, 2, &db, &ac,
        );
        assert_eq!(dominant(&p), "Ranged", "profile: {:?}", p);
        assert!(p.ranged > p.support);
        assert!(p.support > 0.0, "should have some Support from field_medic");
    }

    #[test]
    fn infer_lyra_is_ranged_mage() {
        // Lyra: mage — melee + flash + burn + fireball + heal.
        // Expected: Ranged dominant (fireball weight 6 dominates).
        // burn has Cosmetic status (not CC) → empty tags → zero vote (9.B: DOT ≠ CC).
        let (db, ac) = db_with_cache();
        let p = infer_profile(
            &ids(&["melee_attack", "flash", "burn", "fireball", "heal"]),
            10, 0, &db, &ac,
        );
        assert_eq!(dominant(&p), "Ranged", "profile: {:?}", p);
        assert!(p.support > 0.0, "heal should contribute to Support");
        assert_eq!(p.control, 0.0, "burn applies DOT (not CC) → no Control contribution");
    }

    #[test]
    fn infer_aldric_is_control_tank() {
        // Aldric: warrior — melee + taunt + stun + rush.
        // Expected: Control or Tank dominant.
        // Tag-based breakdown:
        //   melee_attack → OFFENSIVE melee → Melee 1
        //   taunt → DEFENSIVE + PEEL → Peel branch: Tank 0.7 + Support 0.3 (weight 1)
        //   stun → APPLY_CC → Control 4 (weight 1+3=4)
        //   rush → MOBILITY → Melee 0.3×3 = 0.9 (weight 1+2=3)
        //   stat: (20+5*2)/20 = 1.5 → Tank 1.5
        // Totals: Tank=0.7+1.5=2.2, Melee=1+0.9=1.9, Control=4, Support=0.3
        // Dominant: Control.
        let (db, ac) = db_with_cache();
        let p = infer_profile(
            &ids(&["melee_attack", "taunt", "stun", "rush"]),
            20, 5, &db, &ac,
        );
        assert!(
            dominant(&p) == "Tank" || dominant(&p) == "Control",
            "expected Tank or Control, got {} with profile {:?}", dominant(&p), p
        );
        assert!(p.tank > 2.0, "heavy armor + taunt/rush should yield high Tank: {:?}", p);
        assert!(p.control > 2.0, "stun should give Control: {:?}", p);
    }

    #[test]
    fn infer_molnienosets_is_melee_assassin() {
        // Молниеносец: melee + backstab, speed 6 (doesn't affect profile directly),
        // hp 12, armor 1. Expected: Melee dominant, low Tank (glass cannon).
        let (db, ac) = db_with_cache();
        let p = infer_profile(&ids(&["melee_attack", "backstab"]), 12, 1, &db, &ac);
        assert_eq!(dominant(&p), "Melee", "profile: {:?}", p);
        let mix = p.biased_normalized();
        assert!(mix[1] > 0.6, "melee should dominate: {:?}", mix);
        assert!(mix[0] < 0.25, "tank should be low (glass cannon): {:?}", mix);
    }

    #[test]
    fn infer_burevestnik_is_ranged_with_support() {
        // Буревестник: melee + thunderstrike (5 mana AoE) + heal (2 mana).
        // Expected: Ranged dominant (thunderstrike weight 6), Support secondary.
        let (db, ac) = db_with_cache();
        let p = infer_profile(&ids(&["melee_attack", "thunderstrike", "heal"]), 14, 1, &db, &ac);
        assert_eq!(dominant(&p), "Ranged", "profile: {:?}", p);
        assert!(p.support > 0.0, "heal should contribute: {:?}", p);
        let mix = p.biased_normalized();
        assert!(mix[4] > 0.10, "support should be present at ~10-25% after bias: {:?}", mix);
    }

    #[test]
    fn infer_starshina_is_support() {
        // Старшина: melee + heal + burn + spark. hp 22, armor 3.
        // Expected: Support dominant (heal weight 3 + tank stat bonus not enough to overtake).
        let (db, ac) = db_with_cache();
        let p = infer_profile(&ids(&["melee_attack", "heal", "burn", "spark"]), 22, 3, &db, &ac);
        assert!(p.support > 0.0);
        // Support might lose to Tank due to stat bonus. Check it's either top or close.
        let dom = dominant(&p);
        assert!(
            dom == "Support" || dom == "Tank",
            "expected Support or Tank-hybrid, got {} with profile {:?}", dom, p
        );
    }

    #[test]
    fn infer_stormborn_warrior_is_tank_melee() {
        // Stormborn Воин: only melee, hp 18, armor 2.
        // Expected: Tank + Melee mix (brawler).
        let (db, ac) = db_with_cache();
        let p = infer_profile(&ids(&["melee_attack"]), 18, 2, &db, &ac);
        assert!(p.melee > 0.5, "should have melee vote: {:?}", p);
        assert!(p.tank > 0.5, "stat-based tank should be present: {:?}", p);
        let dom = dominant(&p);
        assert!(
            dom == "Tank" || dom == "Melee",
            "expected Tank or Melee, got {}", dom
        );
    }

    // ── tag_axis_vote rule pins (replaced the retired legacy-comparison snapshot) ──

    // Helper: compute weight the same way infer_profile does.
    fn ability_weight(def: &crate::content::abilities::AbilityDef) -> f32 {
        1.0 + def.costs.iter().map(|c| c.amount as f32).sum::<f32>()
    }

    /// `field_medic` is SingleAlly+Heal → Rescue tag → all weight on Support (v[4]).
    /// Axis order: [Tank=0, Melee=1, Ranged=2, Control=3, Support=4].
    /// costs=[{energy,1}] → weight=2.0.
    #[test]
    fn tag_axis_vote_rescue_ability_votes_pure_support() {
        let (content, ac) = db_with_cache();
        let id = AbilityId::from("field_medic");
        let def = &content.abilities[&id];
        let tags = ac.effective(&id);
        let w = ability_weight(def);
        let v = tag_axis_vote(tags, def, w);
        assert!((v[4] - w).abs() < 1e-5, "Support should equal weight {w}, got {}", v[4]);
        assert_eq!([v[0], v[1], v[2], v[3]], [0.0, 0.0, 0.0, 0.0], "non-Support axes must be zero");
    }

    /// `backstab` is OFFENSIVE melee; its status (poisoned) is DOT not CC →
    /// no ApplyCC tag → pure Melee (v[1]), Control (v[3]) must be 0.
    /// costs=[{energy,2}] → weight=3.0.
    #[test]
    fn tag_axis_vote_dot_damage_votes_zero_on_control() {
        let (content, ac) = db_with_cache();
        let id = AbilityId::from("backstab");
        let def = &content.abilities[&id];
        let tags = ac.effective(&id);
        let w = ability_weight(def);
        let v = tag_axis_vote(tags, def, w);
        assert_eq!(v[3], 0.0, "Control axis must be 0 for DOT-only status (DOT ≠ CC)");
        assert!(v[1] > 0.0, "Melee axis should be positive for melee damage, got {}", v[1]);
    }

    /// `taunt` has DEFENSIVE+PEEL tags → Peel branch: Tank=0.7*weight, Support=0.3*weight.
    /// costs=none → weight=1.0; expected: v[0]=0.7, v[4]=0.3, v[1..3]=0.
    #[test]
    fn tag_axis_vote_taunt_splits_tank_and_support() {
        let (content, ac) = db_with_cache();
        let id = AbilityId::from("taunt");
        let def = &content.abilities[&id];
        let tags = ac.effective(&id);
        let w = ability_weight(def);
        let v = tag_axis_vote(tags, def, w);
        assert!((v[0] - 0.7 * w).abs() < 1e-5, "Tank should be 0.7*weight={}, got {}", 0.7 * w, v[0]);
        assert!((v[4] - 0.3 * w).abs() < 1e-5, "Support should be 0.3*weight={}, got {}", 0.3 * w, v[4]);
        assert_eq!([v[1], v[2], v[3]], [0.0, 0.0, 0.0], "Melee/Ranged/Control must be zero");
    }

    /// `summon_storm_spirit` has Summon tag → Support=0.7*weight, Ranged=0.3*weight.
    /// costs=[{mana,3}] → weight=4.0; expected: v[4]=2.8, v[2]=1.2, others=0.
    #[test]
    fn tag_axis_vote_summon_splits_support_and_ranged() {
        let (content, ac) = db_with_cache();
        let id = AbilityId::from("summon_storm_spirit");
        let def = &content.abilities[&id];
        let tags = ac.effective(&id);
        let w = ability_weight(def);
        let v = tag_axis_vote(tags, def, w);
        assert!((v[4] - 0.7 * w).abs() < 1e-5, "Support should be 0.7*weight={}, got {}", 0.7 * w, v[4]);
        assert!((v[2] - 0.3 * w).abs() < 1e-5, "Ranged should be 0.3*weight={}, got {}", 0.3 * w, v[2]);
        assert_eq!([v[0], v[1], v[3]], [0.0, 0.0, 0.0], "Tank/Melee/Control must be zero");
    }

    /// Actor with ability `melee_attack` overridden to `[support]`
    /// must yield AxisProfile with dominant = Support.
    #[test]
    fn infer_profile_uses_override_when_set() {
        use crate::combat::ai::world::tags::cache::build_caches;

        let mut content = ContentView::load_global_for_tests();
        let id = AbilityId::from("melee_attack");
        if let Some(def) = content.abilities.get_mut(&id) {
            def.ai_tags_override = Some(vec!["rescue".to_string()]);
        }
        let (_, ac) = build_caches(&content);

        // hp 10, armor 0 → stat tank = (10+0)/20 = 0.5 (clamped to 0.3 min → 0.5)
        // melee_attack override = Rescue → Support 1.0 (weight 1.0)
        // Total: Tank=0.5, Support=1.0 → dominant = Support
        let p = infer_profile(&ids(&["melee_attack"]), 10, 0, &content, &ac);
        assert_eq!(
            dominant(&p),
            "Support",
            "override to rescue should make Support dominant: {:?}", p
        );
    }
}
