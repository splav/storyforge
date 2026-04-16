use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
use crate::combat::ai::role::AiRole;
use crate::combat::ai::snapshot::AiTags;
use crate::game::hex::Hex;
use crate::game::resources::{UiDirty, UiDirtyFlags};
use bevy::prelude::*;

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
}

/// Actor state at decision time.
pub struct ActorDebug {
    pub role: AiRole,
    pub pos: [i32; 2],
    pub hp: i32,
    pub max_hp: i32,
    pub threat: f32,
    pub tags: AiTags,
    pub action: bool,
    pub movement: bool,
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
    pub decision: DecisionDebug,
    pub candidate_count: usize,
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
        "═══ AI DEBUG: {} ({:?}) ═══",
        snap.actor_name, a.role,
    );
    println!(
        "  HP: {}/{} | threat: {:.1} | pos: {} | tags: {} | act={} mov={}",
        a.hp, a.max_hp, a.threat, fmt_pos(a.pos), fmt_tags(a.tags), a.action, a.movement,
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
        let factors: String = c
            .raw
            .iter()
            .zip(FACTOR_NAMES.iter())
            .map(|(v, n)| format!("{}={:.2}", n, v))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "  #{} {} → {} @ {}  [{}] = {:.2}",
            i + 1,
            c.ability,
            c.target_name,
            fmt_pos(c.tile),
            factors,
            c.total,
        );
        println!(
            "     tile: {}",
            fmt_influence(&c.tile_influence),
        );
    }

    // Final decision.
    println!("  ── Decision ──");
    println!("  {}", snap.decision.description);
    if let (Some(dest), Some(inf)) = (&snap.decision.dest_tile, &snap.decision.dest_influence) {
        println!("  dest {}: {}", fmt_pos(*dest), fmt_influence(inf));
    }

    println!("════════════════════════════════");
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
