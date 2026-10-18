//! A debugger workspace: what the user set up for one ROM — labels,
//! breakpoints, watches, the front-end's window settings — kept between
//! sessions as Mesen2 keeps its per-ROM workspace.
//!
//! [`crate::Emulator::workspace`] takes it from the running session,
//! [`crate::Emulator::apply_workspace`] puts it back after a ROM load
//! (over the defaults and the `<rom>.mlb` the load brought in: what the
//! user set last wins). The file is JSON, one per ROM name, in the
//! settings folder's `debugger/` ([`workspace_path`]), so a homebrew ROM
//! rebuilt with other bytes keeps its workspace.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::labels::{Label, LabelStore};
use crate::memory::MemoryKind;
use crate::{ApiError, BreakCpu, BreakpointInfo, Emulator, WatchSpace};

/// The format of the file.
const VERSION: u32 = 1;

/// One ROM's debugger workspace.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Workspace {
    /// The file format's version.
    pub version: u32,
    /// The labels the user set (the defaults are not stored: a load puts
    /// them back).
    pub labels: Vec<Label>,
    /// Every breakpoint, with its condition, mark and state.
    pub breakpoints: Vec<BreakpointInfo>,
    /// Watch entries, by CPU (`snes`, `spc`, …).
    #[serde(default)]
    pub watches: BTreeMap<String, Vec<String>>,
    /// What a front-end keeps for itself (window settings), by its name.
    #[serde(default)]
    pub front_end: BTreeMap<String, serde_json::Value>,
}

/// The workspace file of a ROM named `rom` (its file name, or its header
/// title for a ROM loaded from bytes): `<settings>/debugger/<stem>.json`.
/// `None` when the environment names no settings folder.
#[must_use]
pub fn workspace_path(rom: &str) -> Option<PathBuf> {
    let stem = Path::new(rom)
        .file_stem()
        .map_or_else(|| rom.to_string(), |s| s.to_string_lossy().into_owned());
    let safe: String = stem
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    crate::paths::config_file(&format!("debugger/{safe}.json"))
}

impl Workspace {
    /// Read a workspace file.
    pub fn load(path: &Path) -> Result<Self, ApiError> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|e| ApiError::BadArg(format!("{}: {e}", path.display())))
    }

    /// Write it, creating its folder.
    pub fn save(&self, path: &Path) -> Result<(), ApiError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|e| ApiError::BadArg(e.to_string()))?;
        std::fs::write(path, text + "\n")?;
        Ok(())
    }

    /// Nothing worth a file.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
            && self.breakpoints.is_empty()
            && self.watches.values().all(Vec::is_empty)
            && self.front_end.is_empty()
    }
}

/// The bus a CPU's watchpoints are given on.
const fn bus_of(cpu: BreakCpu) -> MemoryKind {
    match cpu {
        BreakCpu::Snes => MemoryKind::SnesMemory,
        BreakCpu::Spc => MemoryKind::SpcMemory,
        BreakCpu::Sa1 => MemoryKind::Sa1Memory,
        BreakCpu::Gsu => MemoryKind::GsuMemory,
        BreakCpu::Dsp => MemoryKind::DspProgramRom,
    }
}

/// The memory a registry space stands for.
const fn memory_of(space: WatchSpace, cpu: BreakCpu) -> MemoryKind {
    match space {
        WatchSpace::Bus => bus_of(cpu),
        WatchSpace::Vram => MemoryKind::SnesVideoRam,
        WatchSpace::Cgram => MemoryKind::SnesCgRam,
        WatchSpace::Oam => MemoryKind::SnesSpriteRam,
        WatchSpace::Aram => MemoryKind::SpcRam,
        WatchSpace::Ipl => MemoryKind::SpcRom,
        WatchSpace::DspDataRam => MemoryKind::DspDataRam,
        WatchSpace::PrgRom => MemoryKind::SnesPrgRom,
        WatchSpace::WorkRam => MemoryKind::SnesWorkRam,
        WatchSpace::SaveRam => MemoryKind::SnesSaveRam,
    }
}

impl Emulator {
    /// The file the ROM was loaded from, `None` for one loaded from bytes.
    #[must_use]
    pub fn rom_file(&self) -> Option<&Path> {
        self.rom_file.as_deref()
    }

    /// The workspace file of the ROM loaded: by its file name, or its
    /// header title for a ROM loaded from bytes (see [`workspace_path`]).
    #[must_use]
    pub fn workspace_file(&self) -> Option<PathBuf> {
        let name = match &self.rom_file {
            Some(p) => p.file_name()?.to_string_lossy().into_owned(),
            None => self.rom_info.as_ref()?.title.trim().to_string(),
        };
        (!name.is_empty()).then(|| workspace_path(&name)).flatten()
    }

    /// The session's workspace: the labels the user set (not the
    /// defaults) and every breakpoint. Watches and front-end settings are
    /// the front-end's to add.
    pub fn workspace(&self) -> Result<Workspace, ApiError> {
        let defaults = LabelStore::with_defaults();
        let labels = self
            .labels
            .iter()
            .filter(|l| !defaults.iter().any(|d| d == *l))
            .cloned()
            .collect();
        Ok(Workspace {
            version: VERSION,
            labels,
            breakpoints: self.bp_list()?,
            ..Workspace::default()
        })
    }

    /// Put a workspace's labels and breakpoints back: the labels over
    /// those already there, the breakpoints added to the registry. Returns
    /// how many breakpoints could not be recreated (a CPU or a memory this
    /// cartridge lacks).
    pub fn apply_workspace(&mut self, ws: &Workspace) -> Result<usize, ApiError> {
        for l in &ws.labels {
            let _ = self.set_label(l.clone());
        }
        let mut missed = 0;
        for b in &ws.breakpoints {
            if self.bp_restore(b).is_err() {
                missed += 1;
            }
        }
        Ok(missed)
    }

    /// Recreate one breakpoint as `b` describes it.
    fn bp_restore(&mut self, b: &BreakpointInfo) -> Result<u32, ApiError> {
        let name = b.name.as_deref();
        let memory = memory_of(b.space, b.cpu);
        let id = if b.forbid {
            self.bp_add_forbid(b.cpu, memory, b.lo, b.hi, name)?
        } else if b.exec {
            self.bp_add_exec_on(b.cpu, b.lo, name)?
        } else if b.cpu == BreakCpu::Snes && b.space == WatchSpace::Bus {
            self.bp_add_mem(b.lo, b.hi, b.on_read, b.on_write, b.mirror, name)?
        } else {
            self.bp_add_watch(memory, b.lo, b.hi, b.on_read, b.on_write, name)?
        };
        if let Some(c) = b.condition.as_deref() {
            self.bp_set_condition(id, Some(c))?;
        }
        if b.mark {
            self.bp_set_mark(id, true)?;
        }
        if !b.enabled {
            self.bp_set_enabled(id, false)?;
        }
        Ok(id)
    }
}
