# Wave-3 plan — per-trap ownership + per-team reveal + AI hazard avoidance

Зафиксирован 2026-06-01. Закрывает оставшиеся AI-пункты главы II: модель знания о
ловушках и мягкое избегание их AI. Источник решений: `engine-requirements.md`
(решения #2 «knowledge model» и #4 «severity»), переработанные под уточнённую
модель владельца (per-trap ownership вместо per-unit `knows_env`).

Прошёл planner + critic (вердикт **APPROVE WITH CHANGES**); правки критика вложены
в раздел «Locked fixes». Формат — как `wave-1-plan.md`.

---

## Цель (3 пункта владельца)

1. **Движок знает, чья каждая ловушка** — per-trap ownership. Команда-владелец
   видит свои ловушки сразу. (На будущее: юнит сможет ставить ловушки сам →
   владелец задаётся при spawn.)
2. **Другая команда видит только раскрытые ЕЙ ловушки.** Пассивка-раскрыватель,
   принадлежащая команде-владельцу, НЕ должна раскрывать ловушку противнику.
3. **AI получает мир, отфильтрованный по его видимости** — все ловушки, видимые
   его команде (свои + раскрытые ей), без ещё не раскрытых чужих.

---

## Модель данных (LOCKED)

`EnvObject.revealed: bool` заменяется на:

```rust
pub struct EnvObject {
    pub id: EnvId,
    pub hex: hexx::Hex,
    pub kind: EnvKind,
    pub ability: AbilityId,
    pub owner: Option<Team>,     // None = нейтральная (ничья) ловушка
    pub revealed_to: TeamSet,    // какие команды её обнаружили
}

pub struct TeamSet { pub player: bool, pub enemy: bool }  // НЕ HashSet — детерминизм serde

impl EnvObject {
    pub fn visible_to(&self, team: Team) -> bool {
        self.owner == Some(team) || self.revealed_to.contains(team)
    }
}
```

- `owner == Some(T)` → команда T видит всегда (пункт 1).
- иначе видна только если `T ∈ revealed_to` (пункт 2).
- `owner == None` → ничья: видна команде только после раскрытия ей; **срабатывает
  на ОБЕ команды** (как текущие демо-`spike_trap`).
- **Per-unit `knows_env` НЕ вводится** — видимость целиком выводится из EnvObject.

### Reveal-семантика (решение 2)

`Effect::RevealEnv { id }` → `{ id, revealer: Team }`. Arm `RevealEnvInRange { caster, range }`
читает `caster_team = state.unit(caster)?.team` и штампует его в каждый порождённый
`RevealEnv`. Apply делает `revealed_to.insert(revealer)` (единственная мутация —
никогда не вставляет обе команды). Владелец, вставляя свою же команду, не меняет
видимость для противника ⇒ пункт 2 выполняется автоматически.

### Firing — visibility-agnostic

Срабатывание ловушки (`step.rs` ~819, на `MovePosition`) НЕ зависит от видимости:
юнит наступает на невидимую ему ловушку — она срабатывает. Полный `environment`
живёт в реальном `CombatState`; только AI-СНАПШОТ получает отфильтрованную копию.

---

## Locked fixes (правки критика — обязательны)

1. **T8 tie-break.** Среди путей равной длины и равного штрафа — детерминированный
   порядок предшественника по координатам гекса `(hex.x, hex.y)` лексикографически,
   НЕ итерация HashMap. Записанный путь (`PlanStep::Move { path }`) идёт в trace —
   иначе replay-недетерминизм.
2. **T7 neutral reference.** Prod-конструктор `UnitSnapshot::neutral_reference()`
   (НЕ `#[cfg(test)]`). Документировать ВСЕ поля, что читает `policy::status::value`:
   `max_hp`, **`threat`** (драйвит stun/silence через `horizon_window_sum` fallback,
   `status.rs:24`), `damage_horizon`. Значения — именованные `const` с обоснованием.
3. **Goal #2 doc.** На `Effect::RevealEnv` — «раскрывает только команде `revealer`,
   никогда оппоненту».
4. **Neutral.** Отсутствие `owner` в TOML = `None` (не паника). Нейтральная ловушка
   бьёт обе команды. Нейтральный кейс — в e2e (T10).
5. **T6 must-verify.** Убедиться, что `replay_ai_log --capture-golden` регенерит
   фикстуры из REPLAY СЦЕНАРИЯ, а не ресериализацией старых файлов (иначе цикл).

---

## Тикеты

### Трек видимости

**T1 — `EnvObject` model + `TeamSet` + `visible_to`.**
- `crates/combat_engine/src/state.rs`: `TeamSet` рядом с `Team`; в `EnvObject`
  убрать `revealed`, добавить `owner`/`revealed_to`; `impl visible_to`; удалить
  устаревший коммент «never constructed» у `EffectSource::Env` (:64-68) и про
  `revealed` (:50-52).
- Тесты (serde + visibility): owner-always-visible; visible-after-insert;
  neutral-unrevealed-invisible-both; teamset-deterministic-serde; roundtrip.

**T2 — reveal протягивает команду.**
- `effect.rs`: `RevealEnv { id, revealer }`; arm `RevealEnvInRange` читает
  `caster_team`, штампует; apply `revealed_to.insert(revealer)`; scan-guard
  (:295) `!e.revealed` → `!e.visible_to(caster_team)`; `Event::EnvRevealed`
  (+`revealer` для симметрии/replay-атрибуции — подтвердить в `event.rs`).
- Тесты: reveal-inserts-only-casters-team; owner-reveal-no-leak-to-opponent;
  reveal-idempotent-per-team; миграция существующих trap-тестов с `.revealed`
  на `visible_to`.

**T3 — фильтр снапшота по команде.**
- `snapshot.rs`: `build_snapshot` (+`ai_team: Team`); :486 `retain(|e| e.visible_to(ai_team))`.
- `system.rs`: передать `actor_team` (уже связан :143) в вызов :144.
- Тесты: includes-own-team-owned; excludes-enemy-owned-unrevealed;
  includes-revealed-to-ai-team; neutral-unrevealed-absent-both.

**T4 — UI рендер по `visible_to(Team::Player)`.**
- `visuals.rs` :355-356 `e.revealed` → `e.visible_to(Team::Player)`.
- Тесты: renders-player-owned-immediately; hides-enemy-owned-unrevealed.
- DoD: grep `.revealed` чист в `src/`.

**T5 — TOML `owner`.**
- `encounters.rs`: `EnvRecord` (+`#[serde(default)] owner: Option<String>`),
  `EnvObjectDef` (+`owner: Option<Team>`); резолвер `"player"/"enemy"` →
  `Some`, absent → `None`, unknown → panic с именем плохого значения.
- `combat_scene.rs` :54-60: `owner: def.owner, revealed_to: TeamSet::EMPTY`.
- Тесты: parse-player; parse-enemy; absent-is-neutral-none; unknown-panics;
  старые TOML без `owner` парсятся.

**T6 — schema bump 45→46 + регенерация. ★ checkpoint.**
- Обе константы: `trace.rs:64` и `ai/log/mod.rs:235` → 46 (AI-лог встраивает
  весь `BattleSnapshot`, `mod.rs:1297` → `CombatState.environment`).
- Clean break + регенерация: переименовать `baseline_v44.jsonl` → `baseline_v46.jsonl`;
  recapture фикстур `tests/ai_scenarios/snapshots/*`; `measurements/*.jsonl`.
- Команда: `cargo run --release --bin replay_ai_log -- --capture-golden tests/baselines/baseline_v46.jsonl tests/ai_scenarios/snapshots/*/log.jsonl`
  (СНАЧАЛА подтвердить источник capture — см. Locked fix #5).
- Обновить `docs/ai/extension-checklist.md`.
- Тесты: env-owner-revealed_to-roundtrip; parse-v45-returns-unsupported;
  golden-baseline-zero-diff. **Полный `cargo nextest run --workspace --features dev`.**

### Трек избегания

**T7 — `AiCache.env_severity: HashMap<EnvId,f32>` (unit-independent).**
- `cache.rs`: `AiCache` (+`#[serde(default)] env_severity`).
- `snapshot.rs`: в `build_snapshot` (ContentView в scope) после фильтра (:486)
  посчитать severity по видимым env, учитывая bridge-side override-resource.
- severity = `expected_damage_avg` + `status_cost`:
  - `expected_damage_avg`: `ability` → `AbilityDef` → match `EffectDef`
    (9 engine-вариантов; `Damage{dice}|SpellDamage{dice}` → `dice.expected()`;
    остальное → 0; exhaustive, без паники; `SelfDamage` в engine-enum НЕТ).
  - `status_cost`: `policy::status::value(def, &neutral_ref, content)` —
    `neutral_ref = UnitSnapshot::neutral_reference()` (Locked fix #2).
  - `severity_override` (bridge-side `HashMap<EnvId,f32>`) короткозамыкает.
- Тесты: damage=dice.expected; non-damage=status-only; unit-independent (две
  разно-статовых актёра → одинаковая severity); override-short-circuits;
  only-for-team-visible-envs.

**T8 — `MovementEnv.hazard_costs` + BFS-сохраняющая реконструкция.**
- `pathfinding.rs`: `MovementEnv` (+`hazard_costs: HashMap<Hex,f32>`, default пусто).
  `reach_from` при `hazard_costs.is_empty()` — точный текущий FIFO BFS
  (байт-в-байт `ReachableMap`). При непустом — отдельная ветка: множество
  достижимых = тот же BFS-фронт (hop-count ≤ MP, reachability не меняется),
  меняется только реконструкция пути — минимум накопленного штрафа среди путей
  равной длины, ties по `(hex.x, hex.y)` (Locked fix #1).
- Тесты: empty-byte-identical-to-legacy-bfs (pin); reroutes-equal-length;
  still-reachable-when-only-option; does-not-expand-reachable-set;
  equal-penalty-tie-deterministic.

**T9 — AI wiring + UI пусто.**
- `reach.rs` :30: `hazard_costs` из `snap.state.environment` (уже отфильтрован
  T3) ↦ `snap.cache.env_severity[id]`; 3 AI-вызова идут через эту fn.
- `visuals.rs` :295: `hazard_costs: HashMap::new()` + громкий коммент.
- Тесты: ai-populates-from-snapshot; empty-when-no-env; ui-always-empty (pin);
  ai-sim-and-prod-agree (property).

**T10 — e2e.**
- `tests/env_ownership_e2e.rs` + фикстура bout-1 с `owner="enemy"`.
- Тесты: enemy-owned-visible-to-enemy-not-player; enemy-ai-soft-avoids-own-visible;
  player-steps-on-hidden-enemy-trap-it-FIRES (visibility-agnostic); neutral-fires-both;
  parity-property.
- Документировать residual flee-AI limitation (party-AI, симулируя врага,
  использует свой отфильтрованный снапшот → не моделирует, как враг обходит
  СВОИ скрытые ловушки; граница: только enemy-owned, ещё не раскрытые игроку).

---

## DAG

```
T1 ─► {T2, T3, T4, T5, T6}
T7 ◄─ {T1, T3}
T8
T9 ◄─ {T7, T8}
T10 ◄─ {T2..T9}
```
Порядок мерджа: T1 → T6 (checkpoint) → T2 → T3 → T4 → T5 → T7 → T8 → T9 → T10.
(На практике T1–T5 идут одной волной — крейт не компилируется, пока консьюмеры
shape-изменения не обновлены; T6 — отдельной волной.)

## Checkpoints (полный `cargo nextest run --workspace --features dev`)
- После T6 (schema bump).
- После T9 (трек избегания).
- После T10 (вся фича).

## Риски
| Тикет | Риск | Митигация |
|---|---|---|
| T1 | `HashSet<Team>` → недетерминизм trace-hash | locked `TeamSet`; deterministic-serde тест |
| T2 | site забыл `revealer` | compile-error на конструкции; goal#2 leak-тест |
| T3 | пропущен не-`run_ai_turn` caller | подтверждено: 1 prod-вызов; новый param → compile-error |
| T4 | остался `e.revealed` в src | grep `.revealed` чист перед DoD |
| T6 | бампнули только engine-константу | AI-лог встраивает снапшот → обе =46; clean-break тест |
| T6 | фикстуры несут старый shape | recapture фикстур + baseline вместе; zero-diff guard |
| T7 | target-зависимая severity vs общий кэш | neutral-reference; unit-independent тест |
| T7 | non-exhaustive `EffectDef` | exhaustive match, новый вариант → compile-error |
| T8 | weighted-путь ломает replay | empty=байт-в-байт BFS (pin); ties по координатам |
| T9 | UI авто-обход | UI `hazard_costs` пуст + коммент + regression-тест |
| T10 | ловушка не срабатывает «т.к. не видна» | firing использует полный environment, не снапшот |
