use bevy::prelude::*;

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
    /// [0..1] Quality of the final pick. Derives score_noise (high = 0) and
    /// top_k (high = always best). Single exposed knob for two internal effects.
    pub decision_quality: f32,
    /// Cap on candidates kept after dedup. Low = shallower search, misses
    /// clever move+cast lines.
    pub candidate_budget: usize,
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
}

impl DifficultyProfile {
    pub fn easy() -> Self {
        Self {
            awareness: 0.55,
            decision_quality: 0.30,
            candidate_budget: 12,
            intent_commitment: 0.75,
            survival_instinct: 0.55,
            resource_discipline: 0.60,
            coordination: 0.40,
            mercy: 0.35,
            plan_max_depth: 3,
            plan_beam_width: 8,
            plan_step_discount: 0.75,
        }
    }

    pub fn normal() -> Self {
        Self {
            awareness: 0.80,
            decision_quality: 0.75,
            candidate_budget: 20,
            intent_commitment: 1.00,
            survival_instinct: 0.80,
            resource_discipline: 1.00,
            coordination: 0.90,
            mercy: 0.10,
            plan_max_depth: 3,
            plan_beam_width: 16,
            plan_step_discount: 0.85,
        }
    }

    pub fn hard() -> Self {
        Self {
            awareness: 1.00,
            decision_quality: 1.00,
            candidate_budget: 30,
            intent_commitment: 1.20,
            survival_instinct: 1.00,
            resource_discipline: 1.20,
            coordination: 1.30,
            mercy: 0.00,
            plan_max_depth: 3,
            plan_beam_width: 24,
            plan_step_discount: 0.90,
        }
    }

    // ── Derived parameters ──────────────────────────────────────────────
    // All reads go through these methods so the mapping lives in one place.

    /// Random noise added per-candidate in score_candidates. 0 = deterministic.
    pub fn score_noise(&self) -> f32 {
        lerp(0.6, 0.0, self.decision_quality)
    }

    /// How many top candidates to sample from (1 = always argmax).
    pub fn top_k_choice(&self) -> usize {
        if self.decision_quality < 0.4 {
            3
        } else if self.decision_quality < 0.8 {
            2
        } else {
            1
        }
    }

    /// Minimum `pos_eval` improvement to keep a Reposition candidate.
    /// Used both by Reposition intent_score and compute_retreat.
    pub fn reposition_min_improvement(&self) -> f32 {
        lerp(0.30, 0.12, self.survival_instinct)
    }

    /// Margin in is_defensive: a tile is "safer" if danger(tile) + margin < current.
    pub fn defensive_tile_margin(&self) -> f32 {
        lerp(0.25, 0.10, self.survival_instinct)
    }

    /// HP% threshold for the hard-override "panic" gate in intent.rs.
    /// Low instinct → panics earlier (higher threshold).
    pub fn survival_hp_threshold(&self) -> f32 {
        lerp(0.35, 0.20, self.survival_instinct)
    }

    /// Danger threshold paired with the panic gate.
    /// Low awareness → needs more obvious danger to trigger.
    pub fn awareness_danger_threshold(&self) -> f32 {
        lerp(0.90, 0.60, self.awareness)
    }

    /// `pos_eval` threshold below which Reposition is considered.
    /// Low awareness → only triggers on very bad positions.
    pub fn awareness_reposition_threshold(&self) -> f32 {
        lerp(-0.9, -0.3, self.awareness)
    }

    /// Scales the anti-overkill penalty applied to damage when previous units
    /// have already reserved enough damage to kill the target.
    /// Returns the residual multiplier (lower = harsher penalty).
    pub fn overkill_damage_multiplier(&self) -> f32 {
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
