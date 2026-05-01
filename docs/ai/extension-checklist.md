# Extension Checklist

Куда смотреть при добавлении разных типов механик. Списки — стартовая точка, а не полная диагностика: Rust exhaustive-match ошибки при компиляции доведут до остальных принудительных точек.

**Общий принцип**: правишь ядро → правишь shared resolution (если meaningfully меняет исход каста) → правишь AI (enumerator → filters → scoring → intent). Каждый слой проходит один и тот же каст с разных сторон; пропуск в одном слое — это либо невалидное действие, либо неучтённое в планировщике.

## Новая способность (только TOML)

Файлы: `assets/data/abilities.toml` + соответствующие `classes.toml` / `unit_templates.toml` / `encounters.toml` для владельцев.

Код не трогаешь, **если** способность укладывается в существующие `TargetType`, `EffectDef`, `AoEShape`, `StatusOn`, `ResourceKind`. Если нет — см. соответствующий раздел ниже.

## Новый `TargetType`

Конкретный пример — `Ground` (см. git log вокруг fireball). Затрагивает:

| Файл | Что |
|---|---|
| `src/content/abilities.rs` | Вариант enum + парсер строки из TOML |
| `src/combat/actions/mod.rs` | match arm в `check_legality` (team/alive семантика) |
| `src/combat/resolution.rs` | `primary_target` match arm |
| `src/combat/ai/planning/sim.rs` | `primary` match arm |
| `src/combat/ai/planning/generator.rs::rank_targets` | Как перебирать кандидатов (сущности / клетки) |
| `src/combat/ai/scoring/horizon.rs` | Фильтры `estimate_st_damage`, `estimate_damage_horizon` — если offensive |
| `src/combat/ai/world/snapshot.rs` | `max_attack_range` фильтр — если это "атака" |
| `src/combat/ai/intent/mod.rs` | LastStand +0.5 offensive, прочие intent score'ы если релевантно |
| `src/ui/ability_panel.rs::build_description` | Русская подпись «цель: …» |
| `src/ui/hex_grid/input.rs` | Логика клика (что происходит при выборе клетки / сущности) |
| `src/combat/command_input.rs` | Tab-цикл (что перебирать), Enter-конфирм |
| `docs/content-guide.md` | Строка в списке допустимых target_type |

Тесты: позитивный + негативный кейсы в `combat::actions::tests` + генератор-тест в `combat::ai::planning::generator::tests`.

## Новый `EffectDef`

| Файл | Что |
|---|---|
| `src/content/abilities.rs` | Вариант enum + парсер + `EffectDef::calc` (если даёт число урона/хила) |
| `src/combat/effects_outcome.rs` | `OutcomePrimary` ветка + dispatch в `compute_ability_outcome` |
| `src/combat/resolution.rs` | Обработка нового `OutcomePrimary` (writer / side effects) |
| `src/combat/ai/planning/sim.rs::apply_primary` | Как sim мутирует snapshot |
| `src/combat/ai/scoring/policy/` | HP-эквивалент formulas (`damage::value`, `heal::value`, `cc::value`, `friendly_fire::penalty`) — если новый эффект нужно скорить, добавь named pure function |
| `src/combat/ai/outcome/builder.rs::from_sim_step` | Если новый эффект должен заполнять поле `ActionOutcomeEstimate` (damage/heal/cc/etc.) |
| `src/combat/ai/role.rs::ability_vote` | Голос за ось |
| `src/combat/ai/factors/offensive.rs` | Обычно менять не надо — `compute_offensive` читает `outcome` vector; новые эффекты попадают через `outcome::builder::from_sim_step` |

## Новое поле `StatusDef`

| Файл | Что |
|---|---|
| `src/content/statuses.rs` | Поле структуры + парсер |
| `src/combat/statuses.rs` | Применение эффекта в реальной резолюции (tick / damage_modifier / etc.) |
| `src/combat/ai/snapshot.rs::status_bonuses` | Агрегация в `UnitSnapshot` если это численный бонус |
| `src/combat/ai/snapshot.rs::compute_tags` | Выставление `AiTag` если флаг — сигнал для интента |
| `src/combat/ai/outcome/builder.rs::from_sim_step` | Агрегация в `cc_turns_applied` / `vulnerability_applied` / `armor_shred_applied` в outcome; `policy::status::*` — для value judgment |
| `docs/content-guide.md` | Комментарий в примере `[[statuses]]` |

## Новый `AiTag`

| Файл | Что |
|---|---|
| `src/combat/ai/world/snapshot.rs` | `AiTags` bitflag |
| `src/combat/ai/snapshot.rs::compute_tags` | Условие выставления |
| `src/combat/ai/intent/mod.rs::select_intent` | Используется в лестнице выбора интента |
| Прочие consumer'ы тега (например, фактор scarcity читает `AiTags::IS_STUNNED`) |

## Новый `TacticalIntent`

| Файл | Что |
|---|---|
| `src/combat/ai/intent/mod.rs` | Вариант enum |
| `src/combat/ai/intent/mod.rs::select_intent` | Скоринг условия выбора (таблица в [intent.md](intent.md#выбор-интента-scored--max-wins)) |
| `src/combat/ai/intent/mod.rs::intent_score` | Alignment scoring на `ScoredStep` |
| `src/combat/ai/intent/mod.rs` viability thresholds | Порог в viability guard |
| `src/combat/ai/intent/mod.rs::AiMemory` | Stickiness continuation — `kind()` + сравнение last_intent (если применимо) |

## Новая `AoEShape`

| Файл | Что |
|---|---|
| `src/content/abilities.rs` | Вариант enum + парсер |
| `src/combat/effects_math.rs::aoe_cells` | Перечисление клеток |
| `src/ui/hex_grid/visuals.rs::update_hex_visuals` | Preview-рендер под ховером |
| `src/combat/ai/factors/aoe_hits.rs` | Покрытие enemies/allies (если формула нестандартная) |
| `src/ui/ability_panel.rs::build_description` | Строка-подпись формы |

## Новый фактор scoring'а

| Файл | Что |
|---|---|
| `src/combat/ai/factors/step/` или `factors/plan/` | Реализация фактора (per-step или plan-уровень) + регистрация в `factors/registry.rs` |
| `src/combat/ai/factors/mod.rs` | Поле в `Factors` + нормализация (non-neg vs signed) |
| `assets/data/ai_tuning.toml` (`tables.axis_factor_weights`) | Весовая колонка на 5 ролей |
| `src/combat/ai/planning/scorer.rs` | Агрегация по шагам плана (sum / max / discounted) |
| `src/combat/ai/difficulty.rs` | Ручка difficulty, если фактор должен зависеть от сложности |
| [scoring.md](scoring.md) | Строка в таблице факторов |

## Новый critic / SanityCheck

| Файл | Что |
|---|---|
| `src/combat/ai/pipeline/stages/critics/<name>.rs` | Реализация `PlanCritic` trait |
| `src/combat/ai/pipeline/stages/critics/mod.rs::CriticsStage::first_wave` | Регистрация в композиции |
| `src/combat/ai/planning/sanity.rs` | Только если правило general-purpose и не маппится в critic |
| [critics.md](critics.md) / [pipeline.md](pipeline.md) | Запись в таблице |

SanityCheck = только мягкая корректировка цены. Если у тебя новое правило «если *факт X*, функция ценности этого плана неверна → пересчитай под другим `EvaluationMode`» — это `AdaptationReason`, не `SanityCheck`.

## Новый `AdaptationReason`

| Файл | Что |
|---|---|
| `src/combat/ai/adapt/select.rs` | Вариант `AdaptationReason` + триггер (fact-based) + applicability gate |
| `src/combat/ai/intent/mod.rs` или `planning/scorer.rs` | Если требуется новый `EvaluationMode`, добавить вариант + обработку в `compute_plan_intent_sum` |
| `src/combat/ai/log.rs` | Serde-представление новой ветки reason в JSONL |
| `src/bin/replay_ai_log.rs` | Деструктура в verbose-выводе |
| [adaptation.md](adaptation.md) | Строка в таблице AdaptationReason |

## Ценность юнита / trade-экономика

| Файл | Что |
|---|---|
| `src/combat/ai/scoring/trade.rs` | `unit_value` слагаемое / `TradeBreakdown` поле / `trade_score` множитель |
| `src/combat/ai/pipeline/stages/modifiers/trade_bonus.rs` | Уже читает через public helper — при изменении формулы больше ничего |
| `src/combat/ai/log.rs::TradeBlock` + `SCHEMA_VERSION` bump | Новое поле в JSONL / миграция старых логов через `#[serde(default)]` |
| `src/bin/replay_ai_log.rs::LoggedTradeBlock` | Mirror поля для деструктуризации |
| [trade-economy.md](trade-economy.md) | Строки в разделе |

SanityCheck-аналог: если новое правило «эта *часть плана* даёт отрицательный value неочевидным образом» — это **не** trade-ветвь. Trade отвечает только на «что умирает, чья ценность списывается» — любая другая динамика (урон не до смерти, перемещение важного юнита, position lock) уходит в SanityCheck или в отдельный factor.

## Новый `DifficultyProfile` параметр

| Файл | Что |
|---|---|
| `src/combat/ai/difficulty.rs` | Поле + трио значений easy/normal/hard + derived |
| Потребитель(и) | Чтение поля при принятии решения |
| [difficulty.md](difficulty.md) | Строка в таблице Difficulty |

## Новая константа тюнинга (`AiTuning`)

Вместо hardcoded в `const` — миграция в data-driven `AiTuning` (step 2a).

| Файл | Что |
|---|---|
| `src/combat/ai/tuning.rs` | Поле в `Thresholds` / `Tables` / `Difficulty` + дефолт |
| `assets/data/ai_tuning.toml` | Значение в соответствующей секции |
| Потребитель | Читает через `ctx.world.tuning.thresholds.<field>` (или `.tables.` / `.difficulty.`) |
| `src/combat/ai/tuning.rs::ThresholdsOverride` | Поле override если поле должно уметь перекрываться per-unit (scaffolding сейчас только для `Thresholds`) |

Правила:

- Классы thresholds (scalar) / tables (role-axis matrices) / difficulty (LerpCurve) — зависит от природы параметра.
- Формулы не менять в миграции — только перенос данных; golden-replay должен быть 0 diff.
- `DifficultyProfile` per-tier values (easy/normal/hard/epic) остаются в `difficulty.rs`; lerp endpoints для derived методов — в `AiTuning.difficulty`.

## Новое поле `ActionOutcomeEstimate`

Добавление новой оси в outcome vector — для future consumer'ов (critics, geometry).

| Файл | Что |
|---|---|
| `src/combat/ai/outcome/mod.rs` | Поле в `ActionOutcomeEstimate` + docstring с семантикой |
| `src/combat/ai/outcome/builder.rs::from_sim_step` | Как populate (Cast / Move branches) |
| `src/combat/ai/outcome/builder.rs::hypothetical` | Populate для consumer'ов без sim (если нужно) |
| Consumer(ы) (`factors/offensive.rs`, `intent/mod.rs`, `future_value.rs`, critics) | Чтение поля |
| `src/combat/ai/log.rs::SCHEMA_VERSION` | Bump при изменении shape annotation |
| [scoring.md → Outcome vector](scoring.md#outcome-vector-outcome) | Строка в таблице |

---

## Трассировка: «почему AI не использует Х?»

Если новая способность / механика в игре не задействуется AI, проверяй по порядку:

1. **Знает ли актор способность?** — `snapshot.rs::build` фильтрует по `actor.abilities`.
2. **Проходит ли legality?** — `check_legality` в `actions/mod.rs`. Запусти с прицельным `check_legality` в тесте или debug-логе.
3. **Генерит ли кандидатов?** — `generator.rs::rank_targets` match по `TargetType`. Пустой вектор = никогда не увидит каст.
4. **Проходит ли `ai_policy_ok`?** — эвристики overheal / wasted-CC / FF-ratio режут легальные, но невыгодные касты. Логируй возврат в тесте.
5. **Правильно ли populated outcome?** — `outcome::builder::from_sim_step` заполняет 17 fact-полей `ActionOutcomeEstimate` после sim. Если новый effect / status не попал в `enemy_damage` / `cc_turns_applied` / `hp_restored` — `compute_offensive` прочтёт 0 и план получит низкий damage/cc/heal factor. JSONL-лог содержит annotation (schema v28+) — проверить поля там.
6. **Выживает ли beam-pruning?** — если `partial_score` низкий из-за неучтённого фактора, план режется на глубине. Покрутить `plan_beam_width` на hard для диагностики.
7. **Не роняется ли в sanity?** — `SanityStage` умножает на малые факторы, но не зануляет; если итоговый score всё равно проигрывает — значит эвристики считают что-то другое лучше.
8. **Не валит ли его critic?** — `CriticsStage::first_wave` применяет 6 multipliers; `ann.critics` в JSONL покажет hits.
9. **Подходит ли `intent`?** — intent_score может увести на −1.0, сделав план хуже любых альтернатив. Проверь `intent_score` для своей цепочки `(intent, step, outcome)`.
10. **Не проигрывает ли в agenda?** — `PickBestStage` выбирает лучшую пару (план × agenda_item). `ann.considerations_per_item` показывает per-item оценку.

Debug-оверлей + JSONL-лог (`AiLogger`) показывают топ-планы + raw-факторы + annotation (schema v28+) — через них видно на каком слое запрос провалился.
