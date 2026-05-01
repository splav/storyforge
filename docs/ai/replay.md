# AI Decision Replay (`replay_ai_log`)

Offline verification tool: takes an AI decision log (`logs/*.jsonl`), rebuilds
what the AI saw at each turn, and re-scores the plan pool using the **current
code** (with whatever tuning changes are in the working tree). Reports where
the ranking would have changed.

Purpose: validate scoring/sanity/intent changes against real recorded battles
**without** running the game. Zero-cost iteration on weight calibration —
tweak a constant, rerun replay on a corpus, count how many decisions flip.

## Enabling the log

`[debug].ai_log = true` in `assets/data/settings.toml`. Each combat produces
one file:

```
logs/<UTC-timestamp>_<campaign>_<scenario>_<encounter>.jsonl
```

Example: `logs/20260418T202230_demo_campaign_demo_stormborn_camp.jsonl`.

One JSON object per line (JSONL). Every AI decision becomes an entry with:
the full `BattleSnapshot`, the chosen intent (with selection_kind tag), top-N
plans with **raw** + normalised factors, and the committed decision.

### Schema versions

- **v1** — baseline.
- **v2** — `UnitSnapshot` gains `reactions_left: i32` and
  `aoo_expected_damage: Option<f32>`, used by the AoO-awareness penalty in
  `sanity_adjust_plans`. Old v1 logs still load via `#[serde(default)]`:
  `reactions_left` defaults to `1` (matches the only content-wide
  `Reactions::max`), `aoo_expected_damage` to `None`. On v1 logs the AoO
  penalty sees transitions but cannot estimate damage magnitude — ranking
  deltas reflect topology only.
- **v3** — `UnitSnapshot` gains `caster_ctx` (str/int/spell_power mods) and
  `crit_fail_effect`. Older logs default to zeros and `Miss`.
- **v4** — `UnitSnapshot` gains `damage_horizon: DamageHorizon` (per-unit
  incoming-damage projection). On older logs CC/heal plans fall back to raw
  threat scaling — expect inflated scores for those rows.
- **v5** — `IntentBlock` gains structured `reason` payload (duplicates
  `selection_kind` with typed fields: `killable`/`viability_fallback`/
  `panic_override`/…). The replay only reads `selection_kind` today; the
  structured field exists for downstream tools.
- **v6** — per-plan ADAPTATION dump. Each `PlanLogEntry` gains
  `evaluation_mode` (`default`/`last_stand`), `adaptation_reason`
  (`expected_self_lethal { aoo_dmg, actor_hp }` |
  `protect_self_no_defensive` | null), and `base_score` (pre-adaptation
  score). The `score` field stays as the *final* (post-adaptation) value,
  so older v1-v5 tools that only read `score` still see a meaningful
  number. Verbose mode tags adapted plans with
  `[adapted: last_stand ← <reason>]`.
- **v15** — 4 entry-level telemetry fields for the upcoming killable gate
  (step 3 of AI rework):
  - `gate_applied: bool` — was the killable gate triggered? (stub `false`
    until step 3 ships)
  - `gate_pruned_count: usize` — how many plans the gate masked to -inf
    (stub `0` until step 3 ships)
  - `survival_mode_active: bool` — derived at log-time: intent is
    `ProtectSelf` or `selection_kind` signals panic/last_stand
  - `last_stand_active: bool` — derived at log-time: any plan in the pool
    has `evaluation_mode == LastStand`

  v14 logs deserialize with these fields defaulting to `false`/`0` via
  `#[serde(default)]`. No impact on `raw_factors` layout — bump is
  orthogonal to the Phase 6 axis cleanup.
- **v16** — per-plan `sanity_breakdown` field (step 0.3C). v15 logs default
  to an empty breakdown via `#[serde(default)]`.
- **v17** — three pre-decision snapshots for self-contained replay (step 1.1):
  - `difficulty: DifficultyProfileSnapshot` — full set of 11 difficulty
    knobs frozen at decision time; enables replay with the exact profile
    used in-game rather than a hardcoded default.
  - `ai_memory: Option<AiMemorySnapshot>` — actor's persistent memory
    (`last_intent`, `last_target`, `turns_committed`, `last_plan`) captured
    before `pick_action`. `null` for fresh actors with no prior decisions.
  - `reservations: ReservationsSnapshot` — team-wide reservation state
    (damage, CC, tiles) captured before this actor's own reservations are
    written. Enables "Team coordination" and "Plan freeze" replay scenarios.

  v16 logs deserialize with all three fields defaulting via `#[serde(default)]`
  (`difficulty` → `None`, `ai_memory` → `None`, `reservations` → empty).

The replay accepts schema 1–17; newer versions are rejected with a warning.
Schemas v1–v13 produce an additional warning because their `raw_factors`
layout differs from v14 (three axes removed in Phase 6 cleanup).

*Current schema: v33 (P3b).* v32 is schema-additive with v33: `score_trace_log`
absent in v32 logs → `None`. v31 and below are rejected (missing bands/agenda
fields). In verbose mode (`--verbose`), the chosen plan now shows a
`score_trace:` breakdown — base, rescore_mode, multipliers, addends, masks,
gates, and computed final. See `docs/ai/pipeline.md` for the trace algebra.

## Running

```bash
cargo run --bin replay_ai_log -- <log.jsonl> [<log2.jsonl> ...] [flags]
```

Multiple file arguments are accepted; each is processed in order and a
cumulative `--metrics-summary` section is printed at the end.

Flags:

- `--verbose` / `-v` — show the full ranking for every entry (not just
  entries where the top changed). Adds per-plan `pre → post` with Δ.
- `--simulate-ab` — experimental: simulate the hypothetical intent switch
  (midpanic fallback to `ProtectSelf`) on logs produced **before** the A+B
  fix was deployed. If conditions match (HP below `midpanic_hp_threshold`,
  actor tile above `awareness_danger_threshold`, and the logged entry used
  `viability_fallback`), the replay applies the ProtectSelf mask as if A+B
  had been active at log time. Useful for "would this fix have helped that
  bad decision?" checks.
- `--metrics-summary` — aggregate regression metrics across all processed
  files and print a summary block at the end. Use with a corpus glob to
  capture a baseline: `replay_ai_log --metrics-summary logs/corpus/*.jsonl > baseline.txt`.
- `--campaign <dir>` / `--scenario <dir>` — explicit content override paths
  passed to `ContentView::load_layered`. By default the tool infers the
  campaign and scenario dirs from the log filename
  (`<timestamp>_<campaign>_<scenario>_<encounter>.jsonl`) by scanning
  `assets/data/campaigns/`. Falls back to global-only loading
  (`assets/data`) with a warning if inference fails.
- `--assert [<overlay.toml>]` — run in **assertion mode** against an overlay
  file (see next section). Exit 0 on pass, 1 on mismatch, 2 on IO/parse
  error. Replaces the regular replay output with a pass/fail summary.
- `--capture-golden <out.jsonl>` — iterate all entries in all provided logs,
  run the production scoring pipeline on each, and write one `GoldenRecord`
  per entry to `<out.jsonl>` (JSONL). Fields: `log_path`, `plan_id`,
  `actor_id`, `decision_kind`, `cast_ability`, `cast_target`, `end_position`.
  Use this once to freeze a baseline before a refactor. Exit 0 on success,
  2 on I/O / pipeline error. Mutually exclusive with `--assert` and
  `--compare-golden`.
- `--compare-golden <baseline.jsonl>` — run the same pipeline and compare
  line-by-line against the baseline captured by `--capture-golden`. Prints
  per-field divergences to stderr and a `N / total diverged` summary. Exit 0
  when all records match, 1 if any diverge, 2 on I/O error. Mutually
  exclusive with `--assert` and `--capture-golden`.

## Assertion overlay (`--assert`)

For regression tests and CI gates: each JSONL snapshot can be paired with a
TOML overlay that declares what decision the AI **must** produce on that
snapshot under the current code. Overlay path defaults to
`<jsonl>.expected.toml` (appended to the full filename) or can be passed
explicitly as the `--assert` argument.

```toml
[scope]
plan_id = 5        # optional; default = first entry in the JSONL

# Top-level: array of alternatives. The assertion passes iff ANY variant
# matches fully. An empty/missing expectations list always passes.

[[expectations]]
# Variant A: "press the priority target"
decision_kind = ["CastInPlace", "MoveAndCast"]   # actual ∈ list → OK
cast_ability  = ["fireball"]
cast_target   = [12884901548]                     # entity bits
end_position  = [[3, 4], [4, 4]]
intent_kind   = ["FocusTarget"]
primary_effect = ["Damage"]
not_target    = [12884901543]                     # actual ∉ list → OK

[[expectations]]
# Variant B — independent alternative
decision_kind = ["Move"]
intent_kind   = ["ProtectSelf"]
```

Rules:

- Every field inside a variant is optional (`#[serde(default)]`). Unset
  fields are not checked.
- Every field is a list: `[x]` = exact match, `[x, y, z]` = any-of.
- `not_<field>` — list of forbidden values. Field matches iff actual ∉ list.
- Allowed `decision_kind`: `CastInPlace`, `MoveAndCast`, `Move`, `EndTurn`
  (the replay maps both `MoveOnlyRetreat` and `MoveCloser` to `"Move"`).
- Allowed `intent_kind`: `FocusTarget`, `ApplyCC`, `Reposition`,
  `ProtectSelf`, `ProtectAlly`, `SetupAOE`, `LastStand` (the target entity
  inside intent variants is not compared — use `cast_target` / `not_target`).
- Allowed `primary_effect`: `Damage`, `Heal`, `GrantMovement`,
  `RestoreResources`, `Summon`, `None`. Asserting `primary_effect` on a
  Move/EndTurn decision fails the variant.
- **No assertions on internal scores or `raw_factors`.** Assertions target
  observable behavior only; score-level assertions would break on any
  weight tuning.

### Scope limitation

Assert mode re-runs the existing replay pipeline (`finalize_scores` →
`sanity_adjust_plans` → `pick_best_plan`) with `DifficultyProfile` and
`Reservations` restored from the v17+ snapshots. **Intent selection is not
re-run** — the logged intent is taken as input. This is enough to catch
regressions in scoring, sanity, and plan picking, but cannot catch
regressions in `select_intent` itself. If an intent-level regression test
becomes necessary, the replay pipeline will need `AiMemory` reconstruction
and a `select_intent` call added.

Pre-v17 logs fall back to `DifficultyProfile::normal()` + empty
`Reservations` with a warning; assertion results on those logs may differ
from what the game actually saw.

Output markers:

- `=` entry evaluated, **top plan unchanged** after sanity. Shown only in
  `--verbose`.
- `🔁` entry evaluated, **top plan changed**. Shown always.

Ending summary line: `N entries, K ranking changes after sanity`.

## Interpreting output

### Header per entry

```
🔁 r2 Грозорождённый Буревестник: HP 5/14 AP 1/1 MP 4, intent=FocusTarget [viability_fallback], plans_eval=41, decision=0ms
```

- `r2` — combat round number.
- `HP/AP/MP` — actor stats at decision time.
- `intent=<kind>` — tactical intent as logged.
- `[<selection_kind>]` — queryable tag (`killable`, `viability_fallback`,
  `panic_override`, etc.) extracted from the `intent.selection_kind` field.
- `plans_eval=N` — how many plans the beam search ended up scoring.
- `decision=Xms` — wall-clock time `pick_action` spent on this decision.

### Ranking diff (non-verbose, only on changes)

```
   logged_chose=#1, pre_sanity_top=#1 (+1.83), post_sanity_top=#6 (+1.18)
   pre  #1 score +1.83→-inf  Move→(0,4) · Cast(melee_attack→...)  raw=[...]
   post #6 score +1.16→+1.18  Move→(4,5) · Cast(heal→...)  raw=[...]
```

- `logged_chose` — rank that the game actually picked at log time.
- `pre_sanity_top` — rank after re-normalising + weighting the raw factors
  with current code (no sanity). Useful sanity check that the rescoring
  matches the original logged scores within RNG noise.
- `post_sanity_top` — rank after all current post-hoc adjustments (sanity,
  ProtectSelf mask, etc.) are applied.
- Per-plan lines show the plan's first-step chain and its raw factor vector
  `[damage, kill_now, kill_promised, cc, heal, position, risk, focus, intent, scarcity, tempo_gain, saturation]`.

A score of `-inf` in the **post** column means the plan was masked out by
the current sanity pipeline — either the ProtectSelf mask (when intent is
ProtectSelf), or `sanity_adjust_plans`' lethal-AoO filter (when leaving a
tile would take an AoO that kills the actor outright).

A plan with `score: null` in the raw log file was **pruned by the game
before scoring** (beam-search cut it, usually because the partial factors
were too far below the cutoff). The replay still imports such plans — it
recomputes their `pre` score from the logged `raw_factors` and treats their
logged score as NEG_INFINITY for comparison purposes. Expect to see plans
where `pre` is high but the game never scored them in live play — that is
often the signal that the intent phase diagnosed a target the plan
generator failed to surface, or that AoO/sanity would have killed it.

### Full ranking (`--verbose`)

Each plan appears with its rank, chosen-flag (`★`), pre/post scores, Δ, the
final destination hex, and the step chain. Sorted by **post** score.

## Regression metrics

Three counters that serve as a numeric health-check for the AI scoring
pipeline. Computed from the **logged data** (no re-scoring), so they reflect
the live game's behaviour at the time the log was produced.

### `wasted_mp_ratio`

Fraction of committed `MoveOnly` prefixes whose destination equals the actor's
starting position (displacement = 0). A round-trip move wastes movement points
without advancing the actor toward any goal.

```
wasted_mp_ratio = wasted_MoveOnly / total_MoveOnly
```

Baseline (`logs/corpus_20260421`, 47 entries): **0.0 %** (0/19).
Phase 1 target: no regression.

### `panic_leak_rate`

Among entries where **both** conditions hold:

1. `intent == ProtectSelf` (the actor was in a panic/survival mode), and
2. The chosen plan's `evaluation_mode == Default` (the ProtectSelf mask was
   active — it was *not* overridden by adaptation into `LastStand`),

the fraction where the committed action is **non-defensive**: not `EndTurn`,
not `MoveOnlyRetreat`, not a cast targeting self or an ally.

```
panic_leak_rate = leaked_panic / total_panic

where:
  total_panic  = entries with intent=ProtectSelf AND chosen evaluation_mode=Default
  leaked_panic = … AND committed action is non-defensive
```

**LastStand entries are excluded from the denominator.** When adaptation
transitions all plans to `LastStand`, the actor deliberately commits the most
useful final action regardless of whether it is defensive — that is the
designed behaviour, not a mask leak.

Entries from schemas without `evaluation_mode` (v1–v5) default to `Default`
via `#[serde(default)]` and are included; a warning is printed for those logs.

Baseline: **0.0 %** (0/0 — no Default-mode ProtectSelf entries in current
corpus). Phase 5 target: ≤ 2 %.

### `killable_closure_rate`

Among entries with `selection_kind == "killable"`, the fraction where the
chosen plan's `raw_factors[KILL_NOW_IDX] > 0` — i.e. at least one cast in the
committed prefix scored an immediate kill signal.

```
killable_closure_rate = closed / total_killable
```

Baseline: **36.7 %** (18/49). Phase 2 target: ≥ 85 %.

### `repeated_tile_rate`

Among chosen plans that include ≥1 Move step, the fraction where at least one
tile is visited more than once across all Move paths (starting tile included).
Captures zigzag / return-trip movement where the actor revisits a cell it
already occupied earlier in the same plan.

```
repeated_tile_rate = plans_with_repeated_tile / plans_with_moves
```

Baseline (`logs/baseline_20260422.txt`, 15 боёв, 294 plans-with-moves): **29.3 %**.
Phase 1 (tempo) target: **< 5 %**.

### `zero_net_move_rate`

Among chosen plans that include ≥1 Move step, the fraction where the plan's
`final_pos` equals the actor's starting position (round-trip displacement = 0).

```
zero_net_move_rate = plans_ending_at_start / plans_with_moves
```

Baseline: **17.3 %** (51/294). Phase 1 target: **< 1 %**.

### `post_cast_retreat_rate`

Among chosen plans where a Cast step is followed by ≥1 Move step (post-cast
move), the fraction where:

- the post-cast move revisits ≥1 tile from the pre-cast visit set (including
  the starting tile), **and**
- the net displacement from start at plan end ≤ the displacement at cast time
  (the post-cast move made no net progress away from the starting position).

```
post_cast_retreat_rate = post_cast_retreat_plans / plans_with_post_cast_move
```

Baseline: **33.3 %** (22/66). Phase 1 target: **↓ ≥ 70 %** from baseline (i.e. ≤ ~10 %).

### `killable_non_offensive_rate`  *(step-2 checkpoint)*

Among entries where `selection_kind == "killable"` AND `intent == FocusTarget`
AND ≥1 plan in the pool has a **real kill-line**
(`kill_now ≥ 1.0` OR `damage ≥ target_hp × 0.3`, α from `docs/ai_rework.md §5.2`),
the fraction where the **chosen plan is non-offensive** — it contains no Cast
step directed at the intent target (including the case where the chosen plan has
no Casts at all).

```
killable_non_offensive_rate = killable_non_offensive / killable_with_kill_line_total
```

Step-2 target: **< 2 %**. If already < 5 % post-step-1b → step-3 uses bias
weights rather than hard prune.

### `killable_wrong_target_rate`  *(step-2 checkpoint)*

Subset of the same denominator (`killable_with_kill_line_total`) where the
chosen plan **has** a Cast step but none of them target the intent target (i.e.
the AI cast something but misdirected it at a different unit).

```
killable_wrong_target_rate = killable_wrong_target / killable_with_kill_line_total
```

Step-2 target: **< 5 %**.

### `kill_conversion_rate`  *(step-2 checkpoint)*

Among the same denominator, the fraction where the chosen plan's
`raw_factors[KILL_NOW_IDX] ≥ 1.0` — the target was actually killed this turn
(kill_now normalisation guarantees ≥ 1.0 means guaranteed kill).

```
kill_conversion_rate = killable_kill_converted / killable_with_kill_line_total
```

Step-2 target: **> 85 %**. If already > 70 % post-step-1b → step-3 can be
soft (bias weights); below that → hard prune required.

### `phantom_tail_chosen_rate`

Among chosen plans that contain ≥1 Cast step, the fraction with a
**post-cast Move step** (a Move step after the first Cast in `plan.steps`).
Such Move steps are *phantom tail*: the committed prefix is `Cast` or
`MoveThenCast`, so the trailing Move never executes this tick.

```
phantom_tail_chosen_rate = phantom_tail_chosen / chosen_with_cast_total
```

High values mean the beam frequently selects plans with phantom lookahead.
This is not harmful by itself — see `phantom_tail_flips_committed_rate` for
whether the tail actually distorts the committed action.

### `phantom_tail_flips_committed_rate`

Among chosen plans with a phantom tail (numerator of `phantom_tail_chosen_rate`),
the fraction where the **best tailless alternative** in the scored pool commits a
**different action** than the chosen plan.

```
phantom_tail_flips_committed_rate = phantom_tail_flips_committed / phantom_tail_chosen
```

"Best tailless alt": the highest-*logged*-score plan that has no post-cast Move
tail and is not the chosen plan itself. "Different action" means the two plans'
`committed_prefix()` results differ in at least one of: action kind (EndTurn /
MoveOnly / Cast / MoveThenCast), move destination, ability, or target.

`CommittedActionKey` is extracted via `TurnPlan::committed_prefix()` — the same
production source of truth used by the live picker — so the comparison is
identical to what the game would consider when issuing the `AiDecision`.

- **0 %** — phantom tail is purely cosmetic; same prefix would have been chosen
  regardless.
- **> 0 %** — phantom tail scoring is influencing which committed action wins.
  Investigate whether beam scoring should be gated to the committed prefix.

Baseline (`logs/`, 2 бои, 23 entries): **phantom_tail_chosen_rate = 33.3 %** (4/12),
**phantom_tail_flips_committed = 75.0 %** (3/4). High flip rate signals the
phantom tail is non-cosmetic and actively shifts committed actions on these logs.

### Generating / comparing a baseline

```bash
# Save baseline from the current corpus:
cargo run --bin replay_ai_log -- --metrics-summary logs/corpus_20260422/*.jsonl \
  > logs/baseline_20260422.txt

# After a code change, compare against the saved baseline:
cargo run --bin replay_ai_log -- --metrics-summary logs/corpus_20260422/*.jsonl \
  > logs/candidate.txt
diff logs/baseline_20260422.txt logs/candidate.txt
```

Entries from schemas without the required fields (e.g. v1–v5 lack
`adaptation_reason`) are handled gracefully: the field defaults to `None`, so
they simply don't contribute to `panic_total`. No explicit "partial" marking
is needed.

## Intended workflow

1. Enable logging, play a combat (or let AI vs AI run).
2. Inspect a log entry manually (`jq` / text editor) to confirm a bad
   decision is recorded.
3. Edit scoring / sanity / intent code with a hypothesis for how to fix it.
4. `cargo test && cargo run --bin replay_ai_log -- logs/<that-file>.jsonl` —
   confirm the bad decision now flips.
5. Run against the rest of the corpus to check the change doesn't break
   unrelated decisions. Target: 5–15 % ranking changes on diverse logs. <2 %
   means the fix is too narrow (edge case); >30 % means it's too aggressive
   (revisit the trigger condition).
6. Commit the code change + the log that documents the bad case (so future
   regressions can be caught).

## Gotchas

- **Noise is omitted.** Replay is deterministic. Logged `score` includes the
  game's RNG score-noise (≤ `difficulty.score_noise()`, 0 on hard). Expect
  small (±0.15) deltas between logged `score` and replay's `pre` score even
  before any code change. Ranking should still match unless scores are
  within the noise window.
- **CasterContext is approximated.** The log doesn't record the actor's
  stat modifiers (`str_mod`, `int_mod`, `spell_power`, weapon dice). Replay
  uses zeros. This affects `score_plans_with_raw` if you call it directly —
  but the tool uses logged `raw_factors` as the source of truth, not
  recomputed ones. Sanity and masking use only snapshot + maps + plan
  structure, which are all fully captured.
- **Influence maps are rebuilt.** Replay calls `build_influence_maps` on the
  logged snapshot with `InfluenceConfig::default()`. If you've changed
  `InfluenceConfig` at runtime (e.g. tuning λ values), the replay's maps
  will differ from the game's. Log the config snapshot too if this becomes
  relevant.
- **Schema strictness.** Entries outside the supported range (currently
  v1–v17) are skipped with a warning. Bump + migration required when adding
  or removing fields.
- **Beam-pruned plans.** The log records **only the top-N plans kept by the
  beam**, so plans dropped earlier are invisible. If the intent phase
  diagnoses a `killable` target but the log never shows a plan reaching it,
  check the beam cut-off in `planning::generator` — the replay cannot
  resurrect plans that were never emitted.
- **Deserialization.** The replay requires `Deserialize` on snapshot and
  plan types. Adding new fields to those structs needs both `Serialize`
  (written by the game) and `Deserialize` (read by the tool).

## Extending

### Adding a new post-hoc adjustment

If you add a new plan-level penalty/bonus in `pick_action` (after
`score_plans`), mirror it inside the replay's per-entry loop, between
`sanity_adjust_plans` and the final `argmax`. Keep the order identical to
`pick_action` so the tool stays faithful.

### Filtering / batching

Current tool processes every entry. For larger corpora, `jq` upstream:

```bash
jq -c 'select(.actor_name == "Грозорождённый Буревестник" and .round == 2)' logs/*.jsonl \
  | cargo run --bin replay_ai_log -- /dev/stdin
```

…once stdin support is added (currently the tool reads a path argument —
easy to extend).

### Diff-mode across two commits

Run replay on `main`, save the report. Check out your branch, rerun. Diff
the two reports. A future `--baseline <report>` flag could automate this;
for now, shell diff works.

## Example verification session

```bash
$ cargo run --bin replay_ai_log -- \
    logs/20260418T202230_demo_campaign_demo_stormborn_camp.jsonl --simulate-ab
...
🔁 r2 Грозорождённый Буревестник: HP 5/14 ... (simulated A+B midpanic)
   logged_chose=#1, pre_sanity_top=#1 (+1.83), post_sanity_top=#6 (+1.18)
   pre  #1 score +1.83→-inf  Move→(0,4) · Cast(melee_attack→...)
   post #6 score +1.16→+1.18  Move→(4,5) · Cast(heal→...)
...
=== 6 entries, 2 ranking changes after sanity ===
```

Interpretation: the pre-A+B log had Буревестник rushing melee at 5/14 HP.
Under the A+B simulated switch to `ProtectSelf`, the mask wipes every
melee-on-Lyra plan to `-∞`, and a safe heal plan wins. Specific bad case
resolved; one other actor (R.2 Воин, same midpanic conditions) also shifts
to a retreat. Four other decisions (high HP, low danger) unchanged —
expected for a targeted fix.
