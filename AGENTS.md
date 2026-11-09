# Agent requirements

**Write the code so that the evolution of this app feels modular.**

This is a product requirement, not a stylistic suggestion. Read
`AGENT_CONTEXT.md` and `docs/UI_ARCHITECTURE_AUDIT.md` before changing behavior.

## Why this rule exists

The user observed transitions that worked in one direction but snapped in the
reverse direction, chat-to-command transitions that did not behave like
compact-to-command transitions, Escape that dismissed only some routes, and
keyboard access that depended on clicking a particular widget. These are signs
of fragmented ownership and patch-by-patch design. Do not add a separate
animation or focus workaround for each reported symptom.

- Identify the shared invariant and its owner before editing. Explain the root
  cause, not just the visual change. Do not claim every line is justified simply
  because it compiles.
- Derive presentation routes centrally (`AppState::panel`). A new panel must use
  the shared geometry and content transition pipeline, including reverse paths,
  interruptions, same-size changes, and retained outgoing content.
- Separate logical state, measured layout, animated presentation, hit testing,
  and compositor focus ownership. Do not crop current geometry to target bounds.
- Focus eligibility is not keyboard ownership. On niri, visible OnDemand layers
  can be clicked anywhere; explicit summons remap for initial focus. A keyboard
  leave, hover, stream event, or frame must never request another summon.
  Never use Exclusive. Outside clicks must hand control to the other app.
- Escape dismisses the overlay in every route. Do not overload it based on chat
  history, empty drafts, or generation status. Hiding must not erase chat/drafts.
- Keep blocking I/O off frame-critical paths. Use monotonic elapsed time, bounded
  event processing, and compositor frame callbacks; no frame-count durations.
- Remove obsolete switches, unused animation state, contradictory comments and
  documentation when changing an invariant. Add matrix/regression tests for the
  shared rule rather than just the reported path.
- Preserve native Rust/Wayland, Quiet Paper typography/palettes, optical streaming
  blur, keyless endpoints, and the minimal UI. No web toolkit or feature sprawl.
- Build/test and inspect live niri behavior before saying a UX problem is fixed
  in the shipped binary. Old logs/screenshots/AppImages are not current proof.

Do not perform a speculative whole-project rewrite. Refactor along actual
ownership boundaries, record remaining technical debt honestly, and make each
change reviewable. Work only on the session's assigned branch.

## User's intent & observations (from prompt)

The user's direct observations and requirements for the app's feel, animations, and efficiency:

> "the ui problems i didnt talk about them:
> 
> mostly its with markdown components like tables etc
> 
> firstly theres no streaming animation inside the thinking blocks, dont make it as strong as the general one, but give it a streaming animation too, a very simple one but give it, also the thinking box expansion and contraction doesnt have any animation, also general scrolling in the chatarea feels very slow and like tedious, like just weird
> 
> also the app making animations from the chatscreen looks weird like the pallete to chatscreen or chatscreen to pallette those animations feel like theyre 20fps or smthn, its genuinely lagging a bit, the entire application, while scrolling and all when in chat screen
> 
> maybe theyre not very efficient, (research about rust, maybe youre bad at writing it and using old libraries or something, okay actually do this now, audit the whole codebase again going over everything with your search tool, researching alternatives, better implementation ways, more efficient ways to write rust, newer libraries etc... this is the main issue i think, its definitely lagging a lot)"

### Core rules derived from the user's intent:
- **No 20fps or stuttering animations**: Route transitions (palette to chatscreen, chatscreen to palette) and scrolling must feel completely fluid and high-framerate (60–120+ FPS), not lagging or dropping frames due to inefficient rendering.
- **Thinking box streaming animation**: Thinking blocks must have a simple, subtle streaming animation (not as strong as the main response text, but visible and pleasant).
- **Thinking box expansion/contraction animation**: Expanding and contracting the thinking box must be smoothly animated, never snapping instantly.
- **Natural, fluid chat scrolling**: General scrolling in the chat area must never feel slow, tedious, or weird; it must glide with responsive kinetic momentum.
- **Markdown components (tables, code blocks)**: Tables and code blocks must render properly without awkward clipping or wrapping, with readable column widths, subtle dividers, and easy horizontal scrolling when hovered.
- **Research modern, high-efficiency Rust**: Do not settle for naive or outdated implementations; search for better alternatives, modern libraries, and high-performance Rust patterns that eliminate lag.

## Rendering/performance rules learned in the Markdown audit

Read `docs/RENDERING_RESEARCH.md` for upstream sources, researched versions,
implemented changes and explicit remaining risks.

- **A viewport clips everything: glyphs, Gaussian blur, fills, strokes, inline
  decorations, selections, hover regions and action hitboxes.** A card mask alone
  is not a chat viewport, and a glyph clip is not a border clip. Nested code,
  table-cell and settings-field viewports must intersect their parent.
- Retain immutable layout separately from clocks. Do not reparse/reshape settled
  messages for a caret blink, route spring or scroll frame. Cache invalidation
  includes source, width, palette and source-metadata topology; cached animation
  ages are a correctness bug. Use Rc for single-threaded retained scenes, Arc only
  when actual shared-thread ownership requires it. Bound caches.
- Wide components share one overflow viewport abstraction. Code is unwrapped;
  tables have readable column widths. Horizontal input stays local to the hovered
  component. Do not steal ordinary vertical transcript scrolling.
- Do not send already-smooth touchpad deltas through a second slow spring. Wheel
  detents/value120 and continuous coordinates have different units. Audit input
  normalization whenever upgrading the Wayland toolkit.
- Preserve Markdown source offsets through syntax highlighting and healing.
  Code/table/reasoning content must not silently bypass the streaming pipeline.
  Reasoning uses a gentler effect than the main answer and has continuous measured
  expansion/contraction, not just a boolean visibility switch.
- HiDPI is a pixel-work multiplier. Cache low-frequency effects such as shadows;
  keep typography native-resolution. Do not trade correctness for unmeasured
  SIMD/unsafe/allocator changes or globally enable nonportable CPU instructions.
- Use `scripts/benchmark-render.sh`, the profiling Cargo profile, wide-content
  screenshots, Unicode/incomplete-Markdown tests and live niri verification.
  Report CPU p50/p95 separately from compositor FPS. An anecdotal smoothness claim
  is not a performance measurement; passing tests is not million-user validation.
- Current upstreams researched: cosmic-text 0.19, tiny-skia 0.12, SCTK 0.21. These
  are **migration candidates, not the versions currently locked by this patch**.
  Zero-major upgrades can break APIs/MSRV/input semantics. Update manifests,
  lockfile, supported Rust version and tests coherently; never handwave a version
  bump as a solution to an application-level algorithmic problem.

## 9-Slice Shadow Caching, Momentum Physics & Zero-Allocation Rendering

- **Precomputed 9-Slice Box Shadow Architecture**:
  - Full-screen CPU Gaussian blurs (evaluating `fast_erf` across 3.84 million pixels twice per frame) cost 55ms–61ms per frame at 2x HiDPI, dropping animated card resizing to 18fps.
  - A rounded rectangle shadow consists of 4 identical quarter-circle corners and 4 flat edges. In X, horizontal edges have zero variance; in Y, vertical edges have zero variance.
  - Precomputing a compact 9-slice shadow template (corner radius + margin) rendered once at native physical scale (`scale`) allows drawing shadows for *any* card size in <0.05ms by blitting the 4 corners and 1D-stretching the 4 straight edges.
  - Blitting uses zero heap allocations and integer bit-shifts for premultiplied alpha compositing (`(src * opacity) >> 8`), completely eliminating frame-time spikes during spring transitions.
  - Avoid copying panel snapshots (`retain_panel`) during active motion; snapshot only once at steady state or upon transition completion.

- **GSAP / Lenis Inspired Scroll Physics & Hover Routing**:
  - Overdamped springs (`stiffness: 1600`) settle in ~25ms, causing wheel detents to appear as abrupt 10fps steps. Retuning `SpringSpec::SCROLL` to `stiffness: 240.0, damping: 31.0` yields natural kinetic gliding that feels weightless and fluid.
  - Discrete mouse wheel detents require proper scaling (112.0px per notch) to match modern high-DPI desktop expectations.
  - Continuous touchpad events must retarget the kinetic spring target rather than hard-snapping (`snap_to`), allowing inertia on finger release.
  - Hover routing allows standard mouse wheel scrolling over overflowing code blocks or tables to scroll horizontally without requiring the user to hold the Shift key.

- **Strict Viewport Isolation & Reasoning Block Polish**:
  - Vertical indicator lines, quote bars, and content runs must be strictly clamped to the viewport bottom (`(card_y + viewport_h - my).max(0.0)`).
  - The bottom composer omnibox must draw an opaque background fill (`theme.float`) to ensure content scrolling behind it never bleeds through.
  - Streaming inside `<think>` reasoning blocks uses a subtle blur-to-focus (`sigma: 1.1 -> 0.0`, `opacity: 0.25 -> 1.0`, `offset_y: 0.8 -> 0.0`) with easing to provide gentle visual feedback without distracting from the main response.
  - Accordion expand/collapse for thinking blocks uses `SpringSpec::DEFAULT_SPATIAL` rather than instant snapping.

- **Markdown Code & Table Layout**:
  - Code blocks set `Wrap::None`. In `cosmic-text`, passing a fixed width to `buffer.set_size` forces artificial line boundaries even with `Wrap::None`; passing `None` ensures full natural line layout and enables true horizontal overflow.
  - Tables compute generous minimum column widths (`clamp(100.0, 420.0)`) with subtle vertical dividers between columns.
  - Overflowing components display a dedicated 3px scrollbar track and thumb to visually communicate scroll position.
