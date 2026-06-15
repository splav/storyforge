//! Status-injection helpers for integration tests.
//!
//! Each function registers a `StatusDef` in `ActiveContent` so subsequent
//! ability resolutions can apply it.
//!
//! Split from `common/mod.rs` in Phase H3 of `docs/refactor/helpers-normalization-plan.md`.

#![allow(dead_code)]

use bevy::prelude::*;

use storyforge::content::content_view::ActiveContent;
use storyforge::content::statuses::StatusDef;

pub fn insert_stun_status(app: &mut App) {
    app.world_mut()
        .resource_mut::<ActiveContent>()
        .0
        .statuses
        .insert(
            "stun".into(),
            StatusDef {
                id: "stun".into(),
                name: "Stun".into(),
                dot_dice: None,
                ai_controlled: false,
                buff_class: None,
                engine: storyforge::combat_engine::StatusDef {
                    bonuses: storyforge::combat_engine::StatusBonuses::default(),
                    skips_turn: true,
                    forces_targeting: false,
                    blocks_mana_abilities: false,
                    hp_percent_dot: 0,
                    heal_per_tick: 0,
                    causes_disadvantage: false,
                    ..Default::default()
                },
            },
        );
}
