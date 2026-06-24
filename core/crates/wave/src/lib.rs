//! Waveform access via [wellen](https://github.com/ekiwi/wellen).
//!
//! Phase 0 scope: open a trace (VCD/FST/GHW, auto-detected) and walk its scope
//! hierarchy so the matcher (Phase 1) can map scope paths to NodeIds. Signal
//! value loading stays lazy (wellen's per-signal model); we don't pull bodies.

use anyhow::{Context, Result};
use wellen::simple::Waveform;
use wellen::LoadOptions;

/// A loaded waveform header (hierarchy known; signal bodies lazy).
pub struct LoadedWave {
    wave: Waveform,
}

/// Summary of a trace's hierarchy — the Phase 0 smoke output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveSummary {
    pub scopes: usize,
    pub vars: usize,
    pub file_format: String,
}

impl LoadedWave {
    /// Open a waveform file, auto-detecting the format from its contents.
    pub fn open(path: &str) -> Result<Self> {
        let wave = wellen::simple::read_with_options(path, &LoadOptions::default())
            .map_err(|e| anyhow::anyhow!("{e:?}"))
            .with_context(|| format!("opening waveform {path}"))?;
        Ok(Self { wave })
    }

    /// Number of scopes (modules/blocks/interfaces) in the hierarchy, recursively.
    pub fn scope_count(&self) -> usize {
        let h = self.wave.hierarchy();
        h.scopes().map(|sr| 1 + h[sr].all_scopes(h).count()).sum()
    }

    /// Number of variables (signals) in the hierarchy, recursively.
    pub fn var_count(&self) -> usize {
        self.wave.hierarchy().all_vars().count()
    }

    /// Fully-qualified hierarchical names of every variable in the trace.
    /// These are the scope paths the Phase 1 matcher normalizes and maps.
    pub fn var_full_names(&self) -> Vec<String> {
        let h = self.wave.hierarchy();
        h.all_vars().map(|vr| h[vr].full_name(h)).collect()
    }

    pub fn summary(&self) -> WaveSummary {
        WaveSummary {
            scopes: self.scope_count(),
            vars: self.var_count(),
            file_format: format!("{:?}", self.wave.hierarchy().file_format()),
        }
    }
}
