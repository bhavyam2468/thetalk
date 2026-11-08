# UI architecture audit — 2026-11-08

## Confirmed causes and changes

| Area | What was wrong | Shared rule/change |
| --- | --- | --- |
| Geometry | `RectSpring::current()` was clipped to destination bounds, which discards the source body during contraction. | Current geometry comes only from spring state; retargeting preserves velocity. Covered by contraction/delayed-frame tests. |
| Layout | Message wrapping used animated width, feeding changing heights into its own geometry target. | Measure at destination width; clip content against the current body. |
| Content lifetime | Rendering switched routes immediately and discarded outgoing text before its container contracted. Expansion-ratio opacity is already 1 during contraction and cannot animate equal-size route changes. | `PanelTransition` and retained composited snapshots handle every route pair. Incoming text fades, outgoing text remains bottom-anchored and clipped while geometry moves. An interruption retains the currently displayed scene. |
| Route selection | Settings/commands/conversation priority was repeated in different places. | `AppState::panel()` owns route precedence; measurement and content painting consume that route. |
| Focus | Hovered input hitboxes controlled whether the entire layer could receive a click-to-focus grant. | Visible surfaces are OnDemand across the entire rounded card. Ownership comes exclusively from Wayland enter/leave events. |
| Summon | Setting OnDemand on an already mapped surface was treated as a request to take focus. | Explicit summons have a generation counter; the platform adapter performs one null-buffer + initial-commit remap handshake, then waits for configure before attaching. Startup maps OnDemand. No remap on leave/hover/deltas. |
| Keyboard editing | Clicking non-input chat space had no shared editor destination. | Chat clicks select the composer; settings keep their current editor unless a field is clicked. |
| Escape | Draft/history/stream state selected unrelated meanings: hide, cancel, clear, or release focus. | Escape always hides. Settings save on dismissal as before; chat and drafts survive. Stop remains a dedicated action/Ctrl+C. |
| Scheduling | Wayland handlers could paint before the main loop advanced animation state. | Handlers invalidate; the main loop advances clocks and presents with one outstanding compositor callback. |
| Timing | Elapsed-time caps made a delayed frame slow simulation time. | Closed-form springs and reveal pacing consume elapsed monotonic time. |
| IPC | A nonblocking listener still accepted blocking clients and waited up to 50 ms per read in the frame loop. | Accepted streams are nonblocking, partial lines retained, with bounded peer/byte/accept budgets and expiration. |
| Event backlog | LLM event drain was unbounded. | Process at most 128 queued events per tick before returning to frames/input. |
| Hit testing | The input region was rectangular despite transparent rounded corners. | Rounded scanline regions track current geometry, radius and presence offset. |
| Dead policy | `settings_reveal` was advanced but never rendered; `passive_unfocus` was serialized but never consulted. | Removed rather than treating these as real customization knobs. Old settings JSON remains readable (unknown fields ignored). Removed obsolete destination-clamp/reveal helpers. |
| Chip overflow | Chip width was capped but its text clip was still the entire panel. | Clip long model identifiers to the chip's own inner bounds. |
| Shadow | The Gaussian calculation integrated a rectangle regardless of radius and extended beyond bottom padding. | Rounded Gaussian slices and padding-compatible blur/offset. Profile cost on local HiDPI hardware. |

## niri research

Reviewed upstream source via GitHub at `90d5c1bf7381a81c953cb8b3f26c5ddcaa3f48cb`, not an assumption about generic Wayland:

- https://github.com/YaLTeR/niri/blob/main/src/handlers/layer_shell.rs
  (`was_unmapped && on_demand` initial focus, and null-buffer unmapping/initial configure).
- https://github.com/YaLTeR/niri/blob/main/src/input/mod.rs
  (pointer button press calls `focus_layer_surface_if_on_demand`).
- https://github.com/YaLTeR/niri/blob/main/src/niri.rs
  (`focus_layer_surface_if_on_demand` grants the clicked OnDemand layer; clicking elsewhere clears it).

OnDemand **eligibility** does not mean Exclusive ownership. Hover does not call
our summon path. Live verification must still cover the user's installed niri
version, focus-follows-mouse, multiple outputs, repeated remaps, and panels from
DankMaterialShell. Other compositors may implement initial OnDemand focus
differently; do not claim portable programmatic focus guarantees.

## Boundaries retained, not rewritten

Inspected module boundaries across animation, blur, configuration, IPC, LLM,
Markdown, streaming/healing, rendering, state, CLI and Wayland. This is not a
claim that all ~8,000 lines have been proven or that the project is now fully
modular. Parser, network and typography modules already separate meaningful
concerns; moving all their code in this patch would add risk without fixing
these interaction invariants.

Remaining debt / next measured work:

- `render.rs` and `state.rs` are still large. Layout measurement still happens in
  rendering and geometry can receive its new measured target a frame later.
  A typed, cached measured-scene boundary should replace that coupling in a
  subsequent focused change, not be papered over with per-route delays.
- Clipboard reads, config saves and a few model-selection persistence operations
  still run synchronously. They can stall UI input; move them to a bounded task
  service with explicit editor/revision identity before promising zero stalls.
- Follow-up: unchanged message layouts are now retained independently of optical
  state; changing streamed messages still reparse/reshape. See
  `docs/RENDERING_RESEARCH.md` for caching, clipping, overflow and profiling changes.
- Retained panel snapshots use bounded surface-sized storage and reuse their
  byte allocation. They still copy pixels while visible; measure memory bandwidth
  at 2x/4x. Text must not be geometrically scaled during panel transitions.
- The remap handshake is compositor-specific integration behavior. Unit tests
  cannot replace real Wayland configure/focus verification.
- IPC remains a local text protocol; escaping literal backslashes/newlines is
  legacy and should become a versioned structured protocol if expanded.

## Verification contract

Test all 16 pairs of Compact / Commands / Conversation / Settings, both slow and
rapidly interrupted; include equal-size route changes and slash filtering.
Test Escape with empty/populated drafts and history, during streaming and in
settings. Launch/summon must take keyboard; click chat then type; click another
app then type continuously while streaming; hover back without clicking must
not reclaim keyboard. Rounded corner/shadow clicks must pass through. Hidden
IPC send must not acquire focus. Run Cargo tests, release build, self-test,
previews and AppImage self-test, then capture fresh live evidence.

## Markdown/rendering follow-up

Read `docs/RENDERING_RESEARCH.md` for the next audit. Explicit card clicks now
request focus through the shared summon path when no keyboard enter has been
delivered; the original assumption that compositor click eligibility alone was
sufficient did not hold in the user's test. This does not permit hover/stream
focus requests. Borders/inline decorations now share the chat viewport mask;
code/table components use shared horizontal overflow and retained layout.
