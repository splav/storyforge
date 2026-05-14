//! Bevy `Resource` wrapper for the engine's canonical `DiceRng`.

use bevy::prelude::*;

#[derive(Resource, Default)]
pub struct DiceRngRes(pub combat_engine::DiceRng);

impl std::ops::Deref for DiceRngRes {
    type Target = combat_engine::DiceRng;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for DiceRngRes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl DiceRngRes {
    pub fn with_seed(seed: u64) -> Self {
        Self(combat_engine::DiceRng::with_seed(seed))
    }
}
