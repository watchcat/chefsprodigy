use recipe_scraper::sanitize::html_to_markdown;

#[test]
fn strips_noise_tags_and_converts_to_markdown() {
    let html = r#"<html><head><style>.x{}</style><script>var secret = 1;</script></head>
      <body><nav>Menu</nav><header>Site header</header>
      <main><h1>Garlic Noodles</h1><p>Serves <strong>2</strong>.</p>
      <ul><li>200 g noodles</li><li>3 cloves garlic</li></ul>
      <svg><text>icon</text></svg><aside>Ad</aside></main>
      <footer>Footer</footer></body></html>"#;
    let md = html_to_markdown(html);
    assert!(md.contains("# Garlic Noodles"), "{md}");
    assert!(md.contains("200 g noodles"), "{md}");
    assert!(md.contains("**2**"), "{md}");
    for noise in [
        "secret",
        ".x{}",
        "Menu",
        "Site header",
        "Footer",
        "icon",
        "Ad",
    ] {
        assert!(!md.contains(noise), "noise {noise:?} leaked into: {md}");
    }
}

#[test]
fn collapses_excess_blank_lines() {
    let md = html_to_markdown("<p>a</p><br><br><br><p>b</p>");
    assert!(!md.contains("\n\n\n"), "{md:?}");
}
