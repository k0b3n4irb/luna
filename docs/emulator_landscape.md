# Émulateurs SNES — Panorama comparatif

Tour d'horizon des principaux émulateurs Super Nintendo (SNES / Super Famicom),
classés par niveau de maturité, fidélité matérielle et richesse fonctionnelle.
L'objectif : aider à choisir l'émulateur adapté à un usage donné (jeu casual,
préservation, développement homebrew, intégration multi-systèmes…).

---

## Sommaire

- [Critères d'évaluation](#critères-dévaluation)
- [Classement synthétique](#classement-synthétique)
- [Fiches détaillées](#fiches-détaillées)
  - [ares](#ares)
  - [bsnes](#bsnes)
  - [Mesen-S / Mesen 2](#mesen-s--mesen-2)
  - [no$sns](#nosns)
  - [Snes9x](#snes9x)
  - [bsnes-mercury (core RetroArch)](#bsnes-mercury-core-retroarch)
  - [higan](#higan)
  - [ZSNES](#zsnes)
- [Stacks techniques en un coup d'œil](#stacks-techniques-en-un-coup-dœil)
- [Tableau récapitulatif](#tableau-récapitulatif)
- [Recommandations par usage](#recommandations-par-usage)
- [Glossaire](#glossaire)
- [Sources](#sources)

---

## Critères d'évaluation

Chaque émulateur est noté selon trois axes :

- **Maturité** : ancienneté du projet, stabilité, activité du développement,
  taille de la communauté.
- **Fidélité matérielle** : exactitude de la simulation du hardware
  (cycle-accurate vs HLE), gestion des puces d'extension (Super FX, SA-1,
  DSP, SPC7110…), comportements audio/vidéo conformes à la console réelle.
- **Fonctionnalités** : save states, rewind, netplay, shaders, outils de
  debug, support multi-plateformes, customisation…

---

## Classement synthétique

| Rang | Émulateur        | Maturité | Fidélité HW | Fonctionnalités | Profil       |
|------|------------------|----------|-------------|-----------------|--------------|
| 🥇   | **ares**         | ★★★★★    | ★★★★★       | ★★★★☆           | Précision moderne |
| 🥈   | **bsnes**        | ★★★★☆    | ★★★★★       | ★★★☆☆           | Référence accuracy |
| 🥉   | **Mesen-S**      | ★★★★☆    | ★★★★★       | ★★★★★ (debug)   | Dev / homebrew |
| 4    | **Snes9x**       | ★★★★★    | ★★★★☆       | ★★★★☆           | Polyvalent grand public |
| 5    | **bsnes-mercury**| ★★★★☆    | ★★★★★       | ★★★★☆ (RA)      | Intégration RetroArch |
| 6    | **no$sns**       | ★★★☆☆    | ★★★★☆       | ★★★★☆ (debug)   | Reverse engineering / doc |
| 7    | **higan**        | ★★☆☆☆    | ★★★★★       | ★★★☆☆           | Obsolète (→ ares) |
| 8    | **ZSNES**        | ★☆☆☆☆    | ★★☆☆☆       | ★★☆☆☆           | Historique uniquement |

---

## Fiches détaillées

### ares

Émulateur multi-systèmes open source, fork de higan, considéré comme son
successeur spirituel. Maintient le cœur d'émulation très précis de bsnes/higan
tout en restant activement développé et plus accessible.

**Points forts**
- Émulation cycle-accurate du SNES (intègre le cœur bsnes).
- Compatibilité quasi totale, y compris les puces exotiques
  (Super FX, SA-1, DSP-1/2/3/4, SPC7110, S-DD1, Cx4…).
- Interface moderne, configuration plus simple que higan.
- Fonctionnalités modernes : run-ahead (réduit la latence d'entrée),
  rewind, save states, shaders CRT, correction des couleurs.
- Couvre de nombreux systèmes en plus du SNES (NES, GB/GBC, GBA, N64, PSX
  expérimental, Master System, Mega Drive, PC Engine, etc.).
- Développement actif.

**Points faibles**
- Plus gourmand en CPU que Snes9x.
- Pas de core RetroArch officiel (restrictions de licence).
- Disponibilité limitée à Windows / Linux / macOS (pas d'Android, iOS, ni
  consoles portables).
- Moins de plugins / extensions communautaires que RetroArch.

**Ce qui le distingue** : c'est aujourd'hui *la* combinaison optimale entre
précision et ergonomie. Si l'on veut "le bsnes 2026", c'est ares.

**Stack technique**

- Langage principal : **C++** (~94,6%), avec ~4% de C, plus du CMake,
  GLSL et un peu d'Objective-C pour le glue code macOS.
- Philosophie de code affichée : **clarté avant performance** (ce qui
  explique en partie l'exigence CPU vs Snes9x).
- N'utilise **pas la STL standard** — repose sur un écosystème de
  bibliothèques maison héritées de higan/bsnes (écrites par Near) :

  | Bibliothèque | Rôle |
  |---|---|
  | **nall** | Alternative à la STL (containers, strings, utilitaires) |
  | **hiro** | Toolkit GUI cross-platform utilisant les API natives (Win32, GTK, Cocoa) |
  | **ruby** | Couche d'abstraction vidéo/audio/input (Direct3D, OpenGL, ALSA…) |
  | **libco** | Multi-threading coopératif (coroutines) |
  | **mia** | Base de données ROM et loader interne |

- Le choix de **libco** (coroutines coopératives) est l'astuce
  architecturale clé : chaque composant émulé (CPU, PPU, APU,
  coprocesseurs) est écrit comme un "thread" qui rend la main au
  scheduler après X cycles, ce qui rend le code cycle-accurate lisible
  plutôt qu'une machine à états imbriquée.
- Build system : GNU make (avec profils `debug` / `stable` / `release` /
  `minified` / `optimized`).
- Dépôt : [github.com/ares-emulator/ares](https://github.com/ares-emulator/ares).

---

### bsnes

Émulateur SNES historique créé par byuu (Near). Conçu dès l'origine pour
être l'émulateur le plus précis possible, au prix d'exigences CPU élevées.

**Points forts**
- Pionnier de l'émulation cycle-accurate SNES.
- Trois profils historiques : *Performance*, *Balanced*, *Accuracy*.
- Excellente compatibilité (proche du 100%).
- Options graphiques expérimentales (upscaling Mode 7 HD, widescreen
  hacks sur certains jeux).
- Code source de référence pour la documentation du hardware SNES.

**Points faibles**
- Développement ralenti depuis la disparition de Near (2021).
- Les forks récents convergent désormais vers ares.
- Moins d'outils intégrés que Mesen-S côté debug.
- Interface moins moderne qu'ares.

**Ce qui le distingue** : le projet fondateur de l'émulation SNES haute
fidélité. Toujours pertinent, mais ares est généralement recommandé à sa
place pour bénéficier d'un développement actif.

**Stack technique**

- Langage principal : **C++**.
- Partage la même base technique que ares/higan : utilise **nall**
  (alternative à la STL), **hiro** (GUI native cross-platform), **ruby**
  (couche vidéo/audio/input) et **libco** (coroutines coopératives) —
  toutes développées par Near.
- Architecture cycle-accurate basée sur libco, exactement comme ares.
- Licence : **GPLv3**.
- Build : GNU make.
- Dépôt actuel : [github.com/bsnes-emu/bsnes](https://github.com/bsnes-emu/bsnes).

---

### Mesen-S / Mesen 2

Mesen-S est l'extension SNES du célèbre émulateur NES "Mesen". Depuis
Mesen 2, les deux ont fusionné dans un émulateur multi-systèmes unique
(NES, SNES, GB/GBC, PC Engine).

**Points forts**
- Émulation cycle-accurate du SNES.
- **Outils de debug exceptionnels**, parmi les meilleurs tous émulateurs
  confondus :
  - Debugger avec breakpoints, watch, labels.
  - Assembleur intégré.
  - Event Viewer (raster, DMA, IRQ…).
  - Tile / Sprite / Palette / Tilemap viewers.
  - Trace Logger, Performance Profiler.
  - Script window (Lua).
- HD packs, video filters, netplay, rewind, overclocking, palettes
  personnalisées.
- Interface claire, configuration par jeu sauvegardée automatiquement.

**Points faibles**
- Plateformes limitées (Windows principalement, Linux via build).
- Pas de version mobile.
- Communauté plus petite que Snes9x.
- Moins orienté "joueur lambda" — la richesse de l'UI peut intimider.

**Ce qui le distingue** : c'est l'émulateur de référence pour le
**développement homebrew** et le **ROM hacking**. Aucun concurrent n'offre
un tel niveau d'outillage de debug intégré.

**Stack technique**

- **Architecture bi-langage** typique de Mesen :
  - **Cœur d'émulation en C++** (CPU, PPU, APU, coprocesseurs) pour la
    performance.
  - **Interface graphique et outils de debug en C#** (.NET) — d'abord
    WinForms, puis Avalonia pour Mesen 2 afin de gagner en
    portabilité Linux/macOS.
- Cette séparation explique la richesse de l'UI debug : C# permet de
  développer rapidement les nombreuses fenêtres d'outils sans
  compromettre la perf du cœur.
- Licence : **GPLv3**.
- Dépôts :
  - [github.com/SourMesen/Mesen-S](https://github.com/SourMesen/Mesen-S) (historique, plus maintenu)
  - [github.com/SourMesen/Mesen2](https://github.com/SourMesen/Mesen2) (actif, recommandé)

---

### no$sns

Émulateur/debugger Windows développé par **Martin Korth** (alias "Martin
Korth de Problemkaputt"), auteur de la lignée no$ (no$gba, no$gmb, no$nes,
no$psx…), longtemps réputée pour la précision technique et la qualité
inégalée de la documentation hardware associée.

**Points forts**
- **Documentation hardware "fullsnes" exceptionnelle** : la
  [référence fullsnes.htm](https://problemkaputt.de/fullsnes.htm) est
  considérée comme **la** spec officieuse du SNES par la communauté
  homebrew/reverse — utilisée par les autres émulateurs eux-mêmes comme
  source primaire (registres, timings, comportements des coprocesseurs).
- Debugger très soigné : assembleur, désassembleur intégrés.
- **Seul émulateur** offrant un debug poussé des coprocesseurs au-delà du
  SPC700 (SA-1, Super FX, DSP, CX4, ST018, SPC7110…).
- Émulation très large des add-ons et accessoires exotiques :
  Satellaview, Super Disc CDROM, Turbofile, lightguns, Exertainment Bike,
  Barcode Battler, X-Band Keyboard, NTT Data Pad…
- **Xboo-Upload** : permet d'envoyer du code directement sur du vrai
  hardware SNES pour test (rare).
- Compact, démarre instantanément, interface "old-school" dense mais
  efficace.

**Points faibles**
- **Closed source** (contrairement à tous les autres émulateurs sérieux
  de cette liste).
- Développement très lent : dernière version 1.9 en 2017, peu de mises à
  jour depuis (le projet est en quasi-hibernation).
- Pas de **watchpoints** (data breakpoints) — limite forte pour le debug,
  obligeant à compléter avec bsnes/Mesen-S.
- Précision réputée bonne sur les jeux courants mais inférieure à
  bsnes/ares/Mesen-S sur les cas limites.
- Windows uniquement (fonctionne via Wine sur Linux/macOS).
- Interface très datée, ergonomie déroutante pour les nouveaux venus.
- Version "gratuite" bridée, version "no$sns debug" payante (donation
  via le site de l'auteur).

**Ce qui le distingue** : son **apport majeur à l'écosystème SNES n'est
pas l'émulateur lui-même mais la documentation fullsnes**, qui a permis
à toute une génération de développeurs et d'émulateurs concurrents
(bsnes, Mesen-S, ares) de progresser. C'est aussi le seul à pousser le
debug des coprocesseurs à ce niveau.

À utiliser en **complément** d'un autre émulateur (typiquement Mesen-S
pour les watchpoints, no$sns pour la doc et le debug coprocesseur).

**Stack technique**

- **100% assembleur x86** — c'est la signature de toute la lignée no$ de
  Martin Korth (no$gba, no$gmb, no$nes, no$psx, no$sns).
- Conséquence directe : **empreinte mémoire minuscule** et performances
  extrêmes. Martin Korth indique que "sur des PC à 1 GHz, la plupart des
  jeux tournent 5 à 10× plus vite que sur le hardware réel".
- **Closed source** — code source non publié.
- **x86 32 bits uniquement**, ce qui rend impossible tout portage natif
  vers ARM, x86-64 pur, ou autre architecture. Sous Linux/macOS, il faut
  passer par Wine.
- Pas de build system standard, pas de dépôt public — distribution
  uniquement via binaires sur [problemkaputt.de](https://problemkaputt.de/sns.htm).
- À noter : l'auteur a libéré certains de ses émulateurs (no$gba 2.7c+)
  en freeware, mais pas le code source.

---

### Snes9x

Émulateur SNES historique, le plus populaire pour le grand public. Existe
depuis 1997 et continue d'évoluer.

**Points forts**
- Compatibilité élevée (~99,5% du catalogue SNES).
- Très léger en CPU, tourne sur du matériel modeste.
- Disponible sur quasiment toutes les plateformes :
  Windows, Linux, macOS, Android, iOS, 3DS, PSP, Wii, Xbox, Switch
  (homebrew), navigateurs (WASM), etc.
- Save states, netplay, cheats, fast-forward, slow motion, support
  manettes, multijoueur, customisation très poussée.
- Développement actif, communauté large.
- Forks dérivés extrêmement nombreux.

**Points faibles**
- Pas cycle-accurate : utilise quelques approximations HLE pour la
  performance.
- Quelques jeux à effets pointus ou démos techniques peuvent présenter
  des défauts subtils invisibles pour le joueur lambda.
- Outils de debug nettement plus rudimentaires que Mesen-S.

**Ce qui le distingue** : le meilleur rapport
**compatibilité / performance / portabilité**. C'est *le* choix par défaut
recommandé à 95% des utilisateurs.

**Stack technique**

- Langage principal : **C++**, avec quelques portions historiques en C et
  d'anciens cœurs CPU partiellement en assembleur (largement retirés au
  fil des versions au profit de la portabilité).
- **Pas de dépendance à un toolkit GUI unique** : le cœur est un moteur
  d'émulation pur, et plusieurs front-ends officiels ou tiers coexistent
  (GTK, Qt, Windows natif, Cocoa, SDL, Android…).
- Cette architecture découplée est ce qui explique la **portabilité
  exceptionnelle** : il est trivial pour un développeur tiers de greffer
  le cœur Snes9x sur n'importe quelle plateforme.
- Licence : **non-commerciale** (custom, dérivée d'un esprit BSD mais
  avec clause interdisant l'usage commercial sans accord — d'où l'absence
  de Snes9x dans certaines distributions Linux commerciales).
- Build : Autotools / Make selon la plateforme.
- Dépôt : [github.com/snes9xgit/snes9x](https://github.com/snes9xgit/snes9x).

---

### bsnes-mercury (core RetroArch)

Fork de bsnes maintenu par la communauté libretro, conçu pour s'intégrer
dans RetroArch en restant aussi précis que le bsnes officiel.

**Points forts**
- Précision identique à bsnes par défaut (les HLE optionnels sont
  désactivés).
- Trois cores disponibles : Performance / Balanced / Accuracy.
- Bénéficie de tout l'écosystème RetroArch :
  shaders, netplay, achievements (RetroAchievements), run-ahead,
  rewind, gestion unifiée des manettes, sauvegardes cloud.
- Disponible sur toutes les plateformes RetroArch
  (incluant Android, consoles, Raspberry Pi).
- FPS et taux d'échantillonnage conformes à la norme SNES NTSC/PAL.

**Points faibles**
- Nécessite RetroArch (UI déroutante pour les débutants).
- Le core "Accuracy" reste exigeant en CPU.
- Pas d'outils de debug avancés.

**Ce qui le distingue** : c'est la seule façon d'avoir une précision
type bsnes **dans RetroArch**, donc dans un environnement multi-systèmes
unifié.

**Stack technique**

- Langage principal : **C++**, hérité directement de bsnes (et donc des
  libs nall/libco).
- **Wrappé en core libretro** : le code expose l'API libretro standard,
  ce qui permet à RetroArch (et tout front-end libretro) de le charger
  comme une bibliothèque dynamique (`.so` / `.dll` / `.dylib`).
- Les modifications par rapport au bsnes officiel concernent surtout :
  fonctionnalités restaurées, optimisations ciblées, intégration des
  hooks libretro (input, audio, video callbacks).
- Licence : **GPLv3** (héritée de bsnes).
- Build : GNU make avec adaptations libretro.
- Dépôt : [github.com/libretro/bsnes-mercury](https://github.com/libretro/bsnes-mercury).

---

### higan

Évolution historique de bsnes par Near, ayant élargi le projet à plusieurs
systèmes Nintendo (NES, SNES, GB/GBC/GBA, Famicom Disk System, Super Game
Boy, Satellaview…).

**Points forts (historiques)**
- Premier émulateur à atteindre 100% de compatibilité SNES.
- Premier à émuler correctement SPC7110, cycle-accurate SPC700,
  Super FX, Super Game Boy.
- Renderer dot-based pour le GBA (au lieu de scanline).

**Points faibles**
- Plus maintenu : remplacé par ares.
- Interface réputée austère et déroutante.
- Configuration des ROMs (Game Pak) compliquée pour les nouveaux venus.

**Ce qui le distingue** : intérêt historique uniquement. Tous les
avantages techniques de higan sont aujourd'hui présents dans ares, avec
en plus un développement actif et une UI plus accessible.

**Stack technique**

- Langage principal : **C++**, écrit par Near.
- **Origine de l'écosystème nall / hiro / ruby / libco** : ces
  bibliothèques ont été conçues pour higan, et sont aujourd'hui
  réutilisées par bsnes et ares.
- Architecture cycle-accurate basée sur libco (coroutines coopératives).
- Licence : **GPLv3**.
- Build : GNU make.
- Statut : **archivé** — le développement a basculé vers ares.

---

### ZSNES

L'un des tout premiers émulateurs SNES grand public (1997). Très
populaire dans les années 2000 grâce à ses performances sur les machines
de l'époque.

**Points forts (historiques)**
- Très performant sur du matériel d'époque.
- Interface visuelle "console" appréciée à l'époque.
- Énorme bibliothèque de ROM hacks compatibles.

**Points faibles**
- Développement abandonné depuis 2007.
- Beaucoup d'imprécisions hardware (utilisait des hacks haute fidélité
  absente).
- Vulnérabilités de sécurité connues dans le code x86 hand-written.
- Compatibilité inférieure aux émulateurs modernes.

**Ce qui le distingue** : intérêt purement historique / nostalgique.
**À éviter** pour tout usage sérieux aujourd'hui.

**Stack technique**

- **Massivement écrit en assembleur x86** (la signature emblématique de
  ZSNES), avec un peu de C et C++ pour le glue code et la GUI.
- En version 1.50 (2006), environ **15%** seulement du code asm avait
  été porté en C — le reste était (et est resté) en assembleur x86 32
  bits.
- Conséquences directes :
  - Performances spectaculaires sur le matériel des années 1990–2000.
  - **Portage quasi impossible** vers d'autres architectures (ARM,
    x86-64 pur, PowerPC…), ce qui a scellé son obsolescence.
  - **Vulnérabilités de sécurité** : le code asm hand-written contient
    plusieurs failles de type buffer overflow exploitables via des ROMs
    malicieuses (CVE documentées).
- Licence : **GPLv2**.
- Statut : **abandonné depuis 2007**. Quelques forks tentent de
  maintenir une build moderne (ex.
  [github.com/xyproto/zsnes](https://github.com/xyproto/zsnes)).

---

## Stacks techniques en un coup d'œil

| Émulateur      | Langage(s) principal(aux)       | Licence        | Open source | Toolkit GUI            | Source notable                              |
|----------------|----------------------------------|----------------|-------------|------------------------|---------------------------------------------|
| ares           | C++ (~95%)                       | GPLv3 / ISC    | ✅          | hiro (natif)           | nall / hiro / ruby / libco / mia (Near)     |
| bsnes          | C++                              | GPLv3          | ✅          | hiro (natif)           | nall / hiro / ruby / libco (Near)           |
| Mesen-S / 2    | **C++ (cœur) + C# (.NET / Avalonia pour Mesen 2)** | GPLv3 | ✅ | WinForms → Avalonia    | Architecture bi-langage cœur/UI             |
| no$sns         | **100% assembleur x86 (32-bit)** | Propriétaire   | ❌          | Win32 natif (asm)      | Lignée no$ de Martin Korth                   |
| Snes9x         | C++ (peu de C, ex-asm retiré)    | Non-commerciale custom | ✅  | Multiple (GTK, Qt, Win32…) | Cœur portable, front-ends découplés     |
| bsnes-mercury  | C++ (fork bsnes)                 | GPLv3          | ✅          | Aucun (core libretro)  | Wrap libretro                                |
| higan          | C++                              | GPLv3          | ✅          | hiro (natif)           | Origine de nall / hiro / ruby / libco       |
| ZSNES          | **Assembleur x86 (~85%) + C/C++** | GPLv2         | ✅          | Custom (mode console)  | Quasi-impossible à porter ailleurs que x86  |

> **À retenir** :
> - Quatre familles techniques se dégagent :
>   1. **L'école Near** (ares, bsnes, higan, bsnes-mercury) : C++ + nall + hiro + libco, cycle-accurate via coroutines coopératives.
>   2. **L'école Mesen** (Mesen-S, Mesen 2) : cœur C++ haute perf + UI C# riche en outils.
>   3. **L'école portable** (Snes9x) : C++ pur, cœur découplé de la GUI, optimisé pour la portabilité maximale.
>   4. **L'école assembleur** (no$sns, ZSNES) : performances brutes au prix de la portabilité et de la maintenabilité.

---

## Tableau récapitulatif

| Émulateur      | Type        | Cycle-accurate | Multi-plateforme           | Debug | Netplay | Shaders | Run-ahead | RetroArch core |
|----------------|-------------|----------------|----------------------------|-------|---------|---------|-----------|----------------|
| ares           | Multi-sys   | ✅             | Win / Linux / macOS         | Basique | ❌      | ✅      | ✅        | ❌             |
| bsnes          | SNES dédié  | ✅             | Win / Linux / macOS         | Basique | ❌      | ⚠️ Limité | ❌      | ✅ (officiel)  |
| Mesen-S        | Multi-sys   | ✅             | Win / Linux                 | ✅✅✅ | ✅      | ✅      | ❌        | ❌             |
| no$sns         | SNES dédié  | ≈ (bonne)      | Windows (Wine ailleurs)     | ✅✅ (coproc.) | ❌ | ❌      | ❌        | ❌             |
| Snes9x         | SNES dédié  | ❌ (HLE part.) | Tout                        | Basique | ✅      | ✅      | ✅ (via RA) | ✅           |
| bsnes-mercury  | SNES dédié  | ✅             | Tout (via RetroArch)        | ❌    | ✅ (RA) | ✅ (RA) | ✅ (RA)   | ✅             |
| higan          | Multi-sys   | ✅             | Win / Linux / macOS         | Basique | ❌      | ❌      | ❌        | ❌             |
| ZSNES          | SNES dédié  | ❌             | Win / DOS (legacy)          | ❌    | ✅ (LAN) | ❌    | ❌        | ❌             |

---

## Recommandations par usage

| Usage                                                  | Recommandation principale            | Alternative              |
|--------------------------------------------------------|--------------------------------------|--------------------------|
| Jouer sur PC moderne, expérience fidèle                | **ares**                             | bsnes / bsnes-mercury    |
| Jouer sur matériel modeste / Raspberry Pi              | **Snes9x**                           | bsnes-mercury Performance|
| Jouer sur mobile (Android / iOS)                       | **Snes9x EX+** (Android)             | RetroArch + Snes9x core  |
| Développement homebrew / ROM hacking                   | **Mesen-S / Mesen 2**                | bsnes (debug build)      |
| Reverse engineering hardware / doc des coprocesseurs   | **no$sns** + fullsnes.htm            | Mesen-S                  |
| Multi-systèmes + shaders + achievements                | **RetroArch** (core bsnes-mercury)   | RetroArch + Snes9x       |
| Préservation / archivage (référence académique)        | **ares** ou **bsnes**                | Mesen-S                  |
| Speedrun (lag input minimal)                           | **ares** ou **Snes9x** + run-ahead   | bsnes-mercury            |
| Netplay / multijoueur en ligne                         | **RetroArch** (Snes9x ou bsnes-mercury) | Snes9x natif          |

---

## Glossaire

- **Cycle-accurate** : simulation du processeur cycle par cycle, reproduisant
  fidèlement le comportement temporel du hardware. S'oppose aux approches
  HLE qui simulent les résultats sans reproduire la chronologie réelle.
- **HLE (High-Level Emulation)** : émulation "haut niveau" où certaines
  parties du hardware (puces audio, coprocesseurs) sont remplacées par des
  équivalents logiciels plus rapides mais moins précis.
- **Run-ahead** : technique consistant à exécuter plusieurs frames à
  l'avance puis revenir en arrière, pour cacher la latence d'entrée
  inhérente à l'émulation.
- **Save state** : sauvegarde de l'état complet de la machine émulée à un
  instant donné, restaurable instantanément.
- **Puces d'extension SNES** : Super FX (Star Fox), SA-1 (Super Mario RPG),
  DSP-1/2/3/4 (Super Mario Kart, Pilotwings…), SPC7110 (Far East of Eden
  Zero), S-DD1 (Star Ocean), Cx4 (Mega Man X2/X3).
- **Mode 7** : mode graphique du SNES permettant la rotation et la mise à
  l'échelle d'un plan de tiles (sol de F-Zero, Mario Kart…).
- **Libretro / RetroArch** : framework open source d'émulation multi-systèmes
  utilisant des "cores" (les émulateurs eux-mêmes) chargés dans une UI
  unifiée.

---

## Sources

- [Higan — Wikipedia](https://en.wikipedia.org/wiki/Higan_(emulator))
- [SNES emulators — Emulation General Wiki](https://emulation.gametechwiki.com/index.php/Super_Nintendo_Entertainment_System_emulators)
- [Mesen-S Documentation](https://www.mesen.ca/snes/docs/)
- [bsnes-mercury Accuracy — Libretro Docs](https://docs.libretro.com/library/bsnes_mercury_accuracy/)
- [bsnes-mercury Performance — Libretro Docs](https://docs.libretro.com/library/bsnes_mercury_performance/)
- [Best SNES Emulators 2026 — RetroDodo](https://retrododo.com/best-snes-emulators/)
- [ares vs Snes9x EX — Comparison](https://sugggest.com/compare/ares-formerly-higan-bsnes--vs-snes9x-ex)
- [Snes9x Alternatives — AlternativeTo](https://alternativeto.net/software/snes9x/)
- [no$sns — Problemkaputt (Martin Korth)](https://problemkaputt.de/sns.htm)
- [fullsnes.htm — Specification SNES de référence](https://problemkaputt.de/fullsnes.htm)
- [No$ — Emulation General Wiki](https://emulation.gametechwiki.com/index.php/No$)
