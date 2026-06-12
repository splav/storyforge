//! Invariant tests for the plan sim (`SimState::apply_step`, which drives the
//! real engine `step()` with expected-value dice).
//!
//! **Layer 1** — focused invariant per outcome kind (damage, heal, resource
//! grant, …). Each test constructs an explicit fixture, calls
//! `SimState::apply_step`, and asserts the exact state-delta formula.
//!
//! **Layer 1b** — drift-dimension parity: status-reflow speed, armor-buff
//! mitigation, AoO damage/reactions, rage gain — asserted against the engine
//! damage formulas (`final_damage_f32`).
//!
//! Historical note: a Layer-2 property sweep compared `SimState` against the
//! hand-rolled `ai/sim` resolution core. Post-unisim the sim *is* the engine,
//! so the sweep became a tautology and was deleted along with `ai/sim/`.

#[cfg(test)]
mod tests {
    use crate::combat::ai::plan::sim::SimState;
    use crate::combat::ai::plan::types::PlanStep;
    use crate::combat::ai::test_helpers::{
        bevy_ability, bevy_status, empty_content, empty_status_tag_cache, snapshot_from,
        UnitBuilder,
    };
    use crate::combat::ai::world::snapshot::{ActiveStatusView, BattleSnapshot, UnitSnapshot};
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, ResourceCost,
        StatusApplication, StatusOn, TargetType,
    };
    use crate::content::content_view::ContentView;
    use crate::content::statuses::StatusDef;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;
    use combat_engine::final_damage_f32;
    use combat_engine::{AbilityId, DiceExpr, ResourceKind, StatusId};

    // ── Known sim/production divergences (referenced by tests below) ──────────
    //
    //   1. Summon — the plan sim does not spawn new units (offline, out of scope);
    //      `summon_leaves_snapshot_unit_count_unchanged` pins this.
    //
    //   2. Crit-fail — the sim never rolls crit-fail dice; production may produce
    //      `CritOutcome::ManaOverload` or a primary-skipping crit. All assertions
    //      hold on the no-crit branch only.

    // ── Shared fixture helpers ─────────────────────────────────────────────────

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        snapshot_from(units, 1)
    }

    fn zero_ctx() -> CasterContext {
        CasterContext {
            str_mod: 0,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
            dex_mod: 0,
            ranged_dice: None,
        }
    }

    /// Minimal `AbilityDef` with sane defaults. Callers override only the
    /// fields that matter for their scenario.
    fn make_ability(
        id: &str,
        effect: EffectDef,
        target_type: TargetType,
        range: u32,
        costs: Vec<ResourceCost>,
    ) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.to_string(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                effect,
                target_type,
                range: AbilityRange { min: 0, max: range },
                costs,
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        }
    }

    fn cast_step(ability_id: &AbilityId, target: Entity, pos: crate::game::hex::Hex) -> PlanStep {
        PlanStep::Cast {
            ability: ability_id.clone(),
            target,
            target_pos: pos,
        }
    }

    // ── Layer 1: focused invariants per OutcomePrimary variant ────────────────

    /// `OutcomePrimary::Damage` — HP delta equals `final_damage_f32(raw, armor, vuln, pierces)`.
    ///
    /// 1d6 EV = 3.5 → rounds to 4 (via `DiceExpr::expected().round() as i32`).
    /// str_mod = 2 → raw = 6. Armor = 2 → dealt = final_damage_f32(6, 2, 0, false) = 4.
    #[test]
    fn damage_hp_delta_matches_final_damage_formula() {
        // Engine reads caster_ctx from the unit snapshot.
        let ctx = CasterContext {
            str_mod: 2,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
            dex_mod: 0,
            ranged_dice: None,
        };
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .caster_ctx(ctx.clone())
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .hp(20)
            .armor(2)
            .build();
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        let def = make_ability(
            "strike",
            EffectDef::Damage {
                dice: DiceExpr::new(1, 6, 0),
            },
            TargetType::SingleEnemy,
            1,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &cast_step(&def.id, target_id, hex_from_offset(1, 0)),
            &ctx,
            &content,
            false,
        );

        let t = sim.unit(target_id).unwrap();
        // EV of 1d6 = 3.5 → ExpectedValue rounds to 4; raw = 4 + 2 = 6.
        let raw = DiceExpr::new(1, 6, 0).expected().round() as i32 + ctx.str_mod;
        let expected_armor = 2i32;
        let dealt = final_damage_f32(raw as f32, expected_armor as f32, 0.0, false);
        assert!(
            (t.hp() as f32 - (20.0 - dealt)).abs() < 0.01,
            "hp={} expected {}",
            t.hp(),
            20.0 - dealt
        );
    }

    /// `OutcomePrimary::Damage` with `pierces_armor = true` (SpellDamage) —
    /// armor is ignored.
    #[test]
    fn spell_damage_ignores_armor() {
        // Engine reads caster_ctx from the unit snapshot.
        let ctx = CasterContext {
            str_mod: 0,
            int_mod: 3,
            spell_power: 0,
            weapon_dice: None,
            dex_mod: 0,
            ranged_dice: None,
        };
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .caster_ctx(ctx.clone())
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .hp(20)
            .armor(10)
            .build();
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        let def = make_ability(
            "bolt",
            EffectDef::SpellDamage {
                dice: DiceExpr::new(1, 4, 0),
            },
            TargetType::SingleEnemy,
            3,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &cast_step(&def.id, target_id, hex_from_offset(1, 0)),
            &ctx,
            &content,
            false,
        );

        // EV of 1d4 = 2.5 → 3; bonus = int_mod 3; raw = 6.
        // magic_resist=0 (target default) so mitigation=0; final = max(1, 6-0) = 6.
        // Numerically identical to the old pierces=true path when magic_resist=0.
        let raw = DiceExpr::new(1, 4, 0).expected().round() as i32 + ctx.int_mod;
        let dealt = final_damage_f32(raw as f32, 0.0, 0.0, false); // magic_resist=0, no pierce
        let t = sim.unit(target_id).unwrap();
        assert!(
            (t.hp() as f32 - (20.0 - dealt)).abs() < 0.01,
            "hp={} expected {}",
            t.hp(),
            20.0 - dealt
        );
    }

    /// `OutcomePrimary::Heal` — HP delta = `min(missing_hp, amount - dot_consumed)`.
    /// No DoT on target → full heal capped at missing HP.
    #[test]
    fn heal_hp_delta_caps_at_missing_hp() {
        let actor = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .mana(5, 10)
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .hp(14)
            .max_hp(20)
            .build();
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        // 3d6 EV = 10.5 → 11; no bonus; target missing 6.
        let def = make_ability(
            "cure",
            EffectDef::Heal {
                dice: DiceExpr::new(3, 6, 0),
            },
            TargetType::SingleAlly,
            2,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &cast_step(&def.id, target_id, hex_from_offset(1, 0)),
            &zero_ctx(),
            &content,
            false,
        );

        let t = sim.unit(target_id).unwrap();
        // Missing = 6; heal EV = 11; capped → +6.
        assert_eq!(t.hp(), 20, "heal capped at max_hp");
    }

    /// `OutcomePrimary::Heal` with DoT — cleanse consumes part of heal before
    /// restoring HP. HP delta = heal - dot_consumed.
    #[test]
    fn heal_hp_delta_accounts_for_dot_cleanse() {
        // Engine reads caster_ctx from the unit snapshot.
        let ctx = CasterContext {
            str_mod: 0,
            int_mod: 2,
            spell_power: 0,
            weapon_dice: None,
            dex_mod: 0,
            ranged_dice: None,
        };
        let actor = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .caster_ctx(ctx.clone())
            .build();
        let mut target_unit = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .hp(10)
            .max_hp(20)
            .build();
        // Pre-attach a DoT with dot_per_tick = 4.
        target_unit.statuses.push(ActiveStatusView {
            id: StatusId::from("poison"),
            rounds_remaining: 2,
            dot_per_tick: 4,
        });
        let actor_id = actor.entity;
        let target_id = target_unit.entity;

        let mut content = empty_content();
        content
            .statuses
            .insert(StatusId::from("poison"), blank_status_def("poison"));
        // 1d6 EV = 3.5 → 4; int_mod = 2 → amount = 6.
        let def = make_ability(
            "mend",
            EffectDef::Heal {
                dice: DiceExpr::new(1, 6, 0),
            },
            TargetType::SingleAlly,
            2,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target_unit]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &cast_step(&def.id, target_id, hex_from_offset(1, 0)),
            &ctx,
            &content,
            false,
        );

        // amount = 4 + 2 = 6; dot_consumed = 4; remaining = 2; hp 10+2=12.
        let t = sim.unit(target_id).unwrap();
        assert_eq!(t.hp(), 12, "hp={}", t.hp());
        assert!(
            t.statuses.iter().all(|s| s.id.0 != "poison"),
            "poison cleansed"
        );
    }

    /// `OutcomePrimary::GrantMovement` — engine defers MP-grant to Phase 3.
    /// AP is still paid; MP stays unchanged.
    #[test]
    fn grant_movement_pays_ap_engine_defers_mp() {
        let actor = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build();
        let actor_id = actor.entity;

        let mut content = empty_content();
        let def = make_ability(
            "rush",
            EffectDef::GrantMovement { distance: 5 },
            TargetType::Myself,
            0,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim =
            SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
        sim.apply_step(
            &cast_step(&def.id, actor_id, hex_from_offset(0, 0)),
            &zero_ctx(),
            &content,
            false,
        );

        let a = sim.unit(actor_id).unwrap();
        // Phase 3 TODO: once engine emits GrantMovement effect, assert a.movement_points == 3 + 5.
        assert_eq!(
            a.pools[combat_engine::PoolKind::Mp]
                .map(|(c, _)| c)
                .unwrap_or(0),
            3,
            "engine defers GrantMovement to Phase 3; MP unchanged"
        );
        assert_eq!(
            a.pools[combat_engine::PoolKind::Ap]
                .map(|(c, _)| c)
                .unwrap_or(0),
            0,
            "AP cost still paid"
        );
    }

    /// `OutcomePrimary::RestoreResources` — engine defers to Phase 3.
    /// AP is paid; resources stay unchanged.
    #[test]
    fn restore_resources_pays_ap_engine_defers_increment() {
        let actor = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .hp(15)
            .max_hp(20)
            .mana(3, 10)
            .rage(1, 5)
            .energy(0, 8)
            .build();
        let actor_id = actor.entity;

        let mut content = empty_content();
        let def = make_ability(
            "rest",
            EffectDef::RestoreResources,
            TargetType::Myself,
            0,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim =
            SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
        sim.apply_step(
            &cast_step(&def.id, actor_id, hex_from_offset(0, 0)),
            &zero_ctx(),
            &content,
            false,
        );

        let a = sim.unit(actor_id).unwrap();
        // Phase 3 TODO: once engine emits RestoreResources effect, assert +1 on each.
        assert_eq!(
            a.pools[combat_engine::PoolKind::Ap]
                .map(|(c, _)| c)
                .unwrap_or(0),
            0,
            "AP cost paid"
        );
        assert_eq!(
            a.hp(),
            15,
            "engine defers RestoreResources to Phase 3; HP unchanged"
        );
        assert_eq!(
            a.pools[combat_engine::PoolKind::Mana],
            Some((3, 10)),
            "mana unchanged"
        );
        assert_eq!(
            a.pools[combat_engine::PoolKind::Rage],
            Some((1, 5)),
            "rage unchanged"
        );
        assert_eq!(
            a.pools[combat_engine::PoolKind::Energy],
            Some((0, 8)),
            "energy unchanged"
        );
    }

    /// `OutcomePrimary::Summon` — sim does not spawn units; snapshot is unchanged
    /// except for AP / mana cost deduction.
    #[test]
    fn summon_leaves_snapshot_unit_count_unchanged() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .mana(5, 10)
            .build();
        let actor_id = actor.entity;

        let mut content = empty_content();
        let def = bevy_ability(
            "summon_spirit",
            "Summon Spirit",
            combat_engine::AbilityDef {
                target_type: TargetType::Myself,
                range: AbilityRange { min: 0, max: 0 },
                effect: EffectDef::Summon {
                    template_id: "spirit".to_string(),
                    max_active: None,
                },
                costs: vec![ResourceCost {
                    resource: ResourceKind::Mana,
                    amount: 3,
                }],
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let before_count = 1usize;
        let before_mana = 5i32;
        let mut sim =
            SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
        sim.apply_step(
            &cast_step(&def.id, actor_id, hex_from_offset(0, 0)),
            &zero_ctx(),
            &content,
            false,
        );

        let a = sim.unit(actor_id).unwrap();
        // Costs deducted (AP + mana).
        assert_eq!(
            a.pools[combat_engine::PoolKind::Ap]
                .map(|(c, _)| c)
                .unwrap_or(0),
            0,
            "AP paid"
        );
        assert_eq!(
            a.pools[combat_engine::PoolKind::Mana],
            Some((before_mana - 3, 10)),
            "mana paid"
        );
        // No new unit spawned.
        assert_eq!(
            sim.snapshot.state.units().len(),
            before_count,
            "sim does not spawn: unit count unchanged"
        );
    }

    /// `OutcomePrimary::None` — only costs deducted, no state changes elsewhere.
    #[test]
    fn none_primary_only_deducts_costs() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .rage(4, 10)
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .hp(20)
            .build();
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        // Pure status ability with no primary effect.
        let mut def = make_ability(
            "taunt",
            EffectDef::None,
            TargetType::SingleEnemy,
            1,
            vec![ResourceCost {
                resource: ResourceKind::Rage,
                amount: 2,
            }],
        );
        def.statuses = vec![StatusApplication {
            status: StatusId::from("taunted"),
            duration_rounds: 1,
            on: StatusOn::Target,
        }];
        content
            .statuses
            .insert(StatusId::from("taunted"), blank_status_def("taunted"));
        content.abilities.insert(def.id.clone(), def.clone());

        let before_hp = 20i32;
        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &cast_step(&def.id, target_id, hex_from_offset(1, 0)),
            &zero_ctx(),
            &content,
            false,
        );

        let a = sim.unit(actor_id).unwrap();
        let t = sim.unit(target_id).unwrap();

        // Costs deducted.
        assert_eq!(
            a.pools[combat_engine::PoolKind::Ap]
                .map(|(c, _)| c)
                .unwrap_or(0),
            0,
            "AP paid"
        );
        assert_eq!(
            a.pools[combat_engine::PoolKind::Rage],
            Some((2, 10)),
            "rage paid"
        );
        // Primary has no HP effect on target.
        assert_eq!(t.hp(), before_hp, "target HP unchanged by None primary");
        // Status landed (non-None status is applied even with None primary).
        assert!(
            t.statuses.iter().any(|s| s.id.0 == "taunted"),
            "status was applied"
        );
    }

    /// Blank `StatusDef` used when only presence/dedup matters, not dot semantics.
    fn blank_status_def(id: &str) -> StatusDef {
        StatusDef {
            id: StatusId::from(id),
            name: id.to_string(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                bonuses: combat_engine::StatusBonuses::default(),
                skips_turn: false,
                forces_targeting: false,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: false,
            },
        }
    }

    // ── Layer 1b: drift-dimension parity (relocated from tests/combat/sim_parity.rs) ──
    //
    // Sim-side-only invariants for status-reflow speed, armor-buff mitigation, AoO
    // damage / reaction-decrement, and rage gain. These were misfiled in the full-app
    // integration binary (tests/combat/sim_parity.rs) — they never drove the real
    // combat pipeline (only TODO comments described it). Relocated 2026-05-30 (Phase 3).

    /// Parity check: after a haste status (speed_bonus=+2) is applied, the sim's
    /// `unit.speed` equals `base_speed + 2`.
    ///
    /// Verifies that `SimState::apply_step` on a Cast(haste) correctly reflows the
    /// speed bonus from the status tag cache into the sim unit's `speed` field while
    /// leaving `base_speed` unchanged.
    #[test]
    fn parity_haste_speed_real_vs_sim() {
        use crate::combat::ai::plan::sim::SimState;
        use crate::combat::ai::plan::types::PlanStep;
        use crate::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
        use crate::combat::ai::world::tags::cache::StatusBonuses;
        use crate::combat::ai::world::tags::{StatusTagCache, StatusTagSet};
        use crate::content::abilities::{
            AbilityRange, AoEShape, CasterContext, EffectDef, StatusApplication, StatusOn,
            TargetType,
        };
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use combat_engine::StatusId;

        // Build a cache with "haste" → speed_bonus=+2.
        let mut cache = StatusTagCache::default();
        let haste_id = StatusId::from("haste");
        cache.map.insert(haste_id.clone(), StatusTagSet::empty());
        cache.bonuses.insert(
            haste_id.clone(),
            StatusBonuses {
                speed_bonus: 2,
                armor_bonus: 0,
                damage_taken_bonus: 0,
            },
        );

        // Build a self-haste ability.
        let haste_def = bevy_ability(
            "cast_haste",
            "Haste",
            combat_engine::AbilityDef {
                target_type: TargetType::Myself,
                range: AbilityRange { min: 0, max: 0 },
                effect: EffectDef::None,
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: vec![StatusApplication {
                    status: haste_id.clone(),
                    duration_rounds: 2,
                    on: StatusOn::Target,
                }],
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        );

        let haste_status = bevy_status(
            "haste",
            combat_engine::StatusDef {
                bonuses: combat_engine::StatusBonuses {
                    armor_bonus: 0,
                    damage_taken_bonus: 0,
                    speed_bonus: 2,
                },
                skips_turn: false,
                forces_targeting: false,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: false,
            },
        );

        let mut content = ContentView::default();
        content
            .abilities
            .insert(haste_def.id.clone(), haste_def.clone());
        content.statuses.insert(haste_id.clone(), haste_status);

        // Build cache from content so all bonuses are correct.
        use crate::combat::ai::world::tags::cache::build_caches;
        let (status_tag_cache, _) = build_caches(&content);

        // Actor: base_speed=3, speed=3, ap=2.
        let actor_pair = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .ap(2)
            .threat(0.0)
            .max_attack_range(0)
            .abilities(vec![haste_def.id.clone()])
            .build_pair();
        let actor_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
        let snap = snapshot_from_pairs(vec![actor_pair], 1);

        // --- Sim side ---
        let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tag_cache);
        sim.apply_step(
            &PlanStep::Cast {
                ability: haste_def.id.clone(),
                target: actor_id,
                target_pos: hex_from_offset(0, 0),
            },
            &CasterContext {
                str_mod: 0,
                int_mod: 0,
                spell_power: 0,
                weapon_dice: None,
                dex_mod: 0,
                ranged_dice: None,
            },
            &content,
            false,
        );

        let actor_after = sim.unit(actor_id).expect("actor present after cast");
        assert_eq!(
            actor_after.speed, 5,
            "after haste (speed_bonus=+2), speed should be base(3)+bonus(2)=5, got {}",
            actor_after.speed,
        );
        assert_eq!(actor_after.base_speed, 3, "base_speed unchanged by status");
    }

    /// Parity check: after a stone_skin buff (armor_bonus=+5) is applied to a
    /// target, the sim computes damage correctly (raw - armor - 5).
    #[test]
    fn parity_armor_buff_mitigation_real_vs_sim() {
        use crate::combat::ai::plan::sim::SimState;
        use crate::combat::ai::plan::types::PlanStep;
        use crate::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
        use crate::content::abilities::{
            AbilityRange, AoEShape, CasterContext, EffectDef, StatusApplication, StatusOn,
            TargetType,
        };
        use crate::content::content_view::ContentView;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use combat_engine::final_damage_f32;
        use combat_engine::DiceExpr;
        use combat_engine::StatusId;

        let stone_skin_id = StatusId::from("stone_skin");

        // stone_skin: armor_bonus=+5.
        let stone_skin_def = bevy_status(
            "stone_skin",
            combat_engine::StatusDef {
                bonuses: combat_engine::StatusBonuses {
                    armor_bonus: 5,
                    damage_taken_bonus: 0,
                    speed_bonus: 0,
                },
                skips_turn: false,
                forces_targeting: false,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: false,
            },
        );

        // Buff ability: SingleEnemy (so it reaches a target in tests).
        let buff_def = bevy_ability(
            "stone_skin_cast",
            "Stone Skin",
            combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 3 },
                effect: EffectDef::None,
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: vec![StatusApplication {
                    status: stone_skin_id.clone(),
                    duration_rounds: 3,
                    on: StatusOn::Target,
                }],
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        );

        // Damage ability: 1d6 (EV=3.5→4) + str_mod=4 → raw=8.
        let atk_def = bevy_ability(
            "strike",
            "Strike",
            combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 3 },
                effect: EffectDef::Damage {
                    dice: DiceExpr::new(1, 6, 0),
                },
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        );

        let mut content = ContentView::default();
        content
            .abilities
            .insert(buff_def.id.clone(), buff_def.clone());
        content
            .abilities
            .insert(atk_def.id.clone(), atk_def.clone());
        content
            .statuses
            .insert(stone_skin_id.clone(), stone_skin_def);

        use crate::combat::ai::world::tags::cache::build_caches;
        let (status_tag_cache, _) = build_caches(&content);

        // buffer: Enemy, ap=2, max_attack_range=3, abilities=[buff]
        let buffer_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ap(2)
            .max_attack_range(3)
            .abilities(vec![buff_def.id.clone()])
            .build_pair();
        // target: Player, ap=0, mp=0, threat=0.0, max_attack_range=0, abilities=[]
        let target_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .ap(0)
            .speed(0)
            .threat(0.0)
            .max_attack_range(0)
            .build_pair();
        // attacker: Enemy, ap=2, max_attack_range=3, abilities=[atk], threat=5.0, caster_ctx(str_mod=4)
        let attacker_pair = UnitBuilder::new(3, Team::Enemy, hex_from_offset(2, 0))
            .ap(2)
            .max_attack_range(3)
            .abilities(vec![atk_def.id.clone()])
            .caster_ctx(CasterContext {
                str_mod: 4,
                int_mod: 0,
                spell_power: 0,
                weapon_dice: None,
                dex_mod: 0,
                ranged_dice: None,
            })
            .build_pair();

        let buffer_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
        let target_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");
        let attacker_id = bevy::prelude::Entity::from_raw_u32(3).expect("valid");

        let snap = snapshot_from_pairs(vec![buffer_pair, target_pair, attacker_pair], 1);

        // Step 1: apply stone_skin to target.
        let mut sim = SimState::from_snapshot(&snap, buffer_id, &status_tag_cache);
        sim.apply_step(
            &PlanStep::Cast {
                ability: buff_def.id.clone(),
                target: target_id,
                target_pos: hex_from_offset(1, 0),
            },
            &CasterContext {
                str_mod: 0,
                int_mod: 0,
                spell_power: 0,
                weapon_dice: None,
                dex_mod: 0,
                ranged_dice: None,
            },
            &content,
            false,
        );

        // Verify armor_bonus refreshed.
        assert_eq!(
            sim.unit(target_id).unwrap().armor_bonus,
            5,
            "target armor_bonus must be 5 after stone_skin",
        );

        // Step 2: attacker strikes target (swap actor).
        sim.actor = attacker_id;
        let atk_outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: atk_def.id.clone(),
                target: target_id,
                target_pos: hex_from_offset(1, 0),
            },
            &CasterContext {
                str_mod: 4,
                int_mod: 0,
                spell_power: 0,
                weapon_dice: None,
                dex_mod: 0,
                ranged_dice: None,
            },
            &content,
            false,
        );

        // raw = ceil(EV(1d6)) + str_mod(4) = 4 + 4 = 8. armor_bonus=5. Dealt = max(1, 8-5) = 3.
        let expected_dealt = final_damage_f32(8.0, 5.0, 0.0, false);
        assert!(
            (atk_outcome.damage - expected_dealt).abs() < 0.01,
            "sim damage {:.2} should equal formula {:.2} (raw=8, armor_bonus=5)",
            atk_outcome.damage,
            expected_dealt,
        );

        let target_hp = sim.unit(target_id).unwrap().hp();
        assert_eq!(
            target_hp,
            20 - expected_dealt as i32,
            "target HP should be 20 - {} = {}",
            expected_dealt as i32,
            20 - expected_dealt as i32
        );
    }

    /// Parity check (12.2): sim AoO damage matches `final_damage_f32` formula.
    ///
    /// Actor at (3,3), enemy with AoO raw=6 at (4,3) — adjacent. Actor moves to
    /// (2,3) leaving adjacency. Sim must record `outcome.self_damage ==
    /// final_damage_f32(6.0, mitigation, vuln, false)`.
    #[test]
    fn parity_aoo_real_vs_sim() {
        use crate::combat::ai::plan::sim::SimState;
        use crate::combat::ai::plan::types::PlanStep;
        use crate::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
        use crate::combat::ai::world::tags::StatusTagCache;
        use crate::content::abilities::CasterContext;
        use crate::content::content_view::ContentView;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use combat_engine::final_damage_f32;

        let raw_aoo = 6.0f32;
        let actor_armor = 2;
        let mitigation = actor_armor as f32;
        let vuln = 0.0f32;

        // actor: Enemy at (3,3), armor=2, ap=1, mp=3, threat=0.0, max_attack_range=1
        let actor_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .armor(actor_armor)
            .threat(0.0)
            .build_pair();
        // enemy: Player at (4,3), ap=0, mp=0, threat=5.0, aoo(raw=6, reactions=1)
        let enemy_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .ap(0)
            .speed(0)
            .aoo(raw_aoo, 1)
            .build_pair();

        let actor_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
        let snap = snapshot_from_pairs(vec![actor_pair, enemy_pair], 1);

        let status_tags = StatusTagCache::default();
        let content = ContentView::default();
        let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tags);
        let outcome = sim.apply_step(
            &PlanStep::Move {
                path: vec![hex_from_offset(2, 3)],
            },
            &CasterContext::default(),
            // content not needed for a Move step — pass empty.
            &content,
            false,
        );

        let expected = final_damage_f32(raw_aoo, mitigation, vuln, false);
        assert!(
            (outcome.self_damage - expected).abs() < 0.01,
            "sim AoO self_damage {:.2} must equal formula {:.2} (raw={raw_aoo}, armor={actor_armor})",
            outcome.self_damage,
            expected,
        );
    }

    /// Parity check (12.2): after one Move that provokes AoO, enemy reactions_left
    /// is decremented to 0 in the sim snapshot — mirroring real combat where
    /// `Reactions` is decremented on each AoO.
    #[test]
    fn parity_aoo_decrements_reactions_real_vs_sim() {
        use crate::combat::ai::plan::sim::SimState;
        use crate::combat::ai::plan::types::PlanStep;
        use crate::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
        use crate::combat::ai::world::tags::StatusTagCache;
        use crate::content::abilities::CasterContext;
        use crate::content::content_view::ContentView;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;

        // actor: Enemy at (3,3), ap=1, mp=3, threat=0.0, max_attack_range=1
        let actor_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .threat(0.0)
            .build_pair();
        // enemy: Player at (4,3), ap=0, mp=0, threat=5.0, aoo(raw=5.0, reactions=1)
        let enemy_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .ap(0)
            .speed(0)
            .aoo(5.0, 1)
            .build_pair();

        let actor_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
        let enemy_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");
        let snap = snapshot_from_pairs(vec![actor_pair, enemy_pair], 1);

        let status_tags = StatusTagCache::default();
        let content = ContentView::default();
        let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tags);
        sim.apply_step(
            &PlanStep::Move {
                path: vec![hex_from_offset(2, 3)],
            },
            &CasterContext::default(),
            &content,
            false,
        );

        assert_eq!(
            sim.unit(enemy_id).unwrap().reactions_left,
            0,
            "enemy reactions_left must be 0 after one provoked AoO",
        );
    }

    /// Parity check (12.3): after a single-target Damage cast, attacker rage
    /// increments by +1 and defender rage increments by +1 — mirroring
    /// `apply_effects.rs:117-129` which iterates `for actor in [source, target]`.
    #[test]
    fn parity_rage_real_vs_sim() {
        use crate::combat::ai::plan::sim::SimState;
        use crate::combat::ai::plan::types::PlanStep;
        use crate::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
        use crate::combat::ai::world::tags::StatusTagCache;
        use crate::content::abilities::{
            AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
        };
        use crate::content::content_view::ContentView;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use combat_engine::DiceExpr;

        // attacker: Enemy at (0,0), rage=(5,10), ap=1, threat=5.0
        let attacker_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .rage(5, 10)
            .caster_ctx(CasterContext {
                str_mod: 0,
                int_mod: 0,
                spell_power: 0,
                weapon_dice: None,
                dex_mod: 0,
                ranged_dice: None,
            })
            .build_pair();
        // defender: Player at (1,0), rage=(3,10), ap=0, mp=0, threat=0.0, max_attack_range=0
        let defender_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .ap(0)
            .speed(0)
            .threat(0.0)
            .max_attack_range(0)
            .rage(3, 10)
            .build_pair();

        let attacker_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
        let defender_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");

        let strike_def = bevy_ability(
            "strike",
            "Strike",
            combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 1 },
                effect: EffectDef::Damage {
                    dice: DiceExpr::new(1, 6, 0),
                },
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        );

        let mut content = ContentView::default();
        content
            .abilities
            .insert(strike_def.id.clone(), strike_def.clone());

        let snap = snapshot_from_pairs(vec![attacker_pair, defender_pair], 1);
        let status_tags = StatusTagCache::default();
        let mut sim = SimState::from_snapshot(&snap, attacker_id, &status_tags);

        sim.apply_step(
            &PlanStep::Cast {
                ability: strike_def.id.clone(),
                target: defender_id,
                target_pos: hex_from_offset(1, 0),
            },
            &CasterContext {
                str_mod: 0,
                int_mod: 0,
                spell_power: 0,
                weapon_dice: None,
                dex_mod: 0,
                ranged_dice: None,
            },
            &content,
            false,
        );

        // Real pipeline: both source and target gain +1 rage per damage event.
        assert_eq!(
            sim.unit(attacker_id).unwrap().pools[combat_engine::PoolKind::Rage],
            Some((6, 10)),
            "attacker rage (5/10) should become (6/10) after dealing damage",
        );
        assert_eq!(
            sim.unit(defender_id).unwrap().pools[combat_engine::PoolKind::Rage],
            Some((4, 10)),
            "defender rage (3/10) should become (4/10) after taking damage",
        );
    }

    /// Parity check (12.3): AoE Damage hitting 3 defenders — attacker gains +1
    /// rage per target hit (total +3), each defender gains +1.
    ///
    /// Mirrors `apply_effects.rs:117-129`: the loop iterates one entry per damage
    /// event, so AoE with N targets calls `rage.gain()` on the attacker N times.
    #[test]
    fn parity_rage_aoe_real_vs_sim() {
        use crate::combat::ai::plan::sim::SimState;
        use crate::combat::ai::plan::types::PlanStep;
        use crate::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
        use crate::combat::ai::world::tags::StatusTagCache;
        use crate::content::abilities::{
            AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
        };
        use crate::content::content_view::ContentView;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use combat_engine::DiceExpr;

        let make_unit = |id: u32, team: Team, col: i32, rage: Option<(i32, i32)>| {
            let mut b = UnitBuilder::new(id, team, hex_from_offset(col, 0)).max_attack_range(5);
            if let Some((cur, max)) = rage {
                b = b.rage(cur, max);
            }
            if team == Team::Player {
                b = b.ap(0).speed(0).threat(0.0);
            }
            b.build_pair()
        };

        let attacker_pair = make_unit(1, Team::Enemy, 0, Some((5, 10)));
        // Three defenders clustered within AoE radius 1 of (3,0).
        let d1_pair = make_unit(2, Team::Player, 3, Some((0, 10)));
        let d2_pair = make_unit(3, Team::Player, 4, Some((0, 10)));
        let d3_pair = make_unit(4, Team::Player, 2, Some((0, 10)));

        let attacker_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
        let d1_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");
        let d2_id = bevy::prelude::Entity::from_raw_u32(3).expect("valid");
        let d3_id = bevy::prelude::Entity::from_raw_u32(4).expect("valid");

        let blast_def = bevy_ability(
            "blast",
            "Blast",
            combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 5 },
                effect: EffectDef::SpellDamage {
                    dice: DiceExpr::new(1, 4, 0),
                },
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::Circle { radius: 1 },
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        );

        let mut content = ContentView::default();
        content
            .abilities
            .insert(blast_def.id.clone(), blast_def.clone());

        let snap = snapshot_from_pairs(vec![attacker_pair, d1_pair, d2_pair, d3_pair], 1);
        let status_tags = StatusTagCache::default();
        let mut sim = SimState::from_snapshot(&snap, attacker_id, &status_tags);

        let outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: blast_def.id.clone(),
                target: d1_id,
                target_pos: hex_from_offset(3, 0),
            },
            &CasterContext {
                str_mod: 0,
                int_mod: 0,
                spell_power: 0,
                weapon_dice: None,
                dex_mod: 0,
                ranged_dice: None,
            },
            &content,
            false,
        );

        assert_eq!(
            outcome.hits, 3,
            "AoE radius-1 at (3,0) should hit d1(3,0), d2(4,0), d3(2,0)"
        );

        // Attacker gets +1 per damage event → +3 total.
        assert_eq!(
            sim.unit(attacker_id).unwrap().pools[combat_engine::PoolKind::Rage],
            Some((8, 10)),
            "attacker rage (5/10) + 3 hits = (8/10)",
        );
        // Each defender gets +1.
        assert_eq!(
            sim.unit(d1_id).unwrap().pools[combat_engine::PoolKind::Rage],
            Some((1, 10)),
            "d1 (0/10) → (1/10)"
        );
        assert_eq!(
            sim.unit(d2_id).unwrap().pools[combat_engine::PoolKind::Rage],
            Some((1, 10)),
            "d2 (0/10) → (1/10)"
        );
        assert_eq!(
            sim.unit(d3_id).unwrap().pools[combat_engine::PoolKind::Rage],
            Some((1, 10)),
            "d3 (0/10) → (1/10)"
        );
    }

    /// Parity check (12.3, AoO branch): when a Move provokes an AoO, the real
    /// `movement_system` (`combat/movement.rs:228-236`) iterates
    /// `for actor in [attacker, ev.actor]` and calls `rage.gain()` on both.
    /// The sim mirrors this in `apply_move`.
    #[test]
    fn parity_aoo_grants_rage_real_vs_sim() {
        use crate::combat::ai::plan::sim::SimState;
        use crate::combat::ai::plan::types::PlanStep;
        use crate::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
        use crate::combat::ai::world::tags::StatusTagCache;
        use crate::content::abilities::CasterContext;
        use crate::content::content_view::ContentView;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;

        // Actor at (3,3), adjacent enemy at (4,3). Move to (2,3) — leaves adjacency.
        // actor: Enemy, ap=1, mp=3, rage=(4,10), threat=0.0
        let actor_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .rage(4, 10)
            .threat(0.0)
            .build_pair();
        // enemy: Player, ap=0, mp=0, rage=(7,10), threat=5.0, aoo(5.0, reactions=1)
        let enemy_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .ap(0)
            .speed(0)
            .rage(7, 10)
            .aoo(5.0, 1)
            .build_pair();

        let actor_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
        let enemy_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");
        let snap = snapshot_from_pairs(vec![actor_pair, enemy_pair], 1);

        let status_tags = StatusTagCache::default();
        let content = ContentView::default();
        let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tags);
        sim.apply_step(
            &PlanStep::Move {
                path: vec![hex_from_offset(2, 3)],
            },
            &CasterContext::default(),
            &content,
            false,
        );

        // Both sides bumped by exactly 1, mirroring `for actor in [attacker, ev.actor]`.
        assert_eq!(
            sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage],
            Some((5, 10)),
            "victim 4 → 5"
        );
        assert_eq!(
            sim.unit(enemy_id).unwrap().pools[combat_engine::PoolKind::Rage],
            Some((8, 10)),
            "AoO attacker 7 → 8",
        );
    }

    /// SpellDamage vs a defender with magic_resist > 0: the sim HP delta must
    /// equal `final_damage_f32(raw, magic_resist, 0.0, false)`.
    ///
    /// With magic_resist=0 this is identical to the old pierces_armor=true path
    /// (because `final_damage_f32(raw, 0, 0, false) == final_damage_f32(raw, X, 0, true)`
    /// when mitigation is 0). With magic_resist=3 the delta shrinks by 3 — verifying
    /// the AI sim respects magic_resist via the engine's apply_effect path.
    #[test]
    fn parity_spell_damage_respects_magic_resist() {
        use crate::combat::ai::plan::sim::SimState;
        use crate::combat::ai::test_helpers::{empty_content, snapshot_from_pairs, UnitBuilder};
        use crate::combat::ai::world::tags::StatusTagCache;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;

        let int_mod = 2i32;
        let magic_resist = 3i32;
        let raw_dice = DiceExpr::new(1, 4, 0); // EV=2.5 → rounds to 3
        let expected_raw = raw_dice.expected().round() as i32 + int_mod; // 3 + 2 = 5

        let ctx = CasterContext {
            str_mod: 0,
            int_mod,
            spell_power: 0,
            weapon_dice: None,
            dex_mod: 0,
            ranged_dice: None,
        };

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .caster_ctx(ctx.clone())
            .build_pair();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .hp(20)
            .max_hp(20)
            .magic_resist(magic_resist)
            .build_pair();

        let actor_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
        let target_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");
        let snap = snapshot_from_pairs(vec![actor, target], 1);

        let mut content = empty_content();
        let def = make_ability(
            "bolt",
            EffectDef::SpellDamage { dice: raw_dice },
            TargetType::SingleEnemy,
            3,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let status_tags = StatusTagCache::default();
        let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tags);
        sim.apply_step(
            &cast_step(&def.id, target_id, hex_from_offset(1, 0)),
            &ctx,
            &content,
            false,
        );

        // Engine uses magic_resist (not armor), pierces=false.
        let expected_dealt = final_damage_f32(expected_raw as f32, magic_resist as f32, 0.0, false);
        let t = sim.unit(target_id).unwrap();
        assert!(
            (t.hp() as f32 - (20.0 - expected_dealt)).abs() < 0.01,
            "hp={} expected {} (raw={} mr={} dealt={})",
            t.hp(),
            20.0 - expected_dealt,
            expected_raw,
            magic_resist,
            expected_dealt,
        );
    }
}
