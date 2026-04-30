/// Kept for existing unit tests (adaptation-specific tests rely on it directly).
/// New pipeline code uses `mode_selection` + `finalize` instead.
pub mod adaptation;
pub mod critics;
pub mod finalize;
pub mod item_scoring;
pub mod killable_gate;
pub mod mode_selection;
pub mod overlay_considerations;
pub mod pick_best;
pub mod plan_modifiers;
pub mod protect_self;
pub mod repair_affinity;
pub mod sanity;
pub mod viability;
