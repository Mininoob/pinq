# Pinq — Product Plan & Technical Blueprint

Pinq is a local-first desktop sticky-note and personal knowledge-management application.
Its defining interaction: a **sticky pad** docked to a screen edge that you *pull* notes
out of, and *throw* notes back into. Underneath the tactile surface is a full
object-based PKM system with types, properties, databases, queries, templates, and
peer-to-peer end-to-end-encrypted sync.

This spec is written for a design + engineering team to build from. No production code
exists yet; this is the blueprint.

## Document map (sections follow the required output order)

| File | Sections |
|---|---|
| [01-product-overview.md](01-product-overview.md) | 1. Executive summary · 2. Product principles · 3. Assumptions · 4. Target users & JTBD · 5. Key differentiators · 6. Complete feature inventory · 7. Phase scope (Prototype / MVP / V1 / Later) |
| [02-interactions.md](02-interactions.md) | 8. Detailed user journeys · 9. Sticky-pad interaction spec · 10. Throw-to-unpin gesture spec · 11. Desktop window & fullscreen behavior |
| [03-application.md](03-application.md) | 12. Main-app information architecture · 13. Screen & window inventory · 14. Text wireframes |
| [04-data-model.md](04-data-model.md) | 15. Object & database model · 16. Example schema definitions · 17. Templates, queries, collections, views, widgets |
| [05-storage-sync.md](05-storage-sync.md) | 18. Local storage architecture · 19. P2P synchronization architecture · 20. Conflict-resolution strategy · 21. Encryption & device-pairing model |
| [06-media-and-stack.md](06-media-and-stack.md) | 22. Image/video/audio/link/file architecture · 23. Recommended technology stack · 24. Windows/macOS/Linux feasibility · 25. Recommended first OS · 26. API & module boundaries |
| [07-quality.md](07-quality.md) | 27. Security threat model · 28. Accessibility plan · 29. Performance targets · 30. Failure states & recovery · 31. Analytics & privacy-respecting metrics · 32. Testing strategy |
| [08-delivery.md](08-delivery.md) | 33. Release & migration strategy · 34. Major risks & mitigations · 35. Prioritized backlog · 36. MVP acceptance criteria · 37. Open product decisions · 38. Next-step implementation sequence + ten prototype engineering tasks |

## Headline decisions (justified in the body)

- **Stack:** Tauri 2.x — Rust core ("pinqd" engine as an in-process crate), TypeScript + Solid frontend, SQLite (WAL) + FTS5, content-addressed blob store (BLAKE3), Yjs-compatible CRDT (`yrs`) for block text, custom op-log + version vectors for object metadata.
- **First OS:** Windows 10/11 first, macOS second, Linux (X11 before Wayland) third.
- **Sync:** true device-to-device sync over LAN (mDNS + QUIC), optional end-to-end-encrypted relay/backup nodes (self-hostable), user-held keys, mnemonic recovery phrase. No server required for local use.
- **Anytype is functional inspiration only** — original naming, visuals, templates, and wording throughout.
