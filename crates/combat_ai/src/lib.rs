//! `combat_ai` — pure-Rust AI слой для storyforge.
//!
//! Дизайн: крейт не зависит от Bevy. Feature `bevy` (off by default)
//! подключает `bevy_ecs` ради derive(Resource)/derive(Component) на
//! AI-типах, которые используются как Bevy resources в главном crate.
//! Бины (mine_ai_logs, replay_ai_log) после миграции будут зависеть
//! только от `combat_ai` без feature `bevy` → собираются за секунды.
//!
//! На текущей фазе крейт содержит только инфраструктуру:
//! `EntityId` и serde-адаптеры. Логика AI переедет в Phase 2-3.

pub mod entity_id;
pub mod serde_helpers;

pub use entity_id::EntityId;
