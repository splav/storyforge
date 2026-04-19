#![allow(clippy::too_many_arguments)]
use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::role::AxisProfile;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::{highest_priority_enemy, target_priority};
use crate::combat::ai::factors::compute_factors;
use crate::combat::ai::utility::{
    ActionCandidate, AiDecision, CandidateKind, PickMechanics, UtilityContext,
};
use crate::game::hex::{hex_to_offset, Hex};
use crate::game::resources::{UiDirty, UiDirtyFlags};
use bevy::prelude::*;
use std::collections::HashMap;

// ── Data types ──────────────────────────────────────────────────────────────

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMapKind {
    #[default]
    Danger,
    AllySupport,
    Opportunity,
    Escape,
}

impl OverlayMapKind {
    fn label(self) -> &'static str {
        match self {
            Self::Danger => "Danger",
            Self::AllySupport => "AllySupport",
            Self::Opportunity => "Opportunity",
            Self::Escape => "Escape",
        }
    }

    fn select(self, maps: &InfluenceMaps) -> &InfluenceMap {
        match self {
            Self::Danger => &maps.danger,
            Self::AllySupport => &maps.ally_support,
            Self::Opportunity => &maps.opportunity,
            Self::Escape => &maps.escape,
        }
    }
}

/// Influence map values at a specific hex.
pub struct TileInfluence {
    pub danger: f32,
    pub ally_support: f32,
    pub opportunity: f32,
    pub escape: f32,
    pub position_eval: f32,
}

pub struct CandidateDebug {
    pub ability: String,
    pub target_name: String,
    pub tile: [i32; 2],
    pub tile_influence: TileInfluence,
    pub raw: [f32; 9],
    pub total: f32,
    pub is_move_only: bool,
}

/// Actor state at decision time.
pub struct ActorDebug {
    /// Pre-formatted role label, e.g. "Mage(0.73) + Support(0.18)".
    pub role_label: String,
    pub pos: [i32; 2],
    pub hp: i32,
    pub max_hp: i32,
    pub threat: f32,
    pub tags: AiTags,
    pub action_points: i32,
    pub max_ap: i32,
    pub movement_points: i32,
}

/// Why this intent was chosen (which rule fired).
pub struct IntentReasoning {
    pub intent: String,
    pub rule: String,
}

/// The final decision.
pub struct DecisionDebug {
    pub description: String,
    pub dest_tile: Option<[i32; 2]>,
    pub dest_influence: Option<TileInfluence>,
}

pub struct AiDebugSnapshot {
    pub actor_name: String,
    pub actor: ActorDebug,
    pub intent: IntentReasoning,
    pub priority_target: Option<(String, f32)>,
    pub top_candidates: Vec<CandidateDebug>,
    pub pick: Option<PickDebug>,
    pub decision: DecisionDebug,
    pub candidate_count: usize,
}

/// One candidate that survived the similarity window and entered the sampling pool.
pub struct PoolEntry {
    pub label: String,   // "ability → target_name"
    pub score: f32,
}

/// Diagnostic for the final pick phase: top-K + mercy + similarity window.
pub struct PickDebug {
    pub top_k: usize,           // requested K from difficulty
    pub window: f32,            // similarity window (noise_amp × 2)
    pub mercy_margin: f32,      // mercy_margin at call time
    pub mercy_applied: bool,    // did mercy rerank the window?
    pub pool: Vec<PoolEntry>,   // candidates eligible for random pick
    pub chosen_pos: usize,      // 0-based position in pool that won
}

/// Resource: AI debug state.
///
/// `ai_debug` (from settings) controls data collection + console logging.
/// `show_overlay` (tilde key) controls influence map rendering on the grid.
#[derive(Resource, Default)]
pub struct AiDebugState {
    /// Master switch from settings — enables data collection and console log.
    pub ai_debug: bool,
    /// Tilde toggle — show/hide influence map overlay on the grid.
    pub show_overlay: bool,
    pub overlay_map: OverlayMapKind,
    /// Last influence maps from any AI turn (always stored when ai_debug=true).
    pub influence_maps: Option<InfluenceMaps>,
    /// Last decision snapshot (consumed by print system).
    pub snapshot: Option<AiDebugSnapshot>,
}

// ── Toggle system ───────────────────────────────────────────────────────────

pub fn toggle_debug_system(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<AiDebugState>,
    mut dirty: ResMut<UiDirty>,
) {
    if !state.ai_debug {
        return;
    }

    if keys.just_pressed(KeyCode::Backquote) {
        state.show_overlay = !state.show_overlay;
        if state.show_overlay {
            println!("[AI DEBUG] overlay ON — {}", state.overlay_map.label());
        } else {
            println!("[AI DEBUG] overlay OFF");
            dirty.0 |= UiDirtyFlags::HEX_FILL;
        }
    }

    if !state.show_overlay {
        return;
    }

    let prev = state.overlay_map;
    if keys.just_pressed(KeyCode::Digit1) {
        state.overlay_map = OverlayMapKind::Danger;
    } else if keys.just_pressed(KeyCode::Digit2) {
        state.overlay_map = OverlayMapKind::AllySupport;
    } else if keys.just_pressed(KeyCode::Digit3) {
        state.overlay_map = OverlayMapKind::Opportunity;
    } else if keys.just_pressed(KeyCode::Digit4) {
        state.overlay_map = OverlayMapKind::Escape;
    }
    if state.overlay_map != prev {
        println!("[AI DEBUG] overlay: {}", state.overlay_map.label());
    }
}

// ── Console print system ────────────────────────────────────────────────────

const FACTOR_NAMES: [&str; 9] = ["dmg", "kill", "cc", "heal", "pos", "risk", "foc", "int", "sca"];

fn fmt_pos(p: [i32; 2]) -> String {
    format!("({},{})", p[0], p[1])
}

fn fmt_tags(tags: AiTags) -> String {
    let mut v = Vec::new();
    if tags.contains(AiTags::LOW_HP) { v.push("LOW_HP"); }
    if tags.contains(AiTags::CAN_HEAL) { v.push("CAN_HEAL"); }
    if tags.contains(AiTags::CAN_CC) { v.push("CAN_CC"); }
    if tags.contains(AiTags::HAS_AOE) { v.push("HAS_AOE"); }
    if tags.contains(AiTags::IS_STUNNED) { v.push("STUNNED"); }
    if tags.contains(AiTags::FORCES_TARGETING) { v.push("TAUNT"); }
    if tags.contains(AiTags::RANGED) { v.push("RANGED"); }
    if tags.contains(AiTags::MELEE_ONLY) { v.push("MELEE"); }
    if v.is_empty() { "none".into() } else { v.join("|") }
}

fn fmt_influence(inf: &TileInfluence) -> String {
    format!(
        "dgr={:.1} ally={:.1} opp={:.1} esc={:.1} eval={:.2}",
        inf.danger, inf.ally_support, inf.opportunity, inf.escape, inf.position_eval,
    )
}

pub fn print_ai_debug_system(mut state: ResMut<AiDebugState>) {
    if !state.ai_debug {
        return;
    }
    let Some(snap) = state.snapshot.take() else {
        return;
    };

    let a = &snap.actor;
    println!(
        "═══ AI DEBUG: {} [{}] ═══",
        snap.actor_name, a.role_label,
    );
    println!(
        "  HP: {}/{} | threat: {:.1} | pos: {} | tags: {} | AP={}/{} mov={}",
        a.hp, a.max_hp, a.threat, fmt_pos(a.pos), fmt_tags(a.tags),
        a.action_points, a.max_ap, a.movement_points,
    );

    // Intent reasoning.
    println!(
        "  Intent: {} [{}]",
        snap.intent.intent, snap.intent.rule,
    );

    if let Some((name, score)) = &snap.priority_target {
        println!("  Priority target: {} ({:.2})", name, score);
    }

    // Candidates.
    println!(
        "  ── Candidates ({} total, top {}) ──",
        snap.candidate_count,
        snap.top_candidates.len(),
    );
    for (i, c) in snap.top_candidates.iter().enumerate() {
        // Skip zero factors — only meaningful numbers survive, keeps the line scannable.
        let factors: String = c
            .raw
            .iter()
            .zip(FACTOR_NAMES.iter())
            .filter(|(v, _)| v.abs() > 0.001)
            .map(|(v, n)| format!("{}={:.2}", n, v))
            .collect::<Vec<_>>()
            .join(" ");
        let header = if c.is_move_only {
            format!("retreat → {}", fmt_pos(c.tile))
        } else {
            format!("{} {} at {}", c.ability, c.target_name, fmt_pos(c.tile))
        };
        println!(
            "  #{} {}  [{}] = {:.2}",
            i + 1,
            header,
            factors,
            c.total,
        );
        println!(
            "     tile: {}",
            fmt_influence(&c.tile_influence),
        );
    }

    // Pick phase: top-K + mercy + similarity window.
    if let Some(pick) = &snap.pick {
        let mercy_note = if pick.mercy_applied { " +mercy" } else { "" };
        println!(
            "  ── Pick (top_k={}, window={:.2}, mercy={:.2}{}) ──",
            pick.top_k, pick.window, pick.mercy_margin, mercy_note,
        );
        if pick.pool.is_empty() {
            println!("    pool empty");
        } else {
            for (i, entry) in pick.pool.iter().enumerate() {
                let mark = if i == pick.chosen_pos { " ← chosen" } else { "" };
                println!("    {} = {:.2}{}", entry.label, entry.score, mark);
            }
        }
    }

    // Final decision.
    println!("  ── Decision ──");
    println!("  {}", snap.decision.description);
    if let (Some(dest), Some(inf)) = (&snap.decision.dest_tile, &snap.decision.dest_influence) {
        println!("  dest {}: {}", fmt_pos(*dest), fmt_influence(inf));
    }

    // Influence map scale stats.
    if let Some(maps) = &state.influence_maps {
        println!(
            "  Maps: danger=[{}] ally=[{}] opp=[{}] esc=[{}]",
            map_stats(&maps.danger),
            map_stats(&maps.ally_support),
            map_stats(&maps.opportunity),
            map_stats(&maps.escape),
        );
    }

    println!("════════════════════════════════");
}

fn map_stats(map: &InfluenceMap) -> String {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut sum = 0.0f32;
    let mut count = 0u32;
    for (_, &v) in map.iter() {
        if v < min { min = v; }
        if v > max { max = v; }
        sum += v;
        count += 1;
    }
    if count == 0 {
        return "empty".into();
    }
    let mean = sum / count as f32;
    format!("{:.1}..{:.1} \u{03bc}={:.1}", min, max, mean)
}

// ── Grid overlay system ─────────────────────────────────────────────────────

const NUM_BUCKETS: usize = 32;

#[derive(Default)]
pub struct OverlayMaterials {
    handles: Vec<Handle<ColorMaterial>>,
}

pub fn debug_overlay_system(
    state: Res<AiDebugState>,
    mut cells: Query<(&Hex, &mut MeshMaterial2d<ColorMaterial>)>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut cache: Local<OverlayMaterials>,
) {
    if !state.show_overlay {
        return;
    }
    let Some(maps) = &state.influence_maps else {
        return;
    };

    // Lazily create gradient materials.
    if cache.handles.is_empty() {
        for i in 0..NUM_BUCKETS {
            let t = i as f32 / (NUM_BUCKETS - 1) as f32;
            let color = gradient_color(t);
            cache.handles.push(materials.add(color));
        }
    }

    let map = state.overlay_map.select(maps);
    let (min, max) = map.min_max();
    let range = max - min;

    for (hex, mut mat) in &mut cells {
        let val = map.get(*hex);
        let t = if range > 0.0 {
            ((val - min) / range).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let bucket = ((t * (NUM_BUCKETS - 1) as f32) as usize).min(NUM_BUCKETS - 1);
        mat.0 = cache.handles[bucket].clone();
    }
}

/// Blue (0.0) → Green (0.5) → Red (1.0), with reduced alpha for overlay feel.
fn gradient_color(t: f32) -> ColorMaterial {
    let (r, g, b) = if t < 0.5 {
        let s = t * 2.0;
        (0.0, s, 1.0 - s)
    } else {
        let s = (t - 0.5) * 2.0;
        (s, 1.0 - s, 0.0)
    };
    ColorMaterial::from(Color::srgba(r * 0.7, g * 0.7, b * 0.7, 0.85))
}

// ── Snapshot builders ───────────────────────────────────────────────────────
//
// Rules:
// 1. Never re-derive the "why" of a decision here. The `reason` strings must
//    come from the function that made the decision (see `intent::select_intent`
//    and the intent-fallback block in `utility::pick_action`). Builders only
//    format the data that was captured at decision time.
// 2. Re-computing deterministic outputs (like `compute_factors` per top-K
//    candidate) is fine — same inputs, same outputs, no drift risk.

fn format_intent(intent: &TacticalIntent, names: &HashMap<Entity, String>) -> String {
    match intent {
        TacticalIntent::FocusTarget { target } => {
            format!("FocusTarget → {}", names.get(target).map_or("?", |n| n))
        }
        TacticalIntent::ApplyCC { target } => {
            format!("ApplyCC → {}", names.get(target).map_or("?", |n| n))
        }
        TacticalIntent::ProtectAlly { ally } => {
            format!("ProtectAlly → {}", names.get(ally).map_or("?", |n| n))
        }
        TacticalIntent::Reposition => "Reposition".into(),
        TacticalIntent::ProtectSelf => "ProtectSelf".into(),
        TacticalIntent::SetupAOE => "SetupAOE".into(),
        TacticalIntent::LastStand => "LastStand".into(),
    }
}

fn tile_influence_at(hex: Hex, role: &AxisProfile, maps: &InfluenceMaps) -> TileInfluence {
    TileInfluence {
        danger: maps.danger.get(hex),
        ally_support: maps.ally_support.get(hex),
        opportunity: maps.opportunity.get(hex),
        escape: maps.escape.get(hex),
        position_eval: evaluate_position(hex, role, maps),
    }
}

fn name_of(entity: Entity, names: &HashMap<Entity, String>) -> String {
    names.get(&entity).cloned().unwrap_or_else(|| format!("{:?}", entity))
}

fn target_label(target: Option<Entity>, names: &HashMap<Entity, String>) -> String {
    target.map_or_else(|| "(area)".into(), |e| name_of(e, names))
}

fn fmt_offset(hex: Hex) -> String {
    let [q, r] = hex_to_offset(hex);
    format!("({},{})", q, r)
}

fn actor_debug(active: &UnitSnapshot) -> ActorDebug {
    ActorDebug {
        role_label: active.role.dominant_label(),
        pos: hex_to_offset(active.pos),
        hp: active.hp,
        max_hp: active.max_hp,
        threat: active.threat,
        tags: active.tags,
        action_points: active.action_points,
        max_ap: active.max_ap,
        movement_points: active.movement_points,
    }
}

fn priority_target_debug(
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    names: &HashMap<Entity, String>,
) -> Option<(String, f32)> {
    highest_priority_enemy(active, snap)
        .map(|t| (name_of(t.entity, names), target_priority(active, t, snap)))
}

/// Build the AiDebugSnapshot for a normal (non-fallback) pick_action path.
pub fn build_debug_snapshot(
    active: &UnitSnapshot,
    actor_pos: Hex,
    intent: &TacticalIntent,
    intent_reason: &str,
    candidates: &[ActionCandidate],
    scores: &[f32],
    decision: &AiDecision,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    names: &HashMap<Entity, String>,
    pick_mech: Option<&PickMechanics>,
) -> AiDebugSnapshot {
    // Top 5 candidates by score — skip -inf masked entries so the log shows
    // only candidates actually in play (ProtectSelf masks non-defensive to -inf).
    let mut indexed: Vec<(usize, f32)> = scores
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, s)| s.is_finite())
        .collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    indexed.truncate(5);

    let top_candidates: Vec<CandidateDebug> = indexed
        .iter()
        .map(|&(i, total)| {
            let c = &candidates[i];
            // compute_factors is deterministic — re-running for display gives
            // exactly what score_candidates saw, so no drift risk here.
            let raw = compute_factors(c, active, intent, ctx, snap, maps, reservations);
            let (ability_label, target_name, is_move_only) = match &c.kind {
                CandidateKind::Cast { ability, target, .. } => {
                    (ability.0.clone(), target_label(*target, names), false)
                }
                CandidateKind::MoveOnly => (String::new(), String::new(), true),
            };
            CandidateDebug {
                ability: ability_label,
                target_name,
                tile: hex_to_offset(c.tile),
                tile_influence: tile_influence_at(c.tile, &active.role, maps),
                raw,
                total,
                is_move_only,
            }
        })
        .collect();

    let pick = pick_mech.map(|pm| PickDebug {
        top_k: pm.top_k,
        window: pm.window,
        mercy_margin: pm.mercy_margin,
        mercy_applied: pm.mercy_applied,
        pool: pm
            .pool
            .iter()
            .map(|&(idx, score)| {
                let c = &candidates[idx];
                let label = match &c.kind {
                    CandidateKind::Cast { ability, target, .. } => {
                        format!("{} → {}", ability, target_label(*target, names))
                    }
                    CandidateKind::MoveOnly => {
                        format!("retreat to {}", fmt_offset(c.tile))
                    }
                };
                PoolEntry { label, score }
            })
            .collect(),
        chosen_pos: pm.chosen_pos,
    });

    AiDebugSnapshot {
        actor_name: name_of(active.entity, names),
        actor: actor_debug(active),
        intent: IntentReasoning {
            intent: format_intent(intent, names),
            rule: intent_reason.to_string(),
        },
        priority_target: priority_target_debug(active, snap, names),
        top_candidates,
        pick,
        decision: decision_debug(decision, actor_pos, None, active, maps, names),
        candidate_count: candidates.len(),
    }
}

/// Build the AiDebugSnapshot for a fallback path (no candidates or all filtered).
pub fn build_fallback_debug(
    active: &UnitSnapshot,
    actor_pos: Hex,
    intent: &TacticalIntent,
    intent_reason: &str,
    decision: &AiDecision,
    reason: &str,
    _ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    names: &HashMap<Entity, String>,
) -> AiDebugSnapshot {
    AiDebugSnapshot {
        actor_name: name_of(active.entity, names),
        actor: actor_debug(active),
        intent: IntentReasoning {
            intent: format_intent(intent, names),
            rule: intent_reason.to_string(),
        },
        priority_target: priority_target_debug(active, snap, names),
        top_candidates: vec![],
        pick: None,
        decision: decision_debug(decision, actor_pos, Some(reason), active, maps, names),
        candidate_count: 0,
    }
}

fn decision_debug(
    decision: &AiDecision,
    actor_pos: Hex,
    fallback_reason: Option<&str>,
    active: &UnitSnapshot,
    maps: &InfluenceMaps,
    names: &HashMap<Entity, String>,
) -> DecisionDebug {
    match decision {
        AiDecision::CastInPlace { ability, target, .. } => DecisionDebug {
            description: format!(
                "CastInPlace: {} → {} (stay at {})",
                ability,
                name_of(*target, names),
                fmt_offset(actor_pos),
            ),
            dest_tile: None,
            dest_influence: None,
        },
        AiDecision::MoveAndCast { path, ability, target, .. } => {
            let dest = path.last().copied().unwrap_or(actor_pos);
            DecisionDebug {
                description: format!(
                    "MoveAndCast: {} → {} → {} ({} steps)",
                    fmt_offset(actor_pos),
                    fmt_offset(dest),
                    format_args!("{} → {}", ability, name_of(*target, names)),
                    path.len(),
                ),
                dest_tile: Some(hex_to_offset(dest)),
                dest_influence: Some(tile_influence_at(dest, &active.role, maps)),
            }
        }
        AiDecision::MoveCloser { path } | AiDecision::MoveOnlyRetreat { path } => {
            let label = if matches!(decision, AiDecision::MoveOnlyRetreat { .. }) {
                "MoveOnlyRetreat"
            } else {
                "MoveCloser"
            };
            let dest = path.last().copied().unwrap_or(actor_pos);
            let prefix = match fallback_reason {
                Some(r) => format!("{} (fallback: {})", label, r),
                None => label.to_string(),
            };
            DecisionDebug {
                description: format!(
                    "{}: {}→{} ({} steps)",
                    prefix,
                    fmt_offset(actor_pos),
                    fmt_offset(dest),
                    path.len(),
                ),
                dest_tile: Some(hex_to_offset(dest)),
                dest_influence: Some(tile_influence_at(dest, &active.role, maps)),
            }
        }
        AiDecision::EndTurn => DecisionDebug {
            description: match fallback_reason {
                Some(r) => format!("EndTurn (fallback: {})", r),
                None => "EndTurn (no action/movement)".into(),
            },
            dest_tile: None,
            dest_influence: None,
        },
    }
}
