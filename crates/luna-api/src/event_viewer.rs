//! Faithful port of Mesen2's SNES Event Viewer data layer — categorisation,
//! default colors, register names, and config.
//!
//! Everything here is ported **verbatim** from Mesen2 master (the watchword is
//! fidelity); see `docs/event_viewer_reference.md` for the full study. Sources:
//! - categorisation: `Core/SNES/Debugger/SnesEventManager.cpp` `GetEventConfig`
//! - default colors: `UI/Config/Debugger/SnesEventViewerConfig.cs`
//! - register names: `UI/Debugger/Labels/DefaultLabelHelper.cs` `SetSnesDefaultLabels`

use luna_core::{DmaTraceEvent, MemEventKind, MemTraceEvent};

/// Number of Event Viewer categories (the `visible[]` mask width).
pub const CATEGORY_COUNT: usize = 18;

/// One Event Viewer category — mirrors Mesen2's `SnesEventViewerConfig`
/// categories. The variant order is the canonical index (0..18) used by
/// [`EventViewerConfig::visible`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCategory {
    /// PPU CGRAM register write (`$2121-$2122`).
    PpuCgramWrite,
    /// PPU VRAM register write (`$2115-$2119`).
    PpuVramWrite,
    /// PPU OAM register write (`$2101-$2104`).
    PpuOamWrite,
    /// PPU Mode 7 register write (`$211A-$2120`).
    PpuMode7Write,
    /// PPU BG-options register write (`$2105-$210C`).
    PpuBgOptionWrite,
    /// PPU BG-scroll register write (`$210D-$2114`).
    PpuBgScrollWrite,
    /// PPU window register write (`$2123-$212B`).
    PpuWindowWrite,
    /// Any other PPU register write (`$2100`, `$212C-$213F`).
    PpuOtherWrite,
    /// PPU register read (`$2100-$213F`).
    PpuRead,
    /// CPU register write (`$4000+`).
    CpuWrite,
    /// CPU register read (`$4000+`).
    CpuRead,
    /// APU I/O register write (`$2140-$217F`).
    ApuWrite,
    /// APU I/O register read (`$2140-$217F`).
    ApuRead,
    /// WRAM-port register write (`$2180-$2183`).
    WorkRamWrite,
    /// WRAM-port register read (`$2180-$2183`).
    WorkRamRead,
    /// NMI line raised.
    Nmi,
    /// H/V-timer IRQ line raised.
    Irq,
    /// A marked breakpoint fired.
    MarkedBreakpoint,
}

impl EventCategory {
    /// Canonical index (0..18) into the config visibility mask.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Default RGB color — **verbatim** from `SnesEventViewerConfig.cs`.
    #[must_use]
    pub const fn color(self) -> (u8, u8, u8) {
        match self {
            Self::PpuCgramWrite => (0xC9, 0x29, 0x29),
            Self::PpuVramWrite => (0xB4, 0x7A, 0xDA),
            Self::PpuOamWrite => (0x53, 0xD7, 0x44),
            Self::PpuMode7Write => (0xFE, 0x78, 0x7B),
            Self::PpuBgOptionWrite => (0xBF, 0x80, 0x20),
            Self::PpuBgScrollWrite => (0x4A, 0x7C, 0xD9),
            Self::PpuWindowWrite => (0xE2, 0x51, 0xF7),
            Self::PpuOtherWrite => (0xD1, 0xDD, 0x42),
            Self::PpuRead => (0x00, 0x75, 0x97),
            Self::CpuWrite => (0xFF, 0x5E, 0x5E),
            Self::CpuRead => (0x18, 0x98, 0xE4),
            Self::ApuWrite => (0x9F, 0x93, 0xC6),
            Self::ApuRead => (0xF9, 0xFE, 0xAC),
            Self::WorkRamWrite => (0x2E, 0xFF, 0x28),
            Self::WorkRamRead => (0x8E, 0x33, 0xFF),
            Self::Nmi => (0xAB, 0xAD, 0xAC),
            Self::Irq => (0xC4, 0xF4, 0x7A),
            Self::MarkedBreakpoint => (0x18, 0x98, 0xE4),
        }
    }

    /// Short label for the UI legend / list.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PpuCgramWrite => "CGRAM",
            Self::PpuVramWrite => "VRAM",
            Self::PpuOamWrite => "OAM",
            Self::PpuMode7Write => "Mode 7",
            Self::PpuBgOptionWrite => "BG Options",
            Self::PpuBgScrollWrite => "BG Scroll",
            Self::PpuWindowWrite => "Window",
            Self::PpuOtherWrite => "PPU Other",
            Self::PpuRead => "PPU Read",
            Self::CpuWrite => "CPU Write",
            Self::CpuRead => "CPU Read",
            Self::ApuWrite => "APU Write",
            Self::ApuRead => "APU Read",
            Self::WorkRamWrite => "WRAM Write",
            Self::WorkRamRead => "WRAM Read",
            Self::Nmi => "NMI",
            Self::Irq => "IRQ",
            Self::MarkedBreakpoint => "Breakpoint",
        }
    }
}

/// Categorise a register access by its 16-bit address — **verbatim** port of
/// `SnesEventManager::GetEventConfig` (`SnesEventManager.cpp:116-149`).
///
/// NMI/IRQ are categorised by event *type*, not address — see
/// [`event_category`]. DMA-channel gating is not applied here (see the
/// `TODO` on [`EventViewerConfig::show_dma_channels`]).
#[must_use]
pub const fn categorise(reg: u16, is_write: bool) -> Option<EventCategory> {
    if reg <= 0x213F {
        if is_write {
            Some(match reg {
                0x2101..=0x2104 => EventCategory::PpuOamWrite,
                0x2105..=0x210C => EventCategory::PpuBgOptionWrite,
                0x210D..=0x2114 => EventCategory::PpuBgScrollWrite,
                0x2115..=0x2119 => EventCategory::PpuVramWrite,
                0x211A..=0x2120 => EventCategory::PpuMode7Write,
                0x2121..=0x2122 => EventCategory::PpuCgramWrite,
                0x2123..=0x212B => EventCategory::PpuWindowWrite,
                _ => EventCategory::PpuOtherWrite,
            })
        } else {
            Some(EventCategory::PpuRead)
        }
    } else if reg <= 0x217F {
        Some(if is_write {
            EventCategory::ApuWrite
        } else {
            EventCategory::ApuRead
        })
    } else if reg <= 0x2183 {
        Some(if is_write {
            EventCategory::WorkRamWrite
        } else {
            EventCategory::WorkRamRead
        })
    } else if reg >= 0x4000 {
        Some(if is_write {
            EventCategory::CpuWrite
        } else {
            EventCategory::CpuRead
        })
    } else {
        None
    }
}

/// Map a raw [`MemTraceEvent`] to its Event Viewer category. NMI/IRQ markers map
/// straight to [`EventCategory::Nmi`]/[`EventCategory::Irq`]; register accesses
/// go through [`categorise`].
#[must_use]
pub fn event_category(ev: &MemTraceEvent) -> Option<EventCategory> {
    match ev.kind {
        MemEventKind::NmiSignal => return Some(EventCategory::Nmi),
        MemEventKind::IrqSignal => return Some(EventCategory::Irq),
        MemEventKind::Read | MemEventKind::Write => {}
    }
    // Registers live only in the system-area banks ($00-$3F / $80-$BF); the same
    // offset in WRAM banks ($7E/$7F) or cart banks is not a register access.
    let bank = (ev.addr_full >> 16) as u8;
    if bank > 0x3F && !(0x80..=0xBF).contains(&bank) {
        return None;
    }
    let reg = (ev.addr_full & 0xFFFF) as u16;
    categorise(reg, matches!(ev.kind, MemEventKind::Write))
}

/// Event Viewer configuration — mirrors Mesen2's `SnesEventViewerConfig`
/// (all categories visible, previous-frame on, all DMA channels on by default).
#[derive(Debug, Clone)]
pub struct EventViewerConfig {
    /// Per-category visibility, indexed by [`EventCategory::index`].
    pub visible: [bool; CATEGORY_COUNT],
    /// Show the previous frame's trailing events on the overlay.
    pub show_previous_frame: bool,
    /// Per-DMA-channel visibility — a DMA-sourced event whose channel's flag
    /// is `false` is excluded (Mesen2 `GetEventConfig` `:106-109`, the
    /// `ShowDmaChannels[DmaChannel & 7]` gate). luna tags each DMA B-bus
    /// write with its channel (the controller's `dma->GetActiveChannel()`
    /// equivalent), so this filter is enforced in [`decode_dma_event`].
    pub show_dma_channels: [bool; 8],
}

impl Default for EventViewerConfig {
    fn default() -> Self {
        Self {
            visible: [true; CATEGORY_COUNT],
            show_previous_frame: true,
            show_dma_channels: [true; 8],
        }
    }
}

/// One decoded Event Viewer event — the snapshot unit the overlay/list consume.
#[derive(Debug, Clone, Copy)]
pub struct EventViewerEvent {
    /// PPU scanline at the access.
    pub scanline: u16,
    /// H-clock (0..1363), Mesen2's `Cycle`.
    ///
    /// TODO fidelity: derived as `dot * 4`. Mesen2 uses the exact `GetHClock()`;
    /// luna's trace stores the dot (H/4), so sub-dot H-clock precision is lost.
    pub cycle: u16,
    /// 16-bit register address.
    pub addr: u16,
    /// Byte transferred.
    pub value: u8,
    /// Originating program counter — the 24-bit CPU PC for a CPU access
    /// (`MemTraceEvent::pc_full`), or the DMA source address for a DMA write
    /// (`DmaTraceEvent::src_full`, Mesen2 shows the DMA source here).
    pub pc: u32,
    /// Resolved category.
    pub category: EventCategory,
    /// True if this event belongs to the previous frame (drawn trailing).
    pub is_prev_frame: bool,
    /// `Some(channel)` if this access was performed by DMA (the channel that
    /// drove it, 0-7); `None` for a CPU access (Mesen2 `DmaChannel == -1`).
    pub dma_channel: Option<u8>,
}

/// Decode one raw event into a snapshot row, honouring the config visibility
/// mask. Returns `None` if the access has no category or its category is hidden.
#[must_use]
pub fn decode_event(
    ev: &MemTraceEvent,
    cfg: &EventViewerConfig,
    is_prev_frame: bool,
) -> Option<EventViewerEvent> {
    let category = event_category(ev)?;
    if !cfg.visible[category.index()] {
        return None;
    }
    Some(EventViewerEvent {
        scanline: ev.line,
        cycle: ev.dot.saturating_mul(4),
        addr: (ev.addr_full & 0xFFFF) as u16,
        value: ev.value,
        pc: ev.pc_full & 0xFF_FFFF,
        category,
        is_prev_frame,
        dma_channel: None,
    })
}

/// Decode one DMA B-bus write into a snapshot row, honouring both the
/// category-visibility mask and the per-channel DMA filter.
///
/// A DMA B-bus write to `b_offset` targets register `$2100 + b_offset` and is
/// always a write (`is_write = true`, Mesen2's `DmaWrite`). Returns `None` if
/// the access has no category, its category is hidden, or its channel is
/// hidden by `show_dma_channels` (Mesen2 `GetEventConfig` `:106-109`).
#[must_use]
pub fn decode_dma_event(
    ev: &DmaTraceEvent,
    cfg: &EventViewerConfig,
    is_prev_frame: bool,
) -> Option<EventViewerEvent> {
    // DMA-channel filter first — verbatim Mesen2 `GetEventConfig:106-109`:
    // a DMA op on a hidden channel is excluded entirely.
    if !cfg.show_dma_channels[(ev.channel & 7) as usize] {
        return None;
    }
    let addr = 0x2100u16 + u16::from(ev.b_offset);
    let category = categorise(addr, true)?;
    if !cfg.visible[category.index()] {
        return None;
    }
    Some(EventViewerEvent {
        scanline: ev.line,
        cycle: ev.dot.saturating_mul(4),
        addr,
        value: ev.value,
        // Mesen2 shows the DMA source address in the "PC" column for a
        // DMA-originated event (there is no instruction PC).
        pc: ev.src_full & 0xFF_FFFF,
        category,
        is_prev_frame,
        dma_channel: Some(ev.channel & 7),
    })
}

/// SNES register name — **verbatim** port of `SetSnesDefaultLabels`
/// (`UI/Debugger/Labels/DefaultLabelHelper.cs`). Covers the B-bus PPU/APU/WRAM
/// registers ($2100-$2183) and the A-bus CPU registers ($4016-$421F). Addresses
/// Mesen2 does not label (e.g. the $43xx DMA registers) return `None`.
#[must_use]
pub const fn register_name(addr: u16) -> Option<&'static str> {
    Some(match addr {
        0x2100 => "INIDISP",
        0x2101 => "OBSEL",
        0x2102 => "OAMADDL",
        0x2103 => "OAMADDH",
        0x2104 => "OAMDATA",
        0x2105 => "BGMODE",
        0x2106 => "MOSAIC",
        0x2107 => "BG1SC",
        0x2108 => "BG2SC",
        0x2109 => "BG3SC",
        0x210A => "BG4SC",
        0x210B => "BG12NBA",
        0x210C => "BG34NBA",
        0x210D => "BG1HOFS",
        0x210E => "BG1VOFS",
        0x210F => "BG2HOFS",
        0x2110 => "BG2VOFS",
        0x2111 => "BG3HOFS",
        0x2112 => "BG3VOFS",
        0x2113 => "BG4HOFS",
        0x2114 => "BG4VOFS",
        0x2115 => "VMAIN",
        0x2116 => "VMADDL",
        0x2117 => "VMADDH",
        0x2118 => "VMDATAL",
        0x2119 => "VMDATAH",
        0x211A => "M7SEL",
        0x211B => "M7A",
        0x211C => "M7B",
        0x211D => "M7C",
        0x211E => "M7D",
        0x211F => "M7X",
        0x2120 => "M7Y",
        0x2121 => "CGADD",
        0x2122 => "CGDATA",
        0x2123 => "W12SEL",
        0x2124 => "W34SEL",
        0x2125 => "WOBJSEL",
        0x2126 => "WH0",
        0x2127 => "WH1",
        0x2128 => "WH2",
        0x2129 => "WH3",
        0x212A => "WBGLOG",
        0x212B => "WOBJLOG",
        0x212C => "TM",
        0x212D => "TS",
        0x212E => "TMW",
        0x212F => "TSW",
        0x2130 => "CGWSEL",
        0x2131 => "CGADSUB",
        0x2132 => "COLDATA",
        0x2133 => "SETINI",
        0x2134 => "MPYL",
        0x2135 => "MPYM",
        0x2136 => "MPYH",
        0x2137 => "SLHV",
        0x2138 => "OAMDATAREAD",
        0x2139 => "VMDATALREAD",
        0x213A => "VMDATAHREAD",
        0x213B => "CGDATAREAD",
        0x213C => "OPHCT",
        0x213D => "OPVCT",
        0x213E => "STAT77",
        0x213F => "STAT78",
        0x2140 => "APUIO0",
        0x2141 => "APUIO1",
        0x2142 => "APUIO2",
        0x2143 => "APUIO3",
        0x2180 => "WMDATA",
        0x2181 => "WMADDL",
        0x2182 => "WMADDM",
        0x2183 => "WMADDH",
        0x4016 => "JOYSER0",
        0x4017 => "JOYSER1",
        0x4200 => "NMITIMEN",
        0x4201 => "WRIO",
        0x4202 => "WRMPYA",
        0x4203 => "WRMPYB",
        0x4204 => "WRDIVL",
        0x4205 => "WRDIVH",
        0x4206 => "WRDIVB",
        0x4207 => "HTIMEL",
        0x4208 => "HTIMEH",
        0x4209 => "VTIMEL",
        0x420A => "VTIMEH",
        0x420B => "MDMAEN",
        0x420C => "HDMAEN",
        0x420D => "MEMSEL",
        0x4210 => "RDNMI",
        0x4211 => "TIMEUP",
        0x4212 => "HVBJOY",
        0x4213 => "RDIO",
        0x4214 => "RDDIVL",
        0x4215 => "RDDIVH",
        0x4216 => "RDMPYL",
        0x4217 => "RDMPYH",
        0x4218 => "JOY1L",
        0x4219 => "JOY1H",
        0x421A => "JOY2L",
        0x421B => "JOY2H",
        0x421C => "JOY3L",
        0x421D => "JOY3H",
        0x421E => "JOY4L",
        0x421F => "JOY4H",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(reg: u16, w: bool) -> Option<EventCategory> {
        categorise(reg, w)
    }

    #[test]
    fn ppu_write_subcategories_match_mesen_boundaries() {
        use EventCategory::*;
        // $2100 (INIDISP) is NOT in the OAM bucket → Other.
        assert_eq!(cat(0x2100, true), Some(PpuOtherWrite));
        assert_eq!(cat(0x2101, true), Some(PpuOamWrite));
        assert_eq!(cat(0x2104, true), Some(PpuOamWrite));
        assert_eq!(cat(0x2105, true), Some(PpuBgOptionWrite));
        assert_eq!(cat(0x210C, true), Some(PpuBgOptionWrite));
        assert_eq!(cat(0x210D, true), Some(PpuBgScrollWrite));
        assert_eq!(cat(0x2114, true), Some(PpuBgScrollWrite));
        assert_eq!(cat(0x2115, true), Some(PpuVramWrite));
        assert_eq!(cat(0x2119, true), Some(PpuVramWrite));
        assert_eq!(cat(0x211A, true), Some(PpuMode7Write));
        assert_eq!(cat(0x2120, true), Some(PpuMode7Write));
        assert_eq!(cat(0x2121, true), Some(PpuCgramWrite));
        assert_eq!(cat(0x2122, true), Some(PpuCgramWrite));
        assert_eq!(cat(0x2123, true), Some(PpuWindowWrite));
        assert_eq!(cat(0x212B, true), Some(PpuWindowWrite));
        assert_eq!(cat(0x212C, true), Some(PpuOtherWrite));
        assert_eq!(cat(0x213F, true), Some(PpuOtherWrite));
    }

    #[test]
    fn apu_wram_cpu_ranges_and_gaps() {
        use EventCategory::*;
        assert_eq!(cat(0x2140, true), Some(ApuWrite));
        assert_eq!(cat(0x217F, true), Some(ApuWrite));
        assert_eq!(cat(0x2180, true), Some(WorkRamWrite));
        assert_eq!(cat(0x2183, true), Some(WorkRamWrite));
        // $2184-$3FFF is an unmapped gap.
        assert_eq!(cat(0x2184, true), None);
        assert_eq!(cat(0x3FFF, true), None);
        assert_eq!(cat(0x4000, true), Some(CpuWrite));
        assert_eq!(cat(0x4210, true), Some(CpuWrite));
    }

    #[test]
    fn reads_resolve_to_read_categories() {
        use EventCategory::*;
        assert_eq!(cat(0x2138, false), Some(PpuRead));
        assert_eq!(cat(0x2140, false), Some(ApuRead));
        assert_eq!(cat(0x2180, false), Some(WorkRamRead));
        assert_eq!(cat(0x4210, false), Some(CpuRead));
    }

    #[test]
    fn register_names_match_mesen_labels() {
        assert_eq!(register_name(0x2107), Some("BG1SC"));
        assert_eq!(register_name(0x210D), Some("BG1HOFS"));
        assert_eq!(register_name(0x2100), Some("INIDISP"));
        assert_eq!(register_name(0x4200), Some("NMITIMEN"));
        assert_eq!(register_name(0x420B), Some("MDMAEN"));
        assert_eq!(register_name(0x4210), Some("RDNMI"));
        // Unlabeled (DMA channel registers, gaps) → None.
        assert_eq!(register_name(0x4300), None);
        assert_eq!(register_name(0x2185), None);
    }

    #[test]
    fn category_index_is_stable() {
        assert_eq!(EventCategory::PpuCgramWrite.index(), 0);
        assert_eq!(EventCategory::MarkedBreakpoint.index(), 17);
        assert_eq!(CATEGORY_COUNT, 18);
    }

    /// Build a synthetic DMA B-bus write to `$2100 + b_offset` on `channel`.
    fn dma_ev(b_offset: u8, channel: u8) -> DmaTraceEvent {
        DmaTraceEvent {
            src_full: 0x7E_0000,
            vram_word: 0,
            b_offset,
            value: 0xAB,
            channel,
            frame: 0,
            line: 42,
            dot: 100,
            blank: false,
            force_blank: false,
        }
    }

    #[test]
    fn dma_event_categorises_by_target_register_and_carries_channel() {
        let cfg = EventViewerConfig::default();
        // $2104 (OAMDATA) on channel 5 → OAM-write category, channel tagged.
        let e = decode_dma_event(&dma_ev(0x04, 5), &cfg, false).expect("visible");
        assert_eq!(e.addr, 0x2104);
        assert_eq!(e.category, EventCategory::PpuOamWrite);
        assert_eq!(e.dma_channel, Some(5));
        assert_eq!(e.scanline, 42);
        assert_eq!(e.cycle, 400, "dot 100 * 4 = H-clock 400");

        // $2118 (VMDATAL) on channel 0 → VRAM-write category.
        let v = decode_dma_event(&dma_ev(0x18, 0), &cfg, false).expect("visible");
        assert_eq!(v.addr, 0x2118);
        assert_eq!(v.category, EventCategory::PpuVramWrite);
        assert_eq!(v.dma_channel, Some(0));
    }

    #[test]
    fn show_dma_channels_filter_excludes_hidden_channel() {
        let mut cfg = EventViewerConfig::default();
        // Hide channel 3; a DMA event on channel 3 is excluded entirely
        // (Mesen2 GetEventConfig:106-109), even though its category is visible.
        cfg.show_dma_channels[3] = false;
        assert!(decode_dma_event(&dma_ev(0x18, 3), &cfg, false).is_none());
        // A different channel for the same register is still shown.
        assert!(decode_dma_event(&dma_ev(0x18, 2), &cfg, false).is_some());
    }

    #[test]
    fn hidden_category_excludes_dma_event() {
        let mut cfg = EventViewerConfig::default();
        // Hide the VRAM-write category; a VRAM DMA write is excluded even
        // though its channel is visible.
        cfg.visible[EventCategory::PpuVramWrite.index()] = false;
        assert!(decode_dma_event(&dma_ev(0x18, 0), &cfg, false).is_none());
        // An OAM DMA write (different, still-visible category) survives.
        assert!(decode_dma_event(&dma_ev(0x04, 0), &cfg, false).is_some());
    }
}
