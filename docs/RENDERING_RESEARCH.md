# Rendering and input research — 2026-11-08

This audit addresses observed defects, not a claim of production certification.
Research included upstream APIs/changelogs, Wayland semantics, profiling advice,
and CPU/GPU alternatives. Search results were cross-checked against source where
possible. Dependency versions below are researched candidates, **not upgrades
silently applied to Cargo.toml**.

## Findings applied

1. **Clipping must be an invariant of every drawing path.** tiny-skia fill and
   stroke APIs accept an explicit mask. Our border strokes used `None` while
   text used viewport clips. A card-only mask was also insufficient: scrollable
   content must be intersected with the above-composer viewport. Nested inline
   decorations, cell glyphs, selections, code titles and hitboxes need their own
   intersection too. The compositor's input region is a separate concern.
   - https://github.com/linebender/tiny-skia/blob/v0.11.4/src/painter.rs
   - https://github.com/linebender/tiny-skia/blob/v0.11.4/src/mask.rs

2. **Cache layout, not time.** Cosmic Text recommends persistent font and glyph
   caches and buffers per widget. The old renderer recreated/parses/shapes every
   message each frame. The new bounded per-message `Rc<ShapedMessage>` cache
   invalidates on source, width, palette, frontier topology, copy feedback and
   reasoning-label state. Entrance, word focus, scrolling, and reasoning height
   evaluate at paint time. Rc fits the single-threaded renderer; Arc would add
   synchronization without shared-thread ownership. One streaming message
   replaces its previous cache entry rather than accumulating every revision.
   - https://github.com/pop-os/cosmic-text
   - https://nnethercote.github.io/perf-book/heap-allocations.html

3. **Pixel work scales with output area.** A 2x surface has four times the pixels;
   recomputing a 16-slice rounded shadow at full scale in every frame was a poor
   fit. Shadows now rasterize at logical resolution, bilinearly sample at output
   scale, and cache stationary geometry. Identical Gaussian slice endpoints are
   combined. Text/card edges remain native-resolution; optical word blur remains
   continuous Gaussian, not a CSS-like opacity replacement.
   Outgoing panel composition also now uses tiny-skia's optimized premultiplied
   pixmap pipeline rather than a scalar per-channel float/rounding loop. Optical
   glyph placement scratch is reused instead of allocating per frontier word.
   Nine-patch atlases remain a candidate if measured shadow cost is still high.
   - https://github.com/linebender/tiny-skia
   - https://github.com/orktes/jsui/pull/111 (comparison, not our benchmark)

4. **Wheel detents are not touchpad pixels.** Wayland defines continuous axis
   coordinates, optional discrete detents, and value120 for newer pointers.
   Our SCTK 0.19 path preferred absolute (often small) wheel movement and passed
   touchpad movement through a slow spring. Detents now map to a useful distance;
   continuous movement directly tracks the finger; the wheel spring settles
   faster. Horizontal gestures and Shift+wheel target a shared hovered Markdown
   overflow viewport without changing vertical transcript position.
   A future SCTK upgrade MUST handle value120, not assume discrete is populated.
   - https://wayland.app/protocols/wayland#wl_pointer:event:axis
   - https://wayland.app/protocols/wayland#wl_pointer:event:axis_value120
   - https://github.com/Smithay/client-toolkit/blob/v0.19.2/src/seat/pointer/mod.rs

5. **Streaming source mapping must survive syntax highlighting.** pulldown-cmark's
   offset iterator provides source ranges. Our code highlighter replaced those
   ranges with metadata=0, bypassing the optical pipeline. Code-local projected
   frontier offsets now split highlighted spans without discarding syntax colour
   or Unicode. Optical glyph groups also split when their colour changes.
   Oversized unbroken identifiers exceed the blur scratch budget gracefully:
   clipped opacity-only glyphs replace the old early-return that hid the text. Table
   cells already carry source metadata; row separators now use the row's focus
   opacity instead of appearing at full contrast immediately.
   - https://github.com/pulldown-cmark/pulldown-cmark

6. **Click eligibility is not a delivered focus grant.** Initial OnDemand mapping
   and click grants are niri behavior, but the user still reports blank chat clicks
   failing in their actual shell setup. An explicit card click now uses the same
   one-shot focus-request path as summon when keyboard ownership is absent. It
   does not create a hover, keypress or stream-triggered focus loop. Live testing
   must verify the fallback handshake instead of inferring success from a boolean.
   - https://github.com/niri-wm/niri/blob/main/src/handlers/layer_shell.rs
   - https://github.com/niri-wm/niri/blob/main/src/input/mod.rs

## Dependency/renderer alternatives reviewed

| Candidate | Relevant finding | Decision for this patch |
| --- | --- | --- |
| cosmic-text 0.19.0 | Upstream changelog includes lazy Buffer setters and large-line-count shaping improvements. Setter APIs differ from 0.14.2. | Keep the locked API while fixing repeated work ourselves. Adopt in a separate build/MSRV/layout regression migration; no assertion that an update alone fixes frame latency. |
| tiny-skia 0.12.0 | Includes small antialiased rectangle panic fix and PNG/API changes. | Record as a worthwhile migration. Do not hand-edit registry checksums/transitive lock resolution in a toolchain-less sandbox. |
| SCTK 0.21.1 | Dispatch macros, surface data and frame-callback data changed; newer pointer versions alter wheel event delivery. | Migrate with pointer/focus/HiDPI tests together, not a manifest-only bump. |
| Vello CPU / Hybrid / GPU | Native Rust alternatives with different rasterization and device requirements. | Do not replace the renderer to mask avoidable reparsing, clipping errors, and full-scale shadow work. Benchmark a separate adapter prototype if CPU budgets remain unmet. No Electron/GTK/WebView fallback. |
| Alternative allocators / SIMD | May help only after actual allocation/hot-path evidence. | No allocator/unsafe/global target-cpu switch. Preserve portable AppImage CPU compatibility. |

Primary release/architecture sources:
- https://github.com/pop-os/cosmic-text/blob/main/CHANGELOG.md
- https://github.com/linebender/tiny-skia/blob/main/CHANGELOG.md
- https://github.com/Smithay/client-toolkit/blob/master/CHANGELOG.md
- https://github.com/laveez/olsvr/pull/58 (migration example)
- https://linebender.org/blog/tmil-24/
- https://github.com/linebender/vello

## Reproducible measurements and stress coverage

Run `./scripts/benchmark-render.sh` on the actual user's hardware. It builds with
`--release --locked` and writes JSONL p50/p95/max CPU render timings for steady
content, interrupted route reversal, scrolling, thinking expansion, and streaming
at 1x/2x. Each workload includes 24 messages, wide tables/code and Unicode.
`--render-preview` now includes `05-wide-markdown-clipping.png`.

For profiles: `cargo build --locked --profile profiling`, then use
`samply record ./target/profiling/thetalk --benchmark-render` or
`perf record -g --call-graph dwarf ./target/profiling/thetalk --benchmark-render`.
The custom profile inherits release optimization but overrides strip so symbols
survive. Never weaken system security settings automatically to run a profiler.

- A 60 Hz frame has ~16.67 ms; 120 Hz has ~8.33 ms for the whole pipeline.
- These benchmark timings cover CPU tick/render, not Wayland copy/commit,
  compositor frame delivery, input-to-photon latency or competing desktop load.
- Do not invent speedup/FPS numbers or gate a shared CI runner on arbitrary timing.
- Compare repeated runs on the same host, with a warm/cold distinction. Inspect
  allocation profiles and render/vblank traces before claiming the 20fps feeling
  is eliminated. Test 4x output and memory growth locally as well.

## Remaining risks, explicitly not certified

- Very long active replies still reparse/reshape the changing message. The cache
  eliminates repeated work for unchanged messages, not incremental Markdown ASTs.
- The 128-entry layout cache is bounded by entry count, not exact heap bytes.
  Conversations exceeding cache capacity need measured viewport virtualization.
- Inline decorated buffers temporarily scope a mask; this still has a memory-copy
  cost. Profile many-link/table scenes before introducing a more complex clip stack.
- Full-content SSE snapshots/queued model events, synchronous clipboard reads and
  config persistence still merit bounded worker/service work. Consumer-side batches
  now apply only the latest snapshot (avoiding repeated same-frame full-text work),
  but transport snapshots and the event channel remain an allocation/backlog risk. They are not proven
  stall-free under slow filesystem/process conditions.
- Source projection for indented/normalized code deserves broader CommonMark corpus
  testing beyond fenced-code and Unicode regressions. A source offset is not always
  a one-to-one rendered-character offset after normalization.
- Horizontal scroll currently supports touchpad/tilt gestures and Shift+wheel,
  with a thin position indicator; it is not yet a draggable scrollbar.
- Initial/remap focus and tap-to-focus require real niri + DankMaterialShell tests.
  CI compilation/headless PNGs cannot establish them.
