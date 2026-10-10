# Scripted joypad input (`--input`)

Shared by every subcommand that runs a ROM except `run` — `state`,
`frames`, `diff`, `profile`, `wram-trace`, `bench`, `spc-dump`,
`assets-dump`. `state` and `profile` also take `--input2` … `--input5`
for the other pads (a `luna test` manifest has `input` and `input2`),
same grammar. Format:
comma-separated `frame:hex` checkpoints — frame number in decimal, mask
in hex (optional `0x`). The mask is latched at the **start** of the named
PPU frame and held until the next checkpoint overrides it.

```
--input "100:0x1000,110:0"   # hold Start for frames 100..=109, then release
```

`--input` also accepts **`@<file>`** to read the script from a file, and the
grammar allows `#` comments and newlines — so a recording exported from
`luna-gui` (*Emulation ▸ ● Record input*) or the MCP `take_input_capture`
tool replays straight back:

```bash
luna state -n 60000000 --input @gameplay.input "game.sfc"
```

**JOY1 bit layout:** `B(15) Y(14) Select(13) Start(12) Up(11) Down(10)
Left(9) Right(8) A(7) X(6) L(5) R(4)`. So Start = `$1000`, A = `$80`.

> Most commercial titles sit at a title/demo screen waiting for Start —
> a black/forced-blank screenshot with no input is **not** a bug. Pulse
> Start to get past it.

## Pointer devices (Mouse / Super Scope)

A port can hold a **Mouse** or **Super Scope** instead of a pad. Select the
device with `--port1`/`--port2`, then script its motion. The device names
are `pad` (`joypad` is accepted too), `mouse`, `superscope`, `multitap` and
`none` for an empty port:

```
# Super Scope on port 2, fire at screen pixel (128, 112) on frame 120
--port2 superscope --superscope "120:128,112,1"

# Mouse on port 1: move +5/-3 and press the left button on frame 60
--port1 mouse --mouse "60:5,-3,1"
```

`--mouse` takes signed `dx,dy` motion (`buttons` bit0 = left, bit1 = right);
`--superscope` takes absolute screen `x,y` pixels (`buttons` bit0 = trigger,
bit1 = cursor, bit2 = turbo, bit3 = pause). In the GUI these map to the host
mouse cursor automatically once a port is set to the device under
**Settings → Devices**.

## Super Multitap (3-5 players)

`--port2 multitap` puts a Super Multitap on port 2: player 2 is its pad A
(`--input2`), players 3, 4 and 5 its pads B, C, D (`--input3` … `--input5`).
The game sees the tap's detection signature and reads players 2/3 through
the auto-read (`$421A`, `$421E`) and players 4/5 through `$4017` with WRIO
bit 7 low, as on hardware:

```bash
# Four players pressing Start on frame 300 of a multitap title
luna state -n 20000000 --port2 multitap \
  --input "300:0x1000,310:0" --input2 "300:0x1000,310:0" \
  --input3 "300:0x1000,310:0" --input4 "300:0x1000,310:0" \
  --screenshot /tmp/four.png "game.sfc"
```

Over MCP: `set_port_device {port: 1, device: "multitap"}`, then
`set_joypad {port: 2..4, mask}`. One tap is modelled (the 8-player
two-tap setup is not).

## A script clocked by the game, not by the frame

A frame number says when a press lands on the wall clock. Which game
tick it lands in depends on how fast the code ran until then: make the
game faster (a compiler step, an optimisation) and the press of frame
210 arrives one tick earlier or later, the run diverges, and every hash
and counter after it moves — with no change in the game's logic.

`--input-at <SYMBOL>` indexes the script by **arrivals on a routine**
instead: entry `N:` applies the N-th time execution reaches it (a `.sym`
label, `label+N`, or `BANK:OFFSET`). Give it the routine that starts a
game tick, and the numbers are tick numbers:

```bash
# A held from the 3rd tick to the 6th — on any build of the game.
luna state --input-at tick --input "3:0x0080,6:0" \
  --until-pc tick --hit 10 --peek seen:#9 --out /dev/null game.sfc
#   $7E0020  00 00 00 80 80 80 00 00 00
```

It applies to every script of the command (`--input` … `--input5`,
`--mouse`, `--superscope`). `luna state`, `luna profile` and a `luna
test` manifest (`input_at = "tick"`, see
[Checkpoints](homebrew-ci.md#checkpoints--beforeafter-assertions)) take
it.

**When the game sees the press.** The press changes the *controller* at
arrival `N`, just before the routine's first instruction; it does not
write the game's variables. The game sees it at its next read of the
pad. With the auto-read that is the VBlank after arrival `N`, so a loop
that reads the pad once per pass sees it from arrival `N + 1` on, as
above. This holds whatever the speed of the code as long as an
auto-read completes between two arrivals — true of any loop that waits
for VBlank — and as long as the game does not read `$4218` in the three
scanlines the auto-read takes after VBlank starts.
