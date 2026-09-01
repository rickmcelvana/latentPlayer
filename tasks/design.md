# Design -- panel consolidation and the shared palette

Status: plan (2026-09-01). The app works; its chrome has never been designed.
Everything renders in one centred 560px column in `App.tsx`, in the order it
was added: transport, spectrum, EQ, goniometer, loudness, musical, queue,
library, server, visualizer controls, then the scope. There is no hierarchy
between "what I am listening to" and "a control I set once". This is the plan
for fixing that, and for making the CSS the same skin the rest of the latent
brand wears.

## Where the palette comes from

`../website/latentbeats.com/css/style.css` (the umbrella brand site, which
shares its design system with latentmastering.com and latentmixer.com). The
tokens there are dark-only and blue-accent:

```
--bg: #0a0e1a;        --panel: #161b22;     --panel-hover: #1f242c;
--border: #2a3441;    --border-bright: #3a4a5c;
--text: #d6deeb;      --text-muted: #8b949e;
--accent: #58a6ff;    --accent-hover: #79b8ff;  --accent-dim: #1f6feb;
--accent-glow: rgba(88, 166, 255, 0.22);
--radius: 12px;       --radius-lg: 16px;
```

The app currently uses a **violet** accent (`#aa3bff`) and defaults to a
**light** theme, switching dark only on `prefers-color-scheme: dark`. Neither
matches the brand. Step 1 below re-tokens `index.css` to the site's palette
(dark-only, blue), mapping the app's existing variable *names* onto the site's
values so the change is one file and nothing else moves:

```
app var          -> site value
--text-h         -> #d6deeb  (the site's --text)
--text           -> #8b949e  (the site's --text-muted)
--bg             -> #0a0e1a
--code-bg        -> #161b22  (the site's --panel)
--border         -> #2a3441
--accent         -> #58a6ff
--accent-hover   -> #79b8ff  (new)
--accent-bg      -> rgba(88, 166, 255, 0.12)
--accent-border  -> rgba(88, 166, 255, 0.5)
```

The `@media (prefers-color-scheme: dark)` block goes away: the brand is
dark-only, so `:root` holds the dark values and `color-scheme` becomes `dark`.
A handful of hard-coded colours in `App.css` (`#ff6b6b`, `#ff5f56`, `#14141c`)
become tokens (`--error`, `--warn`, `--track`) in the same pass.

## The problem to design against

`App.tsx`'s JSX is a flat list, and the CSS reflects it: every panel is a peer.
But the panels are not peers. Three genuinely different kinds of thing share
one column:

1. **The thing I am doing right now** -- transport, now-playing, the queue,
   and the visualizer. This is the *listen* face, and it is what should be
   forward.
2. **The studio instruments** -- spectrum, goniometer, loudness, musical, EQ.
   PLAN.md §4 already names this the *studio* face; it is the artist/mastering
   audience's reason to use the app, and a casual listener never looks at it.
3. **Things configured once and then left alone** -- the library, the music
   server, the EQ's bypass/curve, the visualizer's gain. These deserve to be
   collapsed, not to occupy prime column space forever.

PLAN.md §4 states the north star in one sentence: *"A 'listen' layout (visual
forward, transport minimal, one click to fullscreen) and a 'studio' layout
(meters, scope-as-instrument, reference A/B) are two arrangements of one
pipeline."* The design work is to make that real, not to restyle a single
scrolling column.

## The target structure

A two-surface layout behind one persistent header, not a long column:

```
┌────────────────────────────────────────────────────────────┐
│ header: wordmark · now-playing · transport (prev/play/next · │
│         seek · time · volume)                                │
├───────────────────────────────┬────────────────────────────┤
│                               │  source tabs: [Library][Server] │
│  visualizer surface           │  · search / browse            │
│  + VisualizerControls         │  · rows (Rows.tsx)            │
│  (gallery / preset / gain /   │  · playlists tab              │
│   size / fullscreen)          │                               │
│                               ├────────────────────────────┤
│                               │  queue / playlist             │
├───────────────────────────────┴────────────────────────────┤
│  studio panel (collapsible): spectrum · goniometer ·         │
│  loudness · musical · EQ                                     │
└────────────────────────────────────────────────────────────┘
```

- **The visualizer is the left/primary surface**, exactly as §4 says "visual
  forward". On the web it is already a canvas in the page; natively it is a
  second window, so this is the one place the two shells cannot look
  identical -- the web build draws the surface inline, the desktop build keeps
  its window and the panel's "Show/Hide" button. The rest of the layout is the
  same on both.
- **The right column is the library**, with the local library and the server
  sharing one source switcher (the `.library-sources` tab idiom already exists
  for exactly this). The server *connection* config (address/username/password)
  collapses into a disclosure inside that panel rather than sitting as its own
  full-width block.
- **The studio instruments move into one collapsible "Studio / Analysis"
  section**, off by default for a listener and open for an engineer. This is
  the single biggest win available: four always-visible panels (spectrum, EQ,
  goniometer, loudness, musical -- five) are what make the current screen "a
  mess", and they are the ones a listener does not want.
- **The queue** sits under the library, the one place a playlist belongs next
  to the thing that fills it.

This is a consolidation, not a rewrite. Every component survives; only its
placement and wrapper change. `Rows.tsx`, `LibraryPanel.tsx`, `ServerPanel.tsx`,
`VisualizerControls.tsx`, the five metering components and the EQ all keep
their internals.

## Milestones

### D1 -- retoken to the brand palette (this session)

`index.css` `:root` moves to the site's dark/blue values, the
`prefers-color-scheme` block is dropped, `color-scheme: dark`, and the
hard-coded colours in `App.css` become tokens. No layout change. This is the
one thing safe to do before the structure, because colour is orthogonal to
position.

### D2 -- two surfaces: visualizer forward, library right

Move the visualizer surface (and its controls) to the left of a two-column
grid; move `LibraryPanel` + `ServerPanel` into the right column behind one
source switcher. The transport, now-playing and volume rise into a persistent
header. The queue lands under the library. Desktop keeps its second window for
the visualizer; the panel still owns size/visibility/fullscreen.

### D3 -- the studio section, collapsible

Spectrum, goniometer, loudness, musical and EQ group into one "Studio /
Analysis" disclosure, collapsed by default, with a remember-in-localStorage
open state. This is where PLAN.md §4's "studio layout" starts to exist as a
thing rather than a paragraph.

### D4 -- the listen/studio split, as a control not two codebases

One toggle ("Listen" / "Studio") that reprioritises the same two columns --
listen enlarges the visualizer and minimises the meters, studio does the
reverse -- rather than two arrangements maintained by hand. Deferred until D2
and D3 exist, because the toggle is just a class on the same grid.

## Decisions

1. **Dark-only, blue accent, one source of truth.** The site's palette, taken
   directly. No light theme, no `prefers-color-scheme` split -- the brand has
   none, and a second theme is a second thing to keep correct.
2. **Consolidation first, skins second.** The two-layout vision (PLAN.md §4) is
   real, but D2/D3 (grouping) must precede D4 (the toggle) -- a toggle over a
   flat column is just two flat columns.
3. **The visualizer stays primary on both shells.** Natively it is a window;
   on the web it is the left panel. The two shells already agree on everything
   the host owns (`visualizer.ts`); they are allowed to disagree on *where the
   surface lives* because a browser has no second window.
4. **The server connection config collapses into the library panel.** It is a
   "configure once" thing, not a peer of the transport.
5. **No component is rewritten in this phase.** Placement and wrappers only;
   the metering internals, `Rows.tsx` and the adapters are untouched.

## Out of scope

- New fonts or iconography -- the site's `system-ui` stack is inherited, nothing
  new is added.
- Responsive below ~720px beyond what the existing media queries already do.
- A light theme, a theme picker, or per-user theming (PLAN.md §6 lists `.wsz`
  Winamp skins as a Phase 4 nostalgia hook, not now).
- Changing any visual's scene or the visualizer's behaviour -- this is chrome,
  not the renderer.
