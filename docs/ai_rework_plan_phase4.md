# AI Rework — Phase 4: Intent as Weight Vector

Реализует §4 из [`docs/ai_rework_plan.md`](ai_rework_plan.md). Лечит **S5** (low-value armor hit получает полный intent-кредит наравне с реальным damage).

Связанные документы: [`docs/ai_rework.md`](ai_rework.md) §3.2, [`docs/ai.md`](ai.md).

---

## Симптом S5

```
r4 Буревестник: Move×2 → melee → dmg=1 по armored-цели, полный intent-bonus
```

Старый `intent_score` для `FocusTarget` возвращал `1.0` при любом прямом попадании в цель — независимо от фактического урона. Бронированная цель с `armor=9` получала dmg=1, но intent-фактор плана оставался максимальным. Любой другой план (обход, ranged, CC) не мог выиграть по intent, даже если был объективно лучше.

---

## Решение

### `IntentWeights` — dot-product вес-вектор

```rust
pub struct IntentWeights {
    pub damage: f32,
    pub kill_now: f32,
    pub kill_promised: f32,
    pub cc: f32,
}

impl IntentWeights {
    pub fn dot(&self, f: &PlanFactors) -> f32 { ... }
}
```

Только offensive-оси (damage, kill_now, kill_promised, cc). Остальные оси (position, risk, tempo_gain, etc.) на уровне intent пока не нужны — они уже учтены в `AXIS_FACTOR_WEIGHTS` на уровне роли.

### Таблица контрактов

| Intent | kill_now | kill_promised | damage | cc | geometry |
|--------|----------|---------------|--------|----|----------|
| `FocusTarget` | 2.0 | 0.3 | 1.0 | 0.5 | `pursuit_move_score` |
| `ApplyCC` | — | — | 0.3 | 1.5 | `pursuit_move_score(cc_reach)` |
| `Reposition` | — | — | — | — | position-improvement tiered |
| `ProtectSelf` | — | — | — | — | 1.0 или `1 − danger` (Phase 5) |
| `ProtectAlly` | — | — | — | — | target-type + proximity |
| `SetupAOE` | — | — | — | — | `hit/total` ratio |
| `LastStand` | — | — | — | — | offensive type flags |

### `filter_offensive_for_target`

Для `FocusTarget` и `ApplyCC` — offensive-оси считаются только по target'у интента:

```
Cast → focus_entity        : full credit (факторы не трогаем)
Cast → AoE, покрывает focus: offensive × 0.6
Cast → другая цель / miss  : offensive = 0
Move                       : offensive = 0 (geometry hook)
```

### Новая сигнатура `intent_score`

```rust
// Было:
pub fn intent_score(intent, step, active, snap, maps, content, difficulty) -> f32

// Стало:
pub fn intent_score(intent, step, step_ctx: &ScoringCtx) -> f32
```

Внутри вызывается `compute_factors(step_ctx, step)` для получения per-step impact вектора.

Вызов в `scorer::compute_plan_intent_sum`:
```rust
let step_ctx = ctx.with_perspective(&sim_actor, pre_snap);
let iv = intent_score(intent, &scored_step, &step_ctx);
```

---

## Что изменилось в поведении

**FocusTarget:**
- Раньше: `dmg=1 → intent=1.0`, `dmg=10 → intent=1.0` (одинаково)
- Теперь: `dmg=1 → intent≈1.0`, `dmg=10 → intent≈10.0` (после нормализации — пропорционально)

**ApplyCC:**
- Раньше: `cc hit → 1.0`, `damage без cc → 0.5`
- Теперь: `cc hit → cc_factor × 1.5`, `damage без cc → damage × 0.3`

**Reposition, ProtectSelf, ProtectAlly, SetupAOE, LastStand:** поведение не изменилось — формулы перенесены 1:1.

---

## Что **не** изменилось

- `SCHEMA_VERSION` — не бампится (новых факторов нет)
- `TacticalIntent` варианты — не менялись
- `ProtectSelf` контракт — заморожен до Phase 5 (`self_survival` axis)
- `tempo_gain` в dot-product — всегда 0 на per-step уровне (заполняется только на plan-level в `compute_plan_tempo_gain`); Reposition работает через geometry hook

---

## Тесты

Новые тесты в `intent.rs`:

| Тест | Что проверяет |
|------|--------------|
| `focus_target_scores_proportional_to_damage` | 10 dmg > 1 dmg при FocusTarget на ту же цель |
| `focus_target_wrong_target_scores_near_zero` | ST-каст в non-focus entity → intent ≤ 0 |

Обновлённые существующие тесты:

| Тест | Изменение |
|------|-----------|
| `reposition_penalizes_worse_tile` | Переведён на `make_scoring_ctx` |
| `focus_target_pursuit_enters_bubble_above_viability` | Переведён на `make_scoring_ctx` |

---

## Следующие шаги

**Phase 5** (`self_survival` + ProtectSelf contract):
- Добавить ось `self_survival` в `PlanFactors` (`NUM_FACTORS = 13`)
- Реализовать `factors/survival.rs`
- Заменить ProtectSelf formula на `self_survival ≥ ε` threshold
- Добавить `self_survival` в `IntentWeights` для `ProtectSelf` (weights: `self_survival=2.0, heal=1.0, damage=0.2`)

**Phase 6** (cleanup):
- Удалить оси `position`, `focus`, `risk` после стабилизации
- `tempo_gain` в `IntentWeights` начнёт работать, если перевести `Reposition` на plan-level dot-product
