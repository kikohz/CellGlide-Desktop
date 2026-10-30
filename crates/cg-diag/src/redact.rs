//! Takes what could identify a person or place out of diagnostics text before it leaves
//! the computer: device and host names, addresses (kept to their first part), host names and
//! URLs other than CellGlide's own check targets, the home folder's name and device UDIDs.
//! Times, versions, reverse-DNS identifiers, UUIDs, system paths and systemd unit names stay,
//! so the log still reads. Ported from the Mac's `DiagnosticsRedactor` with Linux rules.

use fancy_regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

/// Hosts the apps themselves contact for checks; naming them helps and reveals nothing.
pub const KEPT_HOSTS: &[&str] = &[
    "cloudflare-dns.com",
    "one.one.one.one",
    "www.cloudflare.com",
    "dns.google",
    "www.google.com",
    "www.apple.com",
    "captive.apple.com",
    "cellglide.com",
    "diag.cellglide.com",
    // The other DNS presets.
    "dns.alidns.com",
    "doh.pub",
    "doh.opendns.com",
    "dns.quad9.net",
    // Linux-only checks: the speed test and the DNS leak test.
    "speed.cloudflare.com",
    "leakcheck.cellglide.com",
];

/// Last labels of file names and code identifiers; a dotted token ending in one of these
/// (".service", ".rs", ".mac") is not a host name. Any other dotted token ending in
/// letters is treated as a host and redacted.
const FILE_SUFFIXES: &[&str] = &[
    "swift",
    "json",
    "log",
    "txt",
    "plist",
    "mac",
    "app",
    "framework",
    "dylib",
    "bundle",
    "xcodeproj",
    "md",
    "png",
    "jpg",
    "ktx",
    "strings",
    "lproj",
    "h",
    "m",
    "c",
    "js",
    "sh",
    "py",
    "zip",
    "dmg",
    "pkg",
    "html",
    "css",
    "mp4", // Linux
    "service",
    "socket",
    "conf",
    "rules",
    "pkla",
    "desktop",
    "rs",
    "so",
    "deb",
    "rpm",
    "toml",
    "yml",
    "jsonl",
    // Windows
    "dll",
    "exe",
    "ps1",
    "sys",
    "msi",
];

/// The phone report's network line label as localized: English, Chinese, Spanish.
const NETWORK_LABELS: &[&str] = &["Network:", "网络:", "Red:"];

/// Names too common to replace without rewriting unrelated text.
const GENERIC_NAMES: &[&str] = &[
    "localhost",
    "mac",
    "macbook",
    "iphone",
    "ipad",
    "user",
    "admin",
    "root",
    // Default logins and host names of common distributions and images.
    "ubuntu",
    "debian",
    "fedora",
    "dev",
    "test",
    "pi",
];

const HOST_PLACEHOLDER: &str = "<主机>";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("redactor pattern compiles")
}

static URL: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?i)\b[a-z][a-z0-9+.-]*:(?:\\?/){2}[^\s"'<>)\]]+"#));
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| re(r#"[^\s@<>"'(),;]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+"#));
static UDID: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{16}\b|\b[0-9a-fA-F]{40}\b"));
static IPV4: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,3})\.\d{1,3}\.\d{1,3}\.\d{1,3}\b"));
// IPv6: hex groups with "::" or at least five colons (a time like 12:21:51 has two).
// A ninth group is a port after an unbracketed address.
static IPV6: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?<![\w:])[0-9A-Fa-f]{0,4}(?::[0-9A-Fa-f]{0,4}){2,8}(?:%[\w.]+)?(?:/\d{1,3})?(?![\w:])")
});
// File paths under system folders are left alone; they are matched first, whole.
static HOST: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"(?<![\w.@-])(?:/(?:home|usr|etc|var|run|tmp|opt|proc|sys|dev|lib|srv)/[^\s"'<>]*|(?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,63}(?![\w-]|\.\w))"#,
    )
});
static MACHINE_ID: LazyLock<Regex> = LazyLock::new(|| re(r"\b[0-9a-fA-F]{32}\b"));
// A MAC address in an interface name (enx/wlx + 12 hex), or written with dashes or colons.
static IFACE_MAC: LazyLock<Regex> = LazyLock::new(|| re(r"\b(enx|wlx)[0-9a-fA-F]{12}\b"));
static MAC: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?<![\w:-])(?:[0-9A-Fa-f]{2}-){5}[0-9A-Fa-f]{2}(?![\w:-])|(?<![\w:-])(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}(?![\w:-])",
    )
});
// Wi-Fi names: `connection 'Home'`, `ssid "Home"`, `SSID=Home`.
// A single-quoted name may hold apostrophes ("Alice's iPhone"): it ends at the last quote
// before the end of the line or before the next " (", else at the first closing quote.
// Blanks between the word and the name never include a newline.
static SSID_QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"(?im)\b(?:connection|ssid)(?:[ \t]+|[ \t]*[=:][ \t]*)(?:'[^\n]*?'(?=[ \t]*\(|[ \t]*$)|'[^'\n]*'|"[^"\n]*")"#,
    )
});
static SSID_BARE: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)\bssid[ \t]*[=:][ \t]*[^\s,;'"]+"#));
static HOME: LazyLock<Regex> = LazyLock::new(|| re(r#"/home/[^/\s"'<>]+"#));
// A Windows profile folder, `C:\Users\<name>`, with either slash (or escaped backslashes).
static WINDOWS_PROFILE: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?i)(?<![A-Za-z0-9])[a-z]:(?:\\+|/)users(?:\\+|/)[^\\/\s"'<>:|*?]+"#));
/// Folders under `C:\Users` that belong to no one.
const SHARED_PROFILES: &[&str] = &["public", "default", "user"];
static LOCATION_CITY: LazyLock<Regex> = LazyLock::new(|| re(r"· [^·\n]+, [A-Z]{2}(?= ·|\s*$)"));
static LOCATION_COLO: LazyLock<Regex> = LazyLock::new(|| re(r"· [A-Z]{3}\s*$"));
// journald line start: "Oct 16 12:00:00[.123456] host unit[pid]:" or an ISO time, then the host.
static JOURNAL_HOST: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?m)^(\s*(?:[A-Z][a-z]{2} [ \d]\d \d\d:\d\d:\d\d(?:\.\d+)?|\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:?\d\d)?) )(?!(?:TRACE|DEBUG|INFO|WARN|ERROR) )([^\s:\[\]]+)(?= [\w.@-]+(?:\[\d+\])?:)",
    )
});

/// Replaces every match of `regex` with `transform(match)`. If the regex gives up
/// (backtrack limit), nothing after the last good match is trusted and it is dropped.
fn replace(regex: &Regex, text: &str, mut transform: impl FnMut(&str) -> String) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last = 0;
    for found in regex.find_iter(text) {
        let Ok(found) = found else {
            result.push_str("<已隐去>");
            return result;
        };
        result.push_str(&text[last..found.start()]);
        result.push_str(&transform(found.as_str()));
        last = found.end();
    }
    result.push_str(&text[last..]);
    result
}

pub fn redact(text: &str, names: &BTreeMap<String, String>, user_name: Option<&str>) -> String {
    redact_with_profile(text, names, user_name, own_profile().as_deref())
}

/// This process's profile folder (`%USERPROFILE%`) on Windows; none elsewhere.
fn own_profile() -> Option<String> {
    #[cfg(windows)]
    return std::env::var("USERPROFILE").ok().filter(|p| !p.is_empty());
    #[cfg(not(windows))]
    return None;
}

/// `redact`, with the profile folder to hide as `%USERPROFILE%` given explicitly.
fn redact_with_profile(
    text: &str,
    names: &BTreeMap<String, String>,
    user_name: Option<&str>,
    profile: Option<&str>,
) -> String {
    let mut result = redact_names(text, names);
    if let Some(user) = user_name.filter(|u| !u.is_empty()) {
        result = result.replace(&format!("/home/{user}"), "/home/~");
    }
    // Whoever the user is, the folder name under /home says who.
    result = replace(&HOME, &result, |_| "/home/~".to_string());
    if let Some(regex) = profile.and_then(profile_regex) {
        result = replace(&regex, &result, |_| "%USERPROFILE%".to_string());
    }
    // The same for Windows profile folders, `C:\Users\<name>`: first the requester's own,
    // whose name may hold spaces, then any other.
    if let Some(user) = user_name.filter(|u| !u.is_empty())
        && let Ok(regex) = Regex::new(&format!(
            r#"(?i)(?<![A-Za-z0-9])([a-z]:(?:\\+|/)users(?:\\+|/)){}(?=[\\/\s"'<>]|$)"#,
            fancy_regex::escape(user)
        ))
    {
        result = replace_profile_name(&regex, &result);
    }
    result = replace_profile_name(&WINDOWS_PROFILE, &result);
    // The login name shows up outside paths too (sudo, PAM and logind lines).
    if let Some(user) = user_name.filter(|u| login_name_is_redactable(u))
        && let Ok(regex) = Regex::new(&format!(
            "(?i){}",
            bounded(&fancy_regex::escape(user), user)
        ))
    {
        result = replace(&regex, &result, |_| "<用户>".to_string());
    }
    result = redact_journal_host(&result, names);
    // Any scheme, in any case, with plain or JSON-escaped slashes.
    result = replace(&URL, &result, |url| {
        let cleaned = url.replace('\\', "");
        match url_host(&cleaned) {
            Some(host) if KEPT_HOSTS.contains(&host.as_str()) => url.to_string(),
            _ => "<网址>".to_string(),
        }
    });
    result = replace(&EMAIL, &result, |_| "<邮箱>".to_string());
    if NETWORK_LABELS.iter().any(|label| result.contains(label)) {
        result = result
            .split('\n')
            .map(redact_location)
            .collect::<Vec<_>>()
            .join("\n");
    }
    // Device UDIDs: 8-16 hex (newer phones) or 40 hex (older ones).
    result = replace(&UDID, &result, |m| format!("…{}", last_chars(m, 4)));
    result = replace(&MACHINE_ID, &result, |m| format!("…{}", last_chars(m, 4)));
    result = replace(&IFACE_MAC, &result, |m| format!("{}<MAC>", &m[..3]));
    result = replace(&MAC, &result, |_| "<MAC>".to_string());
    result = replace(&SSID_QUOTED, &result, |m| {
        let quote = m.find(['\'', '"']).unwrap_or(m.len());
        format!("{}<网络名>", &m[..quote])
    });
    result = replace(&SSID_BARE, &result, |m| {
        let sep = m.find(['=', ':']).unwrap_or(0) + 1;
        let spaces = m[sep..].len() - m[sep..].trim_start().len();
        format!("{}<网络名>", &m[..sep + spaces])
    });
    result = replace(&IPV4, &result, |m| {
        format!("{}.x.x.x", m.split('.').next().unwrap_or_default())
    });
    result = replace(&IPV6, &result, redact_ipv6);
    replace(&HOST, &result, |token| {
        if token.starts_with('/') {
            return token.to_string();
        }
        let lower = token.to_lowercase();
        if KEPT_HOSTS.contains(&lower.as_str()) {
            return token.to_string();
        }
        let labels: Vec<&str> = token.split('.').filter(|l| !l.is_empty()).collect();
        let Some(last) = labels.last() else {
            return token.to_string();
        };
        if FILE_SUFFIXES.contains(&last.to_lowercase().as_str()) {
            return token.to_string();
        }
        // Reverse-DNS identifiers (com.kikohz.cellglide) are not hosts.
        if labels.len() >= 3
            && ["com", "org", "net", "io"].contains(&labels[0].to_lowercase().as_str())
        {
            return token.to_string();
        }
        // Type paths (Network.NWError): every label capitalised, the last not all capitals.
        if labels
            .iter()
            .all(|l| l.chars().next().is_some_and(char::is_uppercase))
            && last.chars().any(char::is_lowercase)
        {
            return token.to_string();
        }
        "<域名>".to_string()
    })
}

/// Replaces the folder name at the end of each `C:\Users\<name>` match with `User`,
/// leaving the shared folders alone.
fn replace_profile_name(regex: &Regex, text: &str) -> String {
    replace(regex, text, |m| {
        let name = m.rsplit(['\\', '/']).next().unwrap_or_default();
        if SHARED_PROFILES.contains(&name.to_lowercase().as_str()) {
            return m.to_string();
        }
        format!("{}User", &m[..m.len() - name.len()])
    })
}

/// A pattern for `profile` (`E:\Profiles\carol`) written with either slash and in any
/// case, ending where the folder name does; none for a path with fewer than two parts.
fn profile_regex(profile: &str) -> Option<Regex> {
    let parts: Vec<String> = profile
        .split(['\\', '/'])
        .filter(|p| !p.is_empty())
        .map(|p| fancy_regex::escape(p).into_owned())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    Regex::new(&format!(
        r#"(?i)(?<![A-Za-z0-9]){}(?=[\\/\s"'<>]|$)"#,
        parts.join(r"(?:\\+|/)")
    ))
    .ok()
}

/// A login name worth replacing everywhere: long enough and not a common word.
fn login_name_is_redactable(user: &str) -> bool {
    user.chars().count() >= 3 && !GENERIC_NAMES.contains(&user.to_lowercase().as_str())
}

fn last_chars(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

fn redact_ipv6(m: &str) -> String {
    let address: String = m.chars().take_while(|&c| c != '%' && c != '/').collect();
    let colons = address.matches(':').count();
    if !address.contains("::") && colons < 5 {
        return m.to_string();
    }
    if address == "::" {
        return m.to_string();
    }
    let groups: Vec<&str> = address.split(':').collect();
    let first = groups.first().copied().unwrap_or_default();
    if first.is_empty() {
        return "<IPv6>".to_string();
    }
    if !address.contains("::")
        && colons == 8
        && let Some(port) = groups.last()
    {
        return format!("{first}::/16:{port}");
    }
    format!("{first}::/16")
}

/// The lower-cased host of `scheme://[user@]host[:port]/...`.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host_port.starts_with('[') {
        host_port.split_once(']')?.0.trim_start_matches('[')
    } else {
        host_port.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// On a journald line the word after the time is the computer's host name. A host name that
/// `names` already replaced stays as the replacement; any other one becomes a placeholder.
fn redact_journal_host(text: &str, names: &BTreeMap<String, String>) -> String {
    journal_host_from(text, names, JOURNAL_HOST.captures_iter(text))
}

/// The work of `redact_journal_host` over already-found matches. A match that failed
/// (backtrack limit) is not trusted: the rest of the text is dropped.
fn journal_host_from<'t>(
    text: &str,
    names: &BTreeMap<String, String>,
    matches: impl Iterator<Item = Result<fancy_regex::Captures<'t, str>, fancy_regex::Error>>,
) -> String {
    let replacements: BTreeSet<&str> = names.values().map(String::as_str).collect();
    let mut result = String::with_capacity(text.len());
    let mut last = 0;
    for caps in matches {
        let Ok(caps) = caps else {
            result.push_str("<已隐去>");
            return result;
        };
        let (Some(whole), Some(host)) = (caps.get(0), caps.get(2)) else {
            continue;
        };
        result.push_str(&text[last..host.start()]);
        if replacements.contains(host.as_str()) || host.as_str() == HOST_PLACEHOLDER {
            result.push_str(host.as_str());
        } else {
            result.push_str(HOST_PLACEHOLDER);
        }
        last = whole.end();
    }
    result.push_str(&text[last..]);
    result
}

/// Device names, matched ignoring case and apostrophe style, also in host-name form
/// ("Alices-MacBook-Air"), longest first. Generic and too-short names are ignored,
/// and a Latin name only matches whole (not inside a longer word or number).
fn redact_names(text: &str, names: &BTreeMap<String, String>) -> String {
    let mut patterns: Vec<(String, &str)> = Vec::new();
    for (name, replacement) in names {
        // Two characters are enough for names with non-ASCII letters (Chinese); Latin names need three.
        let minimum = if name.is_ascii() { 3 } else { 2 };
        if name.chars().count() < minimum || GENERIC_NAMES.contains(&name.to_lowercase().as_str()) {
            continue;
        }
        let host_form: String = name
            .chars()
            .filter(|&c| c != '\'' && c != '’')
            .collect::<String>()
            .replace(' ', "-");
        let full: String = name
            .chars()
            .map(|c| {
                if c == '\'' || c == '’' {
                    "['’]".to_string()
                } else {
                    fancy_regex::escape(&c.to_string()).into_owned()
                }
            })
            .collect();
        patterns.push((bounded(&full, name), replacement));
        if host_form != *name {
            patterns.push((
                bounded(&fancy_regex::escape(&host_form), &host_form),
                replacement,
            ));
        }
    }
    patterns.sort_by_key(|(pattern, _)| std::cmp::Reverse(pattern.chars().count()));
    let mut result = text.to_string();
    for (pattern, replacement) in patterns {
        let Ok(regex) = Regex::new(&format!("(?i){pattern}")) else {
            continue;
        };
        result = replace(&regex, &result, |_| replacement.to_string());
    }
    result
}

/// Requires no ASCII letter or digit next to a name end that is itself one, so "Max" skips
/// "Maximum" but Chinese text (no spaces between words) still matches around a name.
fn bounded(pattern: &str, name: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    format!(
        "{}{}{}",
        if word(name.chars().next()) {
            "(?<![A-Za-z0-9])"
        } else {
            ""
        },
        pattern,
        if word(name.chars().next_back()) {
            "(?![A-Za-z0-9])"
        } else {
            ""
        }
    )
}

/// In the phone's "Network:" line the city and the Cloudflare colo say where the phone is.
fn redact_location(line: &str) -> String {
    let trimmed = line.trim_start_matches([' ', '\t']);
    if !NETWORK_LABELS
        .iter()
        .any(|label| trimmed.starts_with(label))
    {
        return line.to_string();
    }
    let result = replace(&LOCATION_CITY, line, |_| "· <位置>".to_string());
    replace(&LOCATION_COLO, &result, |_| "· <位置>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn no_names() -> BTreeMap<String, String> {
        BTreeMap::new()
    }
    fn names(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    fn r(text: &str) -> String {
        redact(text, &no_names(), None)
    }

    // testDeviceNames
    #[test]
    fn device_names() {
        assert_eq!(
            redact(
                "Connected to alice’s MacBook Air via USB",
                &names(&[("alice’s MacBook Air", "Mac")]),
                None
            ),
            "Connected to Mac via USB"
        );
    }

    // testIPv4KeepsTheFirstOctet
    #[test]
    fn ipv4_keeps_the_first_octet() {
        assert_eq!(
            r("gateway 192.0.0.1, peer 203.0.113.212:443"),
            "gateway 192.x.x.x, peer 203.x.x.x:443"
        );
    }

    // testIPv6KeepsTheFirstGroup
    #[test]
    fn ipv6_keeps_the_first_group() {
        assert_eq!(
            r("addr 2607:fb91:149c:41fa:6523:b887:d298:6cff and fe80::acb4:1cc8:43aa:ecae%pdp_ip0"),
            "addr 2607::/16 and fe80::/16"
        );
        assert_eq!(r("[2606:4700::6810:7b60]:443"), "[2606::/16]:443");
    }

    // testTimesVersionsAndIdentifiersAreKept
    #[test]
    fn times_versions_and_identifiers_are_kept() {
        let line = "2026-10-08 12:21:51.820 [com.kikohz.cellproxy.mac:State] App 0.5.8 (40) id 00000000-1111-4222-8333-444444444444 Network.NWError";
        assert_eq!(r(line), line);
    }

    // testHostsAndURLs
    #[test]
    fn hosts_and_urls() {
        assert_eq!(
            r("Data saver refused video.example.com and tracker.ads.net"),
            "Data saver refused <域名> and <域名>"
        );
        assert_eq!(
            r("GET https://private.example.org/path?q=1 failed"),
            "GET <网址> failed"
        );
        assert_eq!(
            r("DoH https://cloudflare-dns.com/dns-query ok, www.apple.com 200"),
            "DoH https://cloudflare-dns.com/dns-query ok, www.apple.com 200"
        );
    }

    // testHomeFolderAndUDID (Mac's /Users/... case becomes /home/...)
    #[test]
    fn home_folder_and_udid() {
        assert_eq!(
            redact("/home/alice/.config/cellglide", &no_names(), Some("alice")),
            "/home/~/.config/cellglide"
        );
        assert_eq!(r("device 00008150-000000000000001C"), "device …001C");
    }

    // testURLsInAnySchemeCaseAndJSONEscaping
    #[test]
    fn urls_in_any_scheme_case_and_json_escaping() {
        assert_eq!(
            r(r#"{"u":"https:\/\/private.example.org\/watch?v=1"}"#),
            r#"{"u":"<网址>"}"#
        );
        assert_eq!(r("via wss://evil.example.com/x ok"), "via <网址> ok");
        assert_eq!(r("HTTPS://Secret.Example.com/a"), "<网址>");
        assert_eq!(
            r("DoH HTTPS://Cloudflare-DNS.com/dns-query"),
            "DoH HTTPS://Cloudflare-DNS.com/dns-query"
        );
        assert_eq!(
            r("GET /watch/video.example.com now"),
            "GET /watch/<域名> now"
        );
        assert_eq!(r("/home/~/x/a.b.example.ru"), "/home/~/x/a.b.example.ru");
    }

    // testAnyDottedTokenWithLetterSuffixIsAHost
    #[test]
    fn any_dotted_token_with_letter_suffix_is_a_host() {
        for host in [
            "yandex.ru",
            "foo.ca",
            "video.example.ru",
            "bar.com.au",
            "Alices-MacBook-Air.local",
        ] {
            assert_eq!(
                r(&format!("saw {host} twice")),
                "saw <域名> twice",
                "{host}"
            );
        }
        let kept = "Network.NWError Foundation.URLError com.kikohz.cellproxy.mac CellGlide.app Log.swift MacBookPro14,1 0.5.8 12:21:51.820";
        assert_eq!(r(kept), kept);
        assert_eq!(r("refused tracker.ads.net."), "refused <域名>.");
    }

    // testDeviceNamesIgnoreCaseApostropheAndMatchHostForm
    #[test]
    fn device_names_ignore_case_apostrophe_and_match_host_form() {
        let n = names(&[("alice’s MacBook Air", "Mac")]);
        assert_eq!(redact("to ALICE'S macbook air ok", &n, None), "to Mac ok");
        assert_eq!(
            redact("host Alices-MacBook-Air ok", &n, None),
            "host Mac ok"
        );
        assert_eq!(
            redact(
                "Alices MacBook",
                &names(&[("MacBook", "A"), ("Alices MacBook", "B")]),
                None
            ),
            "B"
        );
    }

    // testShortAndGenericNamesAreIgnoredAndNamesMatchWholeWordsOnly
    #[test]
    fn short_and_generic_names_are_ignored_and_names_match_whole_words_only() {
        assert_eq!(
            redact("Also all", &names(&[("Al", "User")]), None),
            "Also all"
        );
        assert_eq!(
            redact("127.0.0.1 localhost", &names(&[("localhost", "Mac")]), None),
            "127.x.x.x localhost"
        );
        assert_eq!(
            redact("Max said", &names(&[("Max", "User")]), None),
            "User said"
        );
        assert_eq!(
            redact("Maximum", &names(&[("Max", "User")]), None),
            "Maximum"
        );
    }

    // testChineseNamesMatchWithoutWordBoundaries
    #[test]
    fn chinese_names_match_without_word_boundaries() {
        let u = |n: &str| names(&[(n, "User")]);
        assert_eq!(
            redact("王小明的MacBook Air", &u("王小明"), None),
            "User的MacBook Air"
        );
        assert_eq!(redact("王明的电脑", &u("王明"), None), "User的电脑");
        assert_eq!(redact("Alice的电脑", &u("Alice"), None), "User的电脑");
        assert_eq!(redact("Alicewang", &u("Alice"), None), "Alicewang");
    }

    // testPhoneLocationIsRemovedButCarrierStays
    #[test]
    fn phone_location_is_removed_but_carrier_stays() {
        assert_eq!(
            r("Network: T-Mobile USA, Inc. (AS21928) · 2607:fb91:149c::/48 · Baltimore, US · IAD"),
            "Network: T-Mobile USA, Inc. (AS21928) · 2607::/16 · <位置> · <位置>"
        );
    }

    // testPhoneLocationIsRemovedInChineseAndSpanishReports
    #[test]
    fn phone_location_is_removed_in_chinese_and_spanish_reports() {
        for label in ["网络", "Red"] {
            assert_eq!(
                r(&format!(
                    "{label}: T-Mobile USA, Inc. (AS21928) · 2607:fb91:149c::/48 · Baltimore, US · IAD"
                )),
                format!("{label}: T-Mobile USA, Inc. (AS21928) · 2607::/16 · <位置> · <位置>")
            );
        }
        let other = "Note: a · Baltimore, US · IAD";
        assert_eq!(r(other), other);
    }

    // testEmails
    #[test]
    fn emails() {
        assert_eq!(r("contact alice@example.com now"), "contact <邮箱> now");
    }

    // testIPv6WithPortAndLeadingDoubleColon
    #[test]
    fn ipv6_with_port_and_leading_double_colon() {
        assert_eq!(
            r("peer 2607:fb91:149c:41fa:6523:b887:d298:6cff:443"),
            "peer 2607::/16:443"
        );
        assert_eq!(r("mapped ::ffff:c0a8:0101 end"), "mapped <IPv6> end");
    }

    // testRealLogSamples
    #[test]
    fn real_log_samples() {
        let line = "2026-10-08 12:21:51.820 Df CellProxyMac[47641:d87b6] [com.kikohz.cellproxy.mac:State] Closed :443 after 0.6 s";
        assert_eq!(r(line), line);
        assert_eq!(
            r("System gateway: 192.0.0.1:0、fe80::acb4:1cc8:43aa:ecae%pdp_ip0"),
            "System gateway: 192.x.x.x:0、fe80::/16"
        );
    }

    // Linux (Review Focus 5)
    #[test]
    fn linux_journal_line_hides_host_keeps_unit() {
        let n = names(&[("alice-thinkpad", "computer")]);
        assert_eq!(
            redact(
                "2026-10-16T12:00:00+08:00 alice-thinkpad cellglide-service[812]: connect failed: no exit via /usr/lib/systemd/system/cellglide.service",
                &n,
                None
            ),
            "2026-10-16T12:00:00+08:00 computer cellglide-service[812]: connect failed: no exit via /usr/lib/systemd/system/cellglide.service"
        );
        assert_eq!(
            r("unit cellglide.service started, polkit 50-cellglide.rules"),
            "unit cellglide.service started, polkit 50-cellglide.rules"
        );
    }

    #[test]
    fn journal_host_is_hidden_even_when_unnamed() {
        for line in [
            "Oct 16 12:00:00 alice-thinkpad cellglide-service[123]: started",
            "Oct  6 12:00:00.123456 alice-thinkpad cellglide-service[123]: started",
            "2026-10-16T12:00:00+0800 alice-thinkpad cellglide-service[123]: started",
            "2026-10-16T04:00:00.123456Z alice-thinkpad kernel: started",
        ] {
            let out = r(line);
            assert!(!out.contains("alice"), "{out}");
            assert!(out.contains("<主机>"), "{out}");
            assert!(out.ends_with(": started"), "{out}");
        }
        assert_eq!(
            r("Oct 16 12:00:00 alice-thinkpad cellglide-service[123]: started"),
            "Oct 16 12:00:00 <主机> cellglide-service[123]: started"
        );
        // Several lines: only line starts count.
        let two = "Oct 16 12:00:00 box a[1]: x\nOct 16 12:00:01 box a[1]: y";
        assert_eq!(
            r(two),
            "Oct 16 12:00:00 <主机> a[1]: x\nOct 16 12:00:01 <主机> a[1]: y"
        );
    }

    #[test]
    fn home_folder_is_hidden_without_a_user_name() {
        assert_eq!(
            r("open /home/alice/.config/cellglide/x.toml failed"),
            "open /home/~/.config/cellglide/x.toml failed"
        );
        assert_eq!(
            r("a /usr/lib/x.so and /etc/cellglide/c.conf and /run/cellglide/control.sock"),
            "a /usr/lib/x.so and /etc/cellglide/c.conf and /run/cellglide/control.sock"
        );
    }

    #[test]
    fn windows_profile_paths_are_hidden() {
        let names = BTreeMap::new();
        assert_eq!(
            redact(r"C:\Users\Alice\AppData\x.log", &names, Some("Alice")),
            r"C:\Users\User\AppData\x.log"
        );
        assert_eq!(
            redact("c:/users/alice/x", &names, Some("Alice")),
            "c:/users/User/x"
        );
    }

    #[test]
    fn any_windows_profile_folder_is_hidden() {
        let names = BTreeMap::new();
        // Without knowing who asked, as /home/<name> is.
        assert_eq!(
            redact(r"open D:\USERS\bob\Desktop\a.txt", &names, None),
            r"open D:\USERS\User\Desktop\a.txt"
        );
        // Escaped (Debug or JSON) backslashes, and a folder name with a space when the
        // login is known.
        assert_eq!(
            redact(
                r#"path: "C:\\Users\\Mary Ann\\x""#,
                &names,
                Some("Mary Ann")
            ),
            r#"path: "C:\\Users\\User\\x""#
        );
        // System paths and the shared folders stay.
        let kept =
            r"C:\Windows\System32\drivers C:\Program Files\CellGlide\wintun.dll C:\Users\Public\x";
        assert_eq!(redact(kept, &names, None), kept);
    }

    #[test]
    fn the_profile_path_is_hidden_wherever_it_is() {
        let names = BTreeMap::new();
        let profile = Some(r"E:\Profiles\carol");
        assert_eq!(
            redact_with_profile(r"E:\Profiles\carol\AppData\x.log", &names, None, profile),
            r"%USERPROFILE%\AppData\x.log"
        );
        assert_eq!(
            redact_with_profile("e:/profiles/CAROL/x", &names, None, profile),
            "%USERPROFILE%/x"
        );
        // A longer folder name that merely starts the same is not the profile.
        assert_eq!(
            redact_with_profile(r"E:\Profiles\caroline\x", &names, None, profile),
            r"E:\Profiles\caroline\x"
        );
    }

    #[test]
    fn linux_file_names_are_not_hosts() {
        let kept = "50-cellglide.rules cellglide.service cellglide.socket main.rs libhev.so cellglide.deb x.rpm cfg.toml a.yml 2026-10-16.jsonl p.pkla a.desktop";
        assert_eq!(r(kept), kept);
    }

    #[test]
    fn login_name_is_hidden_everywhere() {
        let u = Some("alice");
        let go = |t: &str| redact(t, &no_names(), u);
        assert_eq!(
            go("sudo[1]: alice : TTY=pts/0 ; PWD=/home/alice ; USER=root"),
            "sudo[1]: <用户> : TTY=pts/0 ; PWD=/home/~ ; USER=root"
        );
        assert_eq!(
            go("session opened for user Alice(uid=1000) by (uid=0)"),
            "session opened for user <用户>(uid=1000) by (uid=0)"
        );
        assert_eq!(go("USER=alice"), "USER=<用户>");
        // Whole words only, and short or common names are left alone.
        assert_eq!(go("alices-box"), "alices-box");
        assert_eq!(
            redact("root su ab", &no_names(), Some("root")),
            "root su ab"
        );
        assert_eq!(redact("ab at ab", &no_names(), Some("ab")), "ab at ab");
    }

    #[test]
    fn machine_and_boot_ids_keep_four_characters() {
        assert_eq!(
            r(
                "_MACHINE_ID=0123456789abcdef0123456789abcdef _BOOT_ID=FEDCBA9876543210fedcba9876543210"
            ),
            "_MACHINE_ID=…cdef _BOOT_ID=…3210"
        );
        assert_eq!(r("id 0123456789abcdef0123456789abcdef."), "id …cdef.");
        // A UUID keeps its dashes and stays.
        let uuid = "00000000-1111-4222-8333-444444444444";
        assert_eq!(r(uuid), uuid);
    }

    #[test]
    fn mac_addresses_are_hidden() {
        assert_eq!(r("link enxaabbccddeeff is up"), "link enx<MAC> is up");
        assert_eq!(r("link wlx001122334455 up"), "link wlx<MAC> up");
        assert_eq!(r("peer AA-BB-CC-DD-EE-FF seen"), "peer <MAC> seen");
        assert_eq!(r("peer aa:bb:cc:dd:ee:ff seen"), "peer <MAC> seen");
        // Ordinary interface names stay readable.
        assert_eq!(r("wlp2s0 enp3s0 cellglide0"), "wlp2s0 enp3s0 cellglide0");
    }

    #[test]
    fn wifi_names_are_hidden() {
        assert_eq!(
            r("NetworkManager[1]: <info> starting connection 'Alice Home 5G' (1b2c)"),
            "NetworkManager[1]: <info> starting connection <网络名> (1b2c)"
        );
        assert_eq!(r("ssid \"Cafe Wifi\" joined"), "ssid <网络名> joined");
        assert_eq!(r("ssid 'Cafe' joined"), "ssid <网络名> joined");
        assert_eq!(r("SSID=HomeNet signal 70"), "SSID=<网络名> signal 70");
        assert_eq!(r("ssid: HomeNet, band 5"), "ssid: <网络名>, band 5");
        assert_eq!(r("SSID=\"My Net\" x"), "SSID=<网络名> x");
        // The word alone is not a name.
        assert_eq!(
            r("connection failed, no ssid known"),
            "connection failed, no ssid known"
        );
    }

    // Task 8 minor (b): an apostrophe inside a quoted name does not end it.
    #[test]
    fn wifi_name_with_an_apostrophe_is_hidden_whole() {
        assert_eq!(
            r("NetworkManager[1]: <info> connection 'Alice's iPhone'"),
            "NetworkManager[1]: <info> connection <网络名>"
        );
        assert_eq!(
            r("starting connection 'Alice's iPhone' (1b2c)"),
            "starting connection <网络名> (1b2c)"
        );
        assert_eq!(r("ssid 'Joe's Cafe'"), "ssid <网络名>");
        // An ordinary name followed by more text still stops at its own closing quote.
        assert_eq!(
            r("connection 'Home' is up, other"),
            "connection <网络名> is up, other"
        );
    }

    // Task 8 minor (c): `ssid:` at the end of a line must not eat the next line.
    #[test]
    fn bare_ssid_does_not_cross_a_line() {
        assert_eq!(
            r("ssid:
next line"),
            "ssid:
next line"
        );
        assert_eq!(
            r("ssid =
  Home"),
            "ssid =
  Home"
        );
        assert_eq!(
            r("connection
'Home'"),
            "connection
'Home'"
        );
    }

    // Task 8 minor (a): common default logins are not redacted everywhere.
    #[test]
    fn default_logins_are_left_alone() {
        for login in [
            "ubuntu", "debian", "fedora", "dev", "test", "admin", "user", "pi",
        ] {
            let text = format!("session opened for {login} by (uid=0), a {login}ed test");
            assert_eq!(redact(&text, &no_names(), Some(login)), text, "{login}");
            let named = names(&[(login, "X")]);
            assert_eq!(redact(&text, &named, None), text, "{login} as a name");
        }
    }

    #[test]
    fn ipv6_zone_is_hidden_and_run_user_stays() {
        assert_eq!(
            r("peer fe80::acb4:1cc8:43aa:ecae%wlp2s0 up"),
            "peer fe80::/16 up"
        );
        assert_eq!(
            r("bus /run/user/1000/bus and /run/user/1000"),
            "bus /run/user/1000/bus and /run/user/1000"
        );
    }

    #[test]
    fn log_levels_are_not_host_names() {
        for level in ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"] {
            let line = format!("2026-10-16T04:00:00.123456Z {level} cg_core::tunnel: boom");
            assert_eq!(r(&line), line);
        }
    }

    #[test]
    fn a_failed_journal_match_drops_the_rest() {
        let text = "Oct 16 12:00:00 box a[1]: x\nOct 16 12:00:01 box a[1]: y";
        let one = JOURNAL_HOST.captures_iter(text).take(1);
        let broken = std::iter::once(Err(fancy_regex::Error::RuntimeError(
            fancy_regex::RuntimeError::BacktrackLimitExceeded,
        )));
        assert_eq!(
            journal_host_from(text, &no_names(), one.chain(broken)),
            "Oct 16 12:00:00 <主机><已隐去>"
        );
    }
}
