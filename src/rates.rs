//! Published rate series, kept as `date,series,value` so a refresh reads as a diff.
//!
//! ```text
//!   EURUSD   ECB reference rate, USD per EUR   daily     a fixing never changes
//!   CPIU     BLS CPI-U, CUUR0000SA0            monthly   a revised print overwrites
//!   HICPFR   Eurostat HICP France, 2025=100    monthly   a revised print overwrites
//! ```

use std::{collections::BTreeMap, path::Path, time::Duration};

use eyre::{OptionExt, Result, ensure};
use jiff::{Zoned, civil::Date};
use reqwest::blocking::Client;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, strum::Display, strum::EnumString)]
pub(crate) enum Series {
	#[strum(serialize = "EURUSD")]
	EurUsd,
	#[strum(serialize = "CPIU")]
	CpiU,
	#[strum(serialize = "HICPFR")]
	HicpFr,
}
impl Series {
	fn monthly(self) -> bool {
		self != Series::EurUsd
	}
}

const ECB: &str = "https://data-api.ecb.europa.eu/service/data";
const BLS: &str = "https://api.bls.gov/publicAPI/v1/timeseries/data/";
const CPI_SERIES: &str = "CUUR0000SA0"; // CPI-U, US city average, all items, not seasonally adjusted

/// A month is keyed on its first day.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rates(pub(crate) BTreeMap<Series, BTreeMap<Date, f64>>);

impl Rates {
	pub fn read(path: &Path) -> Result<Self> {
		let mut rates = Self::default();
		for row in csv::Reader::from_path(path)?.records() {
			let row = row?;
			let series: Series = row[1].parse()?;
			let day = if series.monthly() { format!("{}-01", &row[0]) } else { row[0].to_owned() };
			rates.0.entry(series).or_default().insert(day.parse()?, row[2].parse()?);
		}
		Ok(rates)
	}

	pub fn write(&self, path: &Path) -> Result<()> {
		let mut out = csv::Writer::from_path(path)?;
		out.write_record(["date", "series", "value"])?;
		for (series, points) in &self.0 {
			for (day, value) in points {
				let date = if series.monthly() { day.strftime("%Y-%m").to_string() } else { day.to_string() };
				out.write_record([date, series.to_string(), value.to_string()])?;
			}
		}
		out.flush()?;
		Ok(())
	}

	/// `other` wins where both hold a point: it is the later read of a source that revises.
	pub fn merge(&mut self, other: Rates) {
		for (series, points) in other.0 {
			self.0.entry(series).or_default().extend(points);
		}
	}

	pub fn len(&self) -> usize {
		self.0.values().map(BTreeMap::len).sum()
	}

	/// The point in force on `day`: the latest on or before it.
	pub(crate) fn at(&self, series: Series, day: Date) -> Result<f64> {
		let (_, v) = self
			.0
			.get(&series)
			.and_then(|p| p.range(..=day).next_back())
			.ok_or_eyre(format!("no {series} on or before {day}; refetch rates from an earlier date"))?;
		Ok(*v)
	}
}

/// Every series from `since` to its latest point.
pub fn fetch(since: Date) -> Result<Rates> {
	let http = Client::builder().timeout(Duration::from_secs(60)).build()?;
	let mut rates = Rates::default();
	rates.0.insert(Series::EurUsd, ecb(&http, "EXR/D.USD.EUR.SP00.A", &since.to_string())?);
	rates.0.insert(Series::HicpFr, ecb(&http, "HICP/M.FR.N.000000.4D0.INX", &since.strftime("%Y-%m").to_string())?);
	rates.0.insert(Series::CpiU, cpi(&http, since)?);
	Ok(rates)
}

fn ecb(http: &Client, key: &str, since: &str) -> Result<BTreeMap<Date, f64>> {
	let body = http.get(format!("{ECB}/{key}?format=csvdata&detail=dataonly&startPeriod={since}")).send()?.error_for_status()?.text()?;
	let mut reader = csv::Reader::from_reader(body.as_bytes());
	let headers = reader.headers()?.clone();
	let column = |name: &str| headers.iter().position(|h| h == name).ok_or_eyre(format!("ECB {key}: no {name} column"));
	let (period, value) = (column("TIME_PERIOD")?, column("OBS_VALUE")?);
	let mut out = BTreeMap::new();
	for row in reader.records() {
		let row = row?;
		let day = if row[period].len() == 7 { format!("{}-01", &row[period]) } else { row[period].to_owned() };
		out.insert(day.parse()?, row[value].parse()?);
	}
	ensure!(!out.is_empty(), "ECB {key}: no observations since {since}");
	Ok(out)
}

fn cpi(http: &Client, since: Date) -> Result<BTreeMap<Date, f64>> {
	let query = serde_json::json!({
		"seriesid": [CPI_SERIES],
		"startyear": since.year().to_string(),
		"endyear": Zoned::now().year().to_string(),
	});
	let body: serde_json::Value = http.post(BLS).json(&query).send()?.error_for_status()?.json()?;
	ensure!(body["status"] == "REQUEST_SUCCEEDED", "BLS: {} {}", body["status"], body["message"]);
	let points = body["Results"]["series"][0]["data"].as_array().ok_or_eyre("BLS returned no series")?;

	let mut out = BTreeMap::new();
	for point in points {
		let field = |name: &str| -> Result<&str> { point[name].as_str().ok_or_eyre(format!("BLS point with no {name}: {point}")) };
		let (year, period, value) = (field("year")?, field("period")?, field("value")?);
		if value == "-" {
			continue; // a genuine hole in the series; the previous month stays in force
		}
		let month = period.strip_prefix('M').ok_or_eyre(format!("BLS period is not monthly: {period}"))?;
		let day: Date = format!("{year}-{month}-01").parse()?;
		if day >= since.first_of_month() {
			out.insert(day, value.parse()?);
		}
	}
	ensure!(!out.is_empty(), "BLS {CPI_SERIES}: no prints since {since}");
	Ok(out)
}
