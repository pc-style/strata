# Strata dashboard

Technical search workspace for developers inspecting a local index. Primary action: run a query.
Adjectives: minimal, precise, quiet, inspectable. Essence: search, then inspect.

Choose the single-column concept over a permanent three-panel console: one index selector,
query controls and document rows. Dev mode reveals an inline inspector, not another page.
No metric cards, charts, decorative icons or automatic animations. Native disclosure controls
keep source documents and diagnostics optional. All controls work by keyboard.

## Tokens

IBM Plex Sans for interface text, monospace only for JSON. Base 16px, scale 1.2:
12, 14, 16, 19, 23, 28px. Spacing unit 4px; radius 4px; defined edges, no shadows.
Cool paper background #f5f7fa, white surface, graphite #263242, muted #526173,
border #c5cdd8, blue #245ca6 accent, white accent text, red #a62424 errors.
CSS uses corresponding OKLCH semantic tokens. Maximum content width 960px, left aligned.
Mobile wraps the toolbar and keeps editors/results within the viewport.

## Interaction

Search is the only filled button. Other actions use quiet outlined controls. Visible focus
rings, persistent labels, explicit busy/error/empty states, and no color-only statuses.
Dev statistics describe the search snapshot, not the latest committed writes. Round-trip
timing is browser-observed, not engine execution time. No telemetry or external assets.

## Audit

Initial direction: single workspace, one accent, no invented metrics. Rendered verification
must cover normal search, dev inspection, narrow width and empty/error states.
