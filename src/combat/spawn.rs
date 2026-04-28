//! Handles `SpawnUnit` messages emitted when an ability with `EffectDef::Summon` resolves.
//!
//! Instantiates a combatant from a unit template at the nearest free hex to the
//! summoner. Honours the ability's `max_active` cap per summoner. The spawned
//! entity has no `StartingHexPos` so it never interferes with `assign_hex_positions`
//! on subsequent `StartRound`s; it gets picked up by `build_turn_order` at the
//! next round and joins the queue with `Initiative(0)` (acts last).

use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::role::infer_profile;
use crate::combat::ai::tags::AbilityTagCache;
use crate::content::content_view::ActiveContent;
use crate::game::bundles::enemy_bundle;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::components::{
    CombatPath, Dead, Energy, Equipment, Faction, Mana, Rage, SummonedBy, Team, UnitToken,
};
use crate::game::hex::{hex_circle, LAYOUT};
use crate::game::messages::SpawnUnit;
use crate::game::resources::HexPositions;
use crate::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};
use bevy::prelude::*;

/// Search radius for a landing hex. Small + simple: look at nearest ring first.
const SUMMON_SEARCH_RADIUS: u32 = 2;

#[allow(clippy::too_many_arguments)]
pub fn apply_spawn_system(
    mut commands: Commands,
    mut events: MessageReader<SpawnUnit>,
    mut log: ResMut<CombatLog>,
    mut positions: ResMut<HexPositions>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    content: Res<ActiveContent>,
    tag_cache: Res<AbilityTagCache>,
    mats: Res<HexMaterials>,
    token_mesh: Res<TokenMesh>,
    grid_offset: Res<HexGridOffset>,
    factions: Query<&Faction>,
    summoned_by: Query<&SummonedBy, Without<Dead>>,
) {
    let msgs: Vec<SpawnUnit> = events.read().cloned().collect();

    for msg in msgs {
        let Some(template) = content.unit_templates.get(&msg.template_id) else {
            log.push(CombatEvent::SummonBlocked {
                summoner: msg.summoner,
                reason: format!("шаблон '{}' не найден", msg.template_id),
            });
            continue;
        };

        // max_active cap: count LIVE summons from this summoner — dead ones
        // keep their `SummonedBy` component (no despawn), so filter by !Dead.
        if let Some(cap) = msg.max_active {
            let active = summoned_by
                .iter()
                .filter(|SummonedBy(parent)| *parent == msg.summoner)
                .count() as u32;
            if active >= cap {
                log.push(CombatEvent::SummonBlocked {
                    summoner: msg.summoner,
                    reason: format!("лимит призванных достигнут ({active}/{cap})"),
                });
                continue;
            }
        }

        // Pick a free hex: walk nearest-neighbour ring outward, skipping the summoner's tile.
        let summoner_pos = match positions.get(&msg.summoner) {
            Some(p) => p,
            None => {
                log.push(CombatEvent::SummonBlocked {
                    summoner: msg.summoner,
                    reason: "у призывателя нет позиции".into(),
                });
                continue;
            }
        };
        let candidate_pos = hex_circle(summoner_pos, SUMMON_SEARCH_RADIUS)
            .into_iter()
            .find(|&h| h != summoner_pos && positions.entity_at(h).is_none());
        let Some(pos) = candidate_pos else {
            log.push(CombatEvent::SummonBlocked {
                summoner: msg.summoner,
                reason: "рядом нет свободной клетки".into(),
            });
            continue;
        };

        // Assemble the combatant.
        let equipment = Equipment {
            main_hand: Some(template.equipment.main_hand.clone()),
            off_hand: template.equipment.off_hand.clone(),
            chest: template.equipment.chest.clone(),
            legs: template.equipment.legs.clone(),
            feet: template.equipment.feet.clone(),
        };
        let effective = content.effective_stats(&template.stats, &equipment);
        let armor = content.equipment_armor(&equipment);
        let race_name = content.races.get(&template.race).map_or("", |r| r.name.as_str());
        let display_name = if race_name.is_empty() {
            template.name.clone()
        } else {
            format!("{} {}", race_name, template.name)
        };
        // Summoner's team determines the spawn's team. Fallback: Enemy (matches most use cases).
        let team = factions.get(msg.summoner).map_or(Team::Enemy, |f| f.0);
        let role = infer_profile(&template.ability_ids, effective.max_hp, armor, &content, &tag_cache);

        let mut entity_commands = commands.spawn((
            Name::new(display_name.clone()),
            enemy_bundle(
                effective,
                armor,
                template.speed,
                template.ability_ids.clone(),
                equipment,
            ),
            role,
            AiMemory::default(),
            SummonedBy(msg.summoner),
        ));
        // enemy_bundle forces Team::Enemy — overwrite with actual team via a second Faction insert.
        entity_commands.insert(Faction(team));
        if template.resources.rage_max > 0 {
            entity_commands.insert(Rage::new(template.resources.rage_max));
        }
        if template.resources.mana_max > 0 {
            entity_commands.insert(Mana::new(template.resources.mana_max));
        }
        if template.resources.energy_max > 0 {
            entity_commands.insert(Energy::new(template.resources.energy_max));
        }
        if let Some(ref p) = template.path {
            entity_commands.insert(CombatPath(p.clone()));
        }
        let new_entity = entity_commands.id();

        positions.insert(new_entity, pos);

        // Spawn the token mesh — assign_hex_positions only runs at round start and
        // would miss us (it queries for StartingHexPos, which we intentionally don't add).
        let pixel = LAYOUT.hex_to_world_pos(pos) + grid_offset.0;
        let token_material = match team {
            Team::Player => mats.token_player.clone(),
            Team::Enemy => mats.token_enemy.clone(),
        };
        commands.spawn((
            UnitToken(new_entity),
            Mesh2d(token_mesh.token.clone()),
            MeshMaterial2d(token_material),
            Transform::from_xyz(pixel.x, pixel.y, 0.15),
        ));
        // Silence unused warning when no ring is spawned (summoned units don't have VictoryTarget).
        let _ = &mut materials;

        log.push(CombatEvent::Summoned {
            summoner: msg.summoner,
            summon_name: display_name,
        });
    }
}
