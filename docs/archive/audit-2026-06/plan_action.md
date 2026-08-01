# 🛠️ Luna — Plans d'action détaillés (2026-06-10)

> Compagnon de `/tmp/luna_rapport_final.md`. Chaque action : **objectif · où ·
> référence · étapes · validation · effort · dépendances**. Validation conforme
> aux règles du dépôt (`reference-first`, `audible-fixes-test-first`,
> `rebuild-discipline`, `coproc-testing`). Effort : 🟢 < 1 j · 🟡 1-3 j · 🔴 > 1 sem.

**Légende priorité** : ⭐ ratio impact/effort élevé · 🧱 fondation · 🎁 valeur perçue.

---

## CHANTIER 0 — Quick wins & hygiène (faire en premier, ⭐)

### 0.1 — Régénérer le scorecard contre HEAD 🟢⭐
- **Objectif** : `docs/accuracy_scorecard.md` (commit `f690f74`, 29 mai) décrit un
  état périmé — 16/27 bugs corrigés depuis. Rechasser ces fantômes coûte des
  sessions (piège `project_subsystem_gap_docs`).
- **Où** : `docs/accuracy_scorecard.md` ; source des verdicts :
  `/tmp/luna_scorecard_regrounded.md`.
- **Étapes** : reporter les verdicts re-groundés ; relever les notes (PPU C+→B,
  SA-1 C+→B, DMA C+→B−, SPC700 B→B+) ; dater « re-groundé 2026-06-10 vs HEAD » ;
  garder la liste des **6 bugs réellement ouverts** en tête.
- **Validation** : revue humaine ; aucune compilation (doc).
- **Effort** : 🟢 · **Dépendances** : aucune.

### 0.2 — Corriger les commentaires trompeurs 🟢
- **Objectif** : supprimer la dette de doc inline qui ment sur le code.
- **Où & quoi** :
  - `luna-gui/src/lib.rs` (si présent) / `Cargo.toml` : eframe déjà retiré, vérifier
    qu'aucun doc-comment ne le cite encore comme actif.
  - `luna-cpu-65c816/src/addressing.rs:42-45` : le commentaire décrit un wrap
    page-256 **non implémenté** sur le `direct_page` nu (inerte, mais le commentaire
    induit en erreur). Corriger le commentaire (« wrap inerte ici car offset u8 sur
    dp aligné-page »), pas le code.
  - `docs/emulator_comparison_ness_jgenesis.md` §2b : marquer **OBSOLÈTE** — luna a
    déjà migré le framebuffer vers `triple_buffer` (`main.rs:88`).
- **Validation** : revue ; `cargo fmt --check`.
- **Effort** : 🟢 · **Dépendances** : aucune.

---

## CHANTIER 1 — Bugs d'accuracy discrets restants (6 items)

### 1.1 — PPU : BG scroll write-twice 🟡⭐ (LE bug discret prioritaire)
- **Objectif** : implémenter la formule double-latch des registres de scroll BG
  (BGnHOFS/BGnVOFS). Actuellement `(hi<<8)|lo & 0x3FF` naïf, un seul latch.
- **Pourquoi prioritaire** : **mis-scrolle le H-scroll sub-tile — très fréquent**
  (la majorité des jeux à scrolling fin). Seul bug PPU discret survivant.
- **Où** : `crates/luna-ppu/src/ppu.rs:~662` (`bg_scroll_latch`, écritures
  `$210D-$2114`).
- **Référence** : ares `ppu/io.cpp:308-340` — H = `(data<<8) | (latchHOFS & ~7) |
  (latchVOFS & 7)` ; **deux** latches, les 3 bits bas viennent du byte écrit *deux*
  écritures auparavant. Mesen `SnesPpu.cpp:2000` identique.
- **Étapes** : ajouter les deux latches PPU1/PPU2 ; appliquer la formule par
  composante H (avec `& ~7` et `& 7` croisés) ; V a sa propre variante.
- **Validation** : test unitaire write-twice ; **GUI** (règle audible/visible) sur
  un jeu à H-scroll fin (SMW overworld, CT) ; golden PNG si dispo.
- **Effort** : 🟡 · **Dépendances** : aucune.

### 1.2 — Bus : ROM mirroring non-pow2 🟢
- **Objectif** : mirrorer l'image ROM dans la fenêtre d'adresse au lieu de renvoyer
  open-bus pour les ROMs sous-dimensionnées.
- **Où** : `crates/luna-bus/src/lorom.rs:~46`, `hirom.rs:~103` (renvoient `None` si
  `offset >= rom.len()`).
- **Référence** : ares `memory_inline.hpp` `mirror()` ; Mesen `mappings.cpp:21`
  (`page % size` wrap).
- **Étapes** : porter `mirror(addr, size)` (repli modulo la taille réelle) ;
  remplacer le `None` par l'accès mirroré ; **adapter les tests existants**
  `reads_past_rom_end_return_none` (qui figent le mauvais comportement).
- **Validation** : test unitaire mirroring ; `cargo test --workspace --lib`.
- **Effort** : 🟢 · **Dépendances** : aucune.

### 1.3 — Bus : open-bus = dernier MDR (pas 0xFF) 🟡
- **Objectif** : modéliser l'open-bus comme le dernier *Memory Data Register*
  latché, pas une constante `0xFF`.
- **Où** : `crates/luna-core/src/snes.rs` (tous les `unwrap_or(0xFF)` ~1151+) ;
  ajouter un champ `open_bus: u8` au bus.
- **Référence** : ares `cpu_memory.cpp:13` (last MDR) ; Mesen `mm.cpp:278`
  (`_openBus` latch).
- **Étapes** : latcher chaque lecture/écriture réussie dans `open_bus` ; renvoyer
  `open_bus` sur accès non mappé.
- **Validation** : test unitaire open-bus ; non-régression golden.
- **Effort** : 🟡 (touche le chemin chaud) · **Dépendances** : aucune.

### 1.4 — Cartouche : scoring de détection mapper + bon octet SA-1 🟡
- **Objectif** : remplacer « premier checksum-pass gagne » par un **scoring
  pondéré multi-offset** ; détecter SA-1 par le hi-nibble du RomType `$FFD6`.
- **Où** : `crates/luna-cartridge/src/lib.rs:~179` (`detect_and_parse`), `:~243`
  (`mapper_from_byte`).
- **Référence** : ares `mia/medium/super-famicom.cpp:820` (`scoreHeader()` ×4
  offsets) ; Mesen `cart.cpp:125` (`GetHeaderScore()` ×6). Pour SA-1 :
  RomType `$26` hi-nibble (le GSU est déjà correct via `$FFD6`).
- **Étapes** : implémenter `score_header(offset)` (checksum, reset vector,
  cohérence MapMode, taille) ; choisir le meilleur score ; router SA-1 sur RomType.
- **Validation** : tester sur le corpus de ROMs (commercial gitignored + Peter
  Lemon) ; vérifier que SMRPG/Kirby (SA-1) détectent toujours juste.
- **Effort** : 🟡 · **Dépendances** : aucune.

### 1.5 — DSP : tests golden-vector PCM 🟡⭐ (sécurise l'A−)
- **Objectif** : combler **le plus gros angle mort de test** — le port DSP le plus
  fidèle n'a **aucun** test décodant un vrai BRR et assertant le PCM.
- **Où** : `crates/luna-apu/src/dsp.rs` (les 6 tests actuels = silence/KON/counter/
  roundtrip ; `brr_decode:557` non couvert).
- **Référence** : capturer des vecteurs PCM depuis **ares headless** (piloter un SPC
  + séquence de registres connue à travers `ares/sfc/dsp`), ou blargg/`spctool`.
- **Étapes** : commiter quelques fixtures (séquence SPC → PCM attendu) ; test qui
  fait tourner le DSP et assert sample-pour-sample ; gater en CI.
- **Validation** : le test lui-même + non-régression audio GUI (règle audible).
- **Effort** : 🟡 · **Dépendances** : accès à un binaire ares headless (ou corpus).

### 1.6 — SA-1 : timing d'instruction (sortir du plat 6 mclk) 🔴
- **Objectif** : remplacer le budget fixe `MCLK_PER_SA1_INSN = 6` + `io_cycle`
  no-op par un coût par-accès réel (même « grammaire » que le chantier 3).
- **Où** : `crates/luna-core/src/coproc/sa1.rs:~58,~148,~313`.
- **Référence** : ares SA-1 `step(2)` par accès + conflits bus ; Mesen per-access.
- **Note** : c'est de l'architectural — à traiter **avec** le chantier 3 (même
  modèle de scheduling), pas isolément.
- **Effort** : 🔴 · **Dépendances** : chantier 3.

---

## CHANTIER 2 — Frontend & produit (exposer ce qui existe déjà) 🎁

> Tout passe par **`luna-api`** (règle `api-first`). Ne jamais atteindre
> `luna-core` depuis la GUI.

### 2.1 — Câbler les panneaux de débogage GUI 🟡🎁⭐
- **Objectif** : exposer dans la GUI l'introspection **déjà présente** dans l'API.
- **Où** : `crates/luna-gui/src/` (nouvelles `egui::Window`) ; données via
  `luna_api::Emulator` (`state()`, `peek_memory`, `vram_bytes`, `decode_sprites`).
- **Étapes** (incrémental, un panneau par PR) :
  1. **CpuState** (registres 65c816 + flags) — `EmulatorState.cpu`.
  2. **PpuState + sprites** — `decode_sprites()` (liste déjà fournie).
  3. **Hex viewer mémoire** — `peek_memory(bank,offset,count)`, fetch paresseux
     façon ness (`DebugCpuAccess` debug-safe existe côté API).
  4. **VRAM tile-viewer** — `vram_bytes()` → décoder tuiles en texture egui.
  5. **Palette CGRAM** — grille de couleurs.
- **Référence d'UX** : ness `debug_views/` (hex paresseux, debug-safe), jgenesis
  (textures egui pour VRAM/palette).
- **Validation** : **GUI** (règle visible) — ouvrir chaque panneau sur un jeu connu.
- **Effort** : 🟡 (petit par panneau) · **Dépendances** : 0 (API prête).

### 2.2 — Désassembleur 65C816 + SPC700 🟡🎁
- **Objectif** : décoder les opcodes en mnémoniques ; brancher sur `--cpu-trace`/
  `--superfx-trace` et une vue GUI.
- **Où** : nouveau module dans chaque crate CPU (feature-gated `disasm`, comme
  ness), exposé via `luna-api`.
- **Référence** : ness `core/src/cpu/disasm/` (65C816) + SPC700 — modèle direct,
  même couple de CPU.
- **Étapes** : table mnémonique + décodage de mode d'adressage par opcode ; sortie
  texte ; intégrer aux traces et à une `egui::Window` de disasm live.
- **Validation** : tests unitaires de décodage (quelques opcodes connus) ; GUI.
- **Effort** : 🟡 · **Dépendances** : utile après 2.1.

### 2.3 — Save-state / load-state 🟡🧱🎁
- **Objectif** : sérialiser/restaurer l'état complet du `Snes` (ROM partagée), pas
  juste le snapshot d'introspection. Débloque rewind + fast-forward + « rejouer le
  bug » (qui sert la méthode différentielle).
- **Où** : `luna-api` (méthodes `save_state()/load_state()`), `luna-core/src/snes.rs`
  (dériver `serde` sur l'état mutable, ROM en `Arc` partagé).
- **Référence** : jgenesis `PartialClone` (clone l'état en partageant la ROM
  read-only) ; format versionné (`save_state_version`).
- **Étapes** : `#[derive(Serialize, Deserialize)]` sur l'état émulé ; exclure/`Arc`
  la ROM ; version de format ; API + bouton GUI.
- **Validation** : round-trip (save → load → état identique byte-à-byte) ; GUI.
- **Effort** : 🟡 · **Dépendances** : aucune ; **prérequis** de rewind/fast-forward.

### 2.4 — Dynamic Rate Control (DRC) audio 🟡⭐
- **Objectif** : verrouiller l'émulation 60,0988 Hz sur l'horloge réelle du device
  audio, sans artefact de pitch ni drop/dup de frame. luna a déjà le resampler +
  l'audio-clock ; il manque la boucle profondeur-de-queue → ratio.
- **Où** : `crates/luna-gui/src/audio.rs` + une méthode API
  `update_audio_output_frequency` (façon jgenesis).
- **Référence** : jgenesis `common/jgenesis-common/src/audio.rs:64-91` — tous les
  20 frames, ajuster le ratio de resample de ≤±0,5 % selon la profondeur de queue.
- **Étapes** : mesurer la profondeur du ring `ringbuf` ; calculer un delta de ratio
  borné ; le feeder au resampler Hermite.
- **Validation** : **écoute GUI** (règle audible) — plus de micro-stutter/beat sur
  une session longue.
- **Effort** : 🟡 · **Dépendances** : aucune.

### 2.5 — Options de rendu (aspect-ratio + integer-scaling) 🟢🎁
- **Objectif** : combler le minimum « jouable » sans renier le minimalisme
  diagnostique (garder un mode 1:1 nu par défaut).
- **Où** : `crates/luna-gui/src/` (calcul du quad de présentation côté `pixels`).
- **Étapes** : option aspect-ratio (4:3 NTSC vs pixels carrés vs étiré) ; option
  integer-scaling. Pas de shader CRT (hors scope, optionnel plus tard).
- **Validation** : GUI.
- **Effort** : 🟢 · **Dépendances** : aucune.

### 2.6 — Rewind & fast-forward 🟢🎁
- **Objectif** : confort standard, quasi-gratuit une fois 2.3 en place.
- **Où** : `luna-gui` (anneau de save-states pour rewind ; multiplicateur de step
  pour fast-forward).
- **Référence** : jgenesis `mainloop/rewind.rs`.
- **Effort** : 🟢 · **Dépendances** : **2.3 (save-state)**.

---

## CHANTIER 3 — Architecture timing (la dette lourde, 🧱🔴)

> ⚠️ **Réfutation 2026-06-10** : la Phase 5 DMA n'est **PAS** le correctif du
> scintillement Doom (les DMA ne chevauchent pas les lignes d'IRQ). Le vrai
> problème : la boucle Doom tourne **~3,3× moins souvent** que sous Mesen —
> déficit multi-facteurs non isolable chirurgicalement (décalage de boot). Voir
> `luna_rapport_final.md` §6 et `docs/cooperative_scheduler_reference.md` §4b.

### 3.1 — Oracle par injection d'état complet 🔴🧱 (LE déblocage méthodologique)
- **Objectif** : créer le **seul oracle confond-free** pour les bugs de timing
  profonds : injecter un savestate Mesen (CPU+WRAM+PPU+GSU+APU) dans luna, avancer
  les deux, **bisecter la première divergence** dans la boucle de jeu.
- **Pourquoi** : élimine le décalage de boot-frame qui confond toute comparaison
  luna-vs-Mesen. Sans lui, le patching chirurgical est épuisé.
- **Où** : nouveau harness (façon `gsu_trajectory`), API d'injection d'état complet
  dans `luna-api` (prérequis : le save-state 2.3 généralisé à l'injection).
- **Étapes** : parser un savestate Mesen ; mapper sur l'état luna ; runner pas-à-pas
  comparant CPU/mémoire à chaque instruction ; rapport de 1re divergence.
- **Validation** : le harness retrouve une divergence connue ; oracle Doom
  (4 IRQ/frame réguliers comme Mesen).
- **Effort** : 🔴 · **Dépendances** : 2.3 (sérialisation d'état) ; binaire Mesen.

### 3.2 — Réarchitecture cycle-based (le correctif fondamental) 🔴🧱
- **Objectif** : passer du modèle instruction-atomique + lump-DMA au modèle
  **cycle-driven** de jgenesis : 1 cycle/`tick`, tous les composants avancés par la
  tranche d'accès, IRQ ré-évalué chaque cycle, **DMA steppé par octet**.
- **Où** : `luna-core/src/snes.rs` (boucle maître), trait bus CPU
  (`read/write/idle`), `dma/`.
- **Référence** : jgenesis `cpu/wdc65816-emu/src/traits.rs:4-18` + `api.rs:284-403`
  (ordre DMA→CPU→APU→coproc→PPU→IRQ) ; ness `cpu/dma.rs:336` (DMA per-unit).
- **Détails à porter soigneusement** (game-specific) : writes-lead-reads d'un cycle
  (Rendering Ranger R2), interrupt latché à travers DMA + délai 1-cycle post-DMA
  (Wild Guns), DMA aligné 8-mclk re-aligné cycle entier en fin.
- **Étapes** : staged (le plan §5 phases 4-5), chaque phase shippable + validée par
  Tom Harte `cycles[]` + smoke audio/visuel + sweep coproc.
- **Validation** : Tom Harte cycle-counts ; Doom oracle (3.1) ; gsu_* byte-exact ;
  pas de régression GSU/SA-1.
- **Effort** : 🔴 (multi-PR, multi-semaine) · **Dépendances** : 3.1 fortement
  recommandé d'abord (pour mesurer le progrès confond-free).

### 3.3 — Phase 5 DMA stepping (recadrée) 🔴
- **Objectif** : préemption HDMA-vs-DMA mid-line + DMA per-byte sur grille
  master-clock. **Justifiée pour l'accuracy HDMA** (Rendering Ranger, Wild Guns),
  **PAS** comme fix Doom.
- **Où** : `dma/controller.rs` (burst atomique → state-machine per-unit),
  `snes.rs` (préemption au franchissement H≈278 dots).
- **Référence** : jgenesis `dma.rs:339-421`, ness `dma.rs:336-356`.
- **Effort** : 🔴 · **Dépendances** : sous-ensemble de 3.2.

---

## CHANTIER 4 — Couverture coprocesseurs (périmètre, optionnel) 

### 4.1 — DSP-1 🟡
- **Objectif** : Mario Kart / Pilotwings avec la vraie puce (uPD77C25).
- **Référence** : jgenesis `snes-coprocessors/upd77c25/` ; ares ; golden vectors.
- **Effort** : 🟡 · **Dépendances** : aucune.

### 4.2 — S-DD1 / SPC7110 / CX4 (plus tard) 🔴
- Réservés dans `MapperKind`, non implémentés. Faible priorité (titres rares).

---

## 📅 Séquencement recommandé (roadmap)

**Sprint 1 — Quick wins & exposition (1 semaine, ⭐🎁)**
0.1 scorecard regen · 0.2 commentaires · 2.1 panneaux GUI · 2.5 options rendu ·
1.2 ROM mirroring · 1.1 BG scroll write-twice.
> *Effet : la valeur du backend devient visible, le seul bug PPU discret tombe,
> les docs cessent de mentir. Faible risque, fort ressenti.*

**Sprint 2 — Confort & sécurité (1-2 semaines, 🎁🧱)**
2.3 save-state · 2.6 rewind/fast-forward · 2.4 DRC · 1.5 DSP golden-vectors ·
2.2 désassembleur · 1.3 open-bus MDR · 1.4 détection mapper.
> *Effet : luna devient un émulateur « complet » côté produit ; le port DSP est
> sécurisé ; save-state pose la fondation de l'oracle.*

**Sprint 3 — La dette lourde (multi-semaine, 🧱🔴)**
3.1 oracle injection d'état (sur la base de 2.3) → 3.2 réarchitecture cycle-based
(staged) → 3.3 Phase 5 DMA + 1.6 timing SA-1 (même grammaire).
> *Effet : attaque le déficit Doom 3,3× par l'oracle confond-free, puis le
> correctif fondamental. Le seul chantier vraiment risqué — à ne lancer qu'avec
> l'oracle en place.*

**Optionnel** : 4.1 DSP-1 quand le périmètre de jeux le justifie.

---

## 📊 Matrice impact / effort (vue synthétique)

| Action | Impact | Effort | Priorité |
|---|:---:|:---:|:---:|
| 0.1 scorecard regen | moyen | 🟢 | ⭐ |
| 1.1 BG scroll write-twice | **élevé** (très fréquent) | 🟡 | ⭐ |
| 2.1 panneaux GUI | **élevé** (ressenti) | 🟡 | ⭐🎁 |
| 2.4 DRC | élevé (qualité A/V) | 🟡 | ⭐ |
| 1.5 DSP golden-vectors | élevé (risque latent) | 🟡 | ⭐ |
| 2.3 save-state | élevé (fondation) | 🟡 | 🧱🎁 |
| 2.5 options rendu | moyen | 🟢 | 🎁 |
| 1.2 ROM mirroring | moyen | 🟢 | — |
| 2.2 désassembleur | moyen | 🟡 | 🎁 |
| 1.3 open-bus / 1.4 détection | faible-moyen | 🟡 | — |
| 3.1 oracle injection d'état | **très élevé** (débloque le diag) | 🔴 | 🧱 |
| 3.2 réarchitecture cycle-based | **très élevé** (fix fondamental) | 🔴 | 🧱 |
| 3.3 Phase 5 / 1.6 SA-1 timing | moyen (HDMA accuracy) | 🔴 | — |
| 4.1 DSP-1 | moyen (périmètre) | 🟡 | — |

**Règle d'or** : épuiser Sprint 1+2 (petits, indépendants, fort ressenti) **avant**
de lancer Sprint 3 (lourd, risqué) — et n'attaquer le timing profond qu'**avec
l'oracle d'injection d'état**, jamais en patching chirurgical (épuisé).
