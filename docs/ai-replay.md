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

The replay accepts schema 1–6; newer writes are rejected with a warning.

## Running

```bash
cargo run --bin replay_ai_log -- <log-file> [flags]
```

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
  `[damage, kill, cc, heal, position, risk, focus, intent, scarcity]`.

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
  v1–v5) are skipped with a warning. Bump + migration required when adding
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
