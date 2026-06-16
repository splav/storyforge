//! `combat_ai` — pure-Rust AI слой для storyforge.
//!
//! Дизайн: крейт не зависит от Bevy. Feature `bevy` (off by default) подключает
//! `bevy_ecs` ради derive(Resource/Component) на AI-типах, используемых как
//! Bevy resources в главном crate. Без feature бины собираются за секунды.
//!
//! Пока крейт содержит только инфраструктуру (`EntityId`, serde-адаптеры);
//! логика AI переедет в Phase 2-3.

pub mod entity_id;
pub mod serde_helpers;

pub use entity_id::EntityId;
