# AI scenario regression tests

Поведенческая регрессия AI. **Одна папка = один лог = N кейсов:**

```
snapshots/<group>/
  log.jsonl                      # лог-срез из реального боя
  p000_basic_melee.expected.toml # кейс 1: ожидания по entry plan_id=0
  p010_finisher_1hp.expected.toml # кейс 2: по entry plan_id=10
  p013_last_stand_trade.expected.toml
```

Имя кейса **префикс `p<plan_id>_`** → сразу видно, какой entry тестируется.
Каждый overlay — независимый тест с `[scope] plan_id = N`.

`cargo test --test ai_scenarios` прогоняет production-пайплайн
(`finalize_scores` → `sanity_adjust_plans` → `apply_protect_self_mask`
→ `pick_best_plan`) на каждом кейсе и сверяет с overlay. Весь
batch — один процесс, один load контента.

## Зачем

Дешёвая защита от регрессий: при переработке весов/сigmoid-коэффициентов
harness ловит поведенческие изменения на реальных ситуациях (а не
синтетических мокапах). Ограничение — сверяем только **наблюдаемое
решение**, не внутренние скоры: overlay'ы не ломаются при безобидной
доводке.

## Добавить сценарий

### Новый лог (первый кейс в группе)

1. Найти в `logs/` ситуацию (из mining-таблицы
   `docs/ai_need_signals.md` или из недавнего бага).
2. Создать папку `snapshots/<group>/`, скопировать лог в `log.jsonl`.
   Имя группы — короткое описание playtest'а (например,
   `bell_crypt_r2`, `twisted_grove_focus_fire`).
3. Добавить первый overlay (см. ниже).

### Новый кейс в существующей группе

Просто добавить новый `<case>.expected.toml` рядом с `log.jsonl`.

### Формат overlay

```toml
[scope]
plan_id = 0                         # обязательно для multi-entry log'а

[[expectations]]
decision_kind = ["MoveAndCast"]     # any-of
cast_ability = ["melee_attack"]
intent_kind = ["FocusTarget"]
primary_effect = ["Damage"]
```

Несколько блоков `[[expectations]]` = OR: достаточно совпадения одного
варианта. Поля:
- `decision_kind` — `CastInPlace | MoveAndCast | Move | EndTurn`
- `cast_ability` — имя из `assets/data/abilities.toml`
- `cast_target` — `Entity::to_bits()` из снапшота
- `end_position` — `[x, y]`
- `intent_kind` — имя варианта `TacticalIntent`
- `primary_effect` — `Damage | Heal | GrantMovement | RestoreResources | Summon | None`
- `not_target` / `not_end_position` — exclusion lists.

Запустить: `cargo test --test ai_scenarios`.

## Правила

- **Только наблюдаемое поведение.** Не ассертим внутренние скоры, факторы,
  sanity hits — только то, что видит игрок и оппонент.
- **Реальные снапшоты, не синтетика.** Если нужной ситуации в `logs/`
  нет — сыграй плейтест, она появится. Синтетический JSONL = тест
  врёт про вероятность.
- **Каждый bug → сценарий до фикса.** Сначала overlay, потом правка кода.
- **Независимость.** Сценарии не разделяют состояние между собой.
- **Pre-v17 логи работают** через fallback на
  `DifficultyProfile::normal()` + пустой `Reservations`. Для сценариев
  Plan freeze / Team coordination нужны v17+ логи (см. step 1.1 плана).

## Категории (плана 1.5)

- **Offensive correctness** — гарантированный kill, финишер vs полная цель, дорогой AoE на одиночку.
- **Protect-self correctness** — срабатывание, контракт держится, LastStand.
- **Plan freeze / continuation** — монотонное движение, replan при смерти цели. *v17+*
- **Team coordination** — no overkill через reservations, focus fire, healer protection. *v17+*
