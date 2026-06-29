//! Waveform access via [wellen](https://github.com/ekiwi/wellen).
//!
//! Phase 0 scope: open a trace (VCD/FST/GHW, auto-detected) and walk its scope
//! hierarchy so the matcher (Phase 1) can map scope paths to NodeIds. Signal
//! value loading stays lazy (wellen's per-signal model); we don't pull bodies.

use anyhow::{Context, Result};
use wellen::simple::Waveform;
use wellen::{Hierarchy, LoadOptions, SignalRef, VarType};

/// A single value change: simulation time and the value as a string.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ValueChange {
    pub time: u64,
    pub value: String,
}

/// The trace's timescale: each raw `ValueChange::time` tick equals `factor` of
/// `unit`. `unit` is a normalized short string (`"fs"`, `"ps"`, `"ns"`, `"us"`,
/// `"ms"`, `"s"`, …; `""` when unknown) so the frontend can scale time labels.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceTimescale {
    pub factor: u32,
    pub unit: String,
}

fn unit_str(u: wellen::TimescaleUnit) -> &'static str {
    use wellen::TimescaleUnit::*;
    match u {
        ZeptoSeconds => "zs",
        AttoSeconds => "as",
        FemtoSeconds => "fs",
        PicoSeconds => "ps",
        NanoSeconds => "ns",
        MicroSeconds => "us",
        MilliSeconds => "ms",
        Seconds => "s",
        Unknown => "",
    }
}

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

/// One waveform variable, flattened for the matcher.
///
/// `var_ref` is wellen's stable per-variable handle (as a small int); it is the
/// key the matcher uses for the node ↔ signal index. `signal_ref` is the handle
/// for lazily loading values later (Phase 2+). Multiple vars can alias one
/// `signal_ref`, so the matcher keys on `var_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveVar {
    pub var_ref: u32,
    pub signal_ref: u32,
    pub full_name: String,
    pub is_parameter: bool,
}

fn is_parameter(t: VarType) -> bool {
    matches!(
        t,
        VarType::Parameter | VarType::RealParameter | VarType::EventParameter
    )
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

    /// Every variable flattened to a [`WaveVar`] — the matcher's input.
    pub fn signals(&self) -> Vec<WaveVar> {
        let h = self.wave.hierarchy();
        h.all_vars()
            .map(|vr| {
                let v = &h[vr];
                WaveVar {
                    var_ref: vr.index() as u32,
                    signal_ref: v.signal_ref().index() as u32,
                    full_name: v.full_name(h),
                    is_parameter: is_parameter(v.var_type()),
                }
            })
            .collect()
    }

    /// Direct access to the wellen hierarchy (for callers that need more).
    pub fn hierarchy(&self) -> &Hierarchy {
        self.wave.hierarchy()
    }

    /// The trace's timescale (tick → physical time), if the format declares one.
    pub fn timescale(&self) -> Option<TraceTimescale> {
        self.wave.hierarchy().timescale().map(|ts| TraceTimescale {
            factor: ts.factor,
            unit: unit_str(ts.unit).to_string(),
        })
    }

    /// Load a signal (by its wellen signal-ref index) and return its value
    /// changes over the whole trace as (time, value-string) pairs. Lazy: only
    /// the requested signal is materialized.
    pub fn signal_values(&mut self, signal_ref: u32) -> Vec<ValueChange> {
        let Some(sr) = SignalRef::from_index(signal_ref as usize) else {
            return Vec::new();
        };
        self.wave.load_signals(&[sr]);
        let times = self.wave.time_table().to_vec();
        match self.wave.get_signal(sr) {
            Some(sig) => sig
                .iter_changes()
                .filter_map(|(idx, val)| {
                    times.get(idx as usize).map(|&t| ValueChange {
                        time: t,
                        value: val.to_string(),
                    })
                })
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn summary(&self) -> WaveSummary {
        WaveSummary {
            scopes: self.scope_count(),
            vars: self.var_count(),
            file_format: format!("{:?}", self.wave.hierarchy().file_format()),
        }
    }
}
