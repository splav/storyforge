use super::console_log::fmt_event;
use super::{LogScrollClip, LogScrollThumb, LogText};
use crate::game::resources::{CombatLog, GameDb};
use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;

const SCROLL_SPEED_LINE: f32 = 20.0;
const SCROLL_SPEED_PX: f32 = 1.0;

/// Tracks previous content height to detect growth and auto-scroll.
#[derive(Component, Default)]
pub struct LogScrollState {
    prev_content_h: f32,
}

/// Read viewport and content heights from ComputedNode, converting to logical pixels.
fn scroll_metrics(node: &ComputedNode) -> (f32, f32) {
    let isf = node.inverse_scale_factor();
    let viewport_h = node.size().y * isf;
    let content_h = node.content_size().y * isf;
    (viewport_h, content_h)
}

/// Fills the log text from CombatLog resource.
pub fn update_log(
    log: Res<CombatLog>,
    names: Query<&Name>,
    db: Res<GameDb>,
    mut text_q: Query<&mut Text, With<LogText>>,
) {
    if !log.is_changed() {
        return;
    }
    let Ok(mut t) = text_q.single_mut() else {
        return;
    };

    t.0 = log
        .0
        .iter()
        .map(|e| format!("{}\n", fmt_event(e, &names, &db)))
        .collect();
}

/// Mouse wheel scrolling — works when cursor hovers over the log area.
pub fn log_scroll_input(
    accumulated: Res<AccumulatedMouseScroll>,
    mut clip_q: Query<
        (&Interaction, &ComputedNode, &mut ScrollPosition),
        With<LogScrollClip>,
    >,
) {
    let Ok((interaction, node, mut scroll)) = clip_q.single_mut() else {
        return;
    };
    if *interaction == Interaction::None {
        return;
    }
    if accumulated.delta == Vec2::ZERO {
        return;
    }

    let delta = match accumulated.unit {
        MouseScrollUnit::Line => accumulated.delta.y * SCROLL_SPEED_LINE,
        MouseScrollUnit::Pixel => accumulated.delta.y * SCROLL_SPEED_PX,
    };

    let (viewport_h, content_h) = scroll_metrics(node);
    let max_scroll = (content_h - viewport_h).max(0.0);
    scroll.0.y = (scroll.0.y - delta).clamp(0.0, max_scroll);
}

/// Updates scrollbar thumb + auto-scrolls when content grows.
pub fn log_scrollbar_update(
    mut clip_q: Query<
        (&ComputedNode, &mut ScrollPosition, &mut LogScrollState),
        With<LogScrollClip>,
    >,
    mut thumb_q: Query<&mut Node, With<LogScrollThumb>>,
) {
    let Ok((node, mut scroll, mut state)) = clip_q.single_mut() else {
        return;
    };
    let Ok(mut thumb_node) = thumb_q.single_mut() else {
        return;
    };

    let (viewport_h, content_h) = scroll_metrics(node);

    // All text visible — thumb fills track, no offset.
    if viewport_h <= 0.0 || content_h <= viewport_h {
        scroll.0.y = 0.0;
        state.prev_content_h = content_h;
        thumb_node.height = Val::Percent(100.0);
        thumb_node.top = Val::Px(0.0);
        return;
    }

    let max_scroll = content_h - viewport_h;

    // Auto-scroll to bottom when content grew.
    if content_h > state.prev_content_h {
        scroll.0.y = max_scroll;
    }
    state.prev_content_h = content_h;

    // Clamp.
    scroll.0.y = scroll.0.y.clamp(0.0, max_scroll);

    // Thumb height: viewport/content ratio, minimum 10% of track.
    let ratio = viewport_h / content_h;
    let thumb_h = ratio.max(0.10) * viewport_h;

    // Thumb position.
    let scroll_frac = scroll.0.y / max_scroll;
    let available_track = viewport_h - thumb_h;
    let thumb_top = scroll_frac * available_track;

    thumb_node.height = Val::Px(thumb_h);
    thumb_node.top = Val::Px(thumb_top);
}
