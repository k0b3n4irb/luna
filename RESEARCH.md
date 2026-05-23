# Luna — Recherche pré-Phase-0

> Synthèse des trois axes de recherche menés en amont du code, pour valider
> les choix d'architecture exprimés dans `ARCHITECTURE.md`. Date de la
> recherche : mai 2026.

Trois agents indépendants ont investigué en parallèle :

1. **État de l'art** des émulateurs SNES écrits en Rust → décision
   fork vs from-scratch.
2. **Compatibilité WASM** des dépendances clés → stratégie async
   cross-target.
3. **Patterns cycle-accurate** dans les émulateurs Rust existants →
   architecture du scheduler.

Les rapports complets sont reproduits ci-dessous. Les décisions
structurantes qui en découlent sont consignées dans `ARCHITECTURE.md`
(§4, §5, §6.6, §9.2, §10, §14, §15).

---

## Sommaire

- [Décisions structurantes (résumé)](#décisions-structurantes-résumé)
- [Axe 1 — État de l'art émulateurs SNES Rust](#axe-1--état-de-lart-émulateurs-snes-rust)
- [Axe 3 — Compatibilité WASM des deps clés](#axe-3--compatibilité-wasm-des-deps-clés)
- [Axe 5 — Patterns cycle-accurate en Rust](#axe-5--patterns-cycle-accurate-en-rust)

---

## Décisions structurantes (résumé)

### Décision #1 — From-scratch sous MPL-2.0 confirmé

- **jgenesis** (seul SNES Rust mature, 7 coproc, actif) est **GPL-3.0** →
  bloque MPL-2.0 et contaminerait notre couche MCP/API.
- **siena** a une bonne archi mais **aucune licence déclarée**.
- Pas de fork qui sauverait > 2 mois.
- **Mais** : lecture intensive de jgenesis autorisée (≠ copie) pour
  s'inspirer de l'architecture.
- Dép potentielle : `emu-rs/snes-apu` (licence à vérifier en Phase 0).

### Décision #2 — Scheduler : pattern jgenesis `CPU master-clock catch-up`

- Le `BinaryHeap<Event>` envisagé initialement est rejeté (overhead
  heap + Box<dyn> tue la perf à 21M cycles/s).
- Pattern adopté : CPU pilote le master-clock, `bus.io_cycle()` rattrape
  PPU/HDMA à chaque accès mémoire, APU sync via arithmétique rationnelle
  u64.
- Zero-alloc dans la hot loop, mid-instruction accuracy gratuite,
  save-states triviaux.

### Décision #3 — Façade `luna-async` obligatoire dès V1

- `tokio::time` *panique* en WASM (`wasm32-unknown-unknown`).
- `crossbeam-channel` *panique* en WASM (parking primitive absente).
- Solution : crate maison `luna-async` qui abstrait `spawn`/`sleep`/
  channels avec impls `native` (tokio) et `web` (wasm-bindgen-futures +
  gloo-timers).

### Décision #4 — Splitter `luna-mcp` en 3 crates

- `luna-mcp-core` (types, schemas) → cross-target ✅
- `luna-mcp-server` (rmcp + tokio) → **natif uniquement** ❌
- `luna-mcp-client` (transport WebSocket) → cross-target ✅
- Conséquence : Luna Studio Web (WASM) ne peut pas héberger un serveur
  MCP, se connectera via WebSocket à un Luna natif distant.

### Décision #5 — Rejeter coroutines / genawaiter

- `#[coroutine]` nightly only, save-states cassés.
- `genawaiter` perfs marginales, LLVM peine à inliner.
- Le pattern jgenesis static-dispatch fait mieux en stable + idiomatique.

### Décision #6 — `!Send` partout dans le cœur

- Single-thread compatible WASM par défaut.
- Le parallélisme natif passe par des threads dédiés dans
  `luna-mcp-server` uniquement, pas dans `luna-core`.

---

## Axe 1 — État de l'art émulateurs SNES Rust

### Projets évalués

#### jgenesis (jsgroth) — **LE CANDIDAT SÉRIEUX**

- **URL** : https://github.com/jsgroth/jgenesis
- **Licence** : **GPL-3.0** (bloquant pour MPL-2.0)
- **Activité** : très active — v0.12.1 publiée mai 2026, 2309 commits sur
  master, 339 stars, 15 forks
- **Architecture** : multi-système modulaire (10 consoles). Workspace
  Cargo avec séparation propre `backend/snes-core`,
  `backend/snes-coprocessors`, `cpu/` (cœurs 65816, SPC700, 68000, Z80,
  6502, SH-2 partagés entre systèmes), `config/`, `frontend/`.
- **Précision** : vise "moderately accurate", pas strictement
  cycle-accurate sur SNES mais avec corrections de timing V-IRQ, DMA,
  Mode 7, mosaic vertical, SA-1 wait-states.
- **Coprocesseurs SNES** : **Super FX, SA-1, DSP-1, CX4, S-DD1, SPC7110,
  ST018** — le set le plus complet de l'écosystème Rust SNES.
- **Tests** : harnais CPU dédiés, ARCHITECTURE.md propre, CI GitHub
  Actions, builds Linux/Windows/WASM.
- **Verdict** : top techniquement, **bloqué par licence GPL-3.0**.

#### siena (twvd)

- **URL** : https://github.com/twvd/siena
- **Licence** : non explicitée (flag rouge — défaut copyright)
- **Activité** : 489 commits, 19 stars, CI active, codecov tracké
- **Architecture** : cycle-accurate pour 65816 et SPC700 (revendiqué),
  renderer PPU multi-threadé (scanline)
- **Coprocesseurs** : DSP-1 (LLE), SuperFX, SA-1 (partiel), Super
  Gameboy. Pas de CX4, S-DD1, SPC7110, ST018.
- **État** : auteur déclare "not really fit for playing games" — hobby
  actif
- **Verdict** : risque légal (pas de licence), coproc limités

#### rsnes (nat-rix)

- **URL** : https://github.com/nat-rix/rsnes
- **Licence** : MIT (compatible MPL-2.0 ✓)
- **Activité** : abandonné depuis mars 2022 (~4 ans), 17 stars, 1 fork
- **Coprocesseurs** : aucun complet — tous en TODO
- **Verdict** : mort. Réutilisable comme inspiration uniquement.

#### ness (kelpsyberry)

- **URL** : https://github.com/kelpsyberry/ness
- **Licence** : non visible publiquement
- **Activité** : 18 stars, 59 commits, CI active, prebuilt binaries
- **Verdict** : actif mais peu documenté. Auteur a fait `dust` (GBA),
  compétence sérieuse. À investiguer plus si on rejette jgenesis.

#### Autres

| Projet | URL | Licence | État |
|---|---|---|---|
| FranLMSP/snes | github.com/FranLMSP/snes | GPL-3.0 | WIP, embryon |
| chronium/snes-emu | github.com/chronium/snes-emu | MIT | 22 commits |
| Achtuur/SNESemu | github.com/Achtuur/SNESemu | MIT | early WIP |
| mrjkey/rust-snes-emu | github.com/mrjkey/rust-snes-emu | n/a | coquille vide |
| super-rustcom, pichi, rustsnes | introuvables | — | n'existent pas |

### Crates réutilisables

- **`w65c816`** (crates.io) : seul cœur 65C816 standalone Rust. **Très
  incomplet** — "less than 60 instructions missing, plenty of addressing
  modes missing". Inutilisable en l'état.
- **`snes-apu`** (emu-rs/snes-apu) : portage d'un APU SNES C++ vers Rust.
  "Minimal maintenance", `unsafe` interne, "highly-accurate".
  **Potentiellement réutilisable** pour le SPC700 + DSP audio si licence
  compatible (à vérifier en Phase 0).

### Tableau comparatif

| Projet | Licence | Actif | Cycle-acc | Coproc | Modularité | Verdict Luna |
|---|---|---|---|---|---|---|
| jgenesis | GPL-3.0 | ✅ | "moderate" | **7** | Excellente | Top techniquement, **bloqué par licence** |
| siena | aucune | ✅ | ✅ | 4 partiels | Bonne | Risque légal, coproc limités |
| ness | ? | ✅ | ? | ? | Bonne | À enquêter |
| rsnes | MIT | ❌ 2022 | ❌ | 0 | Moyenne | Mort |
| FranLMSP | GPL-3.0 | WIP | ? | 0 | Moyenne | Trop tôt |
| w65c816 crate | MIT/Apache | Stagnant | Partiel | n/a | n/a | Inutilisable |
| snes-apu crate | MIT (à vérifier) | Minimal | "highly accurate" | n/a | OK | **Potentiel** comme dep |

### Recommandation : FROM-SCRATCH (avec emprunts ciblés)

**Pourquoi pas de fork :**

1. jgenesis (le seul techniquement sérieux) est GPL-3.0 → contaminerait
   toute distribution sous MPL-2.0, tuerait l'intégration dans agents
   IA propriétaires.
2. Les forks à licence permissive (rsnes, chronium…) sont morts ou
   embryonnaires, sans coprocesseurs. Pas d'économie de temps.
3. Siena : pas de licence déclarée = non-forkable légalement, et coproc
   trop limités.
4. Aucun cœur 65C816 Rust standalone réutilisable.
5. From-scratch maximise l'objectif Luna (API d'introspection + MCP)
   conçu comme contrainte de design jour 1.

**Verdict** : from-scratch sous MPL-2.0, en s'inspirant ouvertement de
jgenesis (lecture du code GPL est autorisée — c'est la copie qui
contamine) pour les choix d'architecture, et en utilisant `snes-apu`
comme dépendance si sa licence le permet.

**Code à étudier en Phase 0** (lecture, pas copie) :

- jgenesis `ARCHITECTURE.md` et structure du workspace
- jgenesis `backend/snes-core/src/api.rs` (modèle `Snes::tick`)
- jgenesis `backend/snes-core/src/apu.rs` (catch-up rationnel)
- jgenesis `backend/snes-core/src/memory/dma.rs` (DMA/HDMA timing)
- siena renderer PPU scanline-threaded (idées)
- bsnes C++ (GPL-3.0, lecture seule) pour la précision SNES

### Sources

- [jsgroth/jgenesis](https://github.com/jsgroth/jgenesis) +
  [ARCHITECTURE.md](https://github.com/jsgroth/jgenesis/blob/master/ARCHITECTURE.md)
- [twvd/siena](https://github.com/twvd/siena)
- [nat-rix/rsnes](https://github.com/nat-rix/rsnes)
- [kelpsyberry/ness](https://github.com/kelpsyberry/ness)
- [FranLMSP/snes](https://github.com/FranLMSP/snes)
- [w65c816 crate](https://crates.io/crates/w65c816)
- [emu-rs/snes-apu](https://github.com/emu-rs/snes-apu)
- [GitHub topic: snes-emulator (Rust)](https://github.com/topics/snes-emulator?l=rust)

---

## Axe 3 — Compatibilité WASM des deps clés

Cible visée : `wasm32-unknown-unknown` (pas de WASI, pas de threads).

### Tableau de compatibilité

| Crate | Statut wasm32-unknown-unknown | Caveats / Features |
|---|---|---|
| `tokio` 1.x | Partiel | `sync`, `macros`, `io-util` OK. `rt`/`rt-multi-thread`/`net`/`fs`/`process`/`signal` KO. `time` **panique** sur uu |
| `tokio_with_wasm` 0.7+ | OK | Shim qui réimplémente `spawn`, `sleep`, `JoinHandle`… via `setTimeout`/microtasks. Marqué "hacky, temporaire" |
| `wasm-bindgen-futures` | OK | `spawn_local`, `JsFuture`, `future_to_promise`. Pas d'exécuteur multitâche — pont vers la microtask queue JS |
| `smol` / `async-std` | KO | Reposent sur des reactors I/O natifs. Pas de support web |
| `futures` / `futures-channel` (mpsc/oneshot) | OK | Channels async, single-thread friendly. **À utiliser à la place de tokio mpsc en mode multi-target** |
| `crossbeam-channel` | **KO sur uu** | `recv()` panique "unreachable" — primitive de parking absente sans threads |
| `flume` | Conditionnel | OK en `try_recv` non-bloquant + feature `async`. Bloquant = même problème |
| `async-channel` | OK | Multi-producteur, async pur, fonctionne en single-thread |
| `wgpu` 22+ | OK | Features `webgpu` (Chromium récent) et `webgl` (GLES2). Pour WebGPU : `RUSTFLAGS="--cfg=web_sys_unstable_apis"` |
| `egui` / `eframe` | OK | `eframe` cible web officiellement via `eframe_template` (Trunk + wasm-bindgen) |
| `serde` + `serde_json` | OK | `no_std` possible avec `alloc` |
| `rmcp` 0.13+ | Partiel | Compile sur `wasm32-wasip2` et `wasm32-wasip1`. **Pas de support officiel `wasm32-unknown-unknown`** pour le serveur |
| `schemars` | OK | Feature `std` désactivable pour no_std |
| `ts-rs` | OK (build-time) | Génère les `.ts` à l'exécution des tests côté natif. À ne pas linker dans le binaire WASM |
| `utoipa` | OK | Pure derive/macro |
| `cpal` | Partiel | Backend "wasm-bindgen" (WebAudio) : **output OK, input KO**. Bloqué par autoplay policy |
| `genawaiter` | OK | Stackless coroutines en stable. Pure Rust, pas de syscalls. Compatible WASM mais **rejeté** (cf. axe 5) |
| `gloo-file`, `gloo-events`, `gloo-timers` | OK | Recommandés pour DOM/file/timers depuis Rust |

### Stratégie async cross-target (façade `luna-async`)

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

// Channels universels (cross-target)
pub use futures::channel::{mpsc, oneshot};
```

**Règles** :

- Bannir `tokio::*` dans `luna-core` et `luna-mcp-core` : passer par
  `luna_async::*`.
- Pour les bounds `Send` : la majorité des emus cross-target choisissent
  `!Send` partout pour simplifier. `wasm-bindgen-futures` impose `!Send`,
  donc soit on rend tout `!Send`, soit on cfg-gate le bound.

### Boucle 60Hz cross-target

| Aspect | Natif | WASM |
|---|---|---|
| Cadenceur | thread dédié + `spin_sleep` / `Instant` | `requestAnimationFrame` |
| State sharing | `Arc<Mutex<EmuState>>` ou channel | `Rc<RefCell<EmuState>>` (single-thread) |
| MCP / async | tasks tokio sur autre thread | tasks `spawn_local` sur même thread |

Pattern WASM canonique : `Rc<RefCell<Option<Closure>>>` qui se reschedule
([exemple officiel wasm-bindgen](https://rustwasm.github.io/docs/wasm-bindgen/examples/request-animation-frame.html)).
Entre chaque frame, la microtask queue draine les futures MCP — donc le
MCP handler et la boucle d'émulation partagent un `Rc<RefCell<EmuState>>`
sans contention, **à condition de ne jamais `borrow_mut()` à travers un
`await`**.

### Must replace (WASM-bloquants)

| Bloqueur | Remplacement |
|---|---|
| `crossbeam-channel` | `futures::channel::mpsc` / `async-channel` |
| `std::thread::spawn` | `wasm_bindgen_futures::spawn_local`. CPU-heavy via Web Worker (`gloo-worker`, `wasm_thread`) |
| `tokio::time` | `gloo-timers::future::TimeoutFuture` côté web (façade `luna_async::sleep`) |
| `tokio::net` / `axum` (transport MCP) | Côté web : `postMessage` ou WebSocket (`gloo-net`/`ws_stream_wasm`). **Ne pas** faire tourner rmcp serveur en navigateur |
| `std::fs` (ROM loading) | `<input type="file">` + drag&drop → `gloo-file::File::read_as_bytes` → `Vec<u8>` injecté |
| `cpal` input audio | OK output uniquement. Pour input : `web-sys::MediaStream` direct |
| `rmcp` serveur HTTP/SSE en navigateur | Impossible. **V2 web = client WebSocket vers Luna natif distant** |

### Découpage des crates recommandé

```
luna/
├── luna-core/          ✅ wasm-safe, no I/O, no tokio
├── luna-async/         ✅ wasm-safe façade (spawn/sleep/channels)
├── luna-frontend/      trait Frontend + impl natif & web
├── luna-ui/            ✅ wasm-safe : egui widgets, debugger, watcher
├── luna-mcp-core/      ✅ wasm-safe : types Tool/Resource, serde, schemars
├── luna-mcp-server/    ❌ native-only : rmcp + tokio + axum (SSE/HTTP)
├── luna-mcp-client/    ✅ cross-target : transport WebSocket pour Studio Web
└── luna-app-native/    binaire natif
└── luna-app-web/       cdylib wasm (eframe + wasm-bindgen)
```

### Points vraiment bloquants à arbitrer

1. **MCP en V2 web** : n'embarquer pas rmcp serveur dans le bundle WASM.
   Soit serveur natif distant + client WS, soit attendre
   `wasm32-wasip2` + Component Model (maturité mi-2026).
2. **`tokio::time` panique WASM** : si quoi que ce soit dans `luna-core`
   ou `luna-mcp-core` appelle `tokio::time`, le build compile mais
   crash à l'exécution. Façade `luna-async` **obligatoire** dès V1.
3. **Threads pour audio low-latency** : single-thread WASM = pas
   d'AudioWorklet portable. Acceptez 50-100 ms de latence en V2 ou
   bridgez via JS AudioWorklet + ring buffer SAB (complexe, COOP/COEP).
4. **`Send` bounds** : décidez tôt. Recommandation : `!Send` partout.

### Sources

- [tokio Issue #5418 — time panic on uu](https://github.com/tokio-rs/tokio/issues/5418)
- [tokio_with_wasm](https://github.com/cunarist/tokio-with-wasm)
- [wasm-bindgen-futures](https://crates.io/crates/wasm-bindgen-futures)
- [wgpu Web wiki](https://github.com/gfx-rs/wgpu/wiki/Running-on-the-Web-with-WebGPU-and-WebGL)
- [crossbeam Issue #756 — panic on wasm](https://github.com/crossbeam-rs/crossbeam/issues/756)
- [CPAL WASM wiki](https://github.com/RustAudio/cpal/wiki/Setting-up-a-new-CPAL-WASM-project)
- [rmcp WASM considerations](https://paiml.github.io/rust-mcp-sdk/course/part3-deployment/ch09-01-wasm-considerations.html)
- [mcp-wasm PoC](https://github.com/beekmarks/mcp-wasm)
- [wasm-bindgen rAF example](https://rustwasm.github.io/docs/wasm-bindgen/examples/request-animation-frame.html)
- [eframe_template](https://github.com/emilk/eframe_template)
- [bokuweb/rustynes](https://github.com/bokuweb/rustynes), [takahirox/nes-rust](https://github.com/takahirox/nes-rust) — NES en WASM
- [bmoxb/rustyboy](https://github.com/bmoxb/rustyboy) — cross-target proche de Luna

---

## Axe 5 — Patterns cycle-accurate en Rust

### Survey des patterns

#### TetaNES (lukexor) — "CPU-driven master clock catch-up"

Le CPU détient le `master_clock`. À chaque demi-cycle CPU
(`start_cycle`/`end_cycle`), il appelle
`bus.ppu.clock_to(master_clock - PPU_OFFSET)` qui fait rattraper le PPU
jusqu'à l'instant courant.

```rust
// tetanes-core/src/common.rs:155
pub trait Clock { fn clock(&mut self) {} }
```

DMA géré comme état spécial du CPU : `handle_dma()` émet des
`start_cycle`/`end_cycle` factices qui font avancer le PPU normalement.

#### jgenesis (jsgroth) — "Per-CPU-cycle dispatch" **(modèle pour Luna)**

Le `tick()` central :

```rust
// backend/snes-core/src/api.rs:284-395
let (master_cycles_elapsed, pending_write) = if self.memory_refresh_pending { ... }
else { match self.dma_unit.tick(...) {
        DmaStatus::None => { self.main_cpu.tick(&mut bus); ... (bus.access_master_cycles, ...) }
        DmaStatus::InProgress { master_cycles_elapsed } => (master_cycles_elapsed, None)
}};
self.apu.tick(master_cycles_elapsed);
self.memory.tick(master_cycles_elapsed);
self.ppu.tick(master_cycles_elapsed);
```

APU sync via catch-up à arithmétique rationnelle sans dérive :

```rust
// backend/snes-core/src/apu.rs:274-298
self.master_cycles_product += main_master_cycles * apu_master_clock_frequency;
while self.master_cycles_product >= 24 * self.main_master_clock_frequency {
    self.master_cycles_product -= 24 * self.main_master_clock_frequency;
    self.clock(); // 1 APU master tick
}
```

#### gameroy (Rodrigodd) — "Lazy update + event prediction"

Chaque composant publie `next_interrupt: u64` (master cycles absolus).
Le `GameBoy` n'avance les composants qu'à la demande. Très bonne perf,
mais **chaque composant doit savoir prédire** son prochain événement.
À utiliser **en complément** pour optimiser WAI/STP.

#### rboy (mvdnes) — "Instruction-step + propagate"

Le plus naïf. `cpu.do_cycle()` exécute une instruction complète, retourne
`ticks`, puis `mmu.do_cycle(ticks)` distribue le delta à GPU/timer/sound.
**Lisible mais non cycle-accurate à mid-instruction.**

#### DaveTCode/nes-emulator-rust — "State-machine par cycle"

Le CPU avance d'un master cycle à la fois via une enum `State`. Très
testable mais beaucoup de boilerplate.

#### moa (transistorfet) — "Event-queue à la libco"

Vrai scheduler discrete-event avec `Vec<NextStep>` trié. Pattern propre,
mais beaucoup d'overhead (RefCell, Rc, HashMap). **Pas optimisé pour
21 MHz × 60fps**.

#### Lochnes/bagnalla — "Rust generators/coroutines"

`#[coroutine]` (nightly). Lisible mais ~11ms/frame NES, save-states
impossibles. **À rejeter.**

#### Crates DES génériques

- `desru` : BinaryHeap + Box<dyn FnMut> → trop d'allocations
- `nexosim` : actor model multi-thread, conçu pour simulation système,
  pas real-time 60fps
- `desim` : nightly

**Aucune crate generic ne convient à 21 MHz.**

### Tableau comparatif

| Pattern | Lisibilité | Perf | Testabilité | Cycle-acc | Mid-instr | Verdict Luna |
|---|---|---|---|---|---|---|
| **CPU master-clock catch-up (TetaNES, jgenesis)** | Bonne | Excellente (zero alloc) | Bonne | Oui si bus tick à chaque accès | Oui | **Recommandé** |
| State-machine par cycle (DaveTCode) | Moyenne | Très bonne | Excellente | Parfaite | Native | Pour CPU isolé |
| Event queue (moa) | Très bonne | Médiocre | Excellente | Bonne | Difficile | Trop lent |
| Lazy + next_event (gameroy) | Moyenne | Excellente | Moyenne | Bonne | Difficile | En complément |
| Instruction-step naïf (rboy) | Excellente | Excellente | Bonne | Non | Non | Insuffisant SNES |
| Generators/coroutines | Excellente | Moyenne | Difficile | Parfaite | Native | Rejeter |
| Crate DES (nexosim/desru) | Bonne | Mauvaise | Bonne | Variable | Variable | Pas adapté |

### Recommandation pour Luna

**Pattern principal : CPU-driven master clock catch-up à la jgenesis**,
avec quatre raffinements :

1. **Unité de temps unique** : tout en master clock cycles
   (21.477 MHz NTSC = ~357 954 master cycles/frame). `u64` ne déborde
   pas avant 27 000 ans.
2. **Le 65C816 expose `tick(bus) -> u64`** qui exécute UNE instruction
   et retourne le delta master-cycles consommé. À chaque accès mémoire,
   il appelle `bus.io_cycle()` qui rattrape immédiatement PPU+DMA.
3. **APU** : catch-up à arithmétique rationnelle (3.072 MHz SPC700 /
   21.477 MHz CPU). Pattern jgenesis ligne 274.
4. **Event prediction** en complément (à la gameroy) : le PPU calcule
   `next_event_mclk` pour optimiser WAI/STP du 65C816.

**Pourquoi pas event-queue pur** : à 21M cycles/sec, BinaryHeap + Box +
dyn dispatch ajoute 50-100ns/event, soit 50% du budget cycle. TetaNES et
jgenesis atteignent 60fps confortable parce qu'il n'y a **aucune
allocation** dans la hot loop.

**Pourquoi pas coroutines** : nightly, save-state cassé, perfs marginales.

### Croquis Rust complet

```rust
// === luna-core/src/scheduler.rs ===

pub type MCycles = u64;
pub const NTSC_MASTER_HZ: u64 = 21_477_272;
pub const MCYCLES_PER_FRAME_NTSC: MCycles = 357_366;

#[derive(Default)]
pub struct TickEffect {
    pub frame_complete: bool,
    pub audio_samples: smallvec::SmallVec<[(f32, f32); 8]>,
}

pub trait Bus {
    fn read(&mut self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, val: u8);
    fn io_cycle(&mut self, mcycles: MCycles);
    fn nmi_pending(&self) -> bool;
    fn irq_pending(&self) -> bool;
}

pub trait Component {
    fn tick(&mut self, delta: MCycles) -> TickEffect;
    fn next_event_mclk(&self) -> Option<MCycles> { None }
}

pub struct Snes {
    pub cpu: Cpu65816,
    pub ppu: Ppu,
    pub apu: Apu,
    pub dma: DmaUnit,
    pub cart: Cartridge,
    pub wram: Box<[u8; 0x20000]>,
    pub total_mclk: MCycles,
    pub frame_mclk: MCycles,
    pub memory_refresh_pending: bool,
}

impl Snes {
    #[inline]
    pub fn step(&mut self) -> TickEffect {
        let delta = if self.memory_refresh_pending {
            self.memory_refresh_pending = false;
            MEMORY_REFRESH_CYCLES
        } else if self.dma.active() {
            self.dma.tick(&mut SnesBus::new(&mut self.cart, &mut self.wram,
                                           &mut self.ppu, &mut self.cpu))
        } else {
            let mut bus = SnesBus::new(&mut self.cart, &mut self.wram,
                                       &mut self.ppu, &mut self.cpu_regs);
            self.cpu.step(&mut bus);
            bus.access_master_cycles_total
        };

        let mut effect = TickEffect::default();
        let apu_eff = self.apu.tick(delta);
        effect.audio_samples.extend(apu_eff.audio_samples);

        let ppu_eff = self.ppu.catch_up_to(self.total_mclk + delta);
        if ppu_eff.frame_complete { effect.frame_complete = true; }

        self.total_mclk += delta;
        self.frame_mclk += delta;
        if self.crosses_memory_refresh_boundary(delta) {
            self.memory_refresh_pending = true;
        }
        effect
    }

    pub fn run_to_frame(&mut self, audio_out: &mut Vec<(f32, f32)>) {
        loop {
            let eff = self.step();
            audio_out.extend(eff.audio_samples);
            if eff.frame_complete { return; }
        }
    }
}

// === APU à arithmétique rationnelle ===
impl Apu {
    pub fn tick(&mut self, main_mcycles: MCycles) -> TickEffect {
        self.numerator += main_mcycles * APU_HZ;
        while self.numerator >= CPU_HZ * APU_DIVIDER {
            self.numerator -= CPU_HZ * APU_DIVIDER;
            self.spc700.step(&mut self.bus);
            self.timer0.tick(); self.timer1.tick(); self.timer2.tick();
            if self.sample_divider.tick() { self.emit_sample(); }
        }
        TickEffect::default()
    }
}

// === Interruptions ===
impl Cpu65816 {
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        if self.pending_nmi { self.service_nmi(bus); return; }
        if self.pending_irq && !self.p.contains(Flags::I) {
            self.service_irq(bus); return;
        }
        let opcode = self.fetch_op(bus);
        DISPATCH[opcode as usize](self, bus);
        self.pending_nmi |= bus.nmi_pending();
        self.pending_irq = bus.irq_pending();
    }
}
```

### Risques & mitigations

| Risque | Mitigation |
|---|---|
| **Borrow-checker hostile** (bus mut + cpu mut + ppu mut) | Pattern jgenesis : `SnesBus<'a>` créé à chaque step, emprunts séparés. Pas de `Rc<RefCell>` dans la hot loop |
| **Mid-instruction effects manqués** (Mario Kart, transparency) | `bus.io_cycle()` à CHAQUE accès mémoire CPU. Tester contre Tom Harte 65816 |
| **Dérive APU/CPU** | Arithmétique rationnelle `u64` (pas de float). Test : après 1h, `apu.cycle_count() ≈ apu_freq * elapsed` |
| **DMA timing complexe** | DMA unit séparé qui retourne `DmaStatus { None, InProgress { mcycles } }` |
| **NMI/IRQ timing 1-cycle off** (bug Wild Guns documenté chez jgenesis) | Latcher l'état NMI/IRQ au début d'instruction, servir AVANT le fetch suivant |
| **Performance < 60fps** | Profiler avec `criterion` ; cibler 100M cycles/sec min ARM moderne. Inliner DISPATCH jump-table |
| **Testabilité** | Trait `Bus` permet d'injecter un bus de test (RAM-only). Trait `Component::tick` pour PPU |
| **Save states** | Tous les champs concrets `Serialize`/`Deserialize` via serde + bincode (impossible avec coroutines) |
| **Run-ahead / netplay** | `step()` pur → cloner l'état entier et le rejouer |

### Synthèse exécutive

Adopter le pattern **jgenesis-style** (CPU master-clock catch-up +
`bus.io_cycle()` mid-instruction + APU rationnel) qui est aujourd'hui la
référence Rust pour un SNES cycle-accurate validé contre les test suites
Tom Harte. Le code est lisible (1 itération = 1 step CPU/DMA),
zero-alloc dans la hot loop, et compatible save-states. C'est
l'équivalent fonctionnel de libco/higan **sans coroutines**, en static
dispatch idiomatique Rust.

### Fichiers à étudier en Phase 0

- `jgenesis/backend/snes-core/src/api.rs` (la `Snes::tick` ligne 284)
- `jgenesis/backend/snes-core/src/apu.rs` (catch-up rationnel ligne 274)
- `jgenesis/backend/snes-core/src/memory/dma.rs` (DMA/HDMA timing)
- `jgenesis/backend/snes-core/src/bus.rs` (calcul `access_master_cycles`)
- `tetanes-core/src/cpu.rs` lignes 280-325 (pattern `start_cycle`/`end_cycle`)
- `gameroy/core/src/gameboy.rs` ligne 375 (event prediction pour WAI/STP)

### Sources

- [TetaNES — common.rs](https://github.com/lukexor/tetanes/blob/main/tetanes-core/src/common.rs),
  [cpu.rs](https://github.com/lukexor/tetanes/blob/main/tetanes-core/src/cpu.rs#L280-L325),
  [control_deck.rs](https://github.com/lukexor/tetanes/blob/main/tetanes-core/src/control_deck.rs#L679)
- [jgenesis — api.rs](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/api.rs#L284),
  [apu.rs](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/apu.rs#L274),
  [memory/dma.rs](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/memory/dma.rs)
- [gameroy — gameboy.rs](https://github.com/Rodrigodd/gameroy/blob/master/core/src/gameboy.rs#L375),
  [interpreter.rs](https://github.com/Rodrigodd/gameroy/blob/master/core/src/interpreter.rs#L633)
- [rboy — mmu.rs](https://github.com/mvdnes/rboy/blob/master/src/mmu.rs#L179)
- [DaveTCode/nes-emulator-rust](https://github.com/DaveTCode/nes-emulator-rust/blob/main/emulator/src/cpu/mod.rs#L1049)
- [moa — system.rs](https://github.com/transistorfet/moa/blob/main/emulator/core/src/system.rs)
- [kyle.space — NES emulator post](https://kyle.space/posts/i-made-a-nes-emulator/),
  [bagnalla/6502](https://github.com/bagnalla/6502)
- [desru](https://docs.rs/desru), [nexosim](https://github.com/asynchronics/nexosim)
- [Tom Harte ProcessorTests 65816](https://github.com/SingleStepTests/65816)
