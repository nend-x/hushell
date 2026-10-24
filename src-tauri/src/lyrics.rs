//! lyrics.rs — the lyrics widget's engine.
//!
//! The playing track's name comes from the SMTC watcher (media.rs); this
//! module takes the name and asks SEVERAL public lyric sites at once. Every
//! source below was requested by hand before it was coded — the request
//! bodies, headers and response paths here are the verified ones:
//!
//! * LRCLIB      GET  https://lrclib.net/api/get?...&duration=  (+ /api/search
//!               fallback) — JSON {syncedLyrics, plainLyrics}; needs a UA —
//!               throttled clients (503/429) are retried twice with backoff;
//!               every request carries the Lrclib-Client header (a throttled
//!               window served a header-carrying request while the bare one
//!               503'd).
//! * QQ Music    POST https://u.y.qq.com/cgi-bin/musicu.fcg (JSON body,
//!               module music.search.SearchCgiService) -> song mid; then
//!               GET c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg ->
//!               base64-encoded LRC.
//! * NetEase     POST https://music.163.com/api/search/get/ (form body, a
//!               plain GET /api/search/get/web answers with an encrypted
//!               blob) -> song id; then GET /api/song/lyric?id=...&lv=1 ->
//!               lrc.lyric (LRC text).
//! * Kugou       GET https://songsearch.kugou.com/song_search_v2 -> hash;
//!               GET http://krcs.kugou.com/search?...&hash= -> id+accesskey;
//!               GET http://krcs.kugou.com/download?...&fmt=lrc -> base64 LRC.
//! * lyrics.ovh  GET https://api.lyrics.ovh/v1/{artist}/{title} -> plain text.
//!
//! Genius, Musixmatch and ChartLyrics were probed too and LEFT OUT: Genius
//! serves a bot challenge to non-browsers, Musixmatch's anonymous token
//! endpoint hands every caller an all-zero token, ChartLyrics is gone (404).
//!
//! Ranking is the user's rule: what TWO sites agree on beats what one site
//! claims, and synced (timestamped) lyrics beat plain text at equal trust.
//! The widget only ever sees the winner plus how many sites backed it.
//!
//! THE SONG'S OWN TIME is a first-class search input everywhere a catalog
//! allows it: QQ (`interval`), NetEase (`duration`, ms) and Kugou
//! (`HQDuration`/`Duration`, seconds — the old read divided them by 1000,
//! so the tiebreaker never fired) pick the candidate cut closest to the
//! playing duration instead of blindly trusting the first search hit, and
//! the ranking demotes any winner whose length runs 12+ s off the playing
//! song (a live cut, a remix — its stamps would mislight every line). A
//! search that ran with NO duration is invalidated when a real one arrives:
//! the cache stores the duration it was built with.
//!
//! THE TITLE ITSELF outranks the clock: every catalog search is fuzzy, and
//! a stylized title ("Sssshhhiiittt!") can come back as five songs by four
//! unrelated artists — the duration-only pick then served whoever was
//! nearest. Every candidate now scores title identity (containment, word
//! Jaccard, length-gated char-bigram Dice) with a 10-point weight, and a
//! source whose best candidate shares nothing with the playing title
//! ABSTAINS — no lyrics beats wrong lyrics. The gate is deliberately
//! tighter on short titles: a 5-letter word holds four bigrams, and one
//! accidental shared pair used to grade a same-artist neighbor up to a
//! pass ("цветы" vs "цвети" scored 0.75). And when a player publishes the
//! COMBINED string as the title ("Sssshhhiiittt! - цветы"), the artist
//! prefix is cut once at the entry so the artist's own words can never
//! inflate a same-artist candidate's similarity.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::util::log;

/// One lyric line of a synced (LRC) result: seconds from track start.
#[derive(Serialize, Clone, Default)]
pub struct LyricLine {
    pub t: f64,
    pub text: String,
}

/// One word of a per-word (karaoke) line: absolute seconds from track start,
/// its duration, the text, and whether a space belongs BEFORE it (latin
/// words need one; CJK glues tight).
#[derive(Serialize, Clone, Default)]
pub struct LyricWord {
    pub t: f64,
    pub d: f64,
    pub w: String,
    pub sp: bool,
}

/// One per-word line: the line's start (matches the synced `lines` twin at
/// the same index), plus the timed words inside it.
#[derive(Serialize, Clone, Default)]
pub struct LyricWordLine {
    pub t: f64,
    pub words: Vec<LyricWord>,
}

/// What the widget renders. `lines` carries the synced result, `plain` the
/// unsynced one — exactly one of them is filled when `found` is true. When
/// `words` is non-empty the song also carries per-word karaoke timing (the
/// widget lights the words inside the active line one by one).
#[derive(Serialize, Clone, Default)]
pub struct LyricsDto {
    pub found: bool,
    pub synced: bool,
    /// Per-word karaoke lines (NetEase YRC) — parallel to `lines` when the
    /// winner carried word timing. Empty for plain LRC results.
    pub words: Vec<LyricWordLine>,
    /// How many sources returned THE SAME lyric as the winner (1 = only the
    /// winner itself).
    pub agreement: usize,
    pub sources: Vec<String>,
    pub lines: Vec<LyricLine>,
    pub plain: String,
}

/// The event payload: the key tells the widget which request this answers.
#[derive(Serialize, Clone)]
pub struct LyricsMsg {
    pub key: String,
    pub lyrics: LyricsDto,
}

/// One cache entry: the lyric AND the song duration the search ran with.
/// A search that ran with no duration (the player had not published a
/// timeline yet when the track changed) can match the wrong cut — a live
/// version, a remix — so a REAL duration arriving later must invalidate it.
#[derive(Clone)]
struct CachedLyrics {
    dto: LyricsDto,
    dur_used: f64,
}

fn cache_slot() -> &'static Mutex<HashMap<String, CachedLyrics>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, CachedLyrics>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn inflight_slot() -> &'static Mutex<HashSet<String>> {
    static INFLIGHT: std::sync::OnceLock<Mutex<HashSet<String>>> =
        std::sync::OnceLock::new();
    INFLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// The identity the widget tracks: raw title + artist from the media
/// payload (the widget composes the same string — no normalization to
/// disagree about).
fn track_key(title: &str, artist: &str) -> String {
    format!("{}\u{1}{}", title, artist)
}

/// Entry point (the `lyrics_fetch` command). Returns:
/// * `Some(dto)` — answered from cache / nothing playing: render at once;
/// * `None`      — a search is running, the answer arrives as `hush:lyrics`.
pub fn fetch(
    app: &AppHandle,
    title: &str,
    artist: &str,
    album: &str,
    duration: f64,
) -> Option<LyricsDto> {
    if title.trim().is_empty() {
        return Some(LyricsDto::default()); // nothing playing
    }
    let key = track_key(title, artist);
    {
        let hit = cache_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        if let Some(c) = hit {
            // A duration-blind result stays in the cache only until a real
            // duration shows up — then the search re-runs so every source
            // gets to match THE TIME OF THE SONG, not just its name.
            if !(c.dur_used <= 0.5 && duration > 3.0) {
                return Some(c.dto);
            }
        }
    }
    {
        let mut inf = inflight_slot().lock().unwrap_or_else(|e| e.into_inner());
        if !inf.insert(key.clone()) {
            return None; // a search for this track is already running
        }
    }
    let a = app.clone();
    // The cache key keeps the RAW media payload (the widget composes the
    // same string); the SEARCH gets the stripped title — see
    // strip_artist_prefix.
    let search_title = strip_artist_prefix(title, artist);
    let title = search_title;
    let artist = artist.to_string();
    let album = album.to_string();
    let tkey = key.clone();
    let spawned = std::thread::Builder::new()
        .name("lyrics-fetch".into())
        .spawn(move || {
            crate::util::register_thread("lyrics-fetch");
            let dto = run_search(&title, &artist, &album, duration);
            cache_slot()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    tkey.clone(),
                    CachedLyrics {
                        dto: dto.clone(),
                        dur_used: duration,
                    },
                );
            inflight_slot()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&tkey);
            let _ = a.emit("hush:lyrics", &LyricsMsg { key: tkey, lyrics: dto });
        });
    if spawned.is_err() {
        inflight_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }
    None
}

// ---------------------------------------------------------------------------
// The five sources
// ---------------------------------------------------------------------------

struct SourceHit {
    source: &'static str,
    synced: bool,
    lrc: Option<String>,
    plain: Option<String>,
    /// Per-word karaoke lines (NetEase YRC) when the source carried them.
    words: Option<Vec<LyricWordLine>>,
    dur: Option<f64>,
    base: f64, // trust weight of the site itself
    /// The candidate's OWN track name, as the catalog spells it — the
    /// ranking's title-identity voice. None = the source had no candidate
    /// list (lyrics.ovh fetches by the exact path, it cannot mis-pick).
    title: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(9))
        .user_agent("hushell-shell/1.0 (lyrics widget)")
        .build()
}

/// The etiquette header LRCLIB's API asks clients to send. Probed live:
/// during a throttle window a request WITH it got served while the same
/// URL without it got 503 — the shell always carries it now.
const LRCLIB_CLIENT: &str = "hushell 1.0.0 (desktop shell lyrics widget)";

/// One LRCLIB GET with throttle survival: LRCLIB answers bursts of
/// searches with 503/429, and it is frequently a song's ONLY carrier (a
/// stylized underground track the CN catalogs do not list at all) — one
/// throttle then means NO lyrics at all. Every request carries the
/// Lrclib-Client header; on a throttle the read backs off twice (1.2 s,
/// 2.5 s) before giving up. The retries live on the source's own thread —
/// the other four sources never wait for them. 404 (no record) and
/// transport errors are NOT retried: nothing arrived and nothing will.
fn lrclib_get(http: &ureq::Agent, url: &str) -> Option<serde_json::Value> {
    for (attempt, wait) in [(0u32, 0u64), (1, 1200), (2, 2500)] {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(wait));
        }
        match http.get(url).set("Lrclib-Client", LRCLIB_CLIENT).call() {
            Ok(resp) => return resp.into_json::<serde_json::Value>().ok(),
            Err(ureq::Error::Status(code, _)) if code == 503 || code == 429 => {
                log(&format!("lyrics: lrclib throttled ({code}) — retry {attempt}/2"));
                continue;
            }
            Err(_) => return None,
        }
    }
    None
}

/// Percent-encode one query/path component (unreserved chars pass through).
fn qenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// The search string the catalog sites match against.
fn query_of(artist: &str, title: &str) -> String {
    format!("{} {}", artist.trim(), title.trim())
}

/// Lowercase alphanumeric residue of a title — punctuation, emoji and
/// casing are cosmetics the catalogs sprinkle differently ("Sssshhhiiittt!"
/// vs "Sssshhhiiittt", "(Explicit)" suffixes, full-width CJK brackets).
/// Every script survives (Cyrillic, Latin, CJK) — only junk is dropped.
fn norm_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut sp = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            sp = false;
        } else if !sp {
            out.push(' ');
            sp = true;
        }
    }
    out.trim().to_string()
}

/// How much a candidate's title IS the playing title, 0..1. Ordered voices:
///   * exact normalized equality — 1.0;
///   * containment — the shorter title's words form a contiguous whole-word
///     run inside the longer (≥ 4 alphanumeric chars) — 0.85 ("цветы" vs
///     "цветы (Explicit)", "как я спасал солнце" vs its "(Radio Edit)"
///     twin, but never "день" inside "деньги");
///   * word-token Jaccard (multi-word titles) and char-bigram Dice — but
///     the bigram voice only speaks when BOTH sides carry ≥ 8 alphanumeric
///     chars. A 5-letter word holds four bigrams, and one accidental pair
///     graded noise up to a pass: "цветы" vs "цвети" scored 0.75, "цветы"
///     vs "ты не я" 0.22 — the same-artist wrong songs the 0.15 gate let
///     through. A stylized 12-char one-word title ("Sssshhhiiittt") keeps
///     its 11 bigrams, where the voice still earns its keep.
fn title_sim(a: &str, b: &str) -> f64 {
    let (a, b) = (norm_title(a), norm_title(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let slim = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    let (sa, sb) = (slim(&a), slim(&b));
    let (la, lb) = (sa.chars().count(), sb.chars().count());
    // Containment as a contiguous whole-word run: "цветы" matches
    // "цветы (Explicit)" and "как я спасал солнце" matches its "(Radio
    // Edit)" twin, but "день" does NOT match "деньги" — a mid-word hit is
    // a different word, the run must align on word boundaries.
    if la.min(lb) >= 4 {
        let (long_t, short_t) = if la <= lb {
            (
                b.split_whitespace().collect::<Vec<&str>>(),
                a.split_whitespace().collect::<Vec<&str>>(),
            )
        } else {
            (
                a.split_whitespace().collect::<Vec<&str>>(),
                b.split_whitespace().collect::<Vec<&str>>(),
            )
        };
        if short_t.len() <= long_t.len()
            && long_t.windows(short_t.len()).any(|w| w == short_t.as_slice())
        {
            return 0.85;
        }
    }
    let at: HashSet<&str> = a.split_whitespace().collect();
    let bt: HashSet<&str> = b.split_whitespace().collect();
    let jac = {
        let inter = at.intersection(&bt).count();
        let uni = at.union(&bt).count();
        if uni > 0 {
            inter as f64 / uni as f64
        } else {
            0.0
        }
    };
    let dice = if la >= 8 && lb >= 8 {
        let bigrams = |s: &str| -> HashSet<(char, char)> {
            s.chars().collect::<Vec<_>>().windows(2).map(|w| (w[0], w[1])).collect()
        };
        let (ab, bb) = (bigrams(&sa), bigrams(&sb));
        if ab.len() + bb.len() > 0 {
            2.0 * ab.intersection(&bb).count() as f64 / (ab.len() + bb.len()) as f64
        } else {
            0.0
        }
    } else {
        0.0
    };
    jac.max(dice)
}

/// A candidate whose title shares nothing with the playing title is not the
/// song — the catalog's fuzzy engine just matched the artist words (probed
/// live: "как я спасал солнце Sssshhhiiittt!" pulls five songs by four
/// unrelated artists). A source without a title-matching candidate ABSTAINS
/// instead of serving its nearest-duration neighbor. 0.25, not 0.15: the
/// combined-title leak ("Artist - Song" as the playing title) shares one
/// artist token with any same-artist candidate — Jaccard 1/5 = 0.2 used to
/// pass; a same-artist wrong song must not ride a single shared word.
const TITLE_MISS: f64 = 0.25;

/// Some players (browsers especially) publish the COMBINED string as the
/// SMTC title: "Sssshhhiiittt! - цветы". The artist's own words then
/// pollute the query AND the title gate — every same-artist candidate
/// shares them. Cut the artist off once, at the entry, before anything
/// downstream sees the title. Conservative: only a leading "<artist><sep>"
/// head that normalizes EXACTLY to the artist is removed.
fn strip_artist_prefix(title: &str, artist: &str) -> String {
    let t = title.trim();
    let ar = artist.trim();
    if ar.is_empty() {
        return t.to_string();
    }
    let ar_norm = norm_title(ar);
    if ar_norm.is_empty() {
        return t.to_string();
    }
    for sep in [" - ", " – ", " — ", " ― ", ": "] {
        if let Some(pos) = t.find(sep) {
            let head = &t[..pos];
            let rest = t[pos + sep.len()..].trim();
            if norm_title(head) == ar_norm && !rest.is_empty() {
                log(&format!(
                    "lyrics: title carried the artist prefix — searching for {:?} instead",
                    rest
                ));
                return rest.to_string();
            }
        }
    }
    t.to_string()
}

/// The song's own time as a candidate voice, one tier set for every source:
/// a cut within 2 s of what is playing is almost certainly the same take
/// (+4), within 8 s is catalog tolerance (+1), past 20 s off is a different
/// version (live / remix / radio edit) and scores NEGATIVE (−2).
fn dur_tier(d: f64, playing: f64) -> f64 {
    if playing <= 0.0 || d <= 0.0 {
        return 0.0;
    }
    let diff = (d - playing).abs();
    if diff <= 2.0 {
        4.0
    } else if diff <= 8.0 {
        1.0
    } else if diff > 20.0 {
        -2.0
    } else {
        0.0
    }
}

/// LRCLIB: exact fetch first (needs album + duration), search as fallback.
fn src_lrclib(http: &ureq::Agent, artist: &str, title: &str, album: &str, dur: f64) -> Option<SourceHit> {
    // /api/get requires every parameter; without album/duration go straight
    // to /api/search.
    if !album.trim().is_empty() && dur > 0.0 {
        let url = format!(
            "https://lrclib.net/api/get?artist_name={}&track_name={}&album_name={}&duration={:.2}",
            qenc(artist.trim()),
            qenc(title.trim()),
            qenc(album.trim()),
            dur
        );
        if let Some(v) = lrclib_get(http, &url) {
            // /api/get is keyed on the exact name — the record's own
            // trackName rides along as the hit's title voice anyway.
            let tn = v
                .get("trackName")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            if let Some(hit) = lrclib_hit(v, dur) {
                return Some(SourceHit {
                    source: "lrclib",
                    synced: hit.0,
                    lrc: hit.1,
                    plain: hit.2,
                    words: None,
                    dur: hit.3,
                    base: 12.0,
                    title: tn.or_else(|| Some(title.trim().to_string())),
                });
            }
        }
    }
    let url = format!(
        "https://lrclib.net/api/search?track_name={}&artist_name={}",
        qenc(title.trim()),
        qenc(artist.trim())
    );
    let v = lrclib_get(http, &url)?;
    let arr = v.as_array()?;
    // Best entry: synced beats plain, THE SONG'S OWN TIME breaks ties, and
    // the title identity outranks both — a same-titler at a loose duration
    // is still the song, a different titler at Δ0 s is somebody else's
    // track the fuzzy engine put in the list.
    let mut best: Option<(f64, f64, serde_json::Value)> = None; // (score, sim, entry)
    for e in arr {
        let synced = e
            .get("syncedLyrics")
            .and_then(|x| x.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let d = e.get("duration").and_then(|x| x.as_f64());
        let sim = e
            .get("trackName")
            .and_then(|x| x.as_str())
            .map(|n| title_sim(n, title))
            .unwrap_or(0.0);
        let mut score = if synced { 2.0 } else { 0.0 };
        if let Some(dd) = d {
            score += dur_tier(dd, dur);
        }
        score += 10.0 * sim;
        if best
            .as_ref()
            .map(|(bs, _, _)| score > *bs)
            .unwrap_or(true)
        {
            best = Some((score, sim, e.clone()));
        }
    }
    let (_, sim, e) = best?;
    if sim < TITLE_MISS {
        let tn = e.get("trackName").and_then(|x| x.as_str()).unwrap_or("?");
        log(&format!(
            "lyrics: lrclib abstained — best candidate {:?} shares nothing with {:?}",
            tn, title
        ));
        return None;
    }
    let tn = e.get("trackName").and_then(|x| x.as_str()).map(|s| s.to_string());
    lrclib_hit(e, dur).map(|hit| SourceHit {
        source: "lrclib",
        synced: hit.0,
        lrc: hit.1,
        plain: hit.2,
        words: None,
        dur: hit.3,
        base: 12.0,
        title: tn,
    })
}

/// (synced, lrc, plain, duration) out of one LRCLIB JSON record.
fn lrclib_hit(
    v: serde_json::Value,
    _dur: f64,
) -> Option<(bool, Option<String>, Option<String>, Option<f64>)> {
    let synced = v
        .get("syncedLyrics")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let plain = v
        .get("plainLyrics")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if synced.is_none() && plain.is_none() {
        return None;
    }
    let d = v.get("duration").and_then(|x| x.as_f64());
    Some((synced.is_some(), synced, plain, d))
}

/// QQ Music: desktop search module -> best song mid -> base64 LRC. The
/// search returns the usual five candidates and the FIRST one is whatever
/// the catalog ranks hottest — often a live cut or a re-recording when the
/// query is thin. The playing song's OWN duration picks among them: the
/// candidate whose `interval` sits closest to it is the same take. Without
/// a duration the catalog's first candidate is kept (nothing better to
/// compare against).
fn src_qq(http: &ureq::Agent, artist: &str, title: &str, dur: f64) -> Option<SourceHit> {
    let body = serde_json::json!({
        "comm": {"ct": 19, "cv": 1859, "uin": 0},
        "req": {
            "module": "music.search.SearchCgiService",
            "method": "DoSearchForQQMusicDesktop",
            "param": {
                "search_type": 0,
                "query": query_of(artist, title),
                "page_num": 1,
                "num_per_page": 5
            }
        }
    });
    let resp = http
        .post("https://u.y.qq.com/cgi-bin/musicu.fcg")
        .set("Referer", "https://y.qq.com/")
        .send_json(body)
        .ok()?;
    let v = resp.into_json::<serde_json::Value>().ok()?;
    let list = v
        .pointer("/req/data/body/song/list")
        .and_then(|x| x.as_array())?;
    let mut pick: Option<(&serde_json::Value, f64, f64)> = None; // (entry, score, sim)
    for e in list.iter().take(5) {
        let mid_ok = e
            .get("mid")
            .and_then(|x| x.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !mid_ok {
            continue;
        }
        let iv = e.get("interval").and_then(|x| x.as_f64()).unwrap_or(0.0); // seconds
        let sim = e
            .get("name")
            .and_then(|x| x.as_str())
            .map(|n| title_sim(n, title))
            .unwrap_or(0.0);
        let score = 10.0 * sim + dur_tier(iv, dur);
        if pick.map(|(_, bs, _)| score > bs).unwrap_or(true) {
            pick = Some((e, score, sim));
        }
    }
    let (e, _, sim) = pick?;
    if sim < TITLE_MISS {
        let nm = e.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        log(&format!(
            "lyrics: qq abstained — best candidate {:?} shares nothing with {:?}",
            nm, title
        ));
        return None;
    }
    let mid = e
        .get("mid")
        .and_then(|x| x.as_str())?
        .to_string();
    let iv = e.get("interval").and_then(|x| x.as_f64()).unwrap_or(0.0);

    let url = format!(
        "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg?songmid={}&g_tk=5381&format=json&loginUin=0",
        qenc(&mid)
    );
    let resp = http
        .get(&url)
        .set("Referer", "https://y.qq.com/")
        .call()
        .ok()?;
    let v = resp.into_json::<serde_json::Value>().ok()?;
    let b64 = v.get("lyric").and_then(|x| x.as_str())?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .ok()?;
    let lrc = String::from_utf8_lossy(&raw).to_string();
    if lrc.trim().is_empty() {
        return None;
    }
    Some(SourceHit {
        source: "qq",
        synced: true,
        lrc: Some(lrc),
        plain: None,
        words: None,
        dur: if iv > 0.0 { Some(iv) } else { None },
        base: 8.0,
        title: e.get("name").and_then(|x| x.as_str()).map(|s| s.to_string()),
    })
}

/// NetEase: the search endpoint answers a FORM post (a plain GET answers
/// with an encrypted blob) -> best song id. The candidates each carry their
/// `duration` in MILLISECONDS; the one closest to the playing song's own
/// time is the same take (the first hit is often a live cut). Then TWO
/// lyric reads:
///   * GET /api/song/lyric?id=..&lv=1 -> lrc.lyric (plain LRC text), and
///   * GET /api/song/lyric/v1?id=..&yv=2 -> yrc.lyric — the per-word
///     karaoke format, `[start,dur](start,dur,0)word(start,dur,0)word…`
///     (milliseconds), probed live: 孤勇者/泡沫 carry it, most songs answer
///     without the yrc field and fall back to the LRC read.
fn src_netease(http: &ureq::Agent, artist: &str, title: &str, dur: f64) -> Option<SourceHit> {
    let resp = http
        .post("https://music.163.com/api/search/get/")
        .set("Referer", "https://music.163.com")
        .set("Cookie", "os=pc; appver=2.10.13")
        .send_form(&[
            ("s", query_of(artist, title).as_str()),
            ("type", "1"),
            ("limit", "5"),
            ("offset", "0"),
        ])
        .ok()?;
    let v = resp.into_json::<serde_json::Value>().ok()?;
    let songs = v.pointer("/result/songs").and_then(|x| x.as_array())?;
    let mut pick: Option<(i64, f64, f64, f64)> = None; // (id, score, sim, seconds)
    for e in songs.iter().take(5) {
        let id = match e.get("id").and_then(|x| x.as_i64()) {
            Some(i) if i > 0 => i,
            _ => continue,
        };
        let ds = e.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0) / 1000.0;
        let sim = e
            .get("name")
            .and_then(|x| x.as_str())
            .map(|n| title_sim(n, title))
            .unwrap_or(0.0);
        let score = 10.0 * sim + dur_tier(ds, dur);
        if pick
            .map(|(_, bs, _, _)| score > bs)
            .unwrap_or(true)
        {
            pick = Some((id, score, sim, ds));
        }
    }
    let (id, _, sim, ds) = pick?;
    if sim < TITLE_MISS {
        let nm = songs
            .iter()
            .find(|e| e.get("id").and_then(|x| x.as_i64()) == Some(id))
            .and_then(|e| e.get("name"))
            .and_then(|x| x.as_str())
            .unwrap_or("?");
        log(&format!(
            "lyrics: netease abstained — best candidate {:?} shares nothing with {:?}",
            nm, title
        ));
        return None;
    }
    // The search response is consumed by the lyric reads below — keep the
    // picked candidate's own name now, it is the hit's title voice.
    let nm = songs
        .iter()
        .find(|e| e.get("id").and_then(|x| x.as_i64()) == Some(id))
        .and_then(|e| e.get("name"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    // The v1 endpoint carries the per-word YRC when the song has karaoke
    // data. Its lrc field is a JSON-object blob — NOT parseable LRC — so it
    // is used for yrc only; the line-level LRC keeps coming from the old
    // endpoint below.
    let words = {
        let url = format!(
            "https://music.163.com/api/song/lyric/v1?id={}&cv=170730&lv=0&kv=0&tv=0&rv=0&ro=0&yv=2",
            id
        );
        http.get(&url)
            .set("Referer", "https://music.163.com")
            .call()
            .ok()
            .and_then(|r| r.into_json::<serde_json::Value>().ok())
            .and_then(|v| {
                v.pointer("/yrc/lyric")
                    .and_then(|x| x.as_str())
                    .and_then(|s| parse_yrc(s))
            })
    };

    let url = format!(
        "https://music.163.com/api/song/lyric?id={}&lv=1&kv=1&tv=-1",
        id
    );
    let resp = http
        .get(&url)
        .set("Referer", "https://music.163.com")
        .call()
        .ok()?;
    let v = resp.into_json::<serde_json::Value>().ok()?;
    let lrc = v
        .pointer("/lrc/lyric")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if lrc.is_none() && words.is_none() {
        return None;
    }
    Some(SourceHit {
        source: "netease",
        synced: true,
        lrc,
        plain: None,
        words,
        dur: if ds > 0.0 { Some(ds) } else { None },
        base: 8.0,
        title: nm,
    })
}

/// Kugou: catalog search -> lyric-server candidate -> base64 LRC download.
/// The catalog returns five candidates and the first is whatever ranks
/// hottest — a remix with the same title ranks above the studio take. The
/// playing song's own duration picks among them: `HQDuration` / `Duration`
/// are SECONDS (probed live: Creep -> 238), and the old read divided them
/// by 1000 — the widget then believed every Kugou lyric was 0.2 s long and
/// the duration tiebreaker never once fired. The picked length also feeds
/// krcs' own `duration` hint (MILLISECONDS there) so the lyric server
/// itself prefers the candidate cut that matches.
fn src_kugou(http: &ureq::Agent, artist: &str, title: &str, dur: f64) -> Option<SourceHit> {
    let url = format!(
        "https://songsearch.kugou.com/song_search_v2?keyword={}&page=1&pagesize=5",
        qenc(&query_of(artist, title))
    );
    let resp = http.get(&url).call().ok()?;
    let v = resp.into_json::<serde_json::Value>().ok()?;
    let lists = v.pointer("/data/lists").and_then(|x| x.as_array())?;
    let mut pick: Option<(String, f64, f64, f64, String)> = None; // (hash, score, sim, seconds, name)
    for e in lists.iter().take(5) {
        let hash = e
            .get("HQFileHash")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                e.get("FileHash")
                    .and_then(|x| x.as_str())
                    .filter(|s| !s.trim().is_empty())
            });
        let Some(hash) = hash else { continue };
        let iv = e
            .get("HQDuration")
            .and_then(|x| x.as_f64())
            .filter(|s| *s > 0.0)
            .or_else(|| e.get("Duration").and_then(|x| x.as_f64()))
            .unwrap_or(0.0); // SECONDS
        let nm = e
            .get("SongName")
            .or_else(|| e.get("songname"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let sim = if nm.is_empty() {
            0.0
        } else {
            title_sim(&nm, title)
        };
        let score = 10.0 * sim + dur_tier(iv, dur);
        if pick
            .as_ref()
            .map(|(_, bs, _, _, _)| score > *bs)
            .unwrap_or(true)
        {
            pick = Some((hash.to_string(), score, sim, iv, nm));
        }
    }
    let (hash, _, sim, iv, nm) = pick?;
    if sim < TITLE_MISS {
        log(&format!(
            "lyrics: kugou abstained — its best candidate shares nothing with {:?}",
            title
        ));
        return None;
    }

    let url = format!(
        "http://krcs.kugou.com/search?ver=1&man=yes&client=mobi&keyword={}&duration={}&hash={}",
        qenc(&query_of(artist, title)),
        if dur > 0.0 { ((dur * 1000.0) as i64).to_string() } else { String::new() },
        qenc(&hash)
    );
    let resp = http.get(&url).call().ok()?;
    let v = resp.into_json::<serde_json::Value>().ok()?;
    let cand = v.get("candidates")?.as_array()?.first()?.clone();
    let cid = cand.get("id").and_then(|x| x.as_str())?.to_string();
    let ak = cand
        .get("accesskey")
        .and_then(|x| x.as_str())?
        .to_string();

    let url = format!(
        "http://krcs.kugou.com/download?ver=1&client=pc&id={}&accesskey={}&fmt=lrc&charset=utf8",
        qenc(&cid),
        qenc(&ak)
    );
    let resp = http.get(&url).call().ok()?;
    let v = resp.into_json::<serde_json::Value>().ok()?;
    let b64 = v.get("content").and_then(|x| x.as_str())?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .ok()?;
    let lrc = String::from_utf8_lossy(&raw).to_string();
    if lrc.trim().is_empty() {
        return None;
    }
    Some(SourceHit {
        source: "kugou",
        synced: true,
        lrc: Some(lrc),
        plain: None,
        words: None,
        dur: if iv > 0.0 { Some(iv) } else { None },
        base: 8.0,
        title: if nm.is_empty() { None } else { Some(nm) },
    })
}

/// lyrics.ovh: the simplest API of the set — artist/title in the path,
/// plain text back. Slow sometimes, hence its low trust weight.
fn src_lyricsovh(http: &ureq::Agent, artist: &str, title: &str) -> Option<SourceHit> {
    let url = format!(
        "https://api.lyrics.ovh/v1/{}/{}",
        qenc(artist.trim()),
        qenc(title.trim())
    );
    let resp = http.get(&url).call().ok()?;
    let v = resp.into_json::<serde_json::Value>().ok()?;
    let text = v
        .get("lyrics")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with("Something went wrong"))?;
    Some(SourceHit {
        source: "lyricsovh",
        synced: false,
        lrc: None,
        plain: Some(text),
        words: None,
        dur: None,
        base: 2.0,
        title: None,
    })
}

// ---------------------------------------------------------------------------
// Search + ranking
// ---------------------------------------------------------------------------

/// The song's own time as a ranking voice: a candidate cut that MATCHES the
/// playing duration earns trust (+6); one that runs 12+ s long or short is
/// a different version whose stamps would mislight every line (−5).
fn dur_bonus(d: Option<f64>, playing: f64) -> f64 {
    match d {
        Some(d) if playing > 0.0 => {
            let diff = (d - playing).abs();
            if diff <= 3.0 {
                6.0
            } else if diff > 12.0 {
                -5.0
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

/// What one source actually delivered, VALIDATED: a hit that claimed synced
/// but whose LRC parses into fewer than two stamped lines is demoted to
/// plain (that is exactly how an unsynced result used to sneak past the
/// ranking — the site claimed synced, the parser produced nothing, the
/// widget fell back to plain text).
struct Resolved {
    source: &'static str,
    base: f64,
    dur: Option<f64>,
    lines: Vec<LyricLine>,     // empty for plain
    words: Vec<LyricWordLine>, // per-word karaoke (rides on lines)
    plain: String,             // empty for synced
}

impl Resolved {
    fn is_synced(&self) -> bool {
        !self.lines.is_empty()
    }
    /// The lyric as one normalized content stream for the shingle metric.
    fn content_text(&self) -> String {
        if !self.lines.is_empty() {
            self.lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            self.plain.clone()
        }
    }
}

fn resolve_hit(h: &SourceHit, title: &str, artist: &str) -> Resolved {
    let (source, base, dur) = (h.source, h.base, h.dur);
    // Per-word karaoke first: the line-level projection is derived from the
    // words themselves (line text = its words joined), so `lines` and
    // `words` can never disagree.
    if let Some(w) = &h.words {
        if w.len() >= 2 {
            let lines = w
                .iter()
                .map(|wl| LyricLine {
                    t: wl.t,
                    text: yrc_line_text(&wl.words),
                })
                .collect();
            return Resolved {
                source,
                base,
                dur,
                lines,
                words: w.clone(),
                plain: String::new(),
            };
        }
    }
    if h.synced {
        if let Some(lrc) = &h.lrc {
            let parsed = parse_lrc_full(lrc, title, artist);
            if parsed.lines.len() >= 2 {
                let mut words = parsed.words;
                // karaoke rides on the lines — the arrays are parallel by
                // construction; a desync (cannot happen, but never render a
                // mislit word) drops the word timing, not the lyrics
                if words.len() != parsed.lines.len() {
                    words = Vec::new();
                }
                return Resolved {
                    source,
                    base,
                    dur,
                    lines: parsed.lines,
                    words,
                    plain: String::new(),
                };
            }
            // claimed synced, no stamps: demote to the plain text twin
            let plain = match &h.plain {
                Some(p) if !p.trim().is_empty() => p.clone(),
                _ => plain_of_lrc(lrc, title, artist),
            };
            return Resolved {
                source,
                base,
                dur,
                lines: Vec::new(),
                words: Vec::new(),
                plain,
            };
        }
    }
    Resolved {
        source,
        base,
        dur,
        lines: Vec::new(),
        words: Vec::new(),
        plain: h.plain.clone().unwrap_or_default(),
    }
}

fn run_search(title: &str, artist: &str, album: &str, duration: f64) -> LyricsDto {
    let http = agent();
    let mut hits: Vec<SourceHit> = Vec::new();
    // One thread per source — a slow site (lyrics.ovh) must not hold the
    // fast ones back; the whole search is bounded by the slowest, ~9 s.
    let t = title.trim();
    let ar = artist.trim();
    std::thread::scope(|s| {
        let h1 = s.spawn(|| src_lrclib(&http, ar, t, album.trim(), duration));
        let h2 = s.spawn(|| src_qq(&http, ar, t, duration));
        let h3 = s.spawn(|| src_netease(&http, ar, t, duration));
        let h4 = s.spawn(|| src_kugou(&http, ar, t, duration));
        let h5 = s.spawn(|| src_lyricsovh(&http, ar, t));
        for hd in [h1, h2, h3, h4, h5] {
            if let Ok(Some(hit)) = hd.join() {
                hits.push(hit);
            }
        }
    });

    if hits.is_empty() {
        log(&format!(
            "lyrics: \"{}\" / \"{}\" — no source had it",
            title, artist
        ));
        return LyricsDto::default();
    }

    let resolved: Vec<Resolved> = hits
        .iter()
        .map(|h| resolve_hit(h, title, artist))
        .collect();

    // Agreement needs a content metric that survives the catalogs' cosmetic
    // differences (line splits, ad-lib rows, punctuation) but still rejects
    // a DIFFERENT lyric — verified against real pulls: the same song across
    // LRCLIB/QQ/NetEase/lyrics.ovh lands at Jaccard 0.58..0.91 while a remix
    // (same chorus, different verses) stays below 0.53. Word-8-gram shingle
    // Jaccard with the 0.55 cut sits exactly in that gap.
    let shingles: Vec<HashSet<u64>> = resolved
        .iter()
        .map(|r| {
            let pseudo = SourceHit {
                source: r.source,
                synced: r.is_synced(),
                lrc: None,
                plain: Some(r.content_text()),
                words: None,
                dur: r.dur,
                base: r.base,
                title: None,
            };
            shingles_of_hit(&pseudo, title, artist)
        })
        .collect();
    let agree_of = |i: usize| -> usize {
        let mut n = 0usize;
        for sj in shingles.iter() {
            if shingle_jaccard(&shingles[i], sj) >= 55 {
                n += 1;
            }
        }
        n.max(1)
    };

    // THE RULE (fixed after the plain-text leak): SYNCED ALWAYS WINS. The
    // old scoring let agreement dominate the kind — two catalogs shipping
    // the same transcription beat the one catalog that had it timestamped,
    // and the user read plain text under a synced star. Now the synced
    // bucket is picked first whenever ANY source carries parseable stamps;
    // agreement ranks WITHIN the bucket. Plain text only renders when no
    // synced result exists at all. The title-identity voice speaks in both
    // buckets: each source abstained unless its candidate IS the song, but
    // a loose spelling ("Song (album version)") still outranks a tighter
    // one from a wrong candidate.
    let sim_of =
        |i: usize| -> f64 { hits[i].title.as_deref().map(|t| title_sim(t, title)).unwrap_or(0.0) };
    let synced_idx: Vec<usize> = (0..resolved.len())
        .filter(|i| resolved[*i].is_synced())
        .collect();
    let (best_i, winner_synced) = if !synced_idx.is_empty() {
        let mut best = synced_idx[0];
        let mut best_score = f64::MIN;
        for i in synced_idx {
            let mut score = (agree_of(i) as f64) * 100.0;
            if !resolved[i].words.is_empty() {
                score += 15.0; // karaoke-grade timing — a real tiebreaker
            }
            score += resolved[i].base;
            score += dur_bonus(resolved[i].dur, duration);
            score += 10.0 * sim_of(i);
            if score > best_score {
                best_score = score;
                best = i;
            }
        }
        (best, true)
    } else {
        let mut best = 0usize;
        let mut best_score = f64::MIN;
        for i in 0..resolved.len() {
            let mut score = (agree_of(i) as f64) * 100.0 + resolved[i].base;
            score += dur_bonus(resolved[i].dur, duration);
            score += 10.0 * sim_of(i);
            if score > best_score {
                best_score = score;
                best = i;
            }
        }
        (best, false)
    };

    let agree: Vec<usize> = (0..resolved.len())
        .filter(|i| *i == best_i || shingle_jaccard(&shingles[best_i], &shingles[*i]) >= 55)
        .collect();

    let winner = &resolved[best_i];
    let mut sources = vec![winner.source.to_string()];
    for i in &agree {
        if *i != best_i {
            sources.push(resolved[*i].source.to_string());
        }
    }

    let dto = LyricsDto {
        found: winner.is_synced() || !winner.plain.trim().is_empty(),
        synced: winner_synced,
        words: winner.words.clone(),
        agreement: agree.len().max(1),
        sources,
        lines: winner.lines.clone(),
        plain: winner.plain.clone(),
    };
    log(&format!(
        "lyrics: \"{}\" / \"{}\" — {}{}, {} source(s) agree{}{}",
        title,
        artist,
        if dto.synced { "synced" } else { "text" },
        if dto.found { "" } else { " (empty)" },
        dto.agreement,
        if !dto.words.is_empty() { ", per-word" } else { "" },
        match &hits[best_i].title {
            Some(t) => format!(", matched {:?}", t),
            None => String::new(),
        },
    ));
    dto
}

// ---------------------------------------------------------------------------
// NetEase YRC — the per-word karaoke format
// ---------------------------------------------------------------------------

/// Parse a YRC body into per-word lines. Two line shapes:
///   * `{"t":..,"c":[{"tx":..},..]}` — metadata cards (JSON), dropped;
///   * `[startMs,durMs](startMs,durMs,0)word(startMs,durMs,0)word…` — the
///     karaoke lines the widget lives on. Verified live on 孤勇者/泡沫.
fn parse_yrc(src: &str) -> Option<Vec<LyricWordLine>> {
    let mut out: Vec<LyricWordLine> = Vec::new();
    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('{') {
            continue; // JSON metadata card
        }
        let rest = match line.strip_prefix('[') {
            Some(r) => r,
            None => continue,
        };
        let close = match rest.find(']') {
            Some(i) => i,
            None => continue,
        };
        let head = &rest[..close];
        let mut head_it = head.split(',');
        // A malformed head skips the line — one weird row must not cost the
        // whole song its karaoke.
        let start_ms: f64 = match head_it.next().and_then(|s| s.trim().parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let _dur_ms: f64 = head_it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
        let mut body = rest[close + 1..].trim_start();
        let mut words: Vec<LyricWord> = Vec::new();
        let mut bad = false;
        while body.starts_with('(') {
            let wclose = match body.find(')') {
                Some(i) => i,
                None => break,
            };
            let timing = &body[1..wclose];
            let mut t_it = timing.split(',');
            let wt: f64 = match t_it.next().and_then(|s| s.trim().parse().ok()) {
                Some(v) => v,
                None => {
                    bad = true;
                    break;
                }
            };
            let wd: f64 = t_it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
            body = body[wclose + 1..].trim_start();
            // the word text runs until the next '(' — but a literal '(' can
            // be part of a lyric; YRC always starts the next word with '(',
            // so scanning to the next '(' is the format's own rule
            let wend = body.find('(').unwrap_or(body.len());
            let text = body[..wend].trim().to_string();
            body = body[wend..].trim_start();
            if text.is_empty() {
                continue;
            }
            let prev_last = words
                .last()
                .and_then(|p: &LyricWord| p.w.chars().last())
                .unwrap_or(' ');
            let first = text.chars().next().unwrap_or(' ');
            // latin<->latin boundary needs a space; CJK glues tight
            let sp = words.len() > 0
                && (prev_last.is_ascii_alphanumeric() && first.is_ascii_alphanumeric());
            words.push(LyricWord {
                t: wt / 1000.0,
                d: wd / 1000.0,
                w: text,
                sp,
            });
        }
        if bad || words.is_empty() {
            continue;
        }
        out.push(LyricWordLine {
            t: start_ms / 1000.0,
            words,
        });
    }
    if out.len() < 2 {
        return None; // too little to claim karaoke
    }
    out.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    if out.len() > 400 {
        out.truncate(400);
    }
    Some(out)
}

/// The readable line text of one YRC line: words joined, honoring the `sp`
/// space-before flags.
fn yrc_line_text(words: &[LyricWord]) -> String {
    let mut out = String::new();
    for w in words {
        if w.sp && !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w.w);
    }
    out
}

// ---------------------------------------------------------------------------
// LRC handling
// ---------------------------------------------------------------------------

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64(data: &[u8], seed: u64) -> u64 {
    let mut h = seed;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// The credit headers catalogs prepend to their LRC (作词/作曲 cards and
/// their english twins). A line STARTING with one of these is metadata.
/// QQ Music's house style is the SHORT form — "词：黄家驹" / "曲：黄家驹" —
/// which the long names miss; the bare 词/曲 heads are matched with the
/// colon required so a lyric that merely starts with the character 词
/// (rare, but possible) survives.
fn is_credit_line(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return true;
    }
    const HEADS: &[&str] = &[
        "作词", "作詞", "填词", "填詞", "作曲", "编曲", "編曲", "混音", "母带", "母帶",
        "和声", "和聲", "监制", "監製", "制作", "製作", "出品", "发行", "發行", "推广",
        "推廣", "lyrics", "composed", "written by", "produced", "arranged", "music by",
        "词：", "词:", "曲：", "曲:", "词曲", "編寫", "编写", "录音", "錄音", "混缩",
        "吉他", "贝斯", "貝斯", "键盘", "鍵盤", "鼓：", "弦乐", "配器", "和音",
    ];
    HEADS.iter().any(|h| t.starts_with(h))
}

/// The catalogs' title card — "[00:00.00]Creep - Radiohead" — is metadata,
/// not a lyric. Only a line at the very top that carries BOTH the title and
/// the artist is dropped (a real opening lyric rarely names both).
fn is_title_card(s: &str, title: &str, artist: &str) -> bool {
    let t = s.to_lowercase();
    !title.trim().is_empty()
        && !artist.trim().is_empty()
        && t.contains(&title.trim().to_lowercase())
        && t.contains(&artist.trim().to_lowercase())
}

/// Timestamp tag "[mm:ss.xx]" — seconds, or None when the tag is metadata.
fn parse_stamp(tag: &str) -> Option<f64> {
    let mut it = tag.split(':');
    let m: f64 = it.next()?.trim().parse().ok()?;
    let s: f64 = it.next()?.trim().parse().ok()?;
    if m < 0.0 || s < 0.0 {
        return None;
    }
    Some(m * 60.0 + s)
}

/// One parsed LRC body: the display lines AND the per-word karaoke lines —
/// ALWAYS index-parallel (a line without word tags carries one whole-line
/// word), so `words[i]` belongs to `lines[i]` for every i.
pub struct ParsedLrc {
    pub lines: Vec<LyricLine>,
    pub words: Vec<LyricWordLine>,
}

pub fn parse_lrc(src: &str, title: &str, artist: &str) -> Vec<LyricLine> {
    parse_lrc_full(src, title, artist).lines
}

/// Split one A2 enhanced-LRC line into (optional tag time, text) chunks —
/// `<mm:ss.xx>` tags inside the line text stamp the words that follow them.
/// `None` when the line carries no parseable tag (a plain LRC line).
fn split_word_tags(text: &str) -> Option<Vec<(Option<f64>, String)>> {
    if !text.contains('<') {
        return None;
    }
    let mut out: Vec<(Option<f64>, String)> = Vec::new();
    let mut cur = String::new();
    // A2 semantics: a tag stamps the words AFTER it. The chunk before the
    // first tag has no time of its own — it rides the line's own stamp.
    let mut pending: Option<f64> = None;
    let mut i = 0usize;
    let mut any = false;
    while i < text.len() {
        if text.as_bytes()[i] == b'<' {
            if let Some(close) = text[i..].find('>') {
                let inner = &text[i + 1..i + close];
                if let Some(t) = parse_stamp(inner) {
                    out.push((pending, std::mem::take(&mut cur)));
                    pending = Some(t);
                    any = true;
                    i += close + 1;
                    continue;
                }
            }
        }
        let ch = text[i..].chars().next()?;
        cur.push(ch);
        i += ch.len_utf8();
    }
    if !any {
        return None;
    }
    out.push((pending, cur));
    Some(out)
}

/// Parse an LRC body into display lines: multi-stamps expand, the offset
/// tag applies (positive = show sooner), metadata and credit cards drop.
/// A2 enhanced word tags become per-word karaoke timing (the public
/// per-word lyrics the widget lights word by word); tag-less lines still
/// yield a whole-line word so the parallel array never desynchronizes.
pub fn parse_lrc_full(src: &str, title: &str, artist: &str) -> ParsedLrc {
    let mut offset = 0f64;
    let mut out: Vec<(LyricLine, LyricWordLine)> = Vec::new();
    for line in src.lines() {
        let mut rest = line.trim();
        let mut stamps: Vec<f64> = Vec::new();
        loop {
            if !rest.starts_with('[') {
                break;
            }
            let close = match rest.find(']') {
                Some(i) => i,
                None => break,
            };
            let tag = &rest[1..close];
            rest = rest[close + 1..].trim_start();
            if let Some(v) = tag.strip_prefix("offset") {
                let ms = v
                    .trim_start_matches(':')
                    .trim()
                    .parse::<f64>()
                    .unwrap_or(0.0);
                offset = ms / 1000.0;
                continue;
            }
            if let Some(t) = parse_stamp(tag) {
                stamps.push(t);
            }
            // anything else ([ti:] [ar:] [al:] [by:] …) is metadata — drop
        }
        let text = rest.trim();
        if text.is_empty() {
            continue;
        }
        if is_credit_line(text) {
            continue;
        }
        if stamps.first().map(|t| *t <= 1.5).unwrap_or(false)
            && is_title_card(text, title, artist)
        {
            continue;
        }
        if stamps.is_empty() {
            continue;
        }
        // Word tags? Build the karaoke words once (for the first stamp),
        // then clone them (shifted) for every additional stamp of the line.
        let chunks = split_word_tags(text);
        let karaoke = chunks
            .as_ref()
            .map(|c| c.len() > 1 || c.first().map_or(false, |(t, _)| t.is_some()))
            .unwrap_or(false);
        if !karaoke {
            for t in stamps {
                let lt = (t - offset).max(0.0);
                out.push((
                    LyricLine {
                        t: lt,
                        text: text.to_string(),
                    },
                    LyricWordLine {
                        t: lt,
                        words: vec![LyricWord {
                            t: lt,
                            d: 0.0,
                            w: text.to_string(),
                            sp: false,
                        }],
                    },
                ));
            }
            continue;
        }
        let chunks = chunks.unwrap_or_default();
        let base_t = (stamps[0] - offset).max(0.0);
        let mut ws: Vec<LyricWord> = Vec::new();
        for (tt, chunk) in &chunks {
            let txt = chunk.trim();
            if txt.is_empty() {
                continue; // a tag hugging the line start / another tag
            }
            let wt = (tt.unwrap_or(stamps[0]) - offset).max(0.0);
            let prev_last = ws
                .last()
                .and_then(|p: &LyricWord| p.w.chars().last())
                .unwrap_or(' ');
            let first = txt.chars().next().unwrap_or(' ');
            // latin<->latin boundary needs a space; CJK glues tight
            let sp = !ws.is_empty()
                && prev_last.is_ascii_alphanumeric()
                && first.is_ascii_alphanumeric();
            ws.push(LyricWord {
                t: wt,
                d: 0.0,
                w: txt.to_string(),
                sp,
            });
        }
        if ws.is_empty() {
            continue;
        }
        for i in 0..ws.len() {
            if i + 1 < ws.len() {
                ws[i].d = (ws[i + 1].t - ws[i].t).max(0.0);
            }
        }
        let line_text = yrc_line_text(&ws);
        if line_text.is_empty() {
            continue;
        }
        for t in stamps {
            let lt = (t - offset).max(0.0);
            let delta = lt - base_t;
            let mut shifted = ws.clone();
            if delta.abs() > 0.001 {
                for w in shifted.iter_mut() {
                    w.t = (w.t + delta).max(0.0);
                }
            }
            out.push((
                LyricLine {
                    t: lt,
                    text: line_text.clone(),
                },
                LyricWordLine {
                    t: lt,
                    words: shifted,
                },
            ));
        }
    }
    out.sort_by(|a, b| {
        a.0.t
            .partial_cmp(&b.0.t)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Dedup identical stamps (catalogs repeat the same line as multi-stamp
    // variants on their own rows) and cap the payload at something sane.
    out.dedup_by(|a, b| (a.0.t - b.0.t).abs() < 0.05 && a.0.text == b.0.text);
    if out.len() > 400 {
        out.truncate(400);
    }
    let mut lines = Vec::with_capacity(out.len());
    let mut words = Vec::with_capacity(out.len());
    for (l, w) in out {
        lines.push(l);
        words.push(w);
    }
    ParsedLrc { lines, words }
}

/// The plain-text twin of an LRC body (content lines only, in order).
fn plain_of_lrc(src: &str, title: &str, artist: &str) -> String {
    let lines = parse_lrc(src, title, artist);
    let mut out = String::new();
    let mut prev = String::new();
    for l in lines {
        if l.text == prev {
            continue; // multi-stamp lines became duplicate rows after parsing
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&l.text);
        prev = l.text;
    }
    out
}

/// The hit's lyric as ONE normalized word stream: tags stripped, credit and
/// title cards dropped, every word lowercased. Line boundaries vanish — a
/// catalog that splits a line in two cannot change the stream.
fn content_words_of_hit(h: &SourceHit, title: &str, artist: &str) -> Vec<String> {
    fn push_line(line: &str, title: &str, artist: &str, words: &mut Vec<String>) {
        let t = line.trim();
        if t.is_empty() || is_credit_line(t) || is_title_card(t, title, artist) {
            return;
        }
        let mut cur = String::new();
        for c in t.chars() {
            if c.is_alphanumeric() {
                cur.extend(c.to_lowercase());
            } else if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            words.push(cur);
        }
    }
    let mut words: Vec<String> = Vec::new();
    if let Some(lrc) = &h.lrc {
        for line in lrc.lines() {
            // strip every leading [..] tag (multi-stamp rows included)
            let mut rest = line.trim();
            loop {
                if !rest.starts_with('[') {
                    break;
                }
                match rest.find(']') {
                    Some(i) => rest = rest[i + 1..].trim_start(),
                    None => break,
                }
            }
            push_line(rest, title, artist, &mut words);
        }
    } else if let Some(p) = &h.plain {
        for line in p.lines() {
            push_line(line, title, artist, &mut words);
        }
    }
    words
}

/// Word-8-gram shingles of the content stream, hashed. A handful of windows
/// straddling an inserted ad-lib row die; the rest match a line-split twin.
fn shingles_of_hit(h: &SourceHit, title: &str, artist: &str) -> HashSet<u64> {
    let words = content_words_of_hit(h, title, artist);
    let n = 8usize;
    let mut set = HashSet::new();
    if words.len() < n + 4 {
        return set; // too little content to claim anything
    }
    for i in 0..=(words.len() - n) {
        let joined = words[i..i + n].join(" ");
        set.insert(fnv1a64(joined.as_bytes(), FNV_OFFSET));
    }
    set
}

/// Jaccard of two shingle sets scaled to 0..100 (integer only). Empty sets
/// never agree.
fn shingle_jaccard(a: &HashSet<u64>, b: &HashSet<u64>) -> u32 {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        return 0;
    }
    ((inter * 100) / union) as u32
}
