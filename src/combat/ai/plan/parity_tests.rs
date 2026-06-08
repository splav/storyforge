//! Property-battery for shared-core ↔ sim parity; extends automatically as
//! new abilities land in content.
//!
//! Two layers:
//!
//! **Layer 1** — focused invariant per `OutcomePrimary` variant. Each test
//! constructs an explicit fixture, calls `SimState::apply_step`, and asserts
//! the exact state delta formula for that variant.
//!
//! **Layer 2** — property sweep over every ability in `assets/data/abilities.toml`.
//! For each ability it builds a minimal snapshot, runs `compute_ability_outcome`
//! to get the expected outcome, runs `SimState::apply_step` on a parallel copy,
//! and asserts HP delta, status presence, cost deductions, and kill detection
//! all agree.

#[cfg(test)]
mod tests {
    use crate::combat::ai::plan::sim::SimState;
    use crate::combat::ai::plan::types::PlanStep;
    use crate::combat::ai::world::snapshot::{ActiveStatusView, BattleSnapshot, UnitSnapshot};
    use crate::combat::ai::test_helpers::{empty_content, empty_status_tag_cache, snapshot_from, UnitBuilder};
    use combat_engine::final_damage_f32;
    use crate::combat::ai::sim::effects_outcome::{
        compute_ability_outcome, ExpectedValue, OutcomePrimary,
    };
    use crate::combat::ai::sim::effects_state::{compute_affected_targets, TargetRef, TargetState};
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, ResourceCost,
        StatusApplication, StatusOn, TargetType,
    };
    use crate::content::content_view::ContentView;
    use crate::content::races::CritFailEffect;
    use crate::content::statuses::StatusDef;
    use combat_engine::{AbilityId, DiceExpr, ResourceKind, StatusId};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;

    // ── Whitelist of known divergences between sim and production ─────────────
    //
    // Listed here for documentation; referenced in test comments where relevant.
    //
    // KNOWN_DIVERGENCES:
    //   1. Summon — sim's `apply_primary` is a no-op for `OutcomePrimary::Summon`.
    //      Production spawns a new unit; sim cannot do that offline (out of scope).
    //      The property sweep skips these abilities rather than asserting state changes.
    //
    //   2. ManaOverload / crit-fail — sim always passes `crit_failed: false` to
    //      `compute_ability_outcome` (see sim.rs ~line 165). Production rolls the
    //      crit-fail die and may produce `CritOutcome::ManaOverload` or a
    //      primary-skipping crit. The parity tests never exercise crit-fail paths;
    //      all assertions hold on the no-crit branch only.
    //
    //   3. ToggleMoveMode / "move" ability — pure UI toggle, never goes through the
    //      resolution pipeline in either backend. The sweep skips it.
    //
    //   4. WeaponAttack with no weapon_dice — `EffectDef::calc` returns an
    //      `EffectCalc` with `dice: None`, so raw = 0 + str_mod. Sim and production
    //      agree; no action needed.

    // ── Shared fixture helpers ─────────────────────────────────────────────────

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        snapshot_from(units, 1)
    }

    fn zero_ctx() -> CasterContext {
        CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None }
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
        PlanStep::Cast { ability: ability_id.clone(), target, target_pos: pos }
    }

    // ── Layer 1: focused invariants per OutcomePrimary variant ────────────────

    /// `OutcomePrimary::Damage` — HP delta equals `final_damage_f32(raw, armor, vuln, pierces)`.
    ///
    /// 1d6 EV = 3.5 → rounds to 4 (via `DiceExpr::expected().round() as i32`).
    /// str_mod = 2 → raw = 6. Armor = 2 → dealt = final_damage_f32(6, 2, 0, false) = 4.
    #[test]
    fn damage_hp_delta_matches_final_damage_formula() {
        // Engine reads caster_ctx from the unit snapshot.
        let ctx = CasterContext { str_mod: 2, int_mod: 0, spell_power: 0, weapon_dice: None };
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
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
        sim.apply_step(&cast_step(&def.id, target_id, hex_from_offset(1, 0)), &ctx, &content, false);

        let t = sim.unit(target_id).unwrap();
        // EV of 1d6 = 3.5 → ExpectedValue rounds to 4; raw = 4 + 2 = 6.
        let raw = DiceExpr::new(1, 6, 0).expected().round() as i32 + ctx.str_mod;
        let expected_armor = 2i32;
        let dealt =
            final_damage_f32(raw as f32, expected_armor as f32, 0.0, false);
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
        let ctx = CasterContext { str_mod: 0, int_mod: 3, spell_power: 0, weapon_dice: None };
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
            EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
            TargetType::SingleEnemy,
            3,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
        sim.apply_step(&cast_step(&def.id, target_id, hex_from_offset(1, 0)), &ctx, &content, false);

        // EV of 1d4 = 2.5 → 3; bonus = int_mod 3; raw = 6; pierces → dealt = 6.
        let raw = DiceExpr::new(1, 4, 0).expected().round() as i32 + ctx.int_mod;
        let dealt = final_damage_f32(raw as f32, 10.0, 0.0, /* pierces */ true);
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
            EffectDef::Heal { dice: DiceExpr::new(3, 6, 0) },
            TargetType::SingleAlly,
            2,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
        sim.apply_step(&cast_step(&def.id, target_id, hex_from_offset(1, 0)), &zero_ctx(), &content, false);

        let t = sim.unit(target_id).unwrap();
        // Missing = 6; heal EV = 11; capped → +6.
        assert_eq!(t.hp(), 20, "heal capped at max_hp");
    }

    /// `OutcomePrimary::Heal` with DoT — cleanse consumes part of heal before
    /// restoring HP. HP delta = heal - dot_consumed.
    #[test]
    fn heal_hp_delta_accounts_for_dot_cleanse() {
        // Engine reads caster_ctx from the unit snapshot.
        let ctx = CasterContext { str_mod: 0, int_mod: 2, spell_power: 0, weapon_dice: None };
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
        content.statuses.insert(
            StatusId::from("poison"),
            blank_status_def("poison"),
        );
        // 1d6 EV = 3.5 → 4; int_mod = 2 → amount = 6.
        let def = make_ability(
            "mend",
            EffectDef::Heal { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleAlly,
            2,
            vec![],
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target_unit]), actor_id, empty_status_tag_cache());
        sim.apply_step(&cast_step(&def.id, target_id, hex_from_offset(1, 0)), &ctx, &content, false);

        // amount = 4 + 2 = 6; dot_consumed = 4; remaining = 2; hp 10+2=12.
        let t = sim.unit(target_id).unwrap();
        assert_eq!(t.hp(), 12, "hp={}", t.hp());
        assert!(t.statuses.iter().all(|s| s.id.0 != "poison"), "poison cleansed");
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

        let mut sim = SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
        sim.apply_step(&cast_step(&def.id, actor_id, hex_from_offset(0, 0)), &zero_ctx(), &content, false);

        let a = sim.unit(actor_id).unwrap();
        // Phase 3 TODO: once engine emits GrantMovement effect, assert a.movement_points == 3 + 5.
        assert_eq!(a.pools[combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0), 3, "engine defers GrantMovement to Phase 3; MP unchanged");
        assert_eq!(a.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 0, "AP cost still paid");
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

        let mut sim = SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
        sim.apply_step(&cast_step(&def.id, actor_id, hex_from_offset(0, 0)), &zero_ctx(), &content, false);

        let a = sim.unit(actor_id).unwrap();
        // Phase 3 TODO: once engine emits RestoreResources effect, assert +1 on each.
        assert_eq!(a.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 0, "AP cost paid");
        assert_eq!(a.hp(), 15, "engine defers RestoreResources to Phase 3; HP unchanged");
        assert_eq!(a.pools[combat_engine::PoolKind::Mana], Some((3, 10)), "mana unchanged");
        assert_eq!(a.pools[combat_engine::PoolKind::Rage], Some((1, 5)), "rage unchanged");
        assert_eq!(a.pools[combat_engine::PoolKind::Energy], Some((0, 8)), "energy unchanged");
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
        let def = AbilityDef {
            id: AbilityId::from("summon_spirit"),
            name: "Summon Spirit".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::Myself,
                range: AbilityRange { min: 0, max: 0 },
                effect: EffectDef::Summon {
                template_id: "spirit".to_string(),
                max_active: None,
            },
                costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }],
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
        };
        content.abilities.insert(def.id.clone(), def.clone());

        let before_count = 1usize;
        let before_mana = 5i32;
        let mut sim = SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
        sim.apply_step(&cast_step(&def.id, actor_id, hex_from_offset(0, 0)), &zero_ctx(), &content, false);

        let a = sim.unit(actor_id).unwrap();
        // Costs deducted (AP + mana).
        assert_eq!(a.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 0, "AP paid");
        assert_eq!(a.pools[combat_engine::PoolKind::Mana], Some((before_mana - 3, 10)), "mana paid");
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
            vec![ResourceCost { resource: ResourceKind::Rage, amount: 2 }],
        );
        def.statuses = vec![StatusApplication {
            status: StatusId::from("taunted"),
            duration_rounds: 1,
            on: StatusOn::Target,
        }];
        content.statuses.insert(StatusId::from("taunted"), blank_status_def("taunted"));
        content.abilities.insert(def.id.clone(), def.clone());

        let before_hp = 20i32;
        let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
        sim.apply_step(&cast_step(&def.id, target_id, hex_from_offset(1, 0)), &zero_ctx(), &content, false);

        let a = sim.unit(actor_id).unwrap();
        let t = sim.unit(target_id).unwrap();

        // Costs deducted.
        assert_eq!(a.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 0, "AP paid");
        assert_eq!(a.pools[combat_engine::PoolKind::Rage], Some((2, 10)), "rage paid");
        // Primary has no HP effect on target.
        assert_eq!(t.hp(), before_hp, "target HP unchanged by None primary");
        // Status landed (non-None status is applied even with None primary).
        assert!(
            t.statuses.iter().any(|s| s.id.0 == "taunted"),
            "status was applied"
        );
    }

    // ── Layer 2: property sweep across all content abilities ──────────────────

    /// For every ability in the global content file this test:
    ///
    /// 1. Builds a minimal snapshot with sufficient resources (`can_afford`).
    /// 2. Calls `compute_ability_outcome` (shared core) to get the expected outcome.
    /// 3. Calls `SimState::apply_step` on a copy of that snapshot.
    /// 4. Asserts: HP delta per affected target, status presence + duration,
    ///    cost deductions, kill detection.
    ///
    /// Abilities in `SWEEP_SKIP` are excluded per the whitelist above.
    #[test]
    fn property_sweep_all_content_abilities() {
        let content = ContentView::load_global_for_tests();
        use crate::combat::ai::world::tags::cache::build_caches;
        let (status_tag_cache, _) = build_caches(&content);

        // Abilities skipped in the sweep (see KNOWN_DIVERGENCES whitelist).
        let sweep_skip: &[&str] = &[
            "move",          // ToggleMoveMode: pure UI, never enters resolution pipeline
            "summon_storm_spirit", // Summon: sim is a no-op by design (divergence #1)
        ];
        // Additional engine-phase skip: WeaponAttack requires weapon_dice on the
        // caster; engine returns None from effect_for_target without it, while
        // the old sim applied max(1, 0) damage via str_mod fallback. Skip all
        // WeaponAttack abilities in the zero_ctx sweep to avoid this divergence.
        // Re-enable when the sweep populates weapon_dice in build_actor_for.
        let is_weapon_attack = |def: &AbilityDef| matches!(def.effect, EffectDef::WeaponAttack);

        let mut tested = 0usize;
        let mut skipped = 0usize;

        // Stable iteration order for deterministic test output.
        let mut ability_ids: Vec<_> = content.abilities.keys().cloned().collect();
        ability_ids.sort_by(|a, b| a.0.cmp(&b.0));

        for ability_id in &ability_ids {
            let def = &content.abilities[ability_id];

            if sweep_skip.contains(&ability_id.0.as_str()) || is_weapon_attack(def) {
                skipped += 1;
                continue;
            }

            // Build actor + optional target depending on target_type.
            let actor_pos = hex_from_offset(0, 0);
            let target_pos = hex_from_offset(1, 0);

            let actor = build_actor_for(def, actor_pos);
            // Engine enforces team rules: SingleAlly must target same team as actor.
            let target = match def.target_type {
                TargetType::SingleAlly => build_ally(target_pos),
                _ => build_target(target_pos),
            };
            let actor_id = actor.entity;
            let target_id = target.entity;

            // Pick primary target entity for the cast.
            let primary_target = match def.target_type {
                TargetType::Myself | TargetType::Ground | TargetType::Environment => actor_id,
                TargetType::SingleEnemy | TargetType::SingleAlly => target_id,
            };
            let primary_target_pos = match def.target_type {
                TargetType::Myself | TargetType::Ground | TargetType::Environment => actor_pos,
                TargetType::SingleEnemy | TargetType::SingleAlly => target_pos,
            };

            // Use zero caster context for determinism; str_mod / int_mod can be
            // non-zero without breaking parity — the same ctx is fed to both paths.
            let ctx = zero_ctx();

            let units = vec![actor.clone(), target.clone()];
            let snap_base = snapshot_from(units.clone(), 1);

            // Derive disadvantage flag the same way check_legality does: short-range
            // penalty when the cast distance is below the ability's min_range.
            // This must match the engine path so the reference outcome agrees with sim.
            let cast_dist = actor_pos.unsigned_distance_to(primary_target_pos);
            let disadvantage = def.range.max > 0 && cast_dist < def.range.min;

            // --- Shared-core outcome (reference) ---
            let affected = {
                let state = SnapshotTargetStateHelper(&snap_base);
                compute_affected_targets(actor_id, def, primary_target, primary_target_pos, &state)
            };
            let mut ev_dice = ExpectedValue;
            let expected_outcome = compute_ability_outcome(
                actor_id,
                def,
                affected,
                &ctx,
                disadvantage,
                /* crit_failed */ false,
                &CritFailEffect::Miss,
                &mut ev_dice,
            );

            // --- Sim ---
            let mut sim = SimState::from_snapshot(&snap_base, actor_id, &status_tag_cache);
            sim.apply_step(
                &PlanStep::Cast {
                    ability: ability_id.clone(),
                    target: primary_target,
                    target_pos: primary_target_pos,
                },
                &ctx,
                &content,
                false,
            );

            // ---- Assertions ----

            let label = &ability_id.0;

            // HP delta for each affected target.
            for &ent in &expected_outcome.affected {
                let before_hp = units.iter().find(|u| u.entity == ent).map(|u| u.hp).unwrap_or(0);
                let before_armor = units.iter().find(|u| u.entity == ent).map(|u| u.armor + u.armor_bonus).unwrap_or(0);
                let before_dtb = units.iter().find(|u| u.entity == ent).map(|u| u.damage_taken_bonus).unwrap_or(0);

                let after = sim.unit(ent);

                match &expected_outcome.primary {
                    OutcomePrimary::Damage { raw, pierces_armor } => {
                        let dealt = final_damage_f32(
                            *raw as f32,
                            before_armor as f32,
                            before_dtb as f32,
                            *pierces_armor,
                        );
                        let expected_hp = (before_hp as f32 - dealt).max(0.0) as i32;
                        let actual_hp = after.map(|u| u.hp()).unwrap_or(0);
                        assert_eq!(
                            actual_hp, expected_hp,
                            "[{label}] hp mismatch for {:?}: before={before_hp} dealt={dealt} expected={expected_hp} actual={actual_hp}",
                            ent,
                        );
                    }
                    OutcomePrimary::Heal { amount } => {
                        // No pre-existing DoT in our fixture target, so delta = min(missing, amount).
                        let missing = (20 - before_hp).max(0);
                        let effective = (*amount).min(missing);
                        let expected_hp = before_hp + effective;
                        let actual_hp = after.map(|u| u.hp()).unwrap_or(0);
                        assert_eq!(
                            actual_hp, expected_hp,
                            "[{label}] heal hp mismatch: expected={expected_hp} actual={actual_hp}",
                        );
                    }
                    // Other primaries don't touch HP of affected units.
                    _ => {}
                }
            }

            // Status applications: each `outcome.statuses[i]` present on target with correct fields.
            for sa in &expected_outcome.statuses {
                let unit_after = sim.unit(sa.target);
                let Some(u) = unit_after else {
                    // Unit may be dead (killed); statuses don't apply to corpses.
                    continue;
                };
                let found = u.statuses.iter().find(|s| s.id == sa.status);
                assert!(
                    found.is_some(),
                    "[{label}] status '{}' not found on {:?} after cast; present: {:?}",
                    sa.status.0,
                    sa.target,
                    u.statuses.iter().map(|s| &s.id.0).collect::<Vec<_>>(),
                );
                if let Some(s) = found {
                    assert_eq!(
                        s.rounds_remaining, sa.duration_rounds,
                        "[{label}] status '{}' rounds_remaining={} expected={}",
                        sa.status.0, s.rounds_remaining, sa.duration_rounds,
                    );
                    // dot_per_tick: engine always sets 0 (Phase 3 deferred — DoT
                    // roll is not yet emitted by `step()`). Skip the assertion
                    // here; re-enable when Phase 3 wires DoT into ApplyStatus.
                    // TODO(Phase 3): assert dot_per_tick matches StatusDef.dot_dice.
                }
            }

            // skips_turn statuses → HARD_CC tag via status cache.
            for sa in &expected_outcome.statuses {
                if let Some(sd) = content.statuses.get(&sa.status) {
                    if sd.skips_turn {
                        if let Some(u) = sim.unit(sa.target) {
                            assert!(
                                u.is_stunned(&status_tag_cache),
                                "[{label}] status '{}' skips_turn but HARD_CC not set on {:?}",
                                sa.status.0,
                                sa.target,
                            );
                        }
                    }
                }
            }

            // Costs: AP and each resource deducted on the actor.
            // After U4, snapshot.units is frozen; read via snapshot.unit() which
            // resolves through the live snapshot.state (engine Unit via Deref).
            if let Some(actor_after) = sim.unit(actor_id) {
                // AP.
                let expected_ap = (actor.action_points - def.cost_ap).max(0);
                assert_eq!(
                    actor_after.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), expected_ap,
                    "[{label}] AP after={} expected={}",
                    actor_after.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), expected_ap,
                );
                // Per-resource costs.
                for cost in &def.costs {
                    let before = resource_of(&actor, cost.resource);
                    let after_r = resource_of_view(&actor_after, cost.resource);
                    let expected_r = (before - cost.amount).max(0);
                    assert_eq!(
                        after_r, expected_r,
                        "[{label}] {:?} after={} expected={}",
                        cost.resource, after_r, expected_r,
                    );
                }
            }

            // Lethal damage: killed units appear in StepOutcome AND have hp=0.
            // (We re-run apply_step here separately to capture the StepOutcome.)
            {
                let mut sim2 = SimState::from_snapshot(&snapshot_from(units.clone(), 1), actor_id, empty_status_tag_cache());
                let step_outcome = sim2.apply_step(
                    &PlanStep::Cast {
                        ability: ability_id.clone(),
                        target: primary_target,
                        target_pos: primary_target_pos,
                    },
                    &ctx,
                    &content,
                    false,
                );
                for &killed_ent in &step_outcome.killed {
                    let corpse = sim2.snapshot.unit(killed_ent).expect("corpse stays in snapshot");
                    assert_eq!(
                        corpse.hp(), 0,
                        "[{label}] killed entity {:?} has hp={}",
                        killed_ent, corpse.hp(),
                    );
                    assert!(!corpse.is_alive(), "[{label}] killed entity still alive?");
                }
                // Any entity with hp=0 after damage must appear in killed list.
                for &ent in &expected_outcome.affected {
                    if matches!(&expected_outcome.primary, OutcomePrimary::Damage { .. })
                        && sim2.snapshot.unit(ent).is_some_and(|u| u.hp() == 0)
                    {
                        assert!(
                            step_outcome.killed.contains(&ent),
                            "[{label}] {:?} has hp=0 but not in killed list",
                            ent,
                        );
                    }
                }
            }

            tested += 1;
        }

        assert!(tested > 0, "sweep tested no abilities (all skipped?)");
        // Sanity: we should skip exactly the whitelist entries that exist in content.
        let _ = skipped; // informational
    }

    // ── Helpers for the property sweep ────────────────────────────────────────

    /// Build an actor with enough resources to afford any typical ability:
    /// large mana + rage + energy pools. AP = 2 so it can always pay at least 1.
    fn build_actor_for(def: &AbilityDef, pos: crate::game::hex::Hex) -> UnitSnapshot {
        let mut b = UnitBuilder::new(1, Team::Enemy, pos).ap(2).speed(3).hp(20).max_hp(20);
        // Determine required resources from costs and over-provision.
        for cost in &def.costs {
            let amount = cost.amount + 10;
            b = match cost.resource {
                ResourceKind::Mana => b.mana(amount, amount + 5),
                ResourceKind::Rage => b.rage(amount, amount + 5),
                ResourceKind::Energy => b.energy(amount, amount + 5),
                ResourceKind::Hp => b.hp(amount + 10).max_hp(amount + 20),
            };
        }
        b.build()
    }

    /// A generic enemy (opponent) target at a fixed position.
    fn build_target(pos: crate::game::hex::Hex) -> UnitSnapshot {
        UnitBuilder::new(2, Team::Player, pos).hp(20).max_hp(20).build()
    }

    /// An ally (same team as actor = Enemy) target for SingleAlly abilities.
    fn build_ally(pos: crate::game::hex::Hex) -> UnitSnapshot {
        UnitBuilder::new(2, Team::Enemy, pos).hp(14).max_hp(20).build()
    }

    /// Helper: get current resource amount from a unit snapshot.
    fn resource_of(u: &UnitSnapshot, kind: ResourceKind) -> i32 {
        match kind {
            ResourceKind::Hp => u.hp,
            ResourceKind::Mana => u.mana.map(|(c, _)| c).unwrap_or(0),
            ResourceKind::Rage => u.rage.map(|(c, _)| c).unwrap_or(0),
            ResourceKind::Energy => u.energy.map(|(c, _)| c).unwrap_or(0),
        }
    }

    /// Helper: get current resource amount from a `UnitView` (engine `Unit` via Deref).
    /// Used post-U4 where post-step reads go through `snapshot.unit()` instead of
    /// `snapshot.unit_snapshot()` (which reads frozen `snap.units`).
    fn resource_of_view(u: &crate::combat::ai::world::snapshot::UnitView<'_>, kind: ResourceKind) -> i32 {
        match kind {
            ResourceKind::Hp => u.hp(),
            ResourceKind::Mana => u.pools[combat_engine::PoolKind::Mana].map(|(c, _)| c).unwrap_or(0),
            ResourceKind::Rage => u.pools[combat_engine::PoolKind::Rage].map(|(c, _)| c).unwrap_or(0),
            ResourceKind::Energy => u.pools[combat_engine::PoolKind::Energy].map(|(c, _)| c).unwrap_or(0),
        }
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

    // ── Thin TargetState adapter for compute_affected_targets in tests ─────────

    struct SnapshotTargetStateHelper<'a>(&'a BattleSnapshot);

    impl TargetState for SnapshotTargetStateHelper<'_> {
        fn actor_pos(&self, actor: Entity) -> Option<crate::game::hex::Hex> {
            self.0.unit(actor).map(|u| u.pos)
        }
        fn unit_at_cell(&self, pos: crate::game::hex::Hex) -> Option<TargetRef> {
            let u = self.0.unit_at(pos)?;
            Some(TargetRef { entity: u.entity(), team: u.team, alive: true })
        }
        fn team_of(&self, entity: Entity) -> Option<Team> {
            self.0.unit(entity).map(|u| u.team)
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
        use crate::combat::ai::world::tags::{StatusTagCache, StatusTagSet};
        use crate::combat::ai::world::tags::cache::StatusBonuses;
        use combat_engine::StatusId;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, StatusApplication, StatusOn, TargetType};
        use combat_engine::AbilityId;

        // Build a cache with "haste" → speed_bonus=+2.
        let mut cache = StatusTagCache::default();
        let haste_id = StatusId::from("haste");
        cache.map.insert(haste_id.clone(), StatusTagSet::empty());
        cache.bonuses.insert(haste_id.clone(), StatusBonuses { speed_bonus: 2, armor_bonus: 0, damage_taken_bonus: 0 });

        // Build a self-haste ability.
        let haste_def = AbilityDef {
            id: AbilityId::from("cast_haste"),
            name: "Haste".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
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
        };

        use crate::content::content_view::ContentView;
        use crate::content::statuses::StatusDef;
        use std::collections::HashMap;

        let haste_status = StatusDef {
            id: haste_id.clone(),
            name: "Haste".into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                bonuses: combat_engine::StatusBonuses { armor_bonus: 0, damage_taken_bonus: 0, speed_bonus: 2 },
                skips_turn: false,
                forces_targeting: false,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: false,
            },
        };

        let mut content = ContentView {
            abilities: HashMap::new(),
            keyed_abilities: Vec::new(),
            statuses: HashMap::new(),
            weapons: HashMap::new(),
            armor: HashMap::new(),
            classes: HashMap::new(),
            unit_templates: HashMap::new(),
            races: HashMap::new(),
            factions: HashMap::new(),
            paths: HashMap::new(),
            ..ContentView::default()
        };
        content.abilities.insert(haste_def.id.clone(), haste_def.clone());
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
            &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
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
        use combat_engine::final_damage_f32;
        use combat_engine::StatusId;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, StatusApplication, StatusOn, TargetType};
        use combat_engine::{AbilityId, DiceExpr};
        use crate::content::statuses::StatusDef;
        use crate::content::content_view::ContentView;
        use std::collections::HashMap;

        let stone_skin_id = StatusId::from("stone_skin");

        // stone_skin: armor_bonus=+5.
        let stone_skin_def = StatusDef {
            id: stone_skin_id.clone(),
            name: "Stone Skin".into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                bonuses: combat_engine::StatusBonuses { armor_bonus: 5, damage_taken_bonus: 0, speed_bonus: 0 },
                skips_turn: false,
                forces_targeting: false,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: false,
            },
        };

        // Buff ability: SingleEnemy (so it reaches a target in tests).
        let buff_def = AbilityDef {
            id: AbilityId::from("stone_skin_cast"),
            name: "Stone Skin".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
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
        };

        // Damage ability: 1d6 (EV=3.5→4) + str_mod=4 → raw=8.
        let atk_def = AbilityDef {
            id: AbilityId::from("strike"),
            name: "Strike".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 3 },
                effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
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
        };

        let mut content = ContentView {
            abilities: HashMap::new(),
            keyed_abilities: Vec::new(),
            statuses: HashMap::new(),
            weapons: HashMap::new(),
            armor: HashMap::new(),
            classes: HashMap::new(),
            unit_templates: HashMap::new(),
            races: HashMap::new(),
            factions: HashMap::new(),
            paths: HashMap::new(),
            ..ContentView::default()
        };
        content.abilities.insert(buff_def.id.clone(), buff_def.clone());
        content.abilities.insert(atk_def.id.clone(), atk_def.clone());
        content.statuses.insert(stone_skin_id.clone(), stone_skin_def);

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
            .caster_ctx(CasterContext { str_mod: 4, int_mod: 0, spell_power: 0, weapon_dice: None })
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
            &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
            &content,
            false,
        );

        // Verify armor_bonus refreshed.
        assert_eq!(
            sim.unit(target_id).unwrap().armor_bonus, 5,
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
            &CasterContext { str_mod: 4, int_mod: 0, spell_power: 0, weapon_dice: None },
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
        assert_eq!(target_hp, 20 - expected_dealt as i32,
            "target HP should be 20 - {} = {}", expected_dealt as i32, 20 - expected_dealt as i32);
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
        use combat_engine::final_damage_f32;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use crate::content::abilities::CasterContext;
        use crate::content::content_view::ContentView;

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
            &PlanStep::Move { path: vec![hex_from_offset(2, 3)] },
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
        use crate::content::content_view::ContentView;
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use crate::content::abilities::CasterContext;

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
            &PlanStep::Move { path: vec![hex_from_offset(2, 3)] },
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
            AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
        };
        use crate::content::content_view::ContentView;
        use combat_engine::{AbilityId, DiceExpr};
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use std::collections::HashMap;

        // attacker: Enemy at (0,0), rage=(5,10), ap=1, threat=5.0
        let attacker_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .rage(5, 10)
            .caster_ctx(CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None })
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

        let strike_def = AbilityDef {
            id: AbilityId::from("strike"),
            name: "Strike".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 1 },
                effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
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
        };

        let mut content = ContentView {
            abilities: HashMap::new(),
            keyed_abilities: Vec::new(),
            statuses: HashMap::new(),
            weapons: HashMap::new(),
            armor: HashMap::new(),
            classes: HashMap::new(),
            unit_templates: HashMap::new(),
            races: HashMap::new(),
            factions: HashMap::new(),
            paths: HashMap::new(),
            ..ContentView::default()
        };
        content.abilities.insert(strike_def.id.clone(), strike_def.clone());

        let snap = snapshot_from_pairs(vec![attacker_pair, defender_pair], 1);
        let status_tags = StatusTagCache::default();
        let mut sim = SimState::from_snapshot(&snap, attacker_id, &status_tags);

        sim.apply_step(
            &PlanStep::Cast {
                ability: strike_def.id.clone(),
                target: defender_id,
                target_pos: hex_from_offset(1, 0),
            },
            &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
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
            AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
        };
        use crate::content::content_view::ContentView;
        use combat_engine::{AbilityId, DiceExpr};
        use crate::game::components::Team;
        use crate::game::hex::hex_from_offset;
        use std::collections::HashMap;

        let make_unit = |id: u32, team: Team, col: i32, rage: Option<(i32, i32)>| {
            let mut b = UnitBuilder::new(id, team, hex_from_offset(col, 0))
                .max_attack_range(5);
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

        let blast_def = AbilityDef {
            id: AbilityId::from("blast"),
            name: "Blast".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 5 },
                effect: EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
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
        };

        let mut content = ContentView {
            abilities: HashMap::new(),
            keyed_abilities: Vec::new(),
            statuses: HashMap::new(),
            weapons: HashMap::new(),
            armor: HashMap::new(),
            classes: HashMap::new(),
            unit_templates: HashMap::new(),
            races: HashMap::new(),
            factions: HashMap::new(),
            paths: HashMap::new(),
            ..ContentView::default()
        };
        content.abilities.insert(blast_def.id.clone(), blast_def.clone());

        let snap = snapshot_from_pairs(vec![attacker_pair, d1_pair, d2_pair, d3_pair], 1);
        let status_tags = StatusTagCache::default();
        let mut sim = SimState::from_snapshot(&snap, attacker_id, &status_tags);

        let outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: blast_def.id.clone(),
                target: d1_id,
                target_pos: hex_from_offset(3, 0),
            },
            &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
            &content,
            false,
        );

        assert_eq!(outcome.hits, 3, "AoE radius-1 at (3,0) should hit d1(3,0), d2(4,0), d3(2,0)");

        // Attacker gets +1 per damage event → +3 total.
        assert_eq!(
            sim.unit(attacker_id).unwrap().pools[combat_engine::PoolKind::Rage],
            Some((8, 10)),
            "attacker rage (5/10) + 3 hits = (8/10)",
        );
        // Each defender gets +1.
        assert_eq!(sim.unit(d1_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((1, 10)), "d1 (0/10) → (1/10)");
        assert_eq!(sim.unit(d2_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((1, 10)), "d2 (0/10) → (1/10)");
        assert_eq!(sim.unit(d3_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((1, 10)), "d3 (0/10) → (1/10)");
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
            &PlanStep::Move { path: vec![hex_from_offset(2, 3)] },
            &CasterContext::default(),
            &content,
            false,
        );

        // Both sides bumped by exactly 1, mirroring `for actor in [attacker, ev.actor]`.
        assert_eq!(sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage], Some((5, 10)), "victim 4 → 5");
        assert_eq!(
            sim.unit(enemy_id).unwrap().pools[combat_engine::PoolKind::Rage],
            Some((8, 10)),
            "AoO attacker 7 → 8",
        );
    }
}
