use std::{path::Path, time::Instant};

use reqwest::Url;
use ruscello_resolver::{PageAnalysis, analyze_html};

#[test]
fn ranks_expected_targets_in_a_saved_corpus() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let base = Url::parse("https://fixtures.example/index")?;
    let started = Instant::now();
    let mut total = 0;
    let mut top_three = 0;
    let mut misses = Vec::new();
    for entry in std::fs::read_dir(fixtures)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("rank-") {
            continue;
        }
        let page = std::fs::read_to_string(entry.path())?;
        let marker = page
            .split_once("name=\"expected-target\" content=\"")
            .and_then(|(_, next)| next.split_once('"'))
            .map(|(url, _)| url.replace("&amp;", "&"))
            .ok_or("missing expected target")?;
        let expected = base.join(&marker)?;
        let PageAnalysis::Page { candidates, .. } = analyze_html(&base, &page, &[]) else {
            return Err("expected page analysis".into());
        };
        total += 1;
        if candidates
            .iter()
            .take(3)
            .any(|candidate| candidate.url == expected)
        {
            top_three += 1;
        } else {
            misses.push(entry.file_name());
        }
    }
    eprintln!(
        "{top_three}/{total} in top three in {:?}; misses: {misses:?}",
        started.elapsed()
    );
    assert!(total >= 30);
    assert!(top_three * 100 >= total * 90);
    Ok(())
}
