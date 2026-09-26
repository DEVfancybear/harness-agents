#!/bin/bash
set -e

echo "📦 Bundling React app to single HTML artifact..."

# Check if we're in a project directory
if [ ! -f "package.json" ]; then
  echo "❌ Error: No package.json found. Run this script from your project root."
  exit 1
fi

# Check if index.html exists
if [ ! -f "index.html" ]; then
  echo "❌ Error: No index.html found in project root."
  echo "   This script requires an index.html entry point."
  exit 1
fi

# pnpm 10 and later do not run dependency build scripts until they are approved,
# and newer pnpm fails the install over it (ERR_PNPM_IGNORED_BUILDS). Parcel's
# native helpers need theirs, so approve exactly those - in both the older and
# the newer setting's spelling - unless the project already configures pnpm.
if [ ! -f "pnpm-workspace.yaml" ] || grep -q "set this to true or false" pnpm-workspace.yaml; then
  cat > pnpm-workspace.yaml << 'EOF'
allowBuilds:
  '@parcel/watcher': true
  '@swc/core': true
  lmdb: true
  msgpackr-extract: true
onlyBuiltDependencies:
  - '@parcel/watcher'
  - '@swc/core'
  - lmdb
  - msgpackr-extract
EOF
fi

# Install bundling dependencies
echo "📦 Installing bundling dependencies..."
pnpm add -D parcel @parcel/config-default parcel-resolver-tspaths html-inline

# Create Parcel config with tspaths resolver
if [ ! -f ".parcelrc" ]; then
  echo "🔧 Creating Parcel configuration with path alias support..."
  cat > .parcelrc << 'EOF'
{
  "extends": "@parcel/config-default",
  "resolvers": ["parcel-resolver-tspaths", "..."]
}
EOF
fi

# Clean previous build
echo "🧹 Cleaning previous build..."
rm -rf dist bundle.html

# Build with Parcel
echo "🔨 Building with Parcel..."
pnpm exec parcel build index.html --dist-dir dist --no-source-maps

# Inline everything into single HTML
echo "🎯 Inlining all assets into single HTML file..."
pnpm exec html-inline dist/index.html > bundle.html

# Get file size
FILE_SIZE=$(du -h bundle.html | cut -f1)

echo ""
echo "✅ Bundle complete!"
echo "📄 Output: bundle.html ($FILE_SIZE)"
echo ""
echo "📍 Full path: $(pwd -W 2>/dev/null || pwd)/bundle.html"
echo "To test locally: open bundle.html in your browser"