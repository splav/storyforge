//! Tests for `snapshot.rs` — split from the source file via `#[path]` in
//! `snapshot.rs` (see end of that file). Production code stays in
//! `snapshot.rs`; this file holds the 3 test modules.
//!
//! Split per [docs/testing.md §2](../../../../docs/testing.md): `snapshot.rs`
//! grew to 1703 LOC with ~60% in tests after Phase 4b coverage work.
//! Splitting keeps the production module under 825 LOC and immediately
//! browsable.
//!
//! `super::*` here resolves to `snapshot.rs` (since this file is included
//! as `mod tests` inside snapshot.rs). The inner test modules pick up
//! snapshot's pub items through the file-level `use super::*;` below.

use super::*;

#[cfg(test)]
mod affordability_tests {
    use super::*;
    use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, ResourceCost};
    use crate::game::hex::hex_from_offset;
    use combat_engine::{AbilityId, DiceExpr};

    fn def(cost_ap: i32, costs: Vec<ResourceCost>) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("x"),
            name: "x".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 1 },
                effect: EffectDef::Damage {
                    dice: DiceExpr::new(1, 6, 0),
                },
                costs,
                cost_ap,
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

    fn cost(kind: ResourceKind, amount: i32) -> ResourceCost {
        ResourceCost {
            resource: kind,
            amount,
        }
    }

    #[test]
    fn can_afford_covers_ap_and_all_resource_kinds() {
        use crate::combat::ai::test_helpers::{snapshot_from, UnitBuilder};
        let unit = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ap(2)
            .mana(5, 10)
            .rage(3, 10)
            .energy(4, 10)
            .build();
        let entity = unit.entity;
        let snap = snapshot_from(vec![unit], 1);
        let u = snap.unit(entity).expect("view");
        // (name, ap_cost, costs, expected can_afford)
        let cases: Vec<(&str, i32, Vec<ResourceCost>, bool)> = vec![
            ("free ability", 1, vec![], true),
            ("AP shortage", 3, vec![], false),
            ("mana ok", 1, vec![cost(ResourceKind::Mana, 5)], true),
            ("mana short", 1, vec![cost(ResourceKind::Mana, 6)], false),
            ("rage ok", 1, vec![cost(ResourceKind::Rage, 3)], true),
            ("rage short", 1, vec![cost(ResourceKind::Rage, 4)], false),
            ("energy ok", 1, vec![cost(ResourceKind::Energy, 4)], true),
            (
                "energy short",
                1,
                vec![cost(ResourceKind::Energy, 5)],
                false,
            ),
            ("hp ok", 1, vec![cost(ResourceKind::Hp, 20)], true),
            ("hp short", 1, vec![cost(ResourceKind::Hp, 21)], false),
            (
                "two costs both ok",
                1,
                vec![cost(ResourceKind::Mana, 5), cost(ResourceKind::Rage, 3)],
                true,
            ),
            (
                "two costs one short",
                1,
                vec![cost(ResourceKind::Mana, 5), cost(ResourceKind::Rage, 4)],
                false,
            ),
        ];
        for (name, ap_cost, costs, want) in cases {
            let d = def(ap_cost, costs);
            assert_eq!(u.can_afford(&d), want, "{name}");
        }
    }

    #[test]
    fn resource_amount_treats_absent_option_pools_as_zero() {
        use crate::combat::ai::test_helpers::{snapshot_from, UnitBuilder};
        // No mana/rage/energy pools (builder defaults to None).
        let unit = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ap(2)
            .build();
        let entity = unit.entity;
        let snap = snapshot_from(vec![unit], 1);
        let u = snap.unit(entity).expect("view");
        assert_eq!(u.resource_amount(ResourceKind::Mana), 0);
        assert_eq!(u.resource_amount(ResourceKind::Rage), 0);
        assert_eq!(u.resource_amount(ResourceKind::Energy), 0);
        assert_eq!(u.resource_amount(ResourceKind::Hp), u.hp());
        // Any positive cost on an absent pool fails.
        let d = def(1, vec![cost(ResourceKind::Mana, 1)]);
        assert!(!u.can_afford(&d));
    }

    /// Dead units stay in `units` (hp=0 marker); the default-facing
    /// `enemies_of` / `allies_of` accessors hide them, while the explicit
    /// `all_enemies_of` / `dead_units` surface them for resurrection / on-kill /
    /// replay call sites. Pins the new contract.
    #[test]
    fn dead_units_stay_in_snapshot_and_are_filtered_by_default() {
        use crate::combat::ai::test_helpers::{snapshot_from, UnitBuilder};
        let alive = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20)
            .build();
        let corpse = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .hp(0)
            .build();
        let alive_entity = alive.entity;
        let corpse_entity = corpse.entity;
        let snap = snapshot_from(vec![alive, corpse], 1);

        assert!(
            snap.unit(corpse_entity).is_some(),
            "corpse must stay in units"
        );
        assert_eq!(
            snap.unit(corpse_entity).map(|u| u.is_alive()),
            Some(false),
            "corpse must report is_alive = false",
        );

        // Default accessors hide the dead.
        assert_eq!(
            snap.enemies_of(Team::Enemy).count(),
            0,
            "default enemies_of hides dead"
        );
        assert_eq!(
            snap.allies_of(Team::Enemy).count(),
            1,
            "alive ally still visible"
        );

        // Explicit "all" + "dead" variants surface them.
        assert_eq!(snap.all_enemies_of(Team::Enemy).count(), 1);
        assert_eq!(snap.dead_enemies_of(Team::Enemy).count(), 1);
        assert_eq!(snap.dead_units().count(), 1);
        let _ = alive_entity; // silence unused warning
    }
}

#[cfg(test)]
mod computation_tests {
    use super::*;
    use crate::combat::ai::test_helpers::{snapshot_from, UnitBuilder};
    use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, ResourceCost};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use combat_engine::DiceExpr;

    fn def_ap(cost_ap: i32) -> AbilityDef {
        AbilityDef {
            id: combat_engine::AbilityId::from("x"),
            name: "x".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: crate::content::abilities::TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 1 },
                effect: EffectDef::Damage {
                    dice: DiceExpr::new(1, 6, 0),
                },
                costs: Vec::new(),
                cost_ap,
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

    fn def_with_cost(cost_ap: i32, resource: ResourceKind, amount: i32) -> AbilityDef {
        let mut d = def_ap(cost_ap);
        d.engine.costs = vec![ResourceCost { resource, amount }];
        d
    }

    // ── UnitView (lines 239-274) ──────────────────────────────────────────
    // UnitView wraps the engine Unit (via Deref); its arithmetic is identical
    // to UnitSnapshot's.  We use snapshot_from + snap.unit(entity) to get a
    // real UnitView, then exercise the same edge-case table.
    // Targets lines 240:23/42, 245:28/47, 255:9, 256:20, 257:13, 273:9/34, 274:74.

    fn snap_view_with(
        entity_raw: u32,
        hp: i32,
        max_hp: i32,
        armor: i32,
        armor_bonus: i32,
    ) -> (BattleSnapshot, Entity) {
        let entity = Entity::from_raw_u32(entity_raw).expect("valid");
        let u = UnitBuilder::new(entity_raw, Team::Player, hex_from_offset(0, 0))
            .hp(hp)
            .max_hp(max_hp)
            .armor(armor)
            .armor_bonus(armor_bonus)
            .build();
        let snap = snapshot_from(vec![u], 1);
        (snap, entity)
    }

    #[test]
    fn unit_view_eff_hp_additive() {
        // Lines 240:23, 240:42 — same formula as UnitSnapshot.
        let cases: &[(i32, i32, i32, i32)] = &[
            (5, 0, 0, 5),
            (5, 3, 0, 8),
            (5, 0, 2, 7),
            (5, 3, 2, 10),
            (0, 5, 0, 5),
        ];
        for (idx, &(hp, armor, ab, expected)) in cases.iter().enumerate() {
            let (snap, entity) = snap_view_with((idx + 1) as u32, hp, 20, armor, ab);
            let view = snap.unit(entity).expect("view");
            assert_eq!(view.eff_hp(), expected, "hp={hp} armor={armor} ab={ab}");
        }
    }

    #[test]
    fn unit_view_eff_max_hp_additive_and_clamp() {
        // Lines 245:28, 245:47 — same formula as UnitSnapshot.
        let cases: &[(i32, i32, i32, i32)] = &[
            (10, 0, 0, 10),
            (10, 3, 0, 13),
            (10, 0, 2, 12),
            (10, 3, 2, 15),
            (-5, 0, 0, 1), // clamp to 1
        ];
        for (idx, &(max_hp, armor, ab, expected)) in cases.iter().enumerate() {
            let (snap, entity) = snap_view_with((idx + 1) as u32, 5, max_hp, armor, ab);
            let view = snap.unit(entity).expect("view");
            assert_eq!(
                view.eff_max_hp(),
                expected,
                "max_hp={max_hp} armor={armor} ab={ab}"
            );
        }
    }

    #[test]
    fn unit_view_hp_pct_correctness() {
        // hp_pct = hp / max_hp.max(1). Tests line 249 (UnitView path).
        let cases: &[(u32, i32, i32, f32)] = &[
            (1, 10, 10, 1.0),
            (2, 5, 10, 0.5),
            (3, 0, 10, 0.0),
            (4, 5, 0, 5.0), // clamp guard
        ];
        for &(raw, hp, max_hp, expected) in cases {
            let (snap, entity) = snap_view_with(raw, hp, max_hp, 0, 0);
            let view = snap.unit(entity).expect("view");
            let got = view.hp_pct();
            assert!(
                (got - expected).abs() < 1e-5,
                "hp={hp} max_hp={max_hp}: got {got}, expected {expected}"
            );
        }
    }

    #[test]
    fn unit_view_killability_correctness() {
        // Lines 255:9, 256:20, 257:13.
        let cases: &[(u32, i32, i32, f32)] = &[
            (1, 10, 10, 0.0), // full HP
            (2, 5, 10, 0.5),  // half HP
            (3, 0, 10, 1.0),  // dead
            (4, 1, 10, 0.9),  // near-dead
        ];
        for &(raw, hp, max_hp, expected) in cases {
            let (snap, entity) = snap_view_with(raw, hp, max_hp, 0, 0);
            let view = snap.unit(entity).expect("view");
            let got = view.killability();
            assert!(
                (got - expected).abs() < 1e-5,
                "hp={hp} max_hp={max_hp}: got {got}, expected {expected}"
            );
        }
    }

    #[test]
    fn unit_view_can_afford_ap_and_resource_boundaries() {
        // Lines 273:9/34, 274:74.
        let snap_unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .ap(2)
            .mana(5, 10)
            .build();
        let entity = Entity::from_raw_u32(1).expect("valid");
        let snap = snapshot_from(vec![snap_unit], 1);
        let view = snap.unit(entity).expect("view");

        assert!(view.can_afford(&def_ap(2)), "AP == cost");
        assert!(!view.can_afford(&def_ap(3)), "AP < cost");
        assert!(
            !view.can_afford(&def_with_cost(2, ResourceKind::Mana, 6)),
            "short mana fails"
        );
        assert!(
            !view.can_afford(&def_with_cost(3, ResourceKind::Mana, 5)),
            "short AP fails even with ok mana"
        );
        assert!(
            view.can_afford(&def_with_cost(2, ResourceKind::Mana, 5)),
            "exact ok"
        );
    }

    #[test]
    fn unit_view_is_alive_boundary() {
        // hp=1 → alive; hp=0 → dead. Mirrors unit_snapshot_is_alive_boundary.
        let (snap_alive, entity_alive) = snap_view_with(1, 1, 20, 0, 0);
        assert!(
            snap_alive.unit(entity_alive).expect("view").is_alive(),
            "hp=1 must be alive"
        );
        let (snap_dead, entity_dead) = snap_view_with(2, 0, 20, 0, 0);
        assert!(
            !snap_dead.unit(entity_dead).expect("view").is_alive(),
            "hp=0 must be dead"
        );
    }

    #[test]
    fn unit_view_resource_amount_returns_correct_pool() {
        // Mirrors unit_snapshot_resource_amount_returns_correct_pool.
        let snap_unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .hp(15)
            .mana(7, 10)
            .rage(3, 10)
            .energy(4, 10)
            .build();
        let entity = Entity::from_raw_u32(1).expect("valid");
        let snap = snapshot_from(vec![snap_unit], 1);
        let view = snap.unit(entity).expect("view");
        assert_eq!(view.resource_amount(ResourceKind::Hp), 15);
        assert_eq!(view.resource_amount(ResourceKind::Mana), 7);
        assert_eq!(view.resource_amount(ResourceKind::Rage), 3);
        assert_eq!(view.resource_amount(ResourceKind::Energy), 4);
    }

    // ── BattleSnapshot::entity_for_uid / uid_for_entity ──────────────────
    // Targets line 645:9 (entity_for_uid returns Option).

    #[test]
    fn entity_for_uid_some_for_known_none_for_unknown() {
        let entity = Entity::from_raw_u32(7).expect("valid");
        let u = UnitBuilder::new(7, Team::Player, hex_from_offset(0, 0)).build();
        let snap = snapshot_from(vec![u], 1);
        let uid = snap.uid_for_entity(entity).expect("uid must be known");
        assert_eq!(
            snap.entity_for_uid(uid),
            Some(entity),
            "known uid → Some(entity)"
        );

        // Fabricate an unknown uid.
        let unknown_uid = combat_engine::state::UnitId(u64::MAX);
        assert_eq!(snap.entity_for_uid(unknown_uid), None, "unknown uid → None");
    }

    // ── BattleSnapshot::unit_at ───────────────────────────────────────────
    // Targets line 683:9.

    #[test]
    fn unit_at_some_for_occupied_none_for_empty() {
        let pos = hex_from_offset(3, 3);
        let other_pos = hex_from_offset(5, 5);
        let u = UnitBuilder::new(1, Team::Player, pos).build();
        let snap = snapshot_from(vec![u], 1);

        assert!(snap.unit_at(pos).is_some(), "occupied position → Some");
        assert!(snap.unit_at(other_pos).is_none(), "empty position → None");
    }

    // ── BattleSnapshot::new_with_id_map ──────────────────────────────────
    // Targets line 671:9 — verifies both maps are populated via the explicit path.

    #[test]
    fn new_with_id_map_populates_both_directions() {
        use combat_engine::state::{CombatState, RoundPhase, UnitId};
        let entity = Entity::from_raw_u32(99).expect("valid");
        let uid = UnitId(42); // intentionally different from entity.to_bits()

        let cache = crate::combat::ai::world::cache::AiCache::from_units(vec![]);
        let state = CombatState::new(vec![], 1, RoundPhase::ActorTurn, 0);

        let snap = BattleSnapshot::new_with_id_map(state, cache, &[(entity, uid)]);
        assert_eq!(snap.entity_for_uid(uid), Some(entity));
        assert_eq!(snap.uid_for_entity(entity), Some(uid));
    }

    // ── BattleSnapshot::enemies_of / all_enemies_of / dead_enemies_of ────
    // Targets the filtering predicates at lines 692-731.
    // The basic happy-path is covered in `dead_units_stay_in_snapshot_and_are_filtered_by_default`;
    // these tests add targeted cases for filter combinations.

    #[test]
    fn enemies_of_excludes_dead_and_same_team() {
        // Scenario: 1 live enemy, 1 dead enemy, 1 live ally.
        let live_enemy = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(10)
            .build();
        let dead_enemy = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0))
            .hp(0)
            .build();
        let live_ally = UnitBuilder::new(3, Team::Player, hex_from_offset(2, 0))
            .hp(10)
            .build();
        let snap = snapshot_from(vec![live_enemy, dead_enemy, live_ally], 1);

        // Queried from Player's perspective:
        // enemies_of(Player) → only live Enemy units.
        assert_eq!(
            snap.enemies_of(Team::Player).count(),
            1,
            "only live enemies counted"
        );
        // allies_of(Player) → only live Player units.
        assert_eq!(snap.allies_of(Team::Player).count(), 1, "ally visible");
    }

    #[test]
    fn all_enemies_of_includes_dead_enemies() {
        let live_enemy = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(10)
            .build();
        let dead_enemy = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0))
            .hp(0)
            .build();
        let snap = snapshot_from(vec![live_enemy, dead_enemy], 1);

        assert_eq!(
            snap.all_enemies_of(Team::Player).count(),
            2,
            "all_enemies_of includes corpses"
        );
        assert_eq!(
            snap.dead_enemies_of(Team::Player).count(),
            1,
            "dead_enemies_of returns only corpses"
        );
        assert_eq!(
            snap.enemies_of(Team::Player).count(),
            1,
            "enemies_of returns only live enemies"
        );
    }

    // ── BattleSnapshot::rebuild_index ────────────────────────────────────
    // Targets lines 612:42 and 621:42 (both && → ||).

    #[test]
    fn rebuild_index_populates_maps_when_empty() {
        let entity = Entity::from_raw_u32(5).expect("valid");
        let snap_unit = UnitBuilder::new(5, Team::Player, hex_from_offset(0, 0))
            .hp(10)
            .build();

        // Build a snapshot via new() to get a properly-seeded uid_to_entity.
        // Then manually clear both maps to simulate deserialization state.
        let mut snap = snapshot_from(vec![snap_unit.clone()], 1);
        snap.uid_to_entity.clear();
        snap.entity_to_uid.clear();

        // After clearing, lookup must fail.
        assert!(snap.unit(entity).is_none(), "must fail before rebuild");

        snap.rebuild_index();

        // After rebuild, both maps must be repopulated.
        assert!(
            snap.uid_for_entity(entity).is_some(),
            "entity_to_uid populated"
        );
        let uid = snap.uid_for_entity(entity).unwrap();
        assert_eq!(
            snap.entity_for_uid(uid),
            Some(entity),
            "uid_to_entity populated"
        );
    }

    #[test]
    fn rebuild_index_no_op_when_state_empty() {
        use combat_engine::state::{CombatState, RoundPhase};
        let cache = crate::combat::ai::world::cache::AiCache::from_units(vec![]);
        let state = CombatState::new(vec![], 1, RoundPhase::ActorTurn, 0);
        let mut snap = BattleSnapshot::new(state, cache);

        // Maps are already empty and state is empty — rebuild_index must not panic.
        snap.rebuild_index();
        assert!(snap.uid_to_entity.is_empty());
        assert!(snap.entity_to_uid.is_empty());
    }
}

// ── env_severity precompute ───────────────────────────────────────────────────

#[cfg(test)]
mod env_severity_snapshot_tests {
    use super::*;
    use crate::combat::ai::scoring::policy::env_severity::severity;
    use crate::combat::ai::world::snapshot::UnitView;
    use crate::content::content_view::ContentView;
    use crate::game::hex::hex_from_offset;
    use combat_engine::state::{EnvId, EnvKind, EnvObject, Team as EngineTeam, TeamSet};

    fn visible_env(id: u32, ability: &str, team: EngineTeam) -> EnvObject {
        EnvObject {
            id: EnvId(id),
            hex: hex_from_offset(0, 0),
            kind: EnvKind::Hazard,
            ability: combat_engine::AbilityId::from(ability),
            owner: Some(team),
            revealed_to: TeamSet::EMPTY,
        }
    }

    fn hidden_env(id: u32, ability: &str) -> EnvObject {
        // Neutral hazard owned by nobody — invisible until revealed.
        EnvObject {
            id: EnvId(id),
            hex: hex_from_offset(0, 0),
            kind: EnvKind::Hazard,
            ability: combat_engine::AbilityId::from(ability),
            owner: None,
            revealed_to: TeamSet::EMPTY,
        }
    }

    /// A visible trap's `EnvId` is present in `env_severity` with the expected
    /// value; a non-visible (neutral, unrevealed) trap is absent because the T3
    /// visibility filter (`retain`) removes it before the precompute loop sees it.
    ///
    /// This mirrors the `build_snapshot` wiring: iterate the already-filtered
    /// `combat_state.environment` (after `retain(|e| e.visible_to(ai_team))`).
    #[test]
    fn snapshot_populates_env_severity_for_visible_traps() {
        let content = ContentView::load_global_for_tests();
        let (neutral_ref_u, neutral_ref_c) =
            crate::combat::ai::scoring::policy::env_severity::neutral_reference_pair();
        let neutral_ref = UnitView {
            state: &neutral_ref_u,
            cache: &neutral_ref_c,
        };
        let ai_team = EngineTeam::Enemy;

        // One trap owned by the AI team (visible), one neutral unrevealed (hidden).
        // Use "spike_trap" — present in test content and has a Damage effect.
        let visible_id = EnvId(1u32);
        let hidden_id = EnvId(2u32);
        let ability_id = combat_engine::AbilityId::from("spike_trap");

        let mut environment = vec![
            visible_env(1u32, "spike_trap", ai_team),
            hidden_env(2u32, "spike_trap"),
        ];
        // Replicate the T3 filter from build_snapshot.
        environment.retain(|e| e.visible_to(ai_team));

        // Build env_severity map via the same loop as build_snapshot.
        let mut env_severity: std::collections::HashMap<EnvId, f32> =
            std::collections::HashMap::new();
        for env_obj in &environment {
            let sev = severity(&env_obj.ability, &content, neutral_ref);
            env_severity.insert(env_obj.id, sev);
        }

        // Visible trap: must be present with severity matching the standalone fn.
        let expected_sev = severity(&ability_id, &content, neutral_ref);
        assert!(
            env_severity.contains_key(&visible_id),
            "visible (enemy-owned) trap must appear in env_severity",
        );
        assert!(
            (env_severity[&visible_id] - expected_sev).abs() < 1e-6,
            "severity value mismatch: map={} fn={}",
            env_severity[&visible_id],
            expected_sev,
        );

        // Hidden trap: must be absent (filtered by T3 retain before this loop).
        assert!(
            !env_severity.contains_key(&hidden_id),
            "neutral unrevealed trap must be absent from env_severity (T3 filter)",
        );
    }
}
