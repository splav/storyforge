# Critics Layer

*Источник: `src/combat/ai/pipeline/stages/critics/` (R5.A: перенесено из `src/combat/ai/critics/`).*

После step 7-stages pipeline'а планы проходят через `CriticsStage` (`pipeline/stages/critics/mod.rs`), который применяет `Vec<Box<dyn PlanCritic>>` — каждый critic читает структурированные секции `PlanAnnotation` (outcomes, terminal, repair_affinity) и возвращает `Option<CriticHit>`. Hit умножает `ann.score *= multiplier` и пишется в `ann.critics: Vec<CriticHit>` для логов. Композиция в `CriticsStage::first_wave()` — code-driven (не TOML).

Pipeline order (step 11.4): `Viability → ItemScoring → ModeSelection → Finalize → Sanity → Critics → ProtectSelfMask → KillableGate → RepairAffinity → OverlayConsiderations → PlanModifiers → PickBest`.

## Первая волна (6 critics в `src/combat/ai/pipeline/stages/critics/`)

| Critic | Что ловит | Вход |
|---|---|---|
| `OvercommitIntoDanger` | Low-HP актор лезет в опасный путь / провоцирует AoO. Объединил sanity rules `Survival` + `AoOBleed` (max из двух multiplier'ов). | `worst_path_danger` × hp_need / `expected_aoo_damage` ÷ hp |
| `SelfLethalWithoutPayoff` | Урон самому себе (включая AoO + self-AoE) без отдачи. Расширение `SelfAoe`: учитывает любой self-damage, не только AoE. | `outcome.self_damage` сумма vs payoff (kills + ally_rescue) |
| `BlindspotRanged` | RANGED актор закончил ход без LoS на врагов. 1:1 порт `LosBlindspot`. | `has_los(final_pos, enemy.pos)` |
| `BuffIntoVoid` | Buff/status на цель, у которой эффект уже активен (или будет активирован раньше в плане). Identity по `target: Entity` (стабильно при движении цели). | `target_unit.statuses` + intra-plan tracking |
| `RareResourceForLowImpact` | Дорогой damage-каст с низкой отдачей. **Только damage-способности** — status-only пропускаются. | `mana_cost ≥ 30` + `actual_damage / expected_damage < 0.5` |
| `HealWithoutRescueValue` | Heal на здорового неугрожаемого ally. Использует `tuning.curves.rescue_ally` — ту же кривую, что в `appraisal::compute_rescue_ally`. | `(1 − hp_pct) × ally_threat_proxy` через Logistic; HP-need gate как fallback для раненых |

## Residual sanity (`planning/sanity.rs`)

Три правила остались как general-purpose multiplicative penalties — содержательно не маппятся на critic-классы из мастер-плана: `HealerExposure`, `RetreatTrap`, `SynergyBonus`. См. [pipeline.md](pipeline.md#plan-sanity-adjust).

В `sanity.rs` остались как `pub(crate)` helpers, переиспользуемые критиками: `expected_aoo_damage`, `plan_has_self_aoe`, `plan_has_useful_cast`. `apply_protect_self_mask` — hard mask (≠ critic), вынесен в `ProtectSelfMaskStage`.

## Backlog

- `ZoneOverlapWaste` critic — отложен до step 17 (geometry awareness; в текущем content нет zone-абилок).
- `Flag`-only critics (observability без штрафа) — step 11 (band/agenda).
- TOML-конфигурируемые thresholds + multiplier'ы — после mining-калибровки на v31+ corpus.
- ai_scenarios harness extension для critic-fixtures (поле `Expectation.critics`) — отдельный backlog.

## Implementation caveat: Adaptation rescore wipes Sanity / Critics

Текущая реализация `FinalizeStage` (бывший вторая половина `AdaptationStage`) вызывает `rescore_with_per_plan_modes(...)`, которая пересчитывает `ann.score` для **всех** планов pool'а из raw factors — как только триггерится хоть один LastStand-кейс (`ProtectSelfNoDefensive` / `ProtectSelfFutile` / `ExpectedSelfLethal`). Это **стирает все pre-finalize score modifiers** — но **в текущем pipeline-порядке** Sanity и Critics запускаются ПОСЛЕ Finalize, так что их multipliers переживают. Тем не менее старый caveat сохраняется для случаев, когда rescoring запускается без переноса post-finalize modifiers — sanity и critics, делающие свою работу до Finalize, будут стёрты.

Practical implications для текущего step 11.4 порядка:

- Finalize применяет mode-rescore первым; затем Sanity и Critics добавляют свои multipliers поверх — они **не** стираются.
- Mining cross-tab `Overcommit × adaptation reason` (G1 секция в `mine_ai_logs`) показывает корректное поведение: critic effect доходит до финального score.

См. также: backlog в [`rework/index.md`](rework/index.md).
