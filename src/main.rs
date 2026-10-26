//! Where each Pocket Sense release is on its way to F-Droid:
//! tagged on GitHub → picked up in fdroiddata → built → published.
//! With cats.

use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};
use ratatui::{DefaultTerminal, Frame};
use serde_json::Value;

const PKG: &str = "ace.software.pocketsense";
const GH_REPO: &str = "software-ace/PocketSense";
const AUTO_REFRESH: Duration = Duration::from_secs(15 * 60);
/// One animation frame.
const TICK: Duration = Duration::from_millis(120);

/// One F-Droid build cycle, from its status JSON.
#[derive(Default)]
struct Cycle {
    start: Option<u64>,
    end: Option<u64>,
    built: HashSet<u64>,
    failed: HashSet<u64>,
}

#[derive(Default)]
struct Data {
    /// versionName → its GitHub release date, or None for a tag without
    /// a published release (yet)
    github: BTreeMap<String, Option<String>>,
    /// versionName → the date f-droid.org added it
    fdroid_dates: BTreeMap<String, String>,
    /// versionName → versionCodes, from fdroiddata and f-droid.org (which
    /// keeps versions fdroiddata has since dropped)
    codes: BTreeMap<String, Vec<u64>>,
    current_version: Option<String>,
    published: HashSet<u64>,
    suggested: Option<u64>,
    last: Cycle,
    running: Cycle,
    errors: Vec<String>,
    fetched_at: Option<u64>,
}

fn get(url: &str) -> Result<String, String> {
    ureq::get(url)
        .header("User-Agent", "fdroid-watch")
        .call()
        .and_then(|mut r| r.body_mut().with_config().limit(64 << 20).read_to_string())
        .map_err(|e| format!("{url}: {e}"))
}

fn get_json(url: &str) -> Result<Value, String> {
    serde_json::from_str(&get(url)?).map_err(|e| format!("{url}: {e}"))
}

fn fetch() -> Data {
    let mut d = Data { fetched_at: Some(now_ms()), ..Default::default() };

    // Tags are what F-Droid's bot reacts to; releases add the date once the
    // release workflow has published one.
    for (what, key) in [("tags", "name"), ("releases", "tag_name")] {
        match get_json(&format!("https://api.github.com/repos/{GH_REPO}/{what}?per_page=15")) {
            Ok(Value::Array(items)) => {
                for r in items {
                    let name = r[key].as_str().unwrap_or_default().trim_start_matches('v').to_string();
                    let date = r["published_at"].as_str().filter(|_| r["draft"] != true).and_then(|s| s.get(..10));
                    let entry = d.github.entry(name).or_default();
                    if let Some(date) = date {
                        *entry = Some(date.to_string());
                    }
                }
            }
            Ok(other) => d.errors.push(format!("GitHub {what}: {}", other["message"].as_str().unwrap_or("unexpected reply"))),
            Err(e) => d.errors.push(e),
        }
    }

    match get(&format!("https://gitlab.com/fdroid/fdroiddata/-/raw/master/metadata/{PKG}.yml")) {
        Ok(yml) => (d.codes, d.current_version) = parse_metadata(&yml),
        Err(e) => d.errors.push(e),
    }

    match get_json(&format!("https://f-droid.org/api/v1/packages/{PKG}")) {
        Ok(v) => {
            d.suggested = v["suggestedVersionCode"].as_u64();
            for p in v["packages"].as_array().into_iter().flatten() {
                if let (Some(name), Some(code)) = (p["versionName"].as_str(), p["versionCode"].as_u64()) {
                    d.published.insert(code);
                    let codes = d.codes.entry(name.to_string()).or_default();
                    if !codes.contains(&code) {
                        codes.push(code);
                        codes.sort();
                    }
                }
            }
        }
        // Not on f-droid.org yet is a 404, not an error worth showing.
        Err(e) if e.contains("404") => {}
        Err(e) => d.errors.push(e),
    }

    // The JSON API has no dates; the package page does.
    match get(&format!("https://f-droid.org/en/packages/{PKG}/")) {
        Ok(html) => d.fdroid_dates = parse_added(&html),
        Err(e) if e.contains("404") => {}
        Err(e) => d.errors.push(e),
    }

    for (file, cycle) in [("build", &mut d.last), ("running", &mut d.running)] {
        match get_json(&format!("https://f-droid.org/repo/status/{file}.json")) {
            Ok(v) => *cycle = parse_cycle(&v),
            Err(e) => d.errors.push(e),
        }
    }
    d
}

/// The `- versionName:` / `versionCode:` pairs under Builds, and CurrentVersion.
fn parse_metadata(yml: &str) -> (BTreeMap<String, Vec<u64>>, Option<String>) {
    let (mut builds, mut current, mut name) = (BTreeMap::<String, Vec<u64>>::new(), None, None::<String>);
    for line in yml.lines() {
        let line = line.trim_start_matches([' ', '-']);
        let value = |key: &str| line.strip_prefix(key).map(|v| v.trim().trim_matches(['\'', '"']).to_string());
        if let Some(v) = value("versionName:") {
            name = Some(v);
        } else if let (Some(code), Some(n)) = (value("versionCode:").and_then(|c| c.parse().ok()), &name) {
            builds.entry(n.clone()).or_default().push(code);
        } else if let Some(v) = value("CurrentVersion:") {
            current = Some(v);
        }
    }
    (builds, current)
}

/// versionName → "2026-10-25", from the package page's
/// `<b>Version 2.2.9</b> (5043)` … `Added on Oct 25, 2026` lines.
// ponytail: scrapes HTML; switch to index-v2.json (tens of MB) if the page changes.
fn parse_added(html: &str) -> BTreeMap<String, String> {
    let (mut out, mut name) = (BTreeMap::new(), None::<String>);
    for line in html.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("<b>Version ") {
            name = rest.split("</b>").next().map(str::to_string);
        } else if let (Some(date), Some(n)) = (line.strip_prefix("Added on "), &name) {
            if let Some(iso) = iso_date(date) {
                out.entry(n.clone()).or_insert(iso);
            }
        }
    }
    out
}

/// "Oct 25, 2026" → "2026-10-25".
fn iso_date(s: &str) -> Option<String> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut parts = s.split([' ', ',']).filter(|p| !p.is_empty());
    let (mon, day, year) = (parts.next()?, parts.next()?, parts.next()?);
    let m = MONTHS.iter().position(|&x| mon.starts_with(x))? + 1;
    Some(format!("{year}-{m:02}-{:02}", day.parse::<u8>().ok()?))
}

fn parse_cycle(v: &Value) -> Cycle {
    let ours = |key: &str| -> HashSet<u64> {
        v[key].as_array().into_iter().flatten()
            .filter(|b| b[0].as_str() == Some(PKG))
            .filter_map(|b| b[1].as_u64())
            .collect()
    };
    Cycle { start: v["startTimestamp"].as_u64(), end: v["endTimestamp"].as_u64(), built: ours("successfulBuildIds"), failed: ours("failedBuilds") }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Stage { Skipped, Tagged, Queued, Built, Failed, Live }

fn stage(codes: &[u64], d: &Data) -> Stage {
    let all = |set: &HashSet<u64>| !codes.is_empty() && codes.iter().all(|c| set.contains(c));
    let any = |set: &HashSet<u64>| codes.iter().any(|c| set.contains(c));
    if all(&d.published) {
        Stage::Live
    } else if any(&d.running.failed) || (any(&d.last.failed) && !any(&d.running.built)) {
        Stage::Failed
    } else if any(&d.last.built) || any(&d.running.built) {
        Stage::Built
    } else if !codes.is_empty() {
        Stage::Queued
    } else {
        Stage::Tagged
    }
}

/// Every version worth a row, newest first, with its codes and stage.
fn rows(d: &Data) -> Vec<(String, Vec<u64>, Stage)> {
    // Tags older than the oldest version F-Droid knows came before the app
    // was on F-Droid; untracked ones in between were skipped (the bot only
    // takes the newest tag it sees).
    let known: Vec<Vec<u64>> = d.codes.keys().map(|n| version_key(n)).collect();
    let (oldest, newest) = (known.iter().min(), known.iter().max());
    let mut names: Vec<&String> = d.github.keys().chain(d.codes.keys()).collect::<HashSet<_>>().into_iter()
        .filter(|n| oldest.is_none_or(|o| version_key(n) >= *o))
        .collect();
    names.sort_by_key(|n| std::cmp::Reverse(version_key(n)));
    names.into_iter().map(|n| {
        let codes = d.codes.get(n).cloned().unwrap_or_default();
        let st = match stage(&codes, d) {
            Stage::Tagged if newest.is_some_and(|m| version_key(n) < *m) => Stage::Skipped,
            st => st,
        };
        (n.clone(), codes, st)
    }).collect()
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// "2026-10-25 18:10" UTC, from epoch milliseconds.
fn fmt_ms(ms: u64) -> String {
    let secs = (ms / 1000) % 86400;
    format!("{} {:02}:{:02}", iso((ms / 1000 / 86400) as i64), secs / 3600, secs % 3600 / 60)
}

/// "2026-10-25" for a day number (days since 1970-01-01); Howard Hinnant's
/// civil_from_days.
fn iso(days: i64) -> String {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The day number of "2026-10-25"; days_from_civil, the inverse of [iso].
fn day_number(iso: &str) -> Option<i64> {
    let mut p = iso.get(..10)?.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, d) = (p.next()??, p.next()??, p.next()??);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    Some(era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468)
}

/// The median days from a GitHub release to f-droid.org, over the
/// releases that made it.
fn typical_lag(d: &Data) -> Option<i64> {
    let mut lags: Vec<i64> = d.fdroid_dates.iter()
        .filter_map(|(n, fd)| Some(day_number(fd)? - day_number(d.github.get(n)?.as_ref()?)?))
        .filter(|&lag| lag >= 0)
        .collect();
    lags.sort();
    lags.get(lags.len() / 2).copied()
}

/// When a version not on f-droid.org yet should get there: its GitHub date
/// (or today, for a bare tag) plus the typical lag; never before today.
// ponytail: one median for every stage; weigh in the build cycle if it misleads.
fn estimate(d: &Data, name: &str, today: i64) -> Option<i64> {
    let from = d.github.get(name)?.as_deref().and_then(day_number).unwrap_or(today);
    Some((from + typical_lag(d)?).max(today))
}

fn ago(ms: u64) -> String {
    let h = now_ms().saturating_sub(ms) / 3_600_000;
    if h < 48 { format!("{h}h ago") } else { format!("{}d ago", h / 24) }
}

// ── Art ─────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone, Copy)]
enum Mood { Hunting, Napping, Purring, Hissing }

/// Fetching → hunting; a failed build → hissing; the newest version live →
/// purring; otherwise F-Droid is taking its days → napping.
fn mood(rows: &[(String, Vec<u64>, Stage)], loading: bool) -> Mood {
    if loading {
        Mood::Hunting
    } else if rows.iter().any(|r| r.2 == Stage::Failed) {
        Mood::Hissing
    } else if rows.first().is_some_and(|r| r.2 == Stage::Live) {
        Mood::Purring
    } else {
        Mood::Napping
    }
}

/// 0, 1, … n, n-1, … 1, 0, 1, …
fn bounce(frame: usize, n: usize) -> usize {
    let p = frame % (2 * n);
    if p > n { 2 * n - p } else { p }
}

/// A five-line cat for the mood, at this frame.
fn cat(m: Mood, frame: usize) -> (Vec<String>, &'static str, Color) {
    let blink = frame % 30 < 2;
    match m {
        // After Felix Lee's sleeping cat.
        Mood::Napping => {
            let zs = ["      ", "z     ", "zZ    ", "zZz   ", "zZzZ  ", "zZzZz "][frame / 5 % 6];
            let drift = bounce(frame / 4, 6);
            (vec![
                format!("{}Z", " ".repeat(drift)),
                "      |\\      _,,,---,,_".into(),
                format!("{zs}/,`.-'`'    -.  ;-;;,_"),
                "     |,4-  ) )-,_. ,\\ (  `'-'".into(),
                "    '---''(_/--'  `-'\\_)".into(),
            ], "napping: F-Droid takes days", Color::Blue)
        }
        Mood::Hunting => {
            let p = bounce(frame, 12);
            let eyes = if blink { "-.-" } else if p < 4 { "o.o" } else { "O.O" };
            let paw = if p < 2 { "  /     \\_" } else { "  /     \\ " };
            (vec![
                "   /\\_/\\".into(),
                format!("  ( {eyes} )"),
                "   > ^ <".into(),
                paw.into(),
                format!(" (_)| |(_){}@", "~".repeat(p + 1)),
            ], "sniffing the servers…", Color::Yellow)
        }
        Mood::Purring => {
            let h = |o: usize| format!("{}♥", " ".repeat(10 + bounce(frame / 3 + o, 4)));
            let tail = if frame / 4 % 2 == 0 { "~" } else { " ~" };
            (vec![
                format!("   /\\_/\\{}", h(0)),
                format!("  ( {} ){}", if blink { "-.-" } else { "^.^" }, h(3)),
                "   > ♥ <".into(),
                format!("  /     \\{tail}"),
                " (_)| |(_)".into(),
            ], "purr: newest release is live", Color::Magenta)
        }
        Mood::Hissing => {
            let shake = " ".repeat(frame % 2);
            (vec![
                format!("{shake}   /\\_/\\   HSSS!"),
                format!("{shake}  ( >.< )"),
                format!("{shake}   > ^ <"),
                format!("{shake}  /     \\"),
                format!("{shake} (_)| |(_)"),
            ], "HISS: a build failed", Color::Red)
        }
    }
}

/// "fdroid-watch" in block letters.
fn banner() -> [String; 3] {
    let glyph = |c: char| -> [&str; 3] {
        match c {
            'f' => ["█▀▀", "█▀ ", "▀  "],
            'd' => ["█▀▄", "█ █", "▀▀ "],
            'r' => ["█▀█", "█▀▄", "▀ ▀"],
            'o' => ["█▀█", "█ █", "▀▀▀"],
            'i' => ["█", "█", "▀"],
            '-' => ["  ", "▀▀", "  "],
            'w' => ["█ █ █", "█ █ █", "▀▀▀▀▀"],
            'a' => ["▄▀█", "█▀█", "▀ ▀"],
            't' => ["▀█▀", " █ ", " ▀ "],
            'c' => ["█▀▀", "█  ", "▀▀▀"],
            'h' => ["█ █", "█▀█", "▀ ▀"],
            _ => [" ", " ", " "],
        }
    };
    let mut out: [String; 3] = Default::default();
    for c in "fdroid-watch".chars() {
        for (row, part) in out.iter_mut().zip(glyph(c)) {
            row.push_str(part);
            row.push(' ');
        }
    }
    out
}

/// A pastel rainbow hue, in degrees.
fn rainbow(deg: usize) -> Color {
    let h = (deg % 360) as f32 / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 { 0 => (1.0, x, 0.0), 1 => (x, 1.0, 0.0), 2 => (0.0, 1.0, x), 3 => (0.0, x, 1.0), 4 => (x, 0.0, 1.0), _ => (1.0, 0.0, x) };
    let c = |v: f32| (140.0 + 115.0 * v) as u8;
    Color::Rgb(c(r), c(g), c(b))
}

/// A cat walking the tag → bot → build → live track.
fn journey(st: Stage, frame: usize, seed: usize) -> Line<'static> {
    const WIDTH: usize = 26;
    const STOPS: [usize; 4] = [0, 7, 14, 21];
    let (at, face, color) = match st {
        Stage::Skipped => return Line::from("┄".repeat(WIDTH)).dark_gray(),
        Stage::Tagged => (0, "=^.^=", Color::Yellow),
        Stage::Queued => (1, "=^.^=", Color::Yellow),
        Stage::Built => (2, "=^.^=", Color::Cyan),
        Stage::Failed => (2, "=x.x=", Color::Red),
        Stage::Live => (3, "=^.^=", Color::Green),
    };
    // Waiting cats pace a step back and forth; the live one sits.
    let pace = if matches!(st, Stage::Tagged | Stage::Queued | Stage::Built) { (frame / 6 + seed) % 2 } else { 0 };
    let pos = STOPS[at] + pace;
    let face = if (frame + seed * 7) % 40 < 2 { "=-.-=" } else { face };
    let rest: String = (pos + 5..WIDTH).map(|x| if STOPS.contains(&x) { '○' } else { '┄' }).collect();
    let mut spans = vec![
        Span::raw("━".repeat(pos)).green(),
        Span::raw(face).fg(color).bold(),
        Span::raw(rest).dark_gray(),
    ];
    if st == Stage::Live {
        spans.push(Span::raw(if frame / 4 % 2 == 0 { " ♥" } else { "  " }).magenta());
    }
    Line::from(spans)
}

// ── Screen ──────────────────────────────────────────────────────────────

struct View<'a> {
    d: &'a Data,
    loading: bool,
    frame: usize,
    next_in: Duration,
}

fn draw(f: &mut Frame, v: &View) {
    let d = v.d;
    let rows = rows(d);
    let [head, table, errs, foot] = Layout::vertical([
        Constraint::Length(11),
        Constraint::Min(5),
        Constraint::Length(if d.errors.is_empty() { 0 } else { d.errors.len() as u16 + 2 }),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let [cat_area, info_area] = Layout::horizontal([Constraint::Length(34), Constraint::Min(40)]).areas(head);

    let (art, caption, color) = cat(mood(&rows, v.loading), v.frame);
    let mut lines = vec![Line::raw("")];
    lines.extend(art.into_iter().map(|l| Line::from(l).fg(color)));
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(format!(" {caption} ")).title_style(Style::new().fg(color))),
        cat_area,
    );

    let mut info: Vec<Line> = banner().iter().map(|row| {
        Line::from(row.chars().enumerate().map(|(x, c)| Span::raw(c.to_string()).fg(rainbow(x * 7 + v.frame * 9))).collect::<Vec<_>>())
    }).collect();
    let cycle = |c: &Cycle| match (c.start, c.end) {
        (Some(s), Some(e)) => format!("{} → {} UTC", fmt_ms(s), fmt_ms(e)),
        (Some(s), None) => format!("started {} UTC ({}), running", fmt_ms(s), ago(s)),
        _ if v.loading => "…".into(),
        _ => "unknown".into(),
    };
    let label = |s: &str| Span::styled(format!("{s:<17}"), Style::new().fg(Color::DarkGray));
    let or_wait = |s: Option<String>| s.unwrap_or_else(|| if v.loading { "…".into() } else { "?".into() });
    info.extend([
        Line::raw(""),
        Line::from(vec![label("Package"), Span::raw(PKG).bold()]),
        Line::from(vec![label("Suggested build"), Span::raw(or_wait(d.suggested.map(|c| c.to_string())))]),
        Line::from(vec![label("fdroiddata"), Span::raw(format!("CurrentVersion {}", or_wait(d.current_version.clone())))]),
        Line::from(vec![label("Last build cycle"), Span::raw(cycle(&d.last))]),
        Line::from(vec![label("Build cycle now"), Span::raw(cycle(&d.running))]),
    ]);
    f.render_widget(Paragraph::new(info).block(Block::bordered().title(" Pocket Sense on F-Droid ".bold())), info_area);

    let today = (now_ms() / 1000 / 86400) as i64;
    let date = |s: Option<&String>| match s {
        Some(s) => Cell::from(format!("✓ {s}")).green(),
        None => Cell::from("·").dark_gray(),
    };
    let table_rows = rows.iter().enumerate().map(|(i, (n, codes, st))| {
        let gh = match d.github.get(n) {
            Some(Some(day)) => date(Some(day)),
            Some(None) => Cell::from("tag only").yellow(),
            None => date(None),
        };
        let codes_txt = match (codes.first(), codes.last()) {
            (Some(a), Some(b)) if a != b => format!("{a}–{b}"),
            (Some(a), _) => a.to_string(),
            _ => "·".into(),
        };
        let suggested = codes.iter().any(|c| Some(*c) == d.suggested);
        let (stage_txt, color) = match st {
            Stage::Skipped => ("skipped", Color::DarkGray),
            Stage::Tagged => ("waiting for the bot", Color::Yellow),
            Stage::Queued => ("waiting for a build", Color::Yellow),
            Stage::Built => ("built, publishing soon", Color::Cyan),
            Stage::Failed => ("build failed", Color::Red),
            Stage::Live if suggested => ("live, suggested", Color::Green),
            Stage::Live => ("live", Color::Green),
        };
        Row::new(vec![
            Cell::from(n.clone()).bold(),
            gh,
            match (d.fdroid_dates.get(n), estimate(d, n, today)) {
                (Some(day), _) => date(Some(day)),
                (None, Some(est)) if *st != Stage::Skipped && est <= today => Cell::from("~ any day").yellow().italic(),
                (None, Some(est)) if *st != Stage::Skipped => Cell::from(format!("~ {}", iso(est))).yellow().italic(),
                _ => date(None),
            },
            Cell::from(codes_txt).fg(if codes.is_empty() { Color::DarkGray } else { Color::Reset }),
            Cell::from(journey(*st, v.frame, i)),
            Cell::from(stage_txt).fg(color),
        ])
    });
    let widths = [Constraint::Length(7), Constraint::Length(12), Constraint::Length(12), Constraint::Length(9), Constraint::Length(28), Constraint::Min(15)];
    let empty = rows.is_empty() && v.loading;
    f.render_widget(
        Table::new(table_rows, widths)
            .header(Row::new(["Version", "GitHub", "F-Droid", "Codes", "tag    bot    build  live", "Stage"]).add_modifier(Modifier::BOLD).bottom_margin(1))
            .column_spacing(1)
            .block(Block::bordered().title(match typical_lag(d) {
                _ if empty => " Releases: fetching… ".to_string(),
                Some(lag) => format!(" Releases · ~ is an estimate: F-Droid usually takes {lag} days after GitHub "),
                None => " Releases ".to_string(),
            })),
        table,
    );

    if !d.errors.is_empty() {
        let lines: Vec<Line> = d.errors.iter().map(|e| Line::from(e.as_str()).red()).collect();
        f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Errors ").red()), errs);
    }

    let status = if v.loading {
        let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][v.frame % 10];
        Span::raw(format!("{spin} fetching…")).yellow()
    } else {
        let checked = d.fetched_at.map_or(String::new(), |t| format!("checked {} UTC", fmt_ms(t)));
        let s = v.next_in.as_secs();
        Span::raw(format!("{checked} · next in {}:{:02}", s / 60, s % 60)).dark_gray()
    };
    f.render_widget(
        Line::from(vec![Span::raw(" r").bold(), Span::raw(" refresh   "), Span::raw("q").bold(), Span::raw(" quit   "), status]),
        foot,
    );
}

fn run(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    let start = Instant::now();
    let mut data = Data::default();
    let mut inbox: Option<Receiver<Data>> = None;
    // Fetch straight away; the screen animates while it runs.
    let mut next = Instant::now();
    loop {
        if inbox.is_none() && Instant::now() >= next {
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || tx.send(fetch()));
            inbox = Some(rx);
        }
        if let Some(rx) = &inbox {
            let done = match rx.try_recv() {
                Ok(d) => {
                    data = d;
                    true
                }
                Err(TryRecvError::Empty) => false,
                Err(TryRecvError::Disconnected) => {
                    data.errors = vec!["the fetch thread crashed".into()];
                    true
                }
            };
            if done {
                inbox = None;
                next = Instant::now() + AUTO_REFRESH;
            }
        }
        let view = View {
            d: &data,
            loading: inbox.is_some(),
            frame: (start.elapsed().as_millis() / TICK.as_millis()) as usize,
            next_in: next.saturating_duration_since(Instant::now()),
        };
        terminal.draw(|f| draw(f, &view))?;
        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('r') if inbox.is_none() => next = Instant::now(),
                        _ => {}
                    }
                }
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_pairs_names_with_codes() {
        let yml = "Builds:\n  - versionName: 2.3.0\n    versionCode: 5051\n  - versionName: '2.3.0'\n    versionCode: 5052\nCurrentVersion: 2.3.0\nCurrentVersionCode: 5053\n";
        let (b, cur) = parse_metadata(yml);
        assert_eq!(b["2.3.0"], vec![5051, 5052]);
        assert_eq!(cur.as_deref(), Some("2.3.0"));
    }

    #[test]
    fn added_dates_from_the_package_page() {
        let html = "<b>Version 2.2.9</b> (5043)\n<span>suggested</span>\n  Added on Oct 25, 2026\n<b>Version 2.2.9</b> (5042)\n Added on Oct 24, 2026\n<b>Version 2.2.8</b> (5033)\nAdded on Sep 2, 2026\n";
        let d = parse_added(html);
        assert_eq!(d["2.2.9"], "2026-10-25", "the newest code's date wins");
        assert_eq!(d["2.2.8"], "2026-09-02");
    }

    #[test]
    fn stages() {
        let mut d = Data::default();
        assert_eq!(stage(&[], &d), Stage::Tagged);
        assert_eq!(stage(&[1, 2], &d), Stage::Queued);
        d.last.failed.insert(1);
        assert_eq!(stage(&[1, 2], &d), Stage::Failed);
        d.running.built.insert(1);
        assert_eq!(stage(&[1, 2], &d), Stage::Built, "a rebuild in the new cycle beats the old failure");
        d.published.extend([1, 2]);
        assert_eq!(stage(&[1, 2], &d), Stage::Live);
    }

    #[test]
    fn dates() {
        assert_eq!(fmt_ms(0), "1970-01-01 00:00");
        assert_eq!(fmt_ms(1_792_707_022_000), "2026-10-22 22:10");
        assert!(version_key("2.10.0") > version_key("2.9.1"));
        for day in ["1970-01-01", "2024-02-29", "2026-10-25", "2027-01-01"] {
            assert_eq!(iso(day_number(day).unwrap()), day);
        }
    }

    #[test]
    fn estimates_from_the_median_lag() {
        let mut d = Data::default();
        for (n, gh, fd) in [("1.0", "2026-10-01", "2026-10-04"), ("1.1", "2026-10-10", "2026-10-15"), ("1.2", "2026-10-20", "2026-10-26")] {
            d.github.insert(n.into(), Some(gh.into()));
            d.fdroid_dates.insert(n.into(), fd.into());
        }
        d.github.insert("1.3".into(), Some("2026-10-26".into()));
        d.github.insert("1.4".into(), None);
        let today = day_number("2026-10-27").unwrap();
        assert_eq!(typical_lag(&d), Some(5), "median of 3, 5 and 6");
        assert_eq!(estimate(&d, "1.3", today).map(iso).as_deref(), Some("2026-10-31"));
        assert_eq!(estimate(&d, "1.4", today).map(iso).as_deref(), Some("2026-11-01"), "a bare tag counts from today");
        d.github.insert("1.3".into(), Some("2026-10-01".into()));
        assert_eq!(estimate(&d, "1.3", today), Some(today), "late, but never in the past");
    }

    /// Draws one live fetch at a few frames: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore = "network"]
    fn live_render() {
        let d = fetch();
        for (frame, loading) in [(0, true), (7, false)] {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 25)).unwrap();
            t.draw(|f| draw(f, &View { d: &d, loading, frame, next_in: AUTO_REFRESH })).unwrap();
            let buf = t.backend().buffer();
            for y in 0..buf.area.height {
                println!("{}", (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>());
            }
        }
    }
}
