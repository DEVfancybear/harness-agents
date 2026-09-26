---
name: web-artifacts-builder
version: 33375500bcea
description: Suite of tools for creating elaborate, multi-component HTML artifacts using modern frontend web technologies (React, Tailwind CSS, shadcn/ui), bundled into one self-contained HTML file. Use for complex artifacts requiring state management, routing, or shadcn/ui components - not for simple single-file HTML/JSX artifacts.
license: Complete terms in LICENSE.txt
---

# Web Artifacts Builder

To build powerful frontend artifacts, follow these steps:
1. Initialize the frontend repo using `<skill-directory>/scripts/init-artifact.sh`
2. Develop your artifact by editing the generated code
3. Bundle all code into a single HTML file using `<skill-directory>/scripts/bundle-artifact.sh`
4. Give the user the bundled file
5. (Optional) Test the artifact

**Stack**: React 18 + TypeScript + Vite + Parcel (bundling) + Tailwind CSS + shadcn/ui

**Requirements**: Bash (Git Bash on Windows), Node.js 18+, and network access for `pnpm` to download packages. The scripts install `pnpm` with npm when it is missing.

## Design & Style Guidelines

VERY IMPORTANT: To avoid what is often referred to as "AI slop", avoid using excessive centered layouts, purple gradients, uniform rounded corners, and Inter font.

## Quick Start

### Step 1: Initialize Project

Run the initialization script from the directory the project should live in:
```bash
bash <skill-directory>/scripts/init-artifact.sh <project-name>
cd <project-name>
```

On Windows, create the project in a short directory (for example `C:/work`): the script refuses a project path longer than 110 characters, because the bundler fails once `node_modules` paths pass Windows' 260-character limit.

This creates a fully configured project with:
- ✅ React + TypeScript (via Vite)
- ✅ Tailwind CSS 3.4.1 with shadcn/ui theming system
- ✅ Path aliases (`@/`) configured
- ✅ 40+ shadcn/ui components pre-installed
- ✅ All Radix UI dependencies included
- ✅ Parcel configured for bundling (via .parcelrc)
- ✅ Node 18+ compatibility (auto-detects and pins Vite version)

### Step 2: Develop Your Artifact

To build the artifact, edit the generated files. See **Common Development Tasks** below for guidance.

### Step 3: Bundle to Single HTML File

To bundle the React app into a single HTML artifact, run from the project root:
```bash
bash <skill-directory>/scripts/bundle-artifact.sh
```

This creates `bundle.html` - a self-contained artifact with all JavaScript, CSS, and dependencies inlined. It opens in any browser with no server and no network.

**Requirements**: Your project must have an `index.html` in the root directory.

**What the script does**:
- Approves the build scripts of Parcel's native helpers for pnpm (newer pnpm refuses to install without it)
- Installs bundling dependencies (parcel, @parcel/config-default, parcel-resolver-tspaths, html-inline)
- Creates `.parcelrc` config with path alias support
- Builds with Parcel (no source maps)
- Inlines all assets into single HTML using html-inline

### Step 4: Give the User the Artifact

Finally, tell the user the full path of `bundle.html` (the script prints it) so they can open it in a browser. If they ask you to open it, use the platform's opener: `start "" bundle.html` on Windows, `open bundle.html` on macOS, `xdg-open bundle.html` on Linux.

### Step 5: Testing/Visualizing the Artifact (Optional)

Note: This is a completely optional step. Only perform if necessary or requested.

To test/visualize the artifact, use available tools (including other skills or browser automation such as Playwright or Puppeteer). In general, avoid testing the artifact upfront as it adds latency between the request and when the finished artifact can be seen. Test later, after presenting the artifact, if requested or if issues arise.

## Common Development Tasks

- Put the app in `src/App.tsx`; import components as `import { Button } from '@/components/ui/button'`.
- For several screens, keep them as components and switch with state: the bundle is one HTML file, so there is no server-side routing.
- Run `pnpm dev` to preview while developing, then bundle again after every change.

## Reference

- **shadcn/ui components**: https://ui.shadcn.com/docs/components
