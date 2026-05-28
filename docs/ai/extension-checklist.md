# Extension Checklist

Куда смотреть при добавлении разных типов механик. Списки — стартовая точка, а не полная диагностика: Rust exhaustive-match ошибки при компиляции доведут до остальных принудительных точек.

**Общий принцип после Phase 5/6**: правишь движок (`crates/combat_engine/`) — он канонический → правишь content (TOML парсер) → правишь мост (`combat/engine_bridge.rs` — translator/projection, если новое поведение должно видеть UI) → правишь AI (scoring → intent → outcome estimate). Sim теперь = вызов `engine_step()`; отдельной реализации `apply_primary` больше нет.

Engine — это `crates/combat_engine/` (Bevy-free), bridge — `src/combat/engine_bridge.rs`. ECS-проекция (`Vital`, `ActionPoints`, `Reactions`, и т.п.) пишется ТОЛЬКО через `project_state_to_ecs`; см. `tests/projection_isolation.rs` (Phase 6 D6 guard).

## Новая способность (только TOML)

Файлы: `assets/data/abilities.toml` + соответствующие `classes.toml` / `unit_templates.toml` / `encounters.toml` для владельцев.

Код не трогаешь, **если** способность укладывается в существующие `TargetType`, `EffectDef`, `AoEShape`, `StatusOn`, `ResourceKind`. Если нет — см. соответствующий раздел ниже.

## Новый `TargetType`

Конкретный пример — `Ground` (см. git log вокруг fireball). Затрагивает:

| Файл | Что |
|---|---|
| `crates/combat_engine/src/content.rs` | Вариант enum `TargetType` |
| `crates/combat_engine/src/legality.rs` | match arm в `check_legality` (team/alive семантика) |
| `crates/combat_engine/src/targeting.rs` | `primary_target` / AoE enumerator arm |
| `src/content/abilities.rs` | Парсер строки из TOML |
| `src/combat/ai/plan/generator.rs::rank_targets` | Как перебирать кандидатов (сущности / клетки) |
| `src/combat/ai/scoring/horizon.rs` | Фильтры `estimate_st_damage`, `estimate_damage_horizon` — если offensive |
| `src/combat/ai/world/snapshot.rs` | `max_attack_range` фильтр — если это "атака" |
| `src/combat/ai/intent/score.rs` | LastStand +0.5 offensive, прочие intent score'ы если релевантно |
| `src/ui/ability_panel.rs::build_description` | Русская подпись «цель: …» |
| `src/ui/hex_grid/input.rs` | Логика клика (что происходит при выборе клетки / сущности) |
| `src/combat/command_input.rs` | Tab-цикл (что перебирать), Enter-конфирм |
| `src/combat/legality_adapter.rs` | UI legality tooltip bridge — обычно только проверь, что новая ветка не падает |
| `docs/content-guide.md` | Строка в списке допустимых target_type |

Тесты: позитивный + негативный кейсы в `crates/combat_engine/tests/legality.rs` + генератор-тест в `src/combat/ai/plan/generator::tests`.

## Новый `EffectDef`

Engine-resident после Phase 2. Sim теперь = вызов `engine_step()`, отдельной реализации `apply_primary` больше нет.

| Файл | Что |
|---|---|
| `crates/combat_engine/src/effect.rs` | Вариант `Effect` + arm в `apply_effect` (фактическая мутация state) |
| `crates/combat_engine/src/event.rs` | Вариант `Event::*` + `effect_to_event`, если эффект наблюдаемый |
| `crates/combat_engine/src/content.rs` | Вариант `EffectDef` + `EffectDef::to_effect` / `EffectDef::calc` |
| `src/content/abilities.rs` | TOML парсер `EffectDef` варианта |
| `src/combat/engine_bridge.rs::translate_cast_events` (или соседний `translate_*_events`) | Перевод нового `Event` в `CombatEvent` для UI / animation — только если поведение наблюдаемо |
| `src/combat/effects_outcome.rs::OutcomePrimary` + `compute_ability_outcome` | Если AI должен скорить новый эффект — добавить ось в outcome |
| `src/combat/ai/outcome/builder.rs::from_sim_step` | Заполнение `ActionOutcomeEstimate` (damage / heal / cc / etc.) для нового эффекта |
| `src/combat/ai/scoring/policy/` | HP-эквивалент formulas (`damage::value`, `heal::value`, `cc::value`, `friendly_fire::penalty`) — если новый эффект нужно скорить, добавь named pure function |
| `src/combat/ai/config/role.rs::ability_vote` | Голос за ось |

Тесты: engine arm — `crates/combat_engine/tests/effect.rs`; replay — добавить scenario в `crates/combat_engine/tests/replay.rs` если детерминизм нетривиален.

**~3–5 файлов** в типичном случае: engine (effect+event+content) + content parser + outcome estimate. Bridge translator и AI scoring добавляются только если поведение наблюдаемое / скорабельное.

## Новое поле `StatusDef`

Статусы и их tick/apply теперь живут в движке.

| Файл | Что |
|---|---|
| `crates/combat_engine/src/content.rs` | Поле в `StatusDef` + serde |
| `crates/combat_engine/src/effect.rs` (`tick_actor_statuses`, status-on hooks) | Применение эффекта в реальной резолюции (tick / damage_modifier / etc.) |
| `src/content/statuses.rs` | TOML парсер поля |
| `src/combat/ai/world/snapshot.rs::status_bonuses` | Агрегация в `UnitSnapshot` если это численный бонус |
| `src/combat/ai/world/snapshot.rs::compute_tags` | Выставление `AiTag` если флаг — сигнал для интента |
| `src/combat/ai/outcome/builder.rs::from_sim_step` | Агрегация в `cc_turns_applied` / `vulnerability_applied` / `armor_shred_applied` в outcome; `policy::status::*` — для value judgment |
| `docs/content-guide.md` | Комментарий в примере `[[statuses]]` |

## Новый `AiTag`

| Файл | Что |
|---|---|
| `src/combat/ai/world/tags/` (`bitflags!` def) | `AiTags` bitflag |
| `src/combat/ai/world/snapshot.rs::compute_tags` | Условие выставления |
| `src/combat/ai/intent/select.rs` | Используется в лестнице выбора интента |
| Прочие consumer'ы тега (например, фактор scarcity читает `AiTags::IS_STUNNED`) |

## Новый `TacticalIntent`

| Файл | Что |
|---|---|
| `src/combat/ai/intent/kinds.rs` | Вариант enum |
| `src/combat/ai/intent/select.rs` | Скоринг условия выбора (таблица в [intent.md](intent.md#выбор-интента-scored--max-wins)) |
| `src/combat/ai/intent/score.rs` | Alignment scoring на `ScoredStep` |
| `src/combat/ai/intent/select.rs` viability thresholds | Порог в viability guard |
| `src/combat/ai/memory/` | Stickiness continuation — `kind()` + сравнение last_intent (если применимо) |

## Новая `AoEShape`

| Файл | Что |
|---|---|
| `crates/combat_engine/src/content.rs` | Вариант enum `AoEShape` |
| `crates/combat_engine/src/targeting.rs::aoe_cells` | Перечисление клеток (engine-side, used by step()) |
| `src/content/abilities.rs` | TOML парсер |
| `src/combat/effects_math.rs::aoe_cells` | Bridge-side AoE enumerator для UI hover-preview (delegates to engine) |
| `src/ui/hex_grid/visuals.rs::update_hex_visuals` | Preview-рендер под ховером |
| `src/combat/ai/scoring/factors/aoe_hits.rs` | Покрытие enemies/allies (если формула нестандартная) |
| `src/ui/ability_panel.rs::build_description` | Строка-подпись формы |

## Новый фактор scoring'а

| Файл | Что |
|---|---|
| `src/combat/ai/scoring/factors/step/` или `scoring/factors/plan/` | Реализация фактора (per-step или plan-уровень) + регистрация в `scoring/factors/registry.rs` |
| `src/combat/ai/scoring/factors/mod.rs` | `pub use` новой функции если нужна из-за пределов `scoring/factors/` |
| `assets/data/ai_tuning.toml` (`tables.axis_factor_weights`) | Весовая колонка на 5 ролей |
| `src/combat/ai/plan/scorer.rs` (или модуль внутри `scoring/factors/aggregate.rs`) | Агрегация по шагам плана (sum / max / discounted) |
| `src/combat/ai/config/difficulty.rs` | Ручка difficulty, если фактор должен зависеть от сложности |
| [scoring.md](scoring.md) | Строка в таблице факторов |

## Новый critic / SanityCheck

| Файл | Что |
|---|---|
| `src/combat/ai/pipeline/stages/critics/<name>.rs` | Реализация `PlanCritic` trait |
| `src/combat/ai/pipeline/stages/critics/mod.rs::CriticsStage::first_wave` | Регистрация в композиции |
| `src/combat/ai/pipeline/stages/sanity/` | Только если правило general-purpose и не маппится в critic |
| [critics.md](critics.md) / [pipeline.md](pipeline.md) | Запись в таблице |

SanityCheck = только мягкая корректировка цены. Если у тебя новое правило «если *факт X*, функция ценности этого плана неверна → пересчитай под другим `EvaluationMode`» — это `AdaptationReason`, не `SanityCheck`.

## Новый `AdaptationReason`

| Файл | Что |
|---|---|
| `src/combat/ai/adapt/select.rs` | Вариант `AdaptationReason` + триггер (fact-based) + applicability gate |
| `src/combat/ai/adapt/` или `plan/scorer.rs` | Если требуется новый `EvaluationMode`, добавить вариант + обработку в `compute_plan_intent_sum` |
| `src/combat/ai/log/mod.rs` | Serde-представление новой ветки reason в JSONL |
| `src/bin/replay_ai_log.rs` | Деструктура в verbose-выводе |
| [adaptation.md](adaptation.md) | Строка в таблице AdaptationReason |

## Ценность юнита / trade-экономика

| Файл | Что |
|---|---|
| `src/combat/ai/scoring/trade.rs` | `unit_value` слагаемое / `TradeBreakdown` поле / `trade_score` множитель |
| `src/combat/ai/pipeline/stages/modifiers/trade_bonus.rs` | Уже читает через public helper — при изменении формулы больше ничего |
| `src/combat/ai/log/mod.rs::TradeBlock` + `SCHEMA_VERSION` bump | Новое поле в JSONL / миграция старых логов через `#[serde(default)]` |
| `src/bin/replay_ai_log.rs::LoggedTradeBlock` | Mirror поля для деструктуризации |
| [trade-economy.md](trade-economy.md) | Строки в разделе |

SanityCheck-аналог: если новое правило «эта *часть плана* даёт отрицательный value неочевидным образом» — это **не** trade-ветвь. Trade отвечает только на «что умирает, чья ценность списывается» — любая другая динамика (урон не до смерти, перемещение важного юнита, position lock) уходит в SanityCheck или в отдельный factor.

## Новый `DifficultyProfile` параметр

| Файл | Что |
|---|---|
| `src/combat/ai/config/difficulty.rs` | Поле + трио значений easy/normal/hard + derived |
| Потребитель(и) | Чтение поля при принятии решения |
| [difficulty.md](difficulty.md) | Строка в таблице Difficulty |

## Новая константа тюнинга (`AiTuning`)

Вместо hardcoded в `const` — миграция в data-driven `AiTuning` (step 2a).

| Файл | Что |
|---|---|
| `src/combat/ai/config/tuning.rs` | Поле в `Thresholds` / `Tables` / `Difficulty` + дефолт |
| `assets/data/ai_tuning.toml` | Значение в соответствующей секции |
| Потребитель | Читает через `ctx.world.tuning.thresholds.<field>` (или `.tables.` / `.difficulty.`) |
| `src/combat/ai/config/tuning.rs::ThresholdsOverride` | Поле override если поле должно уметь перекрываться per-unit (scaffolding сейчас только для `Thresholds`) |

Правила:

- Классы thresholds (scalar) / tables (role-axis matrices) / difficulty (LerpCurve) — зависит от природы параметра.
- Формулы не менять в миграции — только перенос данных; golden-replay должен быть 0 diff.
- `DifficultyProfile` per-tier values (easy/normal/hard/epic) остаются в `difficulty.rs`; lerp endpoints для derived методов — в `AiTuning.difficulty`.

## `SCHEMA_VERSION` bump

Любое изменение, ломающее формат JSONL-логов (новое/изменённое поле в `PlanAnnotation`, `UnitSnapshot`, `IntentBlock`, `ScoreTraceLog`, и т.п.).

| Шаг | Что |
|---|---|
| `src/combat/ai/log/mod.rs::SCHEMA_VERSION` | Bump const. `MIN_SUPPORTED = SCHEMA_VERSION - 1` обновится автоматически. |
| `#[serde(default)]` на новых полях | Чтобы v(N-1)-логи всё ещё разворачивались. |
| `tests/ai_scenarios/snapshots/*/log.jsonl` | Если изменился shape — пересохранить fixtures (играя сценарии заново при включённом `[debug].ai_log = true`). |
| `tests/baselines/baseline_v<N>.jsonl` | Recapture: `cargo run --release --bin replay_ai_log -- --capture-golden tests/baselines/baseline_v<N>.jsonl tests/ai_scenarios/snapshots/*/log.jsonl` |
| `tests/golden_smoke.rs::baseline_path` | Обновить путь на новый файл. |
| `logs/baseline_v<N-1>.jsonl` | Удалить после подтверждения, что новый baseline даёт `0 / N diverged` на self-check. |
| [replay.md](replay.md) | Обновить *Current schema: v<N>* и блок Schema versions, если новое поле семантически значимо для replay. |

**Принцип continuous re-capture.** Не пытаемся поддерживать совместимость с произвольно старыми логами — schema bump'ы случаются часто, цена backward-compat'a большая, а corpus дешевле пересохранить. Поэтому: replay принимает только текущий `v<N>` и `v<N-1>`, всё старое — reject.

**Behaviour-preserving vs intentional behaviour change.** Если изменение поведенческое (например, `KillableGate` semantics в Phase 3) — `--compare-golden` ожидаемо вернёт ≠ 0. Это сигнал к ручному review каждого diverged decision'a через `--verbose` replay. После подтверждения — recapture, чтобы новое поведение стало baseline'ом.

### Recent bumps (changelog)

| From → To | Trigger | Wave |
|---|---|---|
| v42 → v43 | `CombatState.blocked_hexes` (static obstacles), `Unit.template_id` (engine-side initial_statuses), `Effect::ApplyStatus` with `PERMANENT_DURATION` sentinel | ch2 Wave 1 |
| v43 → v44 | `AiTags::OPPONENT_OBJECTIVE` bit (0x100); `KeepAliveTarget` component; `unit_value` objective bonus; `target_selection_score` objective_priority axis (0.35) | AI KeepAlive awareness |

## `engine.jsonl` (Phase 5 trace schema)

Engine-trace (`engine.jsonl`) — отдельный поток JSONL, независимый от AI-лога. Bumpается через `combat_engine::trace::SCHEMA_VERSION` при изменении shape `InitLine` / `StepLine`.

| Шаг | Что |
|---|---|
| `crates/combat_engine/src/trace.rs::SCHEMA_VERSION` | Bump const |
| Поля `InitLine` / `StepLine` | Добавить поле + `#[serde(default)]` для backward read (или hard break — `LogError::UnsupportedSchema`) |
| `crates/combat_engine/tests/replay.rs` | Если изменилось поведение `step()` — добавить/обновить canonical scenario |
| `src/bin/replay_engine_trace.rs` | Если новый flag / нужен новый assertion — обновить |

Принцип per-stream versioning (D4 Phase 5): AI log и engine trace bumpаются независимо. Нет shared constant.

## Новое поле `ActionOutcomeEstimate`

Добавление новой оси в outcome vector — для future consumer'ов (critics, geometry).

| Файл | Что |
|---|---|
| `src/combat/ai/outcome/mod.rs` | Поле в `ActionOutcomeEstimate` + docstring с семантикой |
| `src/combat/ai/outcome/builder.rs::from_sim_step` | Как populate (Cast / Move branches) |
| `src/combat/ai/outcome/builder.rs::hypothetical` | Populate для consumer'ов без sim (если нужно) |
| Consumer(ы) (`factors/offensive.rs`, `intent/score.rs`, `future_value.rs`, critics) | Чтение поля |
| `src/combat/ai/log/mod.rs::SCHEMA_VERSION` | Bump при изменении shape annotation |
| [scoring.md → Outcome vector](scoring.md#outcome-vector-outcome) | Строка в таблице |

## ECS-projected компонент (Phase 6 D6 contract)

Если добавляешь новый Bevy `Component`, **который должен зеркалить engine state** (engine — авторитет, ECS — read-only projection):

| Файл | Что |
|---|---|
| `src/game/components.rs` | Определи компонент |
| `src/combat/engine_bridge.rs::project_state_to_ecs` | Добавь arm записи (engine state → component) |
| `src/combat/engine_bridge.rs::bootstrap_combat_state` / `from_ecs` | Если bootstrap читает из ECS — добавь arm чтения |
| `tests/projection_isolation.rs` | Если выбираешь делать field публично-mutable (например, для UI-spawn) — добавь файл в `ALLOWED_FILES` с обоснованием |

**Контракт:** mutation engine-projected компонента может происходить ТОЛЬКО внутри `engine_bridge.rs` (или allowlisted spawn/init paths). Test `engine_projected_components_only_written_by_bridge` падает на любое нарушение.

---

## Трассировка: «почему AI не использует Х?»

Если новая способность / механика в игре не задействуется AI, проверяй по порядку:

1. **Знает ли актор способность?** — `world/snapshot.rs::build` фильтрует по `actor.abilities`.
2. **Проходит ли legality?** — `combat_engine::legality::check_legality`. Запусти с прицельным вызовом в тесте или debug-логе.
3. **Генерит ли кандидатов?** — `plan/generator.rs::rank_targets` match по `TargetType`. Пустой вектор = никогда не увидит каст.
4. **Проходит ли `ai_policy_ok`?** — эвристики overheal / wasted-CC / FF-ratio режут легальные, но невыгодные касты. Логируй возврат в тесте.
5. **Правильно ли populated outcome?** — `outcome::builder::from_sim_step` заполняет fact-поля `ActionOutcomeEstimate` после sim. Если новый effect / status не попал в `enemy_damage` / `cc_turns_applied` / `hp_restored` — `compute_offensive` прочтёт 0 и план получит низкий damage/cc/heal factor. JSONL-лог содержит annotation — проверить поля там.
6. **Выживает ли beam-pruning?** — если `partial_score` низкий из-за неучтённого фактора, план режется на глубине. Покрутить `plan_beam_width` на hard для диагностики.
7. **Не роняется ли в sanity?** — `SanityStage` умножает на малые факторы, но не зануляет; если итоговый score всё равно проигрывает — значит эвристики считают что-то другое лучше.
8. **Не валит ли его critic?** — `CriticsStage::first_wave` применяет multipliers; `score_trace_log.multipliers` (filter kind=Critic) в JSONL покажет hits с `MultiplierDetail::Critic { critic, reason }`.
9. **Подходит ли `intent`?** — `intent/score.rs::intent_score` может увести на −1.0, сделав план хуже любых альтернатив. Проверь для своей цепочки `(intent, step, outcome)`.
10. **Не проигрывает ли в agenda?** — `PickBestStage` выбирает лучшую пару (план × agenda_item). `ann.considerations_per_item` показывает per-item оценку.

Debug-оверлей + JSONL-лог (`AiLogger`) показывают топ-планы + raw-факторы + annotation — через них видно на каком слое запрос провалился.
