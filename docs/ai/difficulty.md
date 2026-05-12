# Difficulty

*Источник: `src/combat/ai/difficulty.rs`, `src/combat/ai/tuning.rs`.*

| Параметр | Easy | Normal | Hard | Описание |
|----------|------|--------|------|----------|
| `awareness` | 0.55 | 0.80 | 1.00 | Сдвиг порогов в `intent/mod.rs` |
| `decision_quality` | 0.30 | 0.75 | 1.00 | Derived → `score_noise` + `top_k_choice` |
| `intent_commitment` | 0.75 | 1.00 | 1.20 | Множитель веса `intent` |
| `survival_instinct` | 0.55 | 0.80 | 1.00 | Derived → reposition / defensive / survival thresholds |
| `resource_discipline` | 0.60 | 1.00 | 1.20 | Множитель веса `scarcity` |
| `coordination` | 0.40 | 0.90 | 1.30 | Overkill penalty + focus-fire bonus |
| `mercy` | 0.35 | 0.10 | 0.00 | Cruelty-shift в tie-breaker окне |
| `plan_max_depth` | 3 | 3 | 3 | Длина плана в beam search |
| `plan_beam_width` | 8 | 16 | 24 | Partial-plan survivor count per depth |
| `plan_step_discount` | 0.75 | 0.85 | 0.90 | `base^k` discount на cumulative-факторы |

`awareness` сдвигает **пороги решений** в `intent/mod.rs`, а не множит нормализованные скоры (иначе сократится при симметричной нормализации).

**Производные lerp-кривые** (`survival_hp_threshold`, `reposition_min_improvement`, `awareness_danger_threshold`) раньше были hardcoded константами в `difficulty.rs`; step 2.6 перенёс endpoints `{lo, hi}` в `AiTuning.difficulty.*_curve` — методы профиля делают `lerp(curve.lo, curve.hi, tier_param)`. Формулы не изменились, значения редактируются в `assets/data/ai_tuning.toml`.

**Per-unit override scaffolding** (step 2.7): `UnitTemplateDef.ai_tuning_override: Option<AiTuningOverride>` (сейчас только `thresholds`) — позволяет quirk'ам (Berserker / Coward / Focused) сдвигать отдельные пороги. В `pick_action` при наличии override строится локальный `AiTuning` через `apply_override` и локальный `AiWorld` — downstream call-sites не меняются. В текущем контенте ни один unit не декларирует override, инфраструктура inert.
