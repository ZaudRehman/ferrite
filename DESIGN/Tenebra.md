---
name: Tenebra
colors:
  surface: '#0D0A06'
  surface-dim: '#090704'
  surface-bright: '#2A2114'
  surface-container-lowest: '#060402'
  surface-container-low: '#120E08'
  surface-container: '#171209'
  surface-container-high: '#201A0F'
  surface-container-highest: '#2B2214'
  on-surface: '#F0E7D5'
  on-surface-variant: '#CDBFA4'
  inverse-surface: '#F0E7D5'
  inverse-on-surface: '#33270F'
  outline: '#8F7C57'
  outline-variant: '#43381F'
  surface-tint: '#F4B762'
  primary: '#FFC670'
  on-primary: '#402500'
  primary-container: '#7A5312'
  on-primary-container: '#FFDEA8'
  inverse-primary: '#8A5E14'
  secondary: '#A7C0AE'
  on-secondary: '#11231A'
  secondary-container: '#2E3D33'
  on-secondary-container: '#C2DACA'
  tertiary: '#D98E73'
  on-tertiary: '#3D1508'
  tertiary-container: '#5C2817'
  on-tertiary-container: '#F4C3AB'
  error: '#FFB4AB'
  on-error: '#690005'
  error-container: '#93000A'
  on-error-container: '#FFDAD6'
  primary-fixed: '#FFDFA6'
  primary-fixed-dim: '#F4B762'
  on-primary-fixed: '#2A1700'
  on-primary-fixed-variant: '#5C3D05'
  secondary-fixed: '#DDEADC'
  secondary-fixed-dim: '#A7C0AE'
  on-secondary-fixed: '#0C1811'
  on-secondary-fixed-variant: '#2E3D33'
  tertiary-fixed: '#F4C3AB'
  tertiary-fixed-dim: '#D98E73'
  on-tertiary-fixed: '#220A02'
  on-tertiary-fixed-variant: '#5C2817'
  background: '#0D0A06'
  on-background: '#F0E7D5'
  surface-variant: '#2B2214'
  lumen-1: '#241C0F'
  lumen-2: '#1B150C'
  lumen-3: '#130E07'
  lumen-4: '#0C0905'
  lumen-5: '#070503'
  terminator: '#050301'
light:
  origin: { x: -12%, y: -10% }
  angle: 62deg
  falloff: inverse-square
  cone-spread: 11deg
typography:
  display-lg:
    fontFamily: Cormorant Garamond
    fontSize: 72px
    fontWeight: '500'
    lineHeight: '1.0'
    letterSpacing: -0.03em
  headline-lg:
    fontFamily: Cormorant Garamond
    fontSize: 44px
    fontWeight: '500'
    lineHeight: '1.05'
  headline-md:
    fontFamily: Cormorant Garamond
    fontSize: 34px
    fontWeight: '500'
    lineHeight: '1.15'
  headline-sm:
    fontFamily: Cormorant Garamond
    fontSize: 26px
    fontWeight: '500'
    lineHeight: '1.25'
  body-lg:
    fontFamily: Archivo
    fontSize: 18px
    fontWeight: '400'
    lineHeight: '1.65'
  body-md:
    fontFamily: Archivo
    fontSize: 16px
    fontWeight: '400'
    lineHeight: '1.65'
  body-sm:
    fontFamily: Archivo
    fontSize: 14px
    fontWeight: '400'
    lineHeight: '1.55'
  label-catalog:
    fontFamily: IBM Plex Mono
    fontSize: 11px
    fontWeight: '500'
    lineHeight: '1'
    letterSpacing: 0.22em
  headline-lg-mobile:
    fontFamily: Cormorant Garamond
    fontSize: 34px
    fontWeight: '500'
    lineHeight: '1.1'
spacing:
  unit: 4px
  gutter: 32px
  margin: 72px
  beam-offset: 14px
  falloff-step: 16px
---

## Brand & Style
One fixed light origin, off-canvas at 10 o'clock (-12%, -10%). Nothing is lit globally. Roughly 70% of any screen stays near-black; light is reserved for the single most important element per view.

Key visual principles:
- **Tenebrism over decoration:** Darkness is the default material. Light is the ornament.
- **The Terminator:** Every lit element has a razor-sharp boundary where illumination ends, like the moon's day-night line. No feathered transitions on component edges.
- **Catalog Precision:** All metadata, indices, and labels are set in spaced monospace, like accession numbers stenciled beside a painting.

## Colors
The palette uses **warm umber blacks**, every dark token leans brown.

- **Void Umber (#0D0A06):** The canvas. Warm, organic, near-black.
- **Flame Core (#FFC670):** The primary. Reserved for the single most important action per view.
- **Verdigris (#A7C0AE):** The secondary. Used for success states, secondary data, and quiet interactive affordances.
- **Venetian Red (#D98E73):** The tertiary. Used sparingly for emphasis and editorial accents.
- **Lumen Tiers (lumen-1 → lumen-5):** A five-step darkness scale replacing generic surface elevation. Surfaces are assigned a tier by their *distance from the light origin*, not by stacking order.

## Typography
**Cormorant Garamond** is the display voice, an extreme-contrast serif. Use it at large sizes only; its hairlines dissolve below 24px on dark backgrounds. **Archivo** handles body copy. **IBM Plex Mono** sets all labels, buttons, tags, and metadata at `label-catalog` 0.22em tracking. Editorial numerals are old-style; data numerals are always tabular mono.

## Layout & Spacing
The grid abandons symmetry. Content hangs in the upper-left two-thirds of the viewport, nearest the light; the bottom-right third stays intentionally sparse.

- **Falloff rhythm:** Vertical spacing between sections grows by `falloff-step` (16px) as you move down the page, 32px near the top, 48px, then 64px, physically expressing distance from the fixture.
- **Beam offset:** All hard shadows offset 14px down-right, aligned to the 62° ray. Never deviate; the light never moves.
- **Mobile:** The light origin stays proportionally fixed. Collapse to a single column, but preserve the growing falloff rhythm, it's the brand's signature cadence.

## Elevation & Depth
Elevation is measured in **lumens, not layers**. Surfaces are assigned a lumen tier by proximity to the light origin, and modals step *into the light* rather than floating *up*.

- **Hard umbra only:** Shadows are zero-blur, 100% hard-edged, colored `terminator`, offset exactly `beam-offset`. Soft shadows are forbidden, they imply ambient light, which doesn't exist here.
- **The Plinth:** Cards rest on a solid 4px `lumen-5` slab offset down-right, like an object on a gallery plinth. The gap between card and plinth is the terminator.
- **Modals:** The backdrop doesn't fade to uniform black, it dims the page to lumen-5 *except* a cone-shaped gradient (11° spread) from the light origin, inside which the modal sits. The user literally watches the fixture swing to the new subject.
- **Glow rule:** Only the focused element may glow, and only from its top-left corner, a directional bloom, never a uniform halo.

## Shapes
The shape language is **The Orb and The Blade**: perfect circles against absolute straight lines, nothing in between.

- **Orbs:** Light sources, seals, avatars, and focal markers are perfect circles. A circle always represents something that *emits* or *reflects* light.
- **Blades:** Panels and containers are 0px-radius rectangles, but with exactly one clipped corner: bottom-right, 12px, 45°. The corner where the shadow escapes. Never clip any other corner.
- **Slatted light:** Section transitions and image reveals use 3–5 horizontal bands of `on-surface` at 4–8% opacity with 2px gaps, light leaking through a louvre blind. This is the system's most recognizable decorative motif.

## Components
- **Buttons:** Primary is solid `primary` fill with a vertical gradient (Flame Core → `primary-container`), hottest at top-left, plus the hard umbra. Secondary is unlit: transparent with a 1px `outline-variant` border. On hover, the bloom expands from the top-left corner outward.
- **Inputs, The Aperture:** Resting state is a single 1px `outline-variant` blade along the bottom edge only. On focus, the left end of the blade ignites in Flame Core and travels rightward over 250ms. Labels always use `label-catalog`, positioned above, prefixed by an index number (01, 02…).
- **Cards, The Lit Object:** A radial gradient emanates from the global light origin (not the card's own corner, cards near screen-right are dimmer than cards near screen-left, by design). Each card carries a `lumen-1` top edge that fades to nothing by its midpoint. Title set in Cormorant; accession number in catalog mono at top-right.
- **Chips, Wax Seals:** Perfect circles or pill-free circles with a 1px embossed inner ring (`outline` at 40%). Selected state fills with Venetian Red, not the primary.
- **Lists:** No rule lines. Rows are separated by **shadow gaps**, 1px `terminator` lines that are only visible in their leftmost 25%, dissolving into darkness rightward. On hover, a single slatted-light band sweeps down the row.
- **Iconography, Eclipse strokes:** 1.5px strokes drawn only along the top-left 180° arc of each glyph; the shadow-side arc is open. Icons look half-eclipsed. No fills, no rounded caps, and any circular icon element must break at the 5 o'clock position.
- **Ambient detail, Dust motes:** Optional 1px particles at 2–6% opacity, drifting slowly down-right (with the light, never against it), visible only inside lit regions. Cap at ~20 per viewport; they should be felt, not seen.
