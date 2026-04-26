## Оценка дополнительного списка

Это сильный набор — заметно сильнее моего предыдущего в части «качество решений». Главное, что он делает правильно:

**Что безусловно берём:**

- **Appraisal / need layer** (#1) — это честное отсутствие в моём списке. Сейчас смысл «насколько нужно действие» размазан между `intent.rs`, factors и sanity. Выделение need-слоя с response curves — прямой upgrade архитектуры, который **одновременно** даёт понятность и качество. Это должно быть в самом верху.
- **Goal-preserving plan repair + continuation evaluator** (#2, #3, #4) — это фундаментальное переосмысление `plan freeze`. Сейчас freeze держится на хрупком snapshot equality; замена на goal context + repair affinity + семантический continuation check действительно закрывает oscillation содержательно, а не симптоматически. Трёх шагов, как в оригинальном списке, многовато — можно свести в два.
- **Outcome vector вместо `score_action`** (#5) — сильный архитектурный пункт, который я фактически пропустил. Мой `StepFactor` трейт был про агрегацию, а outcome vector — про **декомпозицию смысла** до скоринга. Это разные, комплементарные оси; outcome vector важнее, потому что даёт общий словарь для всех потребителей (factors, intent, critics, terminal eval).
- **Terminal evaluation** (#6) — прямо в точку для short-horizon planning с уже имеющейся sim. Я это не поднимал, и зря. Это именно то, что делает reposition/setup/bait/zoning осмысленными без увеличения глубины поиска.
- **Mid-plan reflow derived stats** (#7) — совпадает с моим drift-шагом, но сформулировано лучше: это не «закрыть drift #speed», это «derived current stats как отдельная структура sim». Правильнее брать в этой формулировке.
- **Semantic AI tags** (#8) — отличная идея, отсутствовавшая у меня. Дешёвый способ поднять authored knowledge, сокращает объём эвристики во всех слоях без перехода на HTN. Хорошо работает поверх outcome vector.
- **Critics** (#9) — переупаковка sanity, которая делает ошибки локализуемыми и тестируемыми поштучно. Мой шаг с «soft penalties» молчаливо предполагал, что sanity и дальше будет одним блобом — это хуже.
- **Priority bands + agenda** (#10) + **scorecard intent** (#11) — это правильное развитие моего пункта «decoupling intent». Bands решают проблему «жёсткая лестница vs плоская куча», agenda даёт честную мульти-кандидатную оценку. #10 и #11 логично слить в один шаг, потому что по отдельности они пол-решения.
- **Geometry awareness** (#12) — хорошо, лучше бы как часть outcome vector и terminal eval, чем отдельным шагом. Так и зашью.
- **Team blackboard поверх reservations** (#13) — совпадает с моим TeamPlanner, но формулировка чище: именно «координация замысла», а не просто иерархия.

**Что откладываем / режем:**

- **Ranking-based tuning** (#14) — полезно, но поздно. Бэклог.
- **Coarse→refined** (#15) — это вторая форма portfolio search из моего пункта 13; концептуально одно и то же («иногда нужно оценивать глубже»). Оставляем в бэклоге единым пунктом.

**Что из моего предыдущего списка сохраняется и встраивается:**

- Scenario tests — остаются первыми, без этого новый список строить страшно.
- AiTuning data-driven — остаётся, теперь ещё важнее, потому что response curves нужно где-то хранить.
- PlanAnnotation, PlanStage pipeline, PlanModifier — остаются, они ортогональны content-ideas из нового списка и обслуживают всё остальное.
- Telegraphing, UnitQuirks, Encounter scripting — остаются как «геймплейный» блок.
- Portfolio search (мой 13) — сливается с #15 в единый бэклог-пункт.

**Инварианты зафиксировать явно** (из преамбулы нового списка):

- Shared Effects Core — единый source of truth для real/sim.
- Sanity ≠ Adaptation — cost correction отдельно от value-function switch.

Добавлю третий инвариант, который тоже стоит зафиксировать: **actor-agnostic trade economics** — `unit_value` не зависит от позиции/threat, чтобы self/ally/enemy оценивались одинаково.

---

## Новый итоговый план

Порядок: сначала фундамент (чтобы рефакторить не страшно), потом ядро смысла (appraisal + outcome + terminal eval — это основа качества), потом структурная гигиена, потом геймплей, потом отложенное.

---

### 1. Scenario / regression test framework

**Сложность:** 3
**Польза:** 5

**Цель:** защитить все последующие рефакторинги от тихих регрессий.

**Суть:** фундамент. Без канонических сценариев с assertions любые изменения в appraisal / outcome / terminal eval делать страшно, и любое улучшение качества невозможно верифицировать.

**Как поменять:**
- `tests/scenarios/*.toml` — декларативные описания боевой ситуации (позиции, HP, способности, ресурсы, cooldowns, статусы).
- Два вида assertions: точные («actor выбирает X на (3,4)») и категориальные («intent == ProtectSelf», «primary_effect ∈ {Damage, CC}»).
- Golden JSONL как вторая линия: полный diff при любом расхождении.
- Базовые пакеты: offensive correctness, protect-self, trade economy, oscillation-free continuation (последнее важно для проверки шага 3).
- В CI: падение блокирует merge.

---

### 2. `AiTuning` + response curves как данные

**Сложность:** 2
**Польза:** 5

**Цель:** собрать все константы тюнинга в одном месте и подготовить механизм для response curves, которые появятся в шаге 3.

**Суть:** это бывший мой пункт 3, но приоритет выше: need layer (следующий шаг) нуждается в инфраструктуре для response curves. Без AiTuning curves будут либо захардкожены, либо в шестом месте одновременно.

**Как поменять:**
- `AiTuning` как Bevy Resource из `assets/data/ai_tuning.toml`.
- Три слоя: `base.toml` + `difficulty/{easy,normal,hard}.toml` + опциональный `encounter/<id>.toml`.
- Отдельная секция `response_curves` — параметризованные кривые (logistic / exponential / bezier), на которые будет опираться need layer.
- `DifficultyProfile` становится подмножеством `AiTuning` + derived.
- Live-reload без перекомпиляции.

---

### 3. Appraisal / Need layer ✓ DONE

**Сложность:** 3
**Польза:** 5

**Цель:** перевести сырые факты в нормализованную «срочность действия» до того, как они попадают в factors/intent/sanity.

**Суть:** одно из главных изменений архитектуры. Смысл «насколько нужно» был размазан между `intent.rs`, `factors`, `sanity` и ручными порогами. Need-слой делает систему и понятнее, и качественнее: downstream-слои перестают заново восстанавливать смысл из snapshot'а.

**Как реализовано** (декомпозиция: `docs/ai_rework_step3_plan.md`, сабшаги 3.0–3.6):

- Модуль `src/combat/ai/appraisal/` со структурой `NeedSignals` (8 полей, Copy+Default): `self_preserve`, `rescue_ally`, `finish_target`, `apply_cc`, `setup_aoe`, `reposition`, `conserve_resource`, `continue_commitment`.
- Producer `compute_need_signals(active, snap, maps, memory, tuning) -> NeedSignals` (3.1) считает 5 mineable: `self_preserve`, `continue_commitment`, `finish_target`, `reposition`, `conserve_resource`. Остальные три (`rescue_ally`/`apply_cc`/`setup_aoe`) — `0.0` до второй итерации mining'а (`docs/ai_need_signals.md:166`).
- Входы: tactical facts из `BattleSnapshot` + `AiMemory` (расширен 3 полями в 3.0: `hp_ratio_at_last_turn`, `last_turn_was_defensive`, `turns_in_low_hp`) + influence maps. `recent_damage_taken` derive'ится из `hp_ratio_at_last_turn`.
- Каждый need через именованную `ResponseCurve` (`Logistic { mid, k }` или `LinearClamped { x_lo, x_hi }`) в `AiTuning.curves` — 6 кривых, всё data-driven через `assets/data/ai_tuning.toml`.
- `select_intent` читает `&NeedSignals` через расширенную сигнатуру; `pick_action` вычисляет один раз перед intent selection. Consumers (3.2–3.5):
  - 3.2: `self_preserve` → panic override + soft ProtectSelf branch.
  - 3.3: `continue_commitment` модулирует stickiness в `consider` для FocusTarget/ApplyCC.
  - 3.4: `reposition` → Reposition intent score.
  - 3.5a: `finish_target` → killable score в FocusTarget killable ветке.
  - 3.5b: `conserve_resource` → soft bonus к cheap intents (ProtectSelf, Reposition) при низкой мане.
- Schema bumps: v19→v20 (AiMemory extension), v20→v21 (PanicOverride/Urgency), v21→v22 (Reposition).
- `compute_factors` миграция на NeedSignals **отложена** до step 11 (scorecard). На этом шаге не делалась — нужны bands+agenda как контейнер для structured intent considerations.
- `PlanAnnotation.need_breakdown` (шаг 7) — будет добавлен когда PlanStage pipeline формализуется.

**Реальный gate в 3.6 (mining post-step-3 plays):**
- `actor_hp_drop` divergence: 21.6% baseline → 0% post-fix. ✓ цель ≤12%.
- `reposition → viability_fallback` cascade: 6 transitions baseline → 0 post-fix. ✓ корневая регрессия снята.
- Reposition как chosen: 0.4% baseline → 12% post-fix. Выше эвристического таргета 3–5%, но `reposition → best_priority` 5/5 outgoing — productive setup-moves, не abandon. Принимаем как новую baseline; calibration кривых может пройти в фазе 2b.
- viability_fallback 16% — не связан с reposition после fix'а; это патология P7 (юниты без defensive ability'ей), требует semantic AI tags (step 9).

---

### 4. `ActionOutcomeEstimate` — outcome vector до factors ✓ DONE

**Сложность:** 4
**Польза:** 5

**Цель:** сделать оценку эффекта шага структурированной и общей для всех потребителей.

**Суть:** второе ключевое изменение. `score_action` ранним схлопыванием в HP-equivalent теряла информацию, которую потом восстанавливали `compute_factors`, `intent_score`, trade, sanity — каждый по-своему. Outcome vector даёт общий словарь: сначала структурированная оценка последствий, потом каждый слой читает **свои** поля.

**Как реализовано** (декомпозиция: `docs/ai_rework_step4_plan.md`, сабшаги 4.0–4.13):

- Модуль `src/combat/ai/outcome/` со структурой `ActionOutcomeEstimate` (17 fact-полей: damage, kill, status/control, support, movement, resource — все raw, policy-free). Populator — `outcome::builder::{from_sim_step, hypothetical}` (первый — после sim, второй — для consumers без sim context).
- Policy module `src/combat/ai/policy/` top-level (5 sub-modules: `damage`, `heal`, `friendly_fire`, `status`, `cc`) — pure named functions `fn(facts, context) -> f32`, stateless, swappable. Единственный source of truth для «как мы оцениваем этот факт».
- `score_action` удалён в 4.5. `compute_score_core` распределён в `policy::*` и удалён в 4.12.
- Schema v27 → v28 (clean break в 4.12): outcome shape — fundamental; v27 logs дают `LogError::UnsupportedSchema`, не migration shim.
- Mining D1/D2 baseline на v28 corpus: mean `enemy_damage` ≈ 5.0 HP/step, kill rate ≈ 3%.

**Реальные gate-результаты:**
- 523 lib tests pass, 0 clippy warnings.
- 0/77 golden diverged на v28 corpus (поведение идентично post-7.5 baseline).
- Mining baseline воспроизводится на new fact fields (`enemy_damage` / `ally_damage` / `cc_turns_applied` distributions).

---

### 5. Terminal state evaluation ✓ DONE

**Сложность:** 4
**Польза:** 5

**Цель:** оценивать план по тому, что он реально сделал с доской, а не только по сумме шагов.

**Суть:** у вас уже есть sim — естественный следующий шаг. Для short-horizon planning качество terminal evaluation часто важнее, чем ещё одна эвристика наверху. Особенно важно для reposition, setup, bait, rescue, zoning и для «почти гарантированно умру на ответе».

**Как реализовано** (декомпозиция: `docs/ai_rework_step5_plan.md`, сабшаги 5.0–5.6):

- Модуль `src/combat/ai/planning/terminal.rs` со структурой `TerminalScore` (8 полей, Copy+Default+Serialize+Deserialize): `exposure_at_end`, `next_turn_lethality`, `secure_kill`, `ally_rescue`, `board_control_gain`, `line_actionability`, `density_value`, `pressure_spacing_zone`.
- 3 cluster-сабшага:
  - **Defensive** (5.1): `exposure_at_end` — danger-map penalty в финальной позиции; `next_turn_lethality` — вероятность смерти от всех врагов на следующем ходу.
  - **Offensive** (5.2): `secure_kill` — гарантированное убийство цели; `ally_rescue` — выход союзника из danger после хила; `board_control_gain` — улучшение контроля доски.
  - **Geometric** (5.3): `line_actionability` — AoE-линии открытые с финальной позиции; `density_value` — ценность кластеров в reach; `pressure_spacing_zone` — тактическое расстояние до врагов.
- Producer `terminal_state_score(plan, initial_snap, ctx) -> TerminalScore` вызывается в `finalize_scores`, заполняет `plan.annotation.terminal`.
- Consumer (5.4): aggregator в `finalize_scores` через `AxisProfile::terminal_weights(tuning)` (symmetric к `factor_weights`) + `(1 + need_signals.X)` modulation:
  - `self_preserve` на `exposure_at_end` + `next_turn_lethality` (defensive cluster)
  - `finish_target` на `secure_kill`
  - `rescue_ally` на `ally_rescue`
  - `reposition` на `board_control_gain`
  - `setup_aoe` на `density_value`
- Калибровка `axis_terminal_weights[5][8]` в `assets/data/ai_tuning.toml`. Defensive + offensive кластеры активны; geometric обнулены до фазы 2b mining-калибровки.
- Schema bump v22 → v23: `PlanAnnotation.terminal` сериализуется в `PlanLogEntry`; v22-логи читаются через `#[serde(default)]` → zero-filled `TerminalScore`.
- Migration: `worst_path_danger` / `compute_plan_self_survival` / `offensive::kill_now` / `secure_kill` — оставлены обе реализации, разная семантика. Cleanup в 5.5: 6 stale `score_action` references обновлено, +57 lines net (документация).

**Реальные gate-результаты:**
- 5.4 golden 6/131 (5 целевых defensive — exposure_at_end penalty на dangerous tiles + 1 позиционный, 0 подозрительных).
- 5.5 cleanup: 6 stale `score_action` references обновлено, +57 lines net (документация).

---

### 6. Goal-preserving plan repair ✓ DONE (6.6b)

**Сложность:** 3
**Польза:** 5

**Цель:** устойчивость поведения без хрупкости exact continuation.

**Суть:** замена plan freeze на содержательный механизм. Три шага из исходного списка схлопывались в один: goal context, repair affinity, continuation evaluator + semantic check.

**Как реализовано** (декомпозиция: `docs/ai_rework_step6_plan.md`, сабшаги 6.0–6.6a):

- Модуль `src/combat/ai/repair/` с тремя файлами:
  - `mod.rs` — `ContinuationSeverity { Cosmetic, Relevant, Invalidating }`, `PlanContinuationCheck`, `classify_mismatch`, `ContinuationOutcome` (7 variant'ов: `GoalPreservedMethodDelivered`, `GoalPreservedInTransit`, `GoalAbandonedReactive { source }`, `GoalAbandonedVoluntary`, `GoalAbandonedInvalidating`, `GoalAbandonedTtlExpired`, `NoStoredGoal`; + `LegacyV25Abandoned { reason }` для backward-compat), `FreshDecisionKind { Cast, Move, EndTurn }`, `classify_continuation_outcome` (6 args, включая `fresh_decision_kind` + `fresh_reason` для reactive/voluntary discrimination).
  - `goal.rs` — `GoalKind` (7 вариантов: `Finish` / `Pressure` / `DisableEnemy` / `HealAlly` / `Retreat` / `SetupAOE` / `Reposition`), `StoredGoalContext` (поля: kind, region_anchor, region_radius, planned_ability, ttl, confidence, created_round + severity-поля expected_actor_pos, actor_hp_at_store, actor_rage_at_store, actor_status_hash, target_hp_at_store, target_pos_at_store), `extract_goal_context` producer + `StoredGoalContext::check_continuation` (заменил `PlanSnapshot::mismatch` для goal-уровня).
  - `affinity.rs` — `RepairAffinity` (6 axes: goal_alignment, region_alignment, method_alignment, severity_factor, ttl_factor, confidence) + `RepairWeights` + `compute_repair_affinity` + `aggregate(weights)`.

- `AiMemory.last_goal: Option<StoredGoalContext>` — пишется в `run_ai_turn` после Move-decision, очищается на Cast/EndTurn. `last_plan` + `StoredPlan` удалены.

- Producer affinity в `pick_action` (`utility/mod.rs`): для каждого fresh plan'а в пуле компонует `RepairAffinity` и кладёт в `plan.annotation.repair_affinity`. Severity берётся из `last_goal.check_continuation(actor, target)`.

- Consumer в `finalize_scores` (`scorer.rs`): когда `ctx.last_goal.is_some()`, к финальному score'у каждого finite-плана прибавляется `affinity.aggregate(weights) * (1 + need_signals.continue_commitment) * tuning.thresholds.repair_bonus_scale`. Bonus всегда ≥ 0 — никаких negative penalty.

- Continuation evaluator: два набора role-axis весов в `AiTuning.tables`:
  - `axis_factor_weights` / `axis_factor_weights_continuation` — discovery vs continuation для 10 факторов.
  - `axis_terminal_weights` / `axis_terminal_weights_continuation` — то же для 8 terminal axes.
  - Множители (от discovery): factors — `kill_now ×1.2`, `kill_promised ×1.2`, `tempo_gain ×1.15`, `self_survival ×0.7`, остальные ×1.0; terminal — `exposure_at_end ×0.8`, `next_turn_lethality ×0.6`, `secure_kill ×1.3`, `board_control_gain ×1.3`, остальные ×1.0.
  - Sanity-mask и `apply_protect_self_mask` contract нетронуты — continuation меняет только axis weights в aggregator'е, не EvaluationMode.

- `continuation_from_stored` (exact-continuation path) удалён в 6.6a. Решение всегда строится из fresh plans + repair-affinity bonus. `used_continuation` поле в `PlanDivergenceEntry` оставлено как deprecated (всегда false) для backward compat v24 logs.

- Schema bumps: v22→v23 (terminal eval, ещё в step 5), v23→v24 (`PlanDivergenceEntry` extension в 6.5), v24→v25 (`AiMemorySnapshot.last_plan` → `last_goal: Option<StoredGoalContextSnapshot>` в 6.6a), v25→v26 (7-variant `ContinuationOutcome` split + `FreshDecisionKind` в 6.6b).

- Логирование `mine_ai_logs.rs` секция C6 `=== Continuation analysis ===` показывает разбивку по `goal_preserved | method_*`, `goal_abandoned | reason`, severity distribution, goal_kind distribution.

**Что ещё отложено (из 6.6b backlog):**
- 5 новых ai_scenarios (`continuation_target_dies_replan`, `continuation_cosmetic_rage_tick_no_replan`, `continuation_actor_hp_drop_relevant`, `continuation_setup_aoe_two_ticks`, `continuation_ttl_expires`) — pending.
- Калибровка `repair_bonus_scale` через playtest mining (старт `0.4`).

**Gate-результаты 6.6b:** `in_transit: 24 (58.5%)`, `legacy_v25_abandoned: 17 (41.5%)` на v25 corpus (6 файлов 20260425T17); 0/131 golden; 443 lib tests + 1 ai_scenarios + 6 mine tests зелёные.
v26 mining gate (full voluntary/reactive split) — следующий playtest corpus.

**Gate-результаты 6.6a:** golden round-trip `golden_post_step6.jsonl` 0/131; diff golden_post_step5 vs golden_post_step6 = 0.

---

### 7. `PlanAnnotation` + `PlanStage` pipeline (объединены)

**Сложность:** 4
**Польза:** 5

**Цель:** формализовать цепочку стадий и их диагностику в одной структуре.

**Суть:** это два моих старых пункта, которые теперь лучше делать одним рефакторингом — оба трогают `pick_action` и оба нужны как фундамент для всех последующих изменений.

**Как поменять:**
- `trait PlanStage { fn apply(&self, pool: &mut ScoredPool, ctx: &StageCtx); fn name(&self) -> &'static str; }`.
- Pool — типизированная пара `(plans, annotations)`, не голый `Vec<f32>`.
- `PlanAnnotation { appraisal, outcomes, terminal, sanity: Vec<SanityHit>, critics: Vec<CriticResult>, adaptation, contract, modifiers, continuation, pick }` — каждая стадия пишет только в свою секцию.
- `pick_action` становится `pipeline.run(&mut pool)` + финалайзер.
- JSONL — `#[derive(Serialize)]` над annotation. Schema bump реже, миграции через `#[serde(default)]`.
- Побочно: adaptation перестаёт быть «особенной» — становится одной из стадий.

**Carry-over из 6.7:** ранний `return` в `enemy_turn.rs:103–106` (нет AP/MP → EndTurn) сейчас не пишет divergence-log и обходит decision-block. Внутри 6.7 туда поставлен минимальный proactive-clear stale `last_goal`, но без telemetry. В рамках pipeline это решается естественно: start-of-turn stage пишет goal-state (если `last_goal` есть), декрементит TTL, выбрасывает event при abandon. Затем pipeline обрывается, если actor не может действовать. Найди в `enemy_turn.rs` метку `FIXME(step 7)` — там же.

**Carry-over из 6.9 (paused 1/5):** 4 из 5 `continuation_*` fixture'ов отложены потому что текущий `ai_scenarios` runner не snapshot'ит runtime `AiMemory.last_goal`. Replay всегда запускается с `last_goal = None`, поэтому cross-round preservation fixture'ы прямо не тестируются (high-margin случаи проходят, low-margin флипают). Pipeline даёт возможность инъекцировать AiMemory-state в start-of-turn stage как часть scenario setup — это перепроектирует runner так, чтобы continuation fixture'ы работали детерминированно. Список pending fixtures (`target_dies_replan`, `cosmetic_rage_tick_no_replan`, `setup_aoe_two_ticks`, `ttl_expires`) — в `docs/ai_rework_step6_plan.md` §6.9 «Статус».

---

### 8. `StepFactor` / `PlanFactor` / `TerminalFactor` + `PlanModifier` (объединены)

**Сложность:** 3
**Польза:** 4

**Цель:** формализовать уровни агрегации и пост-composition бонусы.

**Суть:** два моих старых пункта. После появления outcome vector (шаг 4) и terminal eval (шаг 5) это становится чисто структурной работой — правила агрегации кодируются в типах, пост-composition бонусы получают явный trait. Делается за один проход.

**Как поменять:**
- Три трейта для факторов: `StepFactor` с `aggregate_policy()`, `PlanFactor` (один вызов на план), `TerminalFactor` (читает terminal_state). Нормализация — метод трейта.
- `trait PlanModifier { fn modify(&self, plan, ctx) -> f32; }` — возвращает signed addendum после composition. Список модификаторов — одна стадия pipeline'а.
- `summon_bonus` и `trade_score` — первые две реализации `PlanModifier` без изменения формул.
- Каждый фактор / модификатор живёт в своём файле, регистрируется строкой в реестре.

---

### 9. Semantic AI tags для способностей и статусов

**Сложность:** 3
**Польза:** 5

**Цель:** удешевить расширение и сократить объём эвристики во всех слоях.

**Суть:** authored high-level знание без перехода на полный HTN. Работает особенно хорошо поверх outcome vector: теги подсказывают, **какие измерения outcome релевантны** для данной способности.

**Как поменять:**
- В контенте (`abilities.toml`, `statuses.toml`): `ai_tags`, `ai_profile_hints`, опционально `ai_outcome_hints`.
- Словарь: `offensive`, `defensive`, `rescue`, `setup`, `cleanse`, `escape`, `summon`, `zone_control`, `finisher`, `mobility`, `peel`, `commitment_skill`.
- Потребители: `role.rs`, `intent.rs`, `generator.rs`, `scoring.rs`, critics, team coordination. Во всех — теги сначала, существующая эвристика как fallback.
- `PlanAnnotation.effective_ai_tags` для каждого шага.

---

### 10. `PlanCritic` набор (разложение sanity)

**Сложность:** 2
**Польза:** 4

**Цель:** локализовать и сделать тестируемыми конкретные классы ошибок AI.

**Суть:** после pipeline (шаг 7) и outcome vector (шаг 4) critics становятся дёшевыми — каждый читает стандартные поля и возвращает `Pass | Penalize(x) | Flag(reason)`. Sanity.rs разваливается на 5–8 targeted critics с unit-тестами.

**Как поменять:**
- Новый модуль `src/combat/ai/critics/` с `trait PlanCritic`.
- Первая волна: `SelfLethalWithoutPayoff`, `BuffIntoVoid`, `RareResourceForLowImpact`, `BlindspotRanged`, `ZoneOverlapWaste`, `OvercommitIntoDanger`, `HealWithoutRescueValue`.
- Существующий sanity частично переносится, частично остаётся как general-purpose multiplicative penalties.
- Каждый critic — отдельный файл, отдельные тесты, отдельные сценарии из шага 1.
- В `PlanAnnotation.critics: Vec<CriticResult>` — полный лог всех срабатываний.

---

### 11. Priority bands + agenda + scorecard intent (объединяет #10, #11)

**Сложность:** 4
**Польза:** 4

**Цель:** двухступенчатый выбор вместо плоской лестницы, scorecard-модель considerations вместо if/else.

**Суть:** два связанных изменения, которые имеет смысл делать вместе: сначала band (класс важности ситуации), потом agenda (top-N кандидатов внутри band), каждый agenda item оценивается scorecard'ом considerations. Это закрывает разом и хрупкость жёсткой лестницы, и расплывчатость плоского выбора. Требует need layer из шага 3.

**Как поменять:**
- Два уровня:
  1. **Priority band**: `ForcedTargeting` / `CriticalSelfPreservation` / `HardRescueOpportunity` / `NormalTactical`. Выбор по need signals + hard triggers.
  2. **Agenda внутри band**: top-2..4 кандидата с `kind`, `score`, `confidence`, `reason_breakdown`.
- `IntentConsiderations { urgency, feasibility, leverage, safety, role_affinity, continuation_value }` — каждый agenda item оценивается по этим осям, источники — `NeedSignals` + outcome vector.
- Существующие условия (taunt, low HP, ally danger, killability, cluster opportunity) становятся источниками осей, а не прыжками по лестнице.
- Stickiness из шага 6 = `continuation_value`.
- Для каждого agenda item — отдельное планирование; сравнение лучших планов между agenda items.
- В `PlanAnnotation.band`, `PlanAnnotation.agenda` — полный след выбора.

---

### 12. Mid-plan reflow derived stats

**Сложность:** 3
**Польза:** 5

**Цель:** убрать слепоту forward model на многоходовки через speed/reach/status changes.

**Суть:** sim внутри плана должна обновлять текущие tactical capabilities после каждого шага. Это не polishing, это исправление неполной модели мира. Побочно закрывает drift #speed и drift #3 (rage gain).

**Как поменять:**
- В sim-state разделить `base_stats` и `derived_current_stats`.
- После каждого simulated step обновлять: current speed, current reach, cast reach, threat envelope, AoO envelope, mobility restrictions, LoS-relevant flags.
- `rage` мутируется в `apply_primary` при Damage, аналогично real (+1 attacker / +1 defender) — закрывает drift #3.
- `base_speed` отдельно от `speed_bonus_from_statuses` в `UnitSnapshot`, `refresh_status_aggregates` пересчитывает итоговый, pathing читает итоговый — закрывает drift #speed.
- Следующий шаг генератора обязан читать derived values.
- Purity parity tests: `compute_ability_outcome(RngDice@seed)` vs `ExpectedValue` — закрывают оставшиеся drift'ы автоматически.
- Сценарии из шага 1: `self haste → move → cast`, `grant movement → follow-up`, `enemy slow/root → lost reach`, `status armor/vuln reflow`.

---

### 13. `TeamTasks` blackboard поверх reservations

**Сложность:** 4
**Польза:** 4

**Цель:** координация замысла на уровне группы, а не только фактов.

**Суть:** reservations решают конфликт фактов, но плохо выражают коллективное намерение. Blackboard даёт агентам понимание «команда уже добивает цель», «команда держит choke», «команда прикрывает саппорта», «команда готовит AoE». Правильно делать после усиления индивидуального planner/evaluator (шаги 3–12).

**Как поменять:**
- `TeamTasks` поверх `reservations.rs`: `FinishTarget`, `ProtectAlly`, `CCEnemy`, `HoldCell`, `SetUpAOE`, `ZoneArea`.
- После выбора плана юнит публикует task: `claim_strength`, `ttl`, `owner`, `spatial_anchor`.
- Следующие юниты читают task как контекст для: band selection, agenda scoring, terminal eval, critics.
- Damage/CC reservations остаются factual layer, blackboard — semantic layer.
- `coordination` difficulty-ручка управляет жёсткостью следования task'у.

---

### 14. Encounter scripting hooks

**Сложность:** 4
**Польза:** 5

**Цель:** декларативный инструмент для босс-файтов без правки AI-ядра.

**Суть:** жанровая фича. Нужна после team layer, потому что многие триггеры оперируют team intent.

**Как поменять:**
- `EncounterScript` в TOML, подвешивается к encounter.
- Triggers: `on_round_start(n)`, `on_hp_threshold(unit, pct)`, `on_ally_death`, `on_player_used_ability(X)`, `on_turn_start(actor)`.
- Actions: `override_team_intent`, `override_unit_intent`, `override_band`, `add_ability_for_turn`, `spawn_unit`, `bump_tuning(key, delta)`, `publish_team_task`.
- Отдельная стадия pipeline `EncounterDirector` между team planning и unit planning.
- AI-ядро не знает, что его «подталкивают» — получает вход через те же слоты, что использует обычная логика.
- Property-based тесты на trigger firing.

---

### 15. Telegraphing AI-намерений игроку

**Сложность:** 2
**Польза:** 5

**Цель:** сделать AI читаемым для игрока.

**Суть:** архитектурное требование к AI — явный слот между «решил» и «выполнил». Классическая жанровая фича (Into the Breach, XCOM 2 WOTC). Без неё tactical depth обесценивается: игрок не может строить планы против видимых угроз.

**Как поменять:**
- Две фазы: `TelegraphedDecision` (ход T, UI overlay) и `ExecutedDecision` (ход T+1, если план валиден).
- Для Move→Cast телеграфится цель, не путь.
- Инвалидация через `PlanContinuationCheck` из шага 6 — бесплатно переиспользуется.
- Новая стадия pipeline `TelegraphConsistency` между commit и execute.
- Fork-point для encounter scripts из шага 14: «босс телеграфит → игрок реагирует → босс кастует».

---

### 16. `UnitQuirks` — характер поверх роли

**Сложность:** 2
**Польза:** 4

**Цель:** ощутимая differentiation врагов без новой логики.

**Суть:** дешёвое расширение поверх AiTuning. Quirks — смещения в существующих параметрах, не новая логика.

**Как поменять:**
- `UnitQuirks` в контенте: набор biases поверх role weights и need thresholds.
- Примеры: `Berserker { danger_multiplier: 0.3, opportunity_multiplier: 1.5 }`, `Coward { danger_multiplier: 1.8 }`, `Focused { stickiness_bonus: +0.3 }`, `Greedy { overkill_tolerance: 1.3 }`, `Cruel { prefer_low_hp_bias: +0.2 }`.
- Применение: `UnitPlanner::apply_quirks(&mut tuning_view)` перед scoring этого юнита.
- Не инструмент «сделать хуже» — это роль `decision_quality`. Quirks — «иначе».

---

### 17. Geometry awareness signals

**Сложность:** 2
**Польза:** 4

**Цель:** движение и positioning становятся тактически осмысленными, не только «ближе/дальше».

**Суть:** лучше как набор конкретных tactical gains, интегрированный в outcome vector (шаг 4) и terminal eval (шаг 5), чем отдельной системой. К моменту реализации вся инфраструктура уже есть.

**Как поменять:**
- Новые outcome-поля и terminal-signals: `gained_los`, `broke_enemy_los`, `entered_cast_arc`, `left_blindspot`, `opened_aoe_line`, `secured_cover_angle`.
- Читаются в `goal_alignment`, `terminal_state_score`, части critics.
- Проверки дешёвые, кэшируемые на уровне снимка.
- В логах: move оценивается не только за `Δdist`, но и за geometry gain.

---

### Бэклог (на потом, не блокирует текущие шаги)

**B1. Portfolio search / coarse→refined pipeline.** Сложность 4, польза 4. Deep search для сложных развилок (1-ply minmax → MCTS HP-aware). Или как coarse→refined с дорогой оценкой для top-N финалистов. Делать только после стабилизации основного pipeline, и только если появятся конкретные сценарии, где текущего AI не хватает.

**B2. Ranking-based weight tuning.** Сложность 3, польза 3. Offline harness для подбора весов из JSONL-логов (grid search / coordinate descent / линейная ranking model). Имеет смысл только после стабилизации feature set (шаги 3, 4, 5, 8, 10), иначе маскирует архитектурные дыры коэффициентами.

---

## Инварианты, которые не ломаем

1. **Shared Effects Core** — единый source of truth для real pipeline и AI sim. Любая новая механика добавляется туда, не в AI-специфичный код.
2. **Sanity ≠ Adaptation** — cost correction (мягкие штрафы на план) отдельно от value-function switch (переключение EvaluationMode по фактам). Критики (шаг 10) — cost correction; adaptation остаётся separate.
3. **Actor-agnostic trade economics** — `unit_value` не зависит от позиции/relative threat/кто спрашивает. Self / ally / enemy оцениваются одинаково.
4. **Soft penalties, не hard masks** в sanity/critics. `-∞` остаётся только за contract enforcement (ProtectSelf), и только над планами с `mode = Default`.

---

## Волны

**Волна 1 — фундамент и ядро смысла:** 1 → 2 → 3 → 4 → 5 → 6
*После этой волны AI становится качественно другим. Но пока без рефакторинга pipeline.*

**Волна 2 — структурная гигиена:** 7 → 8 → 9 → 10 → 11
*После этой волны архитектура чистая; добавление новых факторов / critics / intents — тривиально.*

**Волна 3 — корректность модели мира и координация:** 12 → 13
*Mid-plan reflow закрывает forward model, team blackboard даёт коллективный intent.*

**Волна 4 — геймплей:** 14 → 15 → 16 → 17
*Encounter scripting, telegraphing, variability, geometry. Можно в любом порядке, но 14 раньше 15, чтобы reuse fork-point.*

**Бэклог:** B1 (portfolio), B2 (ranking tuning) — когда остальное стабильно и есть конкретная необходимость.

---

Если хотите, следующим шагом могу сделать прикладную раскладку по файлам: «какие именно модули и функции трогает каждый шаг, что переименуем, что удалим, что остаётся legacy-адаптером». Самый полезный кандидат для такого разбора — шаг 4 (outcome vector) или шаг 11 (bands + agenda + scorecard), потому что они трогают больше всего уже существующего кода.