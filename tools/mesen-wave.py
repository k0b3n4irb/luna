#!/usr/bin/env python3
"""mesen-wave.py — record the sound Mesen2 makes of a ROM, headless, as a WAV.

Same native-core route as tools/mesen-trace-log.py (MesenCore.so through
ctypes), but it starts Mesen2's own wave recorder (`WaveRecord`) instead of
the trace logger: an independent reference to set beside `luna state
--audio-out`. Mesen2 runs in real time, so `--seconds` is wall-clock time of
emulation (Mesen2 mixes at 48 kHz; compare shapes, not sample indices).

    tools/mesen-wave.py <rom> <out.wav> [--seconds S] [--ram zeros|ones|random]
"""
import argparse
import ctypes as C
import os
import sys
import tempfile
import time

DEFAULT_FORMAT = (
    "[Disassembly][EffectiveAddress] [MemoryValue,h][Align,38] A:[A,4h] X:[X,4h] "
    "Y:[Y,4h] S:[SP,4h] D:[D,4h] DB:[DB,2h] P:[P,8] V:[Scanline,3] H:[Cycle,3]"
)


class KeyMapping(C.Structure):
    _fields_ = [
        (n, C.c_uint16)
        for n in (
            "A B X Y L R Up Down Left Right Start Select U D TurboA TurboB TurboX "
            "TurboY TurboL TurboR TurboSelect TurboStart GenericKey1"
        ).split()
    ] + [("CustomKeys", C.c_uint16 * 100)]


class KeyMappingSet(C.Structure):
    _fields_ = [("Mappings", KeyMapping * 4), ("TurboSpeed", C.c_uint32)]


class ControllerConfig(C.Structure):
    _fields_ = [("Keys", KeyMappingSet), ("Type", C.c_int)]


class SnesConfig(C.Structure):
    """Mesen2's `SnesConfig` (Core/Shared/SettingTypes.h), field for field."""

    _fields_ = [
        ("Port1", ControllerConfig),
        ("Port2", ControllerConfig),
        ("Port1SubPorts", ControllerConfig * 4),
        ("Port2SubPorts", ControllerConfig * 4),
        ("Region", C.c_int),
        ("AllowInvalidInput", C.c_bool),
        ("BlendHighResolutionModes", C.c_bool),
        ("HideBgLayer1", C.c_bool),
        ("HideBgLayer2", C.c_bool),
        ("HideBgLayer3", C.c_bool),
        ("HideBgLayer4", C.c_bool),
        ("HideSprites", C.c_bool),
        ("DisableFrameSkipping", C.c_bool),
        ("ForceFixedResolution", C.c_bool),
        ("Overscan", C.c_uint32 * 4),
        ("InterpolationType", C.c_int),
        ("ChannelVolumes", C.c_uint32 * 8),
        ("EnableRandomPowerOnState", C.c_bool),
        ("EnableStrictBoardMappings", C.c_bool),
        ("RamPowerOnState", C.c_int),
        ("SpcClockSpeedAdjustment", C.c_int32),
        ("PpuExtraScanlinesBeforeNmi", C.c_uint32),
        ("PpuExtraScanlinesAfterNmi", C.c_uint32),
        ("GsuClockSpeed", C.c_uint32),
        ("BsxCustomDate", C.c_int64),
    ]


class TraceLoggerOptions(C.Structure):
    _fields_ = [
        ("Enabled", C.c_bool),
        ("IndentCode", C.c_bool),
        ("UseLabels", C.c_bool),
        ("Condition", C.c_char * 1000),
        ("Format", C.c_char * 1000),
    ]


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("rom")
    ap.add_argument("out")
    ap.add_argument("--seconds", type=float, default=8.0)
    ap.add_argument("--ram", choices=("random", "zeros", "ones"), default="zeros")
    args = ap.parse_args()

    lib = os.environ.get("MESEN_CORE", os.path.expanduser("~/.config/Mesen2/MesenCore.so"))
    core = C.CDLL(lib)
    home = tempfile.mkdtemp(prefix="luna-mesen-home-") + "/"
    core.InitDll()
    core.InitializeEmu(home.encode(), None, None, True, True, True, True)
    cfg = SnesConfig()
    cfg.GsuClockSpeed = 100
    cfg.BsxCustomDate = -1
    for i in range(8):
        cfg.ChannelVolumes[i] = 100
    cfg.RamPowerOnState = ("random", "zeros", "ones").index(args.ram)
    core.SetSnesConfig(cfg)

    rom = os.path.abspath(args.rom).encode()
    if not core.LoadRom(rom, None):
        sys.exit(f"Mesen2 could not load {args.rom}")
    # Record from the start: pause on the first instruction, start the recorder, run.
    core.InitializeDebugger()
    core.Pause()
    time.sleep(0.2)
    core.LoadRom(rom, None)
    time.sleep(0.5)
    core.WaveRecord(os.path.abspath(args.out).encode())
    core.ResumeExecution()
    time.sleep(args.seconds)
    core.Pause()
    core.WaveStop()
    core.Release()


if __name__ == "__main__":
    main()
