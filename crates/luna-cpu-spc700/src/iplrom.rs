//! The canonical 64-byte SNES SPC700 IPL ROM.
//!
//! This is a literal byte-for-byte copy of the SPC700 boot ROM that
//! sits at `$FFC0..=$FFFF` of every real SPC700's address space. It
//! is part of the SNES hardware — the same bytes appear on every
//! console ever made. Cross-checked against fullsnes (Nocash) and
//! ares' `sfc/smp/iplrom.cpp`.
//!
//! On reset the SPC700 reads its reset vector from `$FFFE/$FFFF`
//! (which the IPL ROM populates as `$FFC0`) and starts executing the
//! IPL ROM, which:
//!
//! 1. Initialises the stack pointer.
//! 2. Clears direct page RAM (`$0000..=$00EF`).
//! 3. Writes `$AA` to mailbox 0 (`$F4`) and `$BB` to mailbox 1 (`$F5`).
//! 4. Spins until the main CPU writes `$CC` to mailbox 0 — the
//!    handshake "kick".
//! 5. Enters a byte-transfer loop driven by counters on `$F4`/`$F5`
//!    and the target address on `$F6`/`$F7`.
//! 6. Finishes by `JMP [$0000+X]` into whatever code was uploaded.
//!
//! The ROM is read-only on real hardware (the SPC700 has a bit in
//! its control register `$F1` to expose it or hide it), but we only
//! model the "exposed" path for now — it's how every commercial
//! SNES game boots.
//!
//! See also: <https://problemkaputt.de/fullsnes.htm#snesapu>.

/// The 64 bytes of the SPC700 boot ROM, mapped at `$FFC0..=$FFFF`.
/// Same on every SNES — these are documented hardware bytes, not
/// reverse-engineered firmware.
pub const IPL_ROM: [u8; 64] = [
    // $FFC0: MOV X,#$EF        ; init top-of-stack value
    0xCD, 0xEF, //
    // $FFC2: MOV SP,X           ; SP = $EF
    0xBD, //
    // $FFC3: MOV A,#$00         ; A = 0
    0xE8, 0x00, //
    // $FFC5: MOV (X),A          ; *(direct[X]) = A; clear dp byte
    0xC6, //
    // $FFC6: DEC X
    0x1D, //
    // $FFC7: BNE $FFC5          ; loop while X != 0
    0xD0, 0xFC, //
    // $FFC9: MOV $F4,#$AA       ; mailbox 0 ← $AA
    0x8F, 0xAA, 0xF4, //
    // $FFCC: MOV $F5,#$BB       ; mailbox 1 ← $BB
    0x8F, 0xBB, 0xF5, //
    // $FFCF: CMP $F4,#$CC       ; spin until CPU writes $CC kick
    0x78, 0xCC, 0xF4, //
    // $FFD2: BNE $FFCF
    0xD0, 0xFB, //
    // $FFD4: BRA $FFEF          ; jump to per-byte transfer loop
    0x2F, 0x19, //
    // $FFD6: MOV Y,$F4
    0xEB, 0xF4, //
    // $FFD8: BNE $FFD6          ; wait for CPU's first counter byte
    0xD0, 0xFC, //
    // $FFDA: CMP Y,$F4          ; CPU just wrote a new counter
    0x7E, 0xF4, //
    // $FFDC: BNE $FFE9
    0xD0, 0x0B, //
    // $FFDE: MOV A,$F5          ; A = data byte
    0xE4, 0xF5, //
    // $FFE0: MOV $F4,Y          ; ack: $F4 ← counter
    0xCB, 0xF4, //
    // $FFE2: MOV ($00)+Y,A      ; store byte at *target+Y
    0xD7, 0x00, //
    // $FFE4: INC Y
    0xFC, //
    // $FFE5: BNE $FFDA          ; loop until Y wraps
    0xD0, 0xF3, //
    // $FFE7: INC $01            ; high byte of target++
    0xAB, 0x01, //
    // $FFE9: BPL $FFDA          ; non-counter write → loop
    0x10, 0xEF, //
    // $FFEB: CMP Y,$F4
    0x7E, 0xF4, //
    // $FFED: BPL $FFEB          ; (loop forever once at end of upload)
    0x10, 0xFB, //
    // $FFEF: BA $F6             ; MOVW YA,$F6 = read target address
    0xBA, 0xF6, //
    // $FFF1: DA $00             ; MOVW $00,YA = store as transfer target
    0xDA, 0x00, //
    // $FFF3: BA $F4             ; MOVW YA,$F4 = read entry address
    0xBA, 0xF4, //
    // $FFF5: MOV $F4,A          ; ack low byte
    0xC4, 0xF4, //
    // $FFF7: MOV A,Y            ; A = entry high byte
    0xDD, //
    // $FFF8: MOV X,A
    0x5D, //
    // $FFF9: BNE $FFD6          ; if entry != 0, do byte transfer
    0xD0, 0xDB, //
    // $FFFB: JMP [$0000+X]      ; jump to uploaded code
    0x1F, 0x00, 0x00, //
    // $FFFE: <reset vector — $C0 $FF (little-endian = $FFC0)>
    0xC0, 0xFF,
];

/// Where the IPL ROM lives in the SPC700's address space.
pub const IPL_ROM_BASE: u16 = 0xFFC0;
