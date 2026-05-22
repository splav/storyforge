# Engine state как единственный source of truth — план унификации (v2)

**Статус**: rewrite после критики и аудита кода (v2). Patched 2026-05-21
после `bridge_turn_lifecycle` work — см. §«Post-bridge update» ниже.
Patched 2026-05-22 после U1+U2/C1-C4 — см. §«Post-U2 update».
Patched 2026-05-22 после U3/A-C — см. §«Post-U3 update».
Patched 2026-05-22 после U4 — см. §«Post-U4 update».

**Текущий HEAD**: `5b15587 refactor(ai): U4 — engine CombatState becomes sole post-step truth; delete sim back-projection`.

**Прогресс**: U1 ✅ · U2 ✅ (C1–C4) · U3 ✅ (A–C) · U4 ✅ · U5 → U6 → U7 — pending.

## Цель

Engine `CombatState.Unit` — единственный runtime источник истины для gameplay-state
юнитов. AI добавляет immutable side-table `AiCache` для derived метрик. Никаких
параллельных копий, никаких sync-points, никаких lossy reconstructions.

### Желаемый endpoint

```rust
BattleSnapshot {
    state: CombatState,    // authoritative gameplay state
    cache: AiCache,        // AI-derived, snapshot-stable, read-only
}

UnitView<'a> {             // read API
    state: &'a Unit,       // engine Unit via Deref
    cache: &'a UnitAiCache,// AI-derived via .cache.*
}

SimState<'a> {             // "what-if" simulator
    state: CombatState,    // owned clone, mutated via engine `step()`
    cache: &'a AiCache,    // borrowed, не мутируется в sim
    actor: UnitId,
}

UnitSnapshot              // удалён. Логи v36/v37 регенерируются
                          // под новую schema, legacy reader не нужен.
```

### Логирование: clean break OK

Пользователь подтвердил: **пересобирать логи или сделать логирование более
органичным — допустимо**. Это снимает главный constraint предыдущей версии
плана — необходимость держать `UnitSnapshot` навсегда как legacy v37
deserializer. Следствия:

- **Schema bump (v37 → v38) — часть core path**, не optional финал. Когда
  поле `BattleSnapshot.units` исчезает, сериализатор естественно пишет
  `{state, cache}`. Старые baselines (`golden_*.jsonl`,
  `tests/ai_scenarios/snapshots/*/log.jsonl`) **регенерируются** с ручным
  diff-review.
- **`UnitSnapshot` удаляется полностью** в финале — не изолируется в legacy
  модуль. Никаких `combat::ai::log::legacy_v37`.
- **Опциональная редизайн логирования** (event-stream, delta-logging,
  direct engine native serialization без AI-обёртки) — orthogonal вопрос,
  не блокирует unification. Если хочется органичнее — выделяем отдельной
  фазой после `U6` (см. §«Backlog»).

---

## Re-framing: что на самом деле осталось сделать

Прошлая версия плана описывала Phase E как «большой архитектурный шаг — переключить
SimState на мутацию engine.Unit». Аудит кода показал, что **этот шаг уже сделан**:

- `SimState.combat_state: CombatState` строится в `from_snapshot`
  (`src/combat/ai/plan/sim.rs:52, 58`).
- `apply_step` зовёт `combat_engine::step(&mut self.combat_state, action, &mut dice, &content)`
  напрямую — никакого hand-rolled damage math.
- Engine ↔ AI RNG boundary уже чистый: `EngineExpectedValue` реализует
  `DiceSource`, AI всегда передаёт его в `step()`.

Что **остаётся** — это удалить «теневую» копию состояния, которая дублирует engine:

1. `BattleSnapshot.units: Vec<UnitSnapshot>` и `by_entity` индекс.
2. `project_engine_to_snapshot` — обратная проекция после каждого `step()`.
3. `snapshot_to_combat_state` — прямая проекция при `from_snapshot`.
4. Callsites, читающие `snap.unit_snapshot(e) -> &UnitSnapshot` вместо `state.units()`.
5. Test fixtures, конструирующие `UnitSnapshot { … }` напрямую.

Таким образом ось плана сдвигается: **Q1 (2A vs 2B) — не вопрос**. Engine API
уже используется. Главная работа — мирно удалить параллельный слой и провести
callsite-миграцию.

---

## Post-bridge update (2026-05-21)

Между v2 rewrite (HEAD `3d7a14b`) и сегодняшней работой над `bridge_turn_lifecycle`
(HEAD `01676a8`) закрыт параллельный фронт, который **меняет scope** некоторых
U-фаз. См. [bridge_turn_lifecycle.md](bridge_turn_lifecycle.md) для деталей.

### Что появилось

1. **`BattleSnapshot.uid_to_entity: HashMap<UnitId, Entity>`** + симметричный
   `entity_to_uid: HashMap<Entity, UnitId>` (B-prime, коммиты `1b522d3`,
   `01676a8`). Заполняются в `build_snapshot` из `UnitIdMap`. Используются
   снаружи через `pub fn entity_for_uid(uid) -> Option<Entity>` и
   `pub fn uid_for_entity(entity) -> Option<UnitId>`.

   **Это убирает все production `Entity::from_bits(u.id.0)` shortcut'ы в lookup-
   методах snapshot.rs (`unit`, `unit_at`, `enemies_of`, `allies_of`,
   `all_enemies_of`, `dead_enemies_of`, `dead_units`) + `UnitView::entity` +
   `target_selection_score`.** Production `from_bits` callsites = 0.

2. **Engine sole owner of turn lifecycle** (Phase 1+B5). `step(EndTurn)`
   эмитит весь cascade включая refill/regen/tick для нового актора. Death of
   current actor → `Effect::Death` derives `AdvanceTurn`. `init_state_from_ecs`
   runs once per combat (`ctx.round < 2` guard). `engine_turn_start_system`
   удалён.

3. **Engine `enemy_phases` pop** (5ffd874). `Effect::EnterPhase` apply now pops
   `unit.enemy_phases[0]` — symmetric to bridge's ECS-side pop.

4. **Regression tests** в `tests/combat/handoff.rs` (7 cases) покрывают:
   mid-round handoff, dead-first-actor round wrap, frame-ordering, status-no-
   double-tick, death-mid-action, multi-combat session teardown, summon lookup.

### Влияние на U-phases

- **U7 (dedup `UnitAiCache.entity`)** — теперь практически тривиально. У нас
  уже есть `BattleSnapshot.entity_to_uid` map; миграция сводится к "выкинуть
  поле `entity` из `UnitAiCache`, переписать `cache.unit(entity)` callsites
  на `snap.uid_for_entity(entity) → cache.unit_by_uid(uid)`". Без map'а
  было бы invasive. **Можно слить с U6 одним коммитом или оставить trivial
  follow-up.**

- **U5 (schema flip)** — два legacy callsite в `snapshot.rs` (`UnitSnapshot::as_pair`
  + `BattleSnapshot::new`'s `summoner` mapping) уже имеют `// LEGACY: …`
  marker-комментарии указывающие что они удаляются здесь. Лёгкий ориентир для
  diff-review.

- **U2 (callsite migration)** — scope в основном тот же (~17 production
  callsites через `snap.unit_snapshot()` accessor). Note: `snap.unit(entity)`
  уже корректно резолвит summons (B-prime audit), так что миграция
  `unit_snapshot()` → `unit()` callsite-by-callsite не несёт латентных
  silent-skip регрессий.

- **U0 (corpus expansion)** — пользователь решил **пропустить** этот hard
  precondition. Принят accepted risk: вместо широкого corpus полагаемся на
  176 combat_engine + 42 combat + 7 handoff (B-prime + bridge work) + 820 lib
  тестов как safety net. R1 (RNG boundary, crit-fail под ExpectedValue) не
  митигирован специальным тестом — рассчитываем что existing parity tests
  поймают регрессию.

- **U1 (parity guard)** — по-прежнему нужен. Existing tests не сравнивают
  `project_engine_to_snapshot(state)` поле-в-поле с `state.units`. U1 это
  добавляет; делать первым шагом для безопасности U4.

### Что НЕ меняется

- Архитектурная ось плана: drop `BattleSnapshot.units`, schema v37→v38,
  delete `UnitSnapshot` тип.
- Зависимости U1 → U2 → U3 → U4 → U5 → U6.
- Список «Чего не делаем».
- Risks & mitigations table (R1 особенно — corpus skipped).

---

## Post-U2 update (2026-05-22)

U1 и U2 завершены пятью коммитами. Cascade пошёл цепочкой
`C1 → C2 → C3 (+ fix-and-finish) → C4`, каждый коммит компилируется
независимо и проходит полный suite (820 lib + 44 combat incl. parity +
1 golden_smoke).

### Коммиты

| Phase | Hash | Содержание |
|-------|------|------------|
| U1 | `37662c2` | Parity guard: `engine.units ≡ snapshot.units` field-by-field after `apply_step`. 7 параметризованных кейсов (Cast/Move × состояния). Zero divergence — `project_engine_to_snapshot` корректен. |
| U2/C1 | `0dcd991` | `ScoringCtx.active_view: UnitView<'p>` добавлен (non-Option); `AppraisalCtx.active` полностью флипнут на `UnitView<'a>`. Appraisal submodules мигрированы. Legacy `ScoringCtx.active: &UnitSnapshot` сохранён для C2/C3 compat. |
| U2/C2 | `5ad8026` | Intent layer (bands/agenda/select/score) + pipeline stages + `scoring/target_selection`. Forced ripples в `aggregate.rs`, `future_value.rs`, `log/debug.rs`. `pick_action` re-derive убран для `assign_band`/`build_agenda`. |
| U2/C3 | `931272e` | Adapt + `scoring/trade::unit_value` + `plan/{sim::actor_unit,reach,generator,future_value}` + orchestration/fallback. `actor_unit() -> Option<UnitView<'_>>` per O3. Skipped: `world/influence.rs` (Plan agent overcount). |
| U2/C4 | `fc2a4b4` | Финал: 6 debug builders на `UnitView`; legacy `ScoringCtx.active` удалён, `active_view → active` rename; `unit_snapshot()` → `pub(crate)`; `replay_ai_log`/U1 parity test перешли на `snapshot.units.iter().find()` shim. `pick_action.active_snap` re-derive удалён. |

### Что появилось / изменилось в архитектуре

- **`ScoringCtx.active: UnitView<'p>`** — чистый, single-field API. `make_scoring_ctx`
  test-helper сохраняет `&UnitSnapshot` параметр для ergonomics, внутри резолвит
  UnitView. Без транзитивной Option-обёртки.
- **`actor_unit() -> Option<UnitView<'_>>`** в `plan/sim.rs` — чистый flip
  (no additive parallel API) per user decision O3.
- **`BattleSnapshot::unit_snapshot()` → `pub(crate)`** per O1. Out-of-tree callers
  (`src/bin/replay_ai_log.rs`, U1 test) перешли на `snapshot.units.iter().find()` —
  оба этих shim'а умрут в U5 вместе с полем `units`.
- **`UnitSnapshot::is_stunned(tags)` / `forces_targeting(tags)`** сохранены как
  shims в `snapshot.rs:439,449` — нужны для sim-cloning путей в
  `outcome::builder` / `aggregate::compute_plan_intent_sum` (умрут в U3/U5).

### Влияние на оставшиеся U-фазы

- **U3** — scope теперь немного шире чем планировался: помимо tests/ есть
  компиляционно-форсированные `snapshot.units.iter().find()` callsites в U1
  parity guard test и `replay_ai_log` бинарнике. Эти ровно две точки уйдут
  естественно при U5 (когда исчезает поле `units`); до того момента их можно
  игнорировать как ожидаемый shim.
- **U4** — без изменений. `project_engine_to_snapshot` и `snapshot_to_combat_state`
  всё ещё на месте, U1 parity guard готов как safety net.
- **U5** — без изменений в scope. Bin migration (`replay_ai_log`, `mine_ai_logs`)
  теперь конкретнее: один знает свой shim site.
- **U6** — без изменений.
- **U7** — без изменений (по-прежнему тривиально после B-prime).

### Lessons learned

1. **Декомпозиция работает**: 4 коммита по 8–22 файла каждый — приемлемая
   единица для одного агента. Больше — упирается в context limit.
2. **`Option<T>` для одного теста** — антипаттерн: лучше потратить 1 строку
   на правку фикстуры, чем тащить Option через всю миграцию.
3. **Fix-and-finish паттерн** для частично выполненной работы — рабочий:
   compile errors дают агенту фокус, отдельный focused агент не нагружен
   контекстом первоначальной миграции.
4. **Doc-edits — отдельный коммит после фазы**, не внутри. Агенты дважды
   пытались править spec во время выполнения; оба раза откатывалось.

---

## Post-U3 update (2026-05-22)

U3 завершён тремя коммитами. Тестовые фикстуры больше не конструируют
`UnitSnapshot { … }` literal'ами — все используют существующий `UnitBuilder`.

### Коммиты

| Phase | Hash | Содержание |
|-------|------|------------|
| U3/A | `a675583` | Добавлено 2 setter'а в `UnitBuilder` (`armor_bonus`, `damage_taken_bonus`); мигрировано 2 literal'а в `src/combat/ai/{intent/score.rs:432, scoring/policy/tests.rs::random_target}`. Остальные сайты в `src/` уже были на builder'е до начала. |
| U3/B | `414f505` | 13 literal сайтов + 2 `make_unit` closure'а в `tests/combat/sim_parity.rs` → `UnitBuilder.build_pair()` + `snapshot_from_pairs` (Q2 engine-native). Добавлено 3 setter'а: `statuses`, `movement_points`, `speed_override` — для U1 parity closure нужен независимый контроль `base_speed`/`speed`/`mp` (haste). U1 parity guard продолжает работать (construction migrated, read side `snap.units.iter().find()` сохранён до U5). −221 строк net. |
| U3/C | `292bbbe` | `tests/combat_engine/parity.rs`: `make_snap_unit` helper + 2 inline literal'а → builder pairs; 4 `snapshot_from` → `snapshot_from_pairs`. `benches/engine_move.rs` пропущен — pre-existing baseline broken (engine API drift). |

### Final exit grep

`ya tool ast-index search 'UnitSnapshot {'` возвращает только allowlist:

- `src/combat/ai/world/snapshot.rs` — struct definition + 3 projection сайта
  (`project_engine_to_snapshot` — U4 scope)
- `src/combat/ai/test_helpers.rs` — UnitBuilder definition (124, 268, 385)
- `src/combat/ai/log/mod.rs:2113` — `schema_round_trip_v36_identity` test
  (R3 exemption — byte-exact schema parity literal, intentional)
- `src/bin/mine_ai_logs.rs:3068` — binary, U5 scope
- `benches/engine_move.rs:39-40` — separate broken baseline, не в exit gate

`tests/` дерево полностью чисто — ни одного direct UnitSnapshot literal.

### UnitBuilder API после U3

Все добавления в `UnitBuilder` (`src/combat/ai/test_helpers.rs`):

- A: `armor_bonus(i32)`, `damage_taken_bonus(i32)`
- B: `statuses(Vec<ActiveStatusView>)`, `movement_points(i32)`, `speed_override(i32)`

Total: 5 новых setter'ов. Существующая семантика `.build()` / `.build_pair()`
не менялась — оба терминатора работают.

### Влияние на оставшиеся U-фазы

- **U4** — без изменений. `project_engine_to_snapshot` и `snapshot_to_combat_state`
  всё ещё на месте; U1 parity guard готов как safety net.
- **U5** — scope сужается: schema flip + удаление `BattleSnapshot.units`/`by_entity`/
  `round` теперь чище, потому что тесты не зависят от literal layout'а. Bin
  migration (`mine_ai_logs.rs:3068`, `replay_ai_log.rs` shim из C4) — точечная.
- **U6** — без изменений (всё ещё удалить `UnitSnapshot` struct).
- **U7** — без изменений (тривиально после B-prime).

### Lessons learned

1. **Plan-agent's stale survey**: первоначальная enumerate'а Plan-агента указывала на
   4 сайта в `src/combat/ai/`, но 2 из них (`dummy_unit`, `base_target`) уже были
   мигрированы. Implementer'у пришлось верифицировать. Урок: при выполнении плана
   первым шагом implementer должен grep'ом подтвердить актуальность списка.
2. **`snapshot_from_pairs` work'нул чище ожидаемого**: Q2 (engine-native сразу)
   дал чище diff, чем minimal-diff snapshot_from. После U3 `snapshot_from` и
   `as_pair` остались тоько в одном callsite — U5/U6 сможет их выкинуть без
   церемоний.
3. **`benches/` накапливают долг**: `benches/engine_move.rs` сломан с момента C2.
   Pre-existing baseline failure не блокирует основной поток, но требует отдельной
   санации — кандидат на backlog после U6.

---

## Post-U4 update (2026-05-22)

U4 завершён одним коммитом `5b15587`. Центральный архитектурный шаг плана:
engine `CombatState` стал единственным источником post-step истины внутри
`SimState`.

### Что сделано

- Удалены `project_engine_to_snapshot` (40-строчный mirror loop) и
  `snapshot_to_combat_state` (forward projection из `snap.units`).
- `SimState::from_snapshot` инициализирует `combat_state` через `snap.state.clone()`.
- 3 callsite'а `project_engine_to_snapshot` (apply_endturn, apply_move Ok,
  apply_cast Ok) убраны.
- Q3 cleanup: `apply_cast` UnitId→Entity маппинг теперь через
  `snapshot.entity_for_uid()`, не через `snap.units.iter().find()`.
- U1 parity fence удалён (412 строк) — invariant который он защищал, U4
  целенаправленно ломает. 4 in-file теста, читавших stale `snap.units` через
  `unit_snapshot()`, переписаны на `snapshot.unit()`.

Net: −516 строк (+74 / −590).

### Pragmatic deviation от spec'а

Spec говорил «engine — единственный источник», но `SimState` оказался
держать ДВЕ `CombatState` в синхроне: `self.combat_state` (где engine
мутирует) и `self.snapshot.state` (clone, обновляется per-step для внешних
readers). Причина: внешние callers зовут `sim.snapshot.unit(entity)` — это
читает из `snap.state`, а не из `combat_state`. Без sync вернул бы pre-step.

Решение implementer'а: keep `self.snapshot.state = self.combat_state.clone()`
после каждого apply. Стоимость: 1 clone per step (sim path, не hot). Win:
174-строчное mirror loop удалено, API не меняется.

**Architectural future cleanup** (не в spec U4, candidate для backlog):
ввести `SimState::unit(entity) -> Option<UnitView>` accessor читающий
из `combat_state`, мигрировать callers с `sim.snapshot.unit(...)` на
`sim.unit(...)`, убрать `self.snapshot.state` sync. Это снимет дуальность
`CombatState` инстансов внутри `SimState`.

### Risk-analysis результаты

- **R-B (golden churn)** — не материализовался. Plan-agent правильно
  определил: serialization путь идёт через `AiLogEntry.snapshot` —
  ОРИГИНАЛЬНЫЙ pre-decision snap из `build_snapshot`, не sim-mutated copy.
  `cargo test --test golden_smoke` — byte-for-byte идентичен.
- **R-F (sim.rs:1249 lethal test)** — fix straightforward: переписали через
  `sim.snapshot.unit(actor_id)` (UnitView Deref → engine `Unit.hp`).
- Никаких других ai_scenarios регрессий — 43/43 combat tests green
  (44 − U1 deletion).

### Влияние на оставшиеся U-фазы

- **U5** — scope сужается. `snap.units` теперь полностью неиспользуемый
  data store в sim path (frozen, никто не пишет, no-one in production code
  читает кроме pre-decision callers в system.rs / influence.rs / aoe_hits.rs).
  Удаление поля в U5 затронет: bin shims (replay_ai_log, mine_ai_logs —
  уже на shim), pre-decision readers (3 файла), `snapshot_from`/`as_pair`
  shim в tests (всё ещё есть, deletable). Plus schema flip v37→v38.
- **U6** — без изменений (delete `UnitSnapshot` struct).
- **U7** — без изменений.

### Lessons learned

1. **Plan agent's R-F call**: «verify by reading sim.rs:1240-1265 before
   declaring R-F closed» сработала — там действительно `hp` assertion,
   и переписать через `unit()` accessor оказалось trivially.
2. **Pragmatic deviation OK когда invariant сохранён**: agent держал
   `combat_state.clone() per-step` вместо архитектурно чистого решения
   с `SimState::unit()` — приняли потому что `snap.units frozen`
   invariant из spec сохранён, тесты зелёные, golden byte-identical.
3. **«Critical: serialization safety» analysis в брифе** оправдал себя:
   implementer не паниковал когда golden_smoke сразу зелёный, потому что
   путь был обоснован заранее.

---

## Phase plan (re-split)

Линейная цепочка `U0 → U1 → U2 → U3 → U4 → U5 → U6`, две опциональные
ответвки `U7`/`U8` от `U6`.

### U0 — Scenario corpus expansion (hard precondition)

**Цель**: расширить ai_scenarios до плотности, при которой удаление back-projection
не пропустит поведенческий сдвиг.

**Почему до всего**: текущий corpus — 8 scenarios + 1 golden_smoke baseline.
Покрывает только ранее найденные регрессии (rescue_ally, taunt). Phase U4
(удаление `project_engine_to_snapshot`) может ввести сдвиги в edge cases
(death/revive ordering, status stacking, AoE friendly fire), которые тихо
пройдут без новых сценариев.

**Что добавляем (8–12 сценариев)**:
- Multi-status interactions: stun + dot + buff в одной цели, порядок применения.
- Death triggers: lethal damage очищает statuses, on_death эффекты.
- Reactions chain: multi-AoO + counter-attack ordering.
- Resource caps: mana overflow, rage overflow, AP underflow.
- AoE friendly fire + reservations: cluster spell с союзником в радиусе.
- **Crit-fail under ExpectedValue**: пин детерминированного outcome'а
  (mitigates R1 — branchy code, который collapsed под expected mode).
- Status order canonical: `apply_status` идемпотентность при двойном вызове.

**Exit**: новые сценарии в `tests/ai_scenarios/`; `tests/baselines/baseline_v37.jsonl`
регенерирован и diff проверен вручную. CI зелёный.

---

### U1 — Parity guard: engine vs back-projection

**Цель**: формализовать инвариант, что для любого `PlanStep` после `apply_step`
выполняется `state.units[i] ≡ project_engine_to_snapshot(state)[i]` по всем
полям, которые мы потом собираемся удалить.

**Почему отдельной фазой**: U4 удаляет проекцию. Если расхождение есть прямо
сейчас (порядок statuses, derived bonuses), оно всплывёт после удаления как
golden-сдвиг — а должно всплыть **до** удаления как failing test.

**Что делаем**:
- Тест `parity_engine_vs_units_after_apply_step` параметризованный по
  `PlanStep::{Cast, Move, EndTurn}` × {target alive / dead / self} × {with
  status / without}. Прогоняет один шаг, сравнивает `state.units[i]` и
  `snapshot.units[i]` по hp / ap / mp / position / statuses / armor_bonus /
  speed / damage_taken_bonus.
- Если расхождение — фиксим в `project_engine_to_snapshot` (или в `as_pair`),
  не в тесте.
- Канонизируем порядок statuses в обеих ветках (engine + snapshot) — иначе
  U4 потянет golden-чёрн.

**Exit**: parity test зелёный для всех `PlanStep` вариантов; clippy clean.

---

### U2 — Callsite migration (mechanical, основной cascade)

**Цель**: перевести reader-callsites с `snap.unit_snapshot(e) -> &UnitSnapshot`
на `snap.unit(e) -> UnitView`. Это шаг подготовки к удалению `units`.

**Главный cascade — сигнатуры**:
- `ScoringCtx.active: &UnitSnapshot` → `UnitView<'_>` (≥10 функций scoring layer).
- `appraisal_ctx::active`, `update_memory`, `assign_band`, `build_agenda`,
  `select_intent`, `unit_value`, `plan_has_self_rescue`, `process_action` — то же.

**Подводный камень** (заметил аудит): `UnitView::is_stunned` / `forces_targeting`
требуют `&StatusTagCache` параметром (см. `snapshot.rs:282-295`). Сейчас многие
функции владеют `&UnitSnapshot` и зовут `.is_stunned(tags)` локально. После
миграции им нужен `UnitView` **плюс** `tags`. Решение: первым коммитом U2
протянуть `&StatusTagCache` в `ScoringCtx`; последующие callsites станут drop-in.

**Что не трогаем в U2**:
- `outcome::builder` / `aggregate.rs` callsites с `.cloned()` для последующих
  мутаций — это test fixtures и replay/mining; их перенесём в U3/U5.
- Бинарники `replay_ai_log` / `mine_ai_logs` — у них своё schema-чтение.

**Exit**: `BattleSnapshot::unit_snapshot()` accessor удалён;
`cargo test --lib`, `ai_scenarios`, `golden_smoke` зелёные.

---

### U3 — Test-fixture migration

**Цель**: убрать `UnitSnapshot { … }` literal-конструкции из тестов scoring /
policy / aggregate / parity.

**Почему отдельной фазой**: production callsites уже мигрированы (U2),
но `grep 'UnitSnapshot {' src/combat/ai` всё ещё возвращает много в тестах.
Эти fixtures конструируют `UnitSnapshot` через `..Default::default()` и крутят
ручные поля — если оставить, U5 (удаление поля `units`) поломает компиляцию
тестов массово.

**Что делаем**: заменить fixture builders. Скорее всего вводим
`UnitBuilder::new(...).hp(...).statuses(...).build() -> (engine::Unit, UnitAiCache)`
с симметричным API. Делаем в `tests/common/builders.rs` и postimport'ом.

**Exit**: `grep -r 'UnitSnapshot {' src/combat/ai` возвращает только
`snapshot.rs` (определение) и `log/legacy_v37.rs` (deserializer).

---

### U4 — Drop sim back-projection

**Цель**: удалить `project_engine_to_snapshot` и `snapshot_to_combat_state`;
`SimState` больше не трогает `snap.units`.

**Что делаем**:
- `SimState::actor_unit()` возвращает engine `&Unit` (из `state.units()`),
  не `&UnitSnapshot`.
- `apply_step` не вызывает `project_engine_to_snapshot` после `step()`.
- `from_snapshot` не строит `CombatState` из `snapshot.units` — вместо этого
  клонирует `snap.state` (поле появится здесь же).

**Опасность**: golden corpus чувствителен к порядку statuses внутри
`UnitSnapshot.statuses`. Если канонизировали порядок в U1 — diff будет нулевой.
Если нет — будут шумовые расхождения, которые надо отделить от настоящих.

**Exit**: `sim.rs` не содержит `snap.units` references; `golden_smoke`
**byte-for-byte идентичен** post-U1 baseline; `ai_scenarios` зелёные.

---

### U5 — Drop duplicate fields + schema flip

**Цель**: удалить `BattleSnapshot.units`, `by_entity`, `round`; в этом же
коммите-серии bump schema v37 → v38 и регенерация baselines.

**Что делаем**:
- `BattleSnapshot { state, cache }` — два поля.
- `BattleSnapshotRepr` упрощается до того же; serializer пишет `{state, cache}` natively.
- `rebuild_index` ветка deserializer'а уходит.
- `unit_snapshots_to_combat_state` удалён.
- `snap.round` → `snap.state.round` все callsites (тривиальный sed).
- Бинарники `replay_ai_log` и `mine_ai_logs` мигрируют здесь же — у них
  `snap.units.iter()` direct callsites; меняем на `snap.state.units()` +
  `snap.cache.units`.
- `SCHEMA_VERSION = 38`. **Clean break**: v37 логи дают `UnsupportedSchema`,
  не migration shim. Прецедент — schema v27→v28 (step 4, outcome vector).
- **Регенерация**: `golden_*.jsonl`, `tests/baselines/baseline_v38.jsonl`,
  `tests/ai_scenarios/snapshots/*/log.jsonl`. Diff-review каждой регенерации
  — не «слепая» перегенерация. Сценарий за сценарием: ожидаемое (структурное)
  отделяем от подозрительного (поведенческое расхождение).
- **Legacy markers**: в `snapshot.rs` уже стоят `// LEGACY: …` коментарии
  на двух точках (`UnitSnapshot::as_pair` line ~465, `BattleSnapshot::new`'s
  `summoner` mapping line ~789) — оба удаляются на этом шаге вместе с
  `UnitSnapshot::summoner` field. Поиск по коду: `LEGACY: shortcut valid`.

**Exit**: `BattleSnapshot` имеет 2 поля; `cargo check`, бинарники собираются;
все ai_scenarios + golden_smoke проходят на v38 baseline.

---

### U6 — Delete `UnitSnapshot`

**Цель**: убрать тип `UnitSnapshot` полностью из codebase.

**Что делаем**:
- Удаляем `struct UnitSnapshot`, `BattleSnapshotRepr` v36/v37 ветку,
  `as_pair`, `unit_snapshots_to_combat_state`.
- Shims `UnitSnapshot::is_stunned`, `forces_targeting`, `add_status`,
  `remove_status`, `refresh_aggregates`, `apply_status_change`, `statuses_mut`,
  `compute_status_delta` (ActiveStatusView вариант), `status_hash`
  (ActiveStatusView вариант) — удаляются.
- `compute_status_delta_engine` / `status_hash_engine` переименовываются
  обратно без `_engine` суффикса (единственная версия).

**Exit**: `grep -r 'UnitSnapshot' src/` возвращает пусто.

---

### U7 — (опционально, тривиально после bridge work) Dedup `UnitAiCache.entity`

**Цель**: убрать единственное поле в `UnitAiCache`, дублирующее engine.

Аудит показал: только `entity` действительно duplicates `Unit.id` (через
`UnitId(entity.to_bits())`). `abilities` и `caster_ctx` в кеше **не**
дублируют engine — engine хранит `caster_context` (singular) и не хранит
abilities (они в `ContentView`). Q2 предыдущей версии плана был неверен.

**Что делаем**: заменить `cache.unit(entity)` lookup callsites на
`snap.uid_for_entity(entity).and_then(|uid| cache.unit_by_uid(uid))`. Поле
`UnitAiCache.entity` удалить. Добавить `AiCache::unit_by_uid(uid)` symmetric
с существующим `unit(entity)` (или совсем заменить — keyed by UnitId).

**Post-bridge упрощение** (B-prime, `01676a8`): `BattleSnapshot` уже имеет
`entity_to_uid` map и публичный `uid_for_entity` accessor. Не нужно
ни UnitIdMap-derive-on-demand, ни нового infrastructure. Миграция —
mechanical sed по callsite-ам `cache.unit(entity)` → новый паттерн.

**Можно слить с U6 одним коммитом** — обе фазы удаляют дублирующие поля,
изменения mechanical.

**Exit**: компиляция; `cargo test --lib` зелёный; `UnitAiCache` без поля `entity`.

---

## Зависимости

```
U0 ── U1 ── U2 ── U3 ── U4 ── U5 ── U6 ── U7 (optional)
```

`U5` объединяет structural cleanup (drop полей) и schema flip (v37→v38 +
регенерация baselines) одним cut'ом — legacy reader не нужен.

---

## Resolved open questions

- **Q1 (2A vs 2B)**: снят. Engine `step()` уже используется; AI sim уже работает
  через `DiceSource` + `ExpectedValue`. Pick 2A *de facto*; падать обратно
  на direct `apply_effect` стоит только если нужен sim-only shortcut (skip
  legality, skip events) — пока такого нет.
- **Q2 (UnitAiCache duplicate fields)**: только `entity`. `abilities` /
  `caster_ctx` намеренно живут в AI cache, потому что engine Unit их не имеет
  в той же форме. Cleanup scope = 1 field, в U7.
- **Q3 (corpus expansion)**: hard precondition (U0), не open question.
- **Q4 (schema bump orthogonality)**: снят. Пользователь подтвердил clean
  break OK — schema flip входит в U5 одной транзакцией с удалением полей,
  legacy adapter не нужен.

---

## Risks & mitigations

| ID | Риск | Mitigation |
|---|---|---|
| R1 | RNG boundary: branchy engine code детерминируется под `ExpectedValue` (например, crit-fail = roll(1)). | U0 сценарий `crit_fail_path_under_expected_value`. Если нашли расхождение — фиксим в engine, не в AI. |
| R2 | `UnitView::is_stunned` требует `&StatusTagCache` — C2 cascade больше «mechanical». | Первый коммит U2 протягивает `&StatusTagCache` в `ScoringCtx`. Последующие callsites — drop-in. |
| R3 | `replay_ai_log` / `mine_ai_logs` ломаются на U5 (direct `snap.units` access). | U5 включает migration этих бинарников на `state.units()` + `cache.units`. Старые v37 логи невалидны — это accepted (clean break). |
| R4 | Golden churn на U4 из-за порядка statuses в `UnitSnapshot.statuses`. | U1 канонизирует порядок в обеих ветках; verify golden delta = 0 перед U4. |
| R5 | `snap.round` дублирует `state.round` — нечаянная dependency в логах. | Dedicated micro-commit в U5: `snap.round` → `snap.state.round` повсюду (sed-able). |

---

## Backlog (после U6, orthogonal)

**B1. Organic logging redesign.** После U6 формат лога — прямой dump
`{state: CombatState, cache: AiCache, annotation: PlanAnnotation}`. Это уже
органичнее текущего, но возможны дальнейшие шаги:

- **Event-stream logging** — вместо snapshot-per-tick писать стрим engine
  Events + AI decisions; восстанавливать state-at-tick через `apply_events`
  при replay. Pro: компактнее, replay-friendly. Con: меняет `replay_ai_log`
  и `mine_ai_logs` структурно.
- **Delta-logging** — только diff против предыдущего tick'а. Pro: меньше
  места. Con: golden-diff тяжелее читать.
- **Engine native serialization** — использовать `combat_engine::Snapshot`
  (если появится в engine) вместо AI-обёртки; AI логирует только `cache`
  + `annotation`. Pro: AI и engine разделены полностью. Con: требует engine
  работы.

Делаем только если из mining workflow выяснится конкретная боль с текущим
форматом. Не блокирует ничего.

**B2. Removal of UnitView<'_> abstraction.** После U6 `UnitView` может
стать тривиальной парой `(&'a Unit, &'a UnitAiCache)`. Если так — заменить
struct на tuple alias или просто разнести параметры в сигнатурах. Скорее
всего не стоит — `UnitView` даёт удобный `Deref` к engine `Unit` + точку
расширения для будущих derived полей.

---

## Чего не делаем

- **Не унифицируем AI ↔ engine RNG.** AI остаётся deterministic, engine —
  random. Boundary через `DiceSource` trait уже чистый.
- **Не объединяем sim path.** AI всегда работает с copy-on-write state +
  expected values; engine — с authoritative state + real RNG.
- **Не трогаем `cache::build`**. Логика построения cache самостоятельна.
- **Не переписываем engine.** Pure mutation API уже extract'нут.
- **Не держим legacy v37 reader.** Schema flip в U5 — clean break;
  старые логи невалидны.

---

## Связанные документы

- `docs/ai/rework/step_unisim*_plan.md` — предыдущие шаги unification (1–7).
- `docs/ai/rework/index.md` — общая навигация.
- `docs/ai/extension-checklist.md` § SCHEMA_VERSION bump — процедура для U7.
- `docs/engine-architecture.md` — canonical post-unisim layout.

## Критические файлы

- `src/combat/ai/plan/sim.rs` — `SimState`, `apply_step`, `project_engine_to_snapshot`.
- `src/combat/ai/world/snapshot.rs` — `BattleSnapshot`, `UnitSnapshot`, `UnitView`.
- `src/combat/ai/world/cache.rs` — `UnitAiCache` (Q2 ground truth).
- `crates/combat_engine/src/state.rs` — `CombatState`, `Unit`, `UnitId`.
- `crates/combat_engine/src/step.rs` — engine `step()` entry point.
- `crates/combat_engine/src/effect.rs` — `apply_effect`, `apply_status`, `remove_status`.
- `src/bin/replay_ai_log.rs`, `src/bin/mine_ai_logs.rs` — out-of-tree consumers
  schema (важны в U5).

---

## Откуда родилось переосмысление

Исходный план объявлял Phase E «большим архитектурным шагом» и оставлял
ключевую развилку Q1 (2A vs 2B) как открытый вопрос. Аудит `sim.rs:1-80`
показал, что Phase E уже сделана в коммите `3d7a14b` и предшествующих:
`SimState.combat_state` уже единственное место мутации, `apply_step` уже
зовёт `combat_engine::step()`. Открытым осталось не «как сшить два мира»,
а «как удалить теневую копию, которая больше не нужна». Это меняет:

1. **Front-loading risk**: вместо «отложить Phase E пока не решим Q1»
   делаем corpus expansion (U0) — единственный реальный риск-mitigator.
2. **Order**: parity guard (U1) идёт **до** callsite migration (U2),
   чтобы не маскировать сдвиги при U4.
3. **Test fixtures**: получают отдельную фазу (U3), потому что они
   составляют большую часть оставшихся `UnitSnapshot { … }` literal'ов
   и блокируют U5.
4. **Binaries**: явная миграция в U5, не «потом разберёмся».
5. **Schema bump (U7)** и **dedup cache.entity (U8)** — orthogonal cleanups,
   правильно отделены от core path.
