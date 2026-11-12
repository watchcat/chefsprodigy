# Video page fixtures

Trimmed copies of YouTube watch pages and TikTok video pages fetched on 2026-11-12 with the
importer's own `USER_AGENT`. The page structure, the script framing and the field names are
the real ones; every title, caption, creator, id and URL is a sample value, and the parts the
readers never look at are cut. `youtube_unavailable.html` and `tiktok_unavailable.html` keep
the status the platforms answered for a video that does not exist, and
`youtube_bot_check.html` the one YouTube answered a hosting provider's address for a public
video ("sign in to confirm you're not a bot", in the address's language).

Real pages change without notice: `cargo test -p recipe-scraper --test social_live -- --ignored`
reads live ones (docs/SPEC-video-import.md).
