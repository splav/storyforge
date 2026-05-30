    use super::*;
    use bevy::prelude::Entity;

    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::scoring::factors::FactorTerminalScore;
    use crate::combat::ai::intent::agenda::{Agenda, AgendaItem};
    use crate::combat::ai::intent::bands::PriorityBand;
    use crate::combat::ai::intent::considerations::IntentConsiderations;
    use crate::combat::ai::intent::{IntentKind, IntentReason, TacticalIntent};
    use crate::combat::ai::outcome::{
        ActionOutcomeEstimate, PerItemEval, PlanAnnotation, ViabilityResult,
    };
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, StageTestHarness, UnitBuilder,
        snapshot_from,
    };
    use combat_engine::DiceRng;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn empty_agenda_item() -> AgendaItem {
        AgendaItem {
            kind: IntentKind::Reposition,
            target: None,
            raw_score: 0.5,
            reason: IntentReason::NoRuleDefault,
            considerations: IntentConsiderations {
                urgency: 0.7,
                feasibility: 0.5,
                leverage: 0.3,
                safety: 0.8,
                role_affinity: 0.6,
                continuation_value: 0.4,
            },
        }
    }

    fn run_overlay(
        plans: Vec<TurnPlan>,
        annotations: Vec<PlanAnnotation>,
        agenda: &Agenda,
    ) -> ScoredPool {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let mut h = StageTestHarness::new(actor);
        h.agenda = Some(agenda.clone());
        let mut pool = ScoredPool::new(plans);
        pool.annotations = annotations;
        h.run(|ctx| OverlayConsiderationsStage.apply(&mut pool, ctx));
        pool
    }

    /// Run overlay with a custom snap (e.g. containing specific target units).
    /// Kept inline: needs a multi-unit snap beyond what the harness builds.
    /// TODO: migrate to StageTestHarness in Phase 5 when harness gains snap injection.
    fn run_overlay_with_snap(
        plans: Vec<TurnPlan>,
        annotations: Vec<PlanAnnotation>,
        agenda: &Agenda,
        snap: BattleSnapshot,
    ) -> ScoredPool {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            actor.pos,
            &mut rng,
        )
        .with_agenda(agenda);
        let mut pool = ScoredPool::new(plans);
        pool.annotations = annotations;
        OverlayConsiderationsStage.apply(&mut pool, &mut ctx);
        pool
    }

    fn make_entity(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    fn ability_id() -> combat_engine::AbilityId {
        "test_ability".into()
    }

    fn make_cast_step(target: Entity) -> PlanStep {
        PlanStep::Cast {
            ability: ability_id(),
            target,
            target_pos: hex_from_offset(1, 0),
        }
    }

    // ── Existing tests (preserved) ────────────────────────────────────────────

    /// Continuous feasibility: passed=true, adjusted_score=1.0, margin=2.0 → 0.5.
    /// This replaces the old binary test (pre-11.8 clamped adjusted_score to [0,1];
    /// 11.8 introduces margin normalisation + !passed guard — see Section B).
    #[test]
    fn overlay_feasibility_is_continuous_adjusted_score() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };

        // passed=true, adjusted_score=1.0 → (1.0 - 0.0) / 2.0 = 0.5.
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let feasibility = pool.annotations[0].per_item[0].considerations.feasibility;
        assert!(
            (feasibility - 0.5).abs() < 1e-5,
            "feasibility=(1.0-0.0)/2.0=0.5, got {feasibility}"
        );
    }

    #[test]
    fn overlay_urgency_and_role_affinity_preserved_from_item() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 0.0 };
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let c = &pool.annotations[0].per_item[0].considerations;
        assert!(
            (c.urgency - 0.7).abs() < 1e-5,
            "urgency should be preserved from item (0.7), got {}", c.urgency
        );
        assert!(
            (c.role_affinity - 0.6).abs() < 1e-5,
            "role_affinity should be preserved from item (0.6), got {}", c.role_affinity
        );
    }

    #[test]
    fn overlay_noop_when_agenda_empty() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![],
        };
        let pool = run_overlay(
            vec![TurnPlan::default()],
            vec![PlanAnnotation::default()],
            &agenda,
        );
        assert!(
            pool.annotations[0].per_item.is_empty(),
            "empty agenda → per_item unchanged (empty)"
        );
    }

    /// Plan-aware overlay uses continuous adjusted_score for feasibility (11.8 formula).
    /// Two plans with different adjusted_score and both passed=true produce different
    /// feasibility values: (score - 0.0) / 2.0.
    #[test]
    fn plan_aware_overlay_changes_feasibility_axis() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::Reposition,
                target: None,
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations {
                    urgency: 0.5,
                    feasibility: 0.5,
                    leverage: 0.5,
                    safety: 0.5,
                    role_affinity: 0.5,
                    continuation_value: 0.5,
                },
            }],
        };

        // passed=true, adjusted_score=1.8 → (1.8-0.0)/2.0 = 0.9.
        let mut ann_high = PlanAnnotation::default();
        ann_high.viability = ViabilityResult { passed: true, adjusted_score: 1.8 };
        ann_high.per_item = vec![PerItemEval::default()];
        let pool_high = run_overlay(vec![TurnPlan::default()], vec![ann_high], &agenda);
        let feasibility_high =
            pool_high.annotations[0].per_item[0].considerations.feasibility;
        assert!(
            (feasibility_high - 0.9).abs() < 1e-5,
            "adjusted_score=1.8 → feasibility=0.9, got {feasibility_high}"
        );

        // passed=false → feasibility=0.0 regardless of adjusted_score.
        let mut ann_zero = PlanAnnotation::default();
        ann_zero.viability = ViabilityResult { passed: false, adjusted_score: 1.8 };
        ann_zero.per_item = vec![PerItemEval::default()];
        let pool_zero = run_overlay(vec![TurnPlan::default()], vec![ann_zero], &agenda);
        let feasibility_zero =
            pool_zero.annotations[0].per_item[0].considerations.feasibility;
        assert!(
            feasibility_zero.abs() < 1e-5,
            "passed=false → feasibility=0.0, got {feasibility_zero}"
        );

        assert!(
            (feasibility_high - 0.5).abs() > 1e-5,
            "overlay must differ from item-level feasibility baseline (0.5)"
        );
    }

    #[test]
    fn overlay_safety_reflects_max_of_self_damage_and_exposure() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };

        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let safety = pool.annotations[0].per_item[0].considerations.safety;
        assert!(
            (safety - 1.0).abs() < 1e-5,
            "zero exposure + zero self_damage → safety=1.0, got {safety}"
        );
    }

    #[test]
    fn overlay_safety_self_damage_dominates_exposure() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };

        let mut plan = TurnPlan::default();
        plan.annotation.outcomes =
            vec![ActionOutcomeEstimate { self_damage: 50.0, ..Default::default() }];

        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay(vec![plan], vec![ann], &agenda);
        let safety = pool.annotations[0].per_item[0].considerations.safety;
        assert!(safety < 1.0, "self_damage should reduce safety below 1.0, got {safety}");
    }

    // ── S4: Leverage branch tests ─────────────────────────────────────────────

    /// FocusTarget: single-target Cast on target with known damage/hp → damage_ratio.
    /// Target hp=40, damage=20 → damage_ratio=0.5, kill=0
    /// → leverage = 0.6 * 0.5 + 0.4 * 0.0 = 0.3
    #[test]
    fn leverage_focus_target_uses_target_specific_damage() {
        let target_ent = make_entity(10);

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let target = UnitBuilder::new(10, Team::Player, hex_from_offset(2, 0))
            .hp(40)
            .max_hp(40)
            .build();
        let snap = snapshot_from(vec![actor.clone(), target], 1);

        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::FocusTarget,
                target: Some(target_ent),
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        let mut plan = TurnPlan::default();
        plan.steps = vec![make_cast_step(target_ent)];
        plan.annotation.outcomes = vec![ActionOutcomeEstimate {
            enemy_damage: 20.0,
            enemy_damage_per_entity: vec![], // single-target
            ..Default::default()
        }];

        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];
        // SecureKill = 0 (default terminal)

        let pool = run_overlay_with_snap(vec![plan], vec![ann], &agenda, snap);
        let leverage = pool.annotations[0].per_item[0].considerations.leverage;
        // damage_ratio = 20/40 = 0.5, kill = 0 → 0.6*0.5 + 0.4*0 = 0.3
        assert!(
            (leverage - 0.3).abs() < 1e-4,
            "FocusTarget leverage expected 0.3, got {leverage}"
        );
    }

    /// ApplyCC: Cast on target with cc_turns_applied=2.0 → leverage = 2/2 = 1.0
    #[test]
    fn leverage_apply_cc_uses_target_specific_cc() {
        let target_ent = make_entity(10);

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let target = UnitBuilder::new(10, Team::Player, hex_from_offset(2, 0)).build();
        let snap = snapshot_from(vec![actor.clone(), target], 1);

        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::ApplyCC,
                target: Some(target_ent),
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        let mut plan = TurnPlan::default();
        plan.steps = vec![make_cast_step(target_ent)];
        plan.annotation.outcomes = vec![ActionOutcomeEstimate {
            cc_turns_applied: 2.0,
            ..Default::default()
        }];

        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay_with_snap(vec![plan], vec![ann], &agenda, snap);
        let leverage = pool.annotations[0].per_item[0].considerations.leverage;
        // cc_turns=2, reference=2 → 2/2 = 1.0
        assert!(
            (leverage - 1.0).abs() < 1e-4,
            "ApplyCC leverage expected 1.0, got {leverage}"
        );
    }

    /// ProtectAlly: heal=10 on ally with deficit=20, plus cc_turns=1.
    /// heal_ratio = 10/20 = 0.5, cc_value = 1.0
    /// → leverage = 0.6*0.5 + 0.4*1.0 = 0.3 + 0.4 = 0.7
    #[test]
    fn leverage_protect_ally_combines_heal_and_cc() {
        let ally_ent = make_entity(10);

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        // ally with hp=10, max_hp=30 → deficit=20
        let ally = UnitBuilder::new(10, Team::Enemy, hex_from_offset(2, 0))
            .hp(10)
            .max_hp(30)
            .build();
        let snap = snapshot_from(vec![actor.clone(), ally], 1);

        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::ProtectAlly,
                target: Some(ally_ent),
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        let mut plan = TurnPlan::default();
        plan.annotation.outcomes = vec![ActionOutcomeEstimate {
            hp_restored: 10.0,
            cc_turns_applied: 1.0,
            ..Default::default()
        }];

        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay_with_snap(
            vec![plan],
            vec![ann],
            &agenda,
            snap,
        );
        let leverage = pool.annotations[0].per_item[0].considerations.leverage;
        // heal_ratio = 10/20 = 0.5, cc_value = min(1.0, 1.0) = 1.0
        // leverage = 0.6*0.5 + 0.4*1.0 = 0.7
        assert!(
            (leverage - 0.7).abs() < 1e-4,
            "ProtectAlly leverage expected 0.7, got {leverage}"
        );
    }

    /// ProtectSelf stationary cap: with empty danger map, danger_now=0 → reduction=0.
    /// Leverage maxes at SELF_SURVIVAL_WEIGHT (0.7) — by design, mobile defense
    /// rewarded over stationary defense. Pins the documented intentional cap.
    #[test]
    fn leverage_protect_self_caps_at_survival_when_no_danger_reduction() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::ProtectSelf,
                target: None,
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];
        ann.factors.set_plan(PlanFactor::SelfSurvival, 0.8);
        let mut terminal = FactorTerminalScore::default();
        terminal.set(TerminalFactor::ExposureAtEnd, 0.2);
        ann.terminal = terminal;
        // empty_maps has danger=0 everywhere → danger_now=0, reduction=(0-0.2).max(0)=0
        // → leverage = 0.7*0.8 + 0.3*0.0 = 0.56

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let leverage = pool.annotations[0].per_item[0].considerations.leverage;
        assert!(
            (leverage - 0.56).abs() < 1e-4,
            "stationary ProtectSelf must cap at SELF_SURVIVAL_WEIGHT * survival = 0.56, got {leverage}"
        );
    }

    /// ProtectSelf active-escape: actor starts on a high-danger tile, plan ends in
    /// safer position → reduction > 0 → leverage > stationary cap. Pins the documented
    /// "mobile defense > stationary defense" semantic (counterpart to the cap test).
    #[test]
    fn leverage_protect_self_uses_reduction_when_danger_decreases() {
        use crate::combat::ai::world::influence::{InfluenceMap, InfluenceMaps};

        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
        let snap = snapshot_from(vec![actor.clone()], 1);

        // Custom danger map: 0.6 at actor_pos, 0 elsewhere.
        let mut danger = InfluenceMap::new();
        danger.add(actor_pos, 0.6);
        let maps = InfluenceMaps {
            danger,
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        };

        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();

        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::ProtectSelf,
                target: None,
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];
        ann.factors.set_plan(PlanFactor::SelfSurvival, 0.8);
        // Plan ends in safer tile → ExposureAtEnd = 0.2 (danger_after).
        // danger_now = 0.6 (from map at actor_pos). reduction = 0.6 - 0.2 = 0.4.
        // leverage = 0.7 * 0.8 + 0.3 * 0.4 = 0.56 + 0.12 = 0.68.
        let mut terminal = FactorTerminalScore::default();
        terminal.set(TerminalFactor::ExposureAtEnd, 0.2);
        ann.terminal = terminal;

        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            actor_pos,
            &mut rng,
        )
        .with_agenda(&agenda);
        let mut pool = ScoredPool::new(vec![TurnPlan::default()]);
        pool.annotations = vec![ann];
        OverlayConsiderationsStage.apply(&mut pool, &mut ctx);

        let leverage = pool.annotations[0].per_item[0].considerations.leverage;
        assert!(
            (leverage - 0.68).abs() < 1e-4,
            "active-escape ProtectSelf must include reduction component: expected 0.68, got {leverage}"
        );
        assert!(
            leverage > 0.56,
            "active-escape leverage must exceed stationary cap (0.56), got {leverage}"
        );
    }

    /// Reposition: LineActionability=0.8, PressureSpacingZone=0.6
    /// leverage = 0.5*0.8 + 0.5*0.6 = 0.4 + 0.3 = 0.7
    #[test]
    fn leverage_reposition_uses_terminal_factors() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::Reposition,
                target: None,
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];
        let mut terminal = FactorTerminalScore::default();
        terminal.set(TerminalFactor::LineActionability, 0.8);
        terminal.set(TerminalFactor::PressureSpacingZone, 0.6);
        ann.terminal = terminal;

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let leverage = pool.annotations[0].per_item[0].considerations.leverage;
        assert!(
            (leverage - 0.7).abs() < 1e-4,
            "Reposition leverage expected 0.7, got {leverage}"
        );
    }

    /// LastStand: SecureKill=1.0, total_damage=10 → damage_norm=10/10=1.0
    /// leverage = 0.7*1.0 + 0.3*1.0 = 1.0
    #[test]
    fn leverage_last_stand_uses_total_damage_and_kill() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::LastStand,
                target: None,
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        let mut plan = TurnPlan::default();
        plan.annotation.outcomes = vec![ActionOutcomeEstimate {
            enemy_damage: 10.0,
            ..Default::default()
        }];

        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];
        let mut terminal = FactorTerminalScore::default();
        terminal.set(TerminalFactor::SecureKill, 1.0);
        ann.terminal = terminal;

        let pool = run_overlay(vec![plan], vec![ann], &agenda);
        let leverage = pool.annotations[0].per_item[0].considerations.leverage;
        // kill=1.0, damage_norm=10/10=1.0 → 0.7 + 0.3 = 1.0
        assert!(
            (leverage - 1.0).abs() < 1e-4,
            "LastStand leverage expected 1.0, got {leverage}"
        );
    }

    // ── S5: AoE negative tests (target-specificity) ───────────────────────────

    /// FocusTarget item with target=A. Plan has AoE Cast targeting B with
    /// enemy_damage_per_entity = [(B, 30), (C, 20)]. Since target=A is not in
    /// per_entity, damage_to_target=0 → leverage ≈ 0 (no kill either).
    #[test]
    fn leverage_focus_target_aoe_does_not_credit_other_enemies() {
        let target_a = make_entity(10);
        let target_b = make_entity(11);
        let target_c = make_entity(12);

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let unit_a = UnitBuilder::new(10, Team::Player, hex_from_offset(2, 0))
            .hp(50)
            .max_hp(50)
            .build();
        let snap = snapshot_from(vec![actor.clone(), unit_a], 1);

        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::FocusTarget,
                target: Some(target_a), // agenda target = A
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        // Plan casts at B (not A) with AoE hitting B and C
        let mut plan = TurnPlan::default();
        plan.steps = vec![make_cast_step(target_b)];
        plan.annotation.outcomes = vec![ActionOutcomeEstimate {
            enemy_damage: 50.0, // total AoE damage, but not to A
            enemy_damage_per_entity: vec![(target_b, 30.0), (target_c, 20.0)],
            ..Default::default()
        }];

        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];
        // SecureKill = 0.0 (default)

        let pool = run_overlay_with_snap(vec![plan], vec![ann], &agenda, snap);
        let leverage = pool.annotations[0].per_item[0].considerations.leverage;
        // damage_to_target(A) = 0, kill = 0 → leverage = 0
        assert!(
            leverage < 1e-4,
            "FocusTarget AoE must not credit damage to other enemies; expected ≈0, got {leverage}"
        );
    }

    /// ApplyCC item with target=A. Plan casts at B with cc_turns_applied=2.
    /// Since Cast.target=B ≠ A → cc_turns_applied_to_target(A)=0 → leverage=0.
    #[test]
    fn leverage_apply_cc_aoe_cc_does_not_credit_other_enemies() {
        let target_a = make_entity(10);
        let target_b = make_entity(11);

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = snapshot_from(vec![actor.clone()], 1);

        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::ApplyCC,
                target: Some(target_a), // agenda target = A
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        // Plan casts at B (not A)
        let mut plan = TurnPlan::default();
        plan.steps = vec![make_cast_step(target_b)];
        plan.annotation.outcomes = vec![ActionOutcomeEstimate {
            cc_turns_applied: 2.0, // CC on B, not A
            ..Default::default()
        }];

        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay_with_snap(vec![plan], vec![ann], &agenda, snap);
        let leverage = pool.annotations[0].per_item[0].considerations.leverage;
        // Cast.target=B ≠ A → cc_turns_applied_to_target(A) = 0 → leverage = 0
        assert!(
            leverage < 1e-4,
            "ApplyCC AoE CC on non-target must not credit leverage; expected ≈0, got {leverage}"
        );
    }

    // ── T5: Feasibility tests (step 11.8 §B) ─────────────────────────────────

    /// Two plans with different `adjusted_score` but both `passed=true` produce
    /// different feasibility values via the continuous formula.
    /// Formula: (adjusted_score - 0.0) / 2.0.  Scores 0.5 and 1.5 → 0.25 and 0.75.
    #[test]
    fn feasibility_continuous_distinguishes_two_adjusted_scores() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };

        // adjusted_score = 0.5, passed = true → (0.5 - 0.0) / 2.0 = 0.25
        let mut ann_low = PlanAnnotation::default();
        ann_low.viability = ViabilityResult { passed: true, adjusted_score: 0.5 };
        ann_low.per_item = vec![PerItemEval::default()];
        let pool_low = run_overlay(vec![TurnPlan::default()], vec![ann_low], &agenda);
        let f_low = pool_low.annotations[0].per_item[0].considerations.feasibility;

        // adjusted_score = 1.5, passed = true → (1.5 - 0.0) / 2.0 = 0.75
        let mut ann_high = PlanAnnotation::default();
        ann_high.viability = ViabilityResult { passed: true, adjusted_score: 1.5 };
        ann_high.per_item = vec![PerItemEval::default()];
        let pool_high = run_overlay(vec![TurnPlan::default()], vec![ann_high], &agenda);
        let f_high = pool_high.annotations[0].per_item[0].considerations.feasibility;

        assert!(
            (f_low - 0.25).abs() < 1e-5,
            "adjusted_score=0.5 → feasibility=0.25, got {f_low}"
        );
        assert!(
            (f_high - 0.75).abs() < 1e-5,
            "adjusted_score=1.5 → feasibility=0.75, got {f_high}"
        );
        assert!(
            f_high > f_low,
            "higher adjusted_score must produce higher feasibility"
        );
    }

    /// A plan with `passed=false` must have feasibility=0.0 regardless of
    /// `adjusted_score`.  This pins the `!passed` guard (Section B rationale:
    /// `adjusted_score` for failed plans is unspecified and may be high).
    #[test]
    fn feasibility_zero_when_viability_failed() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };

        // High adjusted_score but passed=false → guard must fire → feasibility=0.0.
        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: false, adjusted_score: 1.5 };
        ann.per_item = vec![PerItemEval::default()];

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let feasibility = pool.annotations[0].per_item[0].considerations.feasibility;

        assert!(
            feasibility.abs() < 1e-5,
            "passed=false must yield feasibility=0.0 regardless of adjusted_score=1.5, got {feasibility}"
        );
    }

    /// Safety probe (step 11.8 §D): formula isolation test.
    ///
    /// Asserts that `safety = 1.0 - max(self_damage_ratio, exposure_at_end)` correctly
    /// drops below 1.0 when `terminal.ExposureAtEnd` is high. With exposure=0.8 and
    /// self_damage=0, expected safety = 1.0 - 0.8 = 0.2.
    ///
    /// Sets `terminal.ExposureAtEnd` directly — does NOT exercise the
    /// `maps.danger → TerminalStage → exposure_at_end` pipeline (that's covered by
    /// 11.7b synthetic tests in `planning::terminal::tests`). This pins the overlay
    /// formula in isolation.
    ///
    /// Production context: the H1c histogram shows safety flat at 1.0. That is
    /// corpus-bound (OvercommitIntoDanger critic + scenario design keep actors in
    /// safe tiles), not a code bug. If THIS isolation test fails (safety stays 1.0
    /// despite high exposure), the formula itself is broken → escalate to backlog.
    #[test]
    fn safety_drops_below_one_when_exposure_at_end_is_high() {
        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![empty_agenda_item()],
        };

        let mut ann = PlanAnnotation::default();
        ann.viability = ViabilityResult { passed: true, adjusted_score: 1.0 };
        ann.per_item = vec![PerItemEval::default()];
        // High danger at plan end position — captured as terminal factor.
        // safety = 1.0 - max(self_damage_ratio=0, exposure=0.8) = 0.2
        let mut terminal = FactorTerminalScore::default();
        terminal.set(TerminalFactor::ExposureAtEnd, 0.8);
        ann.terminal = terminal;

        let pool = run_overlay(vec![TurnPlan::default()], vec![ann], &agenda);
        let safety = pool.annotations[0].per_item[0].considerations.safety;

        assert!(
            (safety - 0.2).abs() < 1e-4,
            "safety probe: exposure=0.8 must give safety=0.2, got {safety}"
        );
        assert!(
            safety < 1.0,
            "safety formula broken — high exposure must produce safety < 1.0 (got {safety})"
        );
    }

    /// Regression: outcomes consumers must read from `plan.annotation.outcomes`
    /// (authoritative — populated by generator), NOT `pool.annotations[i].outcomes`
    /// (dead during pipeline; only populated at log serialization). This test pins
    /// the bug that previously caused FocusTarget/ApplyCC/ProtectAlly/LastStand
    /// leverage to read empty outcomes → all returned 0 even for winning items.
    #[test]
    fn overlay_uses_plan_annotation_outcomes_not_pipeline_annotation() {
        let target_ent = make_entity(10);

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let target = UnitBuilder::new(10, Team::Player, hex_from_offset(2, 0))
            .hp(40)
            .max_hp(40)
            .build();
        let snap = snapshot_from(vec![actor.clone(), target], 1);

        let agenda = Agenda {
            band: PriorityBand::NormalTactical,
            items: vec![AgendaItem {
                kind: IntentKind::FocusTarget,
                target: Some(target_ent),
                raw_score: 0.5,
                reason: IntentReason::NoRuleDefault,
                considerations: IntentConsiderations::default(),
            }],
        };

        // Plan annotation has the authoritative outcome. Pipeline annotation is empty.
        let mut plan = TurnPlan::default();
        plan.steps = vec![make_cast_step(target_ent)];
        plan.annotation.outcomes = vec![ActionOutcomeEstimate {
            enemy_damage: 20.0,
            ..Default::default()
        }];

        // Intentionally leave pipeline annotation outcomes empty — formula must
        // ignore it and read from plan.annotation.outcomes instead.
        let mut ann = PlanAnnotation::default();
        ann.per_item = vec![PerItemEval::default()];
        assert!(
            ann.outcomes.is_empty(),
            "pipeline annotation outcomes start empty (default)"
        );

        let pool = run_overlay_with_snap(vec![plan], vec![ann], &agenda, snap);
        let leverage = pool.annotations[0].per_item[0].considerations.leverage;

        // Expected: damage_ratio = 20/40 = 0.5, kill = 0 → 0.6*0.5 + 0.4*0 = 0.3.
        // If formula reads pipeline annotation outcomes instead → leverage = 0
        // (regression).
        assert!(
            (leverage - 0.3).abs() < 1e-4,
            "leverage must read from plan.annotation.outcomes (expected 0.3, got {leverage})"
        );
    }
