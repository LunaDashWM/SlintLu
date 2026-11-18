//! Fuzzy application search.

use super::entry::AppEntry;
use super::history::LaunchHistory;

/// How well `query` (lowercase) matches `text`: prefix > word start > substring > in-order
/// letters. `None` when the letters do not occur in order.
fn score_text(query: &str, text: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(1);
    }
    let text = text.to_lowercase();
    if text.starts_with(query) {
        return Some(1000 - text.len().min(500) as u32);
    }
    if text.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(query)) {
        return Some(700);
    }
    if text.contains(query) {
        return Some(500);
    }
    // Subsequence: every query char appears in order; tighter matches score higher.
    let mut gaps = 0u32;
    let mut chars = text.chars();
    for q in query.chars() {
        let mut found = false;
        for c in chars.by_ref() {
            if c == q {
                found = true;
                break;
            }
            gaps += 1;
        }
        if !found {
            return None;
        }
    }
    Some(300u32.saturating_sub(gaps * 5).max(10))
}

/// Every word of the query must match somewhere; their scores add up.
fn score(app: &AppEntry, query: &str) -> Option<u32> {
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.len() <= 1 {
        return score_word(app, query);
    }
    words
        .iter()
        .map(|w| score_word(app, w))
        .sum::<Option<u32>>()
        .map(|total| total / words.len() as u32)
}

fn score_word(app: &AppEntry, query: &str) -> Option<u32> {
    let name = score_text(query, &app.name).map(|s| s + 100);
    let others = [&app.generic_name, &app.id, &app.comment]
        .into_iter()
        .chain(app.keywords.iter())
        .filter_map(|t| score_text(query, t))
        .map(|s| s / 2)
        .max();
    name.into_iter().chain(others).max()
}

/// Matching applications, best first. Frequently launched applications get a boost; an empty
/// query lists everything by frequency, then name.
/// Whether `app` is in any of `categories` (freedesktop names, ";"-separated); every
/// application is when the list is empty.
pub fn in_categories(app: &AppEntry, categories: &str) -> bool {
    let mut wanted = categories.split(';').map(str::trim).filter(|c| !c.is_empty()).peekable();
    if wanted.peek().is_none() {
        return true;
    }
    wanted.any(|category| app.categories.iter().any(|c| c.eq_ignore_ascii_case(category)))
}

pub fn search<'a>(
    apps: &'a [AppEntry],
    query: &str,
    history: &LaunchHistory,
    limit: usize,
) -> Vec<&'a AppEntry> {
    let query = query.trim().to_lowercase();
    let mut scored: Vec<(u32, &AppEntry)> = apps
        .iter()
        .filter_map(|app| {
            let base = score(app, &query)?;
            let launches = history.count(&app.id);
            // Logarithmic boost so a favourite never hides a much better match.
            let boost = (launches as f32 + 1.0).log2() * 40.0;
            Some((base + boost as u32, app))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    scored.into_iter().take(limit).map(|(_, app)| app).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, generic: &str, keywords: &[&str]) -> AppEntry {
        AppEntry {
            id: id.into(),
            name: name.into(),
            generic_name: generic.into(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            ..Default::default()
        }
    }

    fn apps() -> Vec<AppEntry> {
        vec![
            app("firefox.desktop", "Firefox", "Web Browser", &["internet"]),
            app("foot.desktop", "Foot", "Terminal", &["shell"]),
            app("files.desktop", "Files", "File Manager", &["folder"]),
            app("gimp.desktop", "GNU Image Manipulation Program", "Image Editor", &[]),
        ]
    }

    fn names(results: Vec<&AppEntry>) -> Vec<&str> {
        results.into_iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn prefix_matches_rank_first() {
        let apps = apps();
        let history = LaunchHistory::default();
        assert_eq!(names(search(&apps, "fi", &history, 10))[..2], ["Files", "Firefox"]);
        assert_eq!(names(search(&apps, "browser", &history, 10)), vec!["Firefox"]);
        assert_eq!(
            names(search(&apps, "imp", &history, 10)),
            vec!["GNU Image Manipulation Program"]
        );
        assert_eq!(names(search(&apps, "ffx", &history, 10)), vec!["Firefox"]);
        assert!(search(&apps, "zzz", &history, 10).is_empty());
        // Several words: all must match (name and description here).
        assert_eq!(names(search(&apps, "foot term", &history, 10)), vec!["Foot"]);
        assert!(search(&apps, "foot browser", &history, 10).is_empty());
    }

    #[test]
    fn frequent_apps_are_boosted() {
        let apps = apps();
        let dir = tempfile::tempdir().unwrap();
        let mut history = LaunchHistory::load(dir.path().join("h.json"));
        for _ in 0..20 {
            history.record("firefox.desktop");
        }
        assert_eq!(names(search(&apps, "fi", &history, 10))[0], "Firefox");
        assert_eq!(names(search(&apps, "", &history, 2)), vec!["Firefox", "Files"]);
    }

    #[test]
    fn filters_by_category() {
        let mut player = app("mpv.desktop", "mpv", "Media Player", &[]);
        player.categories = vec!["AudioVideo".into(), "Video".into()];
        let editor = app("zed.desktop", "Zed", "Text Editor", &[]);
        assert!(in_categories(&player, "AudioVideo;Audio;Video"));
        assert!(!in_categories(&editor, "AudioVideo;Audio;Video"));
        assert!(in_categories(&editor, ""));
        assert!(in_categories(&player, " video "));
    }
}
