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
