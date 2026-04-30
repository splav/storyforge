use bevy::prelude::*;
use super::tuning::AiTuning;

/// Difficulty profile for enemy AI. Each field represents a "quality of decisions"
/// knob rather than a stat multiplier — behaviour scales by changing how well the
/// AI reads the board, how carefully it picks moves, and how disciplined it is.
///
/// All [0..1] fields are clamped at use-sites; higher = sharper play.
#[derive(Resource, Clone, Debug)]
pub struct DifficultyProfile {
    /// [0..1] How well the AI reads the board. Applied as threshold shifts in
    /// intent selection (not a score multiplier — it would cancel out under
    /// symmetric normalization). Low = misses danger, slow to reposition.
    pub awareness: f32,
    /// [0..1] Quality of the final pick. Controls top_k_choice threshold and
    /// derives score_noise: easy tier (< 0.15) uses top_k = 3; noise scales
    /// linearly from 0.20 at 0.10 down to 0.0 at 0.30. Normal+ → argmax/0.
    pub decision_quality: f32,
    /// Multiplier on role_weights[intent]. Low = intent is a suggestion;
    /// high = AI really plays around the chosen intent.
    pub intent_commitment: f32,
    /// [0..1] Survival-layer knob. Derives Reposition sensitivity, defensive
    /// tile margin and HP panic threshold. High = reads LastStand vs ProtectSelf
    /// more sharply.
    pub survival_instinct: f32,
    /// Multiplier on role_weights[scarcity]. Low = burns mana/CC in the wrong
    /// moments; high = saves key abilities for the right swing.
    pub resource_discipline: f32,
    /// [0..~1.3] Team-play knob. Scales overkill penalty and adds a focus-fire
    /// bonus on targets already reserved by earlier units this round.
    pub coordination: f32,
    /// [0..1] Tie-breaker strength on near-equal candidates: prefer the less
    /// harsh option (lower kill+cc) when scores are within this margin. No
    /// effect when one option is clearly best.
    pub mercy: f32,
    /// Maximum length (number of steps) of a turn plan. Depth 1 = single
    /// action (legacy behaviour); 2+ enables Cast→Move, Move→Cast→Move etc.
    pub plan_max_depth: usize,
    /// How many partial plans survive pruning at each depth level. Wider =
    /// more lines explored, more CPU, fewer pruning-induced misses.
    pub plan_beam_width: usize,
    /// Per-step discount applied to cumulative factors (damage/heal/cc/scarcity)
    /// when aggregating a multi-step plan. step[k] contributes `base^k`. Lower =
    /// more pessimistic about deep plans (less trust that future steps execute
    /// as planned); higher = AI commits harder to long combos.
    pub plan_step_discount: f32,
    /// How many future rounds `UnitSnapshot::damage_horizon` projects. Used as
    /// the window for CC / heal / stun scoring — longer horizon means CC
    /// penalties and heal value factor in more of the enemy's sustained
    /// output. Typical values 3–7; default 5 matches an average combat arc.
    pub damage_horizon_rounds: u32,
}

impl DifficultyProfile {
    /// Beginner tier with stochastic picks: high noise and top-k sampling
    /// let lower-quality plans win occasionally, producing forgiving AI.
    pub fn easy() -> Self {
        Self {
            awareness: 0.35,
            decision_quality: 0.10,
            intent_commitment: 0.55,
            survival_instinct: 0.40,
            resource_discipline: 0.40,
            coordination: 0.20,
            mercy: 0.50,
            plan_max_depth: 2,
            plan_beam_width: 6,
            plan_step_discount: 0.70,
            damage_horizon_rounds: 3,
        }
    }

    pub fn normal() -> Self {
        Self {
            awareness: 0.55,
            decision_quality: 0.30,
            intent_commitment: 0.75,
            survival_instinct: 0.55,
            resource_discipline: 0.60,
            coordination: 0.40,
            mercy: 0.35,
            plan_max_depth: 3,
            plan_beam_width: 8,
            plan_step_discount: 0.75,
            damage_horizon_rounds: 5,
        }
    }

    pub fn hard() -> Self {
        Self {
            awareness: 0.80,
            decision_quality: 0.75,
            intent_commitment: 1.00,
            survival_instinct: 0.80,
            resource_discipline: 1.00,
            coordination: 0.90,
            mercy: 0.10,
            plan_max_depth: 3,
            plan_beam_width: 16,
            damage_horizon_rounds: 5,
            plan_step_discount: 0.85,
        }
    }

    pub fn epic() -> Self {
        Self {
            awareness: 1.00,
            decision_quality: 1.00,
            intent_commitment: 1.20,
            survival_instinct: 1.00,
            resource_discipline: 1.20,
            coordination: 1.30,
            mercy: 0.00,
            plan_max_depth: 3,
            damage_horizon_rounds: 5,
            plan_beam_width: 24,
            plan_step_discount: 0.90,
        }
    }

    // ── Derived parameters ──────────────────────────────────────────────
    // All reads go through these methods so the mapping lives in one place.

    /// Random noise added per-plan in `finalize_scores`. Derived from
    /// `decision_quality`: noise is only present on the `easy` tier (where
    /// `decision_quality < 0.30`), scaling down linearly to 0 by the time
    /// `decision_quality` reaches 0.30. Normal/hard/epic have noise=0 —
    /// reproducibility and determinism by construction, not by explicit flag.
    ///
    /// Amplitude values:
    /// - easy (decision_quality=0.10) → 0.20
    /// - normal+ (decision_quality ≥ 0.30) → 0.0
    pub fn score_noise(&self) -> f32 {
        (0.3 - self.decision_quality).max(0.0)
    }

    /// How many top candidates to sample from (1 = always argmax).
    /// Only the new `easy` tier (decision_quality < 0.15) returns 3;
    /// all other tiers pin this to 1 for deterministic argmax.
    pub fn top_k_choice(&self) -> usize {
        if self.decision_quality < 0.15 {
            3
        } else {
            1
        }
    }

    /// Minimum `pos_eval` improvement to keep a Reposition candidate.
    /// Used both by Reposition intent_score and compute_retreat.
    pub fn reposition_min_improvement(&self, tuning: &AiTuning) -> f32 {
        tuning.difficulty.reposition_min_improvement_curve.eval(self.survival_instinct)
    }

    /// Margin in is_defensive: a tile is "safer" if danger(tile) + margin < current.
    pub fn defensive_tile_margin(&self) -> f32 {
        lerp(0.25, 0.10, self.survival_instinct)
    }

    /// HP% threshold for the hard-override "panic" gate in intent.rs.
    /// Low instinct → panics earlier (higher threshold).
    pub fn survival_hp_threshold(&self, tuning: &AiTuning) -> f32 {
        tuning.difficulty.survival_hp_curve.eval(self.survival_instinct)
    }

    /// Danger threshold paired with the panic gate.
    /// Low awareness → needs more obvious danger to trigger.
    pub fn awareness_danger_threshold(&self, tuning: &AiTuning) -> f32 {
        tuning.difficulty.awareness_danger_curve.eval(self.awareness)
    }

    /// `pos_eval` threshold below which Reposition is considered.
    /// Low awareness → only triggers on very bad positions.
    pub fn awareness_reposition_threshold(&self) -> f32 {
        lerp(-0.9, -0.3, self.awareness)
    }

    /// Residual multiplier applied to offensive signals (damage AND kill)
    /// when previous units have already reserved enough damage to kill the
    /// target. Lower = harsher penalty; hard AI drops to the 0.15 floor so
    /// it almost never doubles up, easy AI keeps ~0.72 so un-coordinated
    /// play still happens. Applied uniformly to damage and kill — zeroing
    /// kill while leaking damage through was inconsistent and left overkill
    /// plans attractive in damage-dominant batches.
    pub fn overkill_multiplier(&self) -> f32 {
        (1.0 - 0.7 * self.coordination).clamp(0.15, 1.0)
    }

    /// Bonus multiplier on the `focus` factor for targets that already have
    /// damage reserved but aren't yet doomed — encourages focus-fire.
    pub fn focus_fire_bonus(&self) -> f32 {
        (0.30 * self.coordination).max(0.0)
    }

    /// Score delta under which mercy's tie-breaker kicks in.
    pub fn mercy_margin(&self) -> f32 {
        self.mercy.clamp(0.0, 1.0)
    }

    /// Middle-tier HP threshold for the viability-guard ProtectSelf fallback.
    /// Sits between the hard panic (`survival_hp_threshold`, typically 0.20)
    /// and normal intent selection. When intent viability fails (no plan can
    /// execute the chosen intent) AND the actor is below this threshold on a
    /// dangerous tile, we switch intent to ProtectSelf rather than forcing a
    /// fallback FocusTarget.
    ///
    /// Keyed off `awareness`: hard AI (aware = 1.0) pulls back earlier at 40%
    /// HP; easy AI (awareness 0.55) waits until 44.5%.
    pub fn midpanic_hp_threshold(&self) -> f32 {
        0.4 + 0.1 * (1.0 - self.awareness).clamp(0.0, 1.0)
    }
}

impl Default for DifficultyProfile {
    fn default() -> Self {
        Self::normal()
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{DifficultyProfile, AiTuning};

    #[test]
    fn noise_is_only_on_easy() {
        assert!(DifficultyProfile::easy().score_noise() > 0.0);
        assert_eq!(DifficultyProfile::normal().score_noise(), 0.0);
        assert_eq!(DifficultyProfile::hard().score_noise(), 0.0);
        assert_eq!(DifficultyProfile::epic().score_noise(), 0.0);
    }

    #[test]
    fn top_k_is_argmax_above_easy() {
        assert_eq!(DifficultyProfile::easy().top_k_choice(), 3);
        assert_eq!(DifficultyProfile::normal().top_k_choice(), 1);
        assert_eq!(DifficultyProfile::hard().top_k_choice(), 1);
        assert_eq!(DifficultyProfile::epic().top_k_choice(), 1);
    }

    /// Sanity: migrated methods with AiTuning::default() return bit-for-bit the same
    /// f32 values as the original hardcoded lerp formulas across all four difficulty tiers.
    #[test]
    fn lerp_curve_migration_values_match_original_hardcodes() {
        let t = AiTuning::default();

        // reposition_min_improvement: lerp(0.30, 0.12, survival_instinct)
        // easy=0.40 → 0.228, normal=0.55 → 0.201, hard=0.80 → 0.156, epic=1.00 → 0.12
        assert_eq!(DifficultyProfile::easy().reposition_min_improvement(&t),   0.30_f32 + (0.12 - 0.30) * 0.40);
        assert_eq!(DifficultyProfile::normal().reposition_min_improvement(&t), 0.30_f32 + (0.12 - 0.30) * 0.55);
        assert_eq!(DifficultyProfile::hard().reposition_min_improvement(&t),   0.30_f32 + (0.12 - 0.30) * 0.80);
        assert_eq!(DifficultyProfile::epic().reposition_min_improvement(&t),   0.30_f32 + (0.12 - 0.30) * 1.00);

        // survival_hp_threshold: lerp(0.35, 0.20, survival_instinct)
        // easy=0.40 → 0.29, normal=0.55 → 0.2675, hard=0.80 → 0.23, epic=1.00 → 0.20
        assert_eq!(DifficultyProfile::easy().survival_hp_threshold(&t),   0.35_f32 + (0.20 - 0.35) * 0.40);
        assert_eq!(DifficultyProfile::normal().survival_hp_threshold(&t), 0.35_f32 + (0.20 - 0.35) * 0.55);
        assert_eq!(DifficultyProfile::hard().survival_hp_threshold(&t),   0.35_f32 + (0.20 - 0.35) * 0.80);
        assert_eq!(DifficultyProfile::epic().survival_hp_threshold(&t),   0.35_f32 + (0.20 - 0.35) * 1.00);

        // awareness_danger_threshold: lerp(0.90, 0.60, awareness)
        // easy=0.35 → 0.795, normal=0.55 → 0.735, hard=0.80 → 0.66, epic=1.00 → 0.60
        assert_eq!(DifficultyProfile::easy().awareness_danger_threshold(&t),   0.90_f32 + (0.60 - 0.90) * 0.35);
        assert_eq!(DifficultyProfile::normal().awareness_danger_threshold(&t), 0.90_f32 + (0.60 - 0.90) * 0.55);
        assert_eq!(DifficultyProfile::hard().awareness_danger_threshold(&t),   0.90_f32 + (0.60 - 0.90) * 0.80);
        assert_eq!(DifficultyProfile::epic().awareness_danger_threshold(&t),   0.90_f32 + (0.60 - 0.90) * 1.00);
    }
}
