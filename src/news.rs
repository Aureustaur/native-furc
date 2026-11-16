//! Furcadia announcements feed parser and fetcher (`http://news.furcadia.com/current`).


const NEWS_URL: &str = "http://news.furcadia.com/current";

/// A single announcement entry from Furcadia's news feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsEntry {
    pub date: String,
    pub category: String,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub image_url: Option<String>,
}

/// Fetches and parses announcements from `http://news.furcadia.com/current`.
pub fn fetch_news() -> Result<Vec<NewsEntry>, String> {
    let output = crate::sys::command("curl")
        .arg("-sSL")
        .arg("--max-time")
        .arg("8")
        .arg(NEWS_URL)
        .output()
        .map_err(|e| format!("failed to run curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("news fetch failed: {}", stderr.trim()));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_news_feed(&text))
}

/// Parses `NewsEntry` lines from the tab-separated Furcadia news feed.
pub fn parse_news_feed(input: &str) -> Vec<NewsEntry> {
    let mut entries = Vec::new();
    for line in input.lines() {
        let Some(rest) = line.strip_prefix("NewsEntry ") else {
            continue;
        };
        let parts: Vec<&str> = rest.split('\t').collect();
        if parts.len() < 5 {
            continue;
        }
        let date = parts[0].trim().to_owned();
        let category = parts[2].trim().to_owned();
        let title = parts[3].trim().to_owned();
        let body = parts[4].trim().to_owned();
        let url = parts
            .get(5)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let image_url = parts
            .get(6)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);

        entries.push(NewsEntry {
            date,
            category,
            title,
            body,
            url,
            image_url,
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sample_news_feed() {
        let sample = "\
NewsVersion 1.0
NewsEntry Oct 27, 2026\t0\tDigo Market\t AUTUMN PORTRAIT SALE\tA special sale is up!\thttp://digomarket.com/?cmd=list&topic=5\thttp://news.furcadia.com/images/z/2/0/00000520.png
NewsEntry Oct 24, 2026\t0\tEvents\t WOLF HOWL TOUR\tJoin us!\t\thttp://news.furcadia.com/images/z/5/2/00000452.jpg
";
        let entries = parse_news_feed(sample);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "AUTUMN PORTRAIT SALE");
        assert_eq!(
            entries[0].url.as_deref(),
            Some("http://digomarket.com/?cmd=list&topic=5")
        );
        assert_eq!(entries[1].url, None);
    }
}
