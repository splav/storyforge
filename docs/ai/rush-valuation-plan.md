# AI plan: let the planner value `rush` (banked bonus-movement)

Status: **IMPLEMENTED.** Design chosen: **B — beam-rescue at the truncation gate** (no score term, no magic constant). The `partial_score`-term variant (A) and potential-based shaping (C) are recorded under Rejected alternatives. Follow-up to the `rush` mechanic (engine commit `092ec5e`).

**Outcome:** ~10-line gate change in `generate_plans`. Verified end-to-end: `rush_selected_to_reach_otherwise_unreachable_kill` (orchestration tests) confirms rush is *selected* through the full `pick_action` pipeline when its banked MP opens a kill a normal move can't reach — so the feature is real, not just beam-surviving. `golden_smoke` stayed **0-diverged** and that is **correct**, not a miss: on the recorded fixtures the warrior is never in a frame where rushing beats its chosen action. **No golden recapture, no `SCHEMA_VERSION` bump.**

## Problem

`rush` (`EffectDef::GrantMovement { distance }`, `target_type=myself`, `cost_ap=1` + `2` rage, `distance=2`) now mechanically works — the engine emits `Effect::GrantMP`, adding `distance` to the Mp pool **above** the normal speed cap. It is in unit rosters (warrior class `["melee_attack","taunt","stun","rush"]`, and the `road_bridge` golden fixture). **But the AI never selects it** — after the engine fix, `golden_smoke` stayed 0-diverged (no decision changed).

## Diagnosis (verified, non-trivial)

The gap is **not** in Phase-3 scoring — it is in **Phase-2 beam pruning**.

- Phase-3 already values what rush buys. `tempo_gain` rewards reach toward the intent target; the 8 `terminal_state` axes reward the **end tile** (`secure_kill`, `line_actionability`, `board_control_gain`, `exposure_at_end`, `ally_rescue`, …). A plan whose `final_pos` is reachable **only** via rush scores strictly higher on these than any move-alone plan that cannot reach that tile.
- But Phase-3 only runs on plans that **survive the beam**. In `generate_plans` (`src/combat/ai/plan/generator.rs`), the proxy `partial_score` =
  `damage*0.1 + heal*0.1 + kills*1.0 + stuns*0.5 + pos_value*0.5`, `pos_value = 1 − danger(final_pos)`.
  A **rush-cast node** has `damage=0` and **no displacement** (rush doesn't move → `final_pos` unchanged) → it scores like a stand-still node → the beam (`sort desc; truncate(beam)`) **drops it at depth-1**, before the depth-2 move that realizes the banked MP. So rush-first branches never reach Phase-3.

The payoff horizon is **intra-turn**, not cross-turn: a single plan `rush → move → attack` is 3 steps, fully reachable at `plan_max_depth = 3` (normal/hard). So this is **not** a "value beyond the depth horizon" problem (depth is sufficient) — it is **width-truncation of a zero-immediate-value intermediate node** at depth-1. Deepening lookahead would not help; keeping the node alive for one more expansion would.

## Core fix (B — beam-rescue at the gate)

At the truncation point in `generate_plans`, after `sort` but before committing the frontier, **carry past the beam-width line any node still sitting on unspent banked MP**:

```rust
let mp_cap = actor_u.pools[Mp].map(|(_, max)| max).unwrap_or(0);   // effective speed (incl. status/aura)
...
next.sort_by(|a, b| b.partial_score.total_cmp(&a.partial_score));
let reprieve: Vec<TurnPlan> =
    next.iter().skip(beam).filter(|p| p.residual_mp > mp_cap).cloned().collect();
next.truncate(beam);
next.extend(reprieve);
```

**Why this is the right shape:**
- **No magic constant, robust to `beam_width`.** Unlike a score bump, nothing needs calibrating; changing `plan_beam_width` cannot silently turn the fix into a no-op.
- **Provably surgical.** `residual_mp > mp_cap` can hold **only** for a node that banked MP above the turn cap — i.e. a rush-banked node. For every non-rush actor the reprieve set is empty and behaviour is byte-identical to plain `truncate`. Golden churn is bounded to rush-capable units.
- **Purely additive.** The reprieve only *adds* nodes; it never displaces a legitimate top-beam node (a score bump could). So it cannot perturb non-rush rankings.
- **Honest semantics.** It states the true thing: "a node holding unspent bonus MP isn't judgeable by `partial_score` yet — its value materializes only once the MP is spent, one depth later." `partial_score` stays an unmodified value proxy.
- **`mp_cap` is the Mp pool `max` after turn-start refill** (= `effective_speed`, incl. status/aura), **never** base/class speed. In the live golden log the warrior has class `speed:3` but Mp-max=5; a base-speed cap would make the filter fire for normal plans and break locality.

**Bound on cost.** Only the single acting unit plans, so the carried set per depth is small (≤ the rush-banked frontier nodes; a banked node stops being reprieved once its MP drops to `≤ mp_cap`, so it is carried at most until spent — bounded by `distance`). Each carried node costs one extra `step()` expansion. Negligible.

**NB — scope of the rescue.** This rescues only the **banking** step (`[rush]` at depth-1). Reaching the eventual attack still needs the post-rush `[rush→move-into-range]` node to survive the *normal* beam — its MP is spent (`residual_mp ≤ mp_cap`), so it gets no reprieve and competes raw, exactly like any approach node. The AI finds ordinary `move→attack` today, so this is expected to suffice; if golden *still* doesn't pick rush after this change, this depth-2 survival — **not** the depth-1 prune — is the next suspect. Don't pre-engineer it; let the golden recapture be the test.

## Why rush wins on *value*, not on "ran farther"

The user requirement: rush must be chosen for the **concrete tactical value the extra movement unlocks** (reach an otherwise-unreachable kill, get in range, escape danger, screen an ally, take a better board-control tile) — never for bare displacement. The architecture already enforces this; B adds nothing that could violate it:

1. **The rescue is beam-only — it never reaches the final score.** Verified: outside `generator.rs`, `partial_score` is only ever written in test fixtures, never *read* by `scoring/`, `pipeline/`, or `orchestration/`. Phase-3 re-scores survivors from scratch. B touches only *which nodes survive the beam*, not *how they are scored*. (This is also why potential-based shaping's policy-invariance — its main theoretical draw — buys nothing here: we already have a two-stage beam, so the proxy can never distort the objective.)

2. **The decisive comparison is rush-vs-plain-move, and the scoring is end-state-based.** The 8 `terminal_state` axes + `tempo_gain` score the **end tile**, agnostic to *how* it was reached. So a plain move that reaches an equally good tile earns the **same** positional score **without** rush's rage cost (charged by the negative `scarcity` step-factor). → plain move wins. Rush wins **iff** the better tile is **beyond normal move range** (needs the +2 MP). There is **no** explicit unspent-AP/idle penalty (confirmed: nothing `idle`/`unspent`/`wasted` in scoring) — idling loses purely by opportunity cost (it earns ~0 on every positional axis), so acting-for-position beats idling, but only a plain move that *can't* reach the tile loses to rush.

3. **`tempo_base` displacement is clamped.** `tempo_gain`'s approach term `((Δdist)/effective_speed).clamp(-1,1)` saturates at 1.0 for any full-speed approach, so in the *far band* rush earns no extra tempo over a normal approach. In the *mid band* (reachable only by rush, not yet in range) rush *does* earn extra raw `tempo_base` — but there the terminal axes usually credit the closer tile as genuine positional value (`line_actionability`, `board_control_gain`), which is exactly the "лучшая позиция" we want; the only thing that must sink a *valueless* closer tile is `scarcity`'s rage penalty. Pure-bare-displacement-with-zero-positional-merit is a corner case, pinned by test 2.

**Known limitation (pre-existing, out of scope):** "screen an ally" is modeled only as far as the `intent`/`ally_rescue`/`pressure_spacing_zone` axes go — approaching/standing-near the ally, not true body-blocking/LoS-denial (there's a deferred `+0.2 gained-LoS` TODO in `tempo_gain.rs`). Rush will be chosen to *reach* an ally it otherwise couldn't, not (yet) to optimally screen one. Flag, don't fix.

## Plumbing

`generate_plans` already holds `actor_u` and reads its Mp pool for the seed. Compute `mp_cap` once at the top and use it in the reprieve filter. **`partial_score` is unchanged** (signature and body), so no call sites move. `seed_partial_score` is unchanged.

## Depth / difficulty note

Profiles `easy / normal / hard` (`difficulty.rs`), depths `2 / 3 / 3`, beam `8` on normal. The live golden/replay path uses `normal()` → depth=3, beam=8, so `rush → move → attack` fits. On `easy` (depth=2) only `rush → move` fits (reposition, no same-turn attack) — acceptable (easy is intentionally weak). Run tests under `normal()` / `hard()` to match golden.

## Tests (as built)

Two layers landed; both green.

1. **B mechanism — beam retention** (`generator_tests.rs`, under `normal()`/`hard()`):
   - `beam_rescue_carries_banked_mp_node_past_truncation`: `beam_width=1` + one in-range strike fills the frontier so the `[rush]` node is below the cut; asserts `generate_plans` still emits a depth-≥2 plan led by the rush cast (only possible via the reprieve) and that it banked MP above the cap.
   - `beam_rescue_inert_without_banked_mp`: a non-rush actor never produces a plan with `residual_mp > mp_cap` → reprieve set provably empty → behaviour identical to plain `truncate`. Pins surgical locality.
2. **End-to-end selection** (`orchestration` tests, full `pick_action`, real content):
   - `rush_selected_to_reach_otherwise_unreachable_kill`: speed 3, target at distance 5 (unreachable by a normal move + melee), HP 1; asserts the *chosen* plan contains the rush cast. This is the proof rush can **win**, not just survive the beam — it resolves the depth-2-survival risk.

**Deliberately not built as bespoke unit tests:** the "mid-band bare-approach loses", "reach-to-escape", and "anti-wasted-rush" *selection* scenarios. There is no lightweight full-decision harness, and these validate **existing** Phase-3 scoring (tempo clamp, `scarcity`, terminal axes) that this change does not touch — building a per-scenario `pick_action` harness for each is disproportionate to a 10-line gate. They are covered in aggregate by the golden 0-diff (no spurious rush selection on real fixtures) plus the factor-level tests already in `tempo_gain.rs` / `scarcity.rs`. If golden ever flips to a *valueless* bare-approach rush, that is the signal to add the `ai_policy_ok` fallback and a targeted test then.

## Golden (actual outcome)

`golden_smoke` stayed **0-diverged** — and this was confirmed **correct**, not a hollow fix. The risk was that the reprieve keeps `[rush]` alive but the depth-2 `[rush→move]` node dies before the payoff, so rush could never *win*. The end-to-end test `rush_selected_to_reach_otherwise_unreachable_kill` rules that out: through the full `pick_action` pipeline, rush **is** selected when it opens an unreachable kill. So rush is genuinely considered everywhere; it simply isn't the best move in any *recorded* golden frame (the warrior is never positioned where rushing beats its chosen action). Result: **baseline_v46.jsonl unchanged, no rename, no `SCHEMA_VERSION` bump** (B changes only beam retention; `partial_score` isn't serialized and no RNG calls were added/removed). If a future fixture *does* place a unit where rush should win, golden will diverge there and the recapture guidance above (same file, no bump) applies.

## Fallback (if test 2/5 fails)

If `scarcity` proves insufficient against over-rush, prefer a cheap guard in `ai_policy_ok` (`generator.rs`) — "reject a `rush` cast that opens no otherwise-unreachable reach" — over a terminal penalty. **Avoid** full intent-scoping of `enumerate_next_steps` (breaks the "enumerate intent-agnostic, scoring decides" contract).

## Rejected alternatives

- **A — `partial_score += W·max(0, residual_mp − mp_cap)` (score bump).** Functionally a soft potential on banked MP. Rejected vs B: introduces a magic weight `W` that needs calibration and is fragile to `beam_width` changes, and a large `W` could displace a legitimate node. B expresses the same locality (`residual_mp > mp_cap`) as a constant-free, purely-additive gate. (A's term is also a category error — crediting latent potential as realized score — tolerable only because `partial_score` is beam-only, which B sidesteps entirely.)
- **C — potential-based shaping `r + γΦ(s′) − Φ(s)`, `Φ = banked MP`.** The textbook-clean form, and its policy-invariance is real — but **redundant here**: we already have a two-stage beam (proxy + Phase-3 re-score), so the proxy cannot distort the objective regardless. Formal shaping adds telescoping bookkeeping for zero benefit with a single consumer. Revisit only if a *second* investment-move mechanic (charge-up, aim, stance) appears — then generalize B's `residual_mp > mp_cap` gate into a named `Φ_invest(s)`.
- **Widen the beam** — blunt; inflates the `step()` budget for every unit every turn for one rare branch.
- **Deepen lookahead** — doesn't apply: the payoff is intra-turn and within depth; the problem is intermediate-width truncation, not horizon.
- **Enumerate rush only when bonus-MP opens an unreachable target** — cleanest for anti-over-rush but injects intent-dependency into `enumerate_next_steps`, violating the intent-agnostic contract. Acceptable only as the narrow `ai_policy_ok` fallback.

## Effort / risk

~0.5–1 day (≈10-line gate change + `mp_cap` + 5 tests + golden recapture & diff review). Risk locus is the golden recapture; B's surgical locality (test 4 companion) keeps the diff bounded to rush-capable units. Run senior-code-critic on the diff + golden-diff review before committing.
