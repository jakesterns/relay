//! The headphone catalogue: search a bundled index, fetch a curve on demand.
//!
//! Relay ships the *index* — 8,849 model names and where each one lives — but
//! never the measurements. Those are licensed CC BY-NC-SA by oratory1990,
//! crinacle and the other contributors, so redistributing them in a product
//! would breach the licence. The curve for a model the user picks is fetched
//! from the source at that moment, cached under the data root so it happens
//! once, and credited to its measurer in the UI.
//!
//! Two consequences shape this module:
//!
//! - The index is a file beside the executable, not `include_str!`. At 751 KB
//!   it would otherwise sit in the always-on core's working set forever to
//!   serve a search the user runs a handful of times. It is read, scanned and
//!   dropped.
//! - This is the only outbound network call Relay makes. It is user-initiated
//!   (picking a model), goes to one host, and is skipped entirely once the
//!   curve is cached.
//!
//! Regenerate the index with `scripts/build-catalog.ps1`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Where the curves come from. Also what the UI must credit.
pub const SOURCE_REPO: &str = "https://github.com/jaakkopasanen/AutoEq";
const RAW_BASE: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/";

/// Index file name, shipped next to the binaries.
pub const INDEX_FILE: &str = "autoeq-index.tsv";

/// One measured model. A given headphone appears once per measurement source,
/// because the same headphone measured on different rigs genuinely needs
/// different correction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    /// Who measured it, e.g. "oratory1990".
    pub source: String,
    /// The measurement rig, where the source published more than one.
    pub rig: String,
    /// Percent-encoded path under the results directory.
    pub path: String,
}

impl CatalogEntry {
    /// A stable id for the library entry this becomes.
    pub fn slug(&self) -> String {
        let mut s = String::with_capacity(self.name.len() + self.source.len() + 1);
        for part in [self.name.as_str(), self.source.as_str()] {
            // Names ending in punctuation — "(ANC Off)" — would otherwise
            // produce a doubled separator here.
            if !s.is_empty() && !s.ends_with('-') {
                s.push('-');
            }
            for ch in part.chars() {
                if ch.is_ascii_alphanumeric() {
                    s.push(ch.to_ascii_lowercase());
                } else if !s.ends_with('-') {
                    s.push('-');
                }
            }
        }
        s.trim_matches('-').to_string()
    }

    /// The results CSV — the same shape `autoeq::parse_curve` already reads,
    /// including the `equalization` column it prefers.
    pub fn curve_url(&self) -> String {
        // The last path segment is the model directory name, and the CSV
        // inside is named after it.
        let leaf = self.path.rsplit('/').next().unwrap_or(&self.path);
        format!("{RAW_BASE}{}/{leaf}.csv", self.path)
    }

    /// Where a fetched curve is cached.
    pub fn cache_path(&self, cache_dir: &Path) -> PathBuf {
        cache_dir.join(format!("{}.csv", self.slug()))
    }

    /// Attribution line for the UI. Not decoration: the licence requires it.
    pub fn credit(&self) -> String {
        if self.rig.is_empty() {
            format!("Measured by {} · via AutoEQ", self.source)
        } else {
            format!("Measured by {} on {} · via AutoEQ", self.source, self.rig)
        }
    }
}

/// The index file, beside the running executable. Falls back to the repo
/// layout so `cargo run` works from a development tree.
pub fn index_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the running executable")?;
    let dir = exe.parent().context("executable has no parent directory")?;
    let candidates = [
        dir.join(INDEX_FILE),
        dir.join("catalog").join(INDEX_FILE),
        // target/debug/deps/<test> -> repo root
        dir.join("..")
            .join("..")
            .join("..")
            .join("crates")
            .join("core")
            .join("catalog")
            .join(INDEX_FILE),
        dir.join("..").join("..").join("crates").join("core").join("catalog").join(INDEX_FILE),
    ];
    for c in candidates {
        if c.exists() {
            return Ok(c);
        }
    }
    bail!("{INDEX_FILE} not found next to {}", dir.display())
}

/// Parse one index line: `name \t source \t rig \t path`.
fn parse_line(line: &str) -> Option<CatalogEntry> {
    let mut f = line.split('\t');
    let name = f.next()?.trim();
    let source = f.next()?.trim();
    let rig = f.next().unwrap_or("").trim();
    let path = f.next()?.trim();
    if name.is_empty() || path.is_empty() {
        return None;
    }
    Some(CatalogEntry {
        name: name.to_string(),
        source: source.to_string(),
        rig: rig.to_string(),
        path: path.to_string(),
    })
}

/// Rank a candidate against a lower-cased query. Lower is better; `None`
/// means it does not match at all.
///
/// Ordering is chosen so that typing "hd 560" puts "HD 560S" above "Beyer
/// DT 560 HD Edition": an earlier match wins, and among equal positions the
/// shorter name wins because it has less unmatched text around the query.
fn rank(name_lower: &str, query: &str) -> Option<usize> {
    let at = name_lower.find(query)?;
    // A match at a word boundary beats one buried mid-token.
    let boundary = at == 0 || name_lower.as_bytes()[at - 1].is_ascii_whitespace();
    Some(at * 4 + usize::from(!boundary) * 2 + name_lower.len() / 64)
}

/// Search the index. Matches every whitespace-separated term, in any order,
/// so "sennheiser 560" and "560 sennheiser" both find the HD 560S.
pub fn search(index: &Path, query: &str, limit: usize) -> Result<Vec<CatalogEntry>> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let terms: Vec<&str> = query.split_whitespace().collect();
    let text =
        std::fs::read_to_string(index).with_context(|| format!("reading {}", index.display()))?;

    let mut hits: Vec<(usize, CatalogEntry)> = Vec::new();
    for line in text.lines() {
        let Some(entry) = parse_line(line) else { continue };
        let lower = entry.name.to_lowercase();
        // Every term must appear; the score is the worst term's rank so a
        // model that matches all terms tightly beats one that barely does.
        let mut score = 0usize;
        let mut ok = true;
        for t in &terms {
            match rank(&lower, t) {
                Some(r) => score = score.max(r),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            hits.push((score, entry));
        }
    }
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| a.1.name.len().cmp(&b.1.name.len())).then_with(|| {
            // Stable, and puts the most-cited measurer first on ties.
            source_rank(&a.1.source).cmp(&source_rank(&b.1.source))
        })
    });
    hits.truncate(limit);
    Ok(hits.into_iter().map(|(_, e)| e).collect())
}

/// Preference between sources when everything else ties. oratory1990's
/// Harman-target results are the ones AutoEQ itself leads with.
fn source_rank(source: &str) -> u8 {
    match source {
        "oratory1990" => 0,
        "crinacle" => 1,
        "Rtings" => 2,
        _ => 3,
    }
}

/// The curve for `entry`, from the cache if it is there and from the source
/// if it is not. Returns the parsed `(Hz, dB)` correction plus whether the
/// network was used, so the UI can say "downloaded" the first time.
pub fn curve(entry: &CatalogEntry, cache_dir: &Path) -> Result<(Vec<(f32, f32)>, bool)> {
    let cached = entry.cache_path(cache_dir);
    if let Ok(text) = std::fs::read_to_string(&cached) {
        if let Ok(points) = super::autoeq::parse_curve(&text) {
            return Ok((points, false));
        }
        // A corrupt cache entry is not worth failing over; re-fetch.
    }

    let url = entry.curve_url();
    let text = http_get(&url).with_context(|| format!("fetching {url}"))?;
    let points = super::autoeq::parse_curve(&text)
        .with_context(|| format!("parsing the measurement from {url}"))?;

    std::fs::create_dir_all(cache_dir).ok();
    let tmp = cached.with_extension("csv.tmp");
    if std::fs::write(&tmp, &text).is_ok() {
        let _ = std::fs::rename(&tmp, &cached);
    }
    Ok((points, true))
}

#[cfg(windows)]
fn http_get(url: &str) -> Result<String> {
    imp::http_get(url)
}

#[cfg(not(windows))]
fn http_get(_url: &str) -> Result<String> {
    bail!("fetching measurements is Windows-only")
}

/// HTTPS GET over WinHTTP.
///
/// WinHTTP rather than an HTTP crate because the always-on core is on a
/// 10 MB budget and `reqwest` plus a TLS stack would cost several megabytes
/// of binary for one user-initiated request per headphone. The API is already
/// linked.
#[cfg(windows)]
mod imp {
    use super::*;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
        WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    };

    /// Cap on a downloaded measurement. The largest AutoEQ CSV is ~60 KB; a
    /// megabyte means something is wrong and we should not buffer it.
    const MAX_BYTES: usize = 1024 * 1024;

    struct Handle(*mut std::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: every handle here came from a WinHttp* open call
                // and is closed exactly once.
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    pub fn http_get(url: &str) -> Result<String> {
        let (host, path) = split_url(url)?;
        let agent = HSTRING::from("Relay");
        let host_w = HSTRING::from(host.as_str());
        let path_w = HSTRING::from(path.as_str());
        let verb = HSTRING::from("GET");

        // SAFETY: handles are wrapped so every early return closes them; all
        // strings outlive their calls.
        unsafe {
            let session = Handle(WinHttpOpen(
                PCWSTR(agent.as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ));
            if session.0.is_null() {
                bail!("WinHttpOpen failed: {}", std::io::Error::last_os_error());
            }
            let connect = Handle(WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), 443, 0));
            if connect.0.is_null() {
                bail!("cannot reach {host}: {}", std::io::Error::last_os_error());
            }
            let request = Handle(WinHttpOpenRequest(
                connect.0,
                PCWSTR(verb.as_ptr()),
                PCWSTR(path_w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null_mut(),
                WINHTTP_OPEN_REQUEST_FLAGS(WINHTTP_FLAG_SECURE.0),
            ));
            if request.0.is_null() {
                bail!("WinHttpOpenRequest failed: {}", std::io::Error::last_os_error());
            }
            WinHttpSendRequest(request.0, None, None, 0, 0, 0)
                .map_err(|e| anyhow::anyhow!("sending the request: {e}"))?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut())
                .map_err(|e| anyhow::anyhow!("reading the response: {e}"))?;

            let mut status: u32 = 0;
            let mut len = std::mem::size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut _),
                &mut len,
                std::ptr::null_mut(),
            )
            .map_err(|e| anyhow::anyhow!("reading the status line: {e}"))?;
            if status == 404 {
                bail!("the measurement is no longer at that path (HTTP 404); rebuild the index");
            }
            if !(200..300).contains(&status) {
                bail!("HTTP {status}");
            }

            let mut out: Vec<u8> = Vec::new();
            let mut buf = [0u8; 16 * 1024];
            loop {
                let mut read: u32 = 0;
                WinHttpReadData(request.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read)
                    .map_err(|e| anyhow::anyhow!("reading the body: {e}"))?;
                if read == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..read as usize]);
                if out.len() > MAX_BYTES {
                    bail!("the measurement is larger than {MAX_BYTES} bytes; refusing it");
                }
            }
            String::from_utf8(out).context("the measurement was not valid UTF-8")
        }
    }

    /// `https://host/path` → `(host, /path)`. Only https is accepted: the
    /// port is hard-coded to 443 and there is no reason to fetch a
    /// measurement in the clear.
    pub(super) fn split_url(url: &str) -> Result<(String, String)> {
        let rest = url.strip_prefix("https://").context("only https URLs are fetched")?;
        match rest.split_once('/') {
            Some((host, path)) if !host.is_empty() => Ok((host.to_string(), format!("/{path}"))),
            _ => bail!("no path in {url}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> CatalogEntry {
        CatalogEntry {
            name: "Sennheiser HD 560S".into(),
            source: "oratory1990".into(),
            rig: String::new(),
            path: "oratory1990/over-ear/Sennheiser%20HD%20560S".into(),
        }
    }

    #[test]
    fn the_csv_url_is_the_directory_plus_its_own_name() {
        assert_eq!(
            entry().curve_url(),
            "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/\
             oratory1990/over-ear/Sennheiser%20HD%20560S/Sennheiser%20HD%20560S.csv"
        );
    }

    #[test]
    fn a_model_with_brackets_in_its_name_still_resolves() {
        // A quarter of the catalogue looks like this.
        let e = CatalogEntry {
            name: "1MORE Aero (ANC Off)".into(),
            source: "HypetheSonics".into(),
            rig: "GRAS RA0045".into(),
            path: "HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20Off)".into(),
        };
        assert!(e.curve_url().ends_with("1MORE%20Aero%20(ANC%20Off).csv"), "{}", e.curve_url());
        assert_eq!(e.slug(), "1more-aero-anc-off-hypethesonics");
    }

    #[test]
    fn the_slug_separates_model_from_measurer() {
        // Two sources for one headphone must not collide in the library.
        let a = entry();
        let b = CatalogEntry { source: "crinacle".into(), ..entry() };
        assert_ne!(a.slug(), b.slug());
        assert_eq!(a.slug(), "sennheiser-hd-560s-oratory1990");
    }

    #[test]
    fn credit_names_the_measurer_and_the_rig() {
        assert_eq!(entry().credit(), "Measured by oratory1990 · via AutoEQ");
        let with_rig = CatalogEntry { rig: "GRAS 43AG-7".into(), ..entry() };
        assert!(with_rig.credit().contains("on GRAS 43AG-7"));
    }

    #[test]
    fn ranking_prefers_earlier_word_boundary_matches() {
        let early = rank("hd 560s", "hd").unwrap();
        let buried = rank("beyer dt 560 hd edition", "hd").unwrap();
        assert!(early < buried, "{early} vs {buried}");
    }

    #[test]
    fn a_term_that_is_absent_does_not_match() {
        assert!(rank("sennheiser hd 560s", "focal").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn urls_split_into_host_and_path() {
        let (h, p) = imp::split_url("https://example.com/a/b.csv").unwrap();
        assert_eq!(h, "example.com");
        assert_eq!(p, "/a/b.csv");
        assert!(imp::split_url("http://example.com/x").is_err(), "plain http is refused");
        assert!(imp::split_url("https://example.com").is_err(), "a bare host has no path");
    }

    #[test]
    fn searching_the_real_index_finds_a_known_headphone() {
        let Ok(index) = index_path() else {
            eprintln!("skipped: catalogue index not staged next to the test binary");
            return;
        };
        let hits = search(&index, "hd 560s", 10).expect("search");
        assert!(!hits.is_empty(), "the HD 560S is in the catalogue");
        assert!(hits[0].name.to_lowercase().contains("560s"), "got {:?}", hits[0]);
        // Several sources measured it; all of them should be offered.
        assert!(hits.len() > 1, "more than one measurement source: {hits:?}");
    }

    #[test]
    fn search_matches_terms_in_any_order() {
        let Ok(index) = index_path() else { return };
        let a = search(&index, "sennheiser 560s", 5).expect("search");
        let b = search(&index, "560s sennheiser", 5).expect("search");
        assert_eq!(a.first().map(|e| &e.name), b.first().map(|e| &e.name));
        assert!(!a.is_empty());
    }

    #[test]
    fn an_empty_query_returns_nothing_rather_than_everything() {
        let Ok(index) = index_path() else { return };
        assert!(search(&index, "   ", 10).unwrap().is_empty());
    }
}
