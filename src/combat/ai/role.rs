use crate::combat::ai::tuning::AiTuning;
use crate::content::abilities::{AoEShape, EffectDef, TargetType};
use crate::content::content_view::ContentView;
use crate::core::AbilityId;
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
) -> AxisProfile {
    let mut p = AxisProfile::default();

    for id in abilities {
        let Some(def) = content.abilities.get(id) else { continue };
        let v = ability_vote(def);
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

/// Vote a single ability into the 5-axis space.
/// Returns `[tank, melee, ranged, control, support]`.
fn ability_vote(def: &crate::content::abilities::AbilityDef) -> [f32; 5] {
    let cost: f32 = def.costs.iter().map(|c| c.amount as f32).sum();
    let weight = 1.0 + cost;
    let mut v = [0.0f32; 5];

    // 1. Heal on ally → pure Support.
    if def.target_type == TargetType::SingleAlly
        && matches!(def.effect, EffectDef::Heal { .. })
    {
        v[4] += weight;
        return v;
    }

    // 2. Summon → Support + Ranged (caster-style summoner, not a self-buff tank).
    if matches!(def.effect, EffectDef::Summon { .. }) {
        v[4] += weight * 0.7;
        v[2] += weight * 0.3;
        return v;
    }

    // 3. Self-buff / taunt (Myself target, no damage) → Tank.
    if def.target_type == TargetType::Myself && !has_damage(def) {
        v[0] += weight;
        return v;
    }

    // 3. Damage abilities: melee vs ranged split.
    if has_damage(def) {
        let is_spell = matches!(def.effect, EffectDef::SpellDamage { .. });
        let is_aoe = def.aoe != AoEShape::None;
        let is_ranged_phys = def.range.min >= 2;

        // TODO: melee cleave (aoe != None && range.max == 1) should vote Melee,
        // not Ranged. No such content yet — all AoE in current game is ranged.
        if is_aoe || is_spell || is_ranged_phys {
            v[2] += weight;
        } else {
            v[1] += weight;
        }

        // Damage + status (e.g. poison_shot) has partial Control signature.
        if !def.statuses.is_empty() {
            v[3] += weight * 0.4;
        }
        return v;
    }

    // 4. Status-only ability (stun, paralyze) → Control.
    if !def.statuses.is_empty() {
        v[3] += weight;
        return v;
    }

    // 5. Movement / utility (rush) → weak Melee fallback (aggressive mobility).
    v[1] += weight * 0.3;
    v
}

fn has_damage(def: &crate::content::abilities::AbilityDef) -> bool {
    matches!(
        def.effect,
        EffectDef::Damage { .. } | EffectDef::SpellDamage { .. } | EffectDef::WeaponAttack,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::factors::{DAMAGE_IDX, HEAL_IDX};
    use crate::core::AbilityId;

    fn db() -> ContentView {
        ContentView::load_global_for_tests()
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
        let db = db();
        let p = infer_profile(
            &ids(&["melee_attack", "bow_shot", "paralyzing_shot", "field_medic", "poison_shot"]),
            18, 2, &db,
        );
        assert_eq!(dominant(&p), "Ranged", "profile: {:?}", p);
        assert!(p.ranged > p.support);
        assert!(p.support > 0.0, "should have some Support from field_medic");
    }

    #[test]
    fn infer_lyra_is_ranged_mage() {
        // Lyra: mage — melee + flash + burn + fireball + heal.
        // Expected: Ranged dominant (fireball weight 6 dominates).
        let db = db();
        let p = infer_profile(
            &ids(&["melee_attack", "flash", "burn", "fireball", "heal"]),
            10, 0, &db,
        );
        assert_eq!(dominant(&p), "Ranged", "profile: {:?}", p);
        assert!(p.support > 0.0, "heal should contribute to Support");
        assert!(p.control > 0.0, "burn/AoE should contribute to Control");
    }

    #[test]
    fn infer_aldric_is_control_tank() {
        // Aldric: warrior — melee + taunt + stun + rush.
        // Expected: Control (stun weight=4) or Tank dominant.
        let db = db();
        let p = infer_profile(
            &ids(&["melee_attack", "taunt", "stun", "rush"]),
            20, 5, &db,
        );
        // Stun weight 4 (3 rage + 1) → Control 4; Tank = taunt(1) + rush(0.3×3 fallback? actually rush goes to melee)
        // Actually: stun has no damage + has status → Control 4. Tank gets taunt(1) + stat 25/20=1.25.
        // rush is target=Myself no damage, no statuses → Tank (not stun-like). Let me recheck.
        // rush: target_type=Myself, effect=grant_movement (not damage), no statuses.
        //   → Self-buff branch: Tank +3 (weight 1+2).
        // taunt: target_type=Myself, effect=None, has statuses → but self-buff branch matches first.
        //   Wait, taunt has target_type=Myself and statuses=[defending, taunted]. Our rule:
        //   "Myself + no damage" catches it BEFORE status check → Tank +1.
        // So: Tank = 1(taunt) + 3(rush) + 1.25(stat) = 5.25. Control = 4(stun).
        // Dominant: Tank.
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
        let db = db();
        let p = infer_profile(&ids(&["melee_attack", "backstab"]), 12, 1, &db);
        assert_eq!(dominant(&p), "Melee", "profile: {:?}", p);
        let mix = p.biased_normalized();
        assert!(mix[1] > 0.6, "melee should dominate: {:?}", mix);
        assert!(mix[0] < 0.25, "tank should be low (glass cannon): {:?}", mix);
    }

    #[test]
    fn infer_burevestnik_is_ranged_with_support() {
        // Буревестник: melee + thunderstrike (5 mana AoE) + heal (2 mana).
        // Expected: Ranged dominant (thunderstrike weight 6), Support secondary.
        let db = db();
        let p = infer_profile(&ids(&["melee_attack", "thunderstrike", "heal"]), 14, 1, &db);
        assert_eq!(dominant(&p), "Ranged", "profile: {:?}", p);
        assert!(p.support > 0.0, "heal should contribute: {:?}", p);
        let mix = p.biased_normalized();
        assert!(mix[4] > 0.10, "support should be present at ~10-25% after bias: {:?}", mix);
    }

    #[test]
    fn infer_starshina_is_support() {
        // Старшина: melee + heal + burn + spark. hp 22, armor 3.
        // Expected: Support dominant (heal weight 3 + tank stat bonus not enough to overtake).
        let db = db();
        let p = infer_profile(&ids(&["melee_attack", "heal", "burn", "spark"]), 22, 3, &db);
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
        let db = db();
        let p = infer_profile(&ids(&["melee_attack"]), 18, 2, &db);
        assert!(p.melee > 0.5, "should have melee vote: {:?}", p);
        assert!(p.tank > 0.5, "stat-based tank should be present: {:?}", p);
        let dom = dominant(&p);
        assert!(
            dom == "Tank" || dom == "Melee",
            "expected Tank or Melee, got {}", dom
        );
    }
}
