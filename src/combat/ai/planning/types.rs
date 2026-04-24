//! Plan data model: steps, multi-step turns, and the cumulative outcome of
//! applying a plan to a simulated battle state.

use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::core::AbilityId;
use crate::game::hex::Hex;
use bevy::prelude::Entity;
use std::hash::{Hash, Hasher};

/// One atomic action inside a turn plan.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum PlanStep {
    /// Walk along `path`. `path` excludes the starting tile and includes the
    /// destination — matches the convention used by `ReachableMap::path_to`
    /// and `MoveUnit { path }`. `path.len()` therefore equals MP cost.
    Move {
        #[serde(with = "crate::combat::ai::serde_helpers::hex_vec")]
        path: Vec<Hex>,
    },
    /// Cast `ability` at `target` (living entity) on `target_pos` (cell the
    /// primary effect is centred on; for AoE this is the blast origin).
    Cast {
        ability: AbilityId,
        #[serde(with = "crate::combat::ai::serde_helpers::entity")]
        target: Entity,
        #[serde(with = "crate::combat::ai::serde_helpers::hex")]
        target_pos: Hex,
    },
}

/// A candidate plan for a whole turn (1..=max_depth steps). Scored as a unit;
/// only the first step is committed per tick, and next tick either validates &
/// continues or replans.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TurnPlan {
    pub steps: Vec<PlanStep>,
    /// Actor's position after all steps.
    #[serde(with = "crate::combat::ai::serde_helpers::hex")]
    pub final_pos: Hex,
    /// AP remaining after all steps.
    pub residual_ap: i32,
    /// MP remaining after all steps.
    pub residual_mp: i32,
    /// Per-step outcomes aggregated during generation; consumed by Phase 3
    /// scoring to compute the final factors without re-running sim.
    pub outcomes: Vec<StepOutcome>,
    /// Cheap proxy score used for beam-search pruning only. The final score
    /// and factor decomposition are produced in Phase 3.
    pub partial_score: f32,
    /// Sim snapshot cached after each applied step. `sim_snapshots[k]` is the
    /// world state AFTER `steps[0..=k]` have been simulated. The "pre-step-k"
    /// snapshot a scorer needs is `sim_snapshots[k-1]` for k>0, or the
    /// original snapshot for k=0.
    ///
    /// **Shape invariant** — exactly one of:
    /// - `sim_snapshots.len() == steps.len()` (generator-filled, normal path).
    /// - `sim_snapshots.is_empty()` (deserialized plan — `#[serde(skip)]`
    ///   drops the vec at round-trip). Readers must tolerate this via
    ///   [`TurnPlan::pre_step_snapshot`] instead of direct indexing, so a
    ///   replay tool that accidentally hands a deserialized plan to the
    ///   scorer gets stale-but-safe factors rather than an OOB panic.
    ///
    /// Populated inside `generate_plans` (we already ran the sim there to
    /// produce `outcomes`; caching the resulting state costs one `.clone()`
    /// per step). Consumed by `compute_plan_factors` so it doesn't re-simulate
    /// the whole plan a second time. Runtime-only; excluded from the AI log
    /// because snapshots are derivable from `snapshot + steps`.
    #[serde(skip)]
    pub sim_snapshots: Vec<BattleSnapshot>,
    /// Per-step outcome annotations. Populated during plan generation alongside
    /// `outcomes`. Consumers (compute_factors, intent_score, critics, terminal
    /// eval) read this instead of recomputing from raw snapshot — see
    /// docs/ai_rework.md §4.
    ///
    /// Step 4.0: zero-filled. Step 4.1+: sim starts writing expected_damage.
    ///
    /// `#[serde(skip)]` — runtime-only in wave 1. Schema bump v18→v19 deferred
    /// to step 4.5 (see open Q3 in docs/ai_rework_step4_plan.md).
    #[serde(skip, default)]
    pub annotation: PlanAnnotation,
}

impl TurnPlan {
    /// Pre-step snapshot for step `idx`. Returns `initial` for `idx == 0`
    /// (no prior step) and also when `sim_snapshots` is empty (deserialized
    /// plan — see shape invariant on `sim_snapshots`). Prefer this over
    /// direct `plan.sim_snapshots[idx - 1]` indexing anywhere a scorer might
    /// run on a deserialized plan — it's the only safe access pattern.
    pub fn pre_step_snapshot<'a>(
        &'a self,
        idx: usize,
        initial: &'a BattleSnapshot,
    ) -> &'a BattleSnapshot {
        if idx == 0 {
            return initial;
        }
        self.sim_snapshots.get(idx - 1).unwrap_or(initial)
    }
}

/// Structural decomposition of a plan's *committed prefix* — the first one or
/// two steps that would actually execute if this plan were picked. The next
/// AI tick re-plans from scratch, so anything past this prefix is lookahead
/// and doesn't fire.
///
/// Single source of truth for the bundling rule, consumed by three places
/// that used to each ship their own parallel pattern-match on `plan.steps`:
///
/// - `picker::commit_plan` wraps a prefix into an `AiDecision` (with a few
///   edge cases — empty path → `EndTurn`, etc. — layered on top).
/// - `ScoredStep::from_plan_committed` converts a prefix into a single
///   `ScoredStep` view for debug/log formatting.
/// - `TurnPlan::committed_step_count` just asks for the step count.
///
/// Adding a new bundling variant (say `[Cast, Move]` for strike-and-retreat)
/// is now one enum arm; the compiler will point at the three consumers that
/// need to grow matching arms, instead of letting one fall out of sync.
pub enum CommittedPrefix<'a> {
    /// Empty plan — nothing commits this tick.
    EndTurn,
    /// Solo Cast — the single step fires from the actor's current tile.
    Cast {
        ability: &'a AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    /// Move→Cast bundle — both steps fire atomically this tick. Cast runs
    /// from the move destination.
    MoveThenCast {
        path: &'a [Hex],
        ability: &'a AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    /// Solo Move (no bundled Cast on the next slice) — only the move commits.
    MoveOnly { path: &'a [Hex] },
}

impl CommittedPrefix<'_> {
    /// How many of `plan.steps` this prefix consumes.
    pub fn step_count(&self) -> usize {
        match self {
            Self::EndTurn => 0,
            Self::Cast { .. } | Self::MoveOnly { .. } => 1,
            Self::MoveThenCast { .. } => 2,
        }
    }
}

impl TurnPlan {
    /// Decompose this plan into its committed prefix — the leading slice that
    /// would fire if this plan were picked. See [`CommittedPrefix`].
    pub fn committed_prefix(&self) -> CommittedPrefix<'_> {
        match self.steps.as_slice() {
            [] => CommittedPrefix::EndTurn,
            [PlanStep::Cast { ability, target, target_pos }, ..] => {
                CommittedPrefix::Cast {
                    ability,
                    target: *target,
                    target_pos: *target_pos,
                }
            }
            [PlanStep::Move { path }, PlanStep::Cast { ability, target, target_pos }, ..] => {
                CommittedPrefix::MoveThenCast {
                    path,
                    ability,
                    target: *target,
                    target_pos: *target_pos,
                }
            }
            [PlanStep::Move { path }, ..] => CommittedPrefix::MoveOnly { path },
        }
    }

    /// Number of leading steps that would be emitted as the `AiDecision` if
    /// this plan were picked. Scoring uses this to gate factors to the
    /// committed prefix — tail steps don't fire this tick.
    pub fn committed_step_count(&self) -> usize {
        self.committed_prefix().step_count()
    }

    /// Iterate steps with the caster tile **as it stands when the step
    /// fires** — i.e. the destination of the previous Move (or `start`
    /// for the very first step). Single source of truth for the
    /// "walk-the-plan-and-track-caster" pattern that used to be inlined in
    /// `sanity::plan_has_self_aoe`, `picker::record_committed_reservations`,
    /// and `generator::logical_key`.
    ///
    /// For Cast steps this is where the spell originates; for Move steps it
    /// is the actor's tile *before* the move (the path's destination
    /// becomes the caster tile of the next iteration).
    pub fn walk_with_caster<'a>(
        &'a self,
        start: Hex,
    ) -> impl Iterator<Item = (usize, &'a PlanStep, Hex)> + 'a {
        let mut caster = start;
        self.steps.iter().enumerate().map(move |(idx, step)| {
            let here = caster;
            if let PlanStep::Move { path } = step {
                if let Some(&dest) = path.last() {
                    caster = dest;
                }
            }
            (idx, step, here)
        })
    }

    /// Feed `hasher` with the plan's canonical logical-key bits — the same
    /// identity used by `generator::dedup_by_logical_key` (Move reduces to its
    /// destination; Cast keys on ability/target/target_pos plus the caster
    /// tile at the moment of firing). Zero-alloc: walks in place rather than
    /// building a `Vec<StepKey>`. Used by `finalize_scores` to derive a
    /// plan-order-independent noise seed.
    pub fn hash_canonical<H: Hasher>(&self, start: Hex, hasher: &mut H) {
        // Discriminants keep `[Move(A), Cast(B)]` distinct from any hypothetical
        // future prefix with the same payload on different arms.
        self.steps.len().hash(hasher);
        for (_, step, caster_pos) in self.walk_with_caster(start) {
            match step {
                PlanStep::Move { path } => {
                    0u8.hash(hasher);
                    path.last().copied().unwrap_or(caster_pos).hash(hasher);
                }
                PlanStep::Cast { ability, target, target_pos } => {
                    1u8.hash(hasher);
                    ability.hash(hasher);
                    target.hash(hasher);
                    target_pos.hash(hasher);
                    caster_pos.hash(hasher);
                }
            }
        }
    }
}

#[cfg(test)]
mod prefix_tests {
    use super::*;
    use crate::game::hex::hex_from_offset;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid")
    }

    fn cast() -> PlanStep {
        PlanStep::Cast {
            ability: AbilityId::from("strike"),
            target: ent(42),
            target_pos: hex_from_offset(3, 0),
        }
    }
    fn mov() -> PlanStep {
        PlanStep::Move { path: vec![hex_from_offset(1, 0)] }
    }
    fn plan(steps: Vec<PlanStep>) -> TurnPlan {
        TurnPlan { steps, ..Default::default() }
    }

    /// Classify a prefix by its discriminant — used only for assertion.
    #[derive(Debug, PartialEq, Eq)]
    enum Kind { EndTurn, Cast, MoveThenCast, MoveOnly }
    impl From<&CommittedPrefix<'_>> for Kind {
        fn from(p: &CommittedPrefix<'_>) -> Kind {
            match p {
                CommittedPrefix::EndTurn => Kind::EndTurn,
                CommittedPrefix::Cast { .. } => Kind::Cast,
                CommittedPrefix::MoveThenCast { .. } => Kind::MoveThenCast,
                CommittedPrefix::MoveOnly { .. } => Kind::MoveOnly,
            }
        }
    }

    #[test]
    fn committed_prefix_matches_plan_shape() {
        // (name, steps, expected variant, expected step count)
        let cases: Vec<(&str, Vec<PlanStep>, Kind, usize)> = vec![
            ("empty",              vec![],                          Kind::EndTurn,      0),
            ("solo cast",          vec![cast()],                    Kind::Cast,         1),
            ("solo move",          vec![mov()],                     Kind::MoveOnly,     1),
            ("move+cast bundle",   vec![mov(), cast()],             Kind::MoveThenCast, 2),
            ("move+move no bundle",vec![mov(), mov()],              Kind::MoveOnly,     1),
            ("cast+..tail ignored",vec![cast(), mov(), cast()],     Kind::Cast,         1),
        ];
        for (name, steps, want_kind, want_count) in cases {
            let p = plan(steps);
            let prefix = p.committed_prefix();
            assert_eq!(Kind::from(&prefix), want_kind, "{name}: variant mismatch");
            assert_eq!(prefix.step_count(), want_count, "{name}: step count");
            assert_eq!(p.committed_step_count(), want_count, "{name}: cached count");
        }
    }
}

/// Effects produced by a single simulated step. Used by scoring to accumulate
/// per-plan factors (damage/kill/heal/cc totals, worst-path danger, etc.).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct StepOutcome {
    /// Expected HP-equivalent damage dealt (post-armor / post-vulnerability).
    pub damage: f32,
    /// Expected HP-equivalent healing done.
    pub heal: f32,
    /// Targets whose HP dropped to 0 during this step (ordered by application).
    #[serde(with = "crate::combat::ai::serde_helpers::entity_vec")]
    pub killed: Vec<Entity>,
    /// Entities that received a turn-skipping status (stun, paralyse, sleep).
    #[serde(with = "crate::combat::ai::serde_helpers::entity_vec")]
    pub stunned: Vec<Entity>,
    /// Number of targets touched by the step (AoE or single). Zero for Move.
    pub hits: u32,
    /// True if the step was a Move.
    pub moved: bool,
}
