# Enemy AI

## Overview

AI-система выбирает действие для вражеских юнитов (и героев под `pact_control`). Работает в рамках `CombatStep::Command`: `enemy_ai_system` для Team::Enemy, `pact_ai_system` для героев с `ai_controlled`-статусом.

Файлы: `src/combat/ai/`

| Файл | Назначение |
|------|-----------|
| `enemy_turn.rs` | Главная система: построение snapshot/maps, вызов `pick_action`, отправка сообщений |
| `intent.rs` | TacticalIntent — выбор стратегической цели, фильтры и скоринг по интенту |
| `utility/` | Top-level decision pipeline: `mod.rs` (pick_action entry, `UtilityContext`, `AiDecision`), `pick.rs` (top-K/mercy sampling + reservations), `sanity.rs` (пост-скоринговые пенальти + `is_defensive`), `fallback.rs` (движение без кастов) |
| `factors/` | 9-факторный скоринг: `mod.rs` (`score_candidates`, `compute_factors`, нормализация), `offensive.rs` (damage/heal/kill/cc + `aoe_area`), `scarcity.rs` (ресурсы × swing), `adjustments.rs` (reservations + `crit_fail_adjusted`) |
| `candidates.rs` | Генерация пула `ActionCandidate` (движение + касты) |
| `scoring.rs` | Базовый скоринг пар (ability, target) в HP-эквивалентах |
| `target_priority.rs` | Оценка важности цели (угроза, добиваемость, уязвимость, роль, расстояние) |
| `position_eval.rs` | Оценка клетки по картам влияния с учётом роли |
| `constraints.rs` | Жёсткие фильтры кандидатов (до скоринга) |
| `snapshot.rs` | BattleSnapshot — снимок боя для чистых вычислений |
| `role.rs` | AxisProfile — 5-мерная роль (Tank/Melee/Ranged/Control/Support), инференс по kit'у |
| `difficulty.rs` | DifficultyProfile — ручки качества решений (awareness, decision_quality, survival_instinct, coordination, mercy, …) |
| `influence.rs` | Карты влияния — пространственная оценка клеток. `InfluenceConfig` resource хранит тюнинг-константы (λ-затухания, веса kill/threat, модификаторы healer/melee), меняется без recompile |
| `debug.rs` | Debug overlay: ресурс `AiDebugState`, консольный лог, визуализация influence maps |

## Цикл принятия решения

AI работает в модели «пула очков движения» (`ActionPoints.movement_points: i32`).
Принципиальное отличие от старой булев-модели: за один ход AI может принять
несколько решений — каждый `AwaitCommand`-тик pipeline запускает
`enemy_ai_system` заново, видит обновлённые `ap` и снова выбирает действие.
Deep-search не ведётся — это чистый greedy re-plan.

```
Один тик enemy_ai_system:
1. Проверка AP (нет action + movement_points == 0 → EndTurn)
2. Построить BattleSnapshot + InfluenceMaps
3. BFS reachable_with_paths (со скоростью из snapshot — учитывает speed-дебафы)
4. Построить UtilityContext (caster, abilities, difficulty, crit_fail)
5. pick_action:
   a. ★ select_intent → выбор TacticalIntent
   b. Генерация кандидатов:
      — Cast: для каждой достижимой клетки × каждая способность
        (отбрасывается, если `path.len() > active.movement_points`)
      — MoveOnly: топ-3 клеток по escape map (если `movement_points > 0`)
   c. Жёсткие фильтры (constraints)
   d. 9-факторный utility scoring с весами по роли
   e. ★ Intent viability guard: если max(intent_factor) < threshold,
      переключиться на FocusTarget { default reachable target } и rescore
   f. Sanity adjust (multiplicative penalties)
   g. Если intent == ProtectSelf: маскировать non-defensive кандидатов в -∞;
      если defensive нет — rescore под LastStand
   h. pick_best: мерси-окно [best-mercy, best] → rerank, затем top-K sampling
      внутри similarity window (score_noise × 2)
6. Исполнение — ни одно из этих ветвлений не пишет EndTurn напрямую:
   - Cast@actor_pos          → CastInPlace        (UseAbility)
   - Cast@other              → MoveAndCast        (MoveUnit + UseAbility)
   - MoveOnly                → MoveOnlyRetreat    (MoveUnit)
   - нет кандидатов          → fallback_move      (MoveCloser / EndTurn)
```

### Re-plan в пределах хода

Ход завершается только через явный `AiDecision::EndTurn` либо через
guard-проверку в начале следующего тика (`!action && movement_points == 0`).
Типичные сценарии:

- **Move → Cast → Move.** Первый тик: `MoveAndCast` (путь тратит часть пула,
  способность расходует action). Следующий тик: action=false, пул>0 — Cast-
  кандидатов нет, выбирается `MoveOnly` или `fallback_move(MoveCloser)` для
  отступа в укрытие. Далее — пока есть очки движения.
- **Cast → Move.** `CastInPlace` тратит только action; next tick — как выше.
- **Обрыв прогресса.** `fallback_move` гарантирует non-empty путь; если лучший
  тайл совпадает с текущим — возвращается `EndTurn`. Иначе каждый `MoveCloser`
  строго уменьшает пул, поэтому за ≤ `speed` тиков достигается
  `movement_points == 0` и срабатывает guard.

### `GrantMovement` mid-turn

Способности с эффектом `GrantMovement { distance }` (например, Рывок)
**немедленно** добавляют `distance` в пул активного юнита и вставляют
marker-компонент `BonusMovement` (триггер UI auto-move-mode для игрока; для AI
просто меняется `movement_points`, следующий тик его увидит). Это корректно
работает для pact-AI и врагов: каст и последующий рефрейм reach-map
происходят на разных тиках, так что расширенный пул сразу доступен для
планирования маршрута.

## Utility Scoring

Каждый `ActionCandidate` оценивается по 9 факторам. Факторы делятся на два типа с разной нормализацией:

* **Non-negative** `[0, 1]`: `/ max` в батче
* **Signed** `[-1, 1]`: `/ max(|min|, |max|)` в батче — симметричная, сохраняет знак и порядок

Финальный скор = `dot(normalized_factors, role_weights) + noise`.

### Факторы

| Фактор | Тип | Источник | Нормализация |
|--------|-----|---------|--------------|
| `damage` | non-neg | `score_action()` (HP-эквиваленты) | `/ max` |
| `kill` | non-neg | 1.0 если expected >= target.hp | бинарный |
| `cc` | non-neg | Ценность статусов (stun × threat × duration) | `/ max` |
| `heal` | non-neg | Скор хила | `/ max` |
| `position` | **signed** | `evaluate_position()` | `/ max(|min|,|max|)` |
| `risk` | non-neg | `1 - danger(tile)` | `/ max` |
| `focus` | non-neg | `target_priority()` | уже 0..1 |
| `intent` | **signed** | `intent_score()` — соответствие TacticalIntent | `/ max(|min|,|max|)` |
| `scarcity` | **signed** | `swing_value - resource_ratio` | `/ max(|min|,|max|)` |

### Весовые таблицы по осям (AxisProfile)

Вместо enum-ролей используется `AxisProfile` — вектор весов по 5 архетипным осям. Roles стали **emergent**: glass cannon = high Melee + low Tank, brawler = Tank + Melee mix, battle-healer = Support + Ranged.

**Axis factor weights (утилити-скоринг):**

| Фактор | Tank | Melee | Ranged | Control | Support |
|--------|------|-------|--------|---------|---------|
| damage | 0.4 | 1.3 | 1.3 | 0.4 | 0.2 |
| kill | 0.6 | 1.6 | 1.3 | 0.5 | 0.3 |
| cc | 0.5 | 0.2 | 0.3 | 1.6 | 0.6 |
| heal | 0.2 | 0.0 | 0.0 | 0.0 | 2.0 |
| position | 1.2 | 0.9 | 1.1 | 0.9 | 1.0 |
| risk | 0.4 | 0.4 | 0.7 | 0.7 | 1.0 |
| focus | 0.6 | 0.9 | 0.7 | 0.8 | 0.5 |
| intent | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 |
| scarcity | 0.4 | 0.3 | 0.5 | 1.2 | 0.8 |

### Composition: squared-smooth bias

Итоговые role-weights — не линейное усреднение осей, а **смещённое в сторону доминирующей оси** через power exponent `1.5`:

```
biased[i] = profile[i]^1.5 / Σ(profile[j]^1.5)
factor_weight[f] = Σ(biased[i] × AXIS_FACTOR_WEIGHTS[i][f])
```

Эффект: pure-axis профили сходятся к enum-like поведению (pure Support heal=2.0 после bias), hybrid'ы сохраняют вторичные оси без сильной dilution (Буревестник ~73% Ranged + 18% Support после bias, heal = 0.36 — в разы меньше pure Support, что matches гибридную идентичность).

### Инференс профиля (role.rs::infer_profile)

Каждая ability голосует за оси с весом `1 + total_cost`:

| Ability pattern | Голос |
|---|---|
| SingleAlly + Heal effect | Support +weight |
| Myself + no damage (taunt, rush) | Tank +weight |
| AoE / SpellDamage / ranged physical + damage | Ranged +weight (+0.4×w Control если есть status) |
| Melee physical + damage | Melee +weight (+0.4×w Control если есть status) |
| Status-only (stun, paralyze) | Control +weight |
| Movement/utility fallback | Melee +0.3×w |

Плюс **stat-based Tank bonus**: `(max_hp + armor×2) / 20`, clamped [0.3, 2.0]. Armor считается ×2 как «active defense».

**TODO:** мели-cleave (aoe != None && range.max == 1) сейчас голосует за Ranged; при появлении такого контента переключить на Melee.

### TacticalIntent (intent.rs)

AI выбирает один стратегический интент перед генерацией кандидатов. Интент не фильтрует кандидатов жёстко — он выражается через фактор `intent` в scoring и через viability guard (если никто не соответствует).

#### Выбор интента (scored — max wins)

| Условие | Intent | Score |
|---------|--------|-------|
| HP < `survival_hp_threshold` И danger > `awareness_danger_threshold` | **ProtectSelf** (hard override) | — |
| HP < 40% И danger > 0 | **ProtectSelf** | (1−hp%)×danger |
| CAN_HEAL И союзник (вкл. self) с HP < `threshold` | **ProtectAlly { ally }** (самый раненый) | 1 − ally_hp% |
| **Есть враг с FORCES_TARGETING** | **FocusTarget { taunter }** (override) | 1.2 |
| **Есть taunter И CAN_CC И taunter не оглушён** | **ApplyCC { taunter }** | 0.8 + threat×0.1 |
| Нет taunter: враг убиваем И достижим за `speed+max_attack_range` | **FocusTarget { killable_target }** | 1.2 + (1−hp%)×0.3 |
| Нет taunter: — (иначе) | **FocusTarget { default }** | 0.5 + prio×0.3 |
| Нет taunter: CAN_CC И есть не-оглушённый враг | **ApplyCC { target }** | 0.8 + threat×0.1 |
| HAS_AOE И враги кластерируются (расст. ≤ 2) | **SetupAOE** | 0.7 + clusters×0.2 |
| pos_eval(текущая) < `awareness_reposition_threshold` | **Reposition** | 0.3 + gap×0.4 |

При taunt'е FocusTarget/ApplyCC запрещают выбор другой цели — иначе viability guard впустую пересчитывал бы intent из-за filter-redirection в constraints.

**ProtectAlly threshold** — role-aware: `0.5 + profile.support × 0.2`. Pure damage-dealers триггерят на 50%, pure healers на 70%, hybrid battle-mages пропорционально между — раньше входят в healer-mode, чем их damage-identity могла бы подсказать.

Stickiness bonus `+0.25` за продолжение того же intent'а (+`0.15` если ещё и target тот же), до 3 ходов подряд.

#### Intent viability guard

После scoring: если `max(intent_factor)` по всем кандидатам ниже порога для данного intent'а — intent переключается на `FocusTarget` над **достижимой** целью (исключая исходную, если она была FocusTarget'ом), candidates rescored.

| Intent | Минимум intent_factor для «работоспособно» |
|--------|-------------------------------------------|
| Reposition | 0.01 (хоть какое-то улучшение) |
| FocusTarget | 1.0 (кандидат реально таргетит цель) |
| ApplyCC | 0.5 (CC на цель или хотя бы damage) |
| ProtectAlly | 0.5 (хил или позиция рядом) |
| SetupAOE | 0.01 (какой-то AoE hit) |
| ProtectSelf / LastStand | — (спец-ветка ниже) |

При срабатывании fallback в debug-логе: `Intent: FocusTarget → X [fallback from Reposition: max_align=-1.00 < threshold=0.01]`.

#### Intent-скоринг (фактор `intent`)

| Intent | Cast score | MoveOnly score |
|--------|-----------|----------------|
| FocusTarget | 1.0 если target совпадает; heal = 0.3; остальное −0.5 | 0.0 (нейтрально) |
| ApplyCC | 1.0 CC на цель; 0.5 damage на цель; −0.5 CC мимо; 0.0 прочее | 0.0 |
| Reposition | **tiered** (см. ниже) | tiered |
| ProtectSelf | self-heal/self-buff = **1.0** (полная защита); иначе `1 − danger(tile)` | `1 − danger(tile)` |
| ProtectAlly | 1.0 heal ally; −0.3 heal wrong; 0.5 tile adj | 0.5 если adj к ally; 0.0 иначе |
| SetupAOE | hits/total или −0.3 для single-target | 0.0 |
| LastStand | dmg+kill+CC offensive combo | −0.3 (ретрит под LastStand неуместен) |

**Reposition tiered** (одинаков для Cast и MoveOnly):

| Условие | Score |
|---------|-------|
| `improvement ≥ reposition_min_improvement` | `improvement.min(2.0)` |
| `0 < improvement < min_improv` | **0.0** (что-то улучшилось, нейтрально) |
| `improvement ≤ 0` + Cast | **−0.3** (хотя бы действие сделал) |
| `improvement ≤ 0` + MoveOnly | **−1.0** (wasted turn) |

Идея: формально «недостаточное» улучшение тайла всё равно лучше чем ничего; pure retreat без улучшения — потеря хода. Cast с плохой позиции хотя бы полезен.

#### ProtectSelf branch

После scoring, если intent == ProtectSelf:
- Все **не-defensive** кандидаты маскируются в `−∞`. Defensive = SingleAlly / Myself cast ИЛИ MoveOnly на безопасную клетку ИЛИ любой cast с клетки safer-by-`defensive_tile_margin`.
- Если defensive кандидатов нет — rescore под `LastStand` intent (доминирует kill/cc/aoe из последнего полезного действия).
- Retreat **не** отдельная ветка — MoveOnly кандидат уже в pool, соревнуется по scoring.

### Resource Scarcity (фактор `scarcity`)

Оценивает, стоит ли тратить ресурс на эту способность в данной ситуации. Бесплатные способности всегда получают 0.0 (нейтральны).

```
scarcity = (swing_value - resource_ratio).clamp(-1.0, 1.0)
```

**resource_ratio** = `max(cost / current_pool)` по всем ресурсам способности (0..1).

**swing_value** — ситуационная ценность:

| Условие | Бонус |
|---------|-------|
| Kill (kill > 0) | +0.8 |
| Kill — role-value бонус | +0.35 × `target.role.role_value()` (Support ~0.35, Melee ~0.17, Tank ~0.1) |
| AoE hits > 1 | +0.2 × (hits − 1) |
| CC на high-threat unstunned | +0.5 × (threat/10) |
| Цель < 25% HP и есть бесплатная атака | −0.3 |
| Round ≤ 1 | −0.15 |

Примеры:
- Fireball (5 mana, pool=10) на 1 HP цель при наличии melee: scarcity ≈ −0.8 (не трать ману)
- Fireball на кластер 3 врагов: scarcity ≈ +0.7 (стоит потратить)
- Stun на high-threat: scarcity ≈ +0.2 (оправдано)

## Базовый скоринг (scoring.rs)

Каждая пара (ability, target) оценивается в **HP-эквивалентах** — используется как фактор `damage`/`heal` в utility scoring.

### Damage
```
raw = max(0, expected - armor + damage_taken_bonus)
progress = min(raw / target.hp, 1.0)
score = raw × (0.5 + 0.5 × progress)
```
Progress-множитель: удар, пробивающий большой % HP цели, ценится выше «капли в полный бассейн». Baseline 0.5 сохраняет значимость обычного удара; bonus до +50% награждает добивающие/быстрые удары. Kill остаётся отдельным бинарным фактором.

### Threat (`estimate_st_damage`)
Best single-target expected damage (одна способность, до армора). Не учитывает AoE, хил и утилити — только damage output. Используется в killable-проверке intent'а и как входной сигнал для `focus` / threat-density.

### Heal
```
effective = min(expected, missing_hp)
delta_pct = effective / target.max_hp
score = delta_pct × target.threat
```
Размерность ≈ «сколько HP/ход врага я сохранил, зашив эту часть союзника» — сопоставимо с `damage`. Хил полного HP = 0 (missing=0). Без отдельного urgency-множителя: низкий HP имеет большее `missing_hp`, значит больший `delta_pct` — urgency отражается неявно.

### Status Effects
```
skips_turn      → +threat × duration
damage_taken_δ  → +|delta| × duration
armor_δ         → +|delta| × duration
dot_dice        → +dice.expected() × duration
hp_percent_dot  → +ceil(max_hp × pct / 100) × duration
silence         → +threat × 0.5 × duration (partial stun)
speed_penalty   → +|bonus| × duration
```
Используется абсолютное значение для armor/vuln, чтобы и дебафф на врага, и бафф на союзника скорились корректно.

### AoE
Сумма score_action по всем целям в зоне. Friendly fire (включая self-damage) вычитается с весом `raw_dmg × (1 + raw_dmg / max_hp)` — хрупкие юниты получают пропорционально больший штраф за урон по себе/союзникам.

### Critical Failure Adjustment
- Miss: `score × (1 - crit_chance)`
- ManaOverload: `score - crit_chance × mana_cost`
- CircuitBreach: `score × (1 - crit_chance) - crit_chance × mana_cost × 0.5`

## Target Priority (target_priority.rs)

Оценка важности цели (0..1), используется как фактор `focus`.

| Фактор | Вес | Формула |
|--------|-----|---------|
| Threat | 0.20 | `target.threat / max_threat` |
| Killability | 0.20 | `1 − eff_hp / eff_max_hp` (armor-aware) |
| Threat density | 0.20 | `(threat / eff_hp) / max_density` — DPS-на-HP-до-смерти |
| Vulnerability | 0.15 | `+0.3` если LOW_HP, `+0.2` если damage_taken_bonus > 0 |
| Proximity | 0.15 | `1 / (1 + distance)` |
| Role value | 0.10 | `target.role.role_value()` — weighted mix: Support=1.0, Control=0.8, Ranged=0.7, Melee=0.5, Tank=0.3 |

`eff_hp` = `hp + armor + armor_bonus`. Killability и threat density используют effective HP — танк с 5 HP и 10 armor менее «добиваем», чем маг с 5 HP без брони.

## Position Evaluation (position_eval.rs)

Оценка клетки — линейная комбинация 3 независимых карт влияния с весами по профилю. Escape (ally_support − danger) не включён, т.к. линейно зависим — используется только в отборе safe-tiles и debug overlay.

**Axis position weights** (composed через biased_normalized):

| Карта | Tank | Melee | Ranged | Control | Support |
|-------|------|-------|--------|---------|---------|
| danger | −1.0 | −0.9 | −1.8 | −1.5 | −2.5 |
| ally_support | 0.7 | 0.4 | 0.7 | 0.8 | 1.3 |
| opportunity | 0.9 | 1.5 | 1.0 | 0.8 | 0.5 |

Итоговый вес = `Σ(biased[axis] × axis_weight[axis])`. Pure Tank: danger=−1.0 (терпит опасность, держит линию). Pure Melee: opportunity=1.5 (лезет на атакующие клетки). Pure Support: danger=−2.5 (максимум аккуратности).

Карты нормализованы в [0,1], поэтому equal-scaling через `awareness` сокращался бы при нормализации факторов и эффекта не давал. Для реального эффекта `awareness` сдвигает пороги решений в `intent.rs` (см. Difficulty).

## Hard Constraints (constraints.rs)

Фильтрация кандидатов **до** скоринга. Удаляет принципиально невозможное:

1. **Forced targeting (taunt)** — taunt ограничивает **только** SingleEnemy Cast на taunted-целях. SingleAlly / Myself / MoveOnly проходят свободно: heal, self-buff и отход не ограничены taunt'ом
2. **Team safety** — `SingleAlly` должен таргетить союзника; `SingleEnemy` — врага. Страховка от багов генерации
3. **Don't AoE allies/self** — отклонить AoE с `friendly_fire`, если кастер в зоне поражения или союзников больше чем `enemies / 2`
4. **Don't waste CC** — не тратить стан на уже оглушённую цель
5. **Don't overheal** — не лечить цель выше 90% HP

Survival-риск (LOW_HP + опасная клетка) теперь **не** hard filter, а квадратичный градиентный штраф в `sanity_adjust` — иначе LOW_HP юнит на опасном поле терял все retreat-кандидаты.

MoveOnly-кандидаты бо́льшую часть фильтров пропускают — применяется только team-safety (N/A для MoveOnly, т.к. нет target).

## Roles (AxisProfile)

Роль — **вектор весов по 5 осям**, инференс через `infer_profile(abilities, max_hp, armor, db)`. Пустые/TOML-override строки (`ai_role = "mage"`) конвертируются в pure-axis профиль через `From<AiRole>`.

**5 архетипных осей:**

| Axis | Смысл | Primary поведение |
|------|-------|-------------------|
| Tank | держит позицию, впитывает урон | высокий danger-tolerance, pos-priority, защитные касты |
| Melee | ближний урон | opportunity-seeking, fokus на уязвимых |
| Ranged | дальний урон | distance-keeping, высокий pos, средний risk |
| Control | CC / дебаффы | CC-priority, scarcity (дорого и стратегически) |
| Support | heal / buffs | heal-dominant, risk-aware, держится с группой |

**Emergent-роли через композицию:**

| Конфигурация | Старое enum-имя | Примеры |
|---|---|---|
| high Tank + medium Melee | Bruiser | Воин stormborn |
| low Tank + high Melee | Assassin | Молниеносец |
| high Ranged (+ optional Control) | Archer / Mage | Kael (ranger), Lyra (mage) |
| high Support | Support | pure healer |
| hybrid: Ranged + Support | "battle healer" | Буревестник (thunderstrike + heal) |
| balanced | "shaman" | Старшина (heal + burn + spark) |

## Difficulty

`DifficultyProfile` — ручки **качества решений**, а не статовые множители. Каждое поле меняет, *как* AI думает: что он замечает, как аккуратно выбирает ход, насколько дисциплинирует ресурсы, как работает в команде.

### Поля

| Параметр | Easy | Normal | Hard | Описание |
|----------|------|--------|------|----------|
| `awareness` | 0.55 | 0.80 | 1.00 | Сдвиг порогов: HP-паника, узнавание опасности, триггер Reposition |
| `decision_quality` | 0.30 | 0.75 | 1.00 | Derived → `score_noise` (0.6→0.0) + `top_k_choice` (3/2/1) |
| `candidate_budget` | 12 | 20 | 30 | Глубина поиска (cap кандидатов после dedup) |
| `intent_commitment` | 0.75 | 1.00 | 1.20 | Множитель веса `intent` фактора в utility scoring |
| `survival_instinct` | 0.55 | 0.80 | 1.00 | Derived → `reposition_min_improvement`, `defensive_tile_margin`, `survival_hp_threshold` |
| `resource_discipline` | 0.60 | 1.00 | 1.20 | Множитель веса `scarcity` фактора |
| `coordination` | 0.40 | 0.90 | 1.30 | Сила overkill-пенальти + focus-fire бонуса на цели с reservations |
| `mercy` | 0.35 | 0.10 | 0.00 | Tie-breaker в окне `[best_score − mercy, best_score]`: внутри окна rerank по `score − mercy × cruelty`, где `cruelty = kill + min(0.5, cc × 0.1)` (kill доминирует). Lethal с большим отрывом не штрафуется. |

### Куда подключается

| Файл | Применение |
|------|-----------|
| `factors::score_candidates` | `score_noise`, множители `intent_commitment` / `resource_discipline` на `role_weights` |
| `candidates::generate_candidates` | `candidate_budget` заменяет жёсткий cap 25 |
| `factors::adjustments::apply_reservation_adjustments` | `overkill_damage_multiplier` + `focus_fire_bonus` на reservations |
| `utility::pick::pick_best_candidate` | `top_k_choice` (sampling), `mercy_margin` (cruelty-shift) |
| `utility::sanity::is_defensive` | `defensive_tile_margin` |
| `intent.rs::select_intent` | `awareness_danger_threshold` + `survival_hp_threshold` (hard override), `awareness_reposition_threshold` |
| `intent.rs::intent_score` | `reposition_min_improvement` для Reposition |

### Замечание про `awareness`

`awareness` *не* применяется как множитель к уже нормализованным картам/скорам — в таком виде он факторизуется и сокращается при симметричной нормализации факторов, не меняя порядок кандидатов. Вместо этого он сдвигает **пороги решений** в `intent.rs`: менее наблюдательный AI позже понимает, что клетка опасна, или что позиция требует отхода.

## Snapshot

`BattleSnapshot` — чистый снимок без Bevy-зависимостей (кроме Entity). Содержит `Vec<UnitSnapshot>` со всеми данными для AI-решений.

### UnitSnapshot
Позиция, HP, ресурсы (mana/rage/energy), скорость (с учётом speed-дебафов от статусов), список способностей, статусы, threat-оценка, `AiTags`, `max_attack_range` (max range офенсивных способностей — для reach-проверок в intent).

### AiTags (bitflags)
```
LOW_HP | CAN_HEAL | CAN_CC | HAS_AOE | IS_STUNNED | FORCES_TARGETING | RANGED | MELEE_ONLY
```
Вычисляются из текущего состояния юнита при построении снимка.

## Influence Maps

Пространственная оценка клеток грида. Каждая карта — `HashMap<Hex, f32>`, инициализированная для всех клеток поля (размер выводится из `GRID_ROWS`/`row_cols`).

**Нормализация:** все карты нормализованы к физически осмысленным шкалам. `danger`, `ally_support`, `opportunity` ∈ [0, 1] — доля доступного свойства. `escape` ∈ [-1, +1] — survival margin (положительный = безопаснее).

```rust
build_influence_maps(snap, active_team, &InfluenceConfig) → InfluenceMaps {
    danger,       // [0, 1] — доля вражеской угрозы, покрывающей клетку
    ally_support, // [0, 1] — доля союзной поддержки, доступной из клетки
    opportunity,  // [0, 1] — доля наступательной ценности, доступной из клетки
    escape,       // [-1, +1] — ally_support - danger (survival margin)
}
```

### Danger Map
Проекция угрозы от вражеских юнитов. Шкала: доля суммарной вражеской угрозы.

Для каждого врага:
1. BFS по speed → множество достижимых клеток
2. Каждую достижимую клетку расширить на max attack range (`hex_circle`)
3. `danger[cell] += enemy.threat`
4. Нормализация: `danger[cell] /= Σ(enemy.threat)`

Проходимость: враг проходит через своих союзников, заблокирован нашими юнитами.

### Ally Support Map
Близость к союзникам с экспоненциальным затуханием. Шкала: доля максимальной поддержки.

- Ядро: `support_weight(ally) × exp(-dist / λ_support)` (λ=2.5)
- Нормализация: `/ Σ(support_weight)`
- Веса: healer (CAN_HEAL) = 2.0, melee (MELEE_ONLY) ×1.5, базовый = 1.0

### Opportunity Map
Привлекательность позиции для атаки. Шкала: доля наступательной ценности.

- Ценность цели: `0.7 × (1 - hp%) + 0.3 × (threat / max_threat)` — threat нормализован
- Ядро: `target_value × exp(-dist / λ_opp)` (λ=3.0)
- Нормализация: `/ Σ(target_value)`

### Escape Map
Derived metric: `ally_support(cell) - danger(cell)`. Линейно зависима от danger и support — **не входит** в `evaluate_position()`.

Используется только в:
- `select_diverse_tiles` (safe tiles для кандидатов)
- `add_move_only_candidates` (топ-3 safe клеток → MoveOnly кандидаты)
- debug overlay

Значения:
- Положительный: клетка безопаснее, ближе к поддержке
- Нулевой: нейтрально
- Отрицательный: клетка под угрозой, далеко от поддержки

## Candidate Generation (candidates.rs)

Пайплайн генерации кандидатов:

1. Диверсифицированный отбор tiles (`select_diverse_tiles`): top-3 opportunity, top-3 escape, ближайшие к priority target, AoE-центры, LOS-tiles для ranged, **для CAN_HEAL — top-2 тайлов в heal-range от каждого раненого союзника (ranked by escape)**, текущая позиция. Итог сортируется по `(x, y)` для детерминизма.
2. Для каждой клетки × каждая способность → Cast-кандидаты с допустимыми target_pos. Target entity фильтруется через `snap.unit().is_some()` — исключает «залежавшиеся» позиции мёртвых (positions map не чистится при смерти)
3. MoveOnly: top-3 клетки по escape map → кандидаты `CandidateKind::MoveOnly`
4. Дедупликация: Cast по `(ability, target)` (кратчайший путь), MoveOnly по `tile`
5. Приоритетная сортировка: `(target_priority DESC, path_len ASC)` — высоко-приоритетные цели впереди
6. Per-target guarantee: перед truncate пинится 1 кратчайший кандидат на каждую уникальную цель — бюджет-cap не может вырезать всю «колонку» способов дойти до цели X
7. Cap: `difficulty.candidate_budget` (12 / 20 / 30)

```rust
struct ActionCandidate {
    tile: Hex,
    path: Vec<Hex>,
    kind: CandidateKind,
}
enum CandidateKind {
    Cast { ability: AbilityId, target_pos: Hex, target: Entity },
    MoveOnly,
}
```

MoveOnly кандидаты проходят scoring как обычно (damage/kill/cc/heal=0, position/risk — активны, intent зависит от интента, scarcity=0). На выходе `decision_from_candidate` превращает MoveOnly с non-empty path в `AiDecision::MoveOnlyRetreat`; MoveOnly с пустым путём (цель — текущая клетка) → `EndTurn`.

Если кандидатов нет — `fallback_move`: `LOW_HP` юниты → клетка с min danger; остальные → ближайший враг.

## Sanity Adjust (utility/sanity.rs)

После базового scoring по top-3 кандидатам применяются multiplicative-пенальти и один bonus:

| Проверка | Эффект | Условие |
|----------|--------|---------|
| **Survival (квадратичный)** | `×(1 − LOW_HP_FACTOR × hp_need × (danger − 0.5)²)`, пол 0.25 | всегда |
| **Healer exposure** | `×0.5` | юнит с `profile.support < 0.3` уходит от единственного healer'а (was near, будет far ≥ 2) |
| **LOS blindspot** | `×0.3` | RANGED юнит идёт на клетку без LOS до врага |
| **Retreat trap** | `×0.5` | целевая клетка имеет < 2 свободных соседей |
| **Self-AoE** | `×0.5` | AoE с friendly_fire центрирован так, что сам кастер в зоне |
| **Synergy bonus** | `×1.1` | `c.tile != actor_pos` И (safer OR better pos) И **useful cast** |

Где:
- `hp_need = clamp((0.6 − hp%) / 0.6, 0, 1)` — шкала «насколько критично HP»
- `LOW_HP_FACTOR = 1.2` (константа)
- `useful cast` = ability имеет direct effect (damage/heal) ИЛИ накладывает status
- Порядок: пенальти сначала, bonus в конце — множится на итоговый результат

**Survival заменяет** прежний `constraints::"Don't walk into death"` hard filter и прежний step-penalty. Градиент даёт retreat-кандидатам шанс дойти до скоринга при любой конфигурации опасности.

**Synergy bonus** даёт пропорциональный буст комбо «отошёл на лучшую клетку И сделал полезное». Мультипликативность сохраняет знак (базовый −0.5 не становится положительным) и масштабируется с силой base score.

## Pick Best Candidate

После scoring + sanity_adjust:

1. **Mercy окно**: `[best_score − mercy, best_score]`. Внутри окна rerank по `score − mercy × cruelty`, где `cruelty = kill + min(0.5, cc × 0.1)`. Lethal-ход с большим отрывом остаётся вне окна — mercy его не трогает.
2. **Similarity window для top_k**: pool = top-K кандидатов, чей score входит в `[best_after_mercy − window, best_after_mercy]`, где `window = max(score_noise × 2, 0.05)`. Кандидаты, очевидно хуже лучшего, выкидываются даже если top_k > 1.
3. **Случайный выбор** в пределах pool. Если в pool 1 элемент — детерминированно.

Это гарантирует: (a) явно лучший ход всегда побеждает, (b) близкие ходы флуктуируют по шуму / top_k, (c) mercy работает только как tie-breaker.

## Debug Overlay (debug.rs)

Инструмент отладки AI: консольный лог решений + визуализация карт влияния на гриде.

### Настройка

`assets/data/settings.toml`:
```toml
[debug]
ai_debug = true   # включает сбор данных и консольный лог (по умолчанию false)
```

### Управление

| Клавиша | Действие |
|---------|----------|
| `~` (Backquote) | Показать / скрыть overlay карт влияния на гриде |
| `1` | Danger map |
| `2` | Ally Support map |
| `3` | Opportunity map |
| `4` | Escape map |

`ai_debug` — мастер-переключатель: при `true` данные собираются на каждом ходу AI и лог печатается в консоль автоматически. Тильда управляет только отображением overlay.

### Консольный лог

При `ai_debug = true` на каждом ходу AI в stdout печатается блок:

```
═══ AI DEBUG: Зверокров Страж [Tank(0.52) + Melee(0.48)] ═══
  HP: 12/20 | threat: 7.0 | pos: (5,2) | tags: MELEE | act=true mov=true
  Intent: FocusTarget → Aldric [killable: threat=7.0×awareness=0.7=4.9 >= hp=4]
  Priority target: Aldric (0.73)
  ── Candidates (8 total, top 5) ──
  #1 melee_attack → Aldric @ (4,2)  [dmg=0.80 kill=1.00 ... int=1.00] = 4.12
     tile: dgr=3.5 ally=1.2 opp=0.8 esc=-2.3 eval=0.45
  #2 melee_attack → Lyra @ (3,2)    [dmg=0.55 kill=0.00 ... int=0.00] = 1.85
     tile: dgr=2.0 ally=0.8 opp=0.5 esc=-1.2 eval=0.30
  ── Decision ──
  MoveAndCast: (6,2) → (5,2) → melee_attack → Aldric ((6,2)→(5,2), 1 steps)
  dest (5,2): dgr=3.5 ally=1.2 opp=0.8 esc=-2.3 eval=0.45
════════════════════════════════
```

Содержимое лога:

| Блок | Данные |
|------|--------|
| Actor | Role label (`"Mage(0.73) + Support(0.18)"`), HP/max, threat, позиция (offset), AiTags, action/movement_points |
| Intent | Выбранный TacticalIntent + причина выбора (какое правило сработало, конкретные значения) |
| Priority target | Цель с наивысшим target_priority и её скор |
| Candidates | Топ-5 из N кандидатов: ability → target @ tile, 8 raw-факторов, total score, influence breakdown для tile |
| Decision | Финальное действие (CastInPlace/MoveAndCast/MoveCloser/EndTurn), маршрут, influence для целевой клетки |

### Grid Overlay

Цветовая визуализация выбранной карты влияния на hex-гриде:
- **Синий** → **Зелёный** → **Красный** (низкое → среднее → высокое значение)
- 32 цветовых бакета, материалы кешируются
- Overlay рисуется поверх обычных hex-цветов; при выключении (`~`) нормальные цвета восстанавливаются

### Архитектура

```
AiDebugState (Resource)
├── ai_debug: bool          ← из settings.toml
├── show_overlay: bool       ← тильда toggle
├── overlay_map: OverlayMapKind  ← 1-4
├── influence_maps: Option<InfluenceMaps>  ← обновляется каждый ход AI
└── snapshot: Option<AiDebugSnapshot>      ← потребляется print-системой
```

Системы:
- `toggle_debug_system` — обработка клавиш (~, 1-4), `run_if(AppState::Combat)`
- `print_ai_debug_system` — печать snapshot в stdout, `.after(CombatStep::Command)`
- `debug_overlay_system` — покраска hex-ячеек, `.after(update_hex_visuals)`

Данные собираются в `pick_action()`: функция принимает `debug: bool` и `debug_names: &HashMap<Entity, String>`, возвращает `(AiDecision, Option<AiDebugSnapshot>)`. Maps клонируются и сохраняются в ресурсе на каждом ходе AI (при `ai_debug=true`).
