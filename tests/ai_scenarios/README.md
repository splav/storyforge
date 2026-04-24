# AI scenario regression tests

Поведенческая регрессия AI. Каждый сценарий — пара файлов:

```
snapshots/<name>.jsonl                 # лог-срез из реального боя
snapshots/<name>.jsonl.expected.toml   # ожидания по решению
```

`cargo test --test ai_scenarios` прогоняет production-пайплайн
(`finalize_scores` → `sanity_adjust_plans` → `apply_protect_self_mask`
→ `pick_best_plan`) на каждом снапшоте и сверяет с overlay. Весь
batch — один процесс, один load контента.

## Зачем

Дешёвая защита от регрессий: при переработке весов/сigmoid-коэффициентов
harness ловит поведенческие изменения на реальных ситуациях (а не
синтетических мокапах). Ограничение — сверяем только **наблюдаемое
решение**, не внутренние скоры: overlay'ы не ломаются при безобидной
доводке.

## Добавить сценарий

1. Найти в `logs/` ситуацию, которая описывает нужную проблему
   (из mining-таблицы `docs/ai_need_signals.md` или из недавнего бага).
2. Скопировать JSONL целиком в `snapshots/<name>.jsonl`. Файл может
   содержать несколько entries — overlay выберет нужную через
   `[scope] plan_id`.
3. Написать `snapshots/<name>.jsonl.expected.toml`:

   ```toml
   [scope]
   plan_id = 0                         # опционально; default = первая запись

   [[expectations]]
   decision_kind = ["MoveAndCast"]     # any-of
   cast_ability = ["melee_attack"]
   intent_kind = ["FocusTarget"]
   primary_effect = ["Damage"]
   ```

   Несколько блоков `[[expectations]]` = OR: достаточно совпадения
   одного варианта. Поля:
   - `decision_kind` — `CastInPlace | MoveAndCast | Move | EndTurn`
   - `cast_ability` — имя из `assets/data/abilities.toml`
   - `cast_target` — `Entity::to_bits()` из снапшота
   - `end_position` — `[x, y]`
   - `intent_kind` — имя варианта `TacticalIntent`
   - `primary_effect` — `Damage | Heal | GrantMovement | RestoreResources | Summon | None`
   - `not_target` / `not_end_position` — exclusion lists.

4. `cargo test --test ai_scenarios`.

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
