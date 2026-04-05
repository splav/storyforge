# storyforge

Пошаговая RPG на Rust + Bevy 0.18. Демо-встреча: **Aldric** против **Goblin Guard** и **Goblin Ravager**.

## Запуск

```bash
cargo run    # открывает окно
cargo test   # запускает интеграционные тесты боевого pipeline
```

Лог боя также дублируется в stdout терминала.

## Управление (в бою)

| Клавиша | Действие |
|---------|----------|
| `1`–`5` | Выбрать способность (первая выбрана по умолчанию) |
| `Tab`   | Переключить цель среди живых врагов |
| `Enter` | Подтвердить действие |

Ходы врагов выполняются автоматически (случайная цель, случайная способность).

## Бойцы демо

| Имя | HP | Броня | Урон | Инициатива | Оружие |
|-----|----|-------|------|------------|--------|
| Aldric (воин) | 20 | 3 | +4 | 6 | Длинный меч (1d8) |
| Goblin Guard  | 14 | 5 | +0 | 10 | Длинный меч (1d8) |
| Goblin Ravager | 8 | 1 | +4 | 3 | Короткий меч (1d6) |

Боевая инициатива = базовая + d20. Способности воина: **Атака мечом** и **Блок щитом** (+4 брони на 1 раунд).

## Архитектура

```
AppState: Boot → Combat
                  └── CombatPhase (SubState):
                        StartRound → AwaitCommand → Victory / Defeat
```

### Боевой pipeline (один ход, всё в одном кадре)

```
skip_dead_turn    пропускает ход мёртвого актора
player_command    читает клавиатуру, пишет UseAbility
enemy_ai          автоход врага (случайный выбор)
validate_action   проверяет: живой ли, его ли ход, есть ли AP
resolve_action    кубики → ApplyDamage / ApplyStatus / EndTurn
cleanup           урон, статусы, Dead-компонент, победа/поражение, передача хода
```

### Модули

| Путь | Содержимое |
|------|-----------|
| `core/` | `AbilityId`, `StatusId`, `WeaponId`, `DiceRng` (LCG), `DiceExpr` |
| `content/` | `AbilityDef`, `StatusDef`, `WeaponDef`, `ClassDef` и дефолтный контент |
| `game/components.rs` | ECS-компоненты: `Combatant`, `CombatStats`, `Vital`, `EquippedWeapon`, `Dead`, … |
| `game/resources.rs` | `CombatContext`, `TurnQueue`, `CombatLog`, `GameDb`, `SelectionState` |
| `game/messages.rs` | `UseAbility`, `ValidatedAction`, `ApplyDamage`, `ApplyStatus`, `EndTurn` |
| `combat/` | Системы боя (по одной на файл) |
| `ui/` | HUD: панель ходов, список бойцов, панель способностей, лог |

### Добавление контента

**Новая способность** — `content/abilities.rs`: константа `AbilityId`, запись в `default_abilities()`.  
**Новое оружие** — `content/weapons.rs`: константа `WeaponId`, запись в `default_weapons()`.  
**Новый статус** — `content/statuses.rs` аналогично.

Боевые системы читают контент только через `GameDb` — ядро боя не меняется.
