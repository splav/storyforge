# Enemy AI

Обзор архитектуры, точка входа в детальные доки. Каждый «слой» вынесен в отдельный файл; здесь — карта модулей и связи.

## Что это

AI-система выбирает действие для вражеских юнитов (и героев под `pact_control`). Работает в рамках `CombatStep::Command`: `enemy_ai_system` для `Team::Enemy`, `pact_ai_system` для героев с `ai_controlled`-статусом.

Каждый AI-тик строит **свежий `pick_action`** — beam-search строит цепочку шагов, коммитится только первый (или `Move→Cast` бандлом). Reservations координируют параллельно действующих юнитов и резервируют только закоммиченный prefix.

**Plan freeze → goal-preserving repair (step 6).** Раньше `last_plan` хранил полный snapshot и replan-ил через binary-continuation. Сейчас `AiMemory.last_goal` хранит `StoredGoalContext` (kind, region, ttl, confidence) — fresh план всегда строится, а планы, сохраняющие goal, получают `repair_bonus` через `RepairAffinityStage`.

Файлы: `src/combat/ai/` + shared core в `src/combat/effects_*`.

## Карта модулей

| Модуль / файл | Назначение |
|---|---|
| `enemy_turn.rs` | Главная система: snapshot/maps + `pick_action` + сообщения |
| `utility/` | Top-level pipeline `pick_action` + fallback movement |
| `pipeline/` | `PlanStage` trait + `run_pool_pipeline` + 14 stage-ов в `stages/` |
| `planning/` | Plan types, beam search generator, sim, scorer, sanity, picker, terminal |
| `factors/` | Factor scoring: `step/` (per-step факторы), `plan/` (plan-уровень), `terminal/` (8 terminal-осей), общая инфра в `mod.rs` + `registry.rs` |
| `outcome/` | `ActionOutcomeEstimate` (17 fact-полей) + `PlanAnnotation` + builder |
| `scoring/policy/` | HP-equivalent value functions: `damage`, `heal`, `cc`, `status`, `friendly_fire` |
| `modifiers/` | Plan-level signed modifiers: `summon_bonus`, `trade_bonus`, `repair_bonus` |
| `intent/` | `TacticalIntent`, `IntentKind`, `AiMemory`, bands, agenda, considerations |
| `critics/` | 6 критиков первой волны (`OvercommitIntoDanger`, `SelfLethalWithoutPayoff`, …) |
| `repair/` | `StoredGoalContext`, `GoalKind`, lifecycle, `RepairAffinity` |
| `appraisal/` | Need signals (`self_preserve`, `rescue_ally`, `apply_cc`, `setup_aoe`, `continue_commitment`, …) |
| `tags/` | `AbilityTag`, `StatusTag` cache + `classify` (single source of truth) |
| `scoring/` | HP-equivalent scoring umbrella: `horizon.rs` (DPR helpers), `target_priority.rs`, `position_eval.rs`, `trade.rs`, `policy/` |
| `config/tuning.rs` | `AiTuning` resource: `thresholds`, `tables`, `difficulty` curves |
| `world/snapshot.rs` | `BattleSnapshot` + `UnitSnapshot.statuses` + `refresh_status_aggregates` |
| `config/role.rs` | `AxisProfile` (5-мерная роль) + инференс по kit'у |
| `config/difficulty.rs` | `DifficultyProfile` — ручки качества решений |
| `world/influence.rs` | Карты влияния + `InfluenceConfig` resource |
| `world/reservations.rs` | Координация команды (reset на round-start) |
| `log/debug.rs`, `log/mod.rs`, `replay.rs`, `replay_assertion.rs` | Debug overlay + JSONL-лог + replay |

### Shared effects core (вне `ai/`)

`src/combat/effects_math.rs`, `effects_state.rs`, `effects_outcome.rs` — **единый источник истины** для разрешения способности. Real pipeline (`combat/resolution.rs`) и AI sim (`combat/ai/planning/sim.rs`) вызывают один и тот же `compute_ability_outcome`; различаются только backend'ами (RNG vs EV, Bevy components vs snapshot). См. [`ability-resolution.md`](ability-resolution.md).

## Документы по слоям

| Документ | Что внутри |
|---|---|
| [decision-cycle.md](decision-cycle.md) | Цикл `pick_action`, порядок stage-ов, `GrantMovement` mid-turn |
| [ability-resolution.md](ability-resolution.md) | `TargetState`, `DiceSource`, `compute_ability_outcome`, drift sim↔real |
| [snapshot.md](snapshot.md) | `BattleSnapshot`, `UnitSnapshot`, `AiTags`, semantic tags (`AbilityTag` / `StatusTag`) |
| [intent.md](intent.md) | `TacticalIntent`, выбор интента, viability guard, intent-scoring, `ProtectSelf` mask |
| [scoring.md](scoring.md) | Factors (10 осей), outcome vector, terminal-axes, goal-preserving repair, role weights/composition |
| [policy.md](policy.md) | HP-эквивалентные value functions (`damage`, `heal`, `cc`, `status`, `friendly_fire`) |
| [target-priority.md](target-priority.md) | Target priority, position evaluation, influence maps |
| [pipeline.md](pipeline.md) | Plan generation hard constraints, sanity (residual), pick best, commit |
| [adaptation.md](adaptation.md) | `EvaluationMode`, `AdaptationReason`, MVP scope. *Stage расщеплён на ModeSelection+Finalize в step 11.0.* |
| [trade-economy.md](trade-economy.md) | `unit_value`, `trade_delta`, `trade_score`, resource scarcity |
| [critics.md](critics.md) | 6 critics первой волны + residual sanity + caveat про adaptation rescore |
| [bands-agenda.md](bands-agenda.md) | `PriorityBand`, `Agenda`, 6 осей `IntentConsiderations`, аддитивная композиция |
| [difficulty.md](difficulty.md) | `DifficultyProfile`, derived lerp curves, per-unit override |
| [debug.md](debug.md) | Overlay, консольный лог, JSONL |
| [extension-checklist.md](extension-checklist.md) | Куда смотреть при добавлении нового effect/status/intent/ability/factor |
| [replay.md](replay.md) | `replay_ai_log`, schema versions, `--assert`, regression metrics |
| [mining.md](mining.md) | `mine_ai_logs` — агрегированная статистика по корпусу (band coverage, agenda win-rate, continuation outcomes) |
| [rework/](rework/) | Архив step-планов, mining-данных, дизайн-документов рефакторинга AI |

## Версии схем

- `SCHEMA_VERSION = 32` (`log.rs`) — текущая версия JSONL.
- Schema v32 (step 11): `ActorTickEvent.band` / `band_reason` / `agenda`, `PlanAnnotation.agenda_item` / `considerations_per_item`.
- Schema v28+ (step 4.12, clean break): outcome shape — fundamental data; v27 logs дают `LogError::UnsupportedSchema`.
