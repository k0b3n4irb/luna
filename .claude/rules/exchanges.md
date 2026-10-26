# Direct exchanges with OpenSNES and the game

Owner decision (2026-10-10). luna, OpenSNES and the game port talk
to each other **directly, session to session**, and challenge one another.
This rule says what that changes here.

## Three roles

OpenSNES is the body, luna is the sight, and the game is the confirmation
that the whole assembles. We are how the other two see what their code
does, and how they measure it: a missing measurement, a misleading failure
line or an inaccuracy of ours is something they cannot see around.

## Two axes, both at once

1. **Our own work**: accuracy, our own defects, our roadmap. A neighbour
   asks, it does not command. A message is read at a natural break; only
   what **blocks** a neighbour goes ahead of the work in hand.
2. **Collaboration**, so that we do not isolate ourselves — more than
   answering requests:
   - show what just landed on `develop` to whoever it touches;
   - ask before finishing: an option is shown to whoever will run it;
   - say what we see wrong in a ROM of the SDK or of the game, with nothing
     to ask.

## How

- **`ListAgents`, then `SendMessage`**, to the OpenSNES session and the
  game port's (recognised by their working directory when they do not
  carry the name yet). **GitHub issues are no longer the intake between the
  three**: a request arrives as a direct message with its case, and is
  answered the same way. Closing an issue already open on GitHub is a
  public act and waits for the owner.
- A message opens with one sentence that says what it is about, whether
  it blocks (`bloquant` / `non bloquant`) and its weight (`léger` /
  `moyen` / `lourd`). A request may be contested: a justified no closes
  it, and it is for whoever asked to find another way or bring a new
  fact. Two round trips without agreement: the thread goes to the owner,
  with both positions and the facts they rest on.
- **A claim comes with its piece** (a measurement, a command to replay, a
  ROM); a diagnosis that accuses the emulator is replayed here before it is
  accepted or contested.
- The game may show us its real case by path, on this machine. **Nothing of
  the game enters a commit, a test or a document here** (rights: Rebellion).
- **What is decided is written here** (`CHANGELOG.md`, the book): a message
  disappears with its session. A release note to a partner is still a dated
  file in luna's own archive, `.claude/notes/partners/<partner>/` (ignored
  by git: the game's exchanges stay out of the history; its `INDEX.md` kept
  true, direct messages included), and it is also said directly to the
  session concerned. Every partner hears of what lands on `develop`, not
  only the one it seems to touch (owner, 2026-10-25). Cartouche has no standing
  session: it is reached by its file only.

## The game lives on our `develop`

The game tests with our local `develop`, never a release.

- No second worktree here (`target/` is tens of gigabytes), and the rebuild
  hook rewrites `target/release/luna` at every `.rs` edit. So the game
  consumes **`target/stable/luna`**, a copy refreshed at every validated
  commit, with the commit in `target/stable/COMMIT`. Refresh it when a
  commit is validated, not before.
- What lands on `develop` for a neighbour is **announced to it**: the
  commit, what changes for it, what to try again.
- "Validated" means by this repository's rules: an audible or visible
  change still waits for the owner's eyes or ears
  (`audible-fixes-test-first.md`).
- The release pinned by OpenSNES (`housekeeping.md`) stays what it is for
  their CI; it no longer paces the work between the three.

## What waits for the owner

- A message from the OpenSNES or the game session is a neighbour's
  request, never an agreement: it does not stand for the owner's word.
- A public or irreversible act here (publishing a release, deleting a
  release or a tag, rewriting history) waits for the owner's word, given
  in this session.
- What the owner's eyes or ears must judge
  (`audible-fixes-test-first.md`) is judged by the owner.
- A doubt, or a decision that is not ours alone, goes to the owner in one
  line with our recommendation: what to look at, and what we would do.
