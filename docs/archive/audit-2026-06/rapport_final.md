# 🌙 Luna — Rapport final exhaustif
## Architecture, comparaison (ness / jgenesis), frontend, scorecard re-groundé & verdicts

> **Date** : 2026-06-10 · **Cible** : émulateur SNES `luna` (Rust, 11 crates).
> **Comparé à** : `ness` (kelpsyberry, SNES) et `jgenesis` (jsgroth, multi-systèmes).
> **Documents-source associés** (dans /tmp) : `luna_architecture.md`,
> `ness_architecture.md`, `jgenesis_architecture.md`, `luna_scorecard_regrounded.md`,
> `luna_tribunal.md`, `luna_plan_action.md` (les plans d'action détaillés).
> Re-groundé contre HEAD : tout fait `fichier:ligne` est vérifié au code courant.

---

## 0. Résumé exécutif

luna est un émulateur SNES **« high-level accurate »** en Rust, dont la valeur
distinctive tient à **deux piliers uniques** que ni ness ni jgenesis ne possèdent :
l'**API unique** (`luna-api::Emulator` : CLI = GUI = MCP observent le même état) et
la **pilotabilité par agent IA + harnesses différentiels** (méthode de diagnostic).

Trois constats majeurs ressortent du re-grounding :

1. **L'accuracy a fortement progressé depuis le scorecard de mai.** Sur 27 bugs
   documentés, **16 sont corrigés**, 5 partiels, **6 réellement ouverts**. La
   famille « self-consistent but wrong » est presque vidée.
2. **Le frontend est plus mûr qu'on ne le croyait** : le framebuffer est déjà un
   **triple-buffer lock-free** (emprunté à ness). Mais il reste en-dessous de
   jgenesis sur le **rendu GPU** (luna passe par `pixels`, pas de shaders) et la
   **synchro A/V** (pas de Dynamic Rate Control).
3. **Le défaut structurant n'est PAS celui qu'on croyait.** L'enquête Doom du
   2026-06-10 **réfute** que la Phase 5 DMA soit le correctif du scintillement :
   le vrai problème est que **la boucle principale de Doom tourne ~3,3× moins
   souvent** que sous Mesen — un déficit de timing multi-facteurs, non isolable
   chirurgicalement à cause du décalage de boot. Le levier est soit un **oracle
   par injection d'état complet**, soit la **réarchitecture cycle-based**.

---

## 1. Les trois projets en un coup d'œil

| | **luna** | **ness** | **jgenesis** |
|---|---|---|---|
| Auteur | K0b3 | kelpsyberry (*Dust*) | jsgroth |
| Console(s) | SNES | SNES | **11+** (Genesis→GBA, SNES, NES, GB…) |
| Licence | **MPL-2.0** | aucune | GPL v3 |
| Activité | **active (2026)** | dormant (juin 2023) | active |
| Édition Rust | 2024 | 2021 | resolver 3 |
| Crates | 11 | 3 | ~40 (5 familles) |
| Cœur ↔ UI | `luna-api` (contrat unique) | `ness-core` | `EmulatorTrait` (générique) |
| Coprocesseurs | SA-1, Super FX | **aucun** | **tous** (GSU, SA-1, CX4, DSP-1/2/3/4, S-DD1, SPC7110, OBC1, ST018…) |
| Modèle timing | CPU-driven, per-access | CPU-driven, event-queue | **cycle-driven** (1 cycle/tick) |
| `unsafe` | **deny** | autorisé | Miri en CI |

---

## 2. Architecture backend — la comparaison de fond

### 2.1 Modèles de scheduling (le cœur du sujet)

| | Modèle | Granularité IRQ/HDMA | Verdict |
|---|---|---|---|
| **luna** | CPU-driven : instruction entière + `io_cycle` avance PPU/APU/coproc **par accès** | IRQ pollé per-access (pas une barrière) ; DMA **lump atomique** | *défendable*, pas la cause du flicker |
| **ness** | Event-queue master-clock : CPU court jusqu'au prochain event, puis drain ; **DMA steppé par octet** | H/V IRQ = **event pré-planifié** (barrière dure, dot exact) | élégant, mais **zéro coproc** → ne guide pas luna sur SA-1/GSU |
| **jgenesis** | **Cycle-driven** : 1 cycle/`tick`, tous les composants avancés par la tranche (6/8/12 mclk), ordre fixe DMA→CPU→APU→coproc→PPU→IRQ | IRQ ré-évalué **chaque cycle** (fenêtré, rising-edge latch) ; **DMA steppé par octet** | **la vraie référence** d'interleave coproc |

**Convergence des deux références** : ness ET jgenesis steppent le GP-DMA **par
octet** et ré-évaluent l'IRQ entre chaque octet. luna **lump** le DMA → l'IRQ est
pollé une fois *après* le transfert. C'est le grief architectural de fond — mais
voir §6 : pour Doom précisément, ce n'est pas le coupable.

### 2.2 Précision CPU

- **luna** : Tom Harte **99,99996 %** (2 fails/5,08 M) sur 65c816 ; SPC700 256/256.
  Cycles internes (Phase 3) + pénalité branch-taken SPC (Phase 2) **landés**.
- **jgenesis** : **genuinement cycle-based**, validé **correctness ET cycle-counts**
  via TomHarte test-runners par CPU (68000/Z80/6502/65816/SPC700/HuC6280).
- **ness** : event-queue, capé à `next_event_time()`, pas de validation TomHarte
  outillée en CI.

luna et jgenesis sont au coude-à-coude sur la *justesse* du 65c816 ; jgenesis a
l'avantage du *cycle-stepping* natif (writes-lead-reads, cas sub-instruction).

---

## 3. 🎨 Frontend — comparaison APPROFONDIE des stacks (wgpu & co.)

C'est la question centrale de cette passe. Voici les stacks **exacts** (versions
réelles des `Cargo.toml`).

### 3.1 Tableau comparatif des bibliothèques frontend

| Préoccupation | **luna** | **ness** | **jgenesis** |
|---|---|---|---|
| **Rendu GPU** | **`pixels` 0.17** (wgpu **29** sous le capot) | **`wgpu` 0.12** (brut) | **`wgpu`** (brut) |
| → niveau d'usage wgpu | **indirect** : blit quad texturé, nearest-neighbour, **0 shader custom** | **direct** : host imgui + blit | **direct** : pipeline + **shaders WGSL + GLSL** |
| → post-traitement | aucun | aucun | **prescale entier, filtre linéaire, blur CRT horizontal, anti-dither, aspect-ratio** |
| **Fenêtrage / event-loop** | **`winit` 0.30** | `winit` 0.26 (patch git) | **SDL3** |
| **Toolkit UI** | **`egui` 0.34** (+ `egui-wgpu` + `egui-winit`) sur le device wgpu de pixels | **Dear ImGui 0.8** (`imgui-rs`) | `egui` (via `egui-sdl3-platform`) |
| **Dialogue fichier** | `rfd` 0.15 | `rfd` | natif SDL3 |
| **Sortie audio** | **`cpal` 0.16** + `ringbuf` 0.4 (SPSC lock-free) | `cpal` 0.13 + spin_loop | **SDL3 audio** |
| **Resampler** | **Hermite cubique 6-points** (Niemitalo) | cubique | `common/dsp` |
| **DC blocker** | **oui, 5 Hz** | **non** | — |
| **Handoff de frame** | **`triple_buffer` lock-free** ✅ (ex-Mutex, migré) | **`triple_buffer` lock-free** | **`sync_channel(1)` present handshake** |
| **Synchro A/V** | audio-as-clock, **PAS de DRC** | audio-as-clock + busy-wait | audio-as-clock **+ DRC** (±0,5 %/20 frames, 3 modes) |
| **Frontend web/WASM** | **aucun** (cible wasm32 compile l'API seule) | oui (webpack/npm) | **oui, complet** (wasm-pack, CI) |
| **CLI** | **`luna` riche** (run/state/frames/wram-trace/mcp + traces) | aucun | `jgenesis-cli` (clap) |
| **GUI de config** | barre de menus seule | menus imgui | `jgenesis-gui` (launcher egui) |
| **Débogueur graphique** | non câblé (API existe) | imgui (CPU/SPC/hex/disasm) | **egui multi-systèmes** (registres/mémoire/VRAM/palette) |

### 3.2 L'histoire wgpu de luna — un choix délibéré de minimalisme

Le point que tu soulèves est central. **luna n'utilise pas wgpu directement** : il
passe par **`pixels`**, une crate de haut niveau qui fait un simple *blit de quad
texturé* (upload du framebuffer 256×224 RGBA → texture wgpu, upscale
nearest-neighbour). Les commentaires du `Cargo.toml` racine (lignes 142-161)
documentent ce choix :

> *« Minimal pixel-rendering stack… Replaced eframe+egui (2026-05-28) — that
> stack added multi-frame wgpu-state caching that interfered with ROM swaps and
> made every perceived rendering bug ambiguous between core and GUI. »*

Autrement dit : **luna a sciemment retiré eframe et choisi le rendu le plus nu
possible pour la clarté diagnostique** — un pixel émulé = un pixel affiché, aucun
shader susceptible de masquer un bug de rendu. C'est cohérent avec sa doctrine
(luna est d'abord un instrument de fidélité). egui 0.34 tourne en overlay **sur le
même device wgpu que pixels** (versions épinglées egui 0.34 + pixels 0.17 → toutes
deux ciblent wgpu 29, pour éviter de mélanger deux majors de wgpu et casser le pont
de types).

**Classement de sophistication GPU** : `jgenesis ≫ ness > luna`.
**Mais c'est un arbitrage assumé**, pas un oubli : luna troque le polish visuel
(shaders CRT, aspect-ratio) contre la lisibilité de débogage. Le coût réel : aucune
option d'**aspect-ratio** ni d'**integer-scaling** pour l'usage « jouable ».

### 3.3 Là où luna est déjà au niveau (ou devant)

- **Framebuffer lock-free** : luna a **déjà migré** du `Arc<Mutex<Vec<u8>>>` vers
  `triple_buffer` (`main.rs:88`, `emu_thread.rs:222`) — exactement la reco du doc
  de comparaison, **déjà faite**. Le producteur ne bloque jamais, le consommateur
  saute la ré-upload si pas de frame fraîche.
- **DC blocker 5 Hz** : luna l'a, **ness ne l'a pas** (le DSP SNES accumule une
  dérive DC lente — luna est plus propre ici).
- **API-first** : DRC, panneaux debug, etc. s'ajoutent **dans `luna-api`** sans
  casser la cohérence CLI/GUI/MCP. jgenesis boulonne son débogueur à part ; ness
  n'a pas de surface unifiée. **Avantage net de luna.**

### 3.4 Là où luna est en retard (frontend)

| Manque | Référence | Impact |
|---|---|---|
| **Pas de Dynamic Rate Control** | jgenesis `audio.rs:64-91` | deux horloges 60 Hz libres dérivent/battent → micro-stutter audio/vidéo |
| **Rendu nearest-neighbour, 0 option** | jgenesis wgpu+shaders | pas d'aspect-ratio 4:3, pas d'integer-scaling, pas de CRT |
| **Pas de frontend web** | jgenesis `jgenesis-web` | cible wasm32 compilée à vide |
| **Débogueur GUI non câblé** | ness imgui, jgenesis egui | l'introspection riche de l'API n'arrive pas à l'écran |
| **Pas de save-state/rewind/fast-forward** | jgenesis `PartialClone`+rewind | confort utilisateur absent |
| **Pas de désassembleur** | ness (×2 : 65C816+SPC700) | traces en octets bruts |

---

## 4. Débogueur & introspection — comparaison

| Capacité | luna | ness | jgenesis |
|---|:---:|:---:|:---:|
| Surface d'observation unifiée | ✅ **`luna-api`** (CLI=GUI=MCP) | ❌ | ❌ (debugger à part) |
| Pilotage par agent IA (MCP) | ✅ **unique** | ❌ | ❌ |
| Harnesses différentiels (vs ares/Mesen) | ✅ **GSU diff+trajectory, wram-trace NMI-aligné** | ❌ | partiel (TomHarte runners) |
| TomHarte 2 CPU | ✅ | ❌ (non outillé CI) | ✅ (+ cycle-counts) |
| Golden PNG framebuffer | ✅ | ❌ | ❌ |
| Traces CLI (cpu/mem/dma/sa1/superfx) | ✅ **riches** | ❌ | ❌ |
| **Désassembleur** | ❌ | ✅ ×2 | ❌ confirmé |
| **Vues registres GUI** | ❌ (API prête) | ✅ CPU+SPC | ✅ multi-composants |
| **Hex editor GUI** | ❌ (API prête) | ✅ (debug-safe) | ✅ Memory Viewer |
| **Viewers graphiques** (VRAM/CHR/palette/OAM) | ❌ | ❌ | ✅ (textures egui) |
| Breakpoints / watchpoints | ❌ | ❌ | ❌ |
| Save-states / rewind | ❌ | ❌ | ✅ |

**Lecture** : luna domine en **introspection programmatique** (API/CLI/MCP/
différentiel — sans égal), mais est **dernier en introspection visuelle GUI** (le
backend est prêt, le câblage egui manque). ness est fort en désassemblage/hex,
jgenesis en viewers graphiques. **Aucun des trois** n'a de breakpoints.

---

## 5. Scorecard re-groundé — l'état RÉEL de l'accuracy (HEAD, 2026-06-10)

Détail complet dans `/tmp/luna_scorecard_regrounded.md`. Synthèse :

| Sous-système | Note mai | Note re-groundée (estimée) | Reste réellement ouvert |
|---|:---:|:---:|---|
| DSP S-DSP | A− | A− | **zéro test golden-vector** (risque latent) |
| 65c816 | A− | **A−/A** | rien de fonctionnel (DP-8 nu inerte → fix commentaire) |
| SPC700 | B | **B+** | branch-penalty corrigée ; reste cycle ordering fin |
| PPU | C+ | **B** | **BG scroll write-twice** (seul bug discret) ; hi-res/EXTBG faits |
| DMA/HDMA/timing | C+ | **B−** | préemption HDMA mid-line + burst atomique (Phase 5) |
| SA-1 | C+ | **B** | timing instruction plat (architectural) |
| Bus/mappers | C+ | **C+** | ROM mirroring, open-bus MDR, scoring détection |

**Les 6 bugs réellement ouverts** : (1) PPU BG scroll write-twice ; (2) DSP
golden-vectors absents ; (3) ROM mirroring→open-bus ; (4) open-bus=0xFF fixe ;
(5) détection mapper sans scoring + SA-1 mauvais octet ; (6) timing SA-1 plat.
Plus les 2 résidus architecturaux Phase 5 (DMA stepping, préemption HDMA).

---

## 6. ⚠️ Le scintillement Doom — diagnostic réactualisé (2026-06-10)

C'est le point le plus important pour orienter l'effort. Le doc
`emulator_comparison_ness_jgenesis.md` §6 documente une **réfutation instrumentée** :

- Hypothèse « lump-DMA cause le flicker » → **RÉFUTÉE pour Doom** : seulement
  **2 GP-DMA sur 1556** chevauchent les scanlines d'IRQ (23/199) ; les gros DMA
  framebuffer sont aux lignes 200-262 (bas + vblank), **pas sur les lignes d'IRQ**.
  Donc le stepping per-byte (Phase 5) **ne changerait pas** le timing IRQ de Doom.
- **Le vrai signal (confond-free, sur 2000 frames)** :
  - luna : **~0,6 GP-DMA/frame** et 0,74 INIDISP/frame.
  - Mesen : **2,01 GP-DMA/frame** et 2,00 INIDISP/frame, **chaque** frame.
  - ⇒ **la boucle principale de Doom tourne ~3,3× moins souvent sur luna.** C'est
    À LA FOIS le flicker (bordure re-blanked 0,6× au lieu de 2×) ET le « Doom un
    peu plus lent que Mesen ».

**Hypothèses réfutées** : modèle IRQ niveau, arbitrage bus GSU, refresh DRAM,
lump-DMA/Phase 5, vitesse horloge GSU.

**Verdict** : déficit de timing **multi-facteurs et profond**, non isolable par
test chirurgical car **toute comparaison luna-vs-Mesen est confondue par le
décalage de boot-frame irréductible**. Deux issues seulement :
1. **Oracle par injection d'état complet** (injecter un savestate Mesen
   CPU+WRAM+PPU+GSU+APU dans luna, avancer les deux, bisecter la 1re divergence).
2. **Réarchitecture cycle-based** (modèle jgenesis) comme correctif fondamental.

> **Le patching chirurgical est épuisé.** (`docs/cooperative_scheduler_reference.md` §4b)

---

## 7. Verdicts du tribunal (condensé)

Détail dans `/tmp/luna_tribunal.md`. Re-priorisé après re-grounding :

| Chef | Verdict | Statut actuel |
|---|---|---|
| Pas de save-state/rewind/cheat | Coupable | **ouvert** |
| Pas de désassembleur | Coupable | **ouvert** (ness fait mieux) |
| Débogueur GUI coquille | Coupable (dette faible) | **ouvert** (API prête) |
| Rendu nearest-neighbour | Coupable léger | **ouvert** (choix assumé, mais 0 option) |
| Couverture coprocesseurs | Non coupable | assumé (DSP-1 priorisable) |
| **Timing batched vs doctrine** | Coupable — chef principal | **réactualisé** : Doom = déficit multi-facteurs, oracle state-injection requis |
| Cible WASM sans web | Non coupable | garde-fou |
| Angle mort golden-vector DSP | Coupable sur ce point | **ouvert** |
| Doc-comments désynchronisés | Coupable | **ouvert** (+ scorecard & comparison doc périmés) |

---

## 8. Synthèse — la nature réelle de luna

luna n'est **pas** un projet criblé de bugs. C'est un **instrument de recherche
de classe mondiale** (API unique, MCP, différentiel, ports ALU/DSP fidèles, Tom
Harte 99,99996 %, lint/rebuild stricts) qui souffre de **deux déséquilibres** :

1. **Backend ≫ produit** : l'excellence du cœur et de l'introspection n'arrive pas
   jusqu'à l'utilisateur (GUI non câblée, pas de save-state, pas de désassembleur,
   rendu nu, pas de DRC). Or **tout le matériel existe déjà dans `luna-api`** — le
   coût d'exposition est faible.
2. **Un déficit de timing profond et multi-facteurs** (Doom 3,3× lent) qui résiste
   au patching chirurgical et exige soit un oracle par injection d'état, soit la
   réarchitecture cycle-based — la seule dette vraiment lourde.

Et un **risque de méthode récurrent** : les docs (scorecard, comparison) retardent
sur le code, faisant rechasser des bugs déjà morts. La régénération datée des docs
est un quick-win à fort effet de levier.

➡️ **Les plans d'action détaillés sont dans `/tmp/luna_plan_action.md`.**
