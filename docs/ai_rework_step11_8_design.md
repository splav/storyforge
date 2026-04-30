# Step 11.8 Calibration Design (skeleton — to fill after 11.7 mining)

> **Status:** Skeleton. Fill in after running mining on new logs collected post-11.7 deploy.
> Source data and H3a output collected during 11.7 mining run; H3b pending new logs.

## Source data

- Mining run on `logs/*.jsonl` after 11.7 reject-reason instrumentation: <date — fill after replay>
- exposure_at_end verification finding: see 11.7b paragraph below
- H3a output: <paste from `cargo run --release --bin mine_ai_logs -- --dir logs/` H3a section>

## 11.7b Finding: exposure_at_end source verification

**Implementation:** `compute_exposure_at_end(plan, ctx)` = `ctx.maps.danger.get(plan.final_pos).clamp(0.0, 1.0)`.

**Source:** reads the danger influence map at the plan's final position. The danger map is built by `build_influence_maps` from the `BattleSnapshot` — it is a spatial map of enemy threat coverage.

**Non-zero when:** actor's final position has non-zero danger value — i.e., when the plan ends in a tile within enemy reach/threat range.

**Zero edge cases:**
1. All plans end in safe tiles (danger=0 at those positions). This is the most likely cause of the observed `safety=1.0` constant in v32 logs — AI scenarios may be too short/safe for enemies to threaten actor final positions.
2. Danger map normalization: if normalization clips small values to 0, low-threat tiles read as 0 even when some threat exists.
3. Actor moves to a "safe" tile deliberately (OvercommitIntoDanger critic may prevent dangerous moves).

**Unit tests (11.7b):** Two tests in `planning::terminal::tests` confirm correct behavior:
- `exposure_at_end_non_zero_when_actor_in_enemy_threat_zone` — danger=0.6 at final_pos → exposure > 0 ✓
- `exposure_at_end_zero_in_safe_backline` — danger=0 everywhere → exposure = 0 ✓

**Verdict:** `exposure_at_end` implementation is correct. The `safety=1.0` constant observed in H1c mining is most likely scenario-bound: AI actors reliably end their turns in safe tiles in the current log corpus (OvercommitIntoDanger critic + short scenarios). Not a code bug.

## Per-axis decisions

### feasibility

- Current behavior: <fill from H1c — expected mean=1.0, p10..p99 all=1.0>
- Diagnosis: <fill — is it binary 1.0 everywhere, or threshold-clipped somewhere?>
- Root cause hypothesis: feasibility formula in `OverlayConsiderationsStage` may clip to 1.0 when `adjusted_score >= some_threshold`. Check overlay line ~36-60.
- Decision: [ ] formula scaling tweak  [ ] new source signal  [ ] structural change  [ ] keep flat with justification
- Proposed formula: <fill>
- Acceptance: distribution shows non-degenerate p10/p50/p90 OR explicit justification why flat is correct

### leverage

- Current behavior: <fill from H1c — expected mean=0.0, p10..p99 all=0.0>
- Diagnosis: <fill — wrong denominator? secure_kill rare? both?>
- Root cause hypotheses:
  1. `secure_kill` is rare in these scenarios → leverage numerator ≈ 0 most of the time.
  2. Denominator normalization issue — divides by a quantity that is usually larger than numerator.
  3. ProtectAlly leverage should measure rescue value, not damage — may be structurally wrong kind.
- Decision: [ ] target-relative normalization  [ ] intent-kind-aware  [ ] both  [ ] keep with justification
- Proposed formula: <fill>
- Open question: ProtectAlly leverage = rescue value, not damage. Worth refactor in 11.8 or defer?

### safety

- Current behavior: <fill from H1c — expected mean=1.0, p10..p99 all=1.0>
- 11.7b finding: exposure_at_end implementation is correct; behavior is scenario-bound (AI stays safe).
- Diagnosis: safety formula = `1 - max(self_damage_factor, exposure_at_end)`. Both inputs are consistently 0 in corpus → safety = 1.0 always.
- Decision: [ ] tweak scaling  [ ] add path_danger source  [ ] keep with justification  [ ] add test scenario with aggressive enemies
- Proposed formula: <fill>
- Note: before changing formula, collect logs from a scenario where enemies actually threaten actor position and verify H1c changes.

## Scope decision

- [ ] Tune formulas only (~1 day)
- [ ] Structural changes (intent-kind-aware leverage etc., ~2-3 days)
- [ ] Defer some axes to backlog with explicit justification

## Acceptance gates for 11.8

- All 6 axes have non-degenerate distributions on rerun mining after formula changes, OR explicit per-axis justification documented
- ai_scenarios golden review (after 11.9 fixture rebuild) shows attributed diff < documented threshold
- H3b section populated (new logs collected after 11.7 deploy) and reject-reason distribution matches expectations

## Follow-ups identified in 11.7

- **HardRescueOpportunity / FocusTarget: 23.5% target=None (8/34 cases).** `build_hard_rescue_opportunity` produces a FocusTarget item even when no threat target is identified. Either (a) skip the FocusTarget item when threat is unknown (agenda becomes N=1 ProtectAlly only), or (b) improve target selection (e.g. nearest enemy, highest priority). Investigate as standalone slice before 11.8.
