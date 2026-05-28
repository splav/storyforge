# Trade Economy

*Источник: `src/combat/ai/scoring/trade.rs`, `src/combat/ai/modifiers/trade_bonus.rs`, `src/combat/ai/factors/step/scarcity.rs`.*

Plan-level signed modifier, параллельный `summon_bonus` и `repair_bonus`. Оценивает размен: ценность убитых врагов минус потерянные союзники минус стоимость собственной смерти, если план self-lethal. Применяется **после** composition-фазы, вне factor normalization, через `PlanModifiersStage`.

## `unit_value(u)`

HP-equivalent actor-agnostic ценность юнита:

```
unit_value(u) = lifetime_rounds(u) × (offense + heal + cc) + objective_bonus(u)
```

где `objective_bonus(u)` = `tuning.thresholds.objective_value_bonus` (default 80.0) если юнит несёт `AiTags::OPPONENT_OBJECTIVE`, иначе 0.

**Objective bonus** необходим для stunned/inert NPC, у которых offense=0, heal=0, cc=0 — без бонуса `unit_value` ≈ 0, и AI экономически безразличен к их гибели. Бонус 80.0 ставит stunned KeepAlive NPC выше стандартного melee hero (lifetime=2 × threat≈8 ≈ 16) в trade economy.

| Слагаемое | Формула | Источник |
|---|---|---|
| `offense_projection` | `horizon_avg(u)` | resource-aware DPR из `scoring.rs` |
| `heal_projection` | best legal `SingleAlly + Heal` EV | `u.caster_ctx` + `u.abilities` |
| `cc_projection` | `max { Σ duration × u.threat : skips_turn statuses on target }` | `u.abilities` + `content.statuses` |
| `lifetime_rounds(u)` | **константа 2.0** | см. «Known limitations» |

**Инварианты:**

1. **Actor-agnostic** — зависит только от `u` и статического контента; никакой proximity, никакого relative threat. self/ally/enemy оцениваются одинаково.
2. **HP-equivalent units** — всё в «HP в минуту», слагаемые можно складывать.
3. **Нет внутреннего floor** — floor `UNIT_VALUE_FLOOR = 1.0` применяется только в знаменателе `trade_score`, чтобы сумма по трешу не раздувалась.

## `trade_delta(plan)`

Анализирует исходы плана **только в пределах commit-prefix** (first fired step для solo, [0..=1] для Move→Cast bundle). Tail steps — lookahead, следующий тик перепланирует.

```
trade_delta = Σ unit_value(killed_enemy)
            − Σ unit_value(lost_ally)
            − (self_lethal ? unit_value(self) : 0)
```

| Поле | Как считается |
|---|---|
| `killed_value` | Σ по `plan.outcomes[k].killed` для `k < prefix_len`, victim на вражеской команде |
| `lost_value` | то же для цели на команде актора (self-AoE FF тоже тут) |
| `self_lethal` | `expected_aoo_damage(active, plan, enemies) ≥ active.hp` ИЛИ actor в killed list |
| `self_lost` | `unit_value(active)` если self_lethal И actor **не** в killed list; иначе 0 (guard против double-count) |

В валидном commit-prefix AoO-релевантный Move всегда шаг 0, поэтому сравнение с `active.hp` (plan-start HP) точное — никакой self-heal не может прогнать до движения.

## `trade_score`

```
trade_score = tanh(delta / max(unit_value(self), UNIT_VALUE_FLOOR)) × TRADE_WEIGHT
```

Добавляется к final score **после** нормализации и role-composition. Tanh-squash гарантирует `trade_score ∈ [−TRADE_WEIGHT, +TRADE_WEIGHT]` — сатурация при «явно выгодном» или «явно катастрофическом» размене. Делитель на `unit_value(self)` нормирует по масштабу актора: дешёвый громила и дорогой мастер видят одну и ту же «форму» размена, не абсолютные HP.

`TRADE_WEIGHT = 0.5` — conservative launch default; повышение — только после replay-свидетельств, что self-trade-for-support не пробивается.

## Log schema

`PlanLogEntry.trade`:

```json
{
  "delta": 12.0,
  "killed": 16.0,
  "lost": 4.0,
  "self_lost": 0.0,
  "self_lethal": false,
  "score": 0.38
}
```

`score` в блоке — ровно тот increment, который `modifiers/trade_bonus.rs` добавил к top-level `score`. Для plan'ов без размена (`delta == 0 && !self_lethal`) блок — null-ish (все нули); `replay_ai_log --verbose` в таких случаях не печатает trade-строку.

## Known limitations

- **lifetime_rounds — константа.** Phase 2c должна заменить на `clamp(eff_hp / incoming_dpr, 0.5, 3.0)` с actor-agnostic прокси для `incoming_dpr`. Сейчас танки получают ценность живучести только косвенно — через то, что их kit нечасто доходит до `offense + heal + cc`.
- **Taunt / forces_targeting redirect не оценивается.** Pure tanks скорятся у нижнего floor — consistent с существующей `role_value` иерархией (Tank 0.3). Если replay покажет «AI радостно меняет танка на крысу», это триггер для `redirect_value`.
- **Multi-cast scaling отсутствует в heal / cc.** Осознанно: resource limits, overheal, non-stacking stuns делают multi-cast projection оптимистичной. best-single-legal — консервативный underestimate.

## Resource Scarcity

Реализация — `factors/step/scarcity.rs` (signed factor, не модификатор):

```
scarcity = (swing_value - resource_ratio).clamp(-1.0, 1.0)
```

`resource_ratio = max(cost / current_pool)` по всем ресурсам.

**swing_value:**

| Условие | Бонус |
|---|---|
| Kill (kill_now > 0) | +0.8 |
| Kill role-value | +0.35 × `target.role.role_value()` |
| AoE hits > 1 | +0.2 × (hits − 1) |
| CC на high-threat unstunned | +0.5 × (threat / 10) |
| Цель < 25% HP и есть free-attack | −0.3 |
| Round ≤ 1 | −0.15 |
