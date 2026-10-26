//! Where each Pocket Sense release is on its way to F-Droid:
//! tagged on GitHub → picked up in fdroiddata → built → published.

use std::collections::{BTreeMap, HashSet};
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

fn parse_cycle(v: &Value) -> Cycle {
    let ours = |key: &str| -> HashSet<u64> {
        v[key].as_array().into_iter().flatten()
            .filter(|b| b[0].as_str() == Some(PKG))
            .filter_map(|b| b[1].as_u64())
            .collect()
    };
    Cycle { start: v["startTimestamp"].as_u64(), end: v["endTimestamp"].as_u64(), built: ours("successfulBuildIds"), failed: ours("failedBuilds") }
}

#[derive(Debug, PartialEq)]
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

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// "2026-10-25 18:10" UTC, from epoch milliseconds.
fn fmt_ms(ms: u64) -> String {
    let (days, secs) = ((ms / 1000) / 86400, (ms / 1000) % 86400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

fn ago(ms: u64) -> String {
    let h = now_ms().saturating_sub(ms) / 3_600_000;
    if h < 48 { format!("{h}h ago") } else { format!("{}d ago", h / 24) }
}

fn draw(f: &mut Frame, d: &Data, loading: bool) {
    let [head, table, errs, foot] = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(5),
        Constraint::Length(if d.errors.is_empty() { 0 } else { d.errors.len() as u16 + 2 }),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let cycle = |c: &Cycle| match (c.start, c.end) {
        (Some(s), Some(e)) => format!("{} → {} UTC", fmt_ms(s), fmt_ms(e)),
        (Some(s), None) => format!("started {} UTC ({}), still running", fmt_ms(s), ago(s)),
        _ => "unknown".into(),
    };
    let suggested = d.suggested.map_or("not on f-droid.org".into(), |c| c.to_string());
    let label = |s: &str| Span::styled(format!("{s:<18}"), Style::new().fg(Color::DarkGray));
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![label("Package"), Span::raw(PKG).bold()]),
            Line::from(vec![label("Suggested build"), Span::raw(suggested)]),
            Line::from(vec![label("fdroiddata"), Span::raw(format!("CurrentVersion {}", d.current_version.as_deref().unwrap_or("?")))]),
            Line::from(vec![label("Last build cycle"), Span::raw(cycle(&d.last))]),
            Line::from(vec![label("Build cycle now"), Span::raw(cycle(&d.running))]),
        ])
        .block(Block::bordered().title(" Pocket Sense on F-Droid ".bold())),
        head,
    );

    // Tags older than the oldest version F-Droid knows came before the app
    // was on F-Droid; untracked ones in between were skipped (the bot only
    // takes the newest tag it sees).
    let known: Vec<Vec<u64>> = d.codes.keys().map(|n| version_key(n)).collect();
    let (oldest, newest) = (known.iter().min(), known.iter().max());
    let mut names: Vec<&String> = d.github.keys().chain(d.codes.keys()).collect::<HashSet<_>>().into_iter()
        .filter(|n| oldest.is_none_or(|o| version_key(n) >= *o))
        .collect();
    names.sort_by_key(|n| std::cmp::Reverse(version_key(n)));
    let tick = |ok: bool, text: String| if ok { Cell::from(format!("✓ {text}")).green() } else { Cell::from("·").dark_gray() };
    let rows = names.into_iter().map(|n| {
        let codes = d.codes.get(n).cloned().unwrap_or_default();
        let st = match stage(&codes, d) {
            Stage::Tagged if newest.is_some_and(|m| version_key(n) < *m) => Stage::Skipped,
            st => st,
        };
        let gh = match d.github.get(n) {
            Some(Some(date)) => tick(true, date.clone()),
            Some(None) => Cell::from("tag only").yellow(),
            None => tick(false, String::new()),
        };
        let codes_txt = match (codes.first(), codes.last()) {
            (Some(a), Some(b)) if a != b => format!("{a}–{b}"),
            (Some(a), _) => a.to_string(),
            _ => String::new(),
        };
        let built = match st {
            Stage::Failed => Cell::from("✗ failed").red().bold(),
            Stage::Built | Stage::Live => tick(true, String::new()),
            _ => tick(false, String::new()),
        };
        let suggested = codes.iter().any(|c| Some(*c) == d.suggested);
        let (stage_txt, color) = match st {
            Stage::Skipped => ("skipped by F-Droid", Color::DarkGray),
            Stage::Tagged => ("waiting for the update bot", Color::Yellow),
            Stage::Queued => ("waiting for a build cycle", Color::Yellow),
            Stage::Built => ("built, waiting to publish", Color::Cyan),
            Stage::Failed => ("build failed", Color::Red),
            Stage::Live if suggested => ("live, suggested", Color::Green),
            Stage::Live => ("live", Color::Green),
        };
        Row::new(vec![
            Cell::from(n.clone()).bold(),
            gh,
            tick(!codes.is_empty(), codes_txt),
            built,
            tick(st == Stage::Live, String::new()),
            Cell::from(stage_txt).fg(color),
        ])
    });
    let widths = [Constraint::Length(9), Constraint::Length(13), Constraint::Length(13), Constraint::Length(9), Constraint::Length(5), Constraint::Min(20)];
    f.render_widget(
        Table::new(rows, widths)
            .header(Row::new(["Version", "GitHub", "Picked up", "Built", "Live", "Stage"]).add_modifier(Modifier::BOLD).bottom_margin(1))
            .column_spacing(2)
            .block(Block::bordered().title(" Releases ")),
        table,
    );

    if !d.errors.is_empty() {
        let lines: Vec<Line> = d.errors.iter().map(|e| Line::from(e.as_str()).red()).collect();
        f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Errors ").red()), errs);
    }

    let status = if loading {
        "refreshing…".to_string()
    } else {
        d.fetched_at.map_or(String::new(), |t| format!("checked {} UTC, again every 15 min", fmt_ms(t)))
    };
    f.render_widget(
        Line::from(vec![Span::raw(" r").bold(), Span::raw(" refresh   "), Span::raw("q").bold(), Span::raw(" quit   "), Span::raw(status).dark_gray()]),
        foot,
    );
}

fn run(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    let mut data = Data::default();
    let mut refresh = true;
    let mut last = Instant::now();
    loop {
        if refresh || last.elapsed() >= AUTO_REFRESH {
            // Show "refreshing…" first; the fetch blocks for a few seconds.
            terminal.draw(|f| draw(f, &data, true))?;
            data = fetch();
            (refresh, last) = (false, Instant::now());
        }
        terminal.draw(|f| draw(f, &data, false))?;
        if event::poll(Duration::from_secs(30))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('r') => refresh = true,
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

    /// Draws one live fetch: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore = "network"]
    fn live_render() {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 22)).unwrap();
        let d = fetch();
        t.draw(|f| draw(f, &d, false)).unwrap();
        let buf = t.backend().buffer();
        for y in 0..buf.area.height {
            println!("{}", (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>());
        }
    }

    #[test]
    fn dates() {
        assert_eq!(fmt_ms(0), "1970-01-01 00:00");
        assert_eq!(fmt_ms(1_792_707_022_000), "2026-10-22 22:10");
        assert!(version_key("2.10.0") > version_key("2.9.1"));
    }
}
