# Debug Overlay & Logs

*Источник: `src/combat/ai/debug.rs`, `src/combat/ai/log.rs`.*

## Overlay

`assets/data/settings.toml`:

```toml
[debug]
ai_debug = true
```

| Клавиша | Действие |
|---|---|
| `~` | Toggle overlay карт |
| `1`..`4` | Danger / AllySupport / Opportunity / Escape |

## Консольный лог

При `ai_debug = true` каждый AI-ход печатает: actor + intent + priority target + топ-5 планов + финальная decision. Formatter ходит по `&[TurnPlan]` напрямую через `ScoredStep::from_plan_committed(plan, actor_pos)` — никаких синтезированных адаптеров.

## JSONL-лог

JSONL-лог с raw-факторами и всем пулом планов — через `AiLogger`. Текущая `SCHEMA_VERSION = 34`. Поле `score_trace_log` (с TLE-1 enriched detail: SanityRule, CriticKind+CriticReason, mask original_score) — primary observability source; legacy mirror fields удалены в TLE-3a. Подробнее по replay/анализу — [replay.md](replay.md).
