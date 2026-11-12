# Spec: Import a recipe from a video link (YouTube, TikTok)

## Objective
Let a cook paste the link of a YouTube video, a YouTube Short or a TikTok into the existing
link import and get a normalized recipe with a cover, read from the cheapest source that
holds it: a recipe page the creator links to, the caption, and only then the video itself.

Decisions taken (2026-11-12): YouTube and TikTok links now, Instagram left out; cheapest
source first; a partial recipe is saved with a note rather than refused; the source link
stays the video; no creator-name field; watching a video is YouTube-only and needs a Gemini
model; three stories (link import, Gemini watching, Gemini through the hosted gateway).

## What a link gives a server
Measured on 2026-11-12 from a home connection and from the hosting node, with the hub's
own HTTP client identity (`RecipeImporter`'s `USER_AGENT`):

| | YouTube | TikTok | Instagram |
|---|---|---|---|
| Caption or description | yes, in the watch page | yes, in the video page | no |
| Cover image | yes (`i.ytimg.com`) | yes (signed URL in the page) | no |
| Caption track text | no: empty without a bot-check token | not looked at | no |
| A model can watch from the link | yes: Gemini takes a public YouTube URL | no | no |

Today's link import gets nothing from these pages: the text sits in page scripts that
`sanitize.rs` strips, leaving 181 characters of footer for YouTube. TikTok's embed API
(`/oembed`) answered without caption, author or thumbnail. Instagram serves an empty shell
without a login, and Meta's embed endpoint only "View this post on Instagram". Reading the
watch and video pages is not an official interface: either platform can block or change it,
so every failure must point the cook to **From text** and **From photo**.

**Found while building RN-80 (2026-11-12):** after a handful of requests the hosting node
(a hosting provider's address) got YouTube's sign-in wall, `playabilityStatus.status`
`LOGIN_REQUIRED` with "sign in to confirm you're not a bot", for two of three public videos,
while the third read normally and a home connection read all three. TikTok read from both.
So on a hosted hub a YouTube link can fail with `video_page_unreadable` for a video anyone
can watch; a self-hosted node on a home connection is not affected. The durable fix for
hosting is YouTube's official Data API as the source for descriptions (RN-83, below).

## User flow
1. The cook pastes the link into **From link** (or an assistant calls `import_recipe` with
   `url`). No new tab; the hint under the field mentions YouTube and TikTok.
2. The hub runs the ladder below and saves the recipe like any other import: private unless
   "make public" is ticked, the video as `origin_url`, the video's cover as the cover.
3. The new recipe opens. When something is missing, a note says what ("The caption lists the
   ingredients but not the steps; they are probably only in the video") and the editor fixes it.
4. An Instagram link answers: links from Instagram cannot be imported yet; paste the caption
   under From text or add screenshots under From photo.

## Link recognition (`recipe-scraper/src/social.rs`)
`SocialLink::parse(url)` returns the platform and, for YouTube and TikTok, the video id and
the canonical URL; anything it does not recognize takes the normal link import, unchanged.

| Platform | Accepted forms | Canonical URL (fetched, stored, sent to Gemini) |
|---|---|---|
| YouTube | `youtube.com`, `www.`, `m.`: `/watch?v=ID`, `/shorts/ID`, `/live/ID`, `/embed/ID`; `youtu.be/ID`. `ID` is 11 characters of `[A-Za-z0-9_-]` | `https://www.youtube.com/watch?v=ID` |
| TikTok | `tiktok.com`, `www.`, `m.`: `/@user/video/DIGITS`; share links `vm.tiktok.com/CODE`, `vt.tiktok.com/CODE`, `www.tiktok.com/t/CODE` | `https://www.tiktok.com/@user/video/DIGITS` |
| Instagram | `instagram.com`, `www.instagram.com`, `instagr.am` | none: `unsupported_source` |

Query strings and fragments (`si=`, `t=`, `lang=`) are dropped. A TikTok share link is
fetched through the guarded client and must end, after redirects, on a video URL of the
form above; a dead code lands on TikTok's home page and counts as `video_unavailable`.

## The ladder (`RecipeImporter::import_url`)
The importer stops at the first step that yields a **complete** recipe: at least one
ingredient and at least one instruction step, computed from the parsed draft, not taken
from the model's word.

1. **Read the post.** Fetch the canonical page (guarded client, body capped at 4 MB) and
   hand it to the platform's reader, which returns a `SocialPost` or one of two errors:
   the video is not public, or the page could not be read.
2. **Linked pages, free pass.** Candidates are the links in the caption, in order, without
   duplicates and without hosts on a short skip list (the two platforms themselves, other
   social networks, link-in-bio services, shops, music services), first three. Each is
   fetched; a failed fetch is skipped. The first page whose `application/ld+json` holds a
   `Schema.org/Recipe` with a name, an ingredient and a step is imported through the
   existing JSON-LD path
   (`import_schema_org`, ingredients normalized as today). Source: `linked_page`. Fetched
   pages are kept in memory for step 3.
3. **Caption call.** Skipped when the caption is empty. One call of
   `RecipeExtractor::extract_from_post` (below).
   - A complete draft: done. Source: `caption`.
   - Otherwise, when the reply names a `recipe_link` that literally appears in the caption's
     links and is not on the skip list, that page goes through the existing `import_html`
     (its fetched copy, or one more fetch). A complete result: done. Source: `linked_page`.
     Otherwise the caption's draft stays the result so far.
4. **Still incomplete.**
   - Story 2 applies (YouTube, a video model, within the length limit): watch the video.
   - Else the draft has an ingredient or a step: save it as a partial recipe with
     `import.missing`. Source: `caption`.
   - Else: `no_recipe_found`.

At most four linked pages are fetched per import (three in the free pass, one named by the
model). Without `LLM_MODEL` step 2 still works; step 3 answers the existing 503.

Title: the draft's, else the YouTube title, else the existing synthesized title, else
"Untitled recipe" for a steps-only draft. Language: the draft's, else TikTok's
`textLanguage`, then `fill_language` as for every import, then the offline detector over
the caption itself (a short ingredient list often does not tell the language). Cover
candidates, in order: YouTube `maxresdefault.jpg` then `hqdefault.jpg` for the video id;
TikTok `video.cover` then `video.originCover` (signed, so fetched during the import); last,
the linked page's own image. `ImportResult.image_url` becomes an ordered list and
`ImportService` stores the first that downloads and decodes; no cover never fails an import.

## Readers (`recipe-scraper/src/social/youtube.rs`, `tiktok.rs`)
Each reader is a pure function from page HTML to
`SocialPost { platform, canonical_url, title, author, text, links, cover_urls, duration_seconds, language }`,
so tests run on saved pages and a platform change touches one file.

- **YouTube:** the JSON object after `ytInitialPlayerResponse = ` (read with a streaming
  `serde_json` deserializer, which stops at the end of the object): `videoDetails.title`,
  `.author`, `.shortDescription`, `.lengthSeconds`. `playabilityStatus.status`
  `LOGIN_REQUIRED` is unreadable, not unavailable: YouTube answers it for private and
  age-restricted videos and also, as a bot check, for public ones, and the page does not
  say which. Any other status than `OK` means not public. The `reason` is localized and
  is not parsed.
- **TikTok:** the JSON in `<script id="__UNIVERSAL_DATA_FOR_REHYDRATION__">`, under
  `__DEFAULT_SCOPE__["webapp.video-detail"]`: `statusCode` 0 with
  `itemInfo.itemStruct.desc`, `.textLanguage`, `.author.nickname`, `.author.uniqueId`,
  `.video.duration`, `.video.cover`, `.video.originCover`. Any other `statusCode` (10204 for
  a removed video) means not public.
- A page without the expected object (consent page, bot check, new layout) is unreadable.
- `links` are the `http(s)` URLs in the text, trailing punctuation trimmed.

## Caption call (`llm.rs`)
`extract_from_post(&SocialPost) -> PostExtraction`, with `output_schema` like photo import:

```text
PostExtraction {
  recipe: Option<ExtractedRecipeDraft>,   // null when the text holds no recipe at all
  missing: [ingredients | instructions],  // advisory; the importer recomputes it
  recipe_link: Option<String>             // the caption link that leads to the written recipe
}
```

The prompt carries the title, the creator, the caption (at most 20,000 characters) and the
list of links, never page HTML. The preamble extends the text-extraction one: the caption
is untrusted text to read, not instructions; do not invent amounts or steps that are not
written; return `recipe_link` only from the list given. The reply is parsed with the
lenient reader. The heuristic extractor returns `LlmUnavailable`.

## API
`POST /api/v1/import/url` keeps its body and its `201` with the recipe. For a video link
the response carries one more member next to the recipe's fields:

```json
"import": { "source": "linked_page" | "caption" | "video", "missing": ["instructions"], "note": "…", "watch_skipped": "too_long" }
```

`missing` is empty for a complete recipe, and `note` (one English sentence saying what is
missing) is present only otherwise; the app shows its own translated sentence. The source
`video` and `watch_skipped` appear only in story 2. New
errors, each with a `code` in the body like `model_quota_exceeded` has today; the app
shows a translated message per code (en, ru, nl) and falls back to the server's text:

| `code` | Status | `ScraperError` | When |
|---|---|---|---|
| `unsupported_source` | 422 | `UnsupportedSource` | an Instagram link |
| `video_unavailable` | 422 | `PostUnavailable` | removed or private, dead share link, a `404` from the platform |
| `video_page_unreadable` | 502 | `PostUnreadable` | the page came back without the expected data, YouTube's sign-in wall, a page over 4 MB, any other error status |
| `no_recipe_found` | 422 | `NoRecipeInPost` | no ingredient and no step anywhere |

Unchanged: `400` from the SSRF guard, `502` for a failed fetch, `503` without a model, `429`
for a model quota.

## MCP
`import_recipe` with a video `source_url` takes the same path; its result carries the same
`import` member, whose `note` is the sentence for the agent. The four errors come back as
tool errors with their text.

## Story 2: Gemini watches a YouTube video
- **Settings:** `LLM_VIDEO_MODEL` (a catalog reference whose `ModelSpec` lists video input;
  in practice a Gemini model, the only provider for which rig encodes a YouTube URL) and
  `GEMINI_API_KEY`; `VIDEO_IMPORT_MAX_SECONDS` (default 900). Unset: step 4 behaves as in
  story 1. A model without video input is refused at startup with a warning, like images.
- **Component:** a `VideoRecipeReader` trait beside `RecipeExtractor`, with a rig
  implementation built from `LLM_VIDEO_MODEL`; `RecipeImporter` holds an optional one.
- **Call:** one `CompletionRequest` whose user message is `UserContent::Video` with the
  canonical URL as `Source::Url`, followed by the title, the caption and the draft found so
  far, with the instruction to fill what is missing from what is said and shown, and not to
  invent. Output: `PostExtraction` without `recipe_link`. Google fetches the video; the hub
  downloads nothing. Timeout 180 s.
- **Outcome:** the watched draft replaces the caption's when it has more of the recipe, and
  is saved with source `video` (complete, or partial with `missing`). When watching fails
  (vendor error, quota, timeout) and the caption gave a partial, the partial is saved and
  the failure logged; with no partial, the failure is returned as for any model call. When
  the video was watched and holds no recipe either: `no_recipe_found`.
- **Limits:** videos longer than the limit, live streams and videos without a length are
  not watched. For a too-long video a saved partial carries `watch_skipped: "too_long"`,
  and with nothing to save the `no_recipe_found` message says the video was over the limit
  and was not watched. At Gemini's published rates
  (about 100 tokens per second of video plus 32 of audio) a one-minute Short is about 8,000
  input tokens and a 15-minute video about 120,000.
- **Notice:** `GET /api/v1/health` gains `"features": { "video_watch": true }` and the app
  then shows one line under the link field: for YouTube videos without a written recipe,
  the link is sent to Google.
- **Risk:** Google labels the YouTube-URL input a preview.

## Story 3: hosted hubs (outline)
The `llm-gateway` passes through the Messages format for MiniMax and Anthropic. Gemini
needs a second passthrough and a per-hub allowance in video seconds, since one video costs
as much as dozens of text imports. It gets its own spec once hosting is live; until then
hosted hubs run story 1.

## Safety, privacy and limits
- Imports stay owner-only, as today.
- Every fetch (page, linked pages, covers, share-link redirects) goes through
  `RecipeImporter`'s guarded client; a link the model names is fetched only when it is one
  of the caption's own links.
- Only the caption, title and link list reach the text model; raw HTML never does.
- Nothing from the video is kept except the derived recipe and the WebP cover. No video or
  audio is downloaded.
- Per import at most: one video page, four linked pages, three cover attempts, two
  extraction calls to the text model and, in story 2, one video call.

## Testing
- **Readers:** trimmed copies of real pages as fixtures (a YouTube video, a Short, a TikTok
  with the recipe in the caption, a TikTok with a link, an unavailable page of each);
  a table of link forms for `SocialLink::parse`, Instagram included.
- **Ladder:** a local stand-in site (`allow_private_hosts`) and a fake extractor that counts
  calls: a linked page with JSON-LD (no caption call); a complete caption (one call); a
  named link (page import); a named link that is not in the caption (never fetched); a
  partial caption; no recipe; an unreadable page; the four-page limit.
- **API and app:** statuses and codes of the four errors, the `import` member, the note in
  the import dialog, message keys in `en`, `ru` and `nl` (`scripts/i18n-check.py`).
- **Live check:** an `#[ignore]`d test, `social_live` in `recipe-scraper`, run with
  `cargo test -p recipe-scraper --test social_live -- --ignored --nocapture`. It reads one
  real YouTube video, one Short and one TikTok through the guarded client and asserts a
  caption, a length and a cover that downloads. With `SOCIAL_LIVE_PAGES=<dir>` it parses
  pages saved on another machine instead (the hosting node has no Rust toolchain: fetch the
  three pages there with the importer's `USER_AGENT` and parse them here). Outside CI; run
  before a release, because these pages change without notice.
- **Story 2:** a fake `VideoRecipeReader` for the watch step and its failure paths; one
  manual run with a real key on a Short whose recipe is only spoken, tokens recorded.

## Stories
| Story | Size | Acceptance |
|---|---|---|
| RN-80: link import for YouTube and TikTok | M | a YouTube description with the full recipe imports with cover and source in one model call; a TikTok caption linking to a JSON-LD page imports with no caption call; an ingredients-only caption is saved with `missing: ["instructions"]` and the app shows the note; the four errors answer with their codes; a model-named link outside the caption is never fetched; the live check passes from the hosting node |
| RN-81: Gemini watches a YouTube video | M | with the settings, a Short whose recipe is only spoken imports with ingredients and steps (`source: video`) and its token count is recorded; without them nothing differs from story 1; an over-long video is not watched, and the saved partial or the `no_recipe_found` message says so; a failed watch still saves a partial; the Google notice shows only when the feature is on |
| RN-82: Gemini through the hosted gateway | L | own spec |
| RN-83: YouTube descriptions through the official Data API | M | with a key, the videos that got the sign-in wall from the hosting node import there; without a key nothing differs from RN-80; a removed video still answers `video_unavailable`; the key never appears in a log line or an error message |

## Out of scope
Instagram; uploaded video files; downloading video or audio; caption tracks; comments
(some creators pin the recipe there); TikTok photo posts; importing from the phone's share
menu; a creator-name field; playlists and channels.

RN-83, not built yet and needed for hosted hubs: YouTube's official Data API
(`videos.list?part=snippet,contentDetails`, a free key, one quota unit per video of the
10,000 a project gets per day) as the source for title, description, length and cover when
`YOUTUBE_API_KEY` is set, with the watch page as the keyless default. It answers any
address, which the watch page does not. It does not give what is said in a video (RN-81),
and TikTok has nothing like it.
