# Game Mechanics

## Stats

6 D&D-style stats, range -5..10. Used via `modifier(stat) = stat >> 1` (floor divide by 2).

| Stat | Modifier Range | Used For |
|------|---------------|----------|
| Strength | -3..+5 | Melee/physical damage bonus |
| Dexterity | -3..+5 | Initiative (d20 + DEX mod) |
| Constitution | -3..+5 | (reserved) |
| Intelligence | -3..+5 | Spell damage + healing bonus |
| Wisdom | -3..+5 | (reserved) |
| Charisma | -3..+5 | (reserved) |

## Damage

### Physical (weapon_attack, damage)
```
raw = dice_roll + STR_mod
actual = max(1, raw - armor - armor_bonus + damage_taken_bonus)
```

### Magical (spell_damage)
```
raw = dice_roll + INT_mod + spell_power
actual = max(1, raw + damage_taken_bonus)
```
Pierces armor: ignores `armor` and `armor_bonus`.

**Минимальный урон = 1.** Любая атака наносит не менее 1 урона независимо от брони. Это значит, что даже полностью заблокированная атака «царапает». Множество слабых атак по бронированной цели суммарно пробивают защиту.

## Healing

```
amount = dice_roll + INT_mod + spell_power
```

Исцеление сначала нейтрализует яды на цели (см. [Яды и исцеление](#яды-и-исцеление)), затем оставшееся количество восстанавливает HP (не выше max_hp).

## Resources

### Unified Costs

Стоимость способности задаётся списком `costs`, где каждый элемент — пара `{ resource, amount }`. Одна способность может тратить несколько ресурсов (например, HP + мана). Ресурсы:

| Resource | ID | Описание |
|----------|----|----------|
| HP | `hp` | Здоровье кастующего |
| Мана | `mana` | Магическая энергия |
| Ярость | `rage` | Боевая ярость |
| Энергия | `energy` | Немагическая энергия |

При использовании способности проверяется наличие всех ресурсов из списка. Если хотя бы одного не хватает — способность недоступна.

### Mana
- Start: `current = max` (full)
- Restore: +1 at start of owner's turn (`turn_start_system`)
- Spend: deducted on ability use (`resolve_action_system`)
- Используется магическими способностями (маг)

### Rage
- Start: `current = 0`
- Gain: +1 for dealing damage, +1 for receiving damage (`apply_effects_system`)
- Spend: deducted on ability use
- Используется боевыми приёмами (воин)

### Energy
- Start: `current = max` (full)
- Restore: +1 at start of owner's turn (`turn_start_system`)
- Spend: deducted on ability use
- Используется немагическими способностями (следопыт)

### Rest (`rest`)
Self-target способность (клавиша `R`, effect `restore_resources`). Восстанавливает за один ход:
- +1 HP (не выше `max_hp`)
- +1 мана / ярость / энергия (если соответствующий компонент есть у актёра; клампится на max)

Доступна всем; не требует ресурсов, завершает ход.

### Action Points
- `action: bool` — can use an ability this turn
- `movement: bool` — can move this turn
- Both reset to `true` at turn start (`advance_turn_system`)
- Movement and action are independent (can do both per turn)

## Speed & Movement
- `Speed(i32)` — max hex cells per movement action
- `BonusMovement(i32)` — temporary override from GrantMovement abilities (e.g. Rush)
- Passability: can walk through allies, blocked by enemies
- Landing: must end on empty cell (no stacking)

## Statuses

| Field | Effect |
|-------|--------|
| `armor_bonus` | Reduces physical damage (stacks with armor) |
| `damage_taken_bonus` | Increases all incoming damage |
| `skips_turn` | Unit cannot act or move |
| `forces_targeting` | Enemies must target this unit (taunt) |
| `dot_dice` | Периодический урон (яд); см. ниже |
| `blocks_mana_abilities` | Блокирует использование способностей с mana-костом |
| `speed_bonus` | Модифицирует скорость передвижения |
| `hp_percent_dot` | % от max_hp как урон за каждый тик |
| `ai_controlled` | Герой действует под управлением AI (pact) |
| `causes_disadvantage` | Все броски кубика носителя — с disadvantage (бросок дважды, худший). См. `disoriented`. |

### Duration
- Ticks on **applier's** EndTurn, not target's
- New statuses applied AFTER existing ones are ticked (no +1 compensation needed)
- Duration 1 = active until end of applier's next turn

## Яды (DoT)

Статус с полем `dot_dice` наносит периодический урон (damage over time).

### Наложение
При наложении ядовитого статуса кубик `dot_dice` бросается **один раз**. Результат записывается в `dot_per_tick` — это фиксированный урон за каждый тик. Например, яд с `1d4` при броске 3 будет наносить 3 урона каждый ход.

### Тикание
Урон от яда наносится при тике статуса (в конце хода **наложившего** эффект), **перед** декрементом `rounds_remaining`. DoT-урон игнорирует броню.

### Пример
Отравленный выстрел: 1d4 DoT на 3 хода. Бросок = 3.
```
Тик 1: -3 HP, rounds_remaining: 3→2
Тик 2: -3 HP, rounds_remaining: 2→1
Тик 3: -3 HP, rounds_remaining: 1→0, яд снят
Итого: 9 урона
```

### Яды и исцеление

Исцеление нейтрализует яды **перед** восстановлением HP:

1. Для каждого DoT-статуса на цели: heal уменьшает `dot_per_tick`.
2. Если heal >= `dot_per_tick` — яд полностью снят, heal уменьшается на величину `dot_per_tick`. Переходим к следующему яду.
3. Если heal < `dot_per_tick` — яд ослаблен (`dot_per_tick -= heal`), heal = 0.
4. Оставшийся heal после нейтрализации всех ядов лечит HP обычным образом.

**Пример — частичное снятие:**
`dot_per_tick = 3`, heal = 2 → `dot_per_tick = 1`, яд продолжает тикать по 1/ход. HP не восстановлены.

**Пример — полное снятие:**
`dot_per_tick = 3`, heal = 5 → яд снят, 2 HP восстановлены.

## Critical Failures

При использовании способности бросается d20. При 1 — критический провал. Эффект зависит от `path` комбатанта (определяется в `races.toml`):

| Path | Эффект | Описание |
|------|--------|----------|
| `faith` | BrokenFaith | Промах + статус `broken_faith` на 1 ход (блокирует мана-способности) |
| `will` | ManaOverload | Способность срабатывает, но стоимость маны ×2. Дефицит маны наносит урон HP |
| `tech` | CircuitBreach | Промах + урон себе = (мана-кост + 1) / 2, игнорирует броню |
| `heritage` | Exhaustion | Промах + статус `exhaustion` на 2 хода (замедление + 5% max_hp DoT) |
| `pact` | PactControl | Промах + статус `pact_control` на 1 ход (AI управляет героем) |
| (нет path) | Miss | Просто промах |

Ресурсы тратятся всегда, даже при крит-провале. AI учитывает 5% вероятность провала при скоринге способностей.

## Enemy AI

See [AI](ai.md) — roles, scoring, difficulty, snapshot, influence maps.

## Initiative
- Round 1: `d20 + DEX_modifier` per combatant
- Subsequent rounds: reuse same order
- Higher total acts first
