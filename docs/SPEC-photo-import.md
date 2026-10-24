# Spec: Import a recipe from photos

## Objective
Let a cook photograph a recipe (a cookbook page, a recipe card, a handwritten note, a
screenshot) and optionally the finished dish, and get a normalized recipe in the library
with a cover picture, in one step. Works from a phone through the PWA.

Decisions taken (2026-10-22): save immediately like other imports; several photos per
import; the model decides which photo (if any) is the cover; original photos are discarded.

## User flow
1. Import dialog gains a **From photo** tab: `<input type="file" accept="image/*"
   capture="environment" multiple>`, thumbnails of the chosen photos, the existing
   "make public" toggle, and an **Import photos** button.
2. The client posts the files as multipart. While waiting (typically 5–20 s) the dialog
   shows a progress state; errors from the API are rendered inline like today.
3. On success the dialog closes and the new recipe opens. Mistakes are fixed in the
   editor (RN-35).

## API
`POST /api/v1/import/photo` — `multipart/form-data`
- `photos` (repeatable): JPEG/PNG/WebP/HEIC-converted images, 1–6 files, 20 MB each max.
- `is_public` (optional text field, `true`/`false`).
- `201` → the stored `Recipe`. `400` → no photos, too many, or unreadable image data.
  `422` → the model could not find a recipe in the photos (message names the problem).
  `502` → model failure. `503` → no `LLM_MODEL` configured or the model cannot read images.

## Server pipeline (`recipe-scraper` + `recipe-mcp::ImportService`)
1. **Normalize photos** (`images.rs`): decode with the `image` crate, auto-orient from
   EXIF, downscale so the longest side is ≤ 1600 px, re-encode as JPEG quality 85.
   Keeps the request to the model small and bounds token cost. Rejects non-images early.
2. **One vision call** (`llm.rs`): a `CompletionRequest` whose user message carries each
   normalized photo as `UserContent::Image` (media type sniffed) followed by a short
   text prompt, with `output_schema` for `PhotoExtraction`:
   ```text
   PhotoExtraction {
     recipe: ExtractedRecipeDraft,            // same draft as text import
     photos: [{ index, kind: recipe_text|dish|both|unrelated, notes }],
     unreadable: [String],                    // parts the model could not read
     language: String                         // BCP-47 of the source text
   }
   ```
   Preamble extends the text-extraction preamble with: transcribe faithfully in the
   source language, convert units to metric, do not invent steps for unreadable parts,
   classify each photo. Parsing reuses the lenient reader (`parse_recipe_reply`) on the
   `recipe` member so sloppy field names do not fail the import.
3. **Persist**: `ImportService::import_photos(photos, is_public)` creates the recipe,
   then picks the first photo whose `kind` is `dish` or `both` and stores it as the cover
   through the existing WebP path (`set_cover`). Text-only photos are never covers.
4. **Capability check**: at startup `extractor_from_env` records whether the catalog
   lists `image` among the model's input modalities (`ModelSpec`); if not, the endpoint
   answers 503 with "model `<ref>` cannot read images; set LLM_MODEL to a vision model".

`RecipeExtractor` gains `extract_from_photos(&[NormalizedPhoto]) -> PhotoExtraction`;
the heuristic extractor returns `LlmUnavailable`.

## MCP
`import_recipe` gains an optional `photo_paths: [String]` argument (stdio clients can
read local files; HTTP clients should use the REST endpoint). Same pipeline.

## Privacy and limits
- Photos are sent to the configured LLM vendor. The UI states this once above the picker.
- Nothing from the photos is kept except the derived recipe and the WebP cover.
- Hard limits: 6 photos, 20 MB per file, 1600 px after normalization. Timeout 120 s.

## Testing
- Unit: photo normalization (orientation, downscale, rejects garbage); parsing of a
  `PhotoExtraction` reply with missing/renamed fields; cover selection rules
  (no dish photo → no cover; `both` counts as dish).
- Integration: endpoint with a fake extractor that records the photos it received
  (count, max dimension) and returns a fixed extraction; 400/422/503 paths.
- Manual: a real cookbook page plus a plate photo through the UI with MiniMax-M3.

## Out of scope for the first version
Cropping the dish out of a mixed photo, OCR without an LLM, keeping originals,
re-running extraction later, batch import of a whole photo album.
