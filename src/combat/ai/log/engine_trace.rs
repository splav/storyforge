//! Engine trace writer (Phase 5 D6/D11). Writes one JSONL file per combat
//! into `logs/<fight_id>/engine.jsonl`, independent of the AI-decision log.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use bevy::prelude::*;
use combat_engine::action::Action;
use combat_engine::event::Event;
use combat_engine::trace::{self, InitLine, StepLine, SCHEMA_VERSION};

#[derive(Resource, Default)]
pub struct EngineTraceWriter {
    writer: Option<BufWriter<File>>,
    step_counter: u64,
}

impl EngineTraceWriter {
    /// Open (or re-open) the trace file at `path`. Parent dir must already exist.
    pub fn open(&mut self, path: &Path) -> std::io::Result<()> {
        let file = File::create(path)?;
        self.writer = Some(BufWriter::new(file));
        self.step_counter = 0;
        Ok(())
    }

    /// Flush and close the writer (called on `OnExit(AppState::Combat)`).
    pub fn close(&mut self) {
        if let Some(mut w) = self.writer.take() {
            let _ = w.flush();
        }
    }

    pub fn is_open(&self) -> bool {
        self.writer.is_some()
    }
    pub fn step_counter(&self) -> u64 {
        self.step_counter
    }

    /// Write the `init` line once at combat start.
    pub fn write_init(&mut self, line: &InitLine) -> std::io::Result<()> {
        let Some(w) = self.writer.as_mut() else {
            return Ok(());
        };
        let json = trace::serialize_init(line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(w, "{json}")?;
        w.flush()
    }

    /// Record a single engine `step()` call. Assigns the step index and
    /// increments the internal counter. Write order: BEFORE any ECS projection
    /// so a downstream panic can't corrupt the trace.
    pub fn write_step(
        &mut self,
        action: &Action,
        events: &[Event],
        rng_calls: u64,
        post_state_hash: String,
    ) -> std::io::Result<()> {
        let Some(w) = self.writer.as_mut() else {
            return Ok(());
        };
        let line = StepLine {
            schema: SCHEMA_VERSION,
            step: self.step_counter,
            action: action.clone(),
            events: events.to_vec(),
            rng_calls,
            post_state_hash,
        };
        let json = trace::serialize_step(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(w, "{json}")?;
        w.flush()?;
        self.step_counter += 1;
        Ok(())
    }
}
