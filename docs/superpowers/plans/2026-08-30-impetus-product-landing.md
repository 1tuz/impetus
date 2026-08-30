# Impetus Product Landing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Design and ship an English, responsive Impetus product landing page through Penpot, Astro, and GitHub Pages.

**Architecture:** Penpot is the visual source of truth for a compact token system, reusable components, and 1440/390 responsive screens. The repository gains an isolated static Astro site under `site/`; Astro renders all content at build time and small inline scripts handle only mobile navigation and command copying. GitHub Actions builds the site with the repository base path and deploys the generated static output.

**Tech Stack:** Penpot MCP, Astro, TypeScript, CSS, Node test runner, GitHub Pages Actions

**Spec:** `docs/superpowers/specs/2026-08-30-impetus-product-landing-design.md`

## Global Constraints

- All user-facing site copy and metadata are English.
- Do not add React, Tailwind, a UI framework, or an animation library.
- Do not change Rust runtime behavior.
- Do not imply that Impetus is a terminal emulator or invent product UI.
- Use `#51D6FF` as the only brand accent and keep it under 20 percent of the page surface.
- Support desktop 1440 px and mobile 390 px with a real mobile recomposition.
- Respect `prefers-reduced-motion` and visible keyboard focus.
- GitHub Pages assets and links must work below `/impetus/`.

---

### Task 1: Build and review the Penpot source of truth

**Files:**
- Create outside repository: Penpot file `Impetus — Product Landing`
- Export: `/tmp/impetus-penpot/landing-desktop.png`
- Export: `/tmp/impetus-penpot/landing-mobile.png`
- Export: `/tmp/impetus-penpot/design-system.png`

**Interfaces:**
- Consumes: copy, tokens, components, and section structure from the design spec.
- Produces: approved visual measurements and export references used by Tasks 3–5.

- [ ] **Step 1: Connect and inspect the Penpot file**

Run the local Penpot MCP server on ports 4400–4403, load
`http://localhost:4400/manifest.json`, connect the active file, then call
`high_level_overview` and inspect the current pages.

- [ ] **Step 2: Create tokens and library styles**

Create the seven color tokens, spacing values `4, 8, 12, 16, 24, 32, 48, 64,
72, 96, 112`, radii `6, 10`, Geist typography styles, and their semantic
aliases. Export and inspect `00 Design System` before continuing.

- [ ] **Step 3: Create reusable components**

Build Navigation, Button variants, Terminal Window, Event Row, Flow Node,
Capability Card, Copy Block, and Section Header. Use Flex or Grid for every
container and component instances for every repeated element.

- [ ] **Step 4: Assemble desktop and mobile screens**

Build `01 Landing Desktop 1440` and `02 Landing Mobile 390` one section at a
time. Export each section before building the next.

- [ ] **Step 5: Run the critique and correction pass**

Score hierarchy, product specificity, spacing, typography, color, and component
consistency from 0–2. Make targeted Penpot corrections until the score is at
least 10/12, then export final desktop and mobile PNGs.

### Task 2: Scaffold a tested static Astro site

**Files:**
- Create: `site/package.json`
- Create: `site/astro.config.mjs`
- Create: `site/tsconfig.json`
- Create: `site/tests/site.test.mjs`
- Create: `site/src/pages/index.astro`
- Create: `site/src/styles/global.css`

**Interfaces:**
- Consumes: repository name from `GITHUB_REPOSITORY` and local default `/`.
- Produces: `npm run check`, `npm run build`, and `site/dist/index.html`.

- [ ] **Step 1: Write the failing static contract test**

Create a Node test that reads `dist/index.html` and asserts the document uses
`lang="en"`, includes the approved headline and installer command, contains
anchors for `capabilities`, `architecture`, and `install`, and contains no
React runtime script.

- [ ] **Step 2: Verify the test fails before the site exists**

Run: `cd site && node --test tests/site.test.mjs`

Expected: FAIL because `dist/index.html` does not exist.

- [ ] **Step 3: Add Astro configuration and the semantic page skeleton**

Configure static output. Resolve `base` as `/impetus` when
`GITHUB_ACTIONS=true`, otherwise `/`. Add metadata, skip link, navigation,
seven semantic sections, and footer to `index.astro`.

- [ ] **Step 4: Add global tokens and accessibility foundations**

Implement the exact design tokens as CSS custom properties. Add self-hosted
Geist fonts, focus-visible styles, skip-link behavior, responsive type scales,
and the reduced-motion override.

- [ ] **Step 5: Install dependencies and prove the skeleton passes**

Run: `cd site && npm install && npm run build && node --test tests/site.test.mjs`

Expected: build succeeds and the static contract test passes.

### Task 3: Implement product-specific components and desktop composition

**Files:**
- Create: `site/src/components/Logo.astro`
- Create: `site/src/components/TerminalPreview.astro`
- Create: `site/src/components/ControlFlow.astro`
- Create: `site/src/components/CopyCommand.astro`
- Modify: `site/src/pages/index.astro`
- Modify: `site/src/styles/global.css`
- Modify: `site/tests/site.test.mjs`

**Interfaces:**
- Consumes: Penpot desktop measurements and the current CLI command names.
- Produces: static component markup with stable class names and accessible labels.

- [ ] **Step 1: Extend the test with product and accessibility assertions**

Assert the built HTML contains `aria-label="Impetus request control flow"`, a
copy button associated with the install command, a single `h1`, and the five
real command labels `doctor`, `create`, `prompt`, `stream`, and `approve`.

- [ ] **Step 2: Verify the new assertions fail**

Run: `cd site && npm run build && node --test tests/site.test.mjs`

Expected: FAIL on missing product components.

- [ ] **Step 3: Implement the components and desktop layout**

Create the wordmark, terminal transcript, request-control flow, and copyable
install block. Assemble the asymmetric hero, four-item capability bento,
architecture flow, CLI showcase, install section, and final action with the
Penpot desktop grid and spacing.

- [ ] **Step 4: Implement minimal interaction**

Use an inline module script to toggle the mobile navigation and copy the
installer command. Update an `aria-live="polite"` message to `Copied install
command.` after success and `Copy failed. Select the command manually.` after
failure.

- [ ] **Step 5: Build and verify the desktop contract**

Run: `cd site && npm run check`

Expected: Astro and static tests pass.

### Task 4: Match the mobile Penpot design and harden performance

**Files:**
- Modify: `site/src/styles/global.css`
- Modify: `site/src/components/TerminalPreview.astro`
- Modify: `site/src/components/ControlFlow.astro`
- Modify: `site/tests/site.test.mjs`

**Interfaces:**
- Consumes: Penpot mobile export and existing semantic component markup.
- Produces: responsive CSS without a client framework or horizontal page overflow.

- [ ] **Step 1: Add reduced-motion and responsive contract assertions**

Assert `global.css` contains `@media (prefers-reduced-motion: reduce)`, a mobile
breakpoint, `overflow-wrap`, and a focus-visible rule.

- [ ] **Step 2: Verify the CSS assertions fail**

Run: `cd site && npm run build && node --test tests/site.test.mjs`

Expected: FAIL on at least one missing mobile contract.

- [ ] **Step 3: Implement the 390 px recomposition**

Collapse navigation, stack the hero, prioritize the capability blocks, convert
the control flow to a vertical sequence, constrain terminal content, preserve
44 px tap targets, and keep commands locally scrollable without page overflow.

- [ ] **Step 4: Remove unnecessary visual and runtime weight**

Remove unused selectors, decorative effects, and scripts. Keep one hero reveal,
one cursor blink, and subtle state transitions; disable all three under reduced
motion.

- [ ] **Step 5: Build and verify the responsive contract**

Run: `cd site && npm run check`

Expected: all tests pass and the built JavaScript contains no framework runtime.

### Task 5: Add GitHub Pages deployment and browser verification

**Files:**
- Create: `.github/workflows/pages.yml`
- Modify: `site/tests/site.test.mjs`
- Modify: `README.md`

**Interfaces:**
- Consumes: `site/package-lock.json`, Astro static build, and GitHub repository metadata.
- Produces: Pages artifact from `site/dist` and a documented landing-page link.

- [ ] **Step 1: Add a failing workflow contract test**

Assert `.github/workflows/pages.yml` uses checkout, setup-pages, upload-pages-
artifact, and deploy-pages; builds from `site`; and grants only `contents: read`,
`pages: write`, and `id-token: write`.

- [ ] **Step 2: Verify the workflow assertion fails**

Run: `cd site && node --test tests/site.test.mjs`

Expected: FAIL because the Pages workflow does not exist.

- [ ] **Step 3: Implement the Pages workflow**

On pushes to `main` and manual dispatch, install locked Node dependencies,
build with `GITHUB_ACTIONS=true`, upload `site/dist`, and deploy through the
official Pages environment.

- [ ] **Step 4: Run desktop and mobile browser review**

Start the Astro preview, inspect 1440 px and 390 px viewports, then verify
overflow, navigation, install copy, hover, keyboard focus, reduced motion,
anchor links, and all asset responses. Correct every mismatch against the final
Penpot exports.

- [ ] **Step 5: Run final repository verification**

Run: `cd site && npm ci && npm run check`

Run: `task verify`

Run: `git diff --check`

Expected: all commands pass.

### Task 6: Commit, push, and enable auto-merge

**Files:**
- Modify: only files already listed in Tasks 1–5.

**Interfaces:**
- Consumes: passing verification and issue `#34`.
- Produces: a GitHub pull request configured to merge automatically after checks.

- [ ] **Step 1: Review the final diff and issue scope**

Confirm no Rust runtime files, credentials, generated `dist`, browser state, or
Penpot local state are included.

- [ ] **Step 2: Create issue-referenced commits**

Use English commit subjects no longer than 72 characters and include
`refs #34` or `closes #34`.

- [ ] **Step 3: Push the feature branch and create the pull request**

Push `feature/issue-34-product-landing`, create a PR whose body closes `#34`,
and request source-branch deletion after merge where supported.

- [ ] **Step 4: Enable squash auto-merge**

Enable GitHub auto-merge so the PR merges only after required checks pass.

- [ ] **Step 5: Verify remote status**

Confirm the PR reports auto-merge enabled and monitor checks until the PR merges
or a concrete failure needs correction.
