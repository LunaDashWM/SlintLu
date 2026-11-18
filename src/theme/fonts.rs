//! Installed font families, for choosing the shell's font.

/// Every installed font family once, sorted without regard to case. Reads the system font
/// directories (fontconfig's), so call it off the UI thread.
pub fn families() -> Vec<String> {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    let mut names: Vec<String> = database
        .faces()
        .filter_map(|face| {
            // The English name when the font has several (CJK fonts name themselves twice).
            face.families
                .iter()
                .find(|(_, language)| *language == fontdb::Language::English_UnitedStates)
                .or_else(|| face.families.first())
                .map(|(name, _)| name.clone())
        })
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// The families whose name contains every word of `query` (any case), names starting with the
/// query first; an empty query lists them all. At most `limit`.
pub fn matching<'a>(families: &'a [String], query: &str, limit: usize) -> Vec<&'a String> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut found: Vec<(bool, &String)> = families
        .iter()
        .filter_map(|family| {
            let name = family.to_lowercase();
            words
                .iter()
                .all(|word| name.contains(word))
                .then(|| (!name.starts_with(&query), family))
        })
        .collect();
    // Stable: alphabetical within each group.
    found.sort_by_key(|(later, _)| *later);
    found.into_iter().take(limit).map(|(_, family)| family).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_words_and_ranks_prefixes_first() {
        let families: Vec<String> =
            ["Inter", "JetBrains Mono", "Noto Sans", "Noto Sans CJK TC", "Source Sans 3"]
                .map(String::from)
                .into();
        assert_eq!(matching(&families, "", 10).len(), 5);
        assert_eq!(
            matching(&families, "sans", 10),
            ["Noto Sans", "Noto Sans CJK TC", "Source Sans 3"]
        );
        assert_eq!(matching(&families, "cjk noto", 10), ["Noto Sans CJK TC"]);
        assert_eq!(matching(&families, "mono", 10), ["JetBrains Mono"]);
        assert_eq!(matching(&families, "noto", 1), ["Noto Sans"]);
        let ranked: Vec<String> = ["A Inter", "Inter"].map(String::from).into();
        assert_eq!(matching(&ranked, "inter", 10), ["Inter", "A Inter"]);
    }
}
