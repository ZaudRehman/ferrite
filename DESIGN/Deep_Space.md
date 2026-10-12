---
name: Deep Space
colors:
  surface: '#0c1324'
  surface-dim: '#0c1324'
  surface-bright: '#33394c'
  surface-container-lowest: '#070d1f'
  surface-container-low: '#151b2d'
  surface-container: '#191f31'
  surface-container-high: '#23293c'
  surface-container-highest: '#2e3447'
  on-surface: '#dce1fb'
  on-surface-variant: '#c5c6cb'
  inverse-surface: '#dce1fb'
  inverse-on-surface: '#2a3043'
  outline: '#8e9195'
  outline-variant: '#44474a'
  surface-tint: '#c1c7cf'
  primary: '#ffffff'
  on-primary: '#2b3137'
  primary-container: '#dde3eb'
  on-primary-container: '#5f656c'
  inverse-primary: '#595f66'
  secondary: '#b7c8e1'
  on-secondary: '#213145'
  secondary-container: '#3a4a5f'
  on-secondary-container: '#a9bad3'
  tertiary: '#ffffff'
  on-tertiary: '#283044'
  tertiary-container: '#dae2fd'
  on-tertiary-container: '#5c647a'
  error: '#ffb4ab'
  on-error: '#690005'
  error-container: '#93000a'
  on-error-container: '#ffdad6'
  primary-fixed: '#dde3eb'
  primary-fixed-dim: '#c1c7cf'
  on-primary-fixed: '#161c22'
  on-primary-fixed-variant: '#41474e'
  secondary-fixed: '#d3e4fe'
  secondary-fixed-dim: '#b7c8e1'
  on-secondary-fixed: '#0b1c30'
  on-secondary-fixed-variant: '#38485d'
  tertiary-fixed: '#dae2fd'
  tertiary-fixed-dim: '#bec6e0'
  on-tertiary-fixed: '#131b2e'
  on-tertiary-fixed-variant: '#3f465c'
  background: '#0c1324'
  on-background: '#dce1fb'
  surface-variant: '#2e3447'
typography:
  display-lg:
    fontFamily: Space Grotesk
    fontSize: 64px
    fontWeight: '700'
    lineHeight: '1.1'
    letterSpacing: -0.02em
  display-lg-mobile:
    fontFamily: Space Grotesk
    fontSize: 40px
    fontWeight: '700'
    lineHeight: '1.2'
    letterSpacing: -0.01em
  headline-md:
    fontFamily: Space Grotesk
    fontSize: 32px
    fontWeight: '500'
    lineHeight: '1.3'
  headline-sm:
    fontFamily: Space Grotesk
    fontSize: 24px
    fontWeight: '500'
    lineHeight: '1.4'
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
  label-md:
    fontFamily: Geist
    fontSize: 14px
    fontWeight: '500'
    lineHeight: '1.2'
    letterSpacing: 0.05em
  label-sm:
    fontFamily: Geist
    fontSize: 12px
    fontWeight: '500'
    lineHeight: '1.2'
    letterSpacing: 0.08em
rounded:
  sm: 0.125rem
  DEFAULT: 0.25rem
  md: 0.375rem
  lg: 0.5rem
  xl: 0.75rem
  full: 9999px
spacing:
  base: 8px
  container-max: 1440px
  gutter: 24px
  margin-desktop: 64px
  margin-mobile: 20px
  section-gap: 120px
---

## Brand & Style

Minimalist surfaces with subtle glassmorphism. Translucent layers over deep gradients, high-ratio whitespace, soft glows instead of hard transitions.

## Colors

The palette is anchored in a monochromatic "Midnight" spectrum to maintain a low-light, cinematic environment.

*   **Primary (Silver):** Used for high-contrast text and critical icons to ensure legibility against dark backgrounds.
*   **Secondary (Slate #e2e8f0):** Used for secondary text, borders, and inactive states.
*   **Neutral (Midnight Navy):** The core surface color. The background is nearly black (`#020617`), while containers use a slightly lighter navy (`#0F172A`).
*   **Accent Glow:** A subtle cyan-blue used sparingly for focus states, progress indicators, and "active" ethereal glows to simulate light reflecting in a vacuum.

## Typography

The typography strategy balances technical precision with high-end editorial flair.

*   **Headlines:** *Space Grotesk* provides a geometric, futuristic feel. For large display sizes, tight letter spacing and heavy weights create a commanding, cinematic presence.
*   **Body:** *Inter* is used for its exceptional readability in dark mode environments, preventing "halving" (text blurring) on high-resolution displays.
*   **Labels/Technical Data:** *Geist* is employed for UI small-print, button labels, and data points.

## Layout & Spacing

This design system utilizes a **Fluid Grid** with generous padding to evoke a sense of vastness. 

*   **The 8px Rule:** All spacing increments are multiples of 8px to ensure mathematical harmony.
*   **Section Gaps:** Vertical rhythm is intentionally slow. Use large gaps (80px to 120px) between major content sections to allow the eye to rest.
*   **Margins:** Desktop layouts should use wide outer margins (64px+) to keep content centered and focused, mimicking a widescreen cinematic frame. 
*   **Mobile Reflow:** On mobile, margins tighten to 20px, and multi-column layouts collapse into a single-column stack.

## Elevation & Depth

Depth is conveyed through **Tonal Layers** and **Backdrop Blurs** rather than traditional drop shadows.

1.  **Base (Level 0):** Deepest Navy (`#020617`).
2.  **Surface (Level 1):** Midnight Navy (`#0F172A`) with a subtle 1px inner border of Silver at 10% opacity.
3.  **Floating (Level 2):** Semi-transparent layers with a `backdrop-filter: blur(12px)`. These elements should have a soft "Ethereal Glow", a low-spread outer shadow using the Accent Glow color at 15% opacity.

Avoid heavy black shadows; instead, use light to define edges against the darkness.

## Shapes

The shape language is sophisticated and architectural. We use **Soft** rounding (4px - 12px) to prevent the UI from feeling too aggressive, yet maintain a precision-engineered look. 

*   **Standard Elements:** 4px radius (Inputs, Small Buttons).
*   **Containers/Cards:** 8px radius.
*   **Large Feature Cards:** 12px radius.

Avoid "Pill" shapes for buttons unless they are secondary floating actions; rectangular forms with slight rounding feel more grounded and professional in this context.

## Components

*   **Buttons:** Primary buttons use the Silver primary color with dark text. Secondary buttons are "Ghost" style with a 1px Silver border at 20% opacity. Hover states trigger a soft outer glow.
*   **Inputs:** Minimalist bottom-border only or a fully enclosed field with a subtle 1px border. Focus state is indicated by the Accent Glow color.
*   **Cards:** Use the "Surface" color with a very subtle gradient (top-left to bottom-right). No heavy shadows; use a 1px border to separate from the background.
*   **Chips:** Small, all-caps Geist text. Backgrounds should be nearly transparent with a stroke that matches the label color at low opacity.
*   **Lists:** High-contrast Silver text for primary items, Slate for descriptions. Separators should be dim (10% opacity Silver) and never full-width.
*   **Status Indicators:** Glowing dots with a "pulse" animation instead of solid red/green/yellow circles.