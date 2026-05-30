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
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::content::abilities::{AbilityRange, AoEShape, EffectDef, ResourceCost};
    use combat_engine::DiceExpr;
    use crate::game::hex::hex_from_offset;

    fn base_unit() -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw_u32(1).expect("valid"),
            team: Team::Enemy,
            role: AxisProfile { tank: 0.5, melee: 0.5, ..Default::default() },
            pos: hex_from_offset(0, 0),
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: 2,
            max_ap: 2,
            movement_points: 3,
            base_speed: 3,
            speed: 3,
            mana: Some((5, 10)),
            rage: Some((3, 10)),
            energy: Some((4, 10)),
            abilities: Vec::new(),
            threat: 0.0,
            tags: AiTags::empty(),
            max_attack_range: 1,
            summoner: None,
            reactions_left: 1,
            aoo_expected_damage: None,
            statuses: Vec::new(),
            caster_ctx: Default::default(),
            crit_fail_effect: Default::default(),
            damage_horizon: Vec::new(),
            ai_tuning_override: None,
            forced_mode: None,
        }
    }

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
                effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
                costs,
                cost_ap,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: None,
            },
        }
    }

    fn cost(kind: ResourceKind, amount: i32) -> ResourceCost {
        ResourceCost { resource: kind, amount }
    }

    #[test]
    fn can_afford_covers_ap_and_all_resource_kinds() {
        let u = base_unit();
        // (name, ap_cost, costs, expected can_afford)
        let cases: Vec<(&str, i32, Vec<ResourceCost>, bool)> = vec![
            ("free ability",        1, vec![],                              true),
            ("AP shortage",         3, vec![],                              false),
            ("mana ok",             1, vec![cost(ResourceKind::Mana, 5)],   true),
            ("mana short",          1, vec![cost(ResourceKind::Mana, 6)],   false),
            ("rage ok",             1, vec![cost(ResourceKind::Rage, 3)],   true),
            ("rage short",          1, vec![cost(ResourceKind::Rage, 4)],   false),
            ("energy ok",           1, vec![cost(ResourceKind::Energy, 4)], true),
            ("energy short",        1, vec![cost(ResourceKind::Energy, 5)], false),
            ("hp ok",               1, vec![cost(ResourceKind::Hp, 20)],    true),
            ("hp short",            1, vec![cost(ResourceKind::Hp, 21)],    false),
            ("two costs both ok",   1, vec![cost(ResourceKind::Mana, 5), cost(ResourceKind::Rage, 3)], true),
            ("two costs one short", 1, vec![cost(ResourceKind::Mana, 5), cost(ResourceKind::Rage, 4)], false),
        ];
        for (name, ap_cost, costs, want) in cases {
            let d = def(ap_cost, costs);
            assert_eq!(u.can_afford(&d), want, "{name}");
        }
    }

    #[test]
    fn resource_amount_treats_absent_option_pools_as_zero() {
        let mut u = base_unit();
        u.mana = None;
        u.rage = None;
        u.energy = None;
        assert_eq!(u.resource_amount(ResourceKind::Mana), 0);
        assert_eq!(u.resource_amount(ResourceKind::Rage), 0);
        assert_eq!(u.resource_amount(ResourceKind::Energy), 0);
        assert_eq!(u.resource_amount(ResourceKind::Hp), u.hp);
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
        let alive = base_unit();
        let mut corpse = base_unit();
        corpse.entity = Entity::from_raw_u32(2).expect("valid");
        corpse.team = Team::Player;
        corpse.hp = 0;
        let snap = snapshot_from(vec![alive.clone(), corpse.clone()], 1);

        assert!(snap.unit(corpse.entity).is_some(), "corpse must stay in units");
        assert_eq!(
            snap.unit(corpse.entity).map(|u| u.is_alive()),
            Some(false),
            "corpse must report is_alive = false",
        );

        // Default accessors hide the dead.
        assert_eq!(snap.enemies_of(Team::Enemy).count(), 0, "default enemies_of hides dead");
        assert_eq!(snap.allies_of(Team::Enemy).count(), 1, "alive ally still visible");

        // Explicit "all" + "dead" variants surface them.
        assert_eq!(snap.all_enemies_of(Team::Enemy).count(), 1);
        assert_eq!(snap.dead_enemies_of(Team::Enemy).count(), 1);
        assert_eq!(snap.dead_units().count(), 1);
    }
}

#[cfg(test)]
mod snapshot_api_tests {
    use super::*;
    use crate::combat::ai::test_helpers::{empty_status_tag_cache, snapshot_from, UnitBuilder};
    use crate::game::hex::hex_from_offset;
    use crate::game::components::Team;

    fn test_unit() -> UnitSnapshot {
        UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build()
    }

    fn test_status(id: &str) -> ActiveStatusView {
        ActiveStatusView {
            id: StatusId::from(id),
            rounds_remaining: 2,
            dot_per_tick: 0,
        }
    }

    // ── base_speed ────────────────────────────────────────────────────────────

    /// v35 logs lack `base_speed` — deserialise as 0 via `#[serde(default)]`.
    #[test]
    fn base_speed_default_zero_on_v35_deserialise() {
        // Serialize a current unit, then strip `base_speed` to simulate a v35 log.
        let unit = test_unit();
        let json = serde_json::to_string(&unit).expect("serialize");
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        value.as_object_mut().unwrap().remove("base_speed");
        let json_v35 = serde_json::to_string(&value).unwrap();

        let restored: UnitSnapshot = serde_json::from_str(&json_v35).expect("deserialise v35 snapshot");
        assert_eq!(restored.base_speed, 0, "base_speed absent in v35 JSON → deserialises as 0");
        assert_eq!(restored.speed, unit.speed);
    }

    /// base_speed round-trips through JSON (v36+ schema where field is present).
    #[test]
    fn base_speed_serialized_on_round_trip() {
        let mut unit = test_unit();
        unit.base_speed = 3;
        let json = serde_json::to_string(&unit).expect("serialize");
        let restored: UnitSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.base_speed, 3);
    }

    // ── add_status / remove_status / statuses() ───────────────────────────────

    #[test]
    fn add_status_inserts_and_calls_refresh() {
        let mut unit = test_unit();
        let cache = empty_status_tag_cache();
        assert_eq!(unit.statuses().len(), 0);
        unit.add_status(test_status("foo"), cache);
        assert_eq!(unit.statuses().len(), 1);
        assert_eq!(unit.statuses()[0].id, StatusId::from("foo"));
    }

    #[test]
    fn remove_status_returns_true_when_removed_false_when_absent() {
        let mut unit = test_unit();
        let cache = empty_status_tag_cache();
        unit.add_status(test_status("foo"), cache);

        assert!(unit.remove_status(&StatusId::from("foo"), cache), "should return true for existing status");
        assert!(!unit.remove_status(&StatusId::from("nonexistent"), cache), "should return false for absent status");
        assert!(unit.statuses().is_empty(), "no statuses remain");
    }

    #[test]
    fn statuses_accessor_returns_immutable_slice() {
        let mut unit = test_unit();
        let cache = empty_status_tag_cache();
        unit.add_status(test_status("bar"), cache);
        let slice: &[ActiveStatusView] = unit.statuses();
        assert_eq!(slice.len(), 1);
        assert_eq!(slice[0].id, StatusId::from("bar"));
    }

    // ── refresh_aggregates: speed ─────────────────────────────────────────────

    /// Build a minimal `StatusTagCache` containing a single status with the
    /// given tags and bonuses. Used by refresh_aggregates tests to avoid
    /// needing a full `ContentView` load.
    fn cache_with_status(id: &str, tags: StatusTagSet, bonuses: StatusBonuses) -> StatusTagCache {
        let mut c = StatusTagCache::default();
        let sid = StatusId::from(id);
        c.map.insert(sid.clone(), tags);
        c.bonuses.insert(sid, bonuses);
        c
    }

    #[test]
    fn apply_haste_increases_speed() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build();
        let cache = cache_with_status(
            "haste",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: 2, armor_bonus: 0, damage_taken_bonus: 0 },
        );
        unit.add_status(test_status("haste"), &cache);
        assert_eq!(unit.speed, 5, "base 3 + speed_bonus 2 = 5");
    }

    #[test]
    fn apply_slow_decreases_speed() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build();
        let cache = cache_with_status(
            "slow",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: -1, armor_bonus: 0, damage_taken_bonus: 0 },
        );
        unit.add_status(test_status("slow"), &cache);
        assert_eq!(unit.speed, 2, "base 3 + speed_bonus -1 = 2");
    }

    #[test]
    fn expire_haste_restores_speed() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build();
        let cache = cache_with_status(
            "haste",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: 2, armor_bonus: 0, damage_taken_bonus: 0 },
        );
        unit.add_status(test_status("haste"), &cache);
        assert_eq!(unit.speed, 5);
        unit.remove_status(&StatusId::from("haste"), &cache);
        assert_eq!(unit.speed, 3, "after removing haste speed returns to base 3");
    }

    #[test]
    fn multiple_speed_statuses_stack() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .speed(3)
            .build();
        let mut cache = StatusTagCache::default();
        let haste_id = StatusId::from("haste");
        let bless_id = StatusId::from("bless");
        cache.map.insert(haste_id.clone(), StatusTagSet::empty());
        cache.bonuses.insert(haste_id.clone(), StatusBonuses { speed_bonus: 2, armor_bonus: 0, damage_taken_bonus: 0 });
        cache.map.insert(bless_id.clone(), StatusTagSet::empty());
        cache.bonuses.insert(bless_id.clone(), StatusBonuses { speed_bonus: 1, armor_bonus: 0, damage_taken_bonus: 0 });

        unit.add_status(test_status("haste"), &cache);
        unit.add_status(test_status("bless"), &cache);
        assert_eq!(unit.speed, 6, "base 3 + haste(+2) + bless(+1) = 6");
    }

    #[test]
    fn apply_armor_buff_recomputes_armor_bonus() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0)).build();
        let cache = cache_with_status(
            "stone_skin",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: 0, armor_bonus: 5, damage_taken_bonus: 0 },
        );
        unit.add_status(test_status("stone_skin"), &cache);
        assert_eq!(unit.armor_bonus, 5);
    }

    #[test]
    fn apply_vulnerability_recomputes_damage_taken_bonus() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0)).build();
        let cache = cache_with_status(
            "vuln",
            StatusTagSet::empty(),
            StatusBonuses { speed_bonus: 0, armor_bonus: 0, damage_taken_bonus: 3 },
        );
        unit.add_status(test_status("vuln"), &cache);
        assert_eq!(unit.damage_taken_bonus, 3);
    }

    #[test]
    fn hard_cc_status_makes_unit_is_stunned() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0)).build();
        let cache = cache_with_status(
            "stun",
            StatusTagSet::HARD_CC,
            StatusBonuses::default(),
        );
        unit.add_status(test_status("stun"), &cache);
        assert!(unit.is_stunned(&cache), "HARD_CC status must make is_stunned true");

        unit.remove_status(&StatusId::from("stun"), &cache);
        assert!(!unit.is_stunned(&cache), "removing stun must clear is_stunned");
    }

    #[test]
    fn compulsion_status_makes_unit_forces_targeting() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0)).build();
        let cache = cache_with_status(
            "taunted",
            StatusTagSet::COMPULSION,
            StatusBonuses::default(),
        );
        unit.add_status(test_status("taunted"), &cache);
        assert!(unit.forces_targeting(&cache), "COMPULSION status must make forces_targeting true");

        unit.remove_status(&StatusId::from("taunted"), &cache);
        assert!(!unit.forces_targeting(&cache), "removing taunt must clear forces_targeting");
    }

    #[test]
    fn refresh_preserves_non_status_tags() {
        let mut unit = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .tags(AiTags::LOW_HP | AiTags::MELEE_ONLY)
            .build();
        let cache = cache_with_status(
            "stun",
            StatusTagSet::HARD_CC,
            StatusBonuses::default(),
        );
        unit.add_status(test_status("stun"), &cache);

        // is_stunned reflected via lazy method (no longer a tag bit post-Path-E)
        assert!(unit.is_stunned(&cache));
        // Non-status-derived tag bits must be untouched by refresh_aggregates
        assert!(unit.tags.contains(AiTags::LOW_HP), "LOW_HP must survive refresh");
        assert!(unit.tags.contains(AiTags::MELEE_ONLY), "MELEE_ONLY must survive refresh");
    }

    /// Parity test: `BattleSnapshot::view(e).state` must agree with the
    /// corresponding `UnitSnapshot` on hp, pos, and ap.
    /// Catches divergence while both representations coexist (D-step-2 → D-step-5).
    #[test]
    fn view_state_matches_unit_snapshot_basic_fields() {
        use combat_engine::state::{CombatState, RoundPhase, Team as EngineTeam, Unit as EngineUnit, UnitId};

        let pos = hex_from_offset(2, 3);
        let entity = Entity::from_raw_u32(42).expect("valid");
        let uid = UnitId(entity.to_bits());

        // Build matching UnitSnapshot and engine Unit with the same fields.
        let snap_unit = UnitBuilder::new(42, Team::Player, pos)
            .hp(15)
            .ap(2)
            .build();

        let engine_unit = EngineUnit::new(
            uid,
            EngineTeam::Player,
            pos,
            0,  // armor
            0,  // armor_bonus
            0,  // damage_taken_bonus
            3,  // base_speed
            3,  // speed
            0,  // reactions_left
            1,  // reactions_max
            vec![],
            None,
            Default::default(),
            None,
            Vec::new(),
            Vec::new(),
            combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Hp     => Some((15, 15)),
                combat_engine::PoolKind::Mana   => None,
                combat_engine::PoolKind::Rage   => None,
                combat_engine::PoolKind::Energy => None,
                combat_engine::PoolKind::Ap     => Some((2, 2)),
                combat_engine::PoolKind::Mp     => Some((3, 3)),
            },
            combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Hp     => combat_engine::RegenRule::None,
                combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
                combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
                combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
                combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
                combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
            },
            None,
        );

        let combat_state = CombatState::new(
            vec![engine_unit],
            1,
            RoundPhase::ActorTurn,
            0,
        );

        let mut snap = snapshot_from(vec![snap_unit.clone()], 1);
        snap.state = combat_state;

        let view = snap.unit(entity).expect("view must resolve for known entity");
        assert_eq!(view.hp(), snap_unit.hp, "view.hp must match UnitSnapshot.hp");
        assert_eq!(view.pos, snap_unit.pos, "view.pos must match UnitSnapshot.pos");
        // AP is read from pools[Ap] on engine Unit.
        let view_ap = view.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0);
        assert_eq!(view_ap, snap_unit.action_points, "view.ap must match");
    }
}

// ── Targeted mutation-killing tests ──────────────────────────────────────────
//
// These tests are specifically written to catch arithmetic-operator and
// constant-replacement mutants identified in the Phase 4b mutation baseline
// (measurements/mutants-snapshot-before/mutants.out/missed.txt).
// Each test function documents the mutant line(s) it targets.

#[cfg(test)]
mod computation_tests {
    use super::*;
    use crate::combat::ai::test_helpers::{snapshot_from, UnitBuilder};
    use crate::game::hex::hex_from_offset;
    use crate::game::components::Team;
    use crate::content::abilities::{AbilityRange, AoEShape, EffectDef, ResourceCost};
    use combat_engine::DiceExpr;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Minimal UnitSnapshot with explicit hp/max_hp/armor/armor_bonus.
    fn unit_with(hp: i32, max_hp: i32, armor: i32, armor_bonus: i32) -> UnitSnapshot {
        UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .hp(hp)
            .max_hp(max_hp)
            .armor(armor)
            .armor_bonus(armor_bonus)
            .build()
    }

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
                effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
                costs: Vec::new(),
                cost_ap,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: None,
            },
        }
    }

    fn def_with_cost(cost_ap: i32, resource: ResourceKind, amount: i32) -> AbilityDef {
        let mut d = def_ap(cost_ap);
        d.engine.costs = vec![ResourceCost { resource, amount }];
        d
    }

    // ── UnitSnapshot::is_alive ─────────────────────────────────────────────
    // Targets lines 306:9 (replace bool), 306:17 (replace > with ==/</>= ).

    #[test]
    fn unit_snapshot_is_alive_boundary() {
        // hp > 0 → true; hp = 0 → false; hp < 0 → false.
        let cases: &[(i32, bool)] = &[
            (1,  true),   // kills "replace > with ==" and "replace with false"
            (10, true),   // kills "replace with false"
            (0,  false),  // kills "replace > with >="
            (-1, false),  // kills "replace > with <"
        ];
        for &(hp, expected) in cases {
            let u = unit_with(hp, 20, 0, 0);
            assert_eq!(u.is_alive(), expected, "hp={hp} → is_alive={expected}");
        }
    }

    // ── UnitSnapshot::eff_hp ──────────────────────────────────────────────
    // Targets lines 312:9 (const replacement), 312:17/312:30 (+ → - / *).

    #[test]
    fn unit_snapshot_eff_hp_additive() {
        let cases: &[(i32, i32, i32, i32)] = &[
            // (hp, armor, armor_bonus, expected)
            (5,  0, 0,  5),    // baseline — kills const-0, const-1, const-(-1)
            (5,  3, 0,  8),    // base armor — kills first + → -  (5-3=2 ≠8)
            (5,  0, 2,  7),    // armor_bonus — kills second + → - (5-2=3 ≠7)
            (5,  3, 2, 10),    // both — kills + → * (5*3*2=30 ≠10)
            (0,  5, 0,  5),    // zero hp — kills + → - (0-5=-5 ≠5)
        ];
        for &(hp, armor, ab, expected) in cases {
            let u = unit_with(hp, 20, armor, ab);
            assert_eq!(u.eff_hp(), expected, "hp={hp} armor={armor} ab={ab}");
        }
    }

    // ── UnitSnapshot::eff_max_hp ──────────────────────────────────────────
    // Targets lines 317:9 (const), 317:22/317:35 (+ → - / *), clamp ≥ 1.

    #[test]
    fn unit_snapshot_eff_max_hp_additive_and_clamp() {
        let cases: &[(i32, i32, i32, i32)] = &[
            (10, 0, 0, 10),   // baseline — kills const-0, const-1, const-(-1)
            (10, 3, 0, 13),   // + armor — kills + → - (10-3=7 ≠13)
            (10, 0, 2, 12),   // + armor_bonus — kills + → - (10-2=8 ≠12)
            (10, 3, 2, 15),   // both — kills + → * (10*3*2=60 ≠15)
            (-5, 0, 0,  1),   // clamped to 1 — kills missing .max(1)
        ];
        for &(max_hp, armor, ab, expected) in cases {
            let u = unit_with(5, max_hp, armor, ab);
            assert_eq!(u.eff_max_hp(), expected, "max_hp={max_hp} armor={armor} ab={ab}");
        }
    }

    // ── UnitSnapshot::hp_pct ──────────────────────────────────────────────
    // Targets lines 323:9 (const), 323:24 (/ → % / *).

    #[test]
    fn unit_snapshot_hp_pct_correctness() {
        // hp_pct = hp / max_hp.max(1).
        let cases: &[(i32, i32, f32)] = &[
            (10, 10, 1.0),  // full HP — kills const-0, const-(-1), / → * (100 ≠1)
            (5,  10, 0.5),  // half — kills / → % (5%10=5 ≠0.5), / → * (50 ≠0.5)
            (0,  10, 0.0),  // dead
            (5,   0, 5.0),  // div-by-zero guard: max_hp clamped to 1 → 5/1=5.0
        ];
        for &(hp, max_hp, expected) in cases {
            let u = unit_with(hp, max_hp, 0, 0);
            let got = u.hp_pct();
            assert!((got - expected).abs() < 1e-5,
                "hp={hp} max_hp={max_hp}: got {got}, expected {expected}");
        }
    }

    // ── UnitSnapshot::killability ─────────────────────────────────────────
    // Targets lines 331:9 (const 0/1/-1), 332:20 (<= → >), 335:13 (- → +/÷),
    // 335:37 (/ → % / *).

    #[test]
    fn unit_snapshot_killability_correctness() {
        // killability = 1 - eff_hp / eff_max_hp.
        // We set armor=0 so eff_hp=hp, eff_max_hp=max_hp.
        let cases: &[(i32, i32, f32)] = &[
            (10, 10, 0.0),  // full HP → 0.0; kills const-1/const-(-1), - → +
            (0,  10, 1.0),  // dead → 1.0; kills const-0, - → + (1+0=1 trivially ok so need below)
            (5,  10, 0.5),  // half → 0.5; kills - → + (1+0.5=1.5), / → * (1-5=... ), / → %
            (1,  10, 0.9),  // near-dead → 0.9; kills <= → > (eff_max=10>0, not ≤0)
        ];
        for &(hp, max_hp, expected) in cases {
            let u = unit_with(hp, max_hp, 0, 0);
            let got = u.killability();
            assert!((got - expected).abs() < 1e-5,
                "hp={hp} max_hp={max_hp}: got {got}, expected {expected}");
        }
    }

    #[test]
    fn unit_snapshot_killability_dead_unit_guard() {
        // When eff_max_hp clamps to 1 and eff_hp ≤ 0, killability ≥ 1.
        // Kills <= → > guard mutation: if eff_max>0 we should NOT return 0.0 early.
        let u = unit_with(0, 0, 0, 0);  // max_hp=0 → eff_max clamped to 1; hp=0 → eff_hp=0
        let got = u.killability();
        // 1 - 0/1 = 1.0
        assert!((got - 1.0).abs() < 1e-5, "dead unit: got {got}, expected 1.0");
    }

    // ── UnitSnapshot::resource_amount ────────────────────────────────────
    // (Already covered by affordability_tests; adding explicit positive cases.)

    #[test]
    fn unit_snapshot_resource_amount_returns_correct_pool() {
        let u = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .hp(15)
            .mana(7, 10)
            .rage(3, 10)
            .energy(4, 10)
            .build();
        assert_eq!(u.resource_amount(ResourceKind::Hp),     15);
        assert_eq!(u.resource_amount(ResourceKind::Mana),    7);
        assert_eq!(u.resource_amount(ResourceKind::Rage),    3);
        assert_eq!(u.resource_amount(ResourceKind::Energy),  4);
    }

    // ── UnitSnapshot::can_afford ──────────────────────────────────────────
    // Targets lines 352/356 (>= → <, && → ||) — boundary edges.

    #[test]
    fn unit_snapshot_can_afford_ap_boundary() {
        // AP exactly equal to cost: must be true.  AP one less: must be false.
        // Kills >= → < (exact match would flip).
        let u = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .ap(2)
            .build();
        assert!( u.can_afford(&def_ap(2)), "AP == cost must succeed");
        assert!(!u.can_afford(&def_ap(3)), "AP < cost must fail");
    }

    #[test]
    fn unit_snapshot_can_afford_resource_boundary() {
        // Resource exactly equal to cost: must be true.  One more: must be false.
        // Kills && → ||: both conditions must be required independently.
        let u = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .ap(2)
            .mana(5, 10)
            .build();
        // Enough AP but not enough mana: overall false (kills && → ||).
        let not_enough_mana = def_with_cost(2, ResourceKind::Mana, 6);
        assert!(!u.can_afford(&not_enough_mana), "short mana must fail even with enough AP");
        // Enough mana but not enough AP: overall false (symmetric check).
        let not_enough_ap = def_with_cost(3, ResourceKind::Mana, 5);
        assert!(!u.can_afford(&not_enough_ap), "short AP must fail even with enough mana");
        // Both exactly sufficient.
        let exact = def_with_cost(2, ResourceKind::Mana, 5);
        assert!(u.can_afford(&exact), "exact AP and mana must succeed");
    }

    // ── default_reactions_left ────────────────────────────────────────────
    // Targets line 296:38 (replace return value with 0 / -1).

    #[test]
    fn default_reactions_left_returns_one() {
        assert_eq!(default_reactions_left(), 1);
    }

    // ── UnitView (lines 239-274) ──────────────────────────────────────────
    // UnitView wraps the engine Unit (via Deref); its arithmetic is identical
    // to UnitSnapshot's.  We use snapshot_from + snap.unit(entity) to get a
    // real UnitView, then exercise the same edge-case table.
    // Targets lines 240:23/42, 245:28/47, 255:9, 256:20, 257:13, 273:9/34, 274:74.

    fn snap_view_with(entity_raw: u32, hp: i32, max_hp: i32, armor: i32, armor_bonus: i32)
        -> (BattleSnapshot, Entity)
    {
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
            (5,  0, 0,  5),
            (5,  3, 0,  8),
            (5,  0, 2,  7),
            (5,  3, 2, 10),
            (0,  5, 0,  5),
        ];
        for (idx, &(hp, armor, ab, expected)) in cases.iter().enumerate() {
            let (snap, entity) = snap_view_with((idx + 1) as u32, hp, 20, armor, ab);
            let view = snap.unit(entity).expect("view");
            assert_eq!(view.eff_hp(), expected,
                "hp={hp} armor={armor} ab={ab}");
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
            (-5, 0, 0,  1),  // clamp to 1
        ];
        for (idx, &(max_hp, armor, ab, expected)) in cases.iter().enumerate() {
            let (snap, entity) = snap_view_with((idx + 1) as u32, 5, max_hp, armor, ab);
            let view = snap.unit(entity).expect("view");
            assert_eq!(view.eff_max_hp(), expected,
                "max_hp={max_hp} armor={armor} ab={ab}");
        }
    }

    #[test]
    fn unit_view_hp_pct_correctness() {
        // hp_pct = hp / max_hp.max(1). Tests line 249 (UnitView path).
        let cases: &[(u32, i32, i32, f32)] = &[
            (1, 10, 10, 1.0),
            (2,  5, 10, 0.5),
            (3,  0, 10, 0.0),
            (4,  5,  0, 5.0),  // clamp guard
        ];
        for &(raw, hp, max_hp, expected) in cases {
            let (snap, entity) = snap_view_with(raw, hp, max_hp, 0, 0);
            let view = snap.unit(entity).expect("view");
            let got = view.hp_pct();
            assert!((got - expected).abs() < 1e-5,
                "hp={hp} max_hp={max_hp}: got {got}, expected {expected}");
        }
    }

    #[test]
    fn unit_view_killability_correctness() {
        // Lines 255:9, 256:20, 257:13.
        let cases: &[(u32, i32, i32, f32)] = &[
            (1, 10, 10, 0.0),  // full HP
            (2,  5, 10, 0.5),  // half HP
            (3,  0, 10, 1.0),  // dead
            (4,  1, 10, 0.9),  // near-dead
        ];
        for &(raw, hp, max_hp, expected) in cases {
            let (snap, entity) = snap_view_with(raw, hp, max_hp, 0, 0);
            let view = snap.unit(entity).expect("view");
            let got = view.killability();
            assert!((got - expected).abs() < 1e-5,
                "hp={hp} max_hp={max_hp}: got {got}, expected {expected}");
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

        assert!( view.can_afford(&def_ap(2)),                              "AP == cost");
        assert!(!view.can_afford(&def_ap(3)),                              "AP < cost");
        assert!(!view.can_afford(&def_with_cost(2, ResourceKind::Mana, 6)), "short mana fails");
        assert!(!view.can_afford(&def_with_cost(3, ResourceKind::Mana, 5)), "short AP fails even with ok mana");
        assert!( view.can_afford(&def_with_cost(2, ResourceKind::Mana, 5)), "exact ok");
    }

    // ── BattleSnapshot::entity_for_uid / uid_for_entity ──────────────────
    // Targets line 645:9 (entity_for_uid returns Option).

    #[test]
    fn entity_for_uid_some_for_known_none_for_unknown() {
        let entity = Entity::from_raw_u32(7).expect("valid");
        let u = UnitBuilder::new(7, Team::Player, hex_from_offset(0, 0)).build();
        let snap = snapshot_from(vec![u], 1);
        let uid = snap.uid_for_entity(entity).expect("uid must be known");
        assert_eq!(snap.entity_for_uid(uid), Some(entity), "known uid → Some(entity)");

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

        assert!(snap.unit_at(pos).is_some(),       "occupied position → Some");
        assert!(snap.unit_at(other_pos).is_none(),  "empty position → None");
    }

    // ── BattleSnapshot::new_with_id_map ──────────────────────────────────
    // Targets line 671:9 — verifies both maps are populated via the explicit path.

    #[test]
    fn new_with_id_map_populates_both_directions() {
        use combat_engine::state::{CombatState, RoundPhase, UnitId};
        let entity = Entity::from_raw_u32(99).expect("valid");
        let uid = UnitId(42);  // intentionally different from entity.to_bits()

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
        let live_enemy = UnitBuilder::new(1, Team::Enemy,  hex_from_offset(0, 0)).hp(10).build();
        let dead_enemy = UnitBuilder::new(2, Team::Enemy,  hex_from_offset(1, 0)).hp(0).build();
        let live_ally  = UnitBuilder::new(3, Team::Player, hex_from_offset(2, 0)).hp(10).build();
        let snap = snapshot_from(vec![live_enemy, dead_enemy, live_ally], 1);

        // Queried from Player's perspective:
        // enemies_of(Player) → only live Enemy units.
        assert_eq!(snap.enemies_of(Team::Player).count(), 1,
            "only live enemies counted");
        // allies_of(Player) → only live Player units.
        assert_eq!(snap.allies_of(Team::Player).count(), 1,
            "ally visible");
    }

    #[test]
    fn all_enemies_of_includes_dead_enemies() {
        let live_enemy = UnitBuilder::new(1, Team::Enemy,  hex_from_offset(0, 0)).hp(10).build();
        let dead_enemy = UnitBuilder::new(2, Team::Enemy,  hex_from_offset(1, 0)).hp(0).build();
        let snap = snapshot_from(vec![live_enemy, dead_enemy], 1);

        assert_eq!(snap.all_enemies_of(Team::Player).count(), 2,
            "all_enemies_of includes corpses");
        assert_eq!(snap.dead_enemies_of(Team::Player).count(), 1,
            "dead_enemies_of returns only corpses");
        assert_eq!(snap.enemies_of(Team::Player).count(), 1,
            "enemies_of returns only live enemies");
    }

    // ── BattleSnapshot::rebuild_index ────────────────────────────────────
    // Targets lines 612:42 and 621:42 (both && → ||).

    #[test]
    fn rebuild_index_populates_maps_when_empty() {
        let entity = Entity::from_raw_u32(5).expect("valid");
        let snap_unit = UnitBuilder::new(5, Team::Player, hex_from_offset(0, 0)).hp(10).build();

        // Build a snapshot via new() to get a properly-seeded uid_to_entity.
        // Then manually clear both maps to simulate deserialization state.
        let mut snap = snapshot_from(vec![snap_unit.clone()], 1);
        snap.uid_to_entity.clear();
        snap.entity_to_uid.clear();

        // After clearing, lookup must fail.
        assert!(snap.unit(entity).is_none(), "must fail before rebuild");

        snap.rebuild_index();

        // After rebuild, both maps must be repopulated.
        assert!(snap.uid_for_entity(entity).is_some(), "entity_to_uid populated");
        let uid = snap.uid_for_entity(entity).unwrap();
        assert_eq!(snap.entity_for_uid(uid), Some(entity), "uid_to_entity populated");
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
