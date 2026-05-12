# AI Log Mining

*Источник: `src/bin/mine_ai_logs.rs` (≈3100 строк). Текущая schema v36. Mining читает enriched `score_trace_log` как primary; v33–v35 логи без TLE-1 detail — graceful degradation (Critic/Sanity per-kind stats недоступны). v32 логи принимаются schema-additive (`score_trace_log` отсутствует → None; mining использует только metadata stats).*

`mine_ai_logs` — оффлайн-утилита, которая агрегирует JSONL-логи AI и печатает статистику по корпусу. Используется для оценки эффективности решений AI: какие интенты часто переключаются впустую, какие критики срабатывают зря, как ведут себя бэнды и агенда. Каждая секция ориентирована на конкретный сигнал — patологию или гипотезу о патологии.

## Команда

```bash
cargo run --release --bin mine_ai_logs -- --dir logs/
```

- Принимает директорию с `*.jsonl` (только top-level — не рекурсивно; `corpus_<date>/` сабдиры игнорируются).
- Обрабатывает `actor_tick` события с `schema_version >= 32`. v31 и ниже — `LogError::UnsupportedSchema`, строка скипается с понятным сообщением.
- v32 corpus принимается как schema-additive: `score_trace_log` отсутствует → `None`; P3b-* секции (A1-trace, E1-trace, G2) будут пустыми.

Связь с replay-метриками (`replay_ai_log`): replay пересчитывает решения текущим кодом и ловит regression'ы (см. [replay.md](replay.md)). Mining работает на сырых логах **as-is** — это инструмент для оценки текущей политики, не валидации регрессий.

## Классы метрик

Утилита группирует выводы в пять классов, по типу анализа.

### Class A — direct aggregation

| Секция | Что показывает |
|---|---|
| **A1.** Adaptation reason frequency | Доля планов в пуле с `EvaluationMode::LastStand` и распределение по `AdaptationReason` (`ProtectSelfNoDefensive`, `ProtectSelfFutile`, `ExpectedSelfLethal`). Низкая доля = adaptation редко срабатывает; высокая на `ProtectSelfNoDefensive` — критики не доходят до self-lethal планов. |
| **A2.** Decision kind frequency | `CastInPlace` / `MoveAndCast` / `MoveOnly` / `EndTurn` / `Skip` — соотношение типов решений за тик. Высокий `MoveOnly` без последующего Cast → актеры тратят ходы на проходку без payoff'а. |
| **A3.** Chosen plan depth histogram | Сколько шагов в выбранном плане. Если 99% планов — depth=1, beam-search не используется. |
| **Skip-path signals** | `skip_total`, `skip_with_stored_goal` — сколько раз AI скипнул ход вообще, и сколько из них с уже сохранённым goal'ом (потенциальная abandoned-ситуация). |

### Class B — sequence reconstruction

**B5.** Decision-kind transition matrix (per-actor, per-combat). Показывает паттерны типа «MoveOnly → MoveOnly → MoveOnly» (зацикленность) или «Cast → MoveOnly → Cast» (нормальный ритм).

### Class C — continuation analysis

**C6.** `classify_continuation_outcome` — сколько решений сохраняют goal, сколько отказываются и почему. Целевые показатели на v32 corpus:

| Outcome | Цель |
|---|---|
| `goal_preserved (combined)` | `≥ 60%` |
| `goal_preserved | method_delivered` | `≥ 10%` (актер довёл арку) |
| `goal_preserved | in_transit` | (балансир) |
| `goal_abandoned | voluntary` | `≤ 10%` (real commitment failure) |
| `goal_abandoned | reactive` | по-источнику breakdown (taunt, panic, viability) |
| `goal_abandoned | invalidating` | (target dead, position mismatch) |
| `goal_abandoned | ttl_expired` | (нормально по дизайну) |
| `no_stored_goal` | первый тик / после Cast/EndTurn |

Плюс распределение `cont_abandoned_reactive` по конкретным источникам (taunt-override, panic, viability fallback).

### Class D — outcome facts

| Секция | Что показывает |
|---|---|
| **D1.** Outcome fact distributions | Гистограмма каждого поля `ActionOutcomeEstimate` (`enemy_damage`, `p_kill_now`, `cc_turns_applied`, `hp_restored`, …) по chosen-plan steps. Аномально низкие damage'и или высокие self_damage — сигнал для policy/critic тюнинга. Post-step 12: `self_damage > 0` ожидается для AoO-провоцирующих сценариев (AoO propagation закрыт в step 12.2; до этого `self_damage` всегда 0 в corpus). |
| **D2.** AoE per-entity damage breakdown | Распределение покрытия AoE — сколько целей и какой урон на каждую. |

### Class E — modifier + jitter

| Секция | Что показывает |
|---|---|
| **E1.** Modifier contributions | Per-modifier (`summon_bonus`, `trade_bonus`, `repair_bonus`) распределения вклада. Знаковое — `trade_bonus` может быть отрицательным. Знаменатель — планы с хотя бы одним эмиттером. |
| **E2.** Picking jitter | `noise_applied` для chosen plans (mean / min / max / abs_max). Sanity-чек что score_noise не доминирует над сигналом. |

### Class F — coverage

| Секция | Что показывает |
|---|---|
| **F1.** AI tags coverage | Распределение `AbilityTag` / `AiTags` среди acted units. Используется ли весь spectrum механик. |
| **F2.** Need signals | Распределение `appraisal::NeedSignals` (`self_preserve`, `rescue_ally`, `apply_cc`, `setup_aoe`, `continue_commitment`, …). |
| **F3.** Continuation severity | Cosmetic / Relevant / Invalidating split — частота ситуаций, где goal нарушается каким изменением. |

### Class G — critics coverage

**G1.** Critics coverage matrix:

- Per-critic hit rate в pool'е (доля планов, на которых сработал критик).
- Cross-tab `Critic × AdaptationReason` — стерлись ли critic-эффекты после rescore (например, `Overcommit` × `protect_self_no_defensive`). После step 11.4 reorder этот сигнал должен быть низким; высокий — баг pipeline-порядка.

### P3b — score_trace_log sourced stats (v33+)

Параллельные секции, читаемые из `score_trace_log` (P3b, schema v33). Появляются только если в corpus есть хотя бы один v33 лог с populated `score_trace_log`; иначе — сообщение «v32-only logs».

| Секция | Что показывает |
|---|---|
| **P3b-A1.** Rescore-mode distribution | `EvaluationMode` из `score_trace_log.rescore_mode` per plan. Параллельный источник к `A1` (trace-sourced). |
| **P3b-E1.** Addend contributions | Значения из `score_trace_log.addends` per (summon_bonus / trade_bonus / repair_bonus). Параллельный источник к `E1`. |
| **P3b-G2.** Multiplier-kind breakdown | Hit count + mean value per `MultiplierKind` (Sanity / Critic) из `score_trace_log.multipliers` для chosen plans. Параллельный источник к `G1`. |

### Class H — bands & agenda (schema v32+)

| Секция | Что показывает |
|---|---|
| **H1a.** Per-band tick count | Базовая частота каждого `PriorityBand`. |
| **H1b.** Winner-intent distribution per band | Какой `IntentKind` чаще всего побеждает в каждом band'е. Sanity: `ForcedTargeting` → 100% FocusTarget, `HardRescueOpportunity` → ProtectAlly доминирует. |
| **H1c.** Per-axis consideration histograms | Распределения по 6 осям `IntentConsiderations` (`urgency`, `feasibility`, `leverage`, `safety`, `role_affinity`, `continuation_value`). |
| **H1c.bis** | Per-IntentKind leverage histograms (step 11.8) — детализация leverage по типу intent. |
| **H2.** Agenda-item win-rate per band | Какой item index (0/1/2) побеждает чаще. **Sanity: `NormalTactical` не должен вырождаться в «всегда item 0»** — это сигнал что N=2/3 expansion бесполезен или considerations-скоринг сломан. |
| **H3a.** Agenda construction-time metrics | Распределение размера агенды per band, rate `item.target=None` per (band, kind), unattributed fallback rate, sub-classification fallback'ов. |
| **H3b.** Per-plan eligibility breakdown | Distribution reject-reasons per (band, item.kind), eligibility rate per (band, item.kind). |
| **H3c.** Fallback cause classification (post-hoc) | Per-band fallback breakdown, per-(band, primary intent), decomposition по причинам. |

## Когда использовать

| Сигнал | Куда смотреть |
|---|---|
| AI стал хуже после изменения формул | сравни D1/D2 + G1 до и после corpus replay |
| Подозрение что critic не работает | G1 — должен быть ненулевой hit rate в релевантных секциях |
| Adaptation rescore стирает penalty | G1 cross-tab `Critic × AdaptationReason`; в текущем порядке должно быть стабильно |
| Heal'еры мечутся между целями | C6 → `goal_abandoned | voluntary` ≥ 10% + B5 матрица per healer |
| Bands wrong distribution | H1a — band'ы редкие/частые против ожиданий |
| Agenda бесполезна | H2 — `NormalTactical` всегда побеждает item 0 |

## Связанные документы

- [`replay.md`](replay.md) — `replay_ai_log` с regression metrics (recompute текущим кодом).
- [`rework/mining_raw.md`](rework/mining_raw.md) — пример вывода ранней версии тулзы (step 0.3).
- [`rework/need_signals.md`](rework/need_signals.md) — спецификация need signals на основе mining'а.
- [`rework/step11_8_findings.md`](rework/step11_8_findings.md) — последний разбор v32 corpus с U-сигналами.
