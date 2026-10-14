//!     grafana-utils rates <csv> <since: YYYY-MM-DD>
//!
//! Fetches every series from `since` and merges it into `<csv>`.

use std::path::PathBuf;

use eyre::{Result, bail};
use grafana_utils::rates::{self, Rates};

fn main() -> Result<()> {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let [command, csv, since] = &args[..] else { bail!("usage: grafana-utils rates <csv> <since: YYYY-MM-DD>") };
	if command != "rates" {
		bail!("unknown command: {command}");
	}
	let csv = PathBuf::from(csv);
	let mut known = if csv.exists() { Rates::read(&csv)? } else { Rates::default() };
	let before = known.len();
	known.merge(rates::fetch(since.parse()?)?);
	known.write(&csv)?;
	eprintln!("{}: {} points, {} new", csv.display(), known.len(), known.len() - before);
	Ok(())
}
