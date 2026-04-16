# Enemy AI

## Overview

AI-система выбирает действие для вражеских юнитов (и героев под `pact_control`). Работает в рамках `CombatStep::Command`: `enemy_ai_system` для Team::Enemy, `pact_ai_system` для героев с `ai_controlled`-статусом.

Файлы: `src/combat/ai/`

| Файл | Назначение |
|------|-----------|
| `enemy_turn.rs` | Главная система: построение snapshot/maps, вызов `pick_action`, отправка сообщений |
| `intent.rs` | TacticalIntent — выбор стратегической цели, фильтры и скоринг по интенту |
| `utility.rs` | Utility AI: генерация кандидатов, 8-факторный скоринг, весовые таблицы по ролям |
| `scoring.rs` | Базовый скоринг пар (ability, target) в HP-эквивалентах |
| `target_priority.rs` | Оценка важности цели (угроза, добиваемость, уязвимость, роль, расстояние) |
| `position_eval.rs` | Оценка клетки по картам влияния с учётом роли |
| `constraints.rs` | Жёсткие фильтры кандидатов (до скоринга) |
| `snapshot.rs` | BattleSnapshot — снимок боя для чистых вычислений |
| `role.rs` | AiRole — тактическая роль юнита (Bruiser/Archer/Mage/Support/Assassin) |
| `difficulty.rs` | DifficultyProfile — ручки сложности (шум, множители, awareness) |
| `influence.rs` | Карты влияния — пространственная оценка клеток |
| `debug.rs` | Debug overlay: ресурс `AiDebugState`, консольный лог, визуализация influence maps |

## Цикл принятия решения

```
1. Проверка AP (нет action + нет movement → EndTurn)
2. Построить BattleSnapshot + InfluenceMaps
3. BFS reachable_with_paths → достижимые клетки
4. Построить UtilityContext (caster, abilities, difficulty, crit_fail)
5. pick_action:
   a. ★ select_intent → выбор TacticalIntent (см. ниже)
   b. Генерация кандидатов (top-8 клеток по position_eval + текущая)
   c. Для каждой клетки × каждая способность → ActionCandidate
   d. Дедупликация, cap 25 кандидатов
   e. Жёсткие фильтры (constraints)
   f. ★ Intent-фильтр (удаляет кандидатов, не соответствующих интенту)
   g. 8-факторный utility scoring с весами по роли
   h. Шум из difficulty
   i. Выбор лучшего → AiDecision
6. Исполнение:
   - CastInPlace → UseAbility
   - MoveAndCast → MoveUnit + UseAbility
   - MoveCloser → MoveUnit + EndTurn
   - EndTurn → EndTurn
```

## Utility Scoring

Каждый `ActionCandidate` оценивается по 8 факторам, нормализованным в 0..1 внутри батча. Финальный скор = `dot(factors, role_weights) + noise`.

### Факторы

| Фактор | Источник | Нормализация |
|--------|---------|--------------|
| `damage` | `score_action()` (HP-эквиваленты) | `/ max в батче` |
| `kill` | 1.0 если expected >= target.hp | бинарный |
| `cc` | Ценность статусов (stun × threat × duration и т.д.) | `/ max в батче` |
| `heal` | Скор хила | `/ max в батче` |
| `position` | `evaluate_position()` × awareness | `/ max в батче` |
| `risk` | `-danger(tile) + armor` | `/ max в батче` |
| `focus` | `target_priority()` | уже 0..1 |
| `intent` | `intent_score()` — соответствие TacticalIntent (см. ниже) | `/ max в батче` |

### Весовые таблицы по ролям

| Фактор | Bruiser | Archer | Mage | Support | Assassin |
|--------|---------|--------|------|---------|----------|
| damage | 1.0 | 1.0 | 0.8 | 0.2 | 0.8 |
| kill | 1.5 | 1.0 | 0.8 | 0.3 | 2.0 |
| cc | 0.3 | 0.3 | 1.2 | 0.8 | 0.2 |
| heal | 0.0 | 0.0 | 0.0 | 2.0 | 0.0 |
| position | 0.5 | 1.0 | 0.8 | 1.0 | 0.3 |
| risk | 0.3 | 0.8 | 0.6 | 1.0 | 0.2 |
| focus | 0.8 | 0.5 | 0.5 | 0.5 | 1.5 |
| intent | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 |

### TacticalIntent (intent.rs)

AI выбирает один стратегический интент перед генерацией кандидатов. Интент задаёт бонусы в scoring и фильтрует кандидатов.

#### Выбор интента (приоритет, первое совпадение побеждает)

| # | Условие | Intent |
|---|---------|--------|
| 1 | HP < 25% И danger(pos) > hp | **ProtectSelf** |
| 2 | Союзник < 30% HP И CAN_HEAL | **ProtectAlly { ally }** (самый раненый) |
| 3 | Враг убиваем (threat × awareness ≥ hp) | **FocusTarget { target }** (минимум HP) |
| 4 | CAN_CC И есть не-оглушённый враг | **ApplyCC { target }** (макс. threat) |
| 5 | HAS_AOE И враги кластерируются (пара на расст. ≤ 2) | **SetupAOE** |
| 6 | position_eval(текущая) < −1.0 | **Reposition** |
| 7 | По умолчанию | **FocusTarget** (макс. target_priority) |

#### Intent-фильтры (после hard constraints)

| Intent | Фильтр | Fallback |
|--------|--------|----------|
| FocusTarget | Убрать кандидатов на других врагов (хил проходит) | Если всё удалено — пропустить фильтр |
| ApplyCC | CC-способности только на цель интента; урон без ограничений | — |
| ProtectSelf | Убрать клетки с danger > hp × 0.5 | — |
| ProtectAlly | Хил только на целевого союзника; урон без ограничений | — |
| SetupAOE | Убрать single-target способности (если есть AoE-кандидат) | — |
| Reposition | Без фильтра | — |

#### Intent-скоринг (фактор `intent`)

| Intent | Score |
|--------|-------|
| FocusTarget | 1.0 если target совпадает, иначе 0.0 |
| ApplyCC | 1.0 если CC на target; 0.5 если damage на target; иначе 0.0 |
| Reposition | evaluate_position(tile) (выше = лучше) |
| ProtectSelf | (−danger + armor) (безопаснее = выше) |
| ProtectAlly | 1.0 если хил союзника; 0.5 если клетка рядом; иначе 0.0 |
| SetupAOE | enemies_hit / total_enemies |

## Базовый скоринг (scoring.rs)

Каждая пара (ability, target) оценивается в **HP-эквивалентах** — используется как фактор `damage`/`heal` в utility scoring.

### Damage
```
expected = dice.expected() - armor × armor_awareness + damage_taken_bonus
score = max(1.0, expected)
```
Kill bonus: `× kill_multiplier` если expected >= target.hp.

### Heal
```
effective = min(expected, missing_hp)
score = effective × urgency_multiplier (если target.hp% < threshold)
```
Хил полного HP = 0 (не тратить на здоровых).

### Status Effects
```
skips_turn → +threat × duration (стан высокоугрозной цели ценнее)
damage_taken_bonus → +bonus × duration
armor_bonus → +bonus × duration
```
Итог `× status_value_scale`.

### AoE
Сумма score_action по всем целям в зоне. Friendly fire вычитает score союзников.

### Critical Failure Adjustment
- Miss: `score × (1 - crit_chance)`
- ManaOverload: `score - crit_chance × mana_cost`
- CircuitBreach: `score × (1 - crit_chance) - crit_chance × mana_cost × 0.5`

## Target Priority (target_priority.rs)

Оценка важности цели (0..1), используется как фактор `focus`.

| Фактор | Вес | Формула |
|--------|-----|---------|
| Threat | 0.30 | `target.threat / max_threat` |
| Killability | 0.25 | `1 - hp / max_hp` |
| Vulnerability | 0.15 | `+0.3` если LOW_HP, `+0.2` если damage_taken_bonus > 0 |
| Role value | 0.15 | Support=1.0, Mage=0.8, Assassin=0.6, Archer=0.5, Bruiser=0.3 |
| Proximity | 0.15 | `1 / (1 + distance)` |

## Position Evaluation (position_eval.rs)

Оценка клетки — линейная комбинация 4 карт влияния с весами по роли.

| Карта | Bruiser | Archer | Mage | Support | Assassin |
|-------|---------|--------|------|---------|----------|
| danger | −0.3 | −1.0 | −0.8 | −1.2 | −0.2 |
| ally_support | 0.5 | 0.3 | 0.5 | 1.0 | 0.1 |
| opportunity | 1.0 | 0.5 | 0.8 | 0.2 | 1.2 |
| escape | 0.0 | 0.8 | 0.5 | 1.0 | 0.0 |

Результат умножается на `difficulty.awareness`.

## Hard Constraints (constraints.rs)

Фильтрация кандидатов **до** скоринга. Удаляет заведомо плохие варианты:

1. **Forced targeting** — если есть цель с `FORCES_TARGETING` (taunt), убрать кандидатов на другие цели
2. **Don't walk into death** — отклонить клетку, если `LOW_HP` и `danger(tile) > hp`
3. **Don't AoE allies** — отклонить AoE с `friendly_fire`, если союзников в зоне больше чем `enemies / 2`
4. **Don't waste CC** — не тратить стан на уже оглушённую цель
5. **Don't overheal** — не лечить цель выше 90% HP

После hard constraints применяются **intent-фильтры** (см. TacticalIntent выше). Если intent-фильтр удалил бы все кандидаты — он молча пропускается (fallback).

## Roles

Роль определяется автоматически из ability-set (`infer_role`) или задаётся в TOML (`ai_role`).

| Role | Условие | Поведение |
|------|---------|-----------|
| Support | Есть heal-способность на союзника | Лечит, держится у группы |
| Mage | AoE или spell_damage | Ищет кластеры целей |
| Archer | Физический ranged (min_range >= 2) | Держит дистанцию |
| Assassin | Melee + speed >= 5 | Фокусит уязвимых |
| Bruiser | Всё остальное | Идёт в ближний бой |

## Difficulty

`DifficultyProfile` — непрерывные ручки, легко интерполировать для адаптивной сложности.

| Параметр | Easy | Normal | Hard | Описание |
|----------|------|--------|------|----------|
| `kill_multiplier` | 1.0 | 1.5 | 2.0 | Бонус за добивание |
| `armor_awareness` | 0.3 | 0.7 | 1.0 | Учёт брони цели (0=игнорирует) |
| `status_value_scale` | 0.3 | 0.7 | 1.0 | Ценность статус-эффектов |
| `heal_urgency_threshold` | 15% | 30% | 40% | Порог «срочного» хила |
| `heal_urgency_multiplier` | 1.2 | 1.5 | 1.8 | Множитель срочного хила |
| `noise` | 3.0 | 1.0 | 0.0 | Случайный шум в скоринге |
| `awareness` | 0.3 | 0.7 | 1.0 | Использование карт влияния (0=игнорирует позицию) |

## Snapshot

`BattleSnapshot` — чистый снимок без Bevy-зависимостей (кроме Entity). Содержит `Vec<UnitSnapshot>` со всеми данными для AI-решений.

### UnitSnapshot
Позиция, HP, ресурсы (mana/rage/energy), скорость, список способностей, статусы, threat-оценка, `AiTags`.

### AiTags (bitflags)
```
LOW_HP | CAN_HEAL | CAN_CC | HAS_AOE | IS_STUNNED | FORCES_TARGETING | RANGED | MELEE_ONLY
```
Вычисляются из текущего состояния юнита при построении снимка.

## Influence Maps

Пространственная оценка клеток грида. Каждая карта — `HashMap<Hex, f32>`, инициализированная для всех клеток поля (размер выводится из `GRID_ROWS`/`row_cols`).

```rust
build_influence_maps(snap, active_team, db) → InfluenceMaps {
    danger, ally_support, opportunity, escape
}
```

### Danger Map
Проекция угрозы от вражеских юнитов.

Для каждого врага:
1. BFS по speed → множество достижимых клеток
2. Каждую достижимую клетку расширить на max attack range (`hex_circle`)
3. `danger[cell] += enemy.threat`

Проходимость: враг проходит через своих союзников, заблокирован нашими юнитами.

### Ally Support Map
Близость к союзникам.

- Базовый: `1.0 / (1 + distance)` от каждого союзника
- Хилер (`CAN_HEAL`): дополнительно `+2.0 / (1 + distance)`
- Melee-союзник (`MELEE_ONLY`): `+0.5` при distance <= 1

### Opportunity Map
Привлекательность позиции для атаки.

- Раненые цели: `(1 - hp%) / (1 + distance)` — ближе к добиванию = ценнее
- Угрожающие цели: `threat × 0.1 / (1 + distance)` — стоит приближаться к опасным

### Escape Map
Безопасность позиции.

- `-danger[cell]` — инверсия карты опасности
- `+1.5 / (1 + distance)` к хилерам — безопаснее рядом с поддержкой

## Candidate Generation (utility.rs)

Пайплайн генерации кандидатов:

1. Оценить все достижимые клетки через `evaluate_position()` → взять **top-8** + текущая позиция
2. Для каждой клетки × каждая доступная способность → найти допустимые цели
3. Дедупликация: одинаковые `(ability, target)` с разных клеток → оставить кратчайший путь
4. Cap: максимум 25 кандидатов

```rust
struct ActionCandidate {
    tile: Hex,          // куда встать
    path: Vec<Hex>,     // путь до tile
    ability: AbilityId, // чем бить
    target_pos: Hex,    // куда целиться
    target: Entity,     // в кого
}
```

Если кандидатов нет — fallback: двигаться к ближайшему врагу.

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
═══ AI DEBUG: Зверокров Страж (Bruiser) ═══
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
| Actor | Роль, HP/max, threat, позиция (offset), AiTags (MELEE/RANGED/CAN_HEAL/...), action/movement |
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
