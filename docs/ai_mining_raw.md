# AI mining — raw output (шаг 0.3 A+B)

Генерируется командой: `cargo run --release --bin mine_ai_logs -- --dir logs/`
Источник: 36 JSONL-файлов, 761 AI-решений, 273 plan_divergence entries.
Дата: 2026-04-24.

Note: `logs/` содержит 36 JSONL напрямую; corpus-поддиректории (`corpus_20260421/`,
`corpus_20260422/`) не включены — mining читает только top-level файлы в переданной директории.

---

```
# AI mining — step 0.3 A+B

Source: 36 JSONL files, 761 AI decisions, 273 plan_divergence entries

## A1. Adaptation reason frequency (per plan in pool)

Total plans in pool (all logged, not just chosen): 6236

  none                                       5730  ( 91.9%)
  protect_self_no_defensive                   270  (  4.3%)
  expected_self_lethal                        217  (  3.5%)
  protect_self_futile                          19  (  0.3%)

## A2. Intent selection_kind frequency (per decision)

Total decisions: 761

  best_priority                               334  ( 43.9%)
  taunt_forced                                167  ( 21.9%)
  killable                                     96  ( 12.6%)
  viability_fallback                           39  (  5.1%)
  panic_override                               38  (  5.0%)
  protect_self_no_defensive                    36  (  4.7%)
  protect_ally                                 17  (  2.2%)
  setup_aoe                                    17  (  2.2%)
  expected_self_lethal                          6  (  0.8%)
  urgency                                       6  (  0.8%)
  reposition                                    3  (  0.4%)
  protect_self_futile                           2  (  0.3%)

## A3. Chosen plan depth (steps.len) histogram

Total chosen plans: 761

  depth  0      152  ( 20.0%)
  depth  1      197  ( 25.9%)
  depth  2      250  ( 32.9%)
  depth  3      162  ( 21.3%)

## A4. Continuation invalidation reasons (plan_divergence entries)

Total plan_divergence entries: 273

  continuation_invalid                        191  ( 70.0%)
  actor_hp_drop                                59  ( 21.6%)
  target_hp_drop                               13  (  4.8%)
  target_moved                                  5  (  1.8%)
  actor_status_changed                          4  (  1.5%)
  actor_pos_mismatch                            1  (  0.4%)

## B5. Intent transition stability matrix

Grouping: per actor per combat (JSONL file). Ordered by plan_id.
Unique (combat, actor) pairs tracked: 137

                  FROM \ TO                best_priority         expected_self_lethal                     killable               panic_override                 protect_ally          protect_self_futile    protect_self_no_defensive                   reposition                    setup_aoe                 taunt_forced                      urgency           viability_fallback  |  TOTAL
---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------
              best_priority                          179                            2                           38                           11                            3                            1                           12                            1                            .                           16                            .                            9  |    272  (43.6%)
       expected_self_lethal                            2                            1                            .                            .                            .                            .                            .                            .                            .                            .                            .                            .  |      3  (0.5%)
                   killable                           43                            .                           26                            3                            1                            .                            2                            .                            1                            1                            .                            .  |     77  (12.3%)
             panic_override                            7                            .                            2                            6                            2                            .                           11                            .                            .                            1                            2                            1  |     32  (5.1%)
               protect_ally                            3                            .                            .                            .                            8                            .                            .                            1                            1                            1                            .                            1  |     15  (2.4%)
        protect_self_futile                            .                            .                            .                            .                            .                            1                            .                            .                            .                            .                            .                            .  |      1  (0.2%)
  protect_self_no_defensive                            3                            .                            3                            1                            .                            .                            9                            .                            .                            1                            .                            .  |     17  (2.7%)
                 reposition                            1                            .                            1                            .                            .                            .                            .                            .                            .                            .                            .                            1  |      3  (0.5%)
                  setup_aoe                            6                            .                            1                            1                            1                            .                            2                            .                            .                            2                            .                            2  |     15  (2.4%)
               taunt_forced                            8                            3                           10                            8                            .                            .                            .                            .                            .                          118                            2                            1  |    150  (24.0%)
                    urgency                            1                            .                            2                            .                            .                            .                            .                            .                            .                            .                            2                            .  |      5  (0.8%)
         viability_fallback                            4                            .                            4                            1                            .                            .                            .                            .                            .                            6                            .                           19  |     34  (5.4%)
---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------
                      TOTAL                          257                            6                           87                           31                           15                            2                           36                            2                            2                          146                            6                           34  |    624

Total transitions: 624

Top-10 transitions:
  best_priority                            -> best_priority                               179  (28.7%)
  taunt_forced                             -> taunt_forced                                118  (18.9%)
  killable                                 -> best_priority                                43  (6.9%)
  best_priority                            -> killable                                     38  (6.1%)
  killable                                 -> killable                                     26  (4.2%)
  viability_fallback                       -> viability_fallback                           19  (3.0%)
  best_priority                            -> taunt_forced                                 16  (2.6%)
  best_priority                            -> protect_self_no_defensive                    12  (1.9%)
  best_priority                            -> panic_override                               11  (1.8%)
  panic_override                           -> protect_self_no_defensive                    11  (1.8%)

Self-loop rate (intent unchanged between ticks): 369 / 624 (59.1%)
```
