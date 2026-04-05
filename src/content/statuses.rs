use crate::core::StatusId;

pub const STATUS_DEFENDING: StatusId = StatusId(1);

/// Flat armor bonus granted by Shield Block for 1 round.
pub const DEFENDING_ARMOR_BONUS: i32 = 4;

#[derive(Debug, Clone)]
pub struct StatusDef {
    pub id:   StatusId,
    pub name: &'static str,
}

pub fn default_statuses() -> Vec<StatusDef> {
    vec![
        StatusDef { id: STATUS_DEFENDING, name: "Defending" },
    ]
}
