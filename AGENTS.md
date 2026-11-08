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
