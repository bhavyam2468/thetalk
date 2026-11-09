# Agent requirements

**Write the code so that the evolution of this app feels modular.**

This is a product requirement, not a stylistic suggestion. Read
`AGENT_CONTEXT.md` and `docs/UI_ARCHITECTURE_AUDIT.md` before changing behavior.

## Core user intentions & product philosophy

The user's vision for `thetalk` is an uncompromisingly fast, fluid, and elegant Wayland AI overlay. Every interaction must meet these non-negotiable product standards:

1. **60–120+ FPS Fluidity Everywhere (Zero Animation Lag)**:
   - Route transitions (e.g., Palette <-> ChatScreen, resizing panels) must never stutter or feel like 20fps. Frame times must remain comfortably under 10ms (allowing 100+ FPS) even at 2x HiDPI.
   - Never run unoptimized, full-screen pixel operations (such as un-cached CPU blurs or multi-megabyte frame buffer copies) on animated paths. Use techniques like 9-slice cached shadow templates and zero-allocation blits.

2. **Weightless, Kinetic Scrolling (No "Heavy Rock at 10fps")**:
   - Scrolling through conversations must feel natural, smooth, and kinetic—inspired by modern smooth-scrolling engines (Lenis, GSAP).
   - Wheel detents must carry adequate momentum (112px impulse) and decay through responsive, critically-damped springs (`SpringSpec::SCROLL`: `stiffness: 240, damping: 31`).
   - Touchpad continuous scrolls must never hard-snap dead on finger release; they must preserve momentum and glide gracefully.
   - Horizontal overflow (tables, code blocks) must be intuitive: hovering directly over wide blocks allows mouse wheel scrolling horizontally without forcing the user to hold Shift.

3. **First-Class Reasoning & Thinking Experience**:
   - Thinking blocks (`<think>`) are integral to modern LLM interaction and must be polished with care.
   - Expansion and contraction must be a smooth accordion animation using spatial springs (`SpringSpec::DEFAULT_SPATIAL`), never an abrupt instantaneous snap.
   - Streaming tokens inside thinking blocks must feature a subtle, gentle blur-to-focus and opacity reveal animation. It should not be as intense as the primary answer text, but soft, refined, and visually alive.
   - Visual containment is strict: vertical indicator lines, reasoning tokens, and quotes must never bleed or overflow into the bottom composer input bar.

4. **Robust Markdown Layout & Aesthetics**:
   - Code blocks must never wrap artificially (`Wrap::None` with unbounded layout width) so code formatting remains intact.
   - Markdown tables must have readable minimum column widths, crisp column dividers, and clear scroll indicators (track + thumb) when overflowing.

5. **Modern, Grounded Rust Engineering**:
   - Write idiomatic, modern, high-performance Rust. Research upstream crates, avoid legacy/deprecated patterns, profile before and after changes, and eliminate unnecessary memory allocations, bounds checks, or CPU bottlenecks.
   - Verify every change with automated tests (`cargo test`), renderer benchmarks (`benchmark-render.sh`), and live compositor tests on Wayland/niri (`verify_on_niri.sh`).

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
