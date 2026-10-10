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
Tenebra is built on one rule: **there is only one light in the room**. Inspired by Caravaggio's tenebrism and the brass picture-light bar mounted above museum paintings, every viewport has a single fixed light origin, off-canvas at 10 o'clock (-12%, -10%). Nothing is lit globally. Everything is struck, grazed, or left in darkness. Where Art Deco Lumina was symmetrical architecture, Tenebra is asymmetric theater: roughly 70% of any screen should read as near-black, with light rationed like it's expensive. The emotional register is a private viewing room after hours, one fixture, one object, total silence.

Key visual principles:
- **Tenebrism over decoration:** Darkness is the default material. Light is the ornament.
- **The Terminator:** Every lit element has a razor-sharp boundary where illumination ends, like the moon's day-night line. No feathered transitions on component edges.
- **Catalog Precision:** All metadata, indices, and labels are set in spaced monospace, like accession numbers stenciled beside a painting.

## Soul of the Design
**Tenebra's soul is restraint as reverence**, the belief that attention is sacred, and light should never be given away cheaply.

The Core Idea:
Most design systems ask: *"How do we make everything visible?"* Tenebra asks the opposite: *"What deserves to be seen?"* Darkness isn't a backdrop here, it's the subject, and light is the only protagonist. One fixture, one beam, one moment. The interface doesn't perform for the user; the user is granted an audience with it.

What It Believes:
- **Scarcity creates meaning.** When 70% of the screen lives in shadow, a single lit element carries the weight of a candle in a cathedral. Light is spent the way a curator spends wall space, reluctantly, and only on what matters.
- **Darkness is atmosphere, not absence.** The warm umber blacks are aged varnish, museum walls, the quiet between movements. Empty space isn't wasted; it's where the eye rests.
- **Physics is more honest than decoration.** Shadows don't blur because real umbra doesn't blur. Objects farther from the light are dimmer because that's how light works. Every decorative shortcut is refused, because it would break the one truth the system protects: the light is real, and it's over there.

Its Emotional Signature:
Tenebra feels like someone unlocked a door for you after closing time. One painting, one light, and the sense that you're the only person in the world currently looking at it. It's not minimalism, stripping away for clarity. It's **curation**, stripping away for intimacy.

In one line: *an interface that doesn't want to be looked at, it wants to be witnessed.*

## Colors
The palette replaces Deco's neutral charcoal-and-brass with **warm umber blacks**, every dark token leans brown, like aged varnish on an old canvas.

- **Void Umber (#0D0A06):** The canvas. Not neutral, warm, organic, museum-wall dark.
- **Flame Core (#FFC670):** The primary. The hottest point of a candle flame. Reserved for the single most important action per view, rationing it is what makes it feel expensive.
- **Verdigris (#A7C0AE):** The secondary. The patina on old bronze fixtures. Used for success states, secondary data, and quiet interactive affordances.
- **Venetian Red (#D98E73):** The tertiary. Oxide pigment straight from a Renaissance palette. Sparingly used for emphasis and editorial accents.
- **Lumen Tiers (lumen-1 → lumen-5):** A five-step darkness scale replacing generic surface elevation. Surfaces are assigned a tier by their *distance from the light origin*, not by stacking order.

## Typography
**Cormorant Garamond** is the voice, an extreme-contrast serif with hairline strokes that behave like light itself: thick strokes catch the beam, thin strokes vanish into shadow. Use it at large sizes only; its hairlines dissolve below 24px on dark backgrounds. **Archivo** handles body copy with a neutral grotesque that stays out of the way. **IBM Plex Mono** is the signature move: all labels, buttons, tags, and metadata use `label-catalog` at 0.22em tracking, evoking stenciled museum catalog plates. Editorial numerals are old-style; data numerals are always tabular mono.

## Layout & Spacing
The grid abandons Deco's symmetry for **the hanging wall**. Content hangs in the upper-left two-thirds of the viewport, nearest the light; the bottom-right third is intentionally sparse, that's the deep field, and emptiness there is a feature, not a bug.

- **Falloff rhythm:** Vertical spacing between sections grows by `falloff-step` (16px) as you move down the page, 32px near the top, 48px, then 64px, physically expressing distance from the fixture.
- **Beam offset:** All hard shadows offset 14px down-right, aligned to the 62° ray. Never deviate; the light never moves.
- **Mobile:** The light origin stays proportionally fixed. Collapse to a single column, but preserve the growing falloff rhythm, it's the brand's signature cadence.

## Elevation & Depth
Elevation is measured in **lumens, not layers**. This is the system's most radical departure: instead of surfaces brightening as they rise, surfaces are assigned a lumen tier by proximity to the light origin, and modals don't float *up*, they step *into the light*.

- **Hard umbra only:** Shadows are zero-blur, 100% hard-edged, colored `terminator`, offset exactly `beam-offset`. Soft shadows are forbidden, they imply ambient light, which doesn't exist here.
- **The Plinth:** Cards rest on a solid 4px `lumen-5` slab offset down-right, like an object on a gallery plinth. The gap between card and plinth is the terminator.
- **Modals:** The backdrop doesn't fade to uniform black, it dims the page to lumen-5 *except* a cone-shaped gradient (11° spread) from the light origin, inside which the modal sits. The user literally watches the fixture swing to the new subject.
- **Glow rule:** Only the focused element may glow, and only from its top-left corner, a directional bloom, never a uniform halo.

## Shapes
The shape language is **The Orb and The Blade**, perfect circles against absolute straight lines, nothing in between. This directly opposes Deco's beveled rectangles.

- **Orbs:** Light sources, seals, avatars, and focal markers are perfect circles. A circle always represents something that *emits* or *reflects* light.
- **Blades:** Panels and containers are 0px-radius rectangles, but with exactly one clipped corner: bottom-right, 12px, 45°. The corner where the shadow escapes. Never clip any other corner.
- **Slatted light:** Section transitions and image reveals use 3–5 horizontal bands of `on-surface` at 4–8% opacity with 2px gaps, light leaking through a louvre blind. This is the system's most recognizable decorative motif.

## Components
- **Buttons:** Primary is the "lit switch", solid `primary` fill with a vertical gradient (Flame Core → `primary-container`) so it reads hottest at top-left, plus the hard umbra. Secondary is unlit: transparent with a 1px `outline-variant` border. On hover, primary buttons don't brighten uniformly, the bloom expands from the top-left corner outward, like the fixture warming up.
- **Inputs, The Aperture:** Resting state is a single 1px `outline-variant` blade along the bottom edge only. On focus, the left end of the blade ignites in Flame Core and travels rightward over 250ms, like an iris opening. Labels always use `label-catalog`, positioned above, prefixed by an index number (01, 02…).
- **Cards, The Lit Object:** A radial gradient emanates from the global light origin (not the card's own corner, cards near screen-right are dimmer than cards near screen-left, by design). Each card carries a `lumen-1` top edge that fades to nothing by its midpoint. Title set in Cormorant; accession number in catalog mono at top-right.
- **Chips, Wax Seals:** Perfect circles or pill-free circles with a 1px embossed inner ring (`outline` at 40%). Selected state fills with Venetian Red, not the primary, a curatorial stamp, not a light.
- **Lists:** No rule lines. Rows are separated by **shadow gaps**, 1px `terminator` lines that are only visible in their leftmost 25%, dissolving into darkness rightward. On hover, a single slatted-light band sweeps down the row.
- **Iconography, Eclipse strokes:** 1.5px strokes drawn only along the top-left 180° arc of each glyph; the shadow-side arc is open. Icons look half-eclipsed. No fills, no rounded caps, and any circular icon element must break at the 5 o'clock position.
- **Ambient detail, Dust motes:** Optional 1px particles at 2–6% opacity, drifting slowly down-right (with the light, never against it), visible only inside lit regions. Cap at ~20 per viewport; they should be felt, not seen.
