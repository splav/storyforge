//! Mode-selection and adaptation algorithm.
//!
//! Two entry points:
//! - [`select_evaluation_modes`] — pure pass: which `EvaluationMode` for
//!   each plan, based on snapshot facts only. Does not touch scores.
//! - [`apply_adaptation`] — mutation pass: calls `select_evaluation_modes`
//!   then triggers `rescore_with_per_plan_modes` for switched plans.
//!   Preserved for the existing unit-test suite; new pipeline code uses
//!   `select_evaluation_modes` + `FinalizeStage` instead.

use crate::combat::ai::adapt::{Adaptation, AdaptationReason, EvaluationMode};
use crate::combat::ai::factors::{PlanFactor, PlanFactorValues};
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::pipeline::stages::sanity::expected_aoo_damage;
use crate::combat::ai::planning::scorer::rescore_with_per_plan_modes;
use crate::combat::ai::pipeline::stages::sanity::plan_is_defensive;
use crate::combat::ai::planning::TurnPlan;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::ScoringCtx;
use crate::content::content_view::ContentView;

/// Sum of damage the actor is guaranteed to take from active status
/// effects before their next meaningful action — i.e. the pending DoT
/// tick that will fire while someone else is acting.
///
/// Components:
/// - `dot_per_tick` (capped at `.max(0)` — defensive guardrail against
///   hypothetical negative values; live code only writes positives).
/// - `hp_percent_dot` from `StatusDef` in content, converted to absolute
///   HP via `ceil(max_hp × pct / 100)` — mirrors the live tick path in
///   `advance_turn::tick_statuses_on_entity`.
///
/// Only statuses with `rounds_remaining > 0` are counted (expired rows
/// are usually filtered already, but the check keeps this safe if a
/// snapshot includes zero-round entries mid-refresh).
///
/// Used by `select_evaluation_modes` to detect the `ProtectSelfFutile` case —
/// "contract can be satisfied spatially, but DoT will kill the actor
/// anyway before he acts again".
pub fn pending_dot_before_next_action(active: &UnitSnapshot, content: &ContentView) -> i32 {
    let mut total = 0i32;
    for s in &active.statuses {
        if s.rounds_remaining == 0 {
            continue;
        }
        total = total.saturating_add(s.dot_per_tick.max(0));
        if let Some(sd) = content.statuses.get(&s.id) {
            if sd.hp_percent_dot > 0 {
                let tick = (active.max_hp as f32 * sd.hp_percent_dot as f32 / 100.0).ceil() as i32;
                total = total.saturating_add(tick.max(0));
            }
        }
    }
    total
}

/// Would executing this plan leave the actor alive at the end of its
/// own turn, after the next status tick that will fire before the actor
/// acts again?
///
/// Reads the post-plan snapshot (`sim_snapshots.last()`) — the correct
/// horizon for DoT doom because external state doesn't change during a
/// single actor's turn in this engine, and DoT from enemy-applied
/// statuses ticks on the *applier's* turn (after this actor is done).
/// So a plan that heals / cleanses in its tail genuinely rescues, even
/// if the heal is outside the committed prefix — the next AI-tick
/// continues the same plan from the same doom-state.
///
/// Deserialized plans (empty `sim_snapshots`, see shape invariant on
/// `TurnPlan::sim_snapshots`) fall back to `initial` — safe but stale;
/// in that case the check sees pre-plan HP/statuses, which for a doomed
/// actor means "no rescue" and LastStand triggers. That's the
/// conservative side of the fallback.
fn plan_has_self_rescue(
    plan: &TurnPlan,
    active: &UnitSnapshot,
    initial: &BattleSnapshot,
    content: &ContentView,
) -> bool {
    let post = plan.sim_snapshots.last().unwrap_or(initial);
    let Some(actor_post) = post.unit(active.entity) else {
        return false;
    };
    if actor_post.hp <= 0 {
        return false;
    }
    actor_post.hp > pending_dot_before_next_action(actor_post, content)
}

/// Pure mode-selection pass — step 11.0.
///
/// Evaluates snapshot facts for every plan and returns per-plan
/// `EvaluationMode` assignments together with their reasons.
/// Does **not** touch `ann.score`, `ann.factors`, or `raw` — scoring
/// is entirely deferred to `FinalizeStage`.
///
/// This is the first half of the old `apply_adaptation` split:
/// - **`select_evaluation_modes`** — which mode for each plan? (this fn)
/// - **`rescore_with_per_plan_modes`** in `scorer.rs` — apply those modes.
///
/// # Contract
///
/// Pure function over snapshot facts. Inputs are read-only (except
/// `plans` which is `&[TurnPlan]`). Callers may run Sanity/Critics
/// multipliers *after* this returns and *before* calling
/// `rescore_with_per_plan_modes`, because mode selection is
/// fact-driven and does not depend on scores.
pub fn select_evaluation_modes(
    plans: &[TurnPlan],
    raw: &[PlanFactorValues],
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
) -> Adaptation {
    debug_assert_eq!(plans.len(), raw.len());

    let mut adaptation = Adaptation::empty(plans.len());
    if plans.is_empty() {
        return adaptation;
    }

    let active = ctx.active;
    let content = ctx.world.content;

    // ── Global rules under ProtectSelf ────────────────────────────────────
    if matches!(intent, TacticalIntent::ProtectSelf) {
        let any_defensive = raw
            .iter()
            .any(|f| plan_is_defensive(f.get_plan(PlanFactor::SelfSurvival), ctx.world.tuning.thresholds.self_survival_epsilon));
        if !any_defensive {
            for i in 0..plans.len() {
                adaptation.modes[i] = EvaluationMode::LastStand;
                adaptation.reasons[i] = Some(AdaptationReason::ProtectSelfNoDefensive);
            }
            return adaptation;
        }

        let pending_dot = pending_dot_before_next_action(active, content);
        if pending_dot >= active.hp {
            let any_rescue = plans
                .iter()
                .any(|p| plan_has_self_rescue(p, active, ctx.snap, content));
            if !any_rescue {
                let reason = AdaptationReason::ProtectSelfFutile {
                    pending_dot,
                    actor_hp: active.hp,
                };
                for i in 0..plans.len() {
                    adaptation.modes[i] = EvaluationMode::LastStand;
                    adaptation.reasons[i] = Some(reason.clone());
                }
                return adaptation;
            }
        }

        // ProtectSelf with defensive options AND feasible survival:
        // contract still holds. Per-plan ExpectedSelfLethal is gated off.
        return adaptation;
    }

    // ── Per-plan rule: ExpectedSelfLethal ─────────────────────────────────
    let enemies: Vec<&UnitSnapshot> = ctx.snap.enemies_of(active.team).collect();
    let hp_cutoff = active.hp as f32;
    for (i, plan) in plans.iter().enumerate() {
        if active.hp <= 0 {
            break;
        }
        let aoo_dmg = expected_aoo_damage(active, plan, &enemies);
        if aoo_dmg >= hp_cutoff {
            adaptation.modes[i] = EvaluationMode::LastStand;
            adaptation.reasons[i] = Some(AdaptationReason::ExpectedSelfLethal {
                aoo_dmg,
                actor_hp: active.hp,
            });
        }
    }

    adaptation
}

/// Run the ADAPTATION pass over the plan pool. Returns an `Adaptation`
/// reflecting per-plan mode decisions; mutates `raw` (intent-column for
/// switched plans) and `scored` (full batch rescored so normalisation
/// stays consistent).
///
/// See the module docstring for the five invariants this pass upholds.
/// The code structure below maps 1:1 to them:
/// - single function body, no recursion → **ONE PASS**
/// - triggers read `active.hp`, `expected_aoo_damage`, `plan_is_defensive`,
///   `intent` — all snapshot/input facts → **FACTS ONLY**
/// - no score multiplication, no masking; only mode map + rescore →
///   **NO PENALTIES / NO MASKS**
/// - rescore overwrites `raw[i].intent`; calling again produces the same
///   value → **IDEMPOTENT**
/// - does not consult contract masks and does not prevent them from
///   running afterwards → **CONTRACT-NEUTRAL**
///
/// # Deprecation note (step 11.0)
///
/// Preserved for the existing unit-test suite in this module, which tests
/// mode-selection + rescore in concert. New pipeline code uses
/// `select_evaluation_modes` + `FinalizeStage` instead.
pub fn apply_adaptation(
    plans: &mut [TurnPlan],
    raw: &mut [PlanFactorValues],
    scored: &mut Vec<f32>,
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
) -> Adaptation {
    debug_assert_eq!(plans.len(), raw.len());
    debug_assert_eq!(plans.len(), scored.len());

    let mut adaptation = Adaptation::empty(plans.len());
    if plans.is_empty() {
        return adaptation;
    }

    let active = ctx.active;
    let content = ctx.world.content;

    // ── Global rules under ProtectSelf ────────────────────────────────────
    // Two ways the contract can be unsatisfiable, each with its own
    // rescue horizon (see module docstring on AdaptationReason variants):
    //   1. SPATIAL — `ProtectSelfNoDefensive`: no plan reaches safety.
    //   2. TEMPORAL — `ProtectSelfFutile`: safety is reachable but pending
    //      DoT kills the actor before he acts again.
    //
    // Applied first because global — any per-plan ExpectedSelfLethal
    // rule would be shadowed by a global switch anyway.
    if matches!(intent, TacticalIntent::ProtectSelf) {
        let any_defensive = raw
            .iter()
            .any(|f| plan_is_defensive(f.get_plan(PlanFactor::SelfSurvival), ctx.world.tuning.thresholds.self_survival_epsilon));
        if !any_defensive {
            for i in 0..plans.len() {
                adaptation.modes[i] = EvaluationMode::LastStand;
                adaptation.reasons[i] = Some(AdaptationReason::ProtectSelfNoDefensive);
            }
            *scored = rescore_with_per_plan_modes(plans, raw, &adaptation.modes, intent, ctx);
            return adaptation;
        }

        // Defensive option exists spatially — check temporal feasibility.
        // is_doomed: pending DoT alone would kill the actor next tick.
        // If so, require *some* plan to leave the actor alive at end of
        // turn (via self-heal or cleanse in sim_snapshots.last()). If no
        // such plan exists, the contract is futile — flip global LastStand.
        //
        // MVP gate: only under intent == ProtectSelf. A future extension
        // might extend doom-check to non-ProtectSelf intents (actor
        // doomed but picked FocusTarget because danger-map didn't flag
        // panic_override), but that requires a broader rescue-audit and
        // is deferred until replay evidence demands it.
        let pending_dot = pending_dot_before_next_action(active, content);
        if pending_dot >= active.hp {
            let any_rescue = plans
                .iter()
                .any(|p| plan_has_self_rescue(p, active, ctx.snap, content));
            if !any_rescue {
                let reason = AdaptationReason::ProtectSelfFutile {
                    pending_dot,
                    actor_hp: active.hp,
                };
                for i in 0..plans.len() {
                    adaptation.modes[i] = EvaluationMode::LastStand;
                    adaptation.reasons[i] = Some(reason.clone());
                }
                *scored = rescore_with_per_plan_modes(plans, raw, &adaptation.modes, intent, ctx);
                return adaptation;
            }
        }

        // ProtectSelf with defensive options AND feasible survival:
        // contract still holds. ExpectedSelfLethal per-plan adaptation
        // is gated off — the actor is committed to self-preservation,
        // self-lethal plans are contract violations and should be
        // masked, not rescored.
        return adaptation;
    }

    // ── Per-plan rule: ExpectedSelfLethal ─────────────────────────────────
    // Only under non-ProtectSelf intents. Under FocusTarget/ApplyCC/...,
    // a plan whose EV-AoO cost exceeds the actor's HP represents a trade
    // that the actor's current value function cannot express; LastStand's
    // "final useful action" table evaluates it on its own terms (kill >
    // cc > damage), so the plan competes honestly against defensive
    // alternatives.
    let enemies: Vec<&UnitSnapshot> = ctx.snap.enemies_of(active.team).collect();
    let hp_cutoff = active.hp as f32;
    let mut any_switched = false;
    for (i, plan) in plans.iter().enumerate() {
        if active.hp <= 0 {
            break; // Dead actor has no plans to adapt — guard against weird snapshots.
        }
        let aoo_dmg = expected_aoo_damage(active, plan, &enemies);
        if aoo_dmg >= hp_cutoff {
            adaptation.modes[i] = EvaluationMode::LastStand;
            adaptation.reasons[i] = Some(AdaptationReason::ExpectedSelfLethal {
                aoo_dmg,
                actor_hp: active.hp,
            });
            any_switched = true;
        }
    }

    if any_switched {
        *scored = rescore_with_per_plan_modes(plans, raw, &adaptation.modes, intent, ctx);
    }

    adaptation
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::pipeline::stages::sanity::sanity_adjust_plans;
    use crate::combat::ai::planning::PlanStep;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};

    // ── apply_adaptation ─────────────────────────────────────────────────
    //
    // Scaffolding: each test builds a minimal actor + adjacent melee
    // enemy + plan(s), runs `apply_adaptation`, inspects `modes` /
    // `reasons` / side effects on `scored`. `expected_aoo_damage`
    // lights up when a Move step leaves adjacency (see sanity.rs tests
    // for the full matrix) — we use the simplest topology: actor at
    // origin, enemy at (1,0), Move to (-1,0) triggers an AoO.

    fn move_plan(path: Vec<Hex>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            final_pos: *path.last().unwrap(),
            residual_ap: 1,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    #[test]
    fn detect_expected_self_lethal_sets_last_stand_mode() {
        // 3-HP actor leaves adjacency with a 5-expected-dmg melee enemy:
        // AoO EV is well above HP → plan gets mode=LastStand under
        // non-ProtectSelf intent.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .aoo(0.0, 0) // self has no AoO; irrelevant here
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let mut plans = vec![move_plan(vec![hex_from_offset(-1, 0)])];
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut raw = vec![PlanFactorValues::default()];
        let mut scored = vec![0.5];
        let intent = TacticalIntent::Reposition;
        let adaptation = apply_adaptation(&mut plans, &mut raw, &mut scored, &intent, &ctx);

        assert!(matches!(adaptation.modes[0], EvaluationMode::LastStand));
        assert!(matches!(
            adaptation.reasons[0],
            Some(AdaptationReason::ExpectedSelfLethal { .. })
        ));
    }

    #[test]
    fn expected_self_lethal_gated_off_under_protect_self() {
        // Intent=ProtectSelf with at least one defensive option in the pool
        // (empty plan = defensive by convention, see sanity::plan_is_defensive).
        // Contract takes priority over trade: the self-lethal Move plan is a
        // contract violation, not a trade — adaptation leaves its mode at
        // Default so the contract mask masks it downstream.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let empty_defensive = TurnPlan {
            steps: Vec::new(),
            final_pos: hex_from_offset(0, 0),
            residual_ap: 1,
            residual_mp: 3,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let lethal_move = move_plan(vec![hex_from_offset(-1, 0)]);
        let mut plans = vec![empty_defensive, lethal_move];
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // raw[0] is the empty defensive plan — self_survival ≥ ε so spatial
        // check passes (any_defensive=true) and ExpectedSelfLethal is gated off.
        let mut raw = vec![
            { let mut f = PlanFactorValues::default(); f.set_plan(PlanFactor::SelfSurvival, 0.2); f },
            PlanFactorValues::default(),
        ];
        let mut scored = vec![0.5, 0.5];
        let adaptation = apply_adaptation(
            &mut plans, &mut raw, &mut scored, &TacticalIntent::ProtectSelf, &ctx,
        );

        assert!(
            matches!(adaptation.modes[0], EvaluationMode::Default)
                && matches!(adaptation.modes[1], EvaluationMode::Default),
            "ExpectedSelfLethal must not fire under ProtectSelf when defensive options exist",
        );
        assert!(adaptation.reasons[0].is_none() && adaptation.reasons[1].is_none());
    }

    #[test]
    fn protect_self_no_defensive_switches_all_plans_globally() {
        // ProtectSelf intent, but every plan is non-defensive — here a
        // Move into a strictly more dangerous tile (danger map peaks at
        // the destination). `plan_is_defensive` returns false because
        // the move destination is not safer than the actor's current
        // tile. All plans flip to LastStand mode with the global reason.
        let pos = hex_from_offset(0, 0);
        let danger_tile = hex_from_offset(3, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(5).build();
        let mut plans = vec![move_plan(vec![danger_tile])];
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let mut maps = empty_maps();
        maps.danger.add(danger_tile, 2.0);
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut raw = vec![PlanFactorValues::default()];
        let mut scored = vec![0.5];
        let adaptation = apply_adaptation(
            &mut plans, &mut raw, &mut scored, &TacticalIntent::ProtectSelf, &ctx,
        );

        assert!(matches!(adaptation.modes[0], EvaluationMode::LastStand));
        assert!(matches!(
            adaptation.reasons[0],
            Some(AdaptationReason::ProtectSelfNoDefensive)
        ));
    }

    // ── ProtectSelfFutile: DoT doom, rescue feasibility ─────────────────
    //
    // End-of-turn horizon: `plan_has_self_rescue` reads
    // `sim_snapshots.last()`. In a turn-based engine external state
    // doesn't change during an actor's turn, so the post-plan snapshot
    // correctly answers "will this actor be alive when his turn ends".
    // Tests construct `sim_snapshots` by hand — we're exercising
    // adaptation logic, not the generator's sim.

    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType,
    };
    use crate::content::statuses::StatusDef;
    use crate::core::{AbilityId, DiceExpr, StatusId};

    /// Minimal self-heal AbilityDef for content injection in rescue tests.
    /// TargetType::SingleAlly makes `plan_is_defensive` return true for a
    /// first-step Cast, so the contract's spatial check passes and the
    /// doom/rescue branch actually runs.
    fn heal_def() -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("heal"),
            name: "heal".into(),
            target_type: TargetType::SingleAlly,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::Heal { dice: DiceExpr::new(1, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        }
    }

    /// Minimal StatusDef with only the fields pending_dot_before_next_action
    /// reads from content (`hp_percent_dot`). All other fields default.
    fn dot_status(id: &str, hp_percent_dot: i32) -> StatusDef {
        StatusDef {
            id: StatusId::from(id),
            name: id.into(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: false,
            forces_targeting: false,
            dot_dice: None as Option<DiceExpr>,
            blocks_mana_abilities: false,
            speed_bonus: 0,
            hp_percent_dot,
            ai_controlled: false,
            causes_disadvantage: false,
            buff_class: None,
        }
    }

    /// Single-Cast "rescue" plan whose post-step snapshot reflects the
    /// given actor state. Real generator emits one snapshot per step; we
    /// pair a placeholder Cast step with the injected post-state so the
    /// `sim_snapshots.len() == steps.len()` shape invariant holds.
    fn rescue_plan(actor_post: UnitSnapshot) -> TurnPlan {
        let post_snap = BattleSnapshot::new(vec![actor_post.clone()], 1);
        TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: crate::core::AbilityId::from("heal"),
                target: actor_post.entity,
                target_pos: actor_post.pos,
            }],
            final_pos: actor_post.pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![crate::combat::ai::planning::types::StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: vec![post_snap],
            annotation: Default::default(),
        }
    }

    /// Empty-steps "skip" plan. Deserialized-like: `sim_snapshots` is
    /// empty, so `plan_has_self_rescue` falls back to the initial
    /// snapshot — which for a doomed actor encodes "no rescue".
    fn skip_plan(actor_pos: Hex) -> TurnPlan {
        TurnPlan {
            steps: Vec::new(),
            final_pos: actor_pos,
            residual_ap: 1,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    #[test]
    fn doom_no_rescue_flips_all_plans_to_protect_self_futile() {
        // #13-class scenario: hp=2, poison dot=4 → pending >= hp → doomed.
        // Only plan = "skip" which leaves actor state unchanged → no rescue.
        // Gate fires: all plans → LastStand w/ ProtectSelfFutile.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(2)
            .max_hp(20)
            .build();
        let mut actor_with_dot = actor.clone();
        actor_with_dot.statuses.push(crate::combat::ai::world::snapshot::ActiveStatusView {
            id: StatusId::from("poison"),
            rounds_remaining: 1,
            dot_per_tick: 4,
        });

        let mut plans = vec![skip_plan(actor_with_dot.pos)];
        let snap = BattleSnapshot::new(vec![actor_with_dot.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor_with_dot);

        // self_survival ≥ ε so spatial check (any_defensive) passes and the
        // doom/rescue branch is reached. The skip plan has no rescue → Futile.
        let mut raw = vec![{ let mut f = PlanFactorValues::default(); f.set_plan(PlanFactor::SelfSurvival, 0.2); f }];
        let mut scored = vec![0.5];
        let adaptation = apply_adaptation(
            &mut plans, &mut raw, &mut scored, &TacticalIntent::ProtectSelf, &ctx,
        );

        assert!(matches!(adaptation.modes[0], EvaluationMode::LastStand));
        match &adaptation.reasons[0] {
            Some(AdaptationReason::ProtectSelfFutile { pending_dot, actor_hp }) => {
                assert_eq!(*pending_dot, 4);
                assert_eq!(*actor_hp, 2);
            }
            other => panic!("expected ProtectSelfFutile, got {:?}", other),
        }
    }

    #[test]
    fn doom_with_self_heal_rescue_leaves_default_mode() {
        // Critic guardrail: actor has self-heal. The rescue plan's
        // post-state has HP above pending DoT → any_rescue=true →
        // adaptation must NOT flip to LastStand. Contract (ProtectSelf)
        // stays, and the rescue plan wins via the contract mask downstream.
        let pos = hex_from_offset(0, 0);
        let mut actor_doomed = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(2)
            .max_hp(20)
            .build();
        actor_doomed.statuses.push(crate::combat::ai::world::snapshot::ActiveStatusView {
            id: StatusId::from("poison"),
            rounds_remaining: 1,
            dot_per_tick: 4,
        });
        // Post-plan: self-heal raises HP to 12, DoT still pending (heal
        // didn't cleanse). 12 > 4 → rescue holds.
        let mut actor_healed = actor_doomed.clone();
        actor_healed.hp = 12;

        let mut plans = vec![rescue_plan(actor_healed)];
        let snap = BattleSnapshot::new(vec![actor_doomed.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let mut content = empty_content();
        let def = heal_def();
        content.abilities.insert(def.id.clone(), def);
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor_doomed);

        // self_survival ≥ ε so spatial check passes; rescue plan heals above
        // pending DoT → any_rescue=true → contract holds.
        let mut raw = vec![{ let mut f = PlanFactorValues::default(); f.set_plan(PlanFactor::SelfSurvival, 0.2); f }];
        let mut scored = vec![0.5];
        let adaptation = apply_adaptation(
            &mut plans, &mut raw, &mut scored, &TacticalIntent::ProtectSelf, &ctx,
        );

        assert!(
            matches!(adaptation.modes[0], EvaluationMode::Default),
            "self-heal enough to outpace DoT must keep contract alive",
        );
        assert!(adaptation.reasons[0].is_none());
    }

    #[test]
    fn doom_with_cleanse_rescue_leaves_default_mode() {
        // Cleanse path: post-plan actor has hp=2 (not healed) but DoT
        // status removed. pending_dot post = 0, actor hp > 0 → rescued.
        let pos = hex_from_offset(0, 0);
        let mut actor_doomed = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(2)
            .max_hp(20)
            .build();
        actor_doomed.statuses.push(crate::combat::ai::world::snapshot::ActiveStatusView {
            id: StatusId::from("poison"),
            rounds_remaining: 1,
            dot_per_tick: 4,
        });
        // Post-plan: statuses vec cleared (cleanse). HP unchanged.
        let mut actor_cleansed = actor_doomed.clone();
        actor_cleansed.statuses.clear();

        let mut plans = vec![rescue_plan(actor_cleansed)];
        let snap = BattleSnapshot::new(vec![actor_doomed.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let mut content = empty_content();
        let def = heal_def();
        content.abilities.insert(def.id.clone(), def);
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor_doomed);

        // self_survival ≥ ε so spatial check passes; cleanse drops pending
        // DoT to 0 → any_rescue=true → contract holds.
        let mut raw = vec![{ let mut f = PlanFactorValues::default(); f.set_plan(PlanFactor::SelfSurvival, 0.2); f }];
        let mut scored = vec![0.5];
        let adaptation = apply_adaptation(
            &mut plans, &mut raw, &mut scored, &TacticalIntent::ProtectSelf, &ctx,
        );

        assert!(
            matches!(adaptation.modes[0], EvaluationMode::Default),
            "cleanse that drops pending DoT below hp must keep contract alive",
        );
    }

    #[test]
    fn pending_dot_includes_hp_percent_dot_from_content() {
        // Status has no per-tick flat damage but has hp_percent_dot=20
        // on a 40-max-hp actor → 8 per tick. Actor hp=5 → pending >= hp
        // even though `dot_per_tick` alone reports 0.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(5)
            .max_hp(40)
            .build();
        let mut actor_sick = actor.clone();
        actor_sick.statuses.push(crate::combat::ai::world::snapshot::ActiveStatusView {
            id: StatusId::from("exhaustion"),
            rounds_remaining: 1,
            dot_per_tick: 0, // the flat-damage channel is empty
        });
        let mut content = empty_content();
        content.statuses.insert(
            StatusId::from("exhaustion"),
            dot_status("exhaustion", 20), // 20% of max_hp=40 → 8 per tick
        );
        let pending = pending_dot_before_next_action(&actor_sick, &content);
        assert_eq!(pending, 8, "hp_percent_dot must contribute to pending");
        assert!(pending >= actor_sick.hp, "doom holds via %hp DoT alone");
    }

    #[test]
    fn adaptation_is_idempotent_on_second_call() {
        // Running apply_adaptation twice in sequence must not churn state.
        // Second call re-detects the same facts, re-writes the same
        // mode/reason, triggers the same rescore that produces the same
        // `raw[i].intent` value. Final `scored` equal across the two.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let mut plans = vec![move_plan(vec![hex_from_offset(-1, 0)])];
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut raw = vec![PlanFactorValues::default()];
        let mut scored = vec![0.5];
        let intent = TacticalIntent::Reposition;

        let _ = apply_adaptation(&mut plans, &mut raw, &mut scored, &intent, &ctx);
        let after_first = scored.clone();
        let raw_after_first = raw.clone();

        let _ = apply_adaptation(&mut plans, &mut raw, &mut scored, &intent, &ctx);

        assert_eq!(after_first, scored, "scored stable across a second call");
        assert_eq!(
            raw_after_first[0].get_plan(PlanFactor::Intent),
            raw[0].get_plan(PlanFactor::Intent),
            "intent-column stable across a second call",
        );
    }

    #[test]
    fn default_plans_untouched_when_no_trigger_fires() {
        // Actor at full HP, no adjacent enemy, intent=Reposition, plan
        // is a harmless move. No AdaptationReason applies — scored/raw
        // unchanged, mode stays Default.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20)
            .build();
        let mut plans = vec![move_plan(vec![hex_from_offset(1, 1)])];
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut raw = vec![PlanFactorValues::default()];
        let scored_before = vec![0.5];
        let mut scored = scored_before.clone();
        let adaptation = apply_adaptation(
            &mut plans, &mut raw, &mut scored, &TacticalIntent::Reposition, &ctx,
        );

        assert!(matches!(adaptation.modes[0], EvaluationMode::Default));
        assert!(adaptation.reasons[0].is_none());
        assert!(!adaptation.any_adapted());
        // Empty adaptation path skips rescore; scored must equal input.
        assert_eq!(scored, scored_before);
    }

    #[test]
    fn sanity_no_longer_masks_expected_lethal_aoo() {
        // Regression: before MVP1, `sanity_adjust_plans` wrote -∞ when
        // `aoo_dmg >= hp`. The adaptation layer now owns that case;
        // sanity must stay in its "soft multipliers only" lane.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let plans = vec![
            // First plan: a no-op so sanity has a 2nd entry to compare
            // against (sanity_adjust_plans short-circuits on len<=1).
            TurnPlan {
                steps: Vec::new(),
                final_pos: hex_from_offset(0, 0),
                residual_ap: 0,
                residual_mp: 0,
                outcomes: Vec::new(),
                partial_score: 0.0,
                sim_snapshots: Vec::new(),
                annotation: Default::default(),
            },
            move_plan(vec![hex_from_offset(-1, 0)]),
        ];
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut scored = vec![1.0, 1.0];
        let _ = sanity_adjust_plans(&mut scored, &plans, &ctx);

        assert!(
            scored[1].is_finite(),
            "sanity must not produce -inf for expected-lethal AoO (got {})",
            scored[1],
        );
        // The non-lethal bleed penalty still floors at 0.25 × input.
        assert!(scored[1] >= 0.25, "soft AoO bleed floor holds");
    }
}
