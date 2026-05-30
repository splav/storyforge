    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::scoring::factors::PlanFactorValues;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;

    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, PoolBuilder,
        StageTestHarness, UnitBuilder,
        snapshot_from,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use combat_engine::DiceRng;

    // ── run_pick: no trace_base_eq_score — PickBest does not read score_trace ─

    fn run_pick(scores: Vec<f32>) -> ScoredPool {
        let n = scores.len();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default(); n];
        let h = StageTestHarness::new(actor);
        let mut pool = PoolBuilder::new(plans)
            .scores(&scores)
            .build();
        h.run(|ctx| PickBestStage.apply(&mut pool, ctx));
        pool
    }

    #[test]
    fn pick_best_marks_exactly_one_chosen() {
        let pool = run_pick(vec![0.3, 0.8, 0.5]);
        let chosen_count = pool.annotations.iter().filter(|a| a.chosen).count();
        assert_eq!(chosen_count, 1, "exactly one plan must be chosen");
    }

    #[test]
    fn pick_best_selects_highest_score() {
        // With deterministic DiceRng seed and no mercy margin (default difficulty),
        // the highest-scored plan should be chosen.
        let pool = run_pick(vec![0.1, 0.9, 0.4]);
        // Index 1 has the highest score.
        assert!(pool.annotations[1].chosen, "highest-scored plan should be chosen");
        assert!(pool.annotations[1].pick.is_some(), "chosen plan should have PickInfo");
    }

    #[test]
    fn pick_best_noop_on_empty_pool() {
        let pool = run_pick(vec![]);
        assert_eq!(pool.len(), 0);
    }

    // ── apply_pick_jitter tests ───────────────────────────────────────────────

    /// Build a pool with given scores and run apply_pick_jitter.
    /// Returns (noise_vec, post_scores) where post_scores[i] is score post-jitter.
    /// Kept inline: requires a custom DifficultyProfile — harness always uses default.
    fn run_jitter(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        difficulty: &DifficultyProfile,
    ) -> (Vec<f32>, Vec<f32>) {
        assert_eq!(plans.len(), scores.len());
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let reservations = Reservations::default();
        let mut rng = DiceRng::default();

        let world = make_test_ctx(&content, difficulty);
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );
        let mut pool = ScoredPool::new(plans);
        for (ann, s) in pool.annotations.iter_mut().zip(scores.iter()) {
            ann.score = *s;
        }
        let noise = apply_pick_jitter(&mut pool, &ctx);
        let post_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
        (noise, post_scores)
    }

    /// score_noise = 0.0 (normal difficulty) → jitter returns all-zeros, scores unchanged.
    #[test]
    fn pick_jitter_no_op_when_noise_amp_zero() {
        let difficulty = DifficultyProfile::normal();
        assert_eq!(difficulty.score_noise(), 0.0, "precondition");

        let plans = vec![TurnPlan::default(); 3];
        let scores = vec![0.1_f32, 0.5, 0.3];
        let (noise, post_scores) = run_jitter(plans, scores.clone(), &difficulty);

        assert_eq!(noise, vec![0.0_f32; 3], "noise vec must be all zeros");
        assert_eq!(post_scores, scores, "scores must be unchanged");
    }

    /// Plans with score = -inf (masked) must have noise[i] = 0.0 and score unchanged.
    #[test]
    fn pick_jitter_skips_masked_plans() {
        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let plans = vec![TurnPlan::default(); 3];
        // Middle plan is masked.
        let scores = vec![0.5_f32, f32::NEG_INFINITY, 0.3];
        let (noise, post_scores) = run_jitter(plans, scores, &difficulty);

        assert_eq!(noise[1], 0.0, "masked plan noise must be zero");
        assert_eq!(post_scores[1], f32::NEG_INFINITY, "masked plan score must be unchanged");
        // Non-masked plans get non-zero noise (deterministic, may be any value).
        // Just verify they're finite.
        assert!(post_scores[0].is_finite(), "plan 0 score should be finite");
        assert!(post_scores[2].is_finite(), "plan 2 score should be finite");
    }

    /// Noise is order-invariant: same plan in position 0 or 1 gets the same noise value.
    /// Migrates the invariant tested in `scorer.rs::noise_is_plan_order_invariant`.
    #[test]
    fn pick_jitter_is_plan_order_invariant() {
        use crate::combat::ai::plan::types::{PlanStep, StepOutcome};

        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let pos_a = hex_from_offset(3, 0);
        let pos_b = hex_from_offset(2, 0);

        // Two distinct plans targeting different positions (different canonical hash).
        let mk_plan = |target_pos: crate::game::hex::Hex| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: bevy::prelude::Entity::from_raw_u32(99).expect("valid"),
                target_pos,
            }],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let plan_a = mk_plan(pos_a);
        let plan_b = mk_plan(pos_b);

        let scores = vec![0.5_f32, 0.5];

        // Order AB.
        let (noise_ab, _) = run_jitter(vec![plan_a.clone(), plan_b.clone()], scores.clone(), &difficulty);
        // Order BA.
        let (noise_ba, _) = run_jitter(vec![plan_b.clone(), plan_a.clone()], scores.clone(), &difficulty);

        // noise_ab[0] = noise for plan_a; noise_ba[1] = noise for plan_a.
        assert_eq!(
            noise_ab[0], noise_ba[1],
            "plan_a noise must not depend on pool position",
        );
        assert_eq!(
            noise_ab[1], noise_ba[0],
            "plan_b noise must not depend on pool position",
        );
    }

    /// Winner's PickInfo.noise_applied is populated with the actual noise value
    /// when score_noise > 0.
    #[test]
    fn pick_jitter_records_noise_applied_in_pick_info() {
        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let n = 3;
        let plans = vec![TurnPlan::default(); n];
        let scores = [0.1_f32, 0.5, 0.3];

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let reservations = Reservations::default();
        let mut rng = DiceRng::default();

        let world = make_test_ctx(&content, &difficulty);
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(plans);
        for (ann, s) in pool.annotations.iter_mut().zip(scores.iter()) {
            ann.score = *s;
            ann.factors = PlanFactorValues::default();
        }
        PickBestStage.apply(&mut pool, &mut ctx);

        let winner = pool.annotations.iter().find(|a| a.chosen).expect("winner must exist");
        let pi = winner.pick.as_ref().expect("winner must have PickInfo");
        assert_ne!(pi.noise_applied, 0.0, "noise_applied must be non-zero under easy difficulty");
    }

    // ── Step 11.4: per-agenda-item composition tests ──────────────────────────

    use crate::combat::ai::intent::agenda::{Agenda, AgendaItem};
    use crate::combat::ai::intent::bands::PriorityBand;
    use crate::combat::ai::intent::considerations::IntentConsiderations;
    use crate::combat::ai::intent::IntentKind;
    use crate::combat::ai::outcome::PerItemEval;

    fn agenda_item_with_considerations(
        kind: IntentKind,
        considerations: IntentConsiderations,
    ) -> AgendaItem {
        AgendaItem {
            kind,
            target: None,
            raw_score: 0.5,
            reason: IntentReason::NoRuleDefault,
            considerations,
        }
    }

    fn uniform_considerations() -> IntentConsiderations {
        IntentConsiderations {
            urgency: 1.0,
            feasibility: 1.0,
            leverage: 1.0,
            safety: 1.0,
            role_affinity: 1.0,
            continuation_value: 1.0,
        }
    }

    fn zero_considerations() -> IntentConsiderations {
        IntentConsiderations {
            urgency: 0.0,
            feasibility: 0.0,
            leverage: 0.0,
            safety: 0.0,
            role_affinity: 0.0,
            continuation_value: 0.0,
        }
    }

    /// Reconstruct the `w_intent` value that `PickBestStage` uses, mirroring
    /// the same path: default actor role weights × `intent_commitment`.
    /// Used to write exact-value assertions for cdot bonus.
    fn expected_w_intent() -> f32 {
        use crate::combat::ai::scoring::factors::{PlanFactor, StepFactor};
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut weights = if scoring.last_goal.is_some() {
            actor.role.factor_weights_continuation(world.tuning)
        } else {
            actor.role.factor_weights(world.tuning)
        };
        let slot = StepFactor::count() + PlanFactor::Intent as usize;
        weights[slot] *= world.difficulty.intent_commitment;
        weights[slot]
    }

    /// Build a pool with per_item data and run PickBest with an agenda.
    fn run_pick_with_agenda(
        pre_scores: Vec<f32>,
        score_initials: Vec<f32>,
        per_items: Vec<Vec<PerItemEval>>,
        agenda: &Agenda,
    ) -> ScoredPool {
        let n = pre_scores.len();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let plans = vec![TurnPlan::default(); n];
        let mut h = StageTestHarness::new(actor);
        h.agenda = Some(agenda.clone());
        let mut pool = PoolBuilder::new(plans)
            .scores(&pre_scores)
            .score_initials(&score_initials)
            .per_items(per_items)
            .build();
        h.run(|ctx| PickBestStage.apply(&mut pool, ctx));
        pool
    }

    /// Single item, uniform considerations, item intent==primary intent (both 0).
    /// intent_delta = 0, tempo_delta = 0.
    /// composed = score_initial + 0 + 0 + w_intent × cdot.
    /// Test pins this explicit form (additive, not multiplicative).
    #[test]
    fn composition_collapses_to_base_when_considerations_uniform() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![agenda_item_with_considerations(
                IntentKind::Reposition,
                uniform_considerations(),
            )],
        };
        let initial = 0.75_f32;
        let per_items = vec![vec![PerItemEval {
            intent_factor: 0.0, // same as primary (all factors default to 0)
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        }]];
        let pool = run_pick_with_agenda(
            vec![initial],
            vec![initial],
            per_items,
            &agenda,
        );
        assert!(pool.annotations[0].chosen, "sole plan should be chosen");
        // Score must be finite and > score_initial because cdot > 0 with uniform considerations.
        let post_score = pool.annotations[0].score;
        assert!(post_score.is_finite(), "composed score must be finite");
        assert!(
            post_score >= initial,
            "composed = initial + W×cdot ≥ initial (cdot≥0), got {post_score} vs initial {initial}"
        );
    }

    /// Two plans, two items: argmax selects the item with highest composed score.
    /// Plan 1 has item 1 ineligible (FocusTarget mask); item 0 is eligible.
    /// Plan 0 has both items eligible; item 1 gives better cdot → attributed to item 1.
    #[test]
    fn multi_item_pick_attributes_to_winning_item() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(
                    IntentKind::Reposition,
                    IntentConsiderations { urgency: 0.1, feasibility: 1.0, leverage: 0.1, safety: 1.0, role_affinity: 0.1, continuation_value: 0.1 },
                ),
                agenda_item_with_considerations(
                    IntentKind::Reposition,
                    IntentConsiderations { urgency: 0.9, feasibility: 1.0, leverage: 0.9, safety: 1.0, role_affinity: 0.9, continuation_value: 0.9 },
                ),
            ],
        };
        // Plan 0 with both items eligible; item 1 has much higher considerations.
        let per_items = vec![
            vec![
                PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: IntentConsiderations { urgency: 0.1, feasibility: 1.0, leverage: 0.1, safety: 1.0, role_affinity: 0.1, continuation_value: 0.1 } },
                PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: IntentConsiderations { urgency: 0.9, feasibility: 1.0, leverage: 0.9, safety: 1.0, role_affinity: 0.9, continuation_value: 0.9 } },
            ],
        ];
        let pool = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            per_items,
            &agenda,
        );
        assert!(pool.annotations[0].chosen, "sole plan should be chosen");
        assert_eq!(
            pool.annotations[0].agenda_item,
            Some(1),
            "plan should be attributed to item 1 (higher cdot)"
        );
    }

    /// Empty agenda → legacy path: annotation.agenda_item stays None, chosen set normally.
    #[test]
    fn empty_agenda_falls_back_to_legacy_pipeline() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![], // empty
        };
        let pool = run_pick_with_agenda(
            vec![0.4, 0.9],
            vec![0.4, 0.9],
            vec![vec![], vec![]],  // empty per_item
            &agenda,
        );
        // Winner should be plan 1 (highest pre_score, no composition).
        assert!(pool.annotations[1].chosen, "highest-score plan should win in legacy path");
        // No agenda attribution.
        assert!(
            pool.annotations[1].agenda_item.is_none(),
            "agenda_item should be None in legacy (empty agenda) path"
        );
    }

    /// agenda_item attribution is written into the winning plan's annotation.
    #[test]
    fn agenda_item_attribution_persisted_in_annotation() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        let per_items = vec![vec![PerItemEval {
            intent_factor: 0.0,
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        }]];
        let pool = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            per_items,
            &agenda,
        );
        assert!(pool.annotations[0].chosen, "plan should be chosen");
        assert_eq!(
            pool.annotations[0].agenda_item,
            Some(0),
            "agenda_item should be attributed to item index 0"
        );
    }


    // ── Step 11.4: new additive composition tests ─────────────────────────────

    /// Pins the main mathematical bug: two plans with different `score_initial`
    /// but identical `intent_factor`, `tempo_factor`, and `cdot` must produce
    /// composed scores that differ by exactly `score_initial_a - score_initial_b`.
    /// (The ratio bug would make the difference scale with score_initial.)
    #[test]
    fn item_score_does_not_scale_with_unrelated_base_score() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![agenda_item_with_considerations(
                IntentKind::Reposition,
                uniform_considerations(),
            )],
        };
        let same_per_item = PerItemEval {
            intent_factor: 0.0,
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        };

        // Plan A: initial=0.2, Plan B: initial=2.0. Identical intent/tempo/cdot.
        let pool = run_pick_with_agenda(
            vec![0.2, 2.0],
            vec![0.2, 2.0],
            vec![vec![same_per_item], vec![same_per_item]],
            &agenda,
        );
        // intent_delta = tempo_delta = 0 (same intent primary as item factor).
        // cdot is the same for both (same considerations + same band weights).
        // composed_A = 0.2 + w_intent*cdot
        // composed_B = 2.0 + w_intent*cdot
        // diff = 2.0 - 0.2 = 1.8 exactly.
        let score_a = pool.annotations[0].score;
        let score_b = pool.annotations[1].score;
        assert!(
            (score_b - score_a - 1.8_f32).abs() < 1e-4,
            "score diff must equal initial diff (1.8), got {}", score_b - score_a
        );
    }

    /// An ineligible item must be skipped; argmax chooses the next eligible item.
    #[test]
    fn ineligible_item_is_skipped_in_argmax() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, zero_considerations()),
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        let per_items = vec![vec![
            PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: false, reject_reason: None, considerations: zero_considerations() },
            PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: uniform_considerations() },
        ]];
        let pool = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            per_items,
            &agenda,
        );
        assert!(pool.annotations[0].chosen);
        assert_eq!(
            pool.annotations[0].agenda_item,
            Some(1),
            "ineligible item 0 must be skipped; argmax selects item 1"
        );
    }

    /// cdot bonus equals exactly `w_intent × weighted_dot(considerations, weights)`.
    /// Pins the additive formula by reconstructing w_intent the same way
    /// `PickBestStage` does and comparing to observed delta within 1e-4.
    #[test]
    fn cdot_changes_score_additively_with_intent_weight() {
        let agenda_zero = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, zero_considerations()),
            ],
        };
        let agenda_full = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };

        let pool_zero = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            vec![vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true, reject_reason: None, considerations: zero_considerations() }]],
            &agenda_zero,
        );
        let pool_full = run_pick_with_agenda(
            vec![0.5],
            vec![0.5],
            vec![vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true, reject_reason: None, considerations: uniform_considerations() }]],
            &agenda_full,
        );

        let cdot_delta = pool_full.annotations[0].score - pool_zero.annotations[0].score;
        let expected = expected_w_intent()
            * uniform_considerations().weighted_dot(&PriorityBand::NormalTactical.weights());

        assert!(
            (cdot_delta - expected).abs() < 1e-4,
            "cdot delta must equal w_intent × weighted_dot exactly: expected {expected}, got {cdot_delta}"
        );
    }

    /// A plan with a band-eligible item beats a fallback (no eligible items) plan
    /// when both start with the same initial score.
    #[test]
    fn attributed_plan_beats_fallback_plan_with_equal_initial_score() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        // Plan A: has one eligible item with uniform cdot.
        // Plan B: no eligible items → fallback (score stays at pipeline score = score_initial).
        let per_items = vec![
            vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: uniform_considerations() }],
            vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: false, reject_reason: None, considerations: uniform_considerations() }],
        ];
        let pool = run_pick_with_agenda(
            vec![0.5, 0.5],  // equal pipeline scores
            vec![0.5, 0.5],  // equal initials
            per_items,
            &agenda,
        );
        // Plan A: composed = 0.5 + 0 + 0 + w_intent*cdot  (cdot > 0 → composed > 0.5)
        // Plan B: fallback → score stays 0.5.
        // Plan A must win.
        assert!(
            pool.annotations[0].chosen,
            "attributed plan (eligible item) must beat fallback plan with equal initial score"
        );
    }

    /// A fallback plan with much higher initial score beats an attributed plan
    /// with low cdot. Pins that W×cdot is bounded and cannot override large signals.
    #[test]
    fn fallback_plan_can_beat_attributed_plan_with_low_cdot_when_initial_dominates() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, zero_considerations()),
            ],
        };
        // Plan A: fallback (no eligible items), high initial = 2.0.
        // Plan B: attributed (cdot=0), initial = 1.0.
        //   composed_B = 1.0 + 0 + 0 + w_intent*0 = 1.0
        //   Plan A score = 2.0 (fallback, pipeline value)
        let per_items = vec![
            vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: false, reject_reason: None, considerations: zero_considerations() }],
            vec![PerItemEval { intent_factor: 0.0, tempo_factor: 0.0, eligible: true,  reject_reason: None, considerations: zero_considerations() }],
        ];
        let pool = run_pick_with_agenda(
            vec![2.0, 1.0],
            vec![2.0, 1.0],
            per_items,
            &agenda,
        );
        assert!(
            pool.annotations[0].chosen,
            "fallback plan with initial=2.0 must beat attributed plan with initial=1.0 and cdot=0"
        );
    }

    /// Single-item agenda where item intent matches primary: intent_delta = 0,
    /// tempo_delta = 0, so composed must equal exactly `initial + W × cdot`.
    #[test]
    fn composed_equals_initial_plus_cdot_when_intent_matches_primary() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        let initial = 0.4_f32;
        // item intent = primary intent = 0, tempo = 0 → both deltas = 0.
        let per_items = vec![vec![PerItemEval {
            intent_factor: 0.0,
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        }]];
        let pool = run_pick_with_agenda(
            vec![initial],
            vec![initial],
            per_items,
            &agenda,
        );
        let composed = pool.annotations[0].score;
        let expected = initial
            + expected_w_intent()
                * uniform_considerations().weighted_dot(&PriorityBand::NormalTactical.weights());
        assert!(
            (composed - expected).abs() < 1e-4,
            "composed must equal initial + W × cdot exactly: expected {expected}, got {composed}"
        );
    }

    /// Intent delta is the same for two plans with different score_initial
    /// but identical per-item intent values. Pins additive (not multiplicative) intent scaling.
    #[test]
    fn intent_delta_is_identical_for_different_base_scores() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![
                agenda_item_with_considerations(IntentKind::Reposition, uniform_considerations()),
            ],
        };
        // Both plans: item intent_factor = 1.0 (different from primary which is 0.0).
        // Batch stats: both plans have intent_factor=1.0 → stats_intent from per_item.
        // But plan A initial=0.2, plan B initial=2.0.
        // intent_delta = factor_contrib(1.0, stats, signed, w) - factor_contrib(0.0, stats, signed, w).
        // Same for both since factors are identical.
        let same_per_item = PerItemEval {
            intent_factor: 1.0,
            tempo_factor: 0.0,
            eligible: true,
            reject_reason: None,
            considerations: uniform_considerations(),
        };
        let pool = run_pick_with_agenda(
            vec![0.2, 2.0],
            vec![0.2, 2.0],
            vec![vec![same_per_item], vec![same_per_item]],
            &agenda,
        );
        let score_a = pool.annotations[0].score;
        let score_b = pool.annotations[1].score;
        // composed_A = 0.2 + intent_delta + W*cdot
        // composed_B = 2.0 + intent_delta + W*cdot
        // diff = 1.8 (intent_delta cancels out — same for both plans).
        assert!(
            (score_b - score_a - 1.8_f32).abs() < 1e-4,
            "intent_delta must be identical for both plans; diff must equal initial diff (1.8), got {}",
            score_b - score_a
        );
    }

    /// With non-zero noise and two plans with equal pre-noise score, the winner
    /// is determined by the jitter — not by insertion order. Jitter runs before argmax.
    #[test]
    fn pipeline_pick_runs_jitter_before_argmax() {
        use crate::combat::ai::plan::types::{PlanStep, StepOutcome};

        let difficulty = DifficultyProfile::easy();
        assert!(difficulty.score_noise() > 0.0, "precondition");

        let pos_a = hex_from_offset(3, 0);
        let pos_b = hex_from_offset(2, 0);

        let mk_plan = |target_pos: crate::game::hex::Hex| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: "melee_attack".into(),
                target: bevy::prelude::Entity::from_raw_u32(99).expect("valid"),
                target_pos,
            }],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 1,
            residual_mp: 3,
            outcomes: vec![StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let plan_a = mk_plan(pos_a);
        let plan_b = mk_plan(pos_b);

        let pre_noise_score = 0.5_f32;

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let reservations = Reservations::default();
        let mut rng = DiceRng::default();

        let world = make_test_ctx(&content, &difficulty);
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(vec![plan_a, plan_b]);
        for ann in pool.annotations.iter_mut() {
            ann.score = pre_noise_score;
            ann.factors = PlanFactorValues::default();
        }
        PickBestStage.apply(&mut pool, &mut ctx);

        // Exactly one winner.
        let chosen: Vec<usize> = pool.annotations.iter().enumerate()
            .filter(|(_, a)| a.chosen)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(chosen.len(), 1, "exactly one plan chosen");

        // Winner must have non-zero noise_applied (jitter ran).
        let winner = &pool.annotations[chosen[0]];
        let pi = winner.pick.as_ref().expect("winner has PickInfo");
        assert_ne!(pi.noise_applied, 0.0, "noise_applied must reflect jitter contribution");
    }

    // ── commit_plan tests (consolidated from planning/picker.rs) ──────────────

    mod commit_plan_tests {
        use super::*;
        use crate::combat::ai::test_helpers::ent;
        use combat_engine::AbilityId;
        use crate::game::hex::hex_from_offset;

        fn plan_from(steps: Vec<PlanStep>) -> TurnPlan {
            TurnPlan {
                steps,
                final_pos: hex_from_offset(0, 0),
                residual_ap: 0,
                residual_mp: 0,
                outcomes: Vec::new(),
                partial_score: 0.0,
                sim_snapshots: Vec::new(),
                annotation: Default::default(),
            }
        }

        #[test]
        fn commit_empty_plan_ends_turn() {
            let (decision, consumed) = commit_plan(&plan_from(vec![]), hex_from_offset(0, 0));
            assert!(matches!(decision, AiDecision::EndTurn));
            assert_eq!(consumed, 0);
        }

        #[test]
        fn commit_solo_cast_consumes_one() {
            let plan = plan_from(vec![PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target: ent(1),
                target_pos: hex_from_offset(0, 0),
            }]);
            let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
            assert!(matches!(decision, AiDecision::CastInPlace { .. }));
            assert_eq!(consumed, 1);
        }

        #[test]
        fn commit_move_cast_bundles_into_two() {
            let plan = plan_from(vec![
                PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
                PlanStep::Cast {
                    ability: AbilityId::from("strike"),
                    target: ent(2),
                    target_pos: hex_from_offset(2, 0),
                },
            ]);
            let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
            match decision {
                AiDecision::MoveAndCast { path, ability, target, .. } => {
                    assert_eq!(path.len(), 1);
                    assert_eq!(ability.0, "strike");
                    assert_eq!(target, ent(2));
                }
                other => panic!("expected MoveAndCast, got {:?}", std::mem::discriminant(&other)),
            }
            assert_eq!(consumed, 2);
        }

        #[test]
        fn commit_solo_move_consumes_one() {
            let plan = plan_from(vec![PlanStep::Move { path: vec![hex_from_offset(1, 0)] }]);
            let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
            assert!(matches!(
                decision,
                AiDecision::Move { origin: MoveOrigin::BestPlan, .. }
            ));
            assert_eq!(consumed, 1);
        }

        #[test]
        fn commit_move_move_keeps_first_only_no_bundle() {
            let plan = plan_from(vec![
                PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
                PlanStep::Move { path: vec![hex_from_offset(2, 0)] },
            ]);
            let (_, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
            assert_eq!(consumed, 1, "Move→Move does not bundle");
        }
    }
