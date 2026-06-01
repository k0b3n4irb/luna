# Luna — Architecture

> Émulateur SNES en Rust avec API d'introspection et serveur MCP intégré,
> conçu pour qu'un agent IA puisse **jouer**, **développer** et **déboguer**
> des jeux Super Nintendo de manière autonome.

---

## Sommaire

- [1. Vision & objectifs](#1-vision--objectifs)
- [2. Non-objectifs](#2-non-objectifs)
- [3. Vue d'ensemble](#3-vue-densemble)
  - [3.1 Architecture en couches](#31-architecture-en-couches)
  - [3.2 Modes d'exécution](#32-modes-dexécution)
- [4. Organisation du workspace Rust](#4-organisation-du-workspace-rust)
  - [4.1 Stratégie cross-target async](#41-stratégie-cross-target-async)
- [5. Couche 1 — Bus & mémoire](#5-couche-1--bus--mémoire)
- [6. Couche 2 — Cœur d'émulation](#6-couche-2--cœur-démulation)
  - [6.1 CPU 65C816](#61-cpu-65c816)
  - [6.2 PPU](#62-ppu)
  - [6.3 APU / SPC700](#63-apu--spc700)
  - [6.4 DMA & HDMA](#64-dma--hdma)
  - [6.5 Coprocesseurs](#65-coprocesseurs)
  - [6.6 Scheduler & synchro cycle-accurate](#66-scheduler--synchro-cycle-accurate)
- [7. Couche 3 — Control & introspection API](#7-couche-3--control--introspection-api)
  - [7.1 Control plane](#71-control-plane)
  - [7.2 Debug API](#72-debug-api)
  - [7.3 Semantic API (pour l'IA)](#73-semantic-api-pour-lia)
  - [7.4 Events & subscriptions](#74-events--subscriptions)
- [8. Couche 4 — Serveur MCP](#8-couche-4--serveur-mcp)
  - [8.1 Transport & runtime](#81-transport--runtime)
  - [8.2 Catalogue de tools](#82-catalogue-de-tools)
  - [8.3 Catalogue de resources](#83-catalogue-de-resources)
  - [8.4 Notifications & streaming](#84-notifications--streaming)
  - [8.5 Économie de tokens & coûts MCP](#85-économie-de-tokens--coûts-mcp)
- [9. API-first & écosystème d'usages](#9-api-first--écosystème-dusages)
  - [9.1 L'API est le produit, pas MCP](#91-lapi-est-le-produit-pas-mcp)
  - [9.2 Catalogue de transports](#92-catalogue-de-transports)
  - [9.3 Cas d'usage produit déverrouillés](#93-cas-dusage-produit-déverrouillés)
  - [9.4 Implications architecturales](#94-implications-architecturales)
  - [9.5 `luna-api` comme contrat public stable](#95-luna-api-comme-contrat-public-stable)
- [10. Modèle de threading](#10-modèle-de-threading)
  - [10.1 Cible native](#101-cible-native-linux--macos--windows)
  - [10.2 Cible WASM (Luna Studio Web — V2)](#102-cible-wasm-luna-studio-web--v2)
  - [10.3 Discipline stricte](#103-discipline-stricte)
- [11. Déterminisme & reproductibilité](#11-déterminisme--reproductibilité)
- [12. Stratégie de test](#12-stratégie-de-test)
- [13. Build, distribution, licence](#13-build-distribution-licence)
- [14. Roadmap & phasage](#14-roadmap--phasage)
- [15. Risques & questions ouvertes](#15-risques--questions-ouvertes)
- [16. Glossaire](#16-glossaire)

---

## 1. Vision & objectifs

**Luna** est un émulateur SNES en Rust qui expose la console comme un
**environnement programmable de première classe** pour les agents IA. Là où
les émulateurs traditionnels considèrent l'IA comme un cas d'usage
secondaire (à brancher via OCR sur des screenshots), Luna fait du dialogue
agent ↔ machine un objectif central de design.

**Objectifs**

1. **Fidélité matérielle élevée** : émulation cycle-accurate du CPU 65C816,
   du PPU, du SPC700 et des principaux coprocesseurs (SA-1, Super FX, DSP-1
   en priorité).
2. **API d'introspection riche** : exposer l'état complet de la machine
   (registres, VRAM, OAM, palette, scroll, tilemap, sprites) sous forme
   structurée.
3. **Serveur MCP intégré** : un agent IA (Claude, Cursor, etc.) peut piloter
   l'émulateur via un catalogue de *tools* JSON-RPC standardisés.
4. **Trois modes d'usage assumés** :
   - 🎮 **Play mode** — l'agent joue à un jeu existant.
   - 🛠️ **Dev mode** — l'agent développe un homebrew (hot-reload, profiler).
   - 🐛 **Debug mode** — l'agent débogue un ROM hack (breakpoints, trace,
     time-travel).
5. **Triple mode d'exécution** : *headless* (pour l'IA en production /
   CI), *standalone* (pour un humain qui joue), *spectator* (l'IA joue,
   l'humain observe avec retours visuels et overlays d'activité).
6. **Économie de tokens MCP** : design intentionnel pour qu'une session
   IA de plusieurs heures tienne dans un budget raisonnable (cf. §8.5).
7. **Déterminisme strict** en mode `replay` : un même input + même seed
   produit la même séquence de frames bit à bit.
8. **API-first** : MCP n'est qu'un des transports. La couche 3
   (`luna-api`) est conçue dès le départ comme un contrat public stable
   qui pourra être exposé via REST, WebSocket, WASM, FFI… pour
   débloquer un écosystème d'outils tiers (IDE web homebrew, client
   desktop dev studio, extensions VSCode, etc. — cf. §9).

**Critères de succès mesurables**

| Métrique                              | Cible                              |
|---------------------------------------|------------------------------------|
| Compatibilité SNES (test suite)       | ≥ 99% des ROMs commerciales        |
| Tests bsnes/ares passés                | ≥ 95%                              |
| Performance (release, x86-64 moderne) | 60 fps cycle-accurate à < 30% CPU  |
| Latence MCP tool round-trip           | < 5 ms (stdio local)               |
| Démarrage à froid → ROM chargée       | < 200 ms                           |
| Taille binaire (release stripped)     | < 15 MB                            |
| Budget tokens / heure (profil balanced, gameplay actif) | < 10 M tokens    |
| Latence GUI spectator (event → rendu) | < 16 ms (1 frame)                  |

---

## 2. Non-objectifs

- **Vitesse au détriment de la précision** : Luna n'est pas Snes9x ; on
  privilégie systématiquement la fidélité.
- **Netplay multi-joueurs en ligne** : hors scope V1.
- **Émulation d'autres consoles** : SNES uniquement. (Une factorisation
  future est possible mais ce n'est pas un objectif.)
- **GUI immersive et complexe** : `luna-gui` est volontairement minimal et
  fonctionnel (framebuffer + overlays debug). On ne fait pas concurrence à
  RetroArch côté shaders, post-processing, frontend multimédia.
- **Compatibilité avec les hacks de bas niveau** (overclocking, MSU-1,
  widescreen patches) : possible en V2.

---

## 3. Vue d'ensemble

### 3.1 Architecture en couches

```
┌─────────────────────────────────────────────────────────────────────┐
│                  Couche 4 — Serveur MCP (luna-mcp)                  │
│         JSON-RPC 2.0 over stdio / SSE / Streamable HTTP             │
│                            (tokio async)                            │
├─────────────────────────────────────────────────────────────────────┤
│        Couche 3 — Control & Introspection API (luna-api)            │
│   ┌───────────────┬────────────────┬─────────────┬──────────────┐   │
│   │ Control plane │   Debug API    │ Semantic API│   Events     │   │
│   │ (lifecycle)   │ (breakpoints,  │ (sprites,   │  (vblank,    │   │
│   │               │  registers,    │  tilemap,   │   irq, bp    │   │
│   │               │  trace, mem)   │  scroll…)   │   hits, …)   │   │
│   └───────────────┴────────────────┴─────────────┴──────────────┘   │
├─────────────────────────────────────────────────────────────────────┤
│           Couche 2 — Cœur d'émulation (luna-core)                   │
│  ┌────────┬────────┬────────────┬─────┬───────────┬───────────────┐ │
│  │ 65C816 │  PPU   │ SPC700/DSP │ DMA │ Coproc.   │   Scheduler   │ │
│  │        │        │            │     │ (SA-1, FX)│ (coroutines)  │ │
│  └────────┴────────┴────────────┴─────┴───────────┴───────────────┘ │
├─────────────────────────────────────────────────────────────────────┤
│           Couche 1 — Bus & memory map (luna-bus)                    │
│        Mappers (LoROM, HiROM, ExHiROM, SA-1, SDD-1, …)              │
└─────────────────────────────────────────────────────────────────────┘
                            ▲
                            │
                   ┌────────┴────────┐
                   │   luna-cli      │   luna-gui (egui/wgpu)
                   │ (headless run)  │   standalone & spectator
                   └─────────────────┘
```

Les couches communiquent uniquement par contrats Rust (traits + types
sérialisables). Aucune dépendance directe d'une couche basse vers une
couche haute.

### 3.2 Modes d'exécution

Luna est conçu pour fonctionner sous **quatre modes** combinables, qui ne
sont pas des binaires séparés mais des configurations du même binaire
`luna`. Cela vient du principe que **le cœur d'émulation, l'API
d'introspection et la GUI sont totalement découplés** : on peut allumer ou
éteindre indépendamment chaque "consommateur" du cœur.

#### Mode 1 — Headless (production IA, CI)

```bash
$ luna mcp --rom game.sfc
```

- Aucune fenêtre, aucune dépendance graphique (sur Linux, pas besoin de X
  ou Wayland).
- Serveur MCP stdio en sortie standard.
- Inputs uniquement via `emu_send_input` MCP.
- Outputs uniquement via les tools/resources MCP.
- **Usage** : intégration dans Claude Code/Cursor, déploiement cloud,
  benchmarks AI batch, suite de tests CI.

#### Mode 2 — Standalone (humain joue)

```bash
$ luna run game.sfc
```

- Fenêtre native, framebuffer à 60 fps, audio.
- Inputs clavier/manette.
- Pas de serveur MCP démarré.
- Menu : save states, reset, charger ROM, options vidéo (filtre intégral,
  ratio 4:3, etc.).
- **Usage** : retrogaming classique, vérification manuelle d'un
  comportement, debug humain.

#### Mode 3 — Spectator (l'IA joue, l'humain observe)

```bash
$ luna mcp --rom game.sfc --spectate
```

- Le serveur MCP est actif (l'IA contrôle).
- **Une fenêtre GUI est ouverte en parallèle**, abonnée au même bus
  d'événements que l'agent.
- L'humain voit en temps réel :
  - le framebuffer (ce que voit l'agent)
  - **un panneau "Agent activity"** : timeline des tool calls récents
    (`emu_send_input(B, 30 frames)`, `sem_get_sprites()`, …)
  - **des overlays visuels** : surbrillance des sprites/régions mémoire
    que l'agent a interrogés dans les N dernières secondes
  - les notifications d'événements (`BreakpointHit`, `RomLoaded`)
- L'humain peut à tout moment :
  - **mettre en pause** (l'agent voit sa prochaine requête mise en file
    d'attente)
  - **inspecter** l'état (registres, mémoire) côte-à-côte avec l'agent
  - **reprendre la main** (toggle "human override") pour rejouer une
    section difficile
- **Usage** : **debug de l'agent lui-même** (pourquoi a-t-il choisi cet
  input ?), démos publiques, observation pédagogique.

#### Mode 4 — Coop (humain + IA simultanément, V2)

```bash
$ luna mcp --rom game.sfc --spectate --coop
```

- Inputs humain + inputs MCP fusionnés.
- Cas d'usage : humain pilote P1, IA pilote P2 dans un jeu coop (Joe &
  Mac, Sunset Riders…), ou l'IA suggère et l'humain valide.
- Hors scope V1, mais l'architecture le permet nativement (le sous-système
  d'input agrège déjà plusieurs sources).

#### Architecture du découplage

```
                      Cœur d'émulation
                  ┌─────────────────────┐
                  │  Bus d'événements   │ (broadcast tokio)
                  └─────────┬───────────┘
                            │
            ┌───────────────┼───────────────┐
            │               │               │
            ▼               ▼               ▼
       ┌─────────┐    ┌─────────┐    ┌──────────┐
       │   MCP   │    │   GUI   │    │  Replay  │
       │ server  │    │ (egui)  │    │ recorder │
       └─────────┘    └─────────┘    └──────────┘
       (optionnel)   (optionnel)    (optionnel)
```

Chaque consommateur est **opt-in**. Le mode `--spectate` allume simplement
GUI + MCP simultanément. La GUI ne passe **jamais** par MCP : elle parle
au cœur via le bus interne (latence < 1ms, zéro coût token).

---

## 4. Organisation du workspace Rust

Workspace Cargo avec ~15 crates. Chaque crate est annoté **cross-target**
(compile en natif et `wasm32-unknown-unknown`) ou **native-only** (interdit
en WASM). Cette discipline est vérifiée en CI : `cargo check --target
wasm32-unknown-unknown` sur les crates cross-target.

```
luna/
├── Cargo.toml                       # workspace root
├── ARCHITECTURE.md                  # ce document
├── README.md                        # présentation du projet (porte d'entrée)
├── RESEARCH.md                      # synthèse des recherches pré-Phase-0
├── docs/emulator_landscape.md       # panorama comparatif des émulateurs SNES
│
├── crates/
│   │── # ──────────── CŒUR D'ÉMULATION (cross-target, !Send, no_std-ready) ────
│   ├── luna-bus/                    # ✅ memory map, mappers cartouche
│   ├── luna-cpu-65c816/             # ✅ CPU principal, cycle-accurate
│   ├── luna-cpu-spc700/             # ✅ CPU audio
│   ├── luna-ppu/                    # ✅ Picture Processing Unit
│   ├── luna-apu/                    # ✅ SPC700 + DSP audio (orchestre spc700)
│   ├── luna-dma/                    # ✅ DMA + HDMA
│   ├── luna-coproc/                 # ✅ SA-1, Super FX, DSP-1/2/3/4, etc.
│   ├── luna-cartridge/              # ✅ parsing ROM, détection header, SRAM
│   ├── luna-core/                   # ✅ assemble les composants, scheduler
│   │
│   │── # ──────────── ABSTRACTIONS CROSS-TARGET ─────────────────────
│   ├── luna-async/                  # ✅ façade runtime (spawn/sleep/channels)
│   ├── luna-api/                    # ✅ ★ contrat public stable (couche 3)
│   │
│   │── # ──────────── TRANSPORTS (mix cross-target / native-only) ────
│   ├── luna-mcp-core/               # ✅ types Tool/Resource, schemas
│   ├── luna-mcp-server/             # ❌ rmcp + tokio mainline (native-only)
│   ├── luna-mcp-client/             # ✅ transport WebSocket cross-target
│   ├── luna-rest/                   # ❌ axum + OpenAPI (V1.1, native-only)
│   ├── luna-ws/                     # ❌ tokio-tungstenite (V1.1, native-only)
│   ├── luna-wasm/                   # ⚠️ WASM-only, bindings JS (V2)
│   ├── luna-ffi/                    # ❌ cdylib C/Python (V2, native-only)
│   ├── luna-libretro/               # ❌ core libretro (V2, native-only)
│   │
│   │── # ──────────── BINAIRES & GUI ───────────────────────────────
│   ├── luna-cli/                    # ❌ binaire `luna`, dispatche les modes
│   ├── luna-gui/                    # ⚠️ egui/wgpu (natif + WASM via eframe)
│   └── luna-overlay/                # ⚠️ overlays spectator (natif + WASM)
│
├── tests/
│   ├── roms/                        # test ROMs (krom, blargg, peter_lemon)
│   ├── tom-harte/                   # JSON suite ProcessorTests pour 65C816
│   └── golden/                      # frames de référence pour tests visuels
│
└── tools/
    └── disasm/                      # désassembleur 65C816 standalone
```

Légende : ✅ cross-target / ⚠️ cross-target avec features cfg-gated /
❌ native-only.

**Choix de dépendances clés** (révisés après recherche, cf. RESEARCH.md)

| Domaine             | Crate(s)                                       | Rationale                                                   |
|---------------------|------------------------------------------------|-------------------------------------------------------------|
| Async runtime natif | `tokio` (rt-multi-thread, sync, macros)        | Standard de facto                                           |
| Async runtime web   | `wasm-bindgen-futures` + `gloo-timers`         | Single-thread, microtask queue                              |
| **Façade async**    | **`luna-async`** (crate maison)                | **Évite `#[cfg(target_arch)]` partout — obligatoire**       |
| Channels (cross)    | `futures::channel::mpsc` / `async-channel`     | `crossbeam-channel` **panique** en WASM (cf. RESEARCH.md)   |
| Sérialisation       | `serde` + `serde_json`                         | Indispensable pour MCP                                      |
| MCP serveur         | `rmcp` (officiel Anthropic) — natif uniquement | Pas de support `wasm32-unknown-unknown`                     |
| Schemas             | `schemars` + `ts-rs` (build-time) + `utoipa`   | Génération JSON Schema / TS / OpenAPI                       |
| Rendu (gui)         | `wgpu` + `egui` / `eframe`                     | Cross-platform natif + WASM via WebGPU/WebGL                |
| Audio natif         | `cpal`                                         | Cross-platform low-latency                                  |
| Audio web           | `cpal` (backend wasm-bindgen, output only)     | Bridge Web Audio API ; latence ~50-100ms                    |
| Test 65C816         | `tom-harte/ProcessorTests` (JSON)              | Suite utilisée par jgenesis, dr.beer, etc.                  |
| Tests visuels       | `image` + `pixelmatch`                         | Comparaison golden frames                                   |
| Tracing             | `tracing` + `tracing-subscriber`               | Logs structurés                                             |
| CLI args            | `clap` (derive)                                | Standard                                                    |
| Coroutines          | **aucune** (`genawaiter` rejeté)               | Pattern static dispatch préféré, cf. §6.6                   |

**Décision sur l'architecture interne** : contrairement à ares qui utilise
libco, et contrairement à certains émulateurs Rust qui tentent
`#[coroutine]` (nightly, save-states cassés), Luna utilise le pattern
**CPU-driven master-clock catch-up** validé par jgenesis et tetanes. Cf.
§6.6 pour les détails.

### 4.1 Stratégie cross-target async

Le cœur (`luna-core`, `luna-api`, `luna-mcp-core`) doit pouvoir compiler
en `wasm32-unknown-unknown`. Or :

- `tokio` mainline ne supporte que partiellement WASM
  (`tokio::time` *panique* à l'exécution).
- `crossbeam-channel` ne fonctionne **pas** en WASM (parking primitive
  absente, panic "unreachable").
- `std::thread::spawn` indisponible en WASM single-thread.

**Solution adoptée** : un crate `luna-async` qui expose une API minimale
(`spawn`, `sleep`, `yield_now`, `mpsc`, `oneshot`) avec deux
implémentations conditionnelles :

```rust
// crates/luna-async/src/lib.rs
#[cfg(not(target_arch = "wasm32"))]
mod imp {
    pub use tokio::task::spawn;
    pub use tokio::time::sleep;
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::future::Future;
    pub fn spawn<F: Future<Output = ()> + 'static>(f: F) {
        wasm_bindgen_futures::spawn_local(f);
    }
    pub async fn sleep(d: std::time::Duration) {
        gloo_timers::future::TimeoutFuture::new(d.as_millis() as u32).await;
    }
}

pub use imp::{spawn, sleep};

// Channels cross-target — `futures::channel` fonctionne partout
pub use futures::channel::{mpsc, oneshot};
```

**Discipline `!Send` partout dans le cœur** : single-thread compatible
WASM. Le parallélisme natif passe par des workers explicites
(threads dédiés) dans `luna-mcp-server` natif, jamais dans `luna-core`.

**Conséquence sur la boucle 60Hz** : voir §10 (thread dédié en natif,
`requestAnimationFrame` en WASM, abstrait par un trait `Frontend`).

---

## 5. Couche 1 — Bus & mémoire

Le **bus** est l'objet central qui route les lectures/écritures vers les
bons composants selon l'adresse 24 bits du 65C816 (`$bb:aaaa`).

### Trait `Bus` (vue exposée au CPU)

```rust
pub type MCycles = u64;

pub trait Bus {
    /// Lit un octet. DÉCLENCHE `io_cycle()` en interne avec le coût
    /// d'accès (SLOW=8 / FAST=6 / XSLOW=12 mclk selon la région).
    fn read(&mut self, addr: u32) -> u8;

    /// Écrit un octet. Peut avoir des effets de bord (registres MMIO).
    /// Déclenche également `io_cycle()` avec le coût d'accès.
    fn write(&mut self, addr: u32, value: u8);

    /// **PRIMITIVE CLÉ POUR LA MID-INSTRUCTION ACCURACY.**
    /// Appelée par le CPU à chaque accès bus (lecture, écriture, ou
    /// cycle interne sans accès). Le bus en profite pour rattraper
    /// immédiatement le PPU, traiter HDMA, et tester NMI/IRQ.
    ///
    /// C'est ce qui rend Mario Kart, F-Zero, et tous les jeux à
    /// HDMA/mid-frame effects corrects.
    fn io_cycle(&mut self, mcycles: MCycles);

    /// Sondage des lignes d'interruption après accumulation via io_cycle.
    fn nmi_pending(&self) -> bool;
    fn irq_pending(&self) -> bool;
}

/// Trait pour les composants stockés derrière le bus (PPU, DMA, etc.)
pub trait BusDevice {
    fn read(&mut self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, value: u8);
    fn snapshot(&self) -> Vec<u8>;
    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError>;
}
```

**Pourquoi `io_cycle()` ?** C'est la primitive qui distingue un émulateur
SNES "moderately accurate" d'un cycle-accurate vrai. Sans elle, le PPU
n'est rattrapé qu'entre les instructions CPU — ce qui rate les effets
HDMA, le timing exact des IRQ H/V, et les bugs Mario Kart. Avec elle,
chaque accès mémoire du CPU déclenche un catch-up du PPU jusqu'au cycle
exact, ce qui garantit la précision tout en restant zero-alloc dans la
hot loop. Pattern validé par jgenesis et tetanes.

### Mappers de cartouche

Trait `Mapper` qui implémente `BusDevice` et expose la topologie spécifique
au type de cartouche :

- `LoRom` (mode 20)
- `HiRom` (mode 21)
- `ExHiRom` (mode 25)
- `Sa1Mapper`
- `SuperFxMapper`
- `SDD1Mapper`
- `SPC7110Mapper`

La détection se fait via `luna-cartridge::detect_mapper(&rom_bytes)` qui
parse l'internal header SNES (offset 0x7FC0 ou 0xFFC0).

### Memory map résumée

```
$00–$3F:$0000–$1FFF  → WRAM mirror (LowRAM)
$00–$3F:$2100–$213F  → PPU registers
$00–$3F:$2140–$2143  → APU communication ports
$00–$3F:$4200–$421F  → CPU registers (NMI, IRQ, DMA)
$00–$3F:$4300–$437F  → DMA channels
$00–$3F:$8000–$FFFF  → ROM (via mapper)
$7E–$7F:$0000–$FFFF  → WRAM (128 KB)
$F0–$FF:...          → SRAM (via mapper)
```

---

## 6. Couche 2 — Cœur d'émulation

### 6.1 CPU 65C816

Implémentation cycle-accurate du Western Design Center 65C816 (variante du
6502 16 bits utilisée dans le SNES).

**Particularités du 65C816 à gérer correctement**

- Modes 8/16 bits indépendants pour A et X/Y (via flags M et X du
  registre de statut).
- Banks de 64 KB séparées pour le programme (PB) et les données (DB).
- Mode emulation (E) qui le fait se comporter comme un 6502.
- Tous les modes d'adressage exotiques (direct page, stack relative,
  long indexed…).

**Cœur de l'implémentation**

```rust
pub struct Cpu65C816 {
    // Registres
    a: u16, x: u16, y: u16,
    pc: u16, pb: u8, db: u8,
    sp: u16, dp: u16,
    p: StatusFlags,        // N V M X D I Z C + E
    
    // État de stepping cycle-accurate
    pending_cycles: u8,
    current_instr: Option<Instruction>,
    micro_op_index: u8,
}

impl Cpu65C816 {
    /// Avance d'un cycle maître. Peut être au milieu d'une instruction.
    pub fn tick(&mut self, bus: &mut Bus) -> CycleResult;
}
```

Chaque instruction est décomposée en **micro-ops** datées en cycles, ce
qui permet :
- des breakpoints au cycle près
- une interruption (IRQ/NMI) gérée au timing exact du hardware
- un débogueur "step" qui peut step instruction ou step cycle

### 6.2 PPU

Le PPU SNES est *complexe* : 8 modes graphiques (dont le fameux Mode 7),
4 plans de tiles, 128 sprites, OAM, palette CGRAM, fenêtres de masquage,
mosaïque, color math…

**Décomposition en sous-modules**

```
luna-ppu/
├── src/
│   ├── lib.rs           # struct Ppu, tick()
│   ├── modes/           # rendu des modes 0–7
│   │   ├── mode0.rs ... mode7.rs
│   ├── sprites.rs       # OAM, sprite renderer
│   ├── window.rs        # window masking, color math
│   ├── vram.rs          # VRAM 64 KB
│   ├── cgram.rs         # palette 512 octets
│   └── registers.rs     # $2100-$213F
```

**Rendu** : scanline-based en V1 (plus simple et 99% suffisant), évoluable
vers dot-based pour les démos qui changent les registres en plein milieu
d'un scanline.

**Framebuffer exposé** : `[u8; 256 * 224 * 4]` (RGBA8) accessible en
lecture seule via la couche 3.

### 6.3 APU / SPC700

L'APU SNES est un sous-système quasi-indépendant : un CPU SPC700 (variante
8-bit dérivée du 6502) avec sa propre RAM 64 KB et un DSP audio. Il
communique avec le CPU principal via 4 registres "boîte aux lettres".

**Implication architecturale critique** : le SPC700 tourne à 1.024 MHz
alors que le 65C816 tourne à ~3.58 MHz, et les deux doivent rester
synchronisés. C'est le rôle du scheduler (§6.6).

```rust
pub struct Apu {
    spc700: Spc700,
    dsp: AudioDsp,
    ram: [u8; 65536],
    ports: [u8; 4],      // $2140–$2143 côté CPU
}
```

### 6.4 DMA & HDMA

8 canaux DMA (transferts mémoire ↔ MMIO en burst) + leurs équivalents
HDMA (transferts synchronisés sur le rendu PPU, scanline par scanline).

C'est crucial : la quasi-totalité des effets visuels SNES (parallaxe,
mode 7 dynamique, color math sur fenêtres) repose sur HDMA. Une émulation
incorrecte casse Final Fantasy VI, Chrono Trigger, etc.

### 6.5 Coprocesseurs

| Puce         | Jeux emblématiques            | Priorité |
|--------------|-------------------------------|----------|
| SA-1         | Super Mario RPG, Kirby Super Star | V1   |
| Super FX     | Star Fox, Yoshi's Island, Doom | V1     |
| DSP-1        | Super Mario Kart, Pilotwings  | V1       |
| DSP-2/3/4    | Dungeon Master, SD Gundam GX  | V2       |
| Cx4          | Mega Man X2, X3               | V2       |
| SPC7110      | Far East of Eden Zero         | V3       |
| S-DD1        | Star Ocean, Street Fighter Alpha 2 | V2  |
| OBC1, ST010+ | Quelques niches               | V3       |

Chaque coprocesseur est un crate-feature de `luna-coproc`, ce qui permet
de compiler une build minimale si on cible un jeu spécifique.

### 6.6 Scheduler & synchro cycle-accurate

**Le problème** : faire avancer dans le bon ordre un CPU principal
(21.477 MHz NTSC), un PPU (cadencé par dots/scanlines), un APU à
fréquence indépendante (3.072 MHz SPC700), des DMAs qui volent des
cycles au CPU, et potentiellement un coprocesseur — le tout en restant
déterministe et performant.

**Décision** : on adopte le pattern **CPU-driven master-clock catch-up**
validé par [jgenesis](https://github.com/jsgroth/jgenesis) et
[tetanes](https://github.com/lukexor/tetanes), considérés comme l'état
de l'art Rust en émulation cycle-accurate.

**Patterns rejetés et pourquoi**

| Pattern | Verdict | Raison |
|---|---|---|
| Event-queue `BinaryHeap` (style moa) | ❌ | À 21M cycles/s, l'overhead heap + `Box<dyn>` consomme 50% du budget cycle |
| Coroutines `#[coroutine]` (Lochnes) | ❌ | Nightly only, save-states impossibles (closures non-sérialisables), perf marginale |
| `genawaiter` (stable, mais...) | ❌ | LLVM peine à inliner ; save-states cassés |
| Instruction-step naïf (rboy GB) | ❌ | Pas de mid-instruction accuracy, casse Mario Kart |
| Lazy + `next_event` (gameroy GB) | ⚠️ | À utiliser **en complément** pour optimiser WAI/STP |
| **CPU master-clock catch-up + `io_cycle()` (jgenesis)** | ✅ | **Notre choix** |
| State-machine pure par cycle (DaveTCode NES) | ⚠️ | Excellent pour le CPU isolé, à utiliser dans `luna-cpu-65c816` |

**Pattern adopté — vue d'ensemble**

```
loop {
  delta_mclk = if memory_refresh_pending { 40 }                  // DRAM refresh
              else if dma.active() { dma.tick(bus) }             // DMA vole les cycles
              else { cpu.step(bus) }                             // 1 instruction CPU
                       ↑ pendant cette étape, le CPU appelle
                         bus.io_cycle(n) à chaque accès,
                         ce qui rattrape PPU/HDMA en cours d'instruction
  apu.tick(delta_mclk)                                           // catch-up rationnel
  ppu.catch_up_to(total_mclk + delta_mclk)                       // résidu cycles internes
  total_mclk += delta_mclk
  if ppu.frame_complete() { return }
}
```

**Croquis Rust complet**

```rust
// crates/luna-core/src/scheduler.rs

pub type MCycles = u64;
pub const NTSC_MASTER_HZ: u64 = 21_477_272;
pub const APU_MASTER_HZ:  u64 = 24_576_000;

pub struct Snes {
    pub cpu: Cpu65816,
    pub ppu: Ppu,
    pub apu: Apu,             // SPC700 + DSP, fréquence indépendante
    pub dma: DmaUnit,
    pub cart: Cartridge,
    pub wram: Box<[u8; 0x20000]>,
    pub total_mclk: MCycles,
    pub frame_mclk: MCycles,
    pub memory_refresh_pending: bool,
}

impl Snes {
    /// Une itération = soit 1 instruction CPU, soit 1 cycle DMA, soit
    /// 1 refresh DRAM. Zero-alloc dans la hot loop.
    #[inline]
    pub fn step(&mut self) -> TickEffect {
        let delta = if self.memory_refresh_pending {
            self.memory_refresh_pending = false;
            MEMORY_REFRESH_CYCLES                                    // ~40 mclk
        } else if self.dma.active() {
            // DMA vole les cycles au CPU. Unité = 8 mclk (1 transfert byte)
            self.dma.tick(&mut self.snes_bus())
        } else {
            // CPU exécute UNE instruction. Pendant `step`, bus.io_cycle()
            // rattrape immédiatement PPU + HDMA + IRQ check.
            let mut bus = self.snes_bus();
            self.cpu.step(&mut bus);
            bus.access_master_cycles_total
        };

        // APU à fréquence différente : catch-up à arithmétique
        // RATIONNELLE u64 (pas de float, pas de dérive).
        let apu_eff = self.apu.tick(delta);

        // Résidu PPU (cycles internes CPU sans accès bus → pas rattrapés
        // par io_cycle). Habituellement 0-2 mclk.
        let ppu_eff = self.ppu.catch_up_to(self.total_mclk + delta);

        self.total_mclk += delta;
        self.frame_mclk += delta;

        TickEffect {
            frame_complete: ppu_eff.frame_complete,
            audio_samples: apu_eff.audio_samples,
        }
    }

    pub fn run_to_frame(&mut self, audio_out: &mut Vec<(f32, f32)>) {
        loop {
            let e = self.step();
            audio_out.extend(e.audio_samples);
            if e.frame_complete { return; }
        }
    }
}

// APU catch-up à arithmétique rationnelle — PAS DE FLOAT
impl Apu {
    pub fn tick(&mut self, main_mcycles: MCycles) -> TickEffect {
        // master CPU = 21.477272 MHz, master APU = 24.576 MHz
        self.numerator += main_mcycles * APU_MASTER_HZ;
        while self.numerator >= NTSC_MASTER_HZ {
            self.numerator -= NTSC_MASTER_HZ;
            self.spc700.step(&mut self.bus);
            self.timer0.tick(); self.timer1.tick(); self.timer2.tick();
            if self.sample_divider.tick() { self.emit_sample(); }
        }
        TickEffect::default()
    }
}
```

**Pourquoi ça marche** (à conserver en tête lors de l'implémentation) :

1. **Zero-alloc dans la hot loop** — pas de `Box<dyn>`, pas de
   `BinaryHeap`, pas de `Vec::push` par cycle. Static dispatch partout.
2. **Mid-instruction accuracy gratuite** — `bus.io_cycle()` rattrape le
   PPU à chaque accès, donc HDMA scanline-précis, IRQ H/V exacts, et
   les bugs Mario Kart sont correctement reproduits.
3. **Pas de dérive APU/CPU** — arithmétique rationnelle u64 (pas de
   float). Vérifiable : après 1h d'émulation, `apu.cycle_count() ≈
   apu_freq * elapsed`.
4. **Save states triviaux** — tous les champs sont des `struct` concrets
   `serde::Serialize` (impossible avec coroutines/closures).
5. **Run-ahead / netplay possibles** — `step()` est pur, on peut cloner
   l'état entier et le rejouer.
6. **Borrow checker compatible** — pattern `SnesBus<'a>` créé à chaque
   step qui emprunte les champs séparément (`&mut self.ppu, &mut
   self.wram, …`). Pas de `Rc<RefCell>` dans la hot loop.

**Risques résiduels & mitigations** : cf. §15.

**Fichiers de référence à étudier en Phase 0** (lecture seule, GPL-3.0
incompatible avec copie) :

- [`jgenesis/backend/snes-core/src/api.rs`](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/api.rs#L284)
  — `Snes::tick` ligne 284 (modèle direct)
- [`jgenesis/backend/snes-core/src/apu.rs`](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/apu.rs#L274)
  — catch-up rationnel ligne 274
- [`jgenesis/backend/snes-core/src/memory/dma.rs`](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/memory/dma.rs)
  — DMA/HDMA timing
- [`jgenesis/backend/snes-core/src/bus.rs`](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/bus.rs)
  — calcul `access_master_cycles` par région mémoire
- [`tetanes-core/src/cpu.rs`](https://github.com/lukexor/tetanes/blob/main/tetanes-core/src/cpu.rs#L280)
  — pattern `start_cycle`/`end_cycle` (NES mais transposable)

---

## 7. Couche 3 — Control & introspection API

C'est la couche qui définit **ce qu'on peut faire avec la machine** sans
parler de MCP encore. Elle est exposée par le crate `luna-api` sous forme
de traits Rust async, indépendants du protocole.

### 7.1 Control plane

```rust
#[async_trait]
pub trait EmulatorControl {
    async fn load_rom(&self, path: &Path) -> Result<RomInfo>;
    async fn load_rom_bytes(&self, bytes: Vec<u8>) -> Result<RomInfo>;
    async fn reset(&self) -> Result<()>;
    async fn pause(&self) -> Result<()>;
    async fn resume(&self) -> Result<()>;
    async fn step_instructions(&self, count: u32) -> Result<StepResult>;
    async fn step_cycles(&self, count: u64) -> Result<StepResult>;
    async fn step_frames(&self, count: u32) -> Result<StepResult>;

    async fn save_state(&self) -> Result<SaveStateId>;
    async fn load_state(&self, id: SaveStateId) -> Result<()>;
    async fn list_states(&self) -> Result<Vec<SaveStateInfo>>;

    async fn screenshot(&self) -> Result<Screenshot>;  // PNG bytes
    async fn send_input(&self, port: u8, buttons: Buttons, frames: u32) -> Result<()>;
}
```

### 7.2 Debug API

```rust
#[async_trait]
pub trait EmulatorDebug {
    // Registres
    async fn cpu_registers(&self) -> CpuRegisters;
    async fn apu_registers(&self) -> ApuRegisters;
    async fn ppu_registers(&self) -> PpuRegisters;

    // Mémoire
    async fn read_memory(&self, space: MemSpace, addr: u32, len: u32) -> Vec<u8>;
    async fn write_memory(&self, space: MemSpace, addr: u32, data: Vec<u8>) -> Result<()>;

    // Breakpoints
    async fn add_breakpoint(&self, bp: Breakpoint) -> Result<BpId>;
    async fn remove_breakpoint(&self, id: BpId) -> Result<()>;
    async fn list_breakpoints(&self) -> Vec<BreakpointInfo>;

    // Désassemblage
    async fn disassemble(&self, addr: u24, count: u32) -> Vec<DisasmLine>;

    // Trace
    async fn start_trace(&self, filter: TraceFilter) -> Result<TraceId>;
    async fn stop_trace(&self, id: TraceId) -> Result<TraceLog>;
}

pub enum MemSpace { Wram, Vram, Oam, Cgram, Sram, ApuRam, Rom }

pub enum Breakpoint {
    Exec   { addr: u24, condition: Option<Expr> },
    Read   { addr: u24, len: u32 },
    Write  { addr: u24, len: u32, value_match: Option<u8> },
    Vblank,
    Hblank { scanline: u16 },
    DmaStart { channel: u8 },
}
```

### 7.3 Semantic API (pour l'IA)

**C'est ici que Luna se différencie de tous les autres émulateurs.** On
expose la *sémantique* du frame courant, pas seulement ses pixels, pour
qu'un agent puisse "comprendre" la scène sans pipeline de vision.

```rust
#[async_trait]
pub trait EmulatorSemantic {
    /// Tous les sprites OAM avec leur état décodé.
    async fn sprites(&self) -> Vec<Sprite>;

    /// Les 4 backgrounds, leur mode, leurs registres de scroll.
    async fn backgrounds(&self) -> [Background; 4];

    /// La région de tilemap actuellement visible pour un BG donné.
    async fn visible_tilemap(&self, bg: u8) -> Tilemap;

    /// Palette CGRAM décodée en couleurs RGB.
    async fn palette(&self) -> [Color; 256];

    /// Mode graphique actif ($2105).
    async fn graphics_mode(&self) -> GraphicsMode;

    /// État des fenêtres et color math.
    async fn window_state(&self) -> WindowState;
}

pub struct Sprite {
    pub index: u8,
    pub x: i16, pub y: i16,
    pub size: SpriteSize,
    pub tile_index: u16,
    pub palette: u8,
    pub priority: u8,
    pub flip_h: bool, pub flip_v: bool,
    pub on_screen: bool,
}
```

**Bonus** : un système optionnel de **annotations par jeu** (`game_maps/`),
qui mappe des adresses RAM connues à des noms sémantiques :

```toml
# game_maps/super_mario_world.toml
[memory.ram]
"player_x"     = { addr = 0x7E0094, type = "u16le" }
"player_y"     = { addr = 0x7E0096, type = "u16le" }
"player_state" = { addr = 0x7E0071, type = "u8" }
"score"        = { addr = 0x7E0F34, type = "u24le" }
"coins"        = { addr = 0x7E0DBF, type = "u8" }
"lives"        = { addr = 0x7E0DBE, type = "u8" }
```

L'agent peut alors faire `read_named("player_x")` au lieu de mémoriser des
adresses hex.

### 7.4 Events & subscriptions

Beaucoup de cas d'usage nécessitent que l'agent réagisse à un événement
plutôt que de poller. On expose un canal d'événements async :

```rust
#[async_trait]
pub trait EmulatorEvents {
    async fn subscribe(&self, filter: EventFilter) -> EventStream;
}

pub enum EmulatorEvent {
    FrameComplete { frame_number: u64 },
    VBlankStart,
    BreakpointHit { id: BpId, pc: u24 },
    MemoryWatchTriggered { addr: u24, old: u8, new: u8 },
    DmaTransferComplete { channel: u8 },
    Crash { reason: CrashReason },
    RomLoaded { info: RomInfo },
}
```

Côté MCP, ces événements sont publiés comme **notifications** JSON-RPC
(messages serveur→client non-sollicités).

---

## 8. Couche 4 — Serveur MCP

### 8.1 Transport & runtime

**Transports supportés** (dans cet ordre de priorité) :

1. **stdio** — pour intégration locale Claude Code, Cursor, etc.
2. **Streamable HTTP** — pour intégration web / cloud.
3. **SSE** — fallback historique.

Le binaire `luna` lance le serveur MCP en mode stdio par défaut :

```bash
$ luna mcp                            # mode stdio (par défaut)
$ luna mcp --http --port 7878         # mode HTTP
$ luna mcp --rom path/to/game.sfc     # charge la ROM au démarrage
```

Le runtime tokio multi-thread gère la concurrence : un thread dédié pour
le cœur d'émulation (60 fps cadencé), N threads pour les handlers MCP qui
parlent au cœur via canaux crossbeam.

### 8.2 Catalogue de tools

Chaque tool MCP est une fine couche de mapping JSON ↔ appel à `luna-api`.
Schémas JSON générés à partir des structs Rust via `schemars`.

**Tools "control"**

| Tool                    | Description                                  |
|-------------------------|----------------------------------------------|
| `emu_load_rom`          | Charge une ROM depuis un chemin              |
| `emu_reset`             | Reset console                                |
| `emu_pause` / `emu_resume` | Pause/reprise                             |
| `emu_step`              | Avance de N instructions / cycles / frames   |
| `emu_send_input`        | Envoie une séquence de boutons               |
| `emu_screenshot`        | PNG du framebuffer courant                   |
| `emu_save_state`        | Crée un save state, retourne un ID           |
| `emu_load_state`        | Restaure un save state                       |

**Tools "debug"**

| Tool                    | Description                                  |
|-------------------------|----------------------------------------------|
| `dbg_read_memory`       | Lit N octets dans un espace mémoire          |
| `dbg_write_memory`      | Écrit N octets                               |
| `dbg_get_registers`     | Tous les registres CPU/PPU/APU               |
| `dbg_add_breakpoint`    | Pose un breakpoint typé                      |
| `dbg_remove_breakpoint` | Retire un breakpoint                         |
| `dbg_list_breakpoints`  | Liste les breakpoints actifs                 |
| `dbg_disassemble`       | Désassemble N instructions à une adresse     |
| `dbg_trace_start`       | Démarre un trace log filtré                  |
| `dbg_trace_stop`        | Stoppe et retourne le trace                  |

**Tools "semantic"** (l'avantage différenciant de Luna)

| Tool                    | Description                                  |
|-------------------------|----------------------------------------------|
| `sem_get_sprites`       | Liste structurée des 128 sprites actifs      |
| `sem_get_backgrounds`   | Les 4 BG avec mode + scroll                  |
| `sem_get_tilemap`       | Tilemap visible pour un BG                   |
| `sem_get_palette`       | Palette CGRAM décodée                        |
| `sem_read_named`        | Lit une adresse via le mapping nommé du jeu  |
| `sem_load_game_map`     | Charge un fichier d'annotations              |

### 8.3 Catalogue de resources

Les **resources** MCP exposent des contenus que l'agent peut "lire"
(différent des tools qui sont des actions).

| URI                                     | Contenu                              |
|-----------------------------------------|--------------------------------------|
| `luna://state/cpu`                      | JSON registres CPU                   |
| `luna://state/ppu`                      | JSON registres PPU                   |
| `luna://state/framebuffer.png`          | Frame courante en PNG                |
| `luna://state/sprites`                  | JSON sprites OAM                     |
| `luna://memory/wram?addr=…&len=…`       | Dump mémoire                         |
| `luna://disasm?addr=…&count=…`          | Désassemblage texte                  |
| `luna://docs/65c816-opcodes`            | Référence opcodes 65C816 intégrée    |
| `luna://docs/ppu-registers`             | Référence registres PPU              |

Ces docs intégrées permettent à l'agent de consulter la spec sans réseau,
ce qui accélère énormément les itérations debug.

### 8.4 Notifications & streaming

Le serveur émet des notifications JSON-RPC pour les événements abonnés.
Le client MCP les reçoit en push :

```json
{
  "jsonrpc": "2.0",
  "method": "luna/event",
  "params": {
    "type": "BreakpointHit",
    "id": "bp_4",
    "pc": "0x808012",
    "cycle": 1234567
  }
}
```

Côté agent, cela permet le pattern :

```
1. add_breakpoint(exec, 0x808012)
2. resume()
3. (attente passive de la notification "BreakpointHit")
4. get_registers() / read_memory() / disassemble()
5. step / continue
```

### 8.5 Économie de tokens & coûts MCP

#### 8.5.1 Le problème

Un agent qui pilote un émulateur peut très rapidement saturer un quota de
tokens si l'API est designée naïvement. Quelques ordres de grandeur pour
fixer les idées (base : ~4 caractères par token, encodage base64 ajoute
~33% de volume) :

| Donnée brute SNES                       | Taille  | Tokens (naive) |
|-----------------------------------------|---------|----------------|
| Framebuffer RGBA 256×224                | 224 KB  | ~76 000        |
| Framebuffer PNG (couleurs limitées)     | 5–20 KB | ~1 700–6 800   |
| VRAM dump complet                       | 64 KB   | ~22 000        |
| WRAM dump complet                       | 128 KB  | ~44 000        |
| OAM complet (128 sprites, raw)          | 544 B   | ~180           |
| 1 seconde de trace CPU non filtrée      | ~1 MB   | ~340 000       |

À titre de comparaison, un appel Claude Sonnet typique a un *context window*
de l'ordre de 200k tokens. **Un screenshot raw consommerait déjà ~38% de
ce budget** ; un trace log brut, plus que le budget entier. Sans
discipline, un agent jouant 5 minutes peut consommer plusieurs millions
de tokens.

#### 8.5.2 Sept principes de design

1. **Sémantique avant pixels** : par défaut, retourner des structures
   décodées (sprites, scroll, named RAM), pas des bytes.
2. **Filtrer côté serveur** : `visible_only`, `region`, `since_frame`,
   `kind` — pas à l'agent de jeter ce qu'il n'a pas demandé.
3. **Hash + diff** : avant un gros payload, exposer un hash de l'état ;
   l'agent ne fetche que si ça a changé.
4. **Resources plutôt qu'inline** : les gros blobs (PNG, dumps mémoire)
   sont exposés comme **MCP resources** (URI), pas inline dans la
   réponse — l'agent ne paie le coût que s'il choisit explicitement de
   lire la resource.
5. **Niveaux de détail explicites** : tout tool potentiellement coûteux
   expose un paramètre `detail: "thumbnail" | "low" | "medium" | "full"`,
   avec `low` par défaut.
6. **Plafonds durs** : chaque tool a un `max_bytes` interne et tronque
   avec un avertissement structuré plutôt que de retourner 100 KB sans
   prévenir.
7. **Budget annoncé** : chaque réponse inclut un champ
   `estimated_output_tokens` (calculé côté serveur) qui permet à l'agent
   et à l'humain de suivre la consommation en temps réel.

#### 8.5.3 Stratégies concrètes par tool

| Tool                | Naive                  | Avec stratégie Luna       | Économie  |
|---------------------|------------------------|---------------------------|-----------|
| `emu_screenshot`    | PNG inline base64 (~5k)| Resource URI (~50 tokens) ; PNG accessible via `luna://state/framebuffer.png` si besoin | ~99% |
| `sem_get_sprites`   | 128 sprites tous champs (~3k) | `{visible_only: true, fields: ["x","y","tile"]}` → ~500 | ~85% |
| `dbg_read_memory`   | 1 KB de bytes (~340)   | Hash si inchangé (~30) ; bytes si changé | ~90% en régime stable |
| `dbg_trace_start`   | Brut (~340k/s)         | Filtre `{pc_range, ops}` + limite `max_lines` | ~99% |
| `sem_get_tilemap`   | Tilemap complet 32×32×4 (~5k) | Auto-crop à la région visible (~1k) | ~80% |
| `dbg_get_registers` | Tous les registres détaillés (~600) | Catégories : `cpu`, `ppu_minimal`, `apu` (~150 chacun) | ~75% |

#### 8.5.4 Mécanismes mis en œuvre dans l'API

**a) Niveaux de détail standardisés**

```rust
#[derive(Deserialize)]
pub struct ScreenshotParams {
    /// "thumbnail" (32×28, ~150 tokens),
    /// "low" (128×112, ~1.5k tokens),
    /// "full" (256×224, via resource URI seulement)
    #[serde(default = "default_low")]
    detail: DetailLevel,
    /// Si true, retourne juste un hash si la frame n'a pas changé
    /// depuis le dernier appel
    #[serde(default)]
    if_changed_since: Option<FrameHash>,
}
```

**b) Hash-then-fetch pattern**

```rust
#[derive(Serialize)]
pub struct MemoryReadResponse {
    pub addr: u32,
    pub len: u32,
    pub hash: u64,             // toujours retourné
    pub data: Option<Vec<u8>>, // None si hash == precedent_hash (économie)
    pub estimated_output_tokens: u32,
}
```

L'agent peut donc faire : "lis 1KB à 0x7E0000, mais juste le hash si rien
n'a changé". Sur une boucle de polling, ça réduit le coût d'un facteur
10–100x.

**c) Resources pour les gros payloads**

Plutôt que d'inliner un PNG dans une réponse de tool, Luna expose :

```
luna://state/framebuffer.png        → PNG complet
luna://state/vram.bin               → 64 KB VRAM
luna://state/sprites.json           → JSON détaillé tous sprites
luna://state/disasm?addr=…&count=…  → désassemblage texte
```

Le tool `emu_screenshot` retourne par défaut **uniquement** l'URI de la
resource + un thumbnail. L'agent décide s'il "ouvre" la resource. Les
clients MCP comme Claude Code peuvent même prévisualiser sans charger en
contexte.

**d) Filtres standardisés**

Tous les tools "list" supportent des filtres uniformes :

```jsonc
{
  "visible_only": true,        // sprites/tiles à l'écran uniquement
  "region": { "x": 0, "y": 0, "w": 128, "h": 128 },
  "since_frame": 1234,          // delta depuis une frame
  "fields": ["x", "y", "tile"], // projection (économie majeure)
  "limit": 50
}
```

**e) Subscriptions plutôt que polling**

Polling coûte cher (1 tool call/frame × 60 frames/s). On encourage
l'agent à utiliser les notifications MCP pour les events fréquents :

```
✘ Mauvais (polling) :
  while True: screenshot(); analyze(); sleep(...)
  → 60 tools/s × 1k tokens = 60k tokens/s

✓ Bon (event-driven) :
  subscribe("FrameComplete", every=30)
  → 2 notifications/s × 200 tokens = 400 tokens/s
```

**f) Budget tracking transparent**

Chaque réponse contient :

```json
{
  "data": "...",
  "_meta": {
    "estimated_output_tokens": 142,
    "session_tokens_used": 28430,
    "session_tokens_budget": 200000
  }
}
```

L'agent (et la GUI spectator) peut afficher la consommation en temps
réel. Quand on approche du budget, on peut soit alerter, soit dégrader
gracieusement (forcer `detail: thumbnail` automatiquement).

#### 8.5.5 Modes de coût configurables

Au démarrage du serveur MCP, l'utilisateur choisit un profil :

```bash
$ luna mcp --rom game.sfc --cost-profile economy
$ luna mcp --rom game.sfc --cost-profile balanced     # défaut
$ luna mcp --rom game.sfc --cost-profile generous
```

| Profil      | Screenshot default | Memory default      | Trace default     |
|-------------|--------------------|---------------------|-------------------|
| `economy`   | thumbnail          | hash-only           | refusé sans filtre|
| `balanced`  | low                | hash-then-data      | 1k lignes max     |
| `generous`  | medium             | full data           | 10k lignes max    |

Le profil `economy` est conçu pour qu'une session de plusieurs heures
tienne dans un budget raisonnable (typiquement < 5M tokens / heure de
gameplay actif).

#### 8.5.6 Estimation budget de session

Sur un cas d'usage typique "agent qui apprend à jouer Super Mario World"
avec profil `balanced` et boucle event-driven :

| Action                            | Fréquence       | Tokens/appel | Total/min  |
|-----------------------------------|-----------------|--------------|------------|
| FrameComplete subscription        | 2/s (filtré)    | 200          | 24 000     |
| sem_get_sprites (visible)         | 2/s             | 500          | 60 000     |
| sem_read_named (player_x/y/lives) | 2/s             | 80           | 9 600      |
| emu_send_input                    | ~5/s            | 50           | 15 000     |
| emu_screenshot (low) occasionnel  | 0.1/s           | 1 500        | 9 000      |
| **Total**                         |                 |              | **~120 k/min** |

→ **~7M tokens/heure** d'agent actif sur ce profil. C'est tenable sur un
plan API "pro" Anthropic, et nettement plus que les ~80M tokens/heure
qu'on consommerait avec un design naïf à base de PNG full + dumps RAM.

---

## 9. API-first & écosystème d'usages

L'agent IA via MCP n'est qu'un client parmi d'autres possibles. Exposer
Luna comme une API stable ouvre tout un éventail d'outils que la
communauté SNES n'a jamais eu : IDE web pour homebrew, client desktop de
développement, CI pour ROM hacks, plateforme TAS, extension VSCode, etc.
Cette section explicite cette ouverture et ses implications.

### 9.1 L'API est le produit, pas MCP

Quand on regarde l'architecture en couches (§3.1), on constate que les
couches 1 à 3 ne dépendent **jamais** de la couche 4. Le serveur MCP
n'est qu'un **adaptateur** qui traduit JSON-RPC ↔ appels Rust de
`luna-api`.

```
   Vision naïve                       Vision Luna
   ────────────                       ───────────
   ┌─────────────┐                    ┌─────────────┐
   │  Émulateur  │                    │  API stable │ ← le produit public
   └──────┬──────┘                    ├─────────────┤
          │                           │  Émulateur  │ ← l'implémentation
   ┌──────▼──────┐                    └─────────────┘
   │   API MCP   │ ← le produit
   └─────────────┘            ┌─MCP─┬─REST─┬─WS─┬─WASM─┬─FFI─┐
                              └─────┴──────┴────┴──────┴─────┘
                                     ↑ adaptateurs interchangeables
```

C'est le pattern **Ports & Adapters** (architecture hexagonale), adapté à
un produit où le cœur (l'émulation) doit survivre aux évolutions des
protocoles d'accès. Concrètement :

- Le crate `luna-api` n'importe **rien de spécifique MCP**.
- Les types publics sont sérialisables avec `serde` mais agnostiques au
  format (JSON, MessagePack, bincode, protobuf possibles).
- Tout nouveau transport est un crate `luna-transport-X` qui dépend
  uniquement de `luna-api`, jamais l'inverse.

### 9.2 Catalogue de transports

| Transport          | Cas d'usage typique                      | Statut    |
|--------------------|------------------------------------------|-----------|
| **MCP stdio**      | Agent IA local (Claude Code, Cursor)     | V1        |
| **MCP HTTP/SSE**   | Agent IA distant, multi-client           | V1        |
| **REST / HTTP**    | Frontends web, intégrations enterprise   | V1.1      |
| **WebSocket**      | Web temps réel (Luna Studio Web)         | V1.1      |
| **gRPC**           | Clients haute perf, microservices        | V2        |
| **WASM / JS bindings** | Émulateur dans le navigateur         | V2        |
| **FFI / cdylib**   | Intégrations C / Python / Lua / …        | V2        |
| **libretro core**  | Intégration RetroArch                    | V2        |

**Principe** : *un schéma source, plusieurs adaptateurs générés*. À
partir des types `luna-api` annotés avec `schemars::JsonSchema`, on
dérive automatiquement :

- JSON Schema pour les tools MCP.
- OpenAPI 3 pour REST (via `utoipa`).
- Fichiers `.proto` pour gRPC.
- Types TypeScript pour les clients web (via `ts-rs`).
- Bindings Python (via `pyo3`).

Une seule source de vérité, plusieurs surfaces. Le risque de
désynchronisation entre client et serveur est éliminé à la compilation.

**⚠️ Contrainte WASM importante** : `rmcp` (le SDK MCP Rust officiel) ne
supporte pas `wasm32-unknown-unknown` (dépend de `tokio` mainline avec
features non-WASM). Conséquence pour Luna Studio Web :

- Le binaire WASM **n'embarque pas de serveur MCP**.
- L'agent IA distant se connecte à un Luna **natif** (qui héberge le
  serveur MCP officiel), via WebSocket relayé par le client web.
- Architecture cible pour la V2 web :

  ```
  Agent IA ──MCP stdio──► Luna natif ──WebSocket──► Luna Studio Web (WASM)
                                                          │
                                                          ▼
                                                   Vue partagée du même
                                                   état d'émulation
  ```

- Alternative future : attendre `wasm32-wasip2` + Component Model
  (maturité mi-2026 selon paiml/rust-mcp-sdk).

Cf. RESEARCH.md pour les détails de l'audit WASM.

### 9.3 Cas d'usage produit déverrouillés

Au-delà de l'agent IA, voici l'écosystème d'outils que l'API rend
possible. Listés par potentiel d'impact pour la communauté SNES.

#### A — Luna Studio Web (IDE homebrew dans le navigateur)

**Priorité haute** post-V1. Un environnement intégré dans le navigateur
pour développer son propre jeu SNES :

- Éditeur de code (Monaco/CodeMirror) avec coloration syntaxique 65C816.
- Assembleur intégré (`wla-dx`, `ca65`) compilé en WASM.
- **Émulateur Luna en WASM** dans la même page, exécution locale.
- **Hot-reload** : `Ctrl+R` ré-assemble et relance le ROM en cours.
- Outils de debug visuels : VRAM viewer, palette editor, sprite editor,
  tilemap painter.
- Versioning Git via libgit2 (in-browser) ou backend serveur.
- Partage de projets via URL (sandboxed).

Tout l'IDE est une SPA qui parle à Luna WASM via JS bindings — aucune
latence réseau dans la boucle dev/test. **C'est de loin le cas d'usage
le plus impactant pour la communauté homebrew SNES**, qui n'a aujourd'hui
aucun équivalent à Godot/Unity pour ses besoins.

#### B — Luna Studio Desktop (client lourd dev studio)

Pour les devs qui veulent les performances natives et l'intégration
système :

- Cycle-accurate sans overhead JS/WASM.
- Filesystem natif, Git natif, pipelines de build pluggable.
- Plugin system (importeurs Aseprite, Tiled, Pyxel…).
- Debugger plus puissant (memory inspector multi-fenêtres, watchpoints
  conditionnels riches).

Construit avec `egui` + appels directs à `luna-api` (pas de transport
JSON, juste Rust ↔ Rust). C'est l'équivalent SNES de "Visual Studio Code
+ extension émulateur intégrée".

#### C — Tests d'intégration CI pour ROM hacks et homebrew

Une crate `luna-test` qui permet d'écrire des tests pour son propre jeu :

```rust
#[luna_test]
fn level_1_can_be_completed() {
    let mut emu = Luna::new().load("game.sfc");
    emu.advance_to_title_screen();
    emu.send_inputs(&["Start", "Start"]);
    emu.run_until_event(Event::LevelComplete, max_frames: 18_000)?;
    assert_eq!(emu.read_named("score"), 12_500);
    assert_eq!(emu.read_named("lives"), 3);
}
```

Les devs homebrew n'ont aujourd'hui aucune CI sérieuse. Luna y apporte
un standard : commit → GitHub Actions → tests joués sur émulateur
cycle-accurate → résultat en ≤ 30s.

#### D — Cloud streaming léger

WebSocket + framebuffer compressé (PNG diff ou H.264 simple) → streamer
une session Luna depuis un serveur vers un client web mince. Pas un
concurrent de Stadia, mais utile pour : "ouvre cette ROM dans un onglet
sans rien installer" (démos, partage, archives interactives).

#### E — Extension VSCode

Plugin qui détecte les projets SNES homebrew et :
- Lance Luna en sous-processus (transport REST local).
- Affiche le framebuffer dans un webview panel.
- Branche le debugger VSCode sur l'API de breakpoints/registres.
- Permet edit → assemble → test sans quitter l'éditeur.

#### F — Plateforme éducative

Cours d'architecture machine 16 bits, Luna comme bac à sable interactif
en temps réel : élèves voient simultanément l'état des registres CPU, la
VRAM, le fetch d'instructions, l'effet pixel par pixel.

#### G — Speedrunning & TAS moderne

Mode `replay` déterministe + frame-stepping + save states + scripting →
plateforme de Tool-Assisted Speedruns. Concurrent sérieux de BizHawk
pour le SNES, avec en plus l'écosystème Rust moderne et l'API
scriptable.

#### H — Auto-arbitrage de tournois

Multi-instances Luna en parallèle (un container par match) + replays
signés cryptographiquement → tournois SNES avec preuves d'intégrité.
Élimine le cheating côté client.

#### I — Embarqué / hardware

Une fois `luna-core` stable en `no_std` (objectif V2), portage possible
sur SBC type Raspberry Pi en mode "console rétro intelligente" :
émulateur + serveur MCP local qui répond aux requêtes d'un assistant
vocal ("Claude, garde mon save state avant le boss").

### 9.4 Implications architecturales

Pour que cet écosystème reste cohérent et maintenable :

1. **Zéro logique applicative dans les transports** : les crates
   `luna-mcp`, `luna-rest`, `luna-wasm` font *uniquement* du marshalling.
   Toute logique métier reste dans `luna-api`.

2. **Schéma source unique** : tous les types publics de `luna-api` sont
   annotés `JsonSchema`. La doc OpenAPI, les types TS, les `.proto` sont
   tous **générés**, pas écrits à la main.

3. **Compilation conditionnelle** : chaque transport est une feature
   Cargo désactivable. Build "headless minimal" = MCP seul. Build
   "Luna Studio Desktop" = tout activé.

4. **Authentification & autorisation** : critique dès qu'on sort de
   stdio local. Design V1.1 :
   - Token API (`Authorization: Bearer …`).
   - Capabilities granulaires (`read_state`, `write_memory`, `load_rom`).
   - Rate limiting + quotas par session.

5. **Multi-tenancy** : V1 = un binaire, une émulation. V1.1+ envisage un
   **mode session manager** pour le cloud (N sessions isolées par
   instance serveur, chacune avec son cœur d'émulation dans un thread).

6. **Versioning d'API** : pin de version dans les requêtes, dépréciation
   propre (≥ 1 mineure de transition), breaking changes annoncés.

7. **Observabilité** : tracing structuré (`tracing` crate), métriques
   Prometheus optionnelles pour les déploiements serveur. Indispensable
   en multi-tenancy.

### 9.5 `luna-api` comme contrat public stable

Conséquence directe : `luna-api` devient le **crate phare** de
l'écosystème, celui qui doit avoir la stabilité la plus forte. On y
applique une discipline supérieure aux autres crates :

- **SemVer strict** : aucun changement breaking sans bump majeur.
- **Politique de dépréciation** : ≥ 1 mineure avec `#[deprecated]` avant
  suppression.
- **Tests d'API publique** : `cargo-public-api` en CI, détecte tout
  changement non documenté.
- **Documentation exhaustive** : chaque trait/struct documenté, exemples
  dans des `///` doctests exécutés en test.
- **Re-exports stratégiques** : `luna::api::prelude::*` rassemble les
  types nécessaires aux clients, isolant des détails internes.
- **Changelog tenu** au format Keep a Changelog, avec section
  "API changes" séparée du reste.

C'est le seul crate dont la stabilité d'API est garantie au niveau
"release product 1.0". Les autres (`luna-cpu`, `luna-ppu`, …) peuvent
évoluer plus librement entre versions tant que `luna-api` reste stable.

---

## 10. Modèle de threading

Le modèle diffère selon la cible (natif vs WASM). Le code partagé passe
par la façade `luna-async` (§4.1) pour rester cross-target.

### 10.1 Cible native (Linux / macOS / Windows)

```
┌────────────────────────────────────────────────────────────────┐
│            Thread "emulation" (dédié, 60 Hz)                   │
│  - Scheduler CPU master-clock catch-up (§6.6)                  │
│  - bus.io_cycle() rattrape PPU/HDMA mid-instruction            │
│  - Vérifie les breakpoints                                     │
│  - Lit les Command entre les frames                            │
│  - Publie les Event sur le bus                                 │
└───────┬────────────────────────────────────────────┬───────────┘
        │ futures::channel::mpsc<Command> (entrée)   │ broadcast<Event>
        ▲                                            ▼
┌───────┴──────────────┐                  ┌──────────────────────┐
│  Tokio runtime       │                  │  Bus d'événements    │
│  (thread principal)  │                  │  (tokio broadcast)   │
│  - luna-mcp-server   │◄─── Event ──────►│  diffusion fan-out   │
│  - handlers async    │                  └──────┬───────────────┘
│  - parse JSON-RPC    │                         │
└──────────────────────┘                         │
                                                 ▼
                                ┌────────────────────────────────┐
                                │     Thread "GUI" (optionnel)   │
                                │  - winit/egui/wgpu             │
                                │  - rendu framebuffer 60 fps    │
                                │  - overlays spectator          │
                                │  - inputs clavier/manette →    │
                                │    Command vers cœur           │
                                └────────────────────────────────┘
```

### 10.2 Cible WASM (Luna Studio Web — V2)

```
┌────────────────────────────────────────────────────────────────┐
│              Tâches single-thread (Web Worker ou main)         │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Émulation cadencée par requestAnimationFrame()           │   │
│  │  - Scheduler CPU master-clock catch-up (§6.6)            │   │
│  │  - bus.io_cycle() rattrape PPU/HDMA mid-instruction      │   │
│  └────────────────┬────────────────────────────────────────┘   │
│                   │ Rc<RefCell<EmuState>>                       │
│                   ▼                                             │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Microtask queue : luna-mcp-client + GUI eframe          │   │
│  │  - WebSocket vers Luna natif distant (PAS de serveur    │   │
│  │    MCP embarqué — rmcp incompatible WASM)               │   │
│  │  - egui/wgpu via eframe (WebGPU ou WebGL2)              │   │
│  └─────────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────────┘
```

### 10.3 Discipline stricte

- Le cœur n'accède à *aucune* ressource async ni GUI directement.
  Toute interaction avec le monde extérieur passe par les canaux
  (`Command` en entrée, `Event` broadcast en sortie).
- **Pas de `crossbeam-channel` dans le cœur** — il panique en WASM.
  Utiliser `futures::channel::mpsc` partout (compatible cross-target).
- **`!Send` partout** dans `luna-core` et `luna-mcp-core` — single-thread
  pour compat WASM. Le parallélisme natif passe par des threads
  explicites dans `luna-mcp-server` natif uniquement.
- **Pas de `borrow_mut()` à travers un `await`** côté WASM — risque de
  panic `RefCell already borrowed`.

Avantages structurels :

- Le cœur reste testable sans tokio ni winit ni WebSocket.
- La latence MCP n'impacte pas le timing d'émulation.
- On peut figer le cœur (pause) sans déranger le serveur MCP ni la GUI.
- **GUI et MCP sont symétriques** : tous deux sont des consommateurs du
  même bus, ce qui rend le mode `spectate` trivial (= activer les deux).
- L'humain peut prendre la main en mode spectator en envoyant des
  `Command::Input` depuis la GUI exactement comme le ferait l'agent MCP
  — l'origine de la commande est tracée pour le panneau "Agent activity".

---

## 11. Déterminisme & reproductibilité

**Garanties par défaut**

- Même ROM + même séquence d'inputs + même seed RNG initial → exactement
  la même séquence de frames.
- Les save states encodent l'état *complet* de la machine (RAM, VRAM,
  OAM, CGRAM, APU RAM, registres, scheduler queue, cycle counter).

**Replay**

Format de fichier `.lreplay` (TOML + binaire) :

```toml
[meta]
rom_sha256 = "abc123..."
luna_version = "0.3.1"
created_at = "2026-05-23T11:00:00Z"

[inputs]
# (frame, port, buttons)
1     = [0, "Start"]
60    = [0, "B"]
120   = [0, "B|Right"]
# ...
```

Un replay peut être rejoué avec :
```bash
$ luna replay session.lreplay --verify
```

Le flag `--verify` re-calcule le hash des framebuffers et le compare à un
manifeste de référence — utile en CI.

**Time travel** : un buffer circulaire de N save states pris toutes les
secondes (configurable) permet à l'agent de faire `rewind(seconds: 5)`.
Coût : ~200 KB × N en RAM (négligeable jusqu'à plusieurs minutes).

---

## 12. Stratégie de test

### Tests unitaires

- Un crate = un module de tests.
- Chaque opcode 65C816 testé sur des cas connus (flags, edge cases du
  mode E, BCD, etc.).
- Chaque registre PPU testé sur les comportements de lecture/écriture.

### Tests d'intégration

- **Test ROMs** open-source dans `tests/roms/` :
  - Suite **krom** (CPU, PPU, DMA, HDMA, ADC, etc.)
  - Suite **blargg** (APU)
  - Suite **peter_lemon** (PPU avancé)
- Chaque ROM affiche "PASS" ou "FAIL" via texte/écran. On capture la
  frame N et on cherche le pattern attendu.

### Tests visuels (golden)

- Pour chaque jeu de référence (~20 jeux), une frame à un point précis
  (après séquence d'inputs déterministe) est stockée comme PNG dans
  `tests/golden/`.
- En CI, on rejoue la séquence et on compare pixel-par-pixel (tolérance
  zéro en mode cycle-accurate).

### Tests de performance

- `cargo bench` (criterion) sur les hot paths : CPU step, PPU scanline,
  APU sample generation.
- Régression de perf détectée si > 5% sur deux commits consécutifs.

### Tests MCP

- Mock client MCP qui rejoue des scénarios scriptés et vérifie les
  réponses (schemas + valeurs attendues).

---

## 13. Build, distribution, licence

**Build**

```bash
# développement
cargo build

# release optimisé
cargo build --release

# build minimal (sans GUI, sans coprocesseurs niche)
cargo build --release --no-default-features --features "core,mcp,sa1,superfx,dsp1"
```

**Distribution**

- **Binaires** : Linux x86-64/aarch64, macOS Intel/ARM, Windows x86-64.
- **Crates.io** : tous les crates `luna-*` publiés indépendamment.
- **GitHub Releases** : tagged + checksums signés.
- **Docker** : image `ghcr.io/<org>/luna:latest` pour intégration CI.

**Licence**

Recommandation : **MPL-2.0** (Mozilla Public License 2.0). Justification :

- Plus permissive que GPL (compatible avec usage commercial).
- File-level copyleft : modifications du code de Luna doivent être
  partagées, mais l'intégration dans un projet plus large (par ex. un
  outil dev propriétaire) reste possible.
- Compatible avec une éventuelle adoption par la communauté libretro /
  Anthropic.

À discuter : GPL-3.0 (plus protecteur) ou Apache-2.0 (plus permissif).

---

## 14. Roadmap & phasage

### Phase 0 — Validation des patterns & squelette (3 semaines)

**Recherche & validation** (1 semaine — préalable à tout code de production) :

- Lecture du code de référence (lecture seule, GPL-3.0, pas de copie) :
  - `jgenesis/backend/snes-core/src/api.rs` (modèle `Snes::tick`)
  - `jgenesis/backend/snes-core/src/apu.rs` (catch-up rationnel)
  - `jgenesis/backend/snes-core/src/bus.rs` (`access_master_cycles`)
  - `jgenesis/backend/snes-core/src/memory/dma.rs` (DMA/HDMA timing)
  - `tetanes-core/src/cpu.rs` (pattern `start_cycle`/`end_cycle`)
  - `jgenesis/ARCHITECTURE.md` (modèle d'organisation du workspace)
- Vérifier la licence de `emu-rs/snes-apu` (MIT/Apache attendu) — si
  compatible, planifier son intégration en Phase 2 pour économiser ~1
  mois.
- Cloner et faire tourner [Tom Harte 65816 ProcessorTests](https://github.com/SingleStepTests/65816)
  pour valider le format de la suite de test.

**Squelette code** (2 semaines) :

- Workspace Cargo avec les ~15 crates (cf. §4), tous compilent vide.
- CI GitHub Actions : `cargo check` + `cargo check --target
  wasm32-unknown-unknown` (échoue si un crate cross-target casse).
- `luna-async` : façade runtime (spawn/sleep/channels) avec
  implémentations natif (tokio) + web (wasm-bindgen-futures).
- `luna-bus` : memory map basique + LoROM mapper + trait `Bus` avec
  `io_cycle()`.
- `luna-cpu-65c816` : décodeur d'instructions complet (sans timing fin
  encore). Jump-table `[fn(&mut Cpu, &mut Bus); 256]`.
- `luna-cli` : charge une ROM, exécute 1 frame, dump l'état CPU.
- Tests : premier passage de quelques tests Tom Harte.

### Phase 1 — Premier rendu (4 semaines)

- `luna-ppu` : modes 0 et 1, scanline-based, sprites basiques.
- `luna-dma` : DMA (sans HDMA).
- `luna-core::Snes::step()` complet (cf. §6.6) — CPU + DMA + PPU
  catch-up via `bus.io_cycle()`.
- 1000+ tests Tom Harte passent (cible : 100% du 65C816).
- Une ROM de test (krom CPUMSC) affiche "PASS".

### Phase 2 — Audio + jeux simples (4 semaines)

- `luna-apu` : SPC700 + DSP basique.
- HDMA fonctionnel.
- **Super Mario World** jouable end-to-end (sans bugs visuels majeurs).

### Phase 3 — API, MCP, GUI standalone (4 semaines)

- `luna-api` : Control + Debug + Semantic.
- `luna-mcp` : serveur stdio avec ~15 tools de base.
- `luna-gui` v0 : mode **standalone** (humain joue avec clavier/manette).
- Démo : Claude Code charge une ROM, prend un screenshot, lit la RAM.
- Implémentation des principes d'économie de tokens dès le départ :
  resources, niveaux de détail, hash-then-fetch.

### Phase 4 — Coprocesseurs prioritaires (6 semaines)

- SA-1, Super FX, DSP-1.
- **Star Fox**, **Super Mario RPG**, **Yoshi's Island** jouables.

### Phase 5 — Debug avancé & mode spectator (5 semaines)

- Breakpoints conditionnels, trace logging, time travel.
- Semantic API enrichie (palette decoded, window state).
- Resources MCP (`luna://docs/...`).
- `luna-gui` v1 : **mode spectator** avec overlays — timeline d'activité
  agent, surbrillance des sprites/régions interrogés, panneau de budget
  tokens en direct.
- `luna-overlay` : composants réutilisables (timeline, mini-map mémoire).

### Phase 6 — Polish & 1.0 (4 semaines)

- Tests visuels golden sur 20 jeux.
- Documentation utilisateur.
- Stabilisation de `luna-api` (SemVer figé, `cargo-public-api` en CI).
- Démos AI publiques :
  1. Claude joue Super Mario World en autonomie.
  2. Claude débogue un crash sur un ROM hack.
  3. Claude développe un homebrew "hello world" en assemblant + testant
     dans la boucle.

**Total estimé** : ~6 mois pour V1.0.

### Post-1.0 — Ouverture de l'écosystème

Phases optionnelles selon traction & feedback communauté :

- **Phase 7 — Transports additionnels** (~4 sem.) : `luna-rest`,
  `luna-ws`, génération OpenAPI + types TS. Débloque les frontends web
  tiers.
- **Phase 8 — Luna Studio Web** (~8 sem.) : `luna-wasm` + SPA IDE
  homebrew. L'objectif "killer app" pour la communauté SNES.
- **Phase 9 — Luna Studio Desktop** (~6 sem.) : evolution de `luna-gui`
  en IDE complet avec assembleur intégré, plugin system.
- **Phase 10 — Bindings & intégrations** (~6 sem.) : FFI Python/C,
  extension VSCode, core libretro.
- **Phase 11 — Cloud & multi-tenancy** (~6 sem.) : auth, session
  manager, observabilité, déploiement Kubernetes.

---

## 15. Risques & questions ouvertes

### Risques techniques

| Risque                                          | Mitigation                                                                          |
|-------------------------------------------------|-------------------------------------------------------------------------------------|
| Performance cycle-accurate trop lente           | Pattern jgenesis static dispatch zero-alloc (§6.6), profiling criterion, SIMD PPU   |
| Sync CPU↔APU difficile à stabiliser             | Arithmétique rationnelle u64 (pas de float, §6.6), tests blargg APU                 |
| Schémas MCP qui changent (spec en évolution)    | Pinner sur version stable, abstraire derrière `luna-mcp-core`                       |
| Coprocesseurs sous-documentés (Super FX)        | S'appuyer sur fullsnes.htm + code jgenesis (lecture, pas copie GPL)                 |
| **Explosion de coûts tokens en usage IA**       | Profils `economy/balanced/generous`, hash-then-fetch, resources MCP, budget tracker (§8.5) |
| GUI spectator qui ralentit le cœur              | Thread GUI séparé, framebuffer partagé via `arc-swap` ou triple-buffer              |
| **Borrow checker hostile** (CPU + bus + PPU mut simultanés) | Pattern `SnesBus<'a>` créé à chaque step, emprunts séparés. Pas de `Rc<RefCell>` dans la hot loop |
| **`tokio::time` panique en WASM**               | Façade `luna-async` obligatoire dès V1 (§4.1) — bannir `tokio::*` direct dans le cœur |
| **`crossbeam-channel` panique en WASM**         | Utiliser `futures::channel::mpsc` partout, jamais crossbeam dans le cœur            |
| **`rmcp` ne tourne pas en WASM**                | V2 Luna Studio Web = client WebSocket vers Luna natif distant (cf. §9.2)            |
| **Mid-instruction effects manqués** (Mario Kart, F-Zero) | Pattern `bus.io_cycle()` à chaque accès CPU (§5, §6.6). Tester contre Tom Harte ProcessorTests |
| **NMI/IRQ timing 1-cycle off**                  | Latcher l'état IRQ/NMI au début d'instruction, le servir avant fetch suivant (cf. jgenesis api.rs:323) |

### Questions ouvertes (à trancher en Phase 0)

1. **Licence finale** : MPL-2.0 (proposée) vs Apache-2.0 (plus permissif).
   Validation après revue des contraintes commerciales souhaitées.
2. **Intégration `emu-rs/snes-apu`** : vérifier la licence en Phase 0. Si
   MIT/Apache, planifier l'intégration Phase 2 (économie ~1 mois). Sinon,
   APU from-scratch.
3. **Compatibilité libretro core** : reportée à la Phase 10. À confirmer
   que les contraintes libretro (sync API, threading) sont compatibles
   avec notre cœur `!Send`.
4. **WASM target dès V1 ?** : recommandé — la façade `luna-async` doit
   être en place dès le départ pour éviter les retours en arrière
   coûteux. La compilation WASM peut rester "compile + tests basiques"
   en V1, sans GUI complète.
5. **Format des game maps** : TOML, JSON, ou format custom ? Comment
   partager dans la communauté (registre GitHub, marketplace) ?
6. **Stratégie de stabilisation `luna-api`** : à quel moment figer
   l'API publique ? Cible : Phase 6.
7. **Multi-tenancy en V1.1 ou V2** : un seul cœur d'émulation par binaire
   (simple) ou plusieurs sessions parallèles (débloque "cloud sandbox") ?
8. **`!Send` partout vs cfg-gate** : la simplicité de `!Send` partout
   l'emporte-t-elle sur le parallélisme natif perdu ? Recommandation
   recherche : `!Send` partout (cf. eframe, la majorité des émulateurs
   Rust cross-target).

### Questions de produit

- **Modèle de licence dual** (open source + commercial) si entreprises
  veulent intégrer Luna ?
- **Marketplace de game maps** annotés par la communauté ?
- **Benchmarks publics** : suite de défis ("battre Super Mario World
  niveau 1") pour comparer les performances des LLMs ?

---

## 16. Glossaire

- **65C816** : CPU 16 bits du SNES, dérivé du 6502.
- **APU** (Audio Processing Unit) : sous-système son du SNES, composé du
  SPC700 et du DSP.
- **CGRAM** : 512 octets de mémoire palette (256 couleurs × 16 bits).
- **Coprocesseur** : puce additionnelle dans une cartouche SNES
  (SA-1, Super FX, DSP-1, etc.).
- **Cycle-accurate** : émulation où chaque cycle d'horloge est simulé,
  pas juste les résultats finaux d'une instruction.
- **DMA** (Direct Memory Access) : transfert mémoire rapide sans CPU.
- **DSP** : Digital Signal Processor (ici, soit le DSP audio APU, soit
  un coprocesseur DSP-N).
- **HDMA** : DMA synchronisé sur les scanlines PPU.
- **HLE** (High-Level Emulation) : émulation simplifiée des comportements
  (vs cycle-accurate).
- **MCP** (Model Context Protocol) : protocole standardisé pour qu'un
  LLM communique avec des outils externes.
- **MMIO** (Memory-Mapped I/O) : registres exposés comme adresses
  mémoire.
- **OAM** (Object Attribute Memory) : mémoire qui décrit les 128
  sprites du SNES (512 octets + 32 octets de table 2).
- **PPU** (Picture Processing Unit) : sous-système vidéo du SNES.
- **Scanline** : ligne horizontale de pixels rendue par le PPU.
- **SPC700** : CPU 8 bits dédié à l'audio dans le SNES.
- **Tilemap** : grille de tiles qui compose un background.
- **VRAM** : 64 KB de mémoire vidéo (tiles, tilemaps).
- **WRAM** (Work RAM) : 128 KB de RAM de travail du CPU principal.
