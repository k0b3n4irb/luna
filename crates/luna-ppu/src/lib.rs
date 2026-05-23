//! SNES Picture Processing Unit.
//!
//! 8 graphics modes (including Mode 7), 4 background layers, 128 sprites,
//! OAM, CGRAM palette, windowing, color math.
//!
//! Rendering is scanline-based in V1; dot-based renderer planned for V2 to
//! support demos that change registers mid-scanline.
//!
//! See `ARCHITECTURE.md` §6.2.
