//! Money read in whichever basis a dashboard's dropdown holds.
//!
//! ```text
//!   value(amount, day, basis) = amount × fx(native → basis.ccy, day) × I(anchor) / I(month of day)
//! ```
//!
//! `I` is the basis currency's own index, so each basis is a valuation of its own
//! rather than a rescaling of another, and a basis is a column per money column:
//! `{col}_{key}`, filled by [`Money::materialize`]. A source writes `$money(col)`.

use std::sync::LazyLock;

use eyre::{OptionExt, Result, ensure};
use jiff::{
	Zoned,
	civil::{Date, date},
};
use regex::Regex;
use rusqlite::{Connection, functions::FunctionFlags};
use serde_json::{Map, Value, json};

use crate::{
	expand::Component,
	rates::{Rates, Series},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::Display, strum::EnumString)]
pub enum Ccy {
	USD,
	EUR,
}
impl Ccy {
	fn index(self) -> Series {
		match self {
			Ccy::USD => Series::CpiU,
			Ccy::EUR => Series::HicpFr,
		}
	}

	fn sym(self) -> &'static str {
		match self {
			Ccy::USD => "$",
			Ccy::EUR => "€",
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::IntoStaticStr)]
pub enum Basis {
	#[strum(serialize = "2025")]
	Usd2025,
	#[strum(serialize = "now")]
	UsdNow,
	#[strum(serialize = "eur")]
	EurNow,
}
impl Basis {
	fn key(self) -> &'static str {
		self.into()
	}

	fn label(self) -> &'static str {
		match self {
			Basis::Usd2025 => "2025 dollars",
			Basis::UsdNow => "today's dollars",
			Basis::EurNow => "today's euros",
		}
	}

	fn ccy(self) -> Ccy {
		match self {
			Basis::Usd2025 | Basis::UsdNow => Ccy::USD,
			Basis::EurNow => Ccy::EUR,
		}
	}
}

/// The `$bases` variable's name, which every expanded column names.
const VAR: &str = "${basis}";
static MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$money\(([\w.]+)\)").unwrap());

#[derive(Clone, Debug)]
pub struct Money {
	rates: Rates,
	native: Ccy,
	/// The first is the dropdown's default.
	bases: Vec<Basis>,
}

impl Money {
	pub fn new(rates: Rates, native: Ccy, bases: &[Basis]) -> Result<Self> {
		ensure!(!bases.is_empty(), "a dropdown with no bases");
		let money = Self { rates, native, bases: bases.to_vec() };
		for &b in bases {
			money.anchor(b)?;
		}
		Ok(money)
	}

	/// One `from` unit moved on `day`, read in `basis`.
	pub fn factor(&self, day: Date, from: Ccy, basis: Basis) -> Result<f64> {
		let to = basis.ccy();
		let fx = match (from, to) {
			_ if from == to => 1.0,
			(Ccy::EUR, Ccy::USD) => self.rates.at(Series::EurUsd, day)?,
			(Ccy::USD, Ccy::EUR) => 1.0 / self.rates.at(Series::EurUsd, day)?,
			_ => unreachable!("two currencies"),
		};
		Ok(fx * self.anchor(basis)? / self.rates.at(to.index(), day.first_of_month())?)
	}

	fn anchor(&self, basis: Basis) -> Result<f64> {
		let index = basis.ccy().index();
		Ok(match basis {
			// The mean of what was printed: a month the source never published (CPI-U,
			// 2025-10) is out of its own annual average too.
			Basis::Usd2025 => {
				let points = self.rates.0.get(&index).ok_or_eyre(format!("no {index} prints"))?;
				let (first, last) = (*points.first_key_value().expect("a series is never empty").0, *points.last_key_value().expect("a series is never empty").0);
				ensure!(first <= date(2025, 1, 1) && last >= date(2025, 12, 1), "{index}: {first}..{last} does not cover 2025; refetch rates from 2025 or earlier");
				let year: Vec<f64> = points.iter().filter(|(d, _)| d.year() == 2025).map(|(_, v)| *v).collect();
				year.iter().sum::<f64>() / year.len() as f64
			}
			Basis::UsdNow | Basis::EurNow => self.rates.at(index, Date::MAX)?,
		})
	}

	/// Adds `{col}_{key}` for every basis, each row valued at the day and in the
	/// currency the two SQL expressions give it: `day` is `YYYY-MM-DD`, `ccy` a [`Ccy`].
	pub fn materialize(&self, conn: &Connection, table: &str, day: &str, ccy: &str, cols: &[&str]) -> Result<()> {
		let money = self.clone();
		conn.create_scalar_function("money", 4, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, move |ctx| {
			let Some(amount) = ctx.get::<Option<f64>>(0)? else { return Ok(None) };
			let (day, ccy, key) = (ctx.get::<String>(1)?, ctx.get::<String>(2)?, ctx.get::<String>(3)?);
			let value = || -> Result<f64> {
				let basis = *money.bases.iter().find(|b| b.key() == key).expect("keys are written below from `bases`");
				Ok(amount * money.factor(day.parse()?, ccy.parse()?, basis)?)
			};
			value().map(Some).map_err(|e| rusqlite::Error::UserFunctionError(e.into()))
		})?;
		let mut set = Vec::new();
		for basis in &self.bases {
			let key = basis.key();
			for col in cols {
				conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col}_{key} REAL"), [])?;
				set.push(format!("{col}_{key} = money({col}, {day}, {ccy}, '{key}')"));
			}
		}
		conn.execute(&format!("UPDATE {table} SET {}", set.join(", ")), [])?;
		Ok(())
	}

	fn option(&self, basis: Basis) -> String {
		let today = Zoned::now().date();
		let factor = self.factor(today, self.native, basis).expect("every basis is anchored in `new`, and today is after any first point");
		format!("{} · {}{factor:.4}", basis.label(), basis.ccy().sym())
	}
}

impl Component for Money {
	fn string(&self, s: String) -> String {
		let s = MARKER.replace_all(&s, |c: &regex::Captures| format!("{}_{VAR}", &c[1])).into_owned();
		assert!(!s.contains("$money("), "`$money(…)` takes one column, not an expression: {s}");
		s
	}

	/// `{"$bases": …}` -> the dropdown, each option priced in what one native unit
	/// spent today is worth in it.
	fn node(&self, node: &Map<String, Value>) -> Option<Value> {
		node.get("$bases")?;
		let derived = ["name", "type", "query", "current", "options", "includeAll", "multi"];
		let kept: Vec<&str> = derived.into_iter().filter(|k| node.contains_key(*k)).collect();
		assert!(kept.is_empty(), "{kept:?} of the `$bases` variable are derived, not kept in the source");
		let mut node = node.clone();
		node.shift_remove("$bases");
		let choices = self.bases.iter().map(|&b| format!("{} : {}", self.option(b), b.key())).collect::<Vec<_>>().join(",");
		node.extend([
			("name".into(), "basis".into()),
			("type".into(), "custom".into()),
			("query".into(), choices.into()),
			("current".into(), json!({"text": self.option(self.bases[0]), "value": self.bases[0].key()})),
			("options".into(), json!([])),
			("includeAll".into(), false.into()),
			("multi".into(), false.into()),
		]);
		Some(Value::Object(node))
	}

	/// A panel reading a basis column gets the basis's currency as its unit.
	///
	/// Grafana will not put a variable in a field's unit, so the unit arrives as data:
	/// one more query, read onto every numeric field the panel's own overrides have
	/// not already claimed for a count or a percent.
	fn panel(&self, panel: &mut Value) {
		let Some(targets) = panel["targets"].as_array() else { return };
		if !targets.iter().any(|t| t["queryText"].as_str().is_some_and(|q| q.contains(&format!("_{VAR}")))) {
			return;
		}
		assert!(panel["fieldConfig"]["defaults"].get("unit").is_none(), "{}: money takes its unit from the basis, not from the panel", panel["title"]);
		let arms: String = self.bases.iter().map(|b| format!(" WHEN '{}' THEN 'currency{}'", b.key(), b.ccy())).collect();
		let unit = format!("SELECT CASE '{VAR}'{arms} END AS unit");
		panel["targets"]
			.as_array_mut()
			.unwrap()
			.push(json!({"refId": "Z", "queryType": "table", "queryText": unit, "rawQueryText": unit, "timeColumns": []}));
		let read = json!({
			"id": "configFromData",
			"options": {"configRefId": "Z", "applyTo": {"id": "byType", "options": "number"}, "mappings": [{"fieldName": "unit", "handlerKey": "unit"}]},
		});
		let object = panel.as_object_mut().expect("a panel is an object");
		// first: the frame it reads is gone once read, before any other transformation sees it
		match object.get_mut("transformations").and_then(Value::as_array_mut) {
			Some(ts) => ts.insert(0, read),
			None => {
				object.insert("transformations".into(), json!([read]));
			}
		}
	}
}
