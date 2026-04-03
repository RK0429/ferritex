# FTX-ASSET-BUNDLE-001 Archive Contract

## Archive

- Archive name: `FTX-ASSET-BUNDLE-001`
- Purpose: Provide self-contained TeX assets (classes, packages, fonts) for bundle-only compilation without a TeX Live runtime

## Manifest Format

The archive root must contain `manifest.json` with these fields:

- `name`
- `version`: CalVer `YYYY.MM.DD`
- `min_ferritex_version`: SemVer
- `format_version`: integer, currently `1`
- `asset_index_path`
- `description` (optional): human-readable description of the bundle
- `content_policy` (optional): summary of content integrity constraints

## Asset Index Format

The manifest must point to `asset-index.json`, which contains these sections:

- `tex_inputs`
- `packages`
- `opentype_fonts`
- `tfm_fonts`
- `default_opentype_fonts`

## Versioning Policy

- `format_version` is bumped on breaking schema changes
- `version` tracks bundle content updates
- `min_ferritex_version` is the minimum Ferritex version that can read this bundle format

## Content Policy

- All paths in `asset-index.json` must be relative to the bundle root
- Parent directory traversal (`..`) is forbidden
- Every referenced path must exist inside the bundle root

## Validation

Ferritex validates the following on bundle load:

- Manifest schema
- Format-version compatibility
- Ferritex-version compatibility
- All asset paths referenced by the bundle

## Current Scope

This fixture currently covers the layout-core subset:

- `article.cls` equivalent
- `report.cls` equivalent
- `book.cls` equivalent
- `letter.cls` equivalent
- Stub packages required for the bounded proof wave
- Computer Modern font metrics/assets needed by the bundle-only bootstrap path
