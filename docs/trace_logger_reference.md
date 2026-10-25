# Mesen2's trace logger — reference spec, and how luna follows it

What Mesen2's trace logger does, read from its source (Mesen2 at `5fc4e387`,
2026-05-21; the trace logger's code is the same on master `b9fa69dd`), and
what luna's
(`crates/luna-api/src/trace_logger.rs`) does the same or differently. The
GUI window is not ported yet (roadmap D4, `docs/mesen2_parity.md`).

Sources:

- `Core/Debugger/BaseTraceLogger.h` — format parsing, shared tags, the
  condition, the 30 000-row window, the file row.
- `Core/SNES/Debugger/TraceLogger/SnesCpuTraceLogger.cpp` — the 65C816 tags.
- `Core/Debugger/TraceLogFileSaver.h` — the file.
- `Core/SNES/Debugger/SnesDebugger.cpp` — when a row is taken
  (`ProcessRead`, `ProcessWrite`).
- `Core/SNES/Debugger/SnesDisUtils.cpp` — disassembly text, effective address
  (`GetEffectiveAddress`, the dummy CPU), addressing-mode table.
- `UI/Debugger/ViewModels/TraceLoggerViewModel.cs` — `GetAutoFormat`, the
  tag help; `UI/Config/Debugger/TraceLoggerCpuConfig.cs` — the defaults.

## When a row is taken

`SnesDebugger::ProcessRead` logs the row on the read of type `ExecOpCode`,
which Mesen2 reports once the access has ended (`SnesMemoryManager::Read`:
the clocks, the read, `IncMasterClock4`, then the debugger). So:

- the registers are those before the instruction (the PC is the opcode's
  address, `PC++` comes after the read);
- the beam (`Scanline`, `Cycle`, `HClock`) and `CycleCount` are those once
  the opcode fetch has ended — one fetch later than the instruction's start
  (6, 8 or 12 master clocks, plus a DRAM refresh or a DMA that falls in it);
- the effective address and the value are computed then, from memory as it
  is before the instruction's own accesses.

Mesen2's Lua `exec` callback fires earlier, in `ProcessInstruction`, before
the fetch: a Lua script cannot reproduce the trace logger's moment.

luna: the core records where the beam stands when the step's first code fetch
ends (`Snes::opcode_fetch`, armed only while a trace logger runs); the API
captures the registers, the bytes, the disassembly, the effective address
and the value before the step, and lays the row out after it with that
point. `$4210`-`$4212`, whose flags move with the beam, are read at the
point; every other register and memory byte before the instruction.

## The condition

An expression in the debugger's language (`ExpressionEvaluator`). Checked
on the `ExecOpCode` access (`address` = the PC, `value` = the opcode). When
it fails there, the row is kept pending (`_pendingLog`) and checked again on
every later access — operands, data, writes, DMA, until the next opcode fetch
replaces it (`LogNonExec`); the first access it holds for logs the pending
row, with the registers of before the instruction and the beam of that
access. An invalid condition is dropped silently (no condition).

luna: the same, on the CPU's own accesses (`Snes::step_accesses`, armed only
with a condition), evaluated after the step — an expression that reads memory
sees it as the instruction left it. DMA accesses are not offered yet. An
invalid condition is refused with an error.

## The format

`[Tag]`, `[Tag,N]`, `[Tag,h]`, `[Tag,Nh]` — `N` a minimum width, `h`
hexadecimal (the pattern ignores case, the comparison with `h` does not: `H`
is decimal). Text between tags is copied. An unknown tag writes
`[Invalid tag]`; a `[` that never closes is dropped.

| Tag | What |
|---|---|
| `ByteCode` | the instruction's bytes, `$A9 $00` in hex, `A9 00` otherwise |
| `Disassembly` | the instruction (a space after the mnemonic, operand or not); indented by `(0xFF - S.low) / 2` with *Indent code* |
| `EffectiveAddress` | ` [$7E1234]` or ` [label]`, when the mode shows one |
| `MemoryValue` | `= $12` / `= $1234` |
| `Align` | spaces up to column `N` **of the whole line** (the file's PC counts) |
| `PC` | the program counter, as short as it can be in hex |
| `Cycle` | the dot: H-clock / 4, except dots 323 and 327 of six clocks |
| `HClock` | the master clock in the line, 0-1363 |
| `Scanline` | the line |
| `FrameCount` | the frame |
| `CycleCount` | CPU cycles since power-on (one per read, write or idle cycle) |
| `A` `X` `Y` `D` `SP` | 16-bit registers |
| `DB` | the data bank |
| `P` | `NVMXDIZC`: set flags upper case, clear ones lower case when `N` ≥ 8, left out otherwise; the byte with `h` |

Numbers: hexadecimal on the digits of their C++ type (2 for a byte, 4 for a
word, 16 for `CycleCount`, 2 to 8 as needed for the 32-bit beam counters),
padded with zeros on the left to `N`; decimal padded with spaces on the
right.

### The default format

`GetAutoFormat` with the defaults of `TraceLoggerCpuConfig` (registers,
flags as text, effective addresses, memory values, frame position):

```
[Disassembly][EffectiveAddress] [MemoryValue,h][Align,38] A:[A,4h] X:[X,4h] Y:[Y,4h] S:[SP,4h] D:[D,4h] DB:[DB,2h] P:[P,8] V:[Scanline,3] H:[Cycle,3]
```

Each option off removes its piece (and its share of the alignment: 24, +8
for the effective address, +6 for the value); the frame counter adds
`Fr:[FrameCount]`, the clock counter `Cycle:[CycleCount]`, byte code
`BC:[ByteCode]`. luna: `auto_format`, `TraceFormatOptions`.

## The effective address and the value

`SnesDisUtils::GetEffectiveAddress` runs the instruction on a dummy CPU whose
reads peek and whose writes are only noted, then:

- a stack operation (mode `Stk`) shows nothing;
- an unconditional jump or a branch of 3 bytes shows where the dummy CPU's
  PC ended, without a value; a shorter one (relative, long) nothing;
- otherwise the **last** access that is not an operand fetch: with the one
  before it, a 16-bit read or write (`addr - 1` then `addr`) gives the first
  address and a size of 2, as a 16-bit read-modify-write (high byte written
  first); else a size of 1.

The address is shown only for the modes that have one (`HasEffectiveAddress`:
not immediate, implied, accumulator, block move, long absolute, relative, a
3-byte jump's operand…); the value is written whenever there is an address
and a size, so `LDA $7E0010` shows `= $12` alone. Registers read as Mesen2
peeks them: the B bus (`$21xx`) reads 0; `$4210`-`$421F` their value without
side effects; a write-only register the data bus, which carries the opcode
once it has been fetched.

luna: the same mechanism — luna's 65C816 core runs on a copy against a
peeking bus (`DummyBus`) — with luna's peek of the registers
(`Snes::peek_io_at`). Implied and accumulator instructions have no
dummy-read: Mesen2's dummy CPU adds a read at the PC to them when an
interrupt is about to be taken (`IdleOrRead`), which would show a value.

## The window and the file

Each CPU keeps its last 30 000 rows. Mesen2 keeps their state and formats a
row when the window draws it (the value is then memory as it is at drawing
time); luna formats every row when it is taken.

The file (`TraceLogFileSaver`, one for every CPU): each row is the 24-bit PC
in hex padded to 6, two spaces, the row, `\n`.

## The disassembly text

`SnesDisUtils::GetDisassembly`: the mnemonic, a space, the operand. Labels
replace only the operands of 24-bit addresses and branches. luna's
`disassemble_named` writes the same text for all 256 opcodes in both widths
(compared 2026-10-25), with no space after the mnemonic when there is no
operand — the trace adds it.

## How luna was compared

`tools/mesen-trace-log.py` drives Mesen2's native core headless and writes
its real trace file; `tools/trace-diff.py` pairs it with luna's
`--trace-log` row by row. On 2026-10-25, with labels off and work RAM zeroed:

| ROM | Rows paired | Different |
|---|---|---|
| PeterLemon `CPUADC` | 754 824 | 36, all within 3 rows after a `$420B` |
| PeterLemon `CPUPHL` | 718 324 | 38, idem |
| PeterLemon `CPULDR` | 727 118 | 56, idem |
| PeterLemon `CPUMOV` | 789 875 | 10, idem |
| PeterLemon `CPUJMP`, `CPURET` | 789 670, 792 556 | 8 each, idem |
| PeterLemon `CPUMSC` | 4 590 | 3 606, a 2-3-dot shift that starts at a DMA |
| Super Mario World | 356 556 | beam only, after the two runs part in the APU handshake (row 1990) |

Every difference is the beam, none the text: luna starts a DMA one bus
access before ares and Mesen2 (`docs/hdma_ares_audit.md` row 16), so the
opcode fetch after `STA $420B` comes after the transfer in luna and before
it in Mesen2. Super Mario World's first 1 990 rows are identical; then its
SPC700 answers the boot handshake at another time in the two emulators.
