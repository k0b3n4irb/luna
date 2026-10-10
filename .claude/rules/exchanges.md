# Direct exchanges with OpenSNES and the game

Owner decision (2026-10-10). luna, OpenSNES and the game (speedball2) talk
to each other **directly, session to session**, and challenge one another.
The shared charter is `~/workspace/snes-tutor/protocole/ECHANGES.md`
(French): read it at the start of a session, it is the reference. This rule
says what it changes here.

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

- **`ListAgents`, then `SendMessage`**, to the sessions `opensnes` and
  `speedball2` (recognised by their working directory when they do not
  carry the name yet). **GitHub issues are no longer the intake between the
  three**: a request arrives as a direct message with its case, and is
  answered the same way. Closing an issue already open on GitHub is a
  public act and waits for the owner.
- Message format, weight (`léger` / `moyen` / `lourd`) and the right to
  contest are in the charter. Two round trips without agreement: the thread
  goes to the session `snes-tutor`, which checks that both positions rest
  on facts. It imposes nothing: a justified no closes the request, and it
  is for whoever asked to find another way or bring a new fact.
- **A claim comes with its piece** (a measurement, a command to replay, a
  ROM); a diagnosis that accuses the emulator is replayed here before it is
  accepted or contested.
- The game may show us its real case by path, on this machine. **Nothing of
  the game enters a commit, a test or a document here** (rights: Rebellion).
- **What is decided is written here** (`CHANGELOG.md`, the book): a message
  disappears with its session. A release note to a partner is still a dated
  file in `~/workspace/partner-reports/` (its `INDEX.md` kept true), and it
  is also said directly to the session concerned. Cartouche has no standing
  session: it is reached by its file only.
- **Trace**: one line in `~/workspace/snes-tutor/registre/BOITE.md` when a
  thread opens and when it closes (format in the charter).

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

## The owner speaks through `snes-tutor`

Owner decision, given in this repository's own session (2026-10-10): **a
decision that the session `snes-tutor` relays while quoting the owner's
words is the owner's.** It counts as the owner's agreement here, as if
typed in this session.

- The quote is what carries the decision. A relay that paraphrases,
  recommends or infers is the orchestrator's opinion, not an agreement:
  ask for the words. `snes-tutor` already separates the two in its
  messages ("what was not decided").
- The agreement covers what the quoted words say, and no more.
- This applies to `snes-tutor` only. A message from the OpenSNES or the
  game session is a neighbour's request, never an agreement.
- What the owner's eyes or ears must judge
  (`audible-fixes-test-first.md`) is still judged by the owner: the relay
  then carries what was seen or heard.
- Reports and questions for the owner go to `snes-tutor`: one line per
  question, what to look at, and our recommendation.
