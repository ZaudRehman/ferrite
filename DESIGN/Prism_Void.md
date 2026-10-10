---
name: Prism Void
colors:
  surface: '#fcf9f8'
  surface-dim: '#dcd9d9'
  surface-bright: '#fcf9f8'
  surface-container-lowest: '#ffffff'
  surface-container-low: '#f6f3f2'
  surface-container: '#f0eded'
  surface-container-high: '#eae7e7'
  surface-container-highest: '#e4e2e1'
  on-surface: '#1b1c1c'
  on-surface-variant: '#4c4546'
  inverse-surface: '#303030'
  inverse-on-surface: '#f3f0f0'
  outline: '#7e7576'
  outline-variant: '#cfc4c5'
  surface-tint: '#5e5e5e'
  primary: '#000000'
  on-primary: '#ffffff'
  primary-container: '#1b1b1b'
  on-primary-container: '#848484'
  inverse-primary: '#c6c6c6'
  secondary: '#5d5f5f'
  on-secondary: '#ffffff'
  secondary-container: '#dfe0e0'
  on-secondary-container: '#616363'
  tertiary: '#000000'
  on-tertiary: '#ffffff'
  tertiary-container: '#1b1d0e'
  on-tertiary-container: '#848570'
  error: '#ba1a1a'
  on-error: '#ffffff'
  error-container: '#ffdad6'
  on-error-container: '#93000a'
  primary-fixed: '#e2e2e2'
  primary-fixed-dim: '#c6c6c6'
  on-primary-fixed: '#1b1b1b'
  on-primary-fixed-variant: '#474747'
  secondary-fixed: '#e2e2e2'
  secondary-fixed-dim: '#c6c6c7'
  on-secondary-fixed: '#1a1c1c'
  on-secondary-fixed-variant: '#454747'
  tertiary-fixed: '#e4e4cc'
  tertiary-fixed-dim: '#c8c8b0'
  on-tertiary-fixed: '#1b1d0e'
  on-tertiary-fixed-variant: '#474836'
  background: '#fcf9f8'
  on-background: '#1b1c1c'
  surface-variant: '#e4e2e1'
typography:
  headline-display:
    fontFamily: Geist
    fontSize: 24px
    fontWeight: '300'
    lineHeight: '1.5'
    letterSpacing: 0.1em
  body-reading:
    fontFamily: Geist
    fontSize: 16px
    fontWeight: '400'
    lineHeight: '1.8'
  label-mono:
    fontFamily: Geist
    fontSize: 10px
    fontWeight: '500'
    lineHeight: '1.0'
    letterSpacing: 0.2em
spacing:
  margin: 10vw
  gutter: 4vw
  safe-area: 2rem
---

## Brand & Style
The brand is an exercise in extreme clarity and intellectual focus. It is "Abstract Minimalist," stripping away everything but the essential. It targets researchers, writers, and minimalists who require a zero-distraction environment.

The style is characterized by **Maximum Whitespace** and **Refractive Interaction**. The UI starts as a pure white "void." Color only appears as a light-splitting prism effect, spectral gradients on hair-thin lines or when the user interacts with an element.

## Soul of the Design
The soul of Prism Void is defined by "Intellectual Absolute." It is a design language that rejects the clutter of the physical world in favor of a pure, digital architecture where ideas can exist in their most concentrated form.

At its core, this design is defined by:

- The Infinite Substrate: The palette is anchored by an Absolute White (#ffffff). Unlike standard light modes that use off-whites or subtle grays, Prism Void uses a stark, high-luminance canvas to create a "void" of distraction. It is a sterile, gallery-like environment where the only color comes from the user's thoughts.
- The Hairline Boundary: Instead of shadows or heavy fills, hierarchy is defined by Ultra-Thin 0.5px Strokes. These boundaries are surgical and precise, signaling that the system respects the structure of your data without imposing its own weight upon it. It is the "Ghost in the Machine" aesthetic, present but weightless.
- Monochrome Precision: By utilizing the Geist typeface and a strictly monochromatic scale, the interface adopts the tone of a high-end technical manual or a research colophon. It treats text not as decoration, but as data, optimizing for the "Flow State" where the tool disappears and the work becomes the interface.
- Contextual Elevation: Rejecting traditional "layered" UI, Prism Void indicates interaction through subtle shifts in border thickness or high-contrast monochromatic inversions. This geometric discipline mirrors the uncompromising logic of minimalist architecture, communicating a sense of permanence, clarity, and intellectual focus.

In essence, the soul of Prism Void is Extreme Clarity. It is built for the "Pure Thinker", the researcher, writer, or theorist who demands a workspace that is as empty, sharp, and focused as a new sheet of paper, allowing the gravity of their own ideas to define the space.

## Colors
The system is strictly monochromatic: Pure White (#ffffff) for surfaces and Pitch Black (#000000) for text. 

The "Rainbow-Refraction" is the only source of color. It is never used for fills; it is only applied to **0.5px hairlines** or as a "fringe" effect on the edges of active elements. It represents the "splitting of light" that occurs when a thought becomes a note.

## Typography
**Geist** is chosen for its mathematical precision and developer-centric clarity. The typography is spaced out significantly to emphasize the "Void" concept.

Headlines are never bold; they use light weights and wide tracking to feel airy and architectural. Body text is optimized for long-form reading with a generous 1.8 line height. Every character should feel like it has room to breathe.

## Layout & Spacing
The system uses **No Grid** in the traditional sense. It relies on **Contextual Layout** and massive safe-area margins (10vw). Content is centered or aligned to specific typographic anchors rather than columns.

The layout adapts to screen size by increasing the whitespace rather than scaling the content. On mobile, margins reduce to 2rem, but the "empty" feel is preserved by limiting the number of elements shown at once.

## Elevation & Depth
Depth is suggested via **Refractive Glass and Outlines**.
1.  **Ultra-Thin Hairlines:** 0.5px borders define the boundaries of notes.
2.  **Prism Hover:** On hover, a border transforms from black to the "prism" gradient, and a tiny 2px blur "refraction" appears behind the element.
3.  **Zero Shadows:** There are no shadows in this system. Elevation is indicated by a "shimmer" effect, a slight shift in the prism gradient.

## Shapes
The shape language is strictly **Sharp (0px roundedness)**. This reinforces the precision and "void" aesthetic. Circles are used only for functional icons to contrast against the sharp rectangularity of the note-taking containers.

## Components
-   **Buttons:** Transparent with a 0.5px black border. On hover, the border becomes the prism gradient and the text shifts 2px to the right.
-   **Notes:** Floating blocks of text with no background. They are separated by a single vertical 0.5px line on the left.
-   **Input:** A single black hairline that spans the width of the viewport. No labels; only placeholder text in a light weight.
-   **Navigation:** A series of tiny dots at the bottom of the screen. Hovering over a dot reveals the prism gradient and the label in a monospaced font.
-   **Cursor:** A custom "prism" dot that leaves a temporary, faint trail of color.