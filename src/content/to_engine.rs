//! Bevy content → `combat_engine` type adapters.
//!
//! This module is the canonical place for translating Bevy/content-layer types
//! (`CritFailEffect`, `AbilityDef`) into their pure-engine equivalents.
//!
//! **No Bevy dependencies here** — only `crate::content::*` and `combat_engine`.

use crate::content::abilities::AbilityDef;
use crate::content::races::CritFailEffect;
use combat_engine::EffectDef as EngineEffectDef;

/// Translate a Bevy `CritFailEffect` into the engine's `CritFailOutcome`.
///
/// `CircuitBreach` uses a fixed `SelfDamage(0d1+2)` placeholder (Phase 2 step 6f).
/// Full mana_cost-derived damage parity is a Phase 2 step 7 follow-up.
pub fn crit_fail_outcome(e: &CritFailEffect) -> combat_engine::CritFailOutcome {
    use combat_engine::CritFailOutcome as Out;
    use combat_engine::{DiceExpr, StatusId};
    use CritFailEffect::*;
    match e {
        Miss => Out::Miss,
        ManaOverload => Out::DoubleCost,
        BrokenFaith => Out::ApplyStatus(StatusId::from("broken_faith")),
        CircuitBreach => Out::SelfDamage(DiceExpr::new(0, 1, 2)), // placeholder; step 7 refines
        Exhaustion => Out::ApplyStatus(StatusId::from("exhaustion")),
        PactControl => Out::ApplyStatus(StatusId::from("pact_control")),
    }
}

/// Translate a Bevy `AbilityDef` into a `combat_engine::AbilityDef`.
///
/// Clone of `def.engine` with one transform: `Summon` → `None`, because the AI
/// plan-sim can't model summons (`sim.rs::unit_template` returns `None`) and
/// shouldn't score spawn outcomes. The ECS/bridge path keeps the real `Summon`.
pub fn ability_def(def: &AbilityDef) -> combat_engine::AbilityDef {
    let mut engine = def.engine.clone();
    if let EngineEffectDef::Summon { .. } = engine.effect {
        engine.effect = EngineEffectDef::None;
    }
    engine
}
