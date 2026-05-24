#!/usr/bin/env python3
"""Audio regression harness for luna's APU.

Runs ``luna run --audio-out`` on a fixed set of ROMs at fixed instruction
counts, computes summary statistics from the resulting WAV, and either
records them as a new "golden" baseline or compares them against the
recorded baseline with fuzzy-equality tolerances.

Because the WAVs are derived from proprietary ROMs we **don't** commit
the WAVs themselves — only a small JSON sidecar per ROM (a few hundred
bytes) that captures the spectral fingerprint of the audio. Anyone with
the same ROM in ``tests/roms/`` can regenerate the WAV and verify it
matches.

Usage:

    # Capture / refresh goldens for all configured ROMs:
    tools/audio_goldens.py --update

    # Compare current build's output against goldens (exit non-zero
    # on any failure):
    tools/audio_goldens.py --check

    # Only one ROM, useful while iterating on the DSP:
    tools/audio_goldens.py --check --rom dkc

Sidecar JSON layout (``tests/golden/audio/<name>.json``):

    {
      "rom":          "Donkey Kong Country (U) (V1.2) [!].smc",
      "insns":        80000000,
      "duration_s":   35.807,
      "stats": {
        "peak_pos":   17376,
        "peak_neg": -16111,
        "rms":        780.6,
        "mean_abs":   244.8,
        "non_zero":   0.511
      },
      "spectrum_peaks": [          # 10 highest FFT bins, sorted
        {"hz": 93.0, "mag_db": 64.8},
        ...
      ]
    }

Tolerances are intentionally generous (±15 % on amplitude/RMS, ±5 Hz
+ ±6 dB on the spectral peaks) — we want to catch "echo got broken,
the bass went silent" regressions, not LSB drift from a refactor.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import struct
import subprocess
import sys
import wave
from dataclasses import dataclass, asdict
from pathlib import Path

# ----------------------------------------------------------------------
# Configuration
# ----------------------------------------------------------------------

REPO = Path(__file__).resolve().parent.parent
LUNA_BIN = REPO / "target" / "release" / "luna"
ROMS_DIR = REPO / "tests" / "roms"
GOLDEN_DIR = REPO / "tests" / "golden" / "audio"

# The ROMs we exercise. Each entry sets an instruction count chosen to
# land us in a point where music is actually playing.
CASES = {
    "smw": {
        "rom":   "Super Mario World (U) [!].smc",
        "insns": 10_000_000,
        "note":  "title screen, ~10 s of audio (no echo on this driver)",
    },
    "dkc": {
        "rom":   "Donkey Kong Country (U) (V1.2) [!].smc",
        "insns": 80_000_000,
        "note":  "past Rareware logo, ~36 s of audio with echo + voices 4-6",
    },
    "bomberman": {
        "rom":   "Super Bomberman (USA).sfc",
        "insns": 60_000_000,
        "note":  "title screen, ~63 s of audio",
    },
}

# Tolerances for `--check`. Anything tighter and the harness fires on
# benign refactors; anything looser and real regressions (e.g. echo
# turning into silence) slip through.
TOL_AMPLITUDE_PCT = 0.15  # ±15 % on peak / RMS / mean
TOL_NONZERO_ABS = 0.05    # ±5 percentage points on non-zero ratio
TOL_DURATION_S = 0.1      # different WAV length = different state
TOL_SPECTRUM_HZ = 5.0     # peak can shift ±5 Hz between builds
TOL_SPECTRUM_DB = 6.0     # ±6 dB on each peak's magnitude

# ----------------------------------------------------------------------
# Stat computation
# ----------------------------------------------------------------------


@dataclass
class Stats:
    peak_pos: int
    peak_neg: int
    rms: float
    mean_abs: float
    non_zero: float


@dataclass
class SpectrumPeak:
    hz: float
    mag_db: float


def _compute_stats(left: list[int]) -> Stats:
    n = len(left)
    if n == 0:
        return Stats(0, 0, 0.0, 0.0, 0.0)
    abs_left = [abs(s) for s in left]
    rms = math.sqrt(sum(s * s for s in left) / n)
    return Stats(
        peak_pos=max(left),
        peak_neg=min(left),
        rms=round(rms, 2),
        mean_abs=round(sum(abs_left) / n, 2),
        non_zero=round(sum(1 for s in left if s != 0) / n, 4),
    )


def _compute_spectrum_peaks(
    left: list[int], rate: int, top: int = 10
) -> list[SpectrumPeak]:
    """Compute a magnitude spectrum on the loudest 4096-sample window
    inside the waveform and return the top `top` peaks. Picking by
    loudest window (rather than centre) means a track with long silent
    gaps still produces a useful spectral fingerprint.

    Hand-rolled DFT — we don't want a numpy/scipy dep just for the
    golden harness, and the only frequency band we care about is
    < 4 kHz where most SNES music's energy lives.
    """
    n_full = len(left)
    if n_full < 4096:
        return []
    # Probe 8 evenly-spaced 4096-sample windows; pick the one with
    # the highest RMS so we DFT a part of the signal that actually
    # has music in it.
    best_start = 0
    best_energy = -1.0
    probe_size = 4096
    n_probes = 8
    for i in range(n_probes):
        start = i * (n_full - probe_size) // (n_probes - 1)
        energy = sum(s * s for s in left[start : start + probe_size])
        if energy > best_energy:
            best_energy = energy
            best_start = start
    chunk = left[best_start : best_start + 4096]
    # Hann window to reduce spectral leakage.
    n = len(chunk)
    win = [s * (0.5 - 0.5 * math.cos(2 * math.pi * i / (n - 1))) for i, s in enumerate(chunk)]
    # DFT magnitude — only over the audible low band (up to 4 kHz) to
    # keep cost manageable. 4 kHz at N=4096 / fs=32 kHz → bin 512.
    max_bin = min(n // 2, int(4000 * n / rate))
    mags: list[tuple[float, float]] = []  # (freq_hz, mag)
    for k in range(2, max_bin):
        re = 0.0
        im = 0.0
        for i, x in enumerate(win):
            ang = -2 * math.pi * k * i / n
            re += x * math.cos(ang)
            im += x * math.sin(ang)
        mag = math.sqrt(re * re + im * im)
        if mag > 0:
            mags.append((k * rate / n, mag))
    mags.sort(key=lambda x: -x[1])
    # Coalesce neighbouring bins so a 100 Hz tone doesn't fill the
    # top-10 with 99/100/101 Hz neighbours.
    selected: list[tuple[float, float]] = []
    for hz, m in mags:
        if all(abs(hz - h) > 5.0 for h, _ in selected):
            selected.append((hz, m))
        if len(selected) == top:
            break
    return [
        SpectrumPeak(hz=round(hz, 1), mag_db=round(20 * math.log10(m), 1))
        for hz, m in selected
    ]


def _read_wav(path: Path) -> tuple[int, list[int]]:
    """Read a stereo s16 WAV. Returns ``(sample_rate, left_channel)``."""
    with wave.open(str(path), "rb") as w:
        if w.getnchannels() != 2 or w.getsampwidth() != 2:
            raise ValueError(f"{path}: expected stereo s16, got {w.getparams()}")
        rate = w.getframerate()
        n = w.getnframes()
        data = w.readframes(n)
    samples = struct.unpack(f"<{n*2}h", data)
    return rate, list(samples[0::2])


# ----------------------------------------------------------------------
# Runner
# ----------------------------------------------------------------------


def _run_case(case_id: str) -> tuple[Stats, list[SpectrumPeak], float]:
    """Run a configured case and return its (stats, spectrum_peaks,
    duration_s) tuple."""
    case = CASES[case_id]
    rom_path = ROMS_DIR / case["rom"]
    if not rom_path.exists():
        raise FileNotFoundError(
            f"ROM missing: {rom_path}\n"
            f"  (golden harness needs the {case['rom']!r} ROM in tests/roms/)"
        )
    if not LUNA_BIN.exists():
        raise FileNotFoundError(
            f"luna binary missing: {LUNA_BIN}\n"
            f"  Run `cargo build --release -p luna-cli` first."
        )
    tmpdir = Path("/tmp")
    wav_path = tmpdir / f"luna_audiogolden_{case_id}.wav"
    if wav_path.exists():
        wav_path.unlink()
    cmd = [
        str(LUNA_BIN),
        "run",
        str(rom_path),
        "-n",
        str(case["insns"]),
        "--audio-out",
        str(wav_path),
    ]
    subprocess.run(cmd, check=True, capture_output=True)
    rate, left = _read_wav(wav_path)
    duration = round(len(left) / rate, 3)
    stats = _compute_stats(left)
    peaks = _compute_spectrum_peaks(left, rate)
    return stats, peaks, duration


# ----------------------------------------------------------------------
# Compare
# ----------------------------------------------------------------------


def _within_pct(a: float, b: float, pct: float) -> bool:
    if abs(b) < 1e-9:
        return abs(a) < 1e-9
    return abs(a - b) / abs(b) <= pct


def _compare(case_id: str, golden: dict, cur_stats: Stats, cur_peaks: list[SpectrumPeak], cur_duration: float) -> list[str]:
    """Return a list of human-readable failure descriptions. Empty
    list = pass."""
    fails: list[str] = []
    g_stats = golden["stats"]
    if abs(cur_duration - golden["duration_s"]) > TOL_DURATION_S:
        fails.append(
            f"duration: {cur_duration:.3f}s vs golden {golden['duration_s']:.3f}s "
            f"(tol ±{TOL_DURATION_S}s)"
        )
    for k, ref in [
        ("peak_pos", g_stats["peak_pos"]),
        ("peak_neg", g_stats["peak_neg"]),
        ("rms", g_stats["rms"]),
        ("mean_abs", g_stats["mean_abs"]),
    ]:
        cur = getattr(cur_stats, k)
        if not _within_pct(cur, ref, TOL_AMPLITUDE_PCT):
            fails.append(
                f"{k}: {cur} vs golden {ref} (tol ±{TOL_AMPLITUDE_PCT*100:.0f} %)"
            )
    if abs(cur_stats.non_zero - g_stats["non_zero"]) > TOL_NONZERO_ABS:
        fails.append(
            f"non_zero: {cur_stats.non_zero:.3f} vs golden {g_stats['non_zero']:.3f} "
            f"(tol ±{TOL_NONZERO_ABS*100:.1f} pp)"
        )
    # Spectrum peaks — for each golden peak, find the closest current
    # peak by frequency and check both freq and magnitude tolerances.
    g_peaks = golden["spectrum_peaks"]
    for g in g_peaks:
        best = min(cur_peaks, key=lambda p: abs(p.hz - g["hz"]), default=None)
        if best is None or abs(best.hz - g["hz"]) > TOL_SPECTRUM_HZ:
            fails.append(
                f"spectrum: golden peak at {g['hz']} Hz "
                f"({g['mag_db']} dB) has no nearby candidate"
            )
            continue
        if abs(best.mag_db - g["mag_db"]) > TOL_SPECTRUM_DB:
            fails.append(
                f"spectrum: peak at ~{g['hz']} Hz drifted from "
                f"{g['mag_db']} → {best.mag_db} dB (tol ±{TOL_SPECTRUM_DB} dB)"
            )
    return fails


# ----------------------------------------------------------------------
# Main
# ----------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    grp = ap.add_mutually_exclusive_group(required=True)
    grp.add_argument("--update", action="store_true", help="rewrite the golden sidecars")
    grp.add_argument("--check",  action="store_true", help="compare current build to goldens")
    ap.add_argument("--rom", choices=list(CASES.keys()), help="only run the named case")
    args = ap.parse_args()

    GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
    targets = [args.rom] if args.rom else list(CASES.keys())

    rc = 0
    for case_id in targets:
        sidecar = GOLDEN_DIR / f"{case_id}.json"
        print(f"\n=== {case_id} ({CASES[case_id]['rom']}) ===")
        try:
            stats, peaks, duration = _run_case(case_id)
        except FileNotFoundError as e:
            print(f"  skip: {e}")
            rc = max(rc, 1)
            continue

        if args.update:
            payload = {
                "rom":        CASES[case_id]["rom"],
                "insns":      CASES[case_id]["insns"],
                "note":       CASES[case_id]["note"],
                "duration_s": duration,
                "stats":      asdict(stats),
                "spectrum_peaks": [asdict(p) for p in peaks],
            }
            sidecar.write_text(json.dumps(payload, indent=2) + "\n")
            print(f"  wrote {sidecar.relative_to(REPO)}")
            print(f"  duration={duration}s peak=±({stats.peak_pos}, {stats.peak_neg}) "
                  f"rms={stats.rms} non_zero={stats.non_zero:.1%}")
            print(f"  top peaks: " +
                  ", ".join(f"{p.hz:.0f}Hz/{p.mag_db:.0f}dB" for p in peaks[:3]))
        else:
            if not sidecar.exists():
                print(f"  FAIL: no golden at {sidecar.relative_to(REPO)} — "
                      f"run with --update to create one")
                rc = max(rc, 2)
                continue
            golden = json.loads(sidecar.read_text())
            fails = _compare(case_id, golden, stats, peaks, duration)
            if fails:
                rc = max(rc, 3)
                print(f"  FAIL ({len(fails)}):")
                for f in fails:
                    print(f"    - {f}")
            else:
                print(f"  ok: duration={duration}s peak=±({stats.peak_pos}, {stats.peak_neg}) "
                      f"rms={stats.rms} non_zero={stats.non_zero:.1%}")
    print()
    if rc == 0:
        print("ALL OK")
    else:
        print(f"FAILURES (rc={rc})")
    return rc


if __name__ == "__main__":
    sys.exit(main())
