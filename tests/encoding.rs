//! Guards against UTF-8 sources being re-saved through a legacy code page
//! (Cyrillic UI strings turn into "РќР°СЃС‚..." in the tray menu).

#[test]
fn sources_have_no_mojibake() {
    // UTF-8 Cyrillic decoded as CP1251 always yields 'Р' or 'С' followed by one of these.
    const MARKS: &[char] = &['Ѓ', 'Ђ', '‚', 'ѓ', '„', '…', '†', '‡', '€', '‰', 'Љ', '‹', 'Њ', 'Ќ', 'Ћ', 'Џ', 'ђ', 'љ', 'њ', 'ќ', 'ћ', 'џ', 'Ў', 'ў', 'Ј', '¤', 'Ґ', '¦', '§', 'Ё', '©', 'Є', '«', '¬', '®', 'Ї', '°', '±', 'І', 'і', 'ґ', 'µ', '¶', '·', 'ё', '№', 'є', '»', 'ј', 'Ѕ', 'ѕ', 'ї'];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{} is not UTF-8", path.display()));
        assert!(!text.starts_with('\u{feff}'), "{} has a BOM", path.display());
        let chars: Vec<char> = text.chars().collect();
        for w in chars.windows(2) {
            if (w[0] == 'Р' || w[0] == 'С') && MARKS.contains(&w[1]) {
                let at: String = chars.iter().collect::<String>();
                let pos = at.find(&format!("{}{}", w[0], w[1])).unwrap();
                panic!("{}: mojibake near {:?}", path.display(), &at[pos..(pos + 40).min(at.len())]);
            }
        }
    }
}
