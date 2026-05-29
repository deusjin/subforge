use std::time::Duration;

pub async fn translate(text: &str, target_lang: &str) -> Result<String, String> {
    let lang_code = resolve_google_lang(target_lang);
    let url = format!(
        "https://translate.google.com/m?sl=auto&tl={}&q={}",
        lang_code,
        urlencoding(text)
    );
    let body = super::http_client()
        .get(&url)
        .header(
            "User-Agent",
            "Mozilla/4.0 (compatible;MSIE 6.0;Windows NT 5.1;SV1)",
        )
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("google translate request failed: {e}"))?
        .text()
        .await
        .map_err(|e| format!("google translate read failed: {e}"))?;

    extract_translation(&body).ok_or_else(|| "google translate: failed to extract result".into())
}

fn extract_translation(html: &str) -> Option<String> {
    // Look for class="result-container">TEXT</div>
    let marker = "class=\"result-container\">";
    let start = html.find(marker)? + marker.len();
    let end = html[start..].find('<')? + start;
    let text = &html[start..end];
    Some(unescape_html(text))
}

fn unescape_html(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                String::from(b as char)
            }
            _ => format!("%{:02X}", b),
        })
        .collect()
}

fn resolve_google_lang(lang: &str) -> String {
    match lang.to_ascii_lowercase().as_str() {
        "zh-hans" | "zh-cn" | "chinese" => "zh-CN",
        "zh-hant" | "zh-tw" => "zh-TW",
        "en" | "english" => "en",
        "ja" | "japanese" => "ja",
        "ko" | "korean" => "ko",
        "fr" | "french" => "fr",
        "de" | "german" => "de",
        "es" | "spanish" => "es",
        _ => lang,
    }
    .to_string()
}
