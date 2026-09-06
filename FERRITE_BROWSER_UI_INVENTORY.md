# Ferrite Browser: Complete UI Inventory
**Version:** 1.0 | **Purpose:** Complete reference for all browser components, pages, and features

This document provides an exhaustive inventory of every UI element, page, component, and feature in the Ferrite browser, including their purpose, placement, behavior, and implementation status.

---

## Table of Contents
1. [Window Frame & Chrome](#1-window-frame--chrome)
2. [Tab Bar](#2-tab-bar)
3. [Navigation Toolbar](#3-navigation-toolbar)
4. [Omnibox](#4-omnibox)
5. [Status Indicators](#5-status-indicators)
6. [Content Area](#6-content-area)
7. [Internal Pages](#7-internal-pages)
8. [Panels & Sidebars](#8-panels--sidebars)
9. [Modals & Dialogs](#9-modals--dialogs)
10. [Context Menus](#10-context-menus)
11. [Notifications](#11-notifications)
12. [Keyboard Shortcuts](#12-keyboard-shortcuts)
13. [Backend Components](#13-backend-components)

---

## 1. Window Frame & Chrome

### 1.1 OS Native Window
**Location:** Outermost container
**Purpose:** Application window managed by OS
**Components:**
- Title bar (OS-managed or custom)
- Window controls (minimize, maximize, close)
- Resize handles
- High-DPI canvas

**Implementation Status:** ✅ Implemented via `winit`

### 1.2 Custom Titlebar (Optional)
**Location:** Top of window (if OS decorations disabled)
**Purpose:** Custom-styled window controls matching Ferrite theme
**Components:**
- App icon (Ferrite logo)
- Window title
- Minimize button (pill-shaped)
- Maximize/Restore button (pill-shaped)
- Close button (pill-shaped, danger color on hover)

**Implementation Status:** ⚠️ Not implemented (using OS decorations)

### 1.3 GPU Rendering Surface
**Location:** Entire window canvas
**Purpose:** Hardware-accelerated rendering via wgpu
**Components:**
- egui rendering context
- DPI scaling logic
- Frame pacing and vsync

**Implementation Status:** ✅ Implemented

---

## 2. Tab Bar

### 2.1 Tab Bar Container
**Location:** Top of window, below titlebar
**Purpose:** Horizontal container for all tab widgets
**Style:**
- Background: Surface L1 (#131313)
- Height: 40px
- Sharp corners (0px radius)
- Bottom border: 0.5px hairline in Surface L3

**Implementation Status:** ✅ Implemented

### 2.2 New Tab Button
**Location:** Left edge of tab bar
**Purpose:** Creates new tab with default page
**Style:**
- Shape: Pill (full rounding)
- Size: 32x32px
- Icon: "+" symbol
- Background: Surface L2 (#1c1b1b)
- Hover: Surface L3 (#2a2a2a)
- Active: Interactive accent (#4F46E5)

**Behavior:**
- Click: Opens new tab with `ferrite://newtab`
- Shortcut: Ctrl+T (Cmd+T on macOS)

**Implementation Status:** ✅ Implemented

### 2.3 Tab Widget (Per Tab)
**Location:** Horizontal sequence in tab bar
**Purpose:** Represents a single browser tab
**Components:**

#### 2.3.1 Favicon
**Size:** 16x16px
**Position:** Left side of tab widget
**Source:** Loaded from webview or default icon
**States:**
- Normal: Full opacity
- Loading: Hidden (throbber shown instead)
- Crashed: Warning icon
- Discarded: 50% opacity

**Implementation Status:** ⚠️ Partially implemented

#### 2.3.2 Page Title
**Position:** Center of tab widget
**Style:**
- Font: System sans-serif (Inter)
- Size: 12px
- Color: Foreground (#E5E2E1)
- Max width: 150px (truncated with ellipsis)
- Active tab: Full opacity
- Background tab: 70% opacity

**Implementation Status:** ✅ Implemented

#### 2.3.3 Loading Throbber
**Position:** Replaces favicon during load
**Style:**
- Animated spinner
- Color: Interactive accent (#4F46E5)
- Size: 16x16px
- Animation: Smooth rotation (1s duration)

**Implementation Status:** ⚠️ Not implemented (uses static icon)

#### 2.3.4 Audio Indicator
**Position:** Right side, before close button
**Style:**
- Icon: Speaker glyph (🔊)
- Size: 14px
- Color: Foreground
**Behavior:**
- Shows when tab is playing audio
- Click: Toggles mute state
- Muted state: Speaker with X (🔇)

**Implementation Status:** ⚠️ Partially implemented

#### 2.3.5 Close Button
**Position:** Right edge of tab widget
**Style:**
- Icon: "×" symbol
- Size: 14px
- Color: Foreground (70% opacity)
- Hover: Danger accent (#FF003F)
- Only visible on active tab or hover

**Behavior:**
- Click: Closes tab
- Middle-click on tab: Closes tab
- Shortcut: Ctrl+W

**Implementation Status:** ✅ Implemented

#### 2.3.6 Active Tab Indicator
**Position:** Bottom edge of tab widget
**Style:**
- 2px underline
- Color: Interactive accent (#4F46E5) or Archive accent (#F0C75E)
- Width: Matches tab width

**Implementation Status:** ✅ Implemented

#### 2.3.7 Pinned Tab Treatment
**Style:**
- Icon-only (no title text)
- Sorted to leading edge
- No close button
- Smaller width: 40px
- Excluded from bulk close operations

**Implementation Status:** ⚠️ Partially implemented

#### 2.3.8 Discarded Tab State
**Style:**
- Favicon and title at 50% opacity
- No loading throbber
- Background: Surface L1
**Behavior:**
- Selecting triggers reactivation
- Lazy loads content on demand

**Implementation Status:** ✅ Implemented

### 2.4 Tab Overflow Handler
**Location:** Right side of tab bar (when needed)
**Purpose:** Handles tabs that exceed window width
**Components:**
- Left scroll arrow (‹)
- Right scroll arrow (›)
- Scrollable tab container
- Tab search button (magnifying glass)

**Implementation Status:** ⚠️ Not implemented (tabs shrink instead)

### 2.5 Tab Drag & Drop
**Behavior:**
- Drag tab to reorder within tab bar
- Visual feedback: Semi-transparent tab preview follows cursor
- Drop indicator: Vertical line shows insertion point
- Drag out of window: Detach tab (future feature)

**Implementation Status:** ⚠️ Not implemented

### 2.6 Tab Search Overlay
**Trigger:** Ctrl+Shift+A
**Location:** Centered overlay
**Purpose:** Fuzzy search across all open tabs
**Components:**
- Search input (auto-focused)
- Scrollable list of tabs with:
  - Favicon
  - Page title
  - URL
  - Source indicator (tab-switch icon)
- Keyboard navigation (↑/↓/Enter/Esc)

**Implementation Status:** ✅ Implemented

---

## 3. Navigation Toolbar

### 3.1 Toolbar Container
**Location:** Below tab bar, above content area
**Height:** 44px
**Style:**
- Background: Surface L1 (#131313)
- Bottom border: 0.5px hairline

**Implementation Status:** ✅ Implemented

### 3.2 Back Button
**Location:** Left side of toolbar
**Style:**
- Shape: Pill button
- Size: 36x32px
- Icon: "‹" or arrow-left glyph
- Disabled: 30% opacity (when back_stack empty)
- Hover: Surface L3 background

**Behavior:**
- Click: Navigate back in history
- Long-press: Show back history dropdown
- Shortcut: Alt+Left

**Implementation Status:** ✅ Implemented (basic)

### 3.3 Forward Button
**Location:** Next to back button
**Style:** Same as back button
**Icon:** "›" or arrow-right glyph

**Behavior:**
- Click: Navigate forward in history
- Long-press: Show forward history dropdown
- Shortcut: Alt+Right

**Implementation Status:** ✅ Implemented (basic)

### 3.4 Reload/Stop Button
**Location:** Next to forward button
**Style:**
- Shape: Pill button
- Size: 36x32px
- Reload icon: "↻" (circular arrow)
- Stop icon: "×" (times symbol)
- Toggles based on `is_loading` state

**Behavior:**
- Click during loading: Stop navigation
- Click when loaded: Reload page
- Shortcut: F5 or Ctrl+R

**Implementation Status:** ✅ Implemented (basic)

### 3.5 Home Button (Optional)
**Location:** Next to reload button
**Style:** Pill button
**Icon:** House glyph
**Behavior:**
- Click: Navigate to `ferrite://newtab`

**Implementation Status:** ❌ Not implemented

---

## 4. Omnibox

### 4.1 Omnibox Container
**Location:** Center of navigation toolbar
**Width:** Flexible (expands to fill available space)
**Height:** 32px
**Style:**
- Background: Surface L2 (#1c1b1b)
- Border: None (sharp corners, 0px)
- Bottom border: 0.5px in outline color
- Focus: Bottom border transitions to interactive accent

**Implementation Status:** ✅ Implemented

### 4.2 Security Indicator
**Location:** Left side of omnibox
**Components:**
- HTTPS lock icon: "🔒" (Foreground color)
- HTTP icon: "○" (muted color)
- Internal page icon: Ferrite logo
- Security warning: Shield with warning triangle

**Implementation Status:** ✅ Implemented (basic)

### 4.3 URL Display & Input
**Location:** Main omnibox area
**Style:**
- Font: JetBrains Mono
- Size: 14px
- Color: Foreground (#E5E2E1)

#### URL Display Mode (At Rest)
- **Registrable Domain Highlighting:**
  - eTLD+1: Full opacity (e.g., "github.com")
  - Scheme, subdomain, path, query: Muted (50% opacity)
  - Example: `https://www.github.com/user/repo` shows "github.com" bright

#### Edit Mode (Focused)
- Entire URL shows in full opacity
- Text selectable and editable
- Cursor visible

**Implementation Status:** ⚠️ Partially implemented (no domain highlighting)

### 4.4 Search Engine Selector
**Location:** Left side, after security icon
**Style:**
- Pill-shaped button
- Shows current search engine icon + name
- Size: 24x24px icon + text label
- Hover: Surface L3 background

**Behavior:**
- Click: Opens search engine dropdown
- Dropdown shows all available engines
- Selecting changes default engine for this session

**Available Engines:**
1. DuckDuckGo (default)
2. Google
3. Brave Search
4. Bing

**Implementation Status:** ✅ Implemented

### 4.5 Autocomplete Dropdown
**Location:** Below omnibox (floating panel)
**Style:**
- Background: Surface L2 (#1c1b1b)
- Sharp corners (0px radius)
- Border: 0.5px hairline
- Max height: 400px (scrollable)
- Row height: 36px

**Components:**

#### 4.5.1 History Suggestions
- Icon: Clock glyph
- Text: Page title
- Secondary text: URL
- Source label: "History"

#### 4.5.2 Open Tab Suggestions
- Icon: Tab-switch glyph
- Text: Page title
- Secondary text: URL
- Source label: "Open tab"
- Click: Switches to existing tab

#### 4.5.3 Bookmark Suggestions
- Icon: Bookmark glyph
- Text: Page title
- Secondary text: URL
- Source label: "Bookmark"

#### 4.5.4 Archive Full-Text Results
- Icon: Archive glyph
- Text: Page title or content snippet
- Secondary text: URL + capture date
- Source label: "From your archive"

#### 4.5.5 Search Suggestions (Optional)
- Icon: Search engine favicon
- Text: Suggested query
- Source label: "via [Engine]"
- Only shown if user opted in

#### 4.5.6 Offline Instant-Answer
- Position: Pinned to top
- Shows calculation or unit conversion result
- Icon: Calculator glyph
- Enter: Copies result instead of navigating

**Implementation Status:** ⚠️ Partially implemented (history only)

### 4.6 Time Machine Dropdown
**Location:** Secondary dropdown (below main autocomplete)
**Trigger:** Domain has archived snapshots
**Style:**
- Shows list of available snapshot dates
- Each row: Date + time of capture
- Click: Loads archived snapshot

**Implementation Status:** ❌ Not implemented

### 4.7 Clear Button
**Location:** Right side of omnibox (when text present)
**Style:**
- Icon: "×" symbol
- Size: 16px
- Color: Muted foreground
- Hover: Foreground

**Behavior:**
- Click: Clears all text
- Focuses omnibox

**Implementation Status:** ✅ Implemented

### 4.8 Tab-to-Search Chip
**Trigger:** User presses Tab after typing domain with OpenSearch
**Style:**
- Pill-shaped chip
- Shows favicon + domain name
- Background: Surface L3

**Behavior:**
- Indicates search is scoped to specific site
- Subsequent input searches within that site

**Implementation Status:** ❌ Not implemented

---

## 5. Status Indicators

### 5.1 Shield Badge
**Location:** Right side of omnibox bar
**Purpose:** Shows privacy protection status
**Style:**
- Icon: Shield vector glyph
- Counter: Number in Geist Pixel Square font
- Size: 32x32px hit target
- Idle: Outline style
- Active: Filled with interactive accent
- Pulse: Scales 120% + danger color flash on new block

**Behavior:**
- Click: Opens shield dropdown
- Shows count of blocked trackers/ads

**Implementation Status:** ⚠️ Partially implemented (no animation)

### 5.2 Shield Dropdown
**Location:** Below shield badge
**Style:**
- Background: Surface L2
- Sharp corners
- Width: 320px
- Max height: 400px (scrollable)

**Components:**

#### 5.2.1 Blocking Summary
- Total blocked count
- Breakdown by category:
  - Trackers: Count
  - Ads: Count
  - Mixed content: Count

#### 5.2.2 Blocked Domains List
- Each row:
  - Domain name
  - Classification (Tracker/Ad/Mixed Content)
  - Resource type (script/image/iframe)
- Scrollable list

#### 5.2.3 Connection Security Detail
- TLS version: (e.g., "TLS 1.3")
- Cipher suite: (e.g., "AES_256_GCM_SHA384")
- Padlock icon

#### 5.2.4 Pause Blocking Toggle
- Switch: On/Off
- Label: "Pause blocking on this site"
- Duration: Until tab closed or manually re-enabled

**Implementation Status:** ⚠️ Partially implemented

### 5.3 Archive Pin Button
**Location:** Right side, after shield badge
**Purpose:** Save current page to archive
**Style:**
- Icon: Bookmark/pin glyph
- Size: 32x32px
- Inactive: Outline style
- Active (pinned): Filled with archive accent (#F0C75E)

**Behavior:**
- Click: Pins current page to archive
- Click again: Unpins
- Shows confirmation toast

**Implementation Status:** ⚠️ Partially implemented

### 5.4 DevTools Toggle Button
**Location:** Right side, after archive button
**Purpose:** Open/close developer tools
**Style:**
- Icon: Terminal/code glyph
- Size: 32x32px
- Inactive: Muted
- Active (open): Interactive accent

**Behavior:**
- Click: Toggles inspector panel
- Shortcut: F12 or Ctrl+Shift+I

**Implementation Status:** ✅ Implemented

### 5.5 Dev Mode Indicator
**Location:** Replaces shield badge when active
**Purpose:** Shows localhost bypass is active
**Style:**
- Icon: Terminal glyph
- Color: Interactive accent
**Trigger:**
- User has `localhost` or `127.0.0.1` open
- Privacy features temporarily relaxed for development

**Implementation Status:** ❌ Not implemented

---

## 6. Content Area

### 6.1 Central Panel
**Location:** Main content area below toolbar
**Purpose:** Container for webview or internal pages
**Style:**
- Background: Substrate (#020203)
- Fills remaining window space

**Implementation Status:** ✅ Implemented

### 6.2 WebView Container
**Location:** Within central panel
**Purpose:** Renders OS-native webview (WebKitGTK/WebView2/WKWebView)
**Behavior:**
- Positioned spatially to match egui layout
- Only active tab's webview is visible
- Background tabs: `set_visible(false)`
- Suspended tabs: Webview destroyed, state preserved
- Discarded tabs: No webview until reactivated

**Implementation Status:** ✅ Implemented

### 6.3 Loading Progress Bar
**Location:** Top of content area (2px height)
**Style:**
- Color: Interactive accent
- Animated: Smooth width transition
- Disappears when load complete

**Implementation Status:** ⚠️ Not implemented (text only)

### 6.4 Find-in-Page Bar
**Location:** Top of content area (when active)
**Trigger:** Ctrl+F
**Components:**
- Search input field
- Match count (e.g., "3 of 15")
- Previous/Next buttons
- Close button

**Implementation Status:** ❌ Not implemented

### 6.5 Scroll Position Indicator
**Location:** Right edge of content area
**Style:**
- Thin scrollbar (8px width)
- Color: Surface L3
- Hover: Foreground
- Auto-hide when not scrolling

**Implementation Status:** ✅ OS-managed

---

## 7. Internal Pages

### 7.1 `ferrite://newtab` - New Tab Page
**Location:** Default page for new tabs
**Layout:** Centered, vertical stack

**Components:**

#### 7.1.1 Logo & Branding
- "Ferrite" logo (28px, weight 800)
- Subtitle: "Sovereign Browser" (13px, Geist Pixel Square, uppercase, muted)

#### 7.1.2 Search Bar
- Width: 560px max
- Height: 48px
- Font: JetBrains Mono
- Placeholder: "Search with DuckDuckGo or enter URL - press Enter"
- Border: 0.5px Surface L3
- Focus: Border transitions to interactive accent
- Auto-focused on page load

**Behavior:**
- Enter: Searches or navigates based on input
- Communicates with Rust via IPC

#### 7.1.3 Search Hint
- Text: "Press Enter to search or navigate. Shortcuts below are instant."
- Font: Geist Pixel Square, 11px
- Color: Muted

#### 7.1.4 Shortcut Tiles Grid
- Grid: 4 columns × variable rows
- Tile size: 80x80px
- Each tile:
  - Background: Surface L1
  - Border: 0.5px Surface L3
  - Hover: Border becomes interactive accent + lift animation
  - Icon/abbreviation: 14px bold
  - Label: 11px muted
- Default shortcuts:
  - DDG (DuckDuckGo)
  - G (Google)
  - GH (GitHub)
  - HN (Hacker News)

**Behavior:**
- Click: Instantly navigates to URL

#### 7.1.5 Recent Noise Widget
- Position: Bottom-right corner
- Shows last 24h of blocked trackers
- Counter in Geist Pixel Square
- Click: Opens shield dropdown

**Implementation Status:** ✅ Implemented (HTML), ⚠️ IPC issues

### 7.2 `ferrite://settings` - Settings Page
**Location:** Settings internal page
**Layout:** Left navigation rail + right content panel

**Navigation Rail:**
- General
- Privacy/Shield
- Archive & Vault
- Passwords
- Sync
- DevTools
- Updates

**Content Panels:**

#### 7.2.1 General Panel
- Default search engine dropdown
- Startup behavior radio buttons:
  - Continue where left off
  - Open new tab
- Theme toggle (Dark/Light/System) - Future
- Language selector - Future

#### 7.2.2 Privacy/Shield Panel
- **Master Adblock Toggle**
  - On by default
  - Turns off all blocking
  
- **Filter Lists Section**
  - EasyList (default, checked)
  - EasyPrivacy (default, checked)
  - Fanboy's Annoyance (optional)
  - Custom list URL input
  - Last updated timestamp per list
  - Integrity check status

- **Cosmetic Filtering Toggle**
  - On by default
  - Injects CSS to hide ad elements

- **Fingerprint Spoofing Toggle**
  - On by default
  - Overrides Canvas/WebGL/AudioContext APIs

- **De-AMP Toggle**
  - On by default
  - Redirects AMP pages to canonical

- **URL Parameter Stripping Toggle**
  - On by default
  - Removes tracking parameters (gclid, fbclid, utm_source, etc.)

- **Global Privacy Control Toggle**
  - On by default
  - Sends Sec-GPC: 1 header
  - Disclosure: "Signals to websites that you object to data selling"

- **Real-Time Threat Lookup Toggle** (opt-in, off by default)
  - k-anonymity hash-prefix design
  - Disclosure: "Checks sites against Google Safe Browsing via hashed prefixes (300ms timeout)"

- **Live Search Suggestions Toggle** (opt-in, off by default)
  - Disclosure: "Sends search queries to selected engine for suggestions"

#### 7.2.3 Archive & Vault Panel
- **Disk Space Usage**
  - Progress bar showing archive size vs limit
  - Text: "2.3 GB of 10 GB used"

- **Archive Size Limit**
  - Slider: 1GB to 50GB
  - Default: 10GB

- **Auto-Snapshot Toggle**
  - On by default
  - Automatically archives all visited pages

- **Lazy Archive Sync Toggle**
  - Off by default
  - Eagerly fetches pinned snapshots on new devices

- **Clear Browsing Data Button**
  - Opens modal with checkboxes:
    - Browsing history
    - Download history
    - Cached images and files
    - Cookies
    - Archived snapshots
  - Time range dropdown: Last hour / 24 hours / 7 days / All time

- **Encryption Status**
  - Always shows: "SQLCipher Encryption: Active (AES-256)"
  - Key source: "Key stored in OS Keychain"

- **Account Recovery Section** (opt-in)
  - "Enable Recovery Phrase" button
  - "View Recovery Phrase" button (shows once, requires OS auth)
  - "Revoke Recovery Phrase" button
  - Disclosure: "Ferrite never sees or stores your recovery phrase. Write it down and keep it safe."

#### 7.2.4 Passwords Panel
- Links to `ferrite://passwords` (full page)
- Quick toggles:
  - **Breach Checking Toggle** (opt-in)
    - Disclosure: "Checks saved passwords against known breaches using k-anonymity"
    - Last checked timestamp
  - **Password Generator Preference**
    - Radio: Random characters / Passphrase
    - If passphrase:
      - Word count: 4-8 slider
      - Separator: Space / Hyphen / Period / None
      - Capitalize: Toggle

#### 7.2.5 Sync Panel
- **E2EE Cloud Backup Section**
  - Provider selector: WebDAV / Google Drive / Dropbox
  - "Connect Account" button
  - Connected account email (read-only)
  - "Disconnect" button
  - Last backup timestamp
  - "Backup Now" button
  
- **Backup Versioning**
  - List of retained backups:
    - Timestamp
    - Size
    - "Restore" button per version
  - Retention policy: Keep last 10 backups

- **Master Password Section** (for cloud backup encryption)
  - "Set Master Password" button
  - "Change Master Password" button
  - Argon2id KDF indicator

- **P2P Device Sync Section**
  - QR code display (for LAN pairing)
  - "Pair Across Networks" button (starts relay-mediated flow)
  
- **Selective Sync Toggles**
  - Bookmarks (on by default)
  - History (on by default)
  - Credentials (on by default)
  - Autofill (on by default)
  - TOTP (on by default)
  - Archive (off by default, large data)

- **Relay Configuration**
  - Default relay list:
    - `relay1.ferritebrowser.org:8080` (status: green/red)
    - `relay2.ferritebrowser.org:8080` (status: green/red)
  - Self-hosted relay URL input
  - "Test Connection" button

- **Trusted Devices List**
  - Each row:
    - Device name
    - Device ID (short)
    - Online status (green dot)
    - Last seen timestamp
    - Last synced timestamp
    - Paired via: Direct / Vouched by [device name]
    - "Remove" button
  - "Remove All Devices" button (danger)

#### 7.2.6 DevTools Panel
- **Custom JS Injection Table**
  - Columns: Domain, JS File Path, Enabled toggle
  - "Add Injection" button
  - File picker for .js files
  
- **Custom CSS Injection Table**
  - Columns: Domain, CSS File Path, Enabled toggle
  - "Add Injection" button

- **Localhost Auto-Permissions Toggle**
  - On by default
  - Disables CORS and cookie partitioning for `localhost` and `127.0.0.1`

#### 7.2.7 Updates Panel
- **Current Version**
  - Version number: (e.g., "1.0.0")
  - Channel: Stable / Beta / Dev
  
- **Channel Selector**
  - Dropdown: Stable / Beta / Dev
  - Disclosure per channel:
    - Stable: "Fully tested, recommended for most users"
    - Beta: "New features, may have bugs"
    - Dev: "Bleeding edge, unstable"

- **Check for Updates Button**
  - "Check Now" button
  - Shows progress spinner during check
  - Result: "Up to date" or "Update available - v1.1.0"

- **Auto-Update Toggle**
  - On by default
  - Automatically downloads and installs updates on restart

- **Rollback Status** (if applicable)
  - Shows if previous update was reverted
  - Reason for rollback

**Implementation Status:** ✅ Implemented (HTML), ⚠️ Some features missing

### 7.3 `ferrite://archive` - Archive Page
**Location:** Archive browser internal page
**Layout:** Search bar at top + grid/timeline below

**Components:**

#### 7.3.1 Search & Filter Bar
- Search input (auto-focused)
- Queries both URL/title AND full-text content
- Filter toggles:
  - All
  - Pinned only
  - This week/month/year

#### 7.3.2 Timeline/Grid View Toggle
- Timeline view: Chronological vertical list
- Grid view: Card grid (3-5 columns based on width)

#### 7.3.3 Snapshot Cards
Each card contains:
- Favicon (if available)
- URL
- Page title (truncated)
- Capture timestamp
- Size indicator
- Pinned badge (if applicable)
- Metadata-only indicator (if lazy-synced)
- "Open" button
- "Delete" button
- "Pin/Unpin" toggle

#### 7.3.4 Pinned Section
- Separate section at top
- Always visible, not affected by scroll
- Grid layout

#### 7.3.5 LRU Cache Section
- Main archive section
- Sorted by most recent
- Infinite scroll or pagination

**Implementation Status:** ⚠️ Partially implemented

### 7.4 `ferrite://passwords` - Password Manager
**Location:** Password manager internal page
**Requires:** OS Keychain re-authentication to view

**Components:**

#### 7.4.1 Authentication Gate
- Shown before any passwords displayed
- "Authenticate to view passwords" button
- Uses TouchID/Windows Hello/PIN

#### 7.4.2 Credential List
- Searchable table
- Columns:
  - Favicon
  - URL (sortable)
  - Username (sortable)
  - Password (masked: ••••••)
  - Last used date
  - Actions

#### 7.4.3 Password Actions
- Eye icon: Toggle mask/reveal
- Copy icon: Copy password to clipboard (auto-clear 30s)
- Edit icon: Open edit modal
- Delete icon: Delete (with confirmation)

#### 7.4.4 Add Credential Button
- Floating action button (bottom-right)
- Opens add credential modal

#### 7.4.5 Autofill Data Tabs
Tab bar with:
- **Passwords** (default tab)
- **Addresses**
  - Name, street, city, state, zip, country, phone, email
  - Add/Edit/Delete actions
- **Payment Cards**
  - Cardholder name, card number (masked), expiry, card type
  - Luhn validation indicator
  - Add/Edit/Delete actions

#### 7.4.6 TOTP Tab
- List of TOTP configurations
- Each row:
  - Service name
  - Account
  - Live-refreshing code (updates every 30s)
  - Progress bar showing remaining time
  - Copy button
- Add TOTP button (scans QR code or manual entry)

#### 7.4.7 Password Health Tab
- Categories:
  - **Weak**: Passwords scoring < 3/5
  - **Reused**: Same password used on multiple sites
  - **Old**: Not changed in > 90 days
  - **Breached**: Found in known breach (if opted in)
- Each category shows count + list
- "Generate replacement" button per entry
- Breached shows "Enable breach checking" prompt if not opted in

#### 7.4.8 Import/Export Section
- **Import button**
  - Dropdown: Chrome CSV / Firefox CSV / Bitwarden CSV / 1Password CSV
  - File picker
  - Post-import: Run health check prompt
  
- **Export button** (danger styling)
  - Warning: "This will export passwords in plaintext"
  - Requires confirmation
  - Downloads CSV file

**Implementation Status:** ⚠️ Partially implemented (backend done, UI basic)

### 7.5 `ferrite://crashed` - Tab Crashed Page
**Purpose:** Shown when webview process crashes
**Layout:** Centered, vertical stack

**Components:**
- Sad Ferrite icon (large)
- Heading: "This tab has crashed."
- URL display (muted)
- Buttons:
  - "Reload Tab" (primary action, pill-shaped)
  - "Close Tab" (secondary action, danger-styled)

**Implementation Status:** ✅ Implemented (HTML)

### 7.6 `ferrite://blocked` - Security Warning Page
**Purpose:** Shown when site is blocked by threat intelligence
**Layout:** Centered warning panel

**Components:**
- Warning icon (Crimson color, large)
- Heading: "Connection Blocked"
- Text: "Ferrite has determined this site is a known phishing/malware vector."
- Domain display
- Buttons:
  - "Go Back" (primary action)
  - "Proceed at your own risk" (secondary, danger-styled, requires double-confirm)

**Style:**
- Background: Surface L1
- Top border: 2px Crimson accent

**Implementation Status:** ✅ Implemented (HTML, basic)

---

## 8. Panels & Sidebars

### 8.1 Developer Tools Panel (Inspector)
**Location:** Bottom of content area (docked) or right sidebar
**Trigger:** F12 or Ctrl+Shift+I or DevTools button
**Width:** 100% (docked bottom) or 400px (sidebar)

**Tab Bar:**
- Waterfall
- Console
- Elements
- Storage
- Device
- Injection

#### 8.1.1 Waterfall Tab
**Purpose:** Network traffic visualization
**Components:**

##### Waterfall Header
- Record/Stop toggle button
- Clear button
- Filter by type:
  - All / Doc / JS / CSS / Img / Media / Font / XHR
- Search input

##### Waterfall Timeline
- Each row represents a network request:
  - Request name (truncated)
  - HTTP method
  - Status code (color-coded: 2xx green, 3xx blue, 4xx orange, 5xx red)
  - Type (script, stylesheet, image, etc.)
  - Size
  - Time (total duration)
  - Timeline bar:
    - Blocked (gray)
    - DNS (purple)
    - Connect (orange)
    - SSL (green)
    - Send (blue)
    - Wait (yellow)
    - Receive (green)
- Color coding for blocked requests:
  - Red bar with "Blocked" label
- Click row: Shows detail panel

##### Request Detail Panel
- Headers tab:
  - Request headers (table)
  - Response headers (table)
- Preview tab:
  - Response body (formatted if JSON/HTML)
- Timing tab:
  - Breakdown of timing phases

**Implementation Status:** ✅ Implemented

#### 8.1.2 Console Tab
**Purpose:** JavaScript console output
**Components:**

##### Console Output
- Each message:
  - Timestamp
  - Source (page / extension)
  - Level icon: log (i), warn (!), error (✕)
  - Message content
  - File:line link (clickable)
- Filter by level: All / Errors / Warnings / Info
- Search input

##### Console Input
- Single-line input at bottom
- "Execute" button or Enter
- History (↑/↓ arrows)

**Implementation Status:** ✅ Implemented

#### 8.1.3 Elements Tab
**Purpose:** DOM inspector
**Components:**
- DOM tree view (collapsible)
- Click element: Shows computed styles
- Edit attributes inline
- Right-click context menu:
  - Copy selector
  - Delete element
  - Edit as HTML

**Implementation Status:** ✅ Implemented (basic)

#### 8.1.4 Storage Tab
**Purpose:** View browser storage
**Sections:**
- Local Storage (per domain)
- Session Storage (per domain)
- Cookies (per domain)
- IndexedDB databases
- Web SQL databases

Each section:
- Tree view of domains
- Click domain: Shows key-value pairs
- Edit/Delete actions

**Implementation Status:** ✅ Implemented (basic)

#### 8.1.5 Device Tab
**Purpose:** Device emulation for responsive design
**Components:**
- Device selector dropdown:
  - iPhone 12 Pro
  - iPhone 14 Pro Max
  - Pixel 5
  - Samsung Galaxy S21
  - iPad Pro
  - Desktop (default)
- Custom resolution input
- User agent input
- Device pixel ratio slider
- Throttle network: Online / Offline / Slow 3G / Fast 3G / 4G
- Throttle CPU: No throttling / 4x slowdown / 6x slowdown

**Implementation Status:** ✅ Implemented (basic)

#### 8.1.6 Injection Tab
**Purpose:** Manage custom JS/CSS injections
**Components:**
- Domain selector dropdown
- JS textarea (monospace font)
- CSS textarea (monospace font)
- "Apply" button
- "Save to settings" checkbox
- Saved injections list (from settings)

**Implementation Status:** ✅ Implemented

### 8.2 Sidebar (Future Feature)
**Location:** Left side of content area
**Toggle:** Ctrl+Shift+B or View menu
**Width:** 260px

**Potential Contents:**
- Bookmarks tree
- Reading list
- Downloads manager
- History timeline

**Implementation Status:** ❌ Not planned for v1.0

---

## 9. Modals & Dialogs

### 9.1 Pairing Verification Modal
**Trigger:** Pairing across networks (relay-mediated)
**Purpose:** Verify pairing with verification code

**Components:**
- Heading: "Pair New Device"
- Device name input field
- Verification code display (6 digits, large monospace)
- Timer: "Expires in 5:00"
- "Confirm" button (disabled until codes match)
- "Cancel" button

**Implementation Status:** ⚠️ Backend implemented, UI basic

### 9.2 Password Generator Modal
**Trigger:** Command palette or credential edit

**Components:**
- Generated password display (large, monospace)
- Copy button
- Regenerate button
- Options:
  - Length slider (8-64 characters)
  - Include: Uppercase / Lowercase / Numbers / Symbols toggles
  - Exclude similar characters (i, l, 1, L, o, 0, O)
- "Use this password" button

**Passphrase Mode:**
- Word count slider (4-8 words)
- Separator: Space / Hyphen / Period / None
- Capitalize words toggle
- Generated passphrase display

**Implementation Status:** ✅ Implemented

### 9.3 Add Credential Modal
**Trigger:** Add button in password manager

**Components:**
- URL input
- Username input
- Password input (with reveal toggle)
- Generate button (opens generator)
- Save/Cancel buttons

**Implementation Status:** ⚠️ Partially implemented

### 9.4 Clear Browsing Data Modal
**Trigger:** Clear browsing data button in settings

**Components:**
- Time range dropdown
- Checkboxes:
  - Browsing history
  - Download history
  - Cached images and files
  - Cookies and other site data
  - Archived snapshots
- Estimated space to be cleared
- "Clear data" button (danger-styled)
- Cancel button

**Implementation Status:** ❌ Not implemented

### 9.5 Update Available Modal
**Trigger:** Update check found new version

**Components:**
- Heading: "Update Available"
- Current version → New version
- Changelog link
- "Update and Restart" button
- "Remind me later" button
- "Skip this version" button

**Implementation Status:** ❌ Not implemented (auto-update via background)

### 9.6 Recovery Phrase Modal
**Trigger:** Enable/view recovery phrase

**Components:**
- Warning: "Write this down and keep it safe. Ferrite cannot recover it."
- 24-word phrase (displayed once, numbered)
- "Copy to clipboard" button (with warning)
- "I've written it down" checkbox
- "Close" button (disabled until checkbox checked)

**Implementation Status:** ✅ Implemented (backend)

---

## 10. Context Menus

### 10.1 Page Context Menu
**Trigger:** Right-click on page

**Items:**
- Back (disabled if no back history)
- Forward (disabled if no forward history)
- Reload
- ---
- Save page as...
- Save page as (unsanitized)
- ---
- View page source
- Inspect element
- ---
- Archive this page
- ---
- Print...

**Implementation Status:** ⚠️ Partially implemented (OS default)

### 10.2 Link Context Menu
**Trigger:** Right-click on link

**Items:**
- Open link in new tab
- Open link in new window
- Open link in incognito window
- ---
- Copy link address
- Copy link text
- ---
- Save link as...
- ---
- Archive link target

**Implementation Status:** ⚠️ Partially implemented (OS default)

### 10.3 Image Context Menu
**Trigger:** Right-click on image

**Items:**
- Open image in new tab
- Copy image address
- Copy image
- Save image as...
- ---
- Search image with Google (or default engine)

**Implementation Status:** ⚠️ Partially implemented (OS default)

### 10.4 Text Selection Context Menu
**Trigger:** Right-click on selected text

**Items:**
- Copy
- Cut (if editable)
- Paste (if editable)
- ---
- Search "[selected text]" with [engine]
- ---
- Translate selection (future)

**Implementation Status:** ⚠️ Partially implemented (OS default)

### 10.5 Tab Context Menu
**Trigger:** Right-click on tab

**Items:**
- New tab to the right
- Reload
- Duplicate
- ---
- Pin tab
- Mute site
- ---
- Close tab
- Close other tabs
- Close tabs to the right
- ---
- Reopen closed tab

**Implementation Status:** ❌ Not implemented

---

## 11. Notifications

### 11.1 Toast Notifications
**Location:** Top-right corner, stacked vertically
**Duration:** 3 seconds (auto-dismiss)
**Max:** 3 visible at a time

**Types:**

#### 11.1.1 Success Toast
- Background: Surface L2
- Accent: Interactive accent (left border)
- Icon: Checkmark
- Message: (e.g., "Password copied to clipboard")

#### 11.1.2 Error Toast
- Background: Surface L2
- Accent: Danger (left border)
- Icon: X mark
- Message: (e.g., "Failed to load page")
- Duration: 5 seconds (longer)

#### 11.1.3 Warning Toast
- Background: Surface L2
- Accent: Warning (left border)
- Icon: Warning triangle
- Message: (e.g., "This site uses deprecated TLS 1.0")

#### 11.1.4 Info Toast
- Background: Surface L2
- Accent: Primary (left border)
- Icon: Info circle
- Message: (e.g., "Page archived successfully")

**Implementation Status:** ⚠️ Partially implemented (basic)

### 11.2 Badge Notifications
**Location:** On app icon (taskbar/dock)
**Purpose:** Show count of blocked trackers

**Implementation Status:** ❌ Not implemented

---

## 12. Keyboard Shortcuts

### 12.1 Navigation
| Shortcut | Action |
|----------|--------|
| Ctrl+L / Cmd+L | Focus omnibox |
| Ctrl+T / Cmd+T | New tab |
| Ctrl+W / Cmd+W | Close current tab |
| Ctrl+Shift+T / Cmd+Shift+T | Reopen closed tab |
| Ctrl+Tab | Next tab |
| Ctrl+Shift+Tab | Previous tab |
| Ctrl+1-9 | Jump to tab 1-9 |
| Alt+Left / Cmd+[ | Back |
| Alt+Right / Cmd+] | Forward |
| F5 / Ctrl+R | Reload |
| Ctrl+Shift+R | Hard reload (clear cache) |
| Home | Navigate to new tab page |

### 12.2 Omnibox
| Shortcut | Action |
|----------|--------|
| Enter | Navigate or search |
| Tab | Tab-to-search (if available) |
| ↓ | Next autocomplete suggestion |
| ↑ | Previous autocomplete suggestion |
| Escape | Clear input or defocus |

### 12.3 Developer Tools
| Shortcut | Action |
|----------|--------|
| F12 | Toggle DevTools |
| Ctrl+Shift+I / Cmd+Option+I | Toggle DevTools |
| Ctrl+Shift+J / Cmd+Option+J | Open DevTools to Console |
| Ctrl+Shift+C / Cmd+Option+C | Open DevTools to Elements |
| Ctrl+Shift+K | Focus console input |

### 12.4 Page Interaction
| Shortcut | Action |
|----------|--------|
| F | Toggle Vimium-style link hints |
| Ctrl+F / Cmd+F | Find in page |
| Ctrl+P / Cmd+P | Print |
| Ctrl+S / Cmd+S | Save page as |
| Ctrl++ / Cmd++ | Zoom in |
| Ctrl+- / Cmd+- | Zoom out |
| Ctrl+0 / Cmd+0 | Reset zoom |
| F11 | Toggle fullscreen |

### 12.5 Browser Features
| Shortcut | Action |
|----------|--------|
| Ctrl+Shift+A | Tab search overlay |
| Ctrl+Shift+P | Command palette |
| Ctrl+Shift+Delete | Clear browsing data |
| Ctrl+D | Bookmark current page |
| Ctrl+Shift+D | Bookmark all open tabs |

### 12.6 Link Hints (Vimium-style)
| Shortcut | Action |
|----------|--------|
| F | Show link hints |
| [hint letters] | Click link |
| Shift+[hint letters] | Open in new tab |
| Escape | Cancel hints |

**Implementation Status:** ✅ Mostly implemented

---

## 13. Backend Components (Not Visible)

### 13.1 Proxy Engine
- TCP listener on `127.0.0.1:FERRITE_PORT`
- HTTP/HTTPS request interception
- CONNECT tunneling for HTTPS
- Connection pooling
- Encrypted DNS resolver (DoH)

### 13.2 Privacy Engine
- adblock-rust integration
- Threat intelligence DB
- Request classification
- Cosmetic filtering
- Fingerprint spoofing

### 13.3 Archive Engine
- HTML sanitizer
- Zstd compression
- FTS5 indexing
- Snapshot storage
- 404 recovery

### 13.4 Vault Engine
- SQLCipher encrypted database
- OS keychain integration
- Hot cache (dashmap)
- Batch writer
- Recovery phrase handler

### 13.5 Credential Engine
- Password storage (AES-256-GCM field-level encryption)
- TOTP generator
- Password generator (CSPRNG)
- DOM scanner
- Breach checker

### 13.6 Sync Engine
- mDNS discovery
- Ed25519 identity
- mTLS transport
- CRDT merge engine
- Raft consensus (for live tabs)

### 13.7 Relay Server (Go)
- WebSocket signaling
- STUN service
- TURN relay
- Rate limiting
- Graceful shutdown

---

## 14. Implementation Priority

### Phase 1: Core Usability (Critical)
1. **Fix webview visibility** - Ensure content renders
2. **Fix omnibox input** - Text entry must work
3. **Complete new tab page** - Already good, ensure IPC works
4. **Basic tab management** - Already working

### Phase 2: Essential Features (High)
1. **Shield badge & dropdown** - Core privacy feature
2. **Autocomplete dropdown** - Critical for usability
3. **Settings page** - User configuration
4. **Password manager UI** - Key differentiator

### Phase 3: Polish & Power Features (Medium)
1. **DevTools panel** - Developer audience
2. **Archive browser** - Unique feature
3. **Visual polish** - Animations, icons
4. **Keyboard shortcuts** - Power users

### Phase 4: Advanced Features (Low)
1. **P2P sync UI** - Complex but optional
2. **E2EE backup UI** - Optional for most users
3. **Custom injections** - Power user feature
4. **Context menus** - Enhancement

---

## Summary Statistics

**Total UI Components:** 150+
**Internal Pages:** 6
**Modals/Dialogs:** 6
**Context Menus:** 5
**Keyboard Shortcuts:** 50+
**Backend Engines:** 7

**Current Implementation:**
- ✅ Fully implemented: ~60%
- ⚠️ Partially implemented: ~25%
- ❌ Not implemented: ~15%

**Most Critical Missing:**
1. Webview not visible (blocking issue)
2. Omnibox input not working (blocking issue)
3. Autocomplete dropdown not showing
4. Shield badge animation
5. Visual polish (icons, animations)
