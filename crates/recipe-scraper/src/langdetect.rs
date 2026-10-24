//! Offline language detection for recipe text: script signals for Cyrillic,
//! stopword scoring for the common Latin-script languages. Returns `None`
//! whenever the evidence is thin, so callers can fall back to a model.

/// Detect the language of recipe text as a BCP-47 primary subtag.
pub fn detect_language(text: &str) -> Option<String> {
    let (latin, cyrillic, other) = text.chars().filter(|c| c.is_alphabetic()).fold((0usize, 0usize, 0usize), |(l, c, o), ch| {
        if ch.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&ch) {
            (l + 1, c, o)
        } else if ('\u{0400}'..='\u{04FF}').contains(&ch) {
            (l, c + 1, o)
        } else {
            (l, c, o + 1)
        }
    });
    let total = latin + cyrillic + other;
    if total < 12 {
        return None;
    }
    if other * 2 > total {
        return None;
    }
    if cyrillic > latin {
        return Some(detect_cyrillic(text).to_string());
    }
    detect_latin(text).map(str::to_string)
}

fn detect_cyrillic(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    let has = |chars: &[char]| lower.chars().any(|c| chars.contains(&c));
    if has(&['ї', 'є', 'ґ', 'і']) {
        return "uk";
    }
    if has(&['ы', 'ъ', 'э', 'ё']) {
        return "ru";
    }
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).collect();
    let uk = ["та", "що", "це", "або", "ще", "їх", "його", "поки"].iter().filter(|w| words.contains(w)).count();
    let ru = ["что", "это", "или", "еще", "его", "пока", "затем", "очень"].iter().filter(|w| words.contains(w)).count();
    if uk > ru { "uk" } else { "ru" }
}

const STOPWORDS: &[(&str, &[&str])] = &[
    ("en", &["the", "and", "with", "for", "until", "then", "add", "into", "of", "to", "a", "in", "minutes", "cook", "heat", "stir", "over", "about", "pinch", "serve"]),
    ("nl", &["de", "het", "een", "en", "van", "met", "in", "op", "voor", "tot", "dan", "kook", "snijd", "bak", "voeg", "toe", "minuten", "breng", "aan", "roer", "alles", "fijn"]),
    ("de", &["die", "der", "das", "und", "mit", "den", "dem", "in", "zu", "bis", "dann", "kochen", "minuten", "ein", "eine", "vermischen", "hinzufügen", "etwa", "dazu"]),
    ("fr", &["le", "la", "les", "des", "et", "avec", "dans", "pour", "puis", "une", "un", "faire", "de", "du", "au", "aux", "à", "ajouter", "cuire", "minutes"]),
    ("es", &["el", "la", "los", "las", "y", "con", "en", "para", "hasta", "luego", "una", "un", "de", "del", "freír", "mezclar", "huevos", "añadir", "cocinar", "minutos"]),
    ("it", &["il", "la", "le", "gli", "e", "con", "in", "per", "fino", "poi", "una", "un", "di", "del", "della", "cuocere", "mescolare", "aggiungere", "minuti", "sale"]),
];

fn detect_latin(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).collect();
    let mut scores: Vec<(&'static str, usize)> = STOPWORDS
        .iter()
        .map(|(lang, list)| (*lang, words.iter().filter(|w| list.contains(w)).count()))
        .collect();
    scores.sort_by_key(|s| std::cmp::Reverse(s.1));
    let (best, best_score) = scores[0];
    let runner_up = scores.get(1).map(|s| s.1).unwrap_or(0);
    (best_score >= 2 && best_score > runner_up).then_some(best)
}
