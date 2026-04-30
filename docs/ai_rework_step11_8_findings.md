# Step 11.8 Post-Implementation Findings

> **Status:** 11.8 closed as implementation slice; calibration partial.
> Mining run on post-11.8 corpus collected 2026-04-30 (n=78 ticks, 6 logs).
> Companion to `docs/ai_rework_step11_8_design.md` (the contract); this file
> records the **results** of acceptance verification.

## Verdict

| Aspect | Status |
|---|---|
| 11.8 implementation slice | **PASS** |
| 11.8 calibration slice | **PARTIAL PASS** |
| 11.9 fixture rebuild | **CLEARED** to proceed |
| Code regression | **None** (744 lib + 30 mining tests green) |

The main behavioral goal — making `ForcedTargeting` band a meaningful obligation
rather than a diluted guidance — is achieved. Distribution gates pass for
ProtectSelf and feasibility; Reposition/SetupAOE leverage axis remains flat,
explained as corpus-bound + minor formula rough-edge (not a code bug). Defer
Reposition-specific calibration to post-11.9.

## Acceptance gates

### Gate 1 — Distribution gate: PARTIAL

Per-axis status on n=78 post-11.8 corpus (chosen-plan sampling per H1c.bis):

| Axis / IntentKind | N | mean | middle_mass | Verdict |
|---|---|---|---|---|
| `feasibility` (per-chosen, sampled by spot-check) | — | wide spread 0.376–0.931 | — | **alive** ✓ |
| `feasibility` (item-level baseline, H1c) | 89 | 1.000 | 0.0% | mining display gap (see backlog #2) |
| `safety` | 89 | 1.000 | 0.0% | **expected flat** (corpus-bound; synthetic probe passes) |
| Leverage / FocusTarget | 50 | 0.000 | 0.0% | **mostly explainable** (most samples are non-winning items; see backlog #4) |
| Leverage / ApplyCC | 0 | — | — | no sample (corpus gap; see backlog #3) |
| Leverage / ProtectAlly | 1 | -0.000 | 0.0% | sample too small |
| Leverage / ProtectSelf | 15 | **0.428** | **93.3%** | **alive** ✓ |
| Leverage / Reposition+SetupAOE | 23 | 0.000 | 0.0% | **calibration limitation** (see below) |
| Leverage / LastStand | 0 | — | — | no sample (adaptation-only intent) |

**Analysis:**
- ProtectSelf passes the middle-mass criterion strongly (93%).
- Feasibility formula passes (verified via spot-check); the **all-1.0 reading in the H1c output is a mining display artifact** — H1c samples item-level baseline `agenda.items[i].considerations.feasibility` which is set to default `1.0` at construction time (plan-aware overlay populates `chosen.considerations_per_item`, not the agenda baseline).
- FocusTarget is mostly 0 because most FocusTarget items in the bucket come from agendas where the chosen plan **didn't win via FocusTarget** (e.g., NormalTactical's chosen Reposition plan still has a FocusTarget item in agenda; that item gets per_item leverage = 0 because the plan doesn't engage that target). Of the 50 FocusTarget bucket entries, only ~21 are "chosen plan won as FocusTarget" — but H1c.bis doesn't currently split winning vs non-winning samples (see backlog #4).
- Safety stays flat by corpus design (OvercommitIntoDanger critic + scenario design). Synthetic probe confirms formula correctness.
- Reposition/SetupAOE — see "Known limitations" below.

### Gate 2 — Eligibility gate (Forced): **STRONG PASS**

```
ForcedTargeting fallback:
  baseline (pre-11.8): 45.9%
  post-11.8:           14.3%
  Δ absolute:           -31.6 pp
  Δ relative:           -68.8%
```

Required: `≤ 35% absolute` **OR** `≥ 20% relative reduction`. Both criteria
satisfied. The remaining 14.3% Forced fallback is **100% `only_move`** (plans
that move but do not approach the taunter) — these are tactically irrelevant
to the obligation, fallback is the correct outcome.

This is the headline result of 11.8: pool-level ApproachTarget eligibility
(activated when no plan in the post-Viability pool engages the taunter
offensively) eliminates ~69% of legacy fallbacks without over-attribution.

### Gate 3 — Unit-test gate: PASS

13 new unit tests across Day 1, 2, 3:
- 6 leverage branches (one per IntentKind)
- 2 AoE negative tests (target-specificity)
- 3 ApproachTarget eligibility (positive, pool-fallback respected, band scope)
- 2 feasibility (continuous + `!passed` guard)
- 1 ApproachTarget reject reason (`NotApproachingTarget` distinct from `NotOffensiveVsTarget`)
- 2 mining (per-IntentKind bucketing + Reposition/SetupAOE shared bucket)
- 2 ProtectSelf (stationary cap + active-escape)
- 1 safety probe (formula isolation)

All green; 744 lib total + 30 mining total.

### Gate 4 — No regression: PASS

`cargo test --lib` 744 passed. `cargo clippy --lib` clean. `ai_scenarios` and
`replay_assert` tests are out of scope (pre-existing v30/v32 schema mismatch;
ownership transferred to 11.9 fixture rebuild per the design doc).

## Known limitations

### Reposition/SetupAOE leverage flat at 0 — calibration limitation, not a bug

Sample N=23, all values ≈ 0. Triage findings:

- **`LineActionability`** = `reachable_enemies / 3` from `plan.final_pos`.
  Returns 0 when actor's `max_range = 0` or no enemies are within range from
  the final position. For melee actors at typical retreat-style Reposition
  positions → no enemies in 1-tile range → 0. Formula is **correct**.
- **`PressureSpacingZone`** = `ally_support_at_end - ally_support_at_start`,
  signed in `[-1, 1]`. The overlay clamps it to `[0, 1]`, **discarding the
  negative side** (which encodes "moved away from allies" — a tactically
  meaningful retreat signal). Plans that retreat-spread lose this signal
  entirely; plans that bunch-up gain it.

Conclusion: the Reposition leverage formula reads the right factors, but those
factors do not adequately describe **retreat-style Reposition** — the most
common Reposition pattern in this corpus. The signed-to-unsigned clamp on
`PressureSpacingZone` is a contributing rough edge.

**Decision:** do **not** retune the Reposition formula before 11.9.
- Gate 2 (the main behavioral goal) passed.
- A formula change here would shift behavior; 11.9 fixtures would need a
  second rebuild to capture the post-recalibration baseline.
- Avoid scope creep: 11.8's contract was "intent-kind-aware leverage", not
  "intent-kind-aware leverage with sub-kind splits."

Post-11.9 backlog will introduce a Reposition-specific calibration slice
(see backlog #1) that splits aggressive vs defensive Reposition and uses
formula inputs that capture both directions.

## Live signals

- ApproachTarget eligibility (Forced + pool-level fallback, post-Viability)
  produces **−68.8% relative** Forced-fallback reduction.
- ProtectSelf leverage formula (survival × 0.7 + danger-reduction × 0.3)
  produces a clean continuous distribution: mean=0.428, p50=0.41, p75=0.535,
  middle_mass=93.3%.
- Continuous feasibility formula (`(adjusted_score - 0.0) / 2.0` with
  `!passed` guard) produces wide spread on chosen plans: 0.376 → 0.931 in
  spot-check sample. Data-driven `FEASIBILITY_MARGIN = 2.0` choice is correct.
- Schema v32 round-trip + mining pipeline: end-to-end stable on real corpus.

## Backlog items

### #1 — Reposition/SetupAOE leverage calibration (post-11.9)

Split current single Reposition/SetupAOE branch into aggressive vs defensive
sub-formulas:

```
aggressive_reposition (improve attack access):
  reposition_leverage = 0.5 × line_actionability + 0.5 × cast_range_progress

defensive_reposition (escape threat):
  reposition_leverage = 0.5 × danger_reduction
                      + 0.3 × |spacing_change|         // both directions valid
                      + 0.2 × future_actionability
```

Or — simpler v1 — make `PressureSpacingZone` clamp two-sided in overlay so
retreat signal is preserved:
```
let cluster = terminal.get(PressureSpacingZone).abs().clamp(0.0, 1.0);
```
(Loses sign distinction but at least reads the magnitude.)

Decision on the right shape requires curated retreat scenarios in the corpus
(see backlog #3).

### #2 — Feasibility per-chosen-plan mining display

Current H1c output samples item-level baseline considerations
(`agenda.items[i].considerations.feasibility`) which is always 1.0 at
construction. The plan-aware overlay value lives in
`chosen.considerations_per_item[i].feasibility` and is invisible in mining
output. Add an `H1c.bis` block for feasibility analogous to the per-IntentKind
leverage histogram, sampling chosen-plan plan-aware values. Same pattern as
the leverage extension already in `mine_ai_logs.rs`.

### #3 — Curated corpus expansion for under-sampled buckets

n=78 ticks, with 0 ApplyCC, 0 LastStand, N=1 ProtectAlly. These buckets cannot
be statistically calibrated without representation. Add curated playthroughs
or scripted fixtures that exercise:
- ApplyCC: actor with stun/freeze ability vs threatening enemy
- LastStand: actor in adaptation-mode (`ExpectedSelfLethal`)
- ProtectAlly: multi-ally scenarios with one wounded
- Defensive Reposition: actor under direct threat needing retreat

### #4 — FocusTarget leverage diagnostic split

Current H1c.bis FocusTarget bucket conflates three semantic populations:
1. Chosen plan won via FocusTarget item (expected leverage > 0)
2. Chosen plan has FocusTarget item in agenda but won via different item (leverage typically 0)
3. Chosen plan with FocusTarget agenda item but `agenda_item = None` (fallback, leverage typically 0)

Today all three flow into a single bucket → "FocusTarget leverage = 0"
**looks** like a formula failure when most samples are populations 2 and 3.
Add a stratified output:

```
FocusTarget (winning)        N=...  mean=...  middle_mass=...
FocusTarget (non-winning)    N=...  mean=...  middle_mass=...
FocusTarget (fallback)       N=...  mean=...  middle_mass=...
```

Population 1 is the only one for which leverage signal is expected; conflating
it with 2 and 3 makes the histogram diagnostically useless.

## Decision summary

```
1. Accept 11.8 as a successful implementation slice.
2. Document calibration gaps (above) — do not retune from n=78 corpus.
3. Proceed with 11.9 fixture rebuild on post-11.8 v32 corpus.
4. Open four backlog items as listed above.
5. Reposition calibration explicitly deferred to post-11.9 slice.
```
