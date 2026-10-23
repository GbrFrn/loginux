---
version: 1
slug: "src-ui-rs"
primary_target: "src/ui.rs"
related_targets: []
---

# Loginux editor window

Scope: the native egui window (`loginux ui`). Visitor mode: Operate. Audience: Linux owners of the G316 X 98 first (the G903 HERO's logo is built in too); jobs are set-once lighting and guided channel checks. Constraints: the window never opens the keyboard itself; it talks to the engine over the local socket. Live preview while painting. Proof shows in the channel check, not while editing.

## Direction contract

THESIS: One keyboard set as a bitmap type specimen on black: keys, bar segments, channel numbers and headings in a single whole-pixel system, echoing the board's own dot screen. It refuses the RGB-suite default of a charcoal window, product render, colour wheel and glow.

OWN-WORLD: Ground #0B0B0C, panels #151517, 2pt rules #2A2A2E, emphasis rules and text #EDEDED, secondary text #A9A9AE. Three signals that never trade roles: red #FF2E3F for selection, the active tab, focus, the RUN AT LOGIN switch when on and the primary action; green #3FD07A for connected and observed; amber #F2B84B for warnings and differing proof. Silkscreen bitmap caps for headings, tabs, labels and numbers, in cells of 1, 1.5, 2, 4 or 6pt snapped to whole device pixels; Archivo for body and readouts. A dot-matrix patch is the house texture. Square corners. LED colours are flat fills of the exact RGB sent, with no glow or gradient.

STORY: The user sees at once which profile the keyboard shows and whether Loginux holds the lighting. They click keys, drag the picker, watch the board change live, save, then pick USE THIS PROFILE, switch RUN AT LOGIN on, or RELEASE LIGHTING. Proof lives in the channel check, opened from SETTINGS, which lights one zone at a time and records what they saw.

FIRST VIEWPORT: A 64pt header with the LOGINUX wordmark, EDIT and SETTINGS tabs (red underline on the active one; the channel check opens from SETTINGS), the dot patch, and the RUN AT LOGIN switch at the right. Below it, three columns. Left, 236pt: profiles in ruled boxes with colour strips, then connection status and RELEASE LIGHTING at the bottom. Centre: the profile name in Title caps (30pt cap height), its editing state (LIVE, UNSAVED), the LIGHTING row, and the keyboard on its black body at full width. Right, 316pt: the specimen column with the selection name, the hex in Hex caps (20pt cap height), the swatch at three sizes, the picker with its value rows, the colour list, and SAVE. On a fresh install the first built-in preset, Breathing, opens: the board is dark, the right column shows the effect's settings, and the save button reads SAVE AS NEW.

FORM: Bitmap Specimen, dark. This is the user's steer of the Emigre bitmap-specimen world, position 1 of the safer lineup and pinned by the user. seed:45ddbf08ecb486ea

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
