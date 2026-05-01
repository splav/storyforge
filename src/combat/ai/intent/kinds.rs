use std::fmt;

// ── Intent enum ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum TacticalIntent {
    /// Focus fire: kill or heavily damage a specific target.
    FocusTarget {
        #[serde(with = "crate::combat::ai::log::serde_helpers::entity")]
        target: bevy::prelude::Entity,
    },
    /// Apply CC (stun) to a high-threat target.
    ApplyCC {
        #[serde(with = "crate::combat::ai::log::serde_helpers::entity")]
        target: bevy::prelude::Entity,
    },
    /// Reposition to a better tile.
    Reposition,
    /// Self-preservation: avoid danger.
    ProtectSelf,
    /// Protect/heal a specific wounded ally.
    ProtectAlly {
        #[serde(with = "crate::combat::ai::log::serde_helpers::entity")]
        ally: bevy::prelude::Entity,
    },
    /// Position to hit multiple enemies with AoE.
    SetupAOE,
    /// Survival is unlikely — maximize last useful action (kill > cc > damage).
    LastStand,
}

/// Intent kind without target data, for stickiness comparison.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum IntentKind {
    FocusTarget,
    ApplyCC,
    Reposition,
    ProtectSelf,
    ProtectAlly,
    SetupAOE,
    LastStand,
}

impl TacticalIntent {
    pub fn kind(&self) -> IntentKind {
        match self {
            Self::FocusTarget { .. } => IntentKind::FocusTarget,
            Self::ApplyCC { .. } => IntentKind::ApplyCC,
            Self::Reposition => IntentKind::Reposition,
            Self::ProtectSelf => IntentKind::ProtectSelf,
            Self::ProtectAlly { .. } => IntentKind::ProtectAlly,
            Self::SetupAOE => IntentKind::SetupAOE,
            Self::LastStand => IntentKind::LastStand,
        }
    }

    pub fn target(&self) -> Option<bevy::prelude::Entity> {
        match self {
            Self::FocusTarget { target } | Self::ApplyCC { target } => Some(*target),
            Self::ProtectAlly { ally } => Some(*ally),
            _ => None,
        }
    }
}

// ── Intent selection reason ────────────────────────────────────────────────

/// Structured explanation for why a given intent was picked.
///
/// Emitted at the decision site — producer fills the variant's fields directly
/// so the log/overlay never re-parse a freetext string. Each variant maps to a
/// stable `code()` for the JSONL analyzer and a `Display` impl for human text.
///
/// Add a new rule by adding a variant here and emitting it at the rule site.
/// Classification (`selection_kind` in the log) is compiler-checked via
/// `code()` — there is no string-prefix table to keep in sync.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntentReason {
    /// Step 3.2: fields migrated from raw hp_pct/hp_threshold to
    /// need_signals.self_preserve. Schema v20 → v21.
    ///
    /// **Step 11.5**: routing equivalent is `BandReason::PanicOverride` in
    /// `CriticalSelfPreservation` band.  Kept for serialization compat (v31)
    /// and as `AgendaItem.reason` in `build_critical_self_preservation`.
    PanicOverride { self_preserve: f32, self_preserve_threshold: f32, danger: f32, danger_threshold: f32 },
    /// Step 3.2: hp_pct field renamed to self_preserve. Schema v20 → v21.
    Urgency { self_preserve: f32, danger: f32 },
    ProtectAlly { ally_hp_pct: f32, threshold: f32, heal_identity: f32 },
    /// **Step 11.5**: routing equivalent is `BandReason::TauntForced` in
    /// `ForcedTargeting` band.  Kept for serialization compat (v31) and as
    /// `AgendaItem.reason` in `build_forced_targeting`.
    TauntForced,
    /// **Step 11.5**: produced only by the deprecated `select_intent`.  Kept
    /// for serialization compat (v31); no longer emitted in production paths.
    TauntCc { dpr: f32 },
    /// Step 3.5: added `finish_target` field for diagnostics (add-only, no schema bump needed).
    Killable { threat: f32, eff_hp: i32, reach_budget: u32, #[serde(default)] finish_target: f32 },
    BestPriority { priority: f32 },
    ApplyCc { dpr: f32 },
    SetupAoe { clustered_pairs: usize },
    /// Step 3.4: fields migrated from raw pos_eval/threshold to
    /// need_signals.reposition/floor. Schema v21 → v22.
    Reposition { reposition: f32, floor: f32 },
    NoRuleDefault,
    MidpanicFallback {
        hp_pct: f32,
        midpanic_hp: f32,
        danger: f32,
        panic_danger: f32,
        max_align: f32,
        threshold: f32,
    },
    ViabilityFallback {
        from: IntentKind,
        max_align: f32,
        threshold: f32,
    },
    /// ADAPTATION switched the chosen plan's evaluation regime. `prior`
    /// is the reason that originally selected the global intent; `reason`
    /// is the fact that triggered the adaptation (per-plan ExpectedSelfLethal
    /// or global ProtectSelfNoDefensive). Boxed so the enum stays small.
    Adapted {
        prior: Box<IntentReason>,
        reason: crate::combat::ai::adapt::AdaptationReason,
    },
}

impl IntentReason {
    /// Stable snake_case code for analyzers. The JSONL log stores this as
    /// `selection_kind`. Must stay backward-compatible — rename requires
    /// bumping `log::SCHEMA_VERSION`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::PanicOverride { .. } => "panic_override",
            Self::Urgency { .. } => "urgency",
            Self::ProtectAlly { .. } => "protect_ally",
            Self::TauntForced => "taunt_forced",
            Self::TauntCc { .. } => "taunt_cc",
            Self::Killable { .. } => "killable",
            Self::BestPriority { .. } => "best_priority",
            Self::ApplyCc { .. } => "apply_cc",
            Self::SetupAoe { .. } => "setup_aoe",
            Self::Reposition { .. } => "reposition",
            Self::NoRuleDefault => "no_rule_default",
            Self::MidpanicFallback { .. } => "midpanic_fallback",
            Self::ViabilityFallback { .. } => "viability_fallback",
            Self::Adapted { reason, .. } => reason.code(),
        }
    }
}

impl fmt::Display for IntentReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PanicOverride { self_preserve, self_preserve_threshold, danger, danger_threshold } => write!(
                f, "panic: self_preserve={:.2}>={:.2} AND danger={:.2}>{:.2}",
                self_preserve, self_preserve_threshold, danger, danger_threshold,
            ),
            Self::Urgency { self_preserve, danger } => write!(
                f, "self_preserve={:.2} × danger={:.2}", self_preserve, danger,
            ),
            Self::ProtectAlly { ally_hp_pct, threshold, heal_identity } => write!(
                f, "ally hp%={:.0}%<{:.0}% (healer support={:.2})",
                ally_hp_pct * 100.0, threshold * 100.0, heal_identity,
            ),
            Self::TauntForced => write!(f, "forced by taunt (FORCES_TARGETING)"),
            Self::TauntCc { dpr } => write!(f, "CC the taunter (dpr={:.1})", dpr),
            Self::Killable { threat, eff_hp, reach_budget, finish_target } => write!(
                f, "killable: threat={:.1}>=eff_hp={}, reach_budget={}, finish_target={:.2}",
                threat, eff_hp, reach_budget, finish_target,
            ),
            Self::BestPriority { priority } => write!(f, "highest priority={:.2}", priority),
            Self::ApplyCc { dpr } => write!(f, "unstunned enemy dpr={:.1}", dpr),
            Self::SetupAoe { clustered_pairs } => write!(
                f, "{} clustered enemy pair(s) within dist≤2", clustered_pairs,
            ),
            Self::Reposition { reposition, floor } => write!(
                f, "reposition_signal={:.2} > floor={:.2}", reposition, floor,
            ),
            Self::NoRuleDefault => write!(f, "no rule matched — default reposition"),
            Self::MidpanicFallback {
                hp_pct, midpanic_hp, danger, panic_danger, max_align, threshold,
            } => write!(
                f,
                "midpanic_fallback: hp%={:.0}%<{:.0}% AND danger={:.2}>{:.2} (max_align={:.2}<{:.2})",
                hp_pct * 100.0, midpanic_hp * 100.0, danger, panic_danger, max_align, threshold,
            ),
            Self::ViabilityFallback { from, max_align, threshold } => write!(
                f, "fallback from {:?}: max_align={:.2}<threshold={:.2}",
                from, max_align, threshold,
            ),
            Self::Adapted { prior, reason } => {
                use crate::combat::ai::adapt::AdaptationReason;
                match reason {
                    AdaptationReason::ExpectedSelfLethal { aoo_dmg, actor_hp } => write!(
                        f,
                        "{} → LastStand (EV-lethal: aoo={:.1} ≥ hp={})",
                        prior, aoo_dmg, actor_hp,
                    ),
                    AdaptationReason::ProtectSelfNoDefensive => write!(
                        f, "{} → LastStand (no defensive plan)", prior,
                    ),
                    AdaptationReason::ProtectSelfFutile { pending_dot, actor_hp } => write!(
                        f,
                        "{} → LastStand (doomed: pending_dot={} ≥ hp={})",
                        prior, pending_dot, actor_hp,
                    ),
                }
            }
        }
    }
}
