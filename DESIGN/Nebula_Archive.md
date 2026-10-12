---
name: Nebula Archive
colors:
  surface: '#16130c'
  surface-dim: '#16130c'
  surface-bright: '#3d3930'
  surface-container-lowest: '#110e07'
  surface-container-low: '#1f1b14'
  surface-container: '#231f18'
  surface-container-high: '#2d2a22'
  surface-container-highest: '#38342c'
  on-surface: '#eae1d5'
  on-surface-variant: '#d1c5b1'
  inverse-surface: '#eae1d5'
  inverse-on-surface: '#343028'
  outline: '#99907d'
  outline-variant: '#4d4636'
  surface-tint: '#eac259'
  primary: '#ffe5ad'
  on-primary: '#3e2e00'
  primary-container: '#f0c75e'
  on-primary-container: '#6c5200'
  inverse-primary: '#765a00'
  secondary: '#cebdff'
  on-secondary: '#381286'
  secondary-container: '#51339f'
  on-secondary-container: '#c1acff'
  tertiary: '#d4ebff'
  on-tertiary: '#00344e'
  tertiary-container: '#98d2ff'
  on-tertiary-container: '#155b82'
  error: '#ffb4ab'
  on-error: '#690005'
  error-container: '#93000a'
  on-error-container: '#ffdad6'
  primary-fixed: '#ffdf95'
  primary-fixed-dim: '#eac259'
  on-primary-fixed: '#251a00'
  on-primary-fixed-variant: '#594400'
  secondary-fixed: '#e8ddff'
  secondary-fixed-dim: '#cebdff'
  on-secondary-fixed: '#21005e'
  on-secondary-fixed-variant: '#4f319d'
  tertiary-fixed: '#cae6ff'
  tertiary-fixed-dim: '#93cdfa'
  on-tertiary-fixed: '#001e2f'
  on-tertiary-fixed-variant: '#004b6f'
  background: '#16130c'
  on-background: '#eae1d5'
  surface-variant: '#38342c'
typography:
  headline-display:
    fontFamily: DM Serif Display
    fontSize: 64px
    fontWeight: '400'
    lineHeight: '1.1'
    letterSpacing: -0.02em
  headline-lg:
    fontFamily: DM Serif Display
    fontSize: 48px
    fontWeight: '400'
    lineHeight: '1.2'
  headline-lg-mobile:
    fontFamily: DM Serif Display
    fontSize: 32px
    fontWeight: '400'
    lineHeight: '1.2'
  headline-md:
    fontFamily: DM Serif Display
    fontSize: 32px
    fontWeight: '400'
    lineHeight: '1.3'
  body-lg:
    fontFamily: Inter
    fontSize: 18px
    fontWeight: '400'
    lineHeight: '1.6'
  body-md:
    fontFamily: Inter
    fontSize: 16px
    fontWeight: '400'
    lineHeight: '1.6'
  label-sm:
    fontFamily: Inter
    fontSize: 12px
    fontWeight: '600'
    lineHeight: '1'
    letterSpacing: 0.1em
spacing:
  unit: 4px
  gutter: 24px
  margin-mobile: 16px
  margin-desktop: 64px
  container-max: 1440px
---

## Brand & Style
Cold, industrial deep-space surfaces with a historical-preservation register. Heavy whitespace, precision typography, atmospheric glows. Built for long-form reading and archival reference.

## Colors
The palette is a near-black base with Pulsar Gold and Nebula Violet accents.

- **Primary (Pulsar Gold #f0c75e):** Used for critical interactions and brand moments.
- **Secondary (Nebula Violet):** Used for highlights, data visualization, and tertiary navigational cues.
- **Surface (Obsidian Shard #16130c):** Used for containers, sidebars, and nested components.
- **Accent Glows:** Instead of standard shadows, components use 1px inner strokes or soft outer glows using low-opacity versions of Pulsar Gold and Nebula Violet to simulate light reflecting off surfaces in a void.

## Typography
The typography balances the traditional authority of a serif with the utilitarian precision of a sans-serif.

- **Headlines:** DM Serif Display for a classic library register. Large display sizes use tighter letter spacing.
- **Body:** Use Inter for all reading and input tasks. The line height is intentionally generous (1.6) to ensure legibility against the deep dark background.
- **Labels:** Small labels use Inter in uppercase with increased letter spacing to mimic technical data readouts found on spacecraft instrumentation.

## Layout & Spacing
The layout follows a **Fixed Grid** philosophy on desktop to create a sense of organized, archival structure, and a **Fluid Grid** on mobile.

- **The Column System:** A 12-column grid with 24px gutters. Elements should be aligned strictly to the grid to maintain the industrial, serious tone.
- **Rhythm:** All spacing (padding, margins) is derived from a 4px base unit. 
- **The "Vacuum" Principle:** 64px+ vertical breathing room between major sections.

## Elevation & Depth
In this design system, depth is communicated through **Tonal Layers** and **Luminescent Borders** rather than traditional shadows.

- **Level 0 (Deep Space):** The #05070a base background.
- **Level 1 (The Station):** Surfaces in #12151c with a subtle 1px border of #ffffff (10% opacity).
- **Interactive Focus:** When an element is active or elevated, it gains a 1px border of Pulsar Gold and a very soft 8px blur glow of the same color (opacity 20%). 
- **The "Glass" Effect:** For overlays (modals/drawers), use a backdrop blur of 12px with a 40% opaque Obsidian Shard fill to create a "viewing port" effect.

## Shapes
The shape language is strictly **Sharp (0px)**. This communicates a rugged, industrial, and uncompromising archival tone. There are no rounded corners in this design system. 

- **Buttons & Inputs:** Hard 90-degree angles.
- **Cards & Modals:** Sharp edges.
- **Dividers:** 1px solid lines using Nebula Violet at 20% opacity.

## Components
Consistent styling across the interface is achieved through architectural precision.

- **Buttons:** 
  - *Primary:* Pulsar Gold background, Black text, sharp corners.
  - *Secondary:* Transparent background, 1px Nebula Violet border, Nebula Violet text.
- **Input Fields:** Obsidian Shard background with a 1px bottom-border only (Pulsar Gold). Labels appear in uppercase Inter directly above the field.
- **Chips/Tags:** Small, sharp-edged boxes with a 1px border and Nebula Violet text. Used for "metadata" and "categorization."
- **Cards:** No shadow. Obsidian Shard background with a subtle Pulsar Gold top-border (2px) to denote "importance" or "archival status."
- **Scrollbars:** Custom slim, sharp scrollbars in Pulsar Gold to maintain the "instrumentation" feel.
- **Lists:** Items separated by 1px Nebula Violet lines. Hover states trigger a subtle Obsidian Shard background shift and a Pulsar Gold left-edge accent (4px width).