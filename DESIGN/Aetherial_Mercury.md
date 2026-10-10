---
name: Aetherial Mercury
colors:
  surface: '#131313'
  surface-dim: '#131313'
  surface-bright: '#3a3939'
  surface-container-lowest: '#0e0e0e'
  surface-container-low: '#1c1b1b'
  surface-container: '#201f1f'
  surface-container-high: '#2a2a2a'
  surface-container-highest: '#353534'
  on-surface: '#e5e2e1'
  on-surface-variant: '#c4c7c8'
  inverse-surface: '#e5e2e1'
  inverse-on-surface: '#313030'
  outline: '#8e9192'
  outline-variant: '#444748'
  surface-tint: '#c6c6c6'
  primary: '#ffffff'
  on-primary: '#2f3131'
  primary-container: '#e2e2e2'
  on-primary-container: '#636465'
  inverse-primary: '#5d5f5f'
  secondary: '#c3c0ff'
  on-secondary: '#1d00a5'
  secondary-container: '#3626ce'
  on-secondary-container: '#b3b1ff'
  tertiary: '#ffffff'
  on-tertiary: '#313033'
  tertiary-container: '#e5e1e5'
  on-tertiary-container: '#656467'
  error: '#ffb4ab'
  on-error: '#690005'
  error-container: '#93000a'
  on-error-container: '#ffdad6'
  primary-fixed: '#e2e2e2'
  primary-fixed-dim: '#c6c6c6'
  on-primary-fixed: '#1a1c1c'
  on-primary-fixed-variant: '#454747'
  secondary-fixed: '#e2dfff'
  secondary-fixed-dim: '#c3c0ff'
  on-secondary-fixed: '#0f0069'
  on-secondary-fixed-variant: '#3323cc'
  tertiary-fixed: '#e5e1e5'
  tertiary-fixed-dim: '#c9c5c9'
  on-tertiary-fixed: '#1c1b1e'
  on-tertiary-fixed-variant: '#474649'
  background: '#131313'
  on-background: '#e5e2e1'
  surface-variant: '#353534'
typography:
  display-xl:
    fontFamily: Plus Jakarta Sans
    fontSize: 72px
    fontWeight: '800'
    lineHeight: '1.1'
    letterSpacing: -0.04em
  headline-lg:
    fontFamily: Plus Jakarta Sans
    fontSize: 40px
    fontWeight: '700'
    lineHeight: '1.2'
    letterSpacing: -0.02em
  headline-lg-mobile:
    fontFamily: Plus Jakarta Sans
    fontSize: 32px
    fontWeight: '700'
    lineHeight: '1.2'
  technical-data:
    fontFamily: JetBrains Mono
    fontSize: 14px
    fontWeight: '500'
    lineHeight: '1.5'
    letterSpacing: 0.05em
  body-md:
    fontFamily: JetBrains Mono
    fontSize: 16px
    fontWeight: '400'
    lineHeight: '1.6'
  label-caps:
    fontFamily: JetBrains Mono
    fontSize: 12px
    fontWeight: '700'
    lineHeight: '1'
    letterSpacing: 0.15em
spacing:
  unit: 4px
  gutter: 24px
  margin-safe: 40px
  container-max: 1440px
---

## Brand & Style
The design system is an avant-garde exploration of "Liquid Metadata." It rejects standard SaaS conventions in favor of a high-gloss, premium aesthetic that mimics the interplay between polished obsidian and flowing liquid metal. The target audience is the elite technologist, users who value information density as much as architectural beauty.

The style is a fusion of **Glassmorphism** and **High-Contrast Minimalism**. It treats the screen as a physical substrate of deep void, where data floats as high-reflection metallic objects. The emotional response is one of precision, "quiet luxury," and futuristic permanence.

## Soul of the Design
The soul of Aetherial Mercury is defined by "Liquid Intelligence." It is a design language that rejects standard SaaS conventions in favor of a workspace that feels like a bespoke operating system for elite technologists, where data is treated with the reverence of a physical artifact.

The Soul of Aetherial Mercury:
- The Polished Void: The foundation is a deep, near-infinite black that feels like high-gloss obsidian. It creates a vacuum of visual noise, allowing your thoughts to take center stage with absolute gravity.
- Liquid Chrome Accents: Primary actions and active states are rendered in high-contrast metallic gradients. These elements behave like pools of mercury, fluid, reflective, and precious, symbolizing the shifting nature of active data.
- Ghostly Iris Glow: A deep, holographic purple bioluminescence provides depth to the interface. It acts as a subtle "pulse" within the dark space, signaling the underlying system's intelligence and readiness.
- Architectural Dissonance: The design purposefully mixes razor-sharp structural edges with perfectly fluid pill shapes. This contrast communicates a sense of bespoke engineering, the rigid logic of the machine meeting the organic flow of human thought.
- Razor Detailing: Ultra-thin 0.5px chrome borders define the containers without adding bulk. This "hairline" precision suggests a level of high-fidelity construction that is felt rather than seen.

It is a theme for the "Architect of Thought" who demands a workspace that is as sharp, cold, and uncompromisingly precise as the systems they build.

## Colors
The palette is centered on the contrast between the absolute black of the void and the reflective brilliance of chrome.

- **Substrate (Background):** #020203. This should be rendered with a subtle radial gradient to simulate a "high-gloss" obsidian finish.
- **Primary (Liquid Chrome):** #E5E5E5. Used for critical data, primary actions, and structural accents. When applied to larger surfaces, use the `liquid_chrome` gradient to simulate metallic luster.
- **Secondary (Ghostly Iris):** #4F46E5. Used sparingly for depth, interactive hover states, and to signal "active" metadata threads.
- **Borders:** Use a 0.5px "Razor" border using `rgba(229, 229, 229, 0.2)` to define edges without breaking the fluid aesthetic.

## Typography
The typography system creates an "architectural" hierarchy by pairing a wide-set, aggressive grotesque for headlines with a technical, high-contrast monospace for data.

- **Headlines:** Use **Plus Jakarta Sans**. Bold, wide-set, and tightly tracked. This represents the "structure" of the archive.
- **Data/Body:** Use **JetBrains Mono**. This emphasizes the "metadata" aspect of the concept, making every piece of information feel like a line of precise code.
- **Scale:** High contrast in scale is encouraged. Large display headings should feel massive against small, legible technical labels.

## Layout & Spacing
The layout follows a **Fixed Grid** philosophy that mimics a physical blueprint or archive ledger. 

- **Grid:** A 12-column system with 24px gutters. 
- **Margins:** Generous 40px outer margins to allow the "Void" background to breathe, reinforcing the premium, spacious feel.
- **Rhythm:** Spacing should be strictly mathematical, based on a 4px baseline unit. 
- **Adaptation:** On mobile, containers transition from sharp-edged floating modules to full-bleed panels, while "Pill" elements remain centered and prominent.

## Elevation & Depth
In this design system, depth is not signaled by shadows, but by **Reflectivity and Blurs**.

- **Polished Obsidian:** Level 1 surfaces (cards/panels) use a backdrop blur of 20px and a 40% opaque background of #111111. 
- **Razor Edges:** Instead of shadows, use 0.5px borders. Top and left borders should be slightly lighter (`rgba(255,255,255,0.1)`) than bottom and right borders to simulate a top-down light source hitting a metallic edge.
- **Ghost Highlights:** Interactive elements emit a subtle #4F46E5 outer glow (blur: 15px, spread: -5px) when active, suggesting an underlying energy source.

## Shapes
The shape language is a study in geometric tension.

- **Structural Elements:** Panels, containers, and data cells use a **0px (Sharp)** corner radius. This creates a rigid, architectural framework.
- **Interactive Elements:** Buttons, chips, and toggles use a **Pill (Circular)** radius. This suggests that "Liquid Mercury" can be touched and manipulated, providing a clear visual distinction between what is *content* and what is *control*.

## Components
- **Buttons:** Primary buttons use the `liquid_chrome` gradient with black text. On hover, the gradient should "shimmer" (shift background position). All buttons are perfectly pill-shaped.
- **Metadata Chips:** Small, pill-shaped outlines with `label-caps` typography. They should look like stamped metal tags.
- **Input Fields:** Sharp 0px rectangles with a 0.5px bottom-only border. When focused, the border transitions to the `secondary_color_hex` (Ghostly Iris).
- **Cards:** Glassmorphic obsidian panels with sharp corners. No shadows; only the razor border defines the boundary.
- **Lists:** Data rows separated by 0.5px chrome lines. High-contrast hover states where the entire row gains a subtle #111111 tint.
- **Scrollbars:** Ultra-thin (2px) chrome tracks with no background, appearing only on interaction to maintain the "Void" aesthetic.