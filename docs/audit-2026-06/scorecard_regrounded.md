# Luna — Scorecard RE-GROUNDÉ contre HEAD (2026-06-10)

> Vérification du `docs/accuracy_scorecard.md` (commit `f690f74`, 2026-05-29)
> contre le code actuel. Méthode : 5 agents parallèles, chaque affirmation
> localisée par contenu (les n° de ligne de mai ont dérivé) et tranchée au code.
> **Verdicts : TOUJOURS RÉEL / CORRIGÉ / PARTIEL.**

## Résultat global — le scorecard de mai était nettement pessimiste

Sur **27 affirmations** vérifiées : **16 CORRIGÉES**, **6 toujours réelles**,
**5 partielles**. La quasi-totalité de la famille « self-consistent but wrong »
(les bugs discrets notés D) a été **refermée** entre fin mai et début juin —
principalement via les Phases 1-4 du `cycle_accuracy_plan` et une vague de
correctifs PPU/SA-1.

| Sous-système | CORRIGÉ | PARTIEL | TOUJOURS RÉEL |
|---|:---:|:---:|:---:|
| CPU 65c816 | 2 (DP-16, IRQ level) | 1 (DP-8 nu, inerte) | 0 |
| SPC700 | 2 (branch-penalty, DIV) | 0 | 0 |
| PPU | 5 (sprites×2, Mode7×2, hi-res) | 0 | **1 (BG scroll write-twice)** |
| DMA/HDMA/timing | 3 (double-charge, H/V-IRQ, per-access sched) | 2 (Phase 5) | 0 |
| SA-1 | 4 (divider, MAC-clear, CC1, MAC-40bit) | 0 | **1 (timing plat)** |
| Bus/mappers | 1 (I/O speed) | 1 (détection coproc) | **3 (mirroring, open-bus, scoring)** |
| DSP | 1 (code mort) | 0 | **1 (zéro golden-vector)** |

---

## Détail par verdict

### ✅ CORRIGÉ depuis le scorecard (16)

| # | Bug (note mai) | Corrigé par | Preuve actuelle |
|---|---|---|---|
| CPU-1 | DP 16-bit fuit en banque 1 (**D**) | latch `bank0_wrap` + `hi_addr` | `opcodes.rs:1596` — confine `(addr as u16).wrapping_add(1)` ; tous les opcodes read/store/RMW passent par là |
| CPU-3 | IRQ edge-latched (**C**) | `86e9702` | `cpu.rs:49` `irq_line: bool` level + `set_irq_line` set/clear ; service ne consomme que le latch edge |
| SPC-4 | Pénalité branch-taken jamais appliquée (**C**) | `cfef84a` (Phase 2) | `opcodes.rs:42` (SPC) `+= SPC700_BRANCH_TAKEN_PENALTY` (=2) ; test `cycles.rs:136` |
| SPC-5 | DIV YA,X (A, à confirmer) | `de0ce63` | `opcodes.rs:1104` H/V depuis Y/X originaux + branche `256-X` ; non régressé |
| PPU-1 | Sprite Y-wrap 8-bit (**D**) | `fefdc91` | `renderer.rs:1607,1752` `& 0xFF` |
| PPU-2 | Tile large-sprite déborde (**D**) | `fefdc91` | `renderer.rs:1804` `col_nib/row_nib` masqués `& 0x0F` indépendamment |
| PPU-4 | Mode 7 OOB / sign-extend (**C**) | `35be343` | `renderer.rs:816` sign-extend 13-bit ; OOB distingue screen_over 3/2/0-1 |
| PPU-5 | Hi-res 5/6 absent (**D**) | `3f8f4ab`,`0950e42` | `renderer.rs:554` chemin `is_hires` dédié, downsample 512→256 |
| PPU-6 | EXTBG Mode 7 ignoré (**F**) | `713ef12` | `renderer.rs:497` `extbg`, BG2 dérivé du plan, test passant |
| DMA-1 | Double-charge coproc (**C−**) | Phase 1 `7c5bef0` | `snes.rs:1830` lump `advance_coproc=false` ; seul `tick` per-byte avance le coproc |
| DMA-2 | H/V IRQ ignore HTIME (**D**) | `f1ef75e`,`9981e52` | `snes.rs:1314` `poll_hv_irq` dot-précis `htime*4` ; IRQ tenu en niveau jusqu'à `$4211` |
| DMA-5 | Scheduler lump-charge (**D arch**) | Phase 1 | `snes.rs:1471` `advance_time` avance PPU/APU/coproc par accès ; plus aucun lump en fin de `step()` |
| SA1-1 | Diviseur signé÷signé (**D**) | `105edde` | `sa1.rs:930` `divisor = i32::from(self.mb as u16)` (non-signé), floored |
| SA1-2 | Guard clear MAC mort (**D**) | `105edde` | `sa1.rs:1277` `if value & 0x02 != 0 { self.mr = 0 }` |
| SA1-3 | CC1 bpp/width inversés (**D**) | port CDMA | `sa1.rs:575` `bpp=(cdma>>2)&7`, `width=8<<(cdma&3)` *(orientation exacte à re-checker vs ares si régression CC1)* |
| SA1-4 | MAC saturating sans OF (**C**) | `105edde` | `sa1.rs:919` `wrapping_add` 40-bit + flag bit-40 exposé en `$230B` b7 |
| Bus-2 | Table vitesse $2000-5FFF (**D**) | `47032bd` | `speed.rs:61` 2000-3FFF→6, 4000-41FF→12, 4200-5FFF→6 ; test `io_region_speeds_match_ares_wait` |
| APU-7 | Code mort ADSR/gaussian | `33def23` | supprimé de `lib.rs` (AdsrPhase, ADSR_RATE_PERIODS, gaussian dupli) |

*(18 lignes — CPU-1/3, SPC-4/5, PPU-1/2/4/5/6, DMA-1/2/5, SA1-1/2/3/4, Bus-2, APU-7)*

### 🟡 PARTIEL (5)

| # | Bug (note mai) | État réel |
|---|---|---|
| CPU-2 | DP 8-bit page-wrap nu manquant (**C**) | **Techniquement réel mais INERTE** : la branche de wrap 256-o n'est pas écrite pour le `direct_page` nu (`addressing.rs:47`), MAIS un offset u8 ajouté à un `dp` aligné-page ne franchit jamais la page ⇒ aucun effet observable. ➜ corriger le **commentaire trompeur** (`:42-45`), pas le code. |
| DMA-3 | Préemption HDMA mid-line (**C**) | Coût-temps HDMA chargé (`controller.rs:140,165` 18+8/byte via stall-loop) ; **préemption mid-line d'un DMA actif absente** — HDMA ne tourne qu'aux frontières de scanline. ➜ **Phase 5**, conforme au plan. |
| DMA-4 | DMA cycle cost lump (**C**) | MDMA reste un **burst atomique** lump-chargé `8+8*bytes` (`controller.rs:108`, `snes.rs:1830`) ; seul le tick coproc per-byte a atterri. Pas de stepping grille master-clock. ➜ **Phase 5**. |
| Bus-5 | Détection coproc mauvais octet (**D**) | **Super FX corrigé** (hi-nibble chipset `$FFD6`, `lib.rs:208`) ; **SA-1 toujours détecté via low-nibble MapMode `$FFD5`** (`lib.rs:243`) au lieu du hi-nibble RomType. ➜ levé pour le GSU seulement. |

### 🔴 TOUJOURS RÉEL (6) — la VRAIE liste de travail restante

| # | Bug (note mai) | Preuve actuelle | Impact |
|---|---|---|---|
| **PPU-3** | **BG scroll write-twice naïf** (**D**) | `ppu.rs:662` un seul `bg_scroll_latch`, `(hi<<8)\|lo & 0x3FF` pour H et V ; manque le terme `& 7` du 2e latch (ares io.cpp:312, DEUX latches). Le commentaire décrit la bonne formule mais admet modéliser « the canonical form ». | **Mis-scrolle le H-scroll sub-tile — très fréquent.** Le seul bug PPU discret survivant. |
| **SA1-5** | **Timing instruction SA-1 plat** (**C−**) | `coproc/sa1.rs:58` `MCLK_PER_SA1_INSN=6` fixe/instr, `io_cycle` no-op. | Modèle batché — même « grammaire non traduite » que Super FX. Architectural. |
| **Bus-1** | **ROM mirroring non-pow2 → open-bus** (**D**) | `lorom.rs:46`,`hirom.rs:103` renvoient `None` si `offset>=rom.len()` ; pas de modulo. Tests figent le comportement. | ROMs sous-dimensionnées. |
| **Bus-3** | **Open-bus = 0xFF fixe** (**C**) | `snes.rs:1151+` `unwrap_or(0xFF)` partout ; aucun champ MDR/open_bus. | Jeux lisant l'open-bus. |
| **Bus-4** | **Détection mapper first-pass-wins** (**C−**) | `lib.rs:179` renvoie au 1er `checksum_valid()`, pas de scoring multi-offset. | ROMs ambiguës / sans header propre. |
| **DSP-6** | **ZÉRO test golden-vector PCM** (**A−**, risque latent) | `dsp.rs:1044` 6 tests = silence/KON/counter/roundtrip ; `brr_decode` (`dsp.rs:557`) non couvert. | Le port le plus fidèle reste cru sur parole. **Plus haute valeur ajoutée unique.** |

---

## Conséquences — ce qui change vs ma synthèse précédente

1. **La « Famille 2 » (bugs discrets self-consistent-but-wrong) est presque
   vidée.** Sur ~12 bugs listés, **9 sont corrigés**. Le scorecard de mai
   décrivait un état qui n'existe plus. ➜ `/tmp/luna_lacunes.md` est à corriger.
2. **Le plan de cycle-accuracy est FIDÈLE au code** : Phases 1-3 confirmées
   atterries, Phase 4 partiellement (H/V IRQ dot-précis ✅, reste préemption),
   Phase 5 non commencée — exactement où le plan les place. Aucune prétention non
   tenue détectée.
3. **La vraie liste de travail restante est COURTE** (6 items réels + 2 résidus
   Phase 5). Par priorité actionnable :
   - 🟠 **PPU-3 BG scroll write-twice** — seul bug PPU discret, *très fréquent*,
     référence ares nette, petit. **Le meilleur ratio impact/effort maintenant.**
   - 🟠 **DSP-6 golden-vectors** — sécurise l'A−, petit, risque latent.
   - 🟡 **Bus-1/3/4** — mirroring, MDR open-bus, scoring détection : 3 petits fixes.
   - 🟡 **Bus-5** — détection SA-1 via le bon octet RomType.
   - ⚪ **CPU-2** — juste corriger le commentaire trompeur.
   - 🔴 **Architectural** (indépendant, gros) : Phase 5 DMA stepping + préemption
     HDMA, et le timing SA-1 plat — tous deux la même « grammaire » de scheduling.
4. **Recommandation méta** : **régénérer `docs/accuracy_scorecard.md` contre
   HEAD** (relever les notes : PPU C+→B, SA-1 C+→B, DMA C+→B−/B, Bus C+ stable).
   Le piège `project_subsystem_gap_docs` (« la doc APU avait gone stale ») s'est
   reproduit sur le scorecard entier — il faut le dater au code, pas à mai.
