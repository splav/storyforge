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

Каждая способность имеет множитель `power` (по умолчанию `1.0`). Он масштабирует
**только** «оружейную» часть урона — кубик у физических атак и `spell_power` у
магических; модификатор характеристики (`STR`/`INT`) добавляется всегда в полном
объёме. Так ослабляющие/усиливающие способности (`power < 1` / `power > 1`)
бьют по масштабируемой части, а не по фиксированному бонусу стата.

### Physical (weapon_attack)
```
raw    = round(dice_roll × power) + mod          # mod = STR (melee) либо DEX (ranged)
actual = max(1, raw - armor - armor_bonus)
```
`damage` (немагический фикс-кубик без оружия) всегда использует `power = 1.0` и STR_mod.

### Magical (spell_damage)
```
raw    = dice_roll + INT_mod + round(power × spell_power)
actual = max(1, raw - magic_resist)
```
Магический урон митигируется `magic_resist` (см. `RuntimeStatsDelta`), **не**
бронёй — поэтому физическая броня против заклинаний бесполезна, и наоборот.

**Минимальный урон = 1.** Любая атака наносит не менее 1 урона независимо от защиты. Это значит, что даже полностью заблокированная атака «царапает». Множество слабых атак по защищённой цели суммарно пробивают её.

## Healing

```
amount = dice_roll + INT_mod + round(power × spell_power)
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
- Restore: +1 at start of owner's turn (engine `start_actor_turn`)
- Spend: deducted on ability use (engine `Effect::PayCost`)
- Используется магическими способностями (маг)

### Rage
- Start: `current = 0`
- Gain: +1 for dealing damage, +1 for receiving damage (engine `Effect::GainRage`)
- Spend: deducted on ability use (engine `Effect::PayCost`)
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
- `BonusMovement(i32)` — additive bonus from GrantMovement abilities (e.g. Rush); суммируется со Speed, удаляется после первого move
- Passability: can walk through allies, blocked by enemies
- Landing: must end on empty cell (no stacking)

## Attacks of Opportunity (AoO)

Юнит, покидающий соседство с живым врагом, провоцирует у того одну атаку оружием.

- **Триггер:** на каждом шаге пути, если враг был соседом предыдущего гекса и не является соседом нового. Срабатывает в момент выхода — если AoO убивает, оставшаяся часть пути отменяется, юнит-жертва остаётся на текущем шаге.
- **Реакция:** одна `weapon_attack` базового оружия провоцирующего (dice + STR_mod, обычное армор-митигирование). Не тратит AP провоцирующего; тратит 1 заряд из `Reactions { remaining, max = 1 }`.
- **Не провоцируют AoO:** телепорт, толчки (когда появятся). Обычное движение и Rush (bonus movement) — провоцируют.
- **Не наносят AoO:** мёртвые, оглушённые (`skips_turn`), без мили-оружия (нет ability с `WeaponAttack` + `range.max == 1`), уже потратившие реакцию в этом раунде.
- **Сброс реакций:** `build_turn_order` при старте раунда восстанавливает `remaining = max` у всех живых.

## Statuses

| Field | Effect |
|-------|--------|
| `armor_bonus` | Reduces physical damage (stacks with base `armor`) |
| `magic_resist_bonus` | Reduces magical damage (stacks with base `magic_resist`) |
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
При наложении DoT-статуса кубик `dot_dice` бросается **один раз** и в `dot_per_tick`
запекается фиксированный урон за тик по той же магической раскладке, что и
`spell_damage`:
```
dot_per_tick = roll(dot_dice) + INT_mod + round(power × spell_power)
```
где `power` — множитель накладывающей способности. Пример: «Ожог» (`power = 0.5`,
`burning` = `1d2`) у мага с INT_mod=0, spell_power=4 при броске 1 → `1 + 0 + round(0.5×4) = 3` урона за тик. Тик фиксирован — последующие изменения статов кастера на него не влияют.

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
