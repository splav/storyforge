# Known Issues — AI subsystem

Повторный аудит `src/combat/ai/` + `src/combat/effects_*.rs` (~10 300 строк).
Дата: 2026-04-19.

Разбит на 5 осей: архитектура, дублирование, сомнительные абстракции, прочие проблемы, странная логика. Ранее исправленные находки (1.1–1.4, 1.8–1.10, 2.1–2.5, 2.7, 3.1, 3.3, 3.5, 4.1–4.2, 4.5–4.6, 5.1, 5.5–5.7, 5.9) вырезаны — смотрите git-историю.

---

## 1. Архитектурные проблемы

### 1.1. `sanity_adjust_plans` смешивает penalties + bonus

`sanity.rs:147–156`, пункт 7 — мультипликативный **+10 % bonus** за «safer tile + useful cast». Остальные 6 проверок — штрафы. Если sanity — это «проверка на глупости», то bonus — логически принадлежит scoring-этажу, а не sanity-этажу. Границы размыты, и следующий «ещё один бонус» легко добавится сбоку, раздув sanity в ad-hoc mini-scorer.

### 1.2. `run_ai_turn` — всё ещё god-function

`enemy_turn.rs:82–215` — 133 строки, 14 параметров (уже с двумя `SystemParam`-группировками `AiEnv`/`AiMessages`). Делает снапшот, maps, ctx, memory, зовёт `pick_action`, декомпозирует `AiDecision` в сообщения. `SystemParam`-bundling обошёл лимит Bevy, но само засилье параметров осталось.

### 1.3. Drift sim ↔ real не закрыт

`docs/ai.md` сам признаёт:
- **rage-gain не моделируется в sim** (real даёт +1 rage attacker/defender на damage — планировщик про это не знает).
- **speed mid-plan не re-flow в pathing**: `UnitSnapshot.speed = base + status_bonus` сохраняет агрегат, но не базу. Статус, меняющий speed в step[k], не пересчитывает achievable tiles для step[k+1].

Оба дрифта описаны — не зафиксированы.

---

## 2. Дублирование

### 2.2. Проходы по статусам — 3 раза

`build_snapshot` делает два прохода по одному и тому же `StatusEffects`:
- `compute_tags` (`snapshot.rs:385–397`) — флаги IS_STUNNED / FORCES_TARGETING.
- `status_bonuses` (`snapshot.rs:411–428`) — speed/armor/damage_taken агрегаты.

Плюс `refresh_status_aggregates` (`snapshot.rs:113–121`) в sim mid-plan. Три прохода по одному списку с пересекающимися выборками полей.

---

## 3. Сомнительные абстракции

### 3.6. `plan_summon_bonus` — post-normalization additive hack

`scorer.rs:125, 141–179`. После `dot(weights, normalized_factors)` подмешивается `summon_bonus` в HP-эквиваленте. Неявный 10-й factor без места в `NUM_FACTORS`, без нормализации, без role-weights. Каждый следующий «особый бонус» будет так же bolted-on сбоку. Абстракции, которая принимает «factor who doesn't fit 9-factor tensor» — пока нет.

---

## 4. Другие архитектурные проблемы

### 4.1. `reservations` — global mutable state, mutation в одном pass со scoring

`pick_action` читает reservations внутри factor-adjustments (`adjustments.rs:22–40`), затем после commit'а пишет (`record_committed_reservations`). Работает только в single-threaded Bevy system; не годится для параллельного выполнения AI-тиков разных юнитов. При переходе на async/parallel AI каждый тик должен взять snapshot reservations при старте и закоммитить дельту в конце.

### 4.3. Hard thresholds в `select_intent` vs. `difficulty.rs`

Рядом в intent selection живут:
- `intent.rs:162` — hard-coded `hp_pct < 0.4` для ProtectSelf.
- `snapshot.rs:334` — hard-coded `hp_pct < 0.3` для `LOW_HP` tag.

Но survival/panic thresholds уже живут в `difficulty.survival_hp_threshold()` / `awareness_danger_threshold()`. Смешение difficulty-driven и магических констант в одном модуле — тяжёлый случай drift'а при балансе.

### 4.4. `default_focus_target` крутится через «plans → committed step targets»

`intent.rs:344–371`: множество «достижимых target'ов» выводится как
```rust
plans.iter().filter_map(|p| ScoredStep::from_plan_committed(p, actor_pos).target())
```

То есть «какие враги достижимы» выводится косвенно через планировщик — при условии, что он породил хоть один план на каждый живой target. Прямее: `enemies_of.filter(|e| reach_budget >= dist)`. Текущая форма скрывает зависимость от output'а beam-search'а внутри intent.rs.

---

## 5. Странная логика

### 5.2. `plan_is_defensive` — empty plan = defensive by default

`sanity.rs:292`: `let Some(first) = plan.steps.first() else { return true };`. Под ProtectSelf это означает, что «ничего не делать» **всегда** считается защитной опцией. Но если актор стоит в high-danger тайле, empty plan = самоубийство. Справедливо только для low-danger позиций.

### 5.3. `score_action` для `Heal` возвращает HP-equivalent через `target.threat`

`scoring.rs:42–43`: `delta_pct × target.threat`. Т.е. «хилнуть союзника» оценивается как «сколько его damage output мы спасли». Но `threat` — это max-ST-damage (см. `estimate_st_damage`), не per-round DPR. За 1 round юнит атакует 1–2 раза. Скейлинг «HP-equiv via threat» натянут; HP-equiv через «сколько рантов он ещё продержится» был бы корректнее.

### 5.4. `focus_sum` empty-plan spec-case

`scorer.rs:303–310`. «Для пустого плана подменяем focus_sum = max(target_priority по всем enemies)», чтобы «ничего не делать» не зарэнкалось с focus=0. Симптом: factor-aggregation плохо определена для «do nothing». Move-only планы (`Move` не вносит в focus_sum ничего) тоже получают focus=0 — но на них этот хак не распространяется. Асимметрия внутри одного and the same «aggregation не покрывает случай».

### 5.8. `infer_profile` Tank-floor всегда ≥ 0.3

`role.rs:190–191`: `p.tank += (eff_hp / 20.0).clamp(0.3, 2.0)` — **всегда** добавляется минимум 0.3 независимо от tank-абилок. 12 HP glass-cannon голый `eff_hp/20 = 0.6 → tank += 0.6`. Это искажает профиль: любой юнит обычных 15–20 HP уже получит ~1.0 tank-веса, которого нет в его kit-диагностике. Проявляется в тестах (`infer_molnienosets_is_melee_assassin`) — mix[0] «<0.25 tank for glass cannon» держится, но только благодаря bias^1.5.

---

## Приоритет фиксов

| Находка | Влияние | Риск фикса |
|---|---|---|
| 3.6 `plan_summon_bonus` post-norm hack | расширяемость | средний |
| 5.8 Tank-floor ≥ 0.3 в `infer_profile` | role mis-inference | низкий |
| 2.2 Status passes × 3 | perf + DRY | низкий |
| 4.1 `reservations` global mut | future concurrency | средний |
| 4.3 Hard thresholds в `select_intent` | balance drift | низкий |
| 4.4 `default_focus_target` через plans | layering | низкий |
| 5.2 `plan_is_defensive` пустой plan = defensive | corner-case | низкий |
| 5.3 `score_action` Heal via threat | scoring semantics | средний |
| 5.4 `focus_sum` empty-plan spec-case | scoring уродство | низкий |
| 1.1 sanity штрафы + bonus | разделение зон | низкий |
| 1.2 `run_ai_turn` god-function | читабельность | средний |
| 1.3 Drift sim ↔ real (rage, speed) | корректность sim | средний |
