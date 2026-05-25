//! Tests for `classify.rs` — split from the source file via `#[path]` in
//! `classify.rs` (see end of that file). Production code stays in
//! `classify.rs`; this file holds the test module body.
//!
//! Split per [docs/testing.md §2](../../../../../docs/testing.md):
//! `classify.rs` had ~70% of its lines in tests (711 LOC total, ~500 test
//! LOC). Splitting keeps the production logic immediately visible.

use super::*;
use combat_engine::AbilityId;

// ── Helpers ───────────────────────────────────────────────────────────────

fn load_content() -> crate::content::content_view::ContentView {
    crate::content::content_view::ContentView::load_global_for_tests()
}

fn build_status_lookup(
    content: &crate::content::content_view::ContentView,
) -> HashMap<StatusId, StatusTagSet> {
    content
        .statuses
        .iter()
        .map(|(id, def)| (id.clone(), derive_status_tags(def)))
        .collect()
}

fn ability(id: &str, content: &crate::content::content_view::ContentView) -> AbilityDef {
    content
        .abilities
        .get(&AbilityId::from(id))
        .unwrap_or_else(|| panic!("ability '{id}' not found in content"))
        .clone()
}

fn status_def(
    id: &str,
    content: &crate::content::content_view::ContentView,
) -> StatusDef {
    content
        .statuses
        .get(&StatusId::from(id))
        .unwrap_or_else(|| panic!("status '{id}' not found in content"))
        .clone()
}

// ── Synthesized-input helpers ─────────────────────────────────────────────
//
// Phase 3: rule-based tests use minimal synthesized StatusDef / AbilityDef
// objects instead of loading the real TOML content. This gives:
//   - branch-level mutation discrimination (each test hits one OR-branch)
//   - no coupling to content edits (rename of "fireball" doesn't break tests)
//   - faster runtime (no TOML I/O)
//
// The remaining content-pin tests (load_content + status_def / ability) live
// in the `// Special/regression tests` section below and intentionally couple
// to specific TOML entries — they are guards on content↔classifier alignment
// for taunt/taunted (the most behaviour-critical pair).

fn empty_engine_status() -> combat_engine::StatusDef {
    combat_engine::StatusDef {
        causes_disadvantage: false,
        blocks_mana_abilities: false,
        forces_targeting: false,
        skips_turn: false,
        bonuses: combat_engine::StatusBonuses::default(),
        hp_percent_dot: 0,
    }
}

fn make_status(engine: combat_engine::StatusDef) -> StatusDef {
    StatusDef {
        id: StatusId::from("test_status"),
        name: "test_status".to_string(),
        dot_dice: None,
        ai_controlled: false,
        buff_class: None,
        engine,
    }
}

fn make_ability(
    effect: crate::content::abilities::EffectDef,
    target_type: crate::content::abilities::TargetType,
    statuses: Vec<crate::content::abilities::StatusApplication>,
) -> AbilityDef {
    AbilityDef {
        id: AbilityId::from("test_ability"),
        name: "test_ability".to_string(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            key: None,
            cost_ap: 1,
            costs: vec![],
            range: combat_engine::content::AbilityRange::SELF_ONLY,
            target_type,
            aoe: combat_engine::content::AoEShape::None,
            friendly_fire: false,
            effect,
            statuses,
        },
    }
}

fn empty_lookup() -> HashMap<StatusId, StatusTagSet> {
    HashMap::new()
}

fn lookup_with(id: &str, tags: StatusTagSet) -> HashMap<StatusId, StatusTagSet> {
    let mut m = HashMap::new();
    m.insert(StatusId::from(id), tags);
    m
}

fn defs_with_forces_targeting(id: &str) -> HashMap<StatusId, StatusDef> {
    let mut eng = empty_engine_status();
    eng.forces_targeting = true;
    let mut m = HashMap::new();
    m.insert(StatusId::from(id), make_status(eng));
    m
}

fn status_app(id: &str, on: crate::content::abilities::StatusOn) -> crate::content::abilities::StatusApplication {
    crate::content::abilities::StatusApplication {
        status: StatusId::from(id),
        duration_rounds: 1,
        on,
    }
}

// ── derive_status_tags rule tests ─────────────────────────────────────────

#[test]
fn status_skips_turn_yields_hard_cc() {
    let mut eng = empty_engine_status();
    eng.skips_turn = true;
    assert_eq!(derive_status_tags(&make_status(eng)), StatusTagSet::HARD_CC);
}

#[test]
fn status_disadvantage_yields_soft_cc() {
    let mut eng = empty_engine_status();
    eng.causes_disadvantage = true;
    assert_eq!(derive_status_tags(&make_status(eng)), StatusTagSet::SOFT_CC);
}

#[test]
fn status_negative_speed_yields_soft_cc() {
    let mut eng = empty_engine_status();
    eng.bonuses.speed_bonus = -1;
    assert_eq!(derive_status_tags(&make_status(eng)), StatusTagSet::SOFT_CC);
}

#[test]
fn status_positive_speed_alone_yields_cosmetic() {
    // Classifier rule: only `speed_bonus < 0` maps to SoftCC. Positive
    // speed_bonus is not recognised as a Buff — falls through to Cosmetic.
    // This pins the documented rule against accidental `speed_bonus > 0 → Buff`.
    let mut eng = empty_engine_status();
    eng.bonuses.speed_bonus = 1;
    assert_eq!(derive_status_tags(&make_status(eng)), StatusTagSet::COSMETIC);
}

#[test]
fn status_dot_dice_yields_dot() {
    let dice = combat_engine::DiceExpr::new(1, 4, 0);
    let def = StatusDef {
        dot_dice: Some(dice),
        ..make_status(empty_engine_status())
    };
    assert_eq!(derive_status_tags(&def), StatusTagSet::DOT);
}

#[test]
fn status_hp_percent_dot_yields_dot() {
    let mut eng = empty_engine_status();
    eng.hp_percent_dot = 5;
    assert_eq!(derive_status_tags(&make_status(eng)), StatusTagSet::DOT);
}

#[test]
fn status_armor_bonus_yields_buff() {
    let mut eng = empty_engine_status();
    eng.bonuses.armor_bonus = 4;
    assert_eq!(derive_status_tags(&make_status(eng)), StatusTagSet::BUFF);
}

#[test]
fn status_buff_class_yields_buff() {
    let def = StatusDef {
        buff_class: Some(crate::content::statuses::BuffClass::ArmorBuff),
        ..make_status(empty_engine_status())
    };
    assert_eq!(derive_status_tags(&def), StatusTagSet::BUFF);
}

#[test]
fn status_unrecognised_field_yields_cosmetic_fallback() {
    // `damage_taken_bonus`, `blocks_mana_abilities`, `ai_controlled` — none of
    // these map to a tag; all-empty → Cosmetic (covers `burning`, `broken_faith`,
    // `pact_control` content-side categories at the rule level).
    let mut eng = empty_engine_status();
    eng.bonuses.damage_taken_bonus = 2;
    eng.blocks_mana_abilities = true;
    let def = StatusDef {
        ai_controlled: true,
        ..make_status(eng)
    };
    assert_eq!(derive_status_tags(&def), StatusTagSet::COSMETIC);
}

#[test]
fn status_combo_negative_speed_and_dot_yields_soft_cc_and_dot() {
    // Exhaustion-style: SoftCC (speed_bonus<0) + DOT (hp_percent_dot>0).
    // Tests that tags accumulate independently (not mutually exclusive).
    let mut eng = empty_engine_status();
    eng.bonuses.speed_bonus = -1;
    eng.hp_percent_dot = 5;
    assert_eq!(
        derive_status_tags(&make_status(eng)),
        StatusTagSet::SOFT_CC | StatusTagSet::DOT
    );
}

// ── derive_ability_tags rule tests: damage effects → OFFENSIVE ────────────

#[test]
fn ability_weapon_attack_yields_offensive() {
    use crate::content::abilities::{EffectDef, TargetType};
    let def = make_ability(EffectDef::WeaponAttack, TargetType::SingleEnemy, vec![]);
    assert_eq!(derive_ability_tags(&def, &empty_lookup(), &HashMap::new()),
               AbilityTagSet::OFFENSIVE);
}

#[test]
fn ability_damage_effect_yields_offensive() {
    use crate::content::abilities::{EffectDef, TargetType};
    let dice = combat_engine::DiceExpr::new(1, 6, 0);
    let def = make_ability(EffectDef::Damage { dice }, TargetType::SingleEnemy, vec![]);
    assert_eq!(derive_ability_tags(&def, &empty_lookup(), &HashMap::new()),
               AbilityTagSet::OFFENSIVE);
}

#[test]
fn ability_spell_damage_yields_offensive() {
    use crate::content::abilities::{EffectDef, TargetType};
    let dice = combat_engine::DiceExpr::new(1, 6, 0);
    let def = make_ability(EffectDef::SpellDamage { dice }, TargetType::Ground, vec![]);
    assert_eq!(derive_ability_tags(&def, &empty_lookup(), &HashMap::new()),
               AbilityTagSet::OFFENSIVE);
}

// ── derive_ability_tags rule tests: Rescue gate ───────────────────────────

#[test]
fn ability_heal_to_ally_yields_rescue() {
    use crate::content::abilities::{EffectDef, TargetType};
    let dice = combat_engine::DiceExpr::new(1, 6, 0);
    let def = make_ability(EffectDef::Heal { dice }, TargetType::SingleAlly, vec![]);
    assert_eq!(derive_ability_tags(&def, &empty_lookup(), &HashMap::new()),
               AbilityTagSet::RESCUE);
}

#[test]
fn ability_heal_to_self_yields_no_rescue() {
    // Rescue requires target_type==SingleAlly. Heal+Myself must not classify.
    use crate::content::abilities::{EffectDef, TargetType};
    let dice = combat_engine::DiceExpr::new(1, 6, 0);
    let def = make_ability(EffectDef::Heal { dice }, TargetType::Myself, vec![]);
    let tags = derive_ability_tags(&def, &empty_lookup(), &HashMap::new());
    assert!(!tags.contains_tag(AbilityTag::Rescue), "Heal+Myself must not yield Rescue");
}

#[test]
fn ability_non_heal_to_ally_yields_no_rescue() {
    // Rescue gate has TWO conjuncts: target_type==SingleAlly AND Heal. Verify
    // SingleAlly with non-Heal effect does NOT yield Rescue (covers `&&` → `||`).
    use crate::content::abilities::{EffectDef, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleAlly, vec![]);
    let tags = derive_ability_tags(&def, &empty_lookup(), &HashMap::new());
    assert!(!tags.contains_tag(AbilityTag::Rescue), "non-Heal+SingleAlly must not yield Rescue");
}

// ── derive_ability_tags rule tests: Summon / Mobility / empty ─────────────

#[test]
fn ability_summon_yields_summon() {
    use crate::content::abilities::{EffectDef, TargetType};
    let def = make_ability(
        EffectDef::Summon { template_id: "x".into(), max_active: None },
        TargetType::Myself, vec![],
    );
    assert_eq!(derive_ability_tags(&def, &empty_lookup(), &HashMap::new()),
               AbilityTagSet::SUMMON);
}

#[test]
fn ability_grant_movement_yields_mobility() {
    use crate::content::abilities::{EffectDef, TargetType};
    let def = make_ability(EffectDef::GrantMovement { distance: 2 }, TargetType::Myself, vec![]);
    assert_eq!(derive_ability_tags(&def, &empty_lookup(), &HashMap::new()),
               AbilityTagSet::MOBILITY);
}

#[test]
fn ability_no_effect_no_statuses_yields_empty() {
    use crate::content::abilities::{EffectDef, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleEnemy, vec![]);
    assert_eq!(derive_ability_tags(&def, &empty_lookup(), &HashMap::new()),
               AbilityTagSet::empty());
}

// ── derive_ability_tags rule tests: Defensive — 3 OR-branches + negative ──

#[test]
fn ability_applies_buff_to_self_via_my_self_yields_defensive() {
    // Branch 1: on=MySelf (regardless of target_type).
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleEnemy,
        vec![status_app("buff", StatusOn::MySelf)]);
    let tags = derive_ability_tags(&def, &lookup_with("buff", StatusTagSet::BUFF), &HashMap::new());
    assert!(tags.contains_tag(AbilityTag::Defensive));
}

#[test]
fn ability_applies_buff_to_ally_via_target_yields_defensive() {
    // Branch 2: on=Target AND target_type=SingleAlly.
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleAlly,
        vec![status_app("buff", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &lookup_with("buff", StatusTagSet::BUFF), &HashMap::new());
    assert!(tags.contains_tag(AbilityTag::Defensive));
}

#[test]
fn ability_applies_buff_via_target_when_target_is_myself_yields_defensive() {
    // Branch 3: on=Target AND target_type=Myself (e.g. taunt's defending status).
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::Myself,
        vec![status_app("buff", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &lookup_with("buff", StatusTagSet::BUFF), &HashMap::new());
    assert!(tags.contains_tag(AbilityTag::Defensive));
}

#[test]
fn ability_applies_buff_to_enemy_target_yields_no_defensive() {
    // Negative: on=Target + target_type=SingleEnemy. None of the 3 branches fire.
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleEnemy,
        vec![status_app("buff", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &lookup_with("buff", StatusTagSet::BUFF), &HashMap::new());
    assert!(!tags.contains_tag(AbilityTag::Defensive));
}

// ── derive_ability_tags rule tests: ApplyCC — branches + negatives ────────

#[test]
fn ability_applies_hardcc_to_single_enemy_yields_apply_cc() {
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleEnemy,
        vec![status_app("stun", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &lookup_with("stun", StatusTagSet::HARD_CC), &HashMap::new());
    assert!(tags.contains_tag(AbilityTag::ApplyCC));
}

#[test]
fn ability_applies_softcc_to_ground_yields_apply_cc() {
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::Ground,
        vec![status_app("slow", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &lookup_with("slow", StatusTagSet::SOFT_CC), &HashMap::new());
    assert!(tags.contains_tag(AbilityTag::ApplyCC));
}

#[test]
fn ability_applies_cc_to_self_via_my_self_yields_no_apply_cc() {
    // ApplyCC requires on=Target. on=MySelf must not qualify (covers `sa.on != Target` early-return).
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleEnemy,
        vec![status_app("stun", StatusOn::MySelf)]);
    let tags = derive_ability_tags(&def, &lookup_with("stun", StatusTagSet::HARD_CC), &HashMap::new());
    assert!(!tags.contains_tag(AbilityTag::ApplyCC));
}

#[test]
fn ability_applies_cc_to_ally_target_yields_no_apply_cc() {
    // ApplyCC requires target_type in {SingleEnemy, Ground}. SingleAlly must not qualify.
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleAlly,
        vec![status_app("stun", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &lookup_with("stun", StatusTagSet::HARD_CC), &HashMap::new());
    assert!(!tags.contains_tag(AbilityTag::ApplyCC));
}

#[test]
fn ability_applies_dot_to_enemy_yields_no_apply_cc() {
    // ApplyCC checks tags HardCC || SoftCC. A DOT status alone does NOT qualify.
    // (Covers backstab/poison_shot — Damage+DOT classifies as OFFENSIVE only.)
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleEnemy,
        vec![status_app("poison", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &lookup_with("poison", StatusTagSet::DOT), &HashMap::new());
    assert!(!tags.contains_tag(AbilityTag::ApplyCC));
}

// ── derive_ability_tags rule tests: Peel — 3 OR-branches + negative ──────

#[test]
fn ability_applies_forces_targeting_to_self_via_my_self_yields_peel() {
    // Branch 1: on=MySelf.
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::Myself,
        vec![status_app("taunt", StatusOn::MySelf)]);
    let tags = derive_ability_tags(&def, &empty_lookup(), &defs_with_forces_targeting("taunt"));
    assert!(tags.contains_tag(AbilityTag::Peel));
}

#[test]
fn ability_applies_forces_targeting_to_ally_via_target_yields_peel() {
    // Branch 2: on=Target AND target_type=SingleAlly.
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleAlly,
        vec![status_app("taunt", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &empty_lookup(), &defs_with_forces_targeting("taunt"));
    assert!(tags.contains_tag(AbilityTag::Peel));
}

#[test]
fn ability_applies_forces_targeting_via_target_when_target_is_myself_yields_peel() {
    // Branch 3: on=Target AND target_type=Myself.
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::Myself,
        vec![status_app("taunt", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &empty_lookup(), &defs_with_forces_targeting("taunt"));
    assert!(tags.contains_tag(AbilityTag::Peel));
}

#[test]
fn ability_applies_forces_targeting_to_enemy_yields_no_peel() {
    // Negative: target_type=SingleEnemy. None of the 3 branches fire.
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let def = make_ability(EffectDef::None, TargetType::SingleEnemy,
        vec![status_app("taunt", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &empty_lookup(), &defs_with_forces_targeting("taunt"));
    assert!(!tags.contains_tag(AbilityTag::Peel));
}

// ── derive_ability_tags rule tests: combos ────────────────────────────────

#[test]
fn ability_damage_plus_cc_status_yields_offensive_and_apply_cc() {
    // Paralyzing-shot-style: Damage effect (OFFENSIVE) + HardCC status (APPLY_CC).
    use crate::content::abilities::{EffectDef, StatusOn, TargetType};
    let dice = combat_engine::DiceExpr::new(1, 6, 0);
    let def = make_ability(EffectDef::Damage { dice }, TargetType::SingleEnemy,
        vec![status_app("stun", StatusOn::Target)]);
    let tags = derive_ability_tags(&def, &lookup_with("stun", StatusTagSet::HARD_CC), &HashMap::new());
    assert_eq!(tags, AbilityTagSet::OFFENSIVE | AbilityTagSet::APPLY_CC);
}

// ── Special/regression tests ──────────────────────────────────────────────

#[test]
fn derive_ability_tags_taunt_has_peel_via_taunted_status() {
    let content = load_content();
    let lookup = build_status_lookup(&content);
    let def = ability("taunt", &content);

    // Verify Peel is present in the full result.
    let tags = derive_ability_tags(&def, &lookup, &content.statuses);
    assert!(tags.contains_tag(AbilityTag::Peel), "taunt must have PEEL");

    // Now test WITHOUT taunted.forces_targeting — Peel must disappear.
    let mut status_defs_modified = content.statuses.clone();
    if let Some(taunted) = status_defs_modified.get_mut(&StatusId::from("taunted")) {
        taunted.forces_targeting = false;
    }
    let tags_no_peel = derive_ability_tags(&def, &lookup, &status_defs_modified);
    assert!(
        !tags_no_peel.contains_tag(AbilityTag::Peel),
        "without forces_targeting, taunt must NOT have PEEL"
    );
    assert!(
        tags_no_peel.contains_tag(AbilityTag::Defensive),
        "Defensive must still be present (via defending status)"
    );
}

#[test]
fn derive_ability_tags_paralyzing_shot_has_offensive_and_apply_cc() {
    let content = load_content();
    let lookup = build_status_lookup(&content);
    let def = ability("paralyzing_shot", &content);
    let tags = derive_ability_tags(&def, &lookup, &content.statuses);
    assert!(tags.contains_tag(AbilityTag::Offensive), "must have OFFENSIVE");
    assert!(tags.contains_tag(AbilityTag::ApplyCC), "must have APPLY_CC");
    // No other tags for paralyzing_shot
    assert_eq!(tags, AbilityTagSet::OFFENSIVE | AbilityTagSet::APPLY_CC);
}

#[test]
fn classify_is_pure_no_io() {
    // Call classify twice on the same inputs — results must be identical.
    let content = load_content();
    let lookup = build_status_lookup(&content);
    let def = ability("taunt", &content);
    let r1 = derive_ability_tags(&def, &lookup, &content.statuses);
    let r2 = derive_ability_tags(&def, &lookup, &content.statuses);
    assert_eq!(r1, r2, "classifier must be deterministic / pure");
}

// ── Step 9.B: Compulsion tests ────────────────────────────────────────────

/// Pin test (9.B): `taunted` has `forces_targeting=true` → tag set must
/// contain `Compulsion` and NOT contain `Cosmetic`.
#[test]
fn derive_status_tags_taunted_has_compulsion() {
    let content = load_content();
    let def = status_def("taunted", &content);
    let tags = derive_status_tags(&def);
    assert!(tags.contains_tag(StatusTag::Compulsion), "taunted must have Compulsion");
    assert!(!tags.contains_tag(StatusTag::Cosmetic), "taunted must NOT have Cosmetic when Compulsion is set");
}

/// Generic test (9.B): any status with `forces_targeting=true` (and no other
/// AI-side fields) receives `Compulsion` as its sole tag; Cosmetic is suppressed.
#[test]
fn derive_status_tags_compulsion_set_for_forces_targeting() {
    let def = StatusDef {
        id: combat_engine::StatusId::from("test_compulsion"),
        name: "test_compulsion".to_string(),
        dot_dice: None,
        ai_controlled: false,
        buff_class: None,
        engine: combat_engine::StatusDef {
            // All other fields at their zero/None values — pure forces_targeting effect.
            forces_targeting: true,
            bonuses: combat_engine::StatusBonuses::default(),
            skips_turn: false,
            blocks_mana_abilities: false,
            hp_percent_dot: 0,
            causes_disadvantage: false,
        },
    };
    let tags = derive_status_tags(&def);
    assert_eq!(tags, StatusTagSet::COMPULSION, "sole forces_targeting → only Compulsion");
    assert!(!tags.contains_tag(StatusTag::Cosmetic), "Cosmetic must be suppressed by Compulsion");
}
