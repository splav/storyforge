//! Shared test helpers for the `ai/` tree. Lives in the binary only under
//! `cfg(test)` and is consumed by `generator.rs`, `scorer.rs`,
//! `scarcity.rs` (and any future scoring sub-test) so the four-line
//! `UtilityContext` boilerplate exists in one place.

use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::utility::{ActorCtx, AiWorld, UtilityContext};
use crate::content::abilities::CasterContext;
use crate::content::content_view::ContentView;
use crate::content::races::CritFailEffect;
use crate::game::components::Abilities;

/// Build a `UtilityContext` with the conventional test defaults
/// (`crit_fail_effect: Miss`, `crit_fail_chance: 0.0`). Caller supplies the
/// caster so per-suite str/int/spell-power tweaks stay explicit.
pub(crate) fn make_test_ctx<'a>(
    content: &'a ContentView,
    difficulty: &'a DifficultyProfile,
    caster: &'a CasterContext,
    abilities: &'a Abilities,
) -> UtilityContext<'a> {
    UtilityContext {
        world: AiWorld { content, difficulty },
        actor: ActorCtx {
            caster,
            abilities,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
        },
    }
}
