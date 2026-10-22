---
name: Loginux
description: Per-key RGB for Logitech HID++ keyboards on Linux, set as a bitmap type specimen on black.
colors:
  ground: "#0B0B0C"
  panel: "#151517"
  raised: "#1F1F23"
  rule: "#2A2A2E"
  text: "#EDEDED"
  text-secondary: "#A9A9AE"
  label: "#8E8E94"
  disabled: "#4A4A50"
  dots: "#6E6E73"
  board-body: "#000000"
  dark-key: "#1A1A1D"
  signal-red: "#FF2E3F"
  signal-red-hot: "#FF5765"
  observed-green: "#3FD07A"
  caution-amber: "#F2B84B"
typography:
  title:
    fontFamily: "Silkscreen Bold, rasterized to cells (src/pixelfont.rs)"
    fontSize: "30pt cap (6pt cell x 5 rows)"
    lineHeight: 1
  brand:
    fontFamily: "Silkscreen Bold, rasterized to cells"
    fontSize: "20pt cap (4pt cell x 5 rows)"
    lineHeight: 1
  hex:
    fontFamily: "Silkscreen Bold, rasterized to cells"
    fontSize: "20pt cap (4pt cell x 5 rows)"
    lineHeight: 1
  tab:
    fontFamily: "Silkscreen Regular, rasterized to cells"
    fontSize: "10pt cap (2pt cell x 5 rows)"
    lineHeight: 1
  label:
    fontFamily: "Silkscreen Bold, rasterized to cells"
    fontSize: "7.5pt cap (1.5pt cell x 5 rows)"
    lineHeight: 1
  small:
    fontFamily: "Silkscreen Regular, rasterized to cells"
    fontSize: "7.5pt cap (1.5pt cell x 5 rows)"
    lineHeight: 1
  legend:
    fontFamily: "Silkscreen Regular, rasterized to cells"
    fontSize: "5pt cap (1pt cell x 5 rows)"
    lineHeight: 1
  heading:
    fontFamily: "Archivo"
    fontSize: "18pt"
    fontWeight: 600
    fontVariation: "'wght' 600"
  body:
    fontFamily: "Archivo"
    fontSize: "14pt"
    fontWeight: 400
    fontVariation: "'wght' 400"
  body-small:
    fontFamily: "Archivo"
    fontSize: "12pt to 13pt"
    fontWeight: 400
    fontVariation: "'wght' 400"
rounded:
  none: "0"
spacing:
  hairline: "1pt"
  rule: "2pt"
  xs: "4pt"
  sm: "8pt"
  md: "16pt"
  lg: "24pt"
  header: "64pt"
components:
  button-primary:
    backgroundColor: "{colors.signal-red}"
    textColor: "{colors.ground}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "9pt 12pt"
  button-primary-hover:
    backgroundColor: "{colors.signal-red-hot}"
    textColor: "{colors.ground}"
  button-outline:
    backgroundColor: "transparent"
    textColor: "{colors.text}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "9pt 12pt"
  button-outline-hover:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.text}"
  button-outline-active:
    backgroundColor: "{colors.rule}"
    textColor: "{colors.text}"
  button-quiet:
    backgroundColor: "transparent"
    textColor: "{colors.text-secondary}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "9pt 12pt"
  button-quiet-hover:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.text}"
  button-disabled:
    backgroundColor: "transparent"
    textColor: "{colors.disabled}"
    typography: "{typography.label}"
  profile-box:
    backgroundColor: "transparent"
    textColor: "{colors.text}"
    typography: "{typography.small}"
    rounded: "{rounded.none}"
    padding: "10pt"
    height: "48pt"
  profile-box-open:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.text}"
  tab:
    backgroundColor: "transparent"
    textColor: "{colors.label}"
    typography: "{typography.tab}"
  tab-active:
    backgroundColor: "transparent"
    textColor: "{colors.text}"
  field:
    backgroundColor: "{colors.board-body}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.none}"
    height: "26pt"
  key-unpainted:
    backgroundColor: "{colors.dark-key}"
    textColor: "{colors.text-secondary}"
    typography: "{typography.legend}"
    rounded: "{rounded.none}"
---

# Design System: Loginux

## Overview

**Creative North Star: "The Bitmap Specimen"**

The window is one keyboard set as a bitmap type specimen on black. Headings, tabs, labels, numbers and key legends all come from one pixel face, Silkscreen, pre-rasterized into cells and painted as flat rectangles. Every cell and every shape lands on whole device pixels, so the lettering echoes the keyboard's own dot screen and stays sharp at 100, 125, 150 and 200 percent display scale. Archivo carries only running prose and numeric readouts.

The density is that of a working instrument: three fixed columns under a 64pt header, ruled boxes, no ornament beyond the dot-matrix patch. Colour belongs to the keyboard. LED colours appear as flat fills of the exact RGB sent, while the chrome stays in near-black greys with three signal colours that each mean one thing. The interface is dark by commitment.

Rejected outright: the RGB-suite default of a charcoal window with a product render, a colour wheel and glow. Also rejected, by the user's standing rules: cream or off-white grounds, italic accent words, numbered section labels, monospace labels and pill buttons.

**Key Characteristics:**
- Bitmap caps for every heading, tab, label, number and key legend; Archivo for prose and readouts only.
- Whole-device-pixel geometry everywhere, with antialiasing (feathering) off.
- Square corners on every surface, control and key.
- LED colours as flat exact-RGB fills, never glowed, shaded or gradiented.
- Red, green and amber each reserved for one meaning.
- The dot-matrix patch as the single house texture.

## Colors

Near-black greys for structure, LED colour for content, and three signals that never trade roles.

### Primary
- **Signal Red** (signal-red): selection, the active tab underline, the primary action, keyboard focus rings, the text cursor and text-selection tint (at 45 percent). On a primary button the label is Ground-coloured.
- **Signal Red, Hot** (signal-red-hot): hover on the primary button only.

### Secondary
- **Observed Green** (observed-green): the connected status square and the proof mark for a zone whose observed position matched; "as expected" answers and a completed check count.

### Tertiary
- **Caution Amber** (caution-amber): warnings, notices, errors (egui's warn and error colours are both amber), input hints that reject a value, and the proof mark for a zone that lit somewhere other than expected.

### Neutral
- **Ground** (ground): the window, the header, the left and centre columns.
- **Panel** (panel): the right specimen column and the open profile's box.
- **Raised** (raised): hover fill on buttons, profile boxes and list rows.
- **Rule** (rule): 2pt rules, quiet-button and box edges, the board frame, the scroll-bar track, the pressed fill of outline buttons.
- **Text** (text): primary text, the 2pt header baseline rule, the outline button's edge, the open profile's edge, hover outline on keys.
- **Secondary Text** (text-secondary): supporting text, quiet-button labels, legends on unpainted keys.
- **Label Grey** (label): group labels such as PROFILES, SELECT and HEX, inactive tabs, meta lines, empty readouts.
- **Disabled** (disabled): text of a disabled control.
- **Dot Grey** (dots): the dot-matrix patch.
- **Board Black** (board-body): the keyboard body and text-field backgrounds.
- **Dark Key** (dark-key): a key or strip segment with no colour assigned.

### Named Rules
**The One Meaning Rule.** Red means selected, active, focused or primary; green means connected or observed; amber means warning or differing proof. No signal colour decorates, and no other hue enters the chrome.

**The Keyboard Owns Colour Rule.** Any hue outside the signals on screen is an LED colour the user chose, shown as a flat fill of its exact RGB. A user colour that happens to be red or green is still content, not a signal.

**The Quiet Disabled Rule.** A disabled control is Disabled-grey text with no edge and no fill, so it reads as absent rather than as a faint button.

## Typography

**Display Font:** Silkscreen Regular and Bold (SIL OFL), pre-rasterized into `src/pixelfont.rs` by `tools/mkpixelfont.py`, with a redrawn bold W
**Body Font:** Archivo variable (SIL OFL), at wght 400 and 600

**Character:** Silkscreen is a 5-row capital bitmap drawn cell by cell, so it reads as hardware type, the same voice as the keyboard's dot screen. Archivo is a plain grotesque that keeps sentences legible beside it. Silkscreen is set in caps only. Its one-cell side bearings are trimmed so ink sizes are exact.

### Hierarchy
- **Title** (Silkscreen Bold, 6pt cell, 30pt cap): the open profile's name and the CHANNEL CHECK heading in the centre column.
- **Brand** (Silkscreen Bold, 4pt cell, 20pt cap): the LOGINUX wordmark at the header's left.
- **Hex** (Silkscreen Bold, 4pt cell, 20pt cap): the selection's hex value, or NONE or MIXED, and the zone number during a check.
- **Tab** (Silkscreen Regular, 2pt cell, 10pt cap): EDIT and CHANNEL CHECK in the header.
- **Label** (Silkscreen Bold, 1.5pt cell, 7.5pt cap): button labels and control-group labels.
- **Small** (Silkscreen Regular, 1.5pt cell, 7.5pt cap): profile names, status words, meta and proof lines, research-zone numbers.
- **Legend** (Silkscreen Regular, 1pt cell, 5pt cap): key legends, inset 4pt from the key's top-left.
- **Heading** (Archivo 600, 18pt): egui heading style.
- **Body** (Archivo 400, 14pt): instructions and empty-state prose; 12 to 13pt for status detail, hints and the colour list.

### Named Rules
**The Whole Cell Rule.** A bitmap cell is its point size times the display scale, rounded to whole device pixels with a +0.01 bias (so 1.4999 rounds like 1.5), and never smaller than one device pixel. Bitmap text is never scaled, smoothed or drawn as an outline font.

**The No Monospace Rule.** egui's monospace family is mapped to Archivo. Labels are bitmap caps or Archivo, never a monospace face.

**The Whole Legend Rule.** A key shows its full label if it fits in the key's width minus 8pt, else the definition's `short` legend, else nothing. Words are never cut.

## Layout

A fixed three-column instrument under a header. The window opens at 1360 x 840pt with a minimum of 1120 x 700pt; `LOGINUX_UI_ZOOM` (0.5 to 4) scales it on top of the display scale.

- **Header:** 64pt tall on Ground, closed by a 2pt Text-coloured rule. Wordmark 22pt from the left; tabs 36pt after it, 34pt apart, hit areas padded 8pt; the login-service button 22pt from the right; the dot patch (192 x 30pt) sits 22pt left of that button and drops out when the tabs need the room.
- **Left column:** 236pt wide with 16pt margins on Ground: profiles in 48pt ruled boxes, then a rule and the connection status block pinned to the bottom.
- **Centre column:** Ground with 24pt margins, scrolling vertically: title, meta line, proof key, then the board at the column's width (at most 1200pt, key unit at most 60pt), then selection and research controls.
- **Right column:** 316pt wide with 16pt margins on Panel: the specimen of the selection (label, hex, swatch at three sizes), picker, value rows, colour list, then save and use actions.
- **Column rules:** 2pt Rule-coloured verticals at both column edges, drawn on top of the panels.
- **Rhythm:** item spacing 8pt by default (7pt in the status block); added gaps step through 2, 4, 6, 10, 12, 14 and 18pt; the board group gets 18pt before and 14pt after.

### Named Rules
**The Snapped Geometry Rule.** Every rectangle passes through `snap()` and every line width through `line()`: edges and widths land on whole device pixels, lines at least one pixel wide. Feathering is off globally, so a non-snapped shape shows as a visible jag. Keep it that way.

## Elevation & Depth

Flat. There are no shadows, glows or gradients anywhere. Depth comes from tonal steps, Ground to Panel to Raised, and from 2pt rules and outlines. Hover lightens a surface to Raised; press darkens an outline button to Rule. The board's black body inside an outset Rule frame is the deepest plane.

### Named Rules
**The Flat Light Rule.** An LED colour is a flat fill of the exact RGB, with no glow, bloom, gradient or shading. The picker's saturation and value field (24 x 24 cells) and hue strip (48 cells) are built from flat snapped cells, not smooth gradients.

## Shapes

Square everywhere: corner radius 0 on windows, menus, widgets, buttons, boxes, fields and keys. Edges are 2pt rules (1pt for hairline marks: the hover outline on keys, swatch edges in the colour list, the drag marquee). Small square marks carry state: an 8pt square for connection status and IN USE, a 3pt square in a key's lower-right corner (below the segment on strips) for proof. The dot-matrix patch is 2pt dots on a 6pt pitch, both snapped.

## Components

### Buttons
Bitmap caps in a snapped rectangle, sized to the label plus 24 x 18pt.
- **Shape:** square (0), 2pt edge drawn inside.
- **Primary:** Signal Red fill, Ground label, no edge; hover Signal Red Hot. One per context: SAVE, RUN AT LOGIN, TAKE OVER, a confirm's yes.
- **Outline:** transparent with a Text edge and Text label; hover Raised, pressed Rule. For committing actions such as RELEASE LIGHTING, USE THIS PROFILE, CREATE, CHECK.
- **Quiet:** transparent with a Rule edge and Secondary Text label; hover Raised with Text label. For secondary and reversible actions: REVERT, DELETE, SKIP, selection presets.
- **Focus:** a 2pt Signal Red outline 3pt outside the button.
- **Disabled:** Disabled text, no edge, no fill, no click sense.

### Tabs
Tab-size bitmap caps in the header. Inactive tabs are Label Grey and turn Text on hover; the active tab is Text with a 4pt Signal Red underline resting on the header rule and overhanging the word by 2pt each side. Focus is a 2pt red outline.

### Profile Boxes
Full-width 48pt boxes with a 2pt Rule edge and 10pt inset. The name sits in Small caps at the top left, an optional mark (IN USE) at the bottom right, and a 5pt colour strip along the bottom shows every zone colour in proportion. The open profile gets a Panel fill and a Text edge; hover is Raised.

### Inputs / Fields
- **Style:** egui text fields and drag values on Board Black with a 2pt Rule edge, square, 26pt tall, Archivo text.
- **Hover / Active:** edge goes to Secondary Text on hover and Text when active; fill steps to Raised or Rule.
- **Cursor and selection:** 2pt Signal Red cursor; selected text tinted Signal Red at 45 percent with a 1pt Text stroke.
- **Scroll bars:** solid, 8pt wide, Rule track, Secondary Text handle (Text when hovered), no fade.

### Specimen Column
The selection shown as a type specimen: a Label line naming the selection (up to five names, else a zone count), the hex in Hex caps, then the colour as three flat swatches of full width x 72pt, half width x 34pt and one-fifth width x 14pt, split evenly when several colours are selected. With nothing selected, a 72pt dot-matrix patch and one line of instruction take the swatch's place.

### Keyboard Board (signature)
The board on a black body with 10pt padding, framed by a 2pt Rule outline set 8pt outside. Keys are inset by a snapped gap of a tenth of a unit (at least 2pt); strip segments touch, separated by a single pixel. Unpainted zones are Dark Key with Secondary Text legends; painted zones show the exact RGB, with a black or white legend picked by luminance. Selection is a 2pt Signal Red outline just outside the key; hover is a 1pt Text outline; the drag marquee is a 1pt red rectangle. Proof marks are 3pt squares, green for observed and amber for differs, explained by the proof key above the board (unmarked means layout data only).

### Status Block
An 8pt square (Observed Green when connected, Caution Amber for no engine, no access or errors, Label Grey otherwise) beside a Small-caps status word, then Archivo detail lines and the lighting action at the column's foot.

### Mark
A keycap with penguin eyes and beak, lit along its bottom edge: a 16 x 16 pixel grid in `icons/loginux.txt`, square-cornered like everything else. Ground keycap and pupils, Text eyes, a Caution Amber beak and a Signal Red edge row; outside the window those two hues are the mark's own colours, not signals. `tools/mkicon.py` draws the SVG and the PNGs at whole-number scales without smoothing; the window and tray icons are read from the same grid.

## Do's and Don'ts

### Do:
- **Do** draw every bitmap word with `px_text` at one of the seven sizes (cells of 1, 1.5, 2, 4 or 6pt) and snap every shape with `snap()`, `line()`, `fill()` and `outline()`.
- **Do** keep feathering off and keep the +0.01 rounding bias in `whole()`.
- **Do** show LED colour as a flat fill of the exact RGB the keyboard receives.
- **Do** reserve Signal Red for selection, the active tab, focus and the one primary action in view; Observed Green for connected and observed; Caution Amber for warnings and differing proof.
- **Do** render disabled controls as Disabled-grey text with no edge.
- **Do** keep proof states distinct: observed, differs, and unmarked layout data.
- **Do** fall back from full label to `short` legend to no legend on keys.

### Don't:
- **Don't** add glow, gradients, shadows or rounded corners to keys, swatches or controls.
- **Don't** use red, green or amber for decoration or for a meaning other than its own.
- **Don't** set bitmap text in mixed case, at a non-whole cell size, or through a vector font.
- **Don't** use cream or off-white grounds, italic accent words, numbered section labels, monospace labels or pill buttons.
- **Don't** add a product render, colour wheel or charcoal RGB-suite chrome.
