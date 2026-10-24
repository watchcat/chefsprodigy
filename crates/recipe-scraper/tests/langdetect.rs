use recipe_scraper::langdetect::detect_language;

#[test]
fn detects_dutch_english_russian_ukrainian_from_recipe_text() {
    let nl = "Torchiette met crispy Parmaham. Breng ruim water aan de kook voor de pasta. Snijd de paprika in blokjes en bak de sjalot met knoflook.";
    let en = "Garlic butter shrimp. Melt the butter in a pan, add the garlic and cook for one minute, then add the shrimp and a pinch of salt.";
    let ru = "Пряные цукини стир-фрай. Прогрейте перец на сухой сковороде около 1 минуты, затем нарежьте цукини и обжарьте с чесноком.";
    let uk = "Борщ з квасолею. Наріжте буряк, додайте капусту і варіть на повільному вогні, поки овочі не стануть м'якими.";
    assert_eq!(detect_language(nl).as_deref(), Some("nl"));
    assert_eq!(detect_language(en).as_deref(), Some("en"));
    assert_eq!(detect_language(ru).as_deref(), Some("ru"));
    assert_eq!(detect_language(uk).as_deref(), Some("uk"));
}

#[test]
fn detects_german_french_spanish_italian() {
    assert_eq!(detect_language("Kartoffelsalat. Die Kartoffeln kochen und mit der Zwiebel und dem Essig vermischen.").as_deref(), Some("de"));
    assert_eq!(detect_language("Ratatouille. Faire revenir les oignons dans l'huile avec les courgettes et les tomates.").as_deref(), Some("fr"));
    assert_eq!(detect_language("Tortilla de patatas. Freír las patatas con la cebolla y mezclar con los huevos batidos.").as_deref(), Some("es"));
    assert_eq!(detect_language("Spaghetti alla carbonara. Cuocere la pasta e mescolare con le uova e il guanciale.").as_deref(), Some("it"));
}

#[test]
fn unsure_inputs_yield_none() {
    assert_eq!(detect_language(""), None);
    assert_eq!(detect_language("Pho"), None, "a bare title is not enough");
    assert_eq!(detect_language("250 g, 3 pcs, 10 ml"), None);
    assert_eq!(detect_language("Tonkotsu ramen 豚骨ラーメン"), None, "scripts we do not model stay unknown");
}
