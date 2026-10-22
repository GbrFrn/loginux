---
version: 1
slug: "src-ui-rs"
primary_target: "src/ui.rs"
related_targets: []
---

# Loginux editor window

Scope: the native egui window (`loginux ui`). Visitor mode: Operate. Audience: Linux owners of the G316 X 98; jobs are set-once lighting and guided channel checks. Constraints: the window never opens the keyboard itself; it talks to the engine over the local socket. Live preview while painting. Proof stays visible.

## Direction contract

THESIS: One keyboard set as a bitmap type specimen on black: keys, bar segments, channel numbers and headings in a single whole-pixel system, echoing the board's own dot screen. It refuses the RGB-suite default of a charcoal window, product render, colour wheel and glow.

OWN-WORLD: Ground #0B0B0C, panels #151517, 2 px rules #2A2A2E, emphasis rules and text #EDEDED, secondary text #A9A9AE. One red, #FF2E3F, only for selection, the active tab and the primary action; green #3FD07A only for connected and observed. Silkscreen bitmap caps for headings, tabs, labels and numbers at multiples of its 8 px grid; Archivo for body and readouts. A dot-matrix patch is the house texture. Square corners. LED colours are flat fills of the exact RGB sent, with no glow or gradient.

STORY: The user sees at once which profile the keyboard shows, whether Loginux holds the lighting, and what is proven. They click keys, drag the picker, watch the board change live, save, then pick Use at login or Release lighting. Channel check lights one zone at a time and records what they saw.

FIRST VIEWPORT: A 64 px header with the LOGINUX wordmark, EDIT and CHANNEL CHECK tabs (red underline on the active one), the dot patch, and USE AT LOGIN at the right. Below it, three columns. Left, 220 px: profiles in ruled boxes with colour strips, then connection status and RELEASE LIGHTING at the bottom. Centre: the profile name in 48 px bitmap caps, a proof line, and the keyboard on its black body at full width. Right, 300 px: the specimen column with the selection name, hex at 32 px, the swatch at three sizes, RGB rows, the picker and SAVE.

FORM: Bitmap Specimen, dark. This is the user's steer of the Emigre bitmap-specimen world, position 1 of the safer lineup and pinned by the user. seed:45ddbf08ecb486ea

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
