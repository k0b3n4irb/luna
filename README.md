# Luna

> Émulateur SNES **cycle-accurate** écrit en Rust, conçu pour qu'un agent IA
> puisse **jouer**, **développer** et **déboguer** des jeux Super Nintendo de
> manière autonome — via une API d'introspection riche et un serveur MCP
> intégré.

[![Édition Rust](https://img.shields.io/badge/Rust-2024-orange)](rust-toolchain.toml)
[![Licence](https://img.shields.io/badge/licence-MPL--2.0-blue)](LICENSE)
[![Statut](https://img.shields.io/badge/statut-pré--1.0-yellow)](#statut)

---

## Pourquoi Luna ?

Les émulateurs SNES traditionnels considèrent l'IA comme un cas d'usage
secondaire — à brancher *a posteriori* via de l'OCR sur des captures d'écran.
Luna inverse la priorité : le dialogue **agent ↔ machine** est un objectif
central de design.

Concrètement, l'état complet de la console (registres CPU, VRAM, OAM, palette,
scroll, tilemap, sprites, mémoire) est exposé sous forme **structurée et
sérialisable**, et un serveur **MCP** (Model Context Protocol) permet à un
agent comme Claude de piloter la machine via un catalogue d'outils JSON-RPC
standardisés — sans jamais regarder un pixel s'il ne le souhaite pas.

Trois usages sont assumés dès la conception :

- 🎮 **Play** — l'agent joue à un jeu existant.
- 🛠️ **Dev** — l'agent développe un homebrew.
- 🐛 **Debug** — l'agent inspecte un ROM hack (breakpoints, trace, mémoire).

La fidélité matérielle n'est pas sacrifiée pour autant : les cœurs CPU sont
validés contre les suites de tests de référence, et chaque sous-système est
implémenté en relisant les émulateurs de référence (ares, Mesen2) avant
d'écrire la moindre ligne — voir [`docs/emulator_landscape.md`](docs/emulator_landscape.md)
pour le panorama qui a motivé ces choix de référence.

## Statut

Projet **en développement actif, pré-1.0** (`v0.0.1`). Ce qui tourne
aujourd'hui :

| Sous-système | Crate | État |
|---|---|---|
| Bus & memory map (LoROM / HiROM / ExHiROM / SA-1) | `luna-bus` | ✅ |
| Parsing ROM & détection de mapper | `luna-cartridge` | ✅ |
| CPU 65C816 (cycle-accurate, suite SingleStepTests 100 %) | `luna-cpu-65c816` | ✅ |
| CPU SPC700 (cycle-accurate, suite SingleStepTests 100 %) | `luna-cpu-spc700` | ✅ |
| APU — SPC700 + S-DSP (port cycle-accurate d'ares) | `luna-apu` | ✅ |
| PPU + renderer + compositor | `luna-ppu` | ✅ |
| Glue système, scheduler, DMA / HDMA, coprocesseur SA-1 | `luna-core` | ✅ |
| API d'introspection (snapshots `EmulatorState`) | `luna-api` | ✅ |
| Serveur MCP (stdio) | `luna-mcp-server` | ✅ |
| Binaire CLI (`run` / `state` / `mcp`) | `luna-cli` | ✅ |
| GUI debugger (eframe, pacing audio-as-clock) | `luna-gui` | ✅ |

Coprocesseurs au-delà de SA-1 (Super FX, DSP-1…), transports REST/WebSocket
et cible WASM sont sur la [roadmap](ARCHITECTURE.md#14-roadmap--phasage), pas
encore livrés.

## Démarrage rapide

Prérequis : la toolchain Rust épinglée dans [`rust-toolchain.toml`](rust-toolchain.toml)
(édition 2024, Rust ≥ 1.85).

```bash
# Build complet (debug + release)
cargo build --release --workspace

# Lancer le debugger graphique sur une ROM
cargo run --release -p luna-gui -- "chemin/vers/jeu.sfc"
```

### Le binaire `luna` (CLI)

```bash
# Exécuter N instructions et dumper une capture d'écran (headless, sans GUI)
./target/release/luna run "jeu.sfc" -n 2000000 --screenshot /tmp/frame.png

# Émettre un snapshot JSON de l'état machine (la même donnée que l'outil MCP get_state)
./target/release/luna state "jeu.sfc" -n 30000 --out -

# Servir le serveur MCP sur stdio (pour Claude Desktop / Claude Code / client custom)
./target/release/luna mcp
```

## Architecture en bref

Luna est un workspace Cargo de 11 crates, organisé en couches qui ne
communiquent que par contrats Rust (traits + types sérialisables) — aucune
dépendance d'une couche basse vers une couche haute.

```
┌──────────────────────────────────────────────────────────┐
│  Serveur MCP (luna-mcp-server)  — JSON-RPC sur stdio       │
├──────────────────────────────────────────────────────────┤
│  API d'introspection (luna-api) — contrat public stable    │
├──────────────────────────────────────────────────────────┤
│  Cœur d'émulation (luna-core)                              │
│   65C816 · PPU · SPC700/DSP · DMA · SA-1 · scheduler       │
├──────────────────────────────────────────────────────────┤
│  Bus & mappers (luna-bus)                                  │
└──────────────────────────────────────────────────────────┘
        ▲                                  ▲
   luna-cli (headless)              luna-gui (egui/wgpu)
```

Ce découplage permet trois **modes d'exécution** combinables sur le même
binaire :

- **Headless** — aucune fenêtre, pilotage 100 % via MCP (production IA, CI).
- **Standalone** — fenêtre native, clavier/manette (un humain joue).
- **Spectator** — l'IA joue, l'humain observe le framebuffer et l'activité de
  l'agent en temps réel.

Le design complet (vision, non-objectifs, couches, threading, déterminisme,
roadmap) est documenté dans **[`ARCHITECTURE.md`](ARCHITECTURE.md)**.

## Documentation

| Document | Contenu |
|---|---|
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Design complet du système, couches, roadmap |
| [`RESEARCH.md`](RESEARCH.md) | Recherche pré-Phase-0 (fork vs from-scratch, WASM, scheduler) |
| [`CLAUDE.md`](CLAUDE.md) | Conventions du dépôt pour les contributeurs (et les agents) |
| [`docs/`](docs/) | Specs de référence PPU/APU/SA-1, scorecard de précision, gap lists |
| [`docs/emulator_landscape.md`](docs/emulator_landscape.md) | Panorama comparatif des émulateurs SNES existants |

## Développement

La séquence canonique avant tout commit (rebuild + tests + lint) :

```bash
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets \
  && cargo test --workspace --lib \
  && cargo fmt --all --check \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Les conventions détaillées (reference-first, discipline de test des
coprocesseurs, workflow de validation audio/vidéo) vivent dans
[`CLAUDE.md`](CLAUDE.md) et `.claude/rules/`.

## Licence

Distribué sous licence **Mozilla Public License 2.0** — voir [`LICENSE`](LICENSE).
