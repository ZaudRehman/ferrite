---
name: Solar Silk
colors:
  surface: '#fef8f6'
  surface-dim: '#ded9d7'
  surface-bright: '#fef8f6'
  surface-container-lowest: '#ffffff'
  surface-container-low: '#f8f2f0'
  surface-container: '#f2edeb'
  surface-container-high: '#ece7e5'
  surface-container-highest: '#e7e1df'
  on-surface: '#1d1b1a'
  on-surface-variant: '#55433f'
  inverse-surface: '#32302f'
  inverse-on-surface: '#f5f0ee'
  outline: '#88726e'
  outline-variant: '#dbc1bc'
  surface-tint: '#974635'
  primary: '#843728'
  on-primary: '#ffffff'
  primary-container: '#a24e3d'
  on-primary-container: '#ffdfd9'
  inverse-primary: '#ffb4a5'
  secondary: '#605e5a'
  on-secondary: '#ffffff'
  secondary-container: '#e6e2dc'
  on-secondary-container: '#666460'
  tertiary: '#544e44'
  on-tertiary: '#ffffff'
  tertiary-container: '#6d665c'
  on-tertiary-container: '#efe5d8'
  error: '#ba1a1a'
  on-error: '#ffffff'
  error-container: '#ffdad6'
  on-error-container: '#93000a'
  primary-fixed: '#ffdad3'
  primary-fixed-dim: '#ffb4a5'
  on-primary-fixed: '#3e0400'
  on-primary-fixed-variant: '#792f21'
  secondary-fixed: '#e6e2dc'
  secondary-fixed-dim: '#c9c6c1'
  on-secondary-fixed: '#1c1c18'
  on-secondary-fixed-variant: '#484743'
  tertiary-fixed: '#ebe1d4'
  tertiary-fixed-dim: '#cfc5b9'
  on-tertiary-fixed: '#1f1b13'
  on-tertiary-fixed-variant: '#4c463c'
  background: '#fef8f6'
  on-background: '#1d1b1a'
  surface-variant: '#e7e1df'
typography:
  display-lg:
    fontFamily: Libre Caslon Text
    fontSize: 48px
    fontWeight: '400'
    lineHeight: 56px
    letterSpacing: -0.01em
  display-lg-mobile:
    fontFamily: Libre Caslon Text
    fontSize: 36px
    fontWeight: '400'
    lineHeight: 44px
    letterSpacing: -0.01em
  headline-md:
    fontFamily: Libre Caslon Text
    fontSize: 32px
    fontWeight: '400'
    lineHeight: 40px
  headline-sm:
    fontFamily: Libre Caslon Text
    fontSize: 24px
    fontWeight: '400'
    lineHeight: 32px
  body-lg:
    fontFamily: DM Sans
    fontSize: 18px
    fontWeight: '400'
    lineHeight: 28px
  body-md:
    fontFamily: DM Sans
    fontSize: 16px
    fontWeight: '400'
    lineHeight: 24px
  label-md:
    fontFamily: DM Sans
    fontSize: 14px
    fontWeight: '500'
    lineHeight: 20px
    letterSpacing: 0.05em
  label-sm:
    fontFamily: DM Sans
    fontSize: 12px
    fontWeight: '700'
    lineHeight: 16px
    letterSpacing: 0.03em
rounded:
  sm: 0.125rem
  DEFAULT: 0.25rem
  md: 0.375rem
  lg: 0.5rem
  xl: 0.75rem
  full: 9999px
spacing:
  unit: 8px
  container-max: 1280px
  gutter: 24px
  margin-mobile: 20px
  margin-desktop: 64px
---

## Brand & Style
Warm minimalism. Soft tonal layering and subtle light-source shadows over an off-white canvas. Generous whitespace, 8px base unit.

## Colors
The palette is inspired by natural materials, baked clay, raw silk, and charcoal. 

- **Primary (Terracotta):** Used for key actions, brand moments, and critical highlights. It provides a grounded energy against the lighter surfaces.
- **Secondary (Silk):** The primary surface color. An off-white that reduces eye strain and provides a premium, parchment-like feel.
- **Tertiary (Sand):** Used for secondary containers, subtle borders, and background depth.
- **Dusty Clay (#ded9e3):** Muted lavender-gray for secondary surfaces.
- **Neutral (Charcoal):** Reserved for typography and high-contrast iconography to ensure maximum legibility and a classic editorial look.

## Typography
The typography strategy pairings a literary, high-contrast serif for headings with a clean, understated geometric sans-serif for functional text.

- **Headlines:** Libre Caslon Text, used sparingly for titles and editorial callouts.
- **Body & Labels:** DM Sans for functional text.
- **Labels:** Small labels and buttons use increased letter spacing and medium weights.

## Layout & Spacing
The layout follows a **Fluid Grid** model with generous margins to reinforce the premium, airy brand personality. 

- **Grid:** Use a 12-column grid for desktop and a 4-column grid for mobile.
- **Rhythm:** Spacing is strictly based on an 8px base unit. Vertical rhythm should be expansive; prioritize whitespace over information density.
- **Breakpoints:**
  - Mobile: < 600px (Margins: 20px, Gutters: 16px)
  - Tablet: 600px - 1024px (Margins: 40px, Gutters: 24px)
  - Desktop: > 1024px (Margins: 64px, Max-width: 1280px)

## Elevation & Depth
Depth is communicated through **Tonal Layers** and soft, directional shadows that mimic natural sunlight.

- **Base Layer:** Secondary color (#FDF9F3).
- **Raised Surfaces:** Tertiary color (#E8DED1) with a soft 1px stroke (5% Charcoal) to define edges.
- **Shadows:** Use "Ambient Glow" shadows, low opacity (4-8%), large blur (20px-40px), with a slight warm tint to the shadow color rather than pure black. This creates an effect of objects resting gently on a soft surface.
- **Transitions:** All elevation changes ease in and out.

## Shapes
Shapes in this design system are "Soft." They avoid the playfulness of fully rounded corners while shunning the harshness of sharp 90-degree angles.

- **Primary Radius:** 0.25rem (4px) for small components like inputs and checkboxes.
- **Large Components:** Cards and modals use `rounded-lg` (8px) to feel substantial yet approachable.
- **Interactive Elements:** Buttons follow the Primary Radius to maintain a structured, professional look.

## Components
Consistent execution of components keeps the aesthetic cohesive.

- **Buttons:** 
  - *Primary:* Terracotta background, Silk text, no border.
  - *Secondary:* Transparent background, Charcoal border (1px), Charcoal text.
- **Inputs:** Soft Sand (#E8DED1) background with a bottom-only 1px border in Charcoal. This mimics premium stationery.
- **Cards:** Use the Secondary color for the surface. Apply a very subtle Sand border or the "Ambient Glow" shadow, never both.
- **Lists:** High-density lists are discouraged. Use generous padding (24px+) between list items and separate them with a hair-line stroke of Sand.
- **Chips:** Small, Pill-shaped (despite the general shape rule) with a Sand background and Charcoal text for a soft, organic feel.
- **Selection Controls:** Checkboxes and Radios use the Primary Terracotta color when active to provide a warm "pop" of color in an otherwise neutral field.