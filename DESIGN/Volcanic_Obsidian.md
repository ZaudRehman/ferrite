---
name: Volcanic Obsidian
colors:
  surface: '#131315'
  surface-dim: '#131315'
  surface-bright: '#39393b'
  surface-container-lowest: '#0e0e10'
  surface-container-low: '#1c1b1e'
  surface-container: '#201f22'
  surface-container-high: '#2a2a2c'
  surface-container-highest: '#353437'
  on-surface: '#e5e1e4'
  on-surface-variant: '#ddc1ae'
  inverse-surface: '#e5e1e4'
  inverse-on-surface: '#313032'
  outline: '#a48c7a'
  outline-variant: '#564334'
  surface-tint: '#ffb77d'
  primary: '#ffb77d'
  on-primary: '#4d2600'
  primary-container: '#ff8c00'
  on-primary-container: '#623200'
  inverse-primary: '#904d00'
  secondary: '#c8c6c8'
  on-secondary: '#303032'
  secondary-container: '#474649'
  on-secondary-container: '#b7b4b7'
  tertiary: '#85cfff'
  on-tertiary: '#00344c'
  tertiary-container: '#00b5fc'
  on-tertiary-container: '#004360'
  error: '#ffb4ab'
  on-error: '#690005'
  error-container: '#93000a'
  on-error-container: '#ffdad6'
  primary-fixed: '#ffdcc3'
  primary-fixed-dim: '#ffb77d'
  on-primary-fixed: '#2f1500'
  on-primary-fixed-variant: '#6e3900'
  secondary-fixed: '#e4e2e4'
  secondary-fixed-dim: '#c8c6c8'
  on-secondary-fixed: '#1b1b1d'
  on-secondary-fixed-variant: '#474649'
  tertiary-fixed: '#c7e7ff'
  tertiary-fixed-dim: '#85cfff'
  on-tertiary-fixed: '#001e2e'
  on-tertiary-fixed-variant: '#004c6c'
  background: '#131315'
  on-background: '#e5e1e4'
  surface-variant: '#353437'
typography:
  display-lg:
    fontFamily: Space Grotesk
    fontSize: 48px
    fontWeight: '700'
    lineHeight: '1.1'
    letterSpacing: -0.02em
  display-lg-mobile:
    fontFamily: Space Grotesk
    fontSize: 32px
    fontWeight: '700'
    lineHeight: '1.2'
    letterSpacing: -0.01em
  headline-md:
    fontFamily: Space Grotesk
    fontSize: 24px
    fontWeight: '600'
    lineHeight: '1.3'
  body-lg:
    fontFamily: Space Grotesk
    fontSize: 18px
    fontWeight: '400'
    lineHeight: '1.6'
  body-md:
    fontFamily: Space Grotesk
    fontSize: 16px
    fontWeight: '400'
    lineHeight: '1.5'
  label-caps:
    fontFamily: Space Grotesk
    fontSize: 12px
    fontWeight: '700'
    lineHeight: '1'
    letterSpacing: 0.1em
  code:
    fontFamily: Space Grotesk
    fontSize: 14px
    fontWeight: '500'
    lineHeight: '1.4'
spacing:
  unit: 4px
  gutter: 24px
  margin-mobile: 16px
  margin-desktop: 40px
  container-max-width: 1440px
---

## Brand & Style

High-contrast minimalism with technical-brutalist geometry. Near-black void, single amber accent, sharp edges, deliberate spacing. Built for dense technical work.

## Colors

This design system utilizes a restricted, high-impact palette to maintain focus and visual hierarchy.

- **Base Obsidian (#08080a):** Used for the primary application background to eliminate screen glare and maximize contrast.
- **Basalt Gray (#1a1a1c):** Used for surfaces, cards, and containers. It provides a subtle lift from the background while remaining firmly in the dark spectrum.
- **Liquid Amber (#ff8c00):** The sole catalyst for interaction. Used for primary buttons, active indicators, and critical alerts. In dark environments, this color should feel like it is emitting light.
- **Borders:** Use a slightly lighter gray (#2d2d30) for structural lines to ensure elements are defined without breaking the moody atmosphere.

## Typography

Space Grotesk is the structural backbone of the design system.

- **Headlines:** Use heavy weights with tighter letter spacing to create a sense of density and importance.
- **Body:** Standard reading text should remain airy with a 1.5x to 1.6x line height to ensure legibility against the dark background.
- **Labels:** Use uppercase for metadata, breadcrumbs, and small UI labels to introduce a rhythmic, systematic feel.
- **Alignment:** Stick to rigid left-alignment for all technical data and prose to maintain the "terminal" logic.

## Layout & Spacing

The layout philosophy follows a **Fixed-Fluid Hybrid** model. Content is housed within a 12-column grid on desktop with a maximum width of 1440px, while secondary sidebars and technical panels utilize fixed-width increments of 64px.

Spacing is strictly based on a 4px baseline grid. Use generous internal padding within Basalt containers (typically 24px or 32px) to prevent the high-contrast typography from feeling cramped. 

**Breakpoints:**
- **Mobile (<768px):** 4-column layout, 16px margins.
- **Tablet (768px - 1024px):** 8-column layout, 24px margins.
- **Desktop (>1024px):** 12-column layout, 40px margins.

## Elevation & Depth

In the absence of traditional shadows, depth is communicated through **Tonal Layering** and **Internal Glows**.

- **Level 0 (Background):** Obsidian (#08080a).
- **Level 1 (Cards/Panels):** Basalt Gray (#1a1a1c) with a 1px solid border (#2d2d30).
- **Level 2 (Popovers/Modals):** Basalt Gray (#1a1a1c) with a more prominent 1px border (#444) and a very subtle 10% opacity Liquid Amber outer glow to indicate "active" float state.
- **Active States:** Elements do not "lift" with shadows; instead, they gain a Liquid Amber left-border (2px) or an inner stroke to signal focus.

## Shapes

All primary containers, buttons, and input fields use a 2px corner radius. Modals never exceed 4px.

## Components

- **Buttons:** 
  - *Primary:* Solid Liquid Amber background with black text. No rounded corners beyond 2px. 
  - *Secondary:* Transparent background with a 1px Liquid Amber border and Liquid Amber text.
- **Input Fields:** Basalt Gray background, 1px border (#2d2d30). Upon focus, the border changes to Liquid Amber and a subtle 1px internal glow is applied.
- **Chips/Status:** Use rectangular tags. Success (Green), Warning (Amber), and Error (Red) should be used sparingly as small, 2px-tall underline accents rather than full-pill backgrounds.
- **Lists:** Technical data rows should be separated by 1px borders (#2d2d30). Hover states should change the entire row background to a slightly lighter gray (#252527).
- **Cards:** Basalt Gray background, no shadow. Content should be padded with 24px on all sides. Use "Label-Caps" typography for card headers.
- **Data Visualizations:** Use Liquid Amber as the primary data line. All grid lines in charts should be #1a1a1c to remain nearly invisible until inspected.