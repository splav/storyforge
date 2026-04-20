use crate::content::content_view::ContentView;
use crate::combat::ai::snapshot::UnitSnapshot;
use crate::content::abilities::{AbilityDef, CasterContext, EffectDef, TargetType};
use crate::content::statuses::StatusDef;
use crate::game::components::Abilities;

/// True if the ability applies any status that skips the target's turn
/// (stun, paralyse, sleep…). Single source of truth for "is this CC?".
pub fn applies_cc(def: &AbilityDef, content: &ContentView) -> bool {
    def.statuses
        .iter()
        .any(|sa| content.statuses.get(&sa.status).is_some_and(|sd| sd.skips_turn))
}

/// Yield `(StatusDef, duration_rounds)` pairs for every status application
/// attached to `def`, resolving the id against `content`. Silently skips
/// applications whose id isn't in the content registry.
///
/// Single source of truth for the iterate-def-statuses-and-lookup-StatusDef
/// pattern used by both `status_score` (full HP-equivalent contribution)
/// and `factors::offensive::status_cc_value` (CC-denial subset). Each
/// caller keeps its own value-accumulating closure — semantics differ
/// (`.abs()` on damage_taken_bonus vs. positive-only gate), so only the
/// iteration shape is shared.
pub fn status_applications<'a, 'c: 'a>(
    def: &'a AbilityDef,
    content: &'c ContentView,
) -> impl Iterator<Item = (&'c StatusDef, f32)> + 'a {
    def.statuses.iter().filter_map(move |sa| {
        content
            .statuses
            .get(&sa.status)
            .map(|sd| (sd, sa.duration_rounds as f32))
    })
}

/// Score a single (ability, target) pair in HP-equivalent units.
/// Higher = more desirable. Returns 0.0 for options that should be skipped.
pub fn score_action(
    def: &AbilityDef,
    target: &UnitSnapshot,
    ctx: &CasterContext,
    content: &ContentView,
) -> f32 {
    let Some(calc) = def.effect.calc(ctx) else {
        return if matches!(def.effect, EffectDef::GrantMovement { .. }) {
            0.0
        } else {
            status_score(def, target, content)
        };
    };

    let expected = calc.expected();

    let dmg_score = if calc.is_heal {
        let missing = (target.max_hp - target.hp) as f32;
        if missing <= 0.0 {
            return 0.0;
        }
        let effective = expected.min(missing);
        // Heal value = fraction of ally restored × their damage output.
        // Dimensionally ≈ "enemy HP/turn this heal keeps alive", so it competes
        // on the same scale as `damage`. A fresh ally has small delta_pct so
        // tops-off auto-score low; urgent heals auto-score high via bigger delta.
        let delta_pct = effective / target.max_hp.max(1) as f32;
        delta_pct * target.threat
    } else {
        let mitigation = if calc.pierces_armor {
            0.0
        } else {
            (target.armor + target.armor_bonus) as f32
        };
        // Post-armor damage. No artificial floor: if armor absorbs everything,
        // score is 0. Kill bonus is handled by the separate `kill` factor.
        let raw = (expected - mitigation + target.damage_taken_bonus as f32).max(0.0);
        // Progress multiplier: a hit that meaningfully clips the target's
        // current HP is worth more than the same raw damage into a fresh pool.
        // 0.5 baseline keeps single hits meaningful; bonus rewards finishing.
        let progress = (raw / target.hp.max(1) as f32).min(1.0);
        raw * (0.5 + 0.5 * progress)
    };

    dmg_score + status_score(def, target, content)
}

/// Best single-target expected damage from one ability (before armor).
/// Used to value stuns/kills: controlling a high-damage target is worth more.
/// Does NOT capture AoE, healing, or utility — it's a damage-only estimate.
pub fn estimate_st_damage(ctx: &CasterContext, abilities: &Abilities, content: &ContentView) -> f32 {
    abilities
        .0
        .iter()
        .filter_map(|id| content.abilities.get(id))
        .filter(|def| matches!(def.target_type, TargetType::SingleEnemy))
        .filter_map(|def| def.effect.calc(ctx))
        .map(|calc| calc.expected().max(0.0))
        .fold(0.0f32, f32::max)
}

// ── Internals ──────────────────────────────────────────────────────────────

fn status_score(
    def: &AbilityDef,
    target: &UnitSnapshot,
    content: &ContentView,
) -> f32 {
    // HP-equivalent scoring — counts BOTH signs of damage_taken_bonus /
    // armor_bonus (.abs()) because a buff on an ally and a debuff on an
    // enemy are both "value to the caster". For CC-denial scoring see
    // `factors::offensive::status_cc_value`.
    status_applications(def, content)
        .map(|(sd, d)| {
            let mut total = 0.0f32;
            // Stun: deny target's damage output for d rounds.
            if sd.skips_turn {
                total += target.threat * d;
            }
            // Vulnerability: extra damage taken per hit for d rounds.
            if sd.damage_taken_bonus != 0 {
                total += sd.damage_taken_bonus.abs() as f32 * d;
            }
            // Armor delta: negative = shred on enemy, positive = buff on ally.
            if sd.armor_bonus != 0 {
                total += sd.armor_bonus.abs() as f32 * d;
            }
            // DoT: expected tick damage × duration.
            if let Some(ref dice) = sd.dot_dice {
                total += dice.expected() * d;
            }
            // %HP DoT (e.g. exhaustion).
            if sd.hp_percent_dot > 0 {
                let tick_dmg = (target.max_hp as f32 * sd.hp_percent_dot as f32 / 100.0).ceil();
                total += tick_dmg * d;
            }
            // Silence (blocks mana abilities): partial stun — target can
            // still basic-attack, so worth ~half a skips_turn.
            if sd.blocks_mana_abilities {
                total += target.threat * 0.5 * d;
            }
            // Speed penalty: reduces tactical options.
            if sd.speed_bonus < 0 {
                total += (-sd.speed_bonus) as f32 * d;
            }
            total
        })
        .sum()
}
