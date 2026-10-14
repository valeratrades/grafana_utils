//! tests/rates.csv: CPI-U averages 300 over 2025 (290 … 310) and last prints 330 for
//! 2026-01; HICP is 100 through 2025, then 102, then 105 for 2026-02; EUR/USD is 1.05
//! from 2025-01-02, 1.10 from 2026-01-05, 1.20 from 2026-02-10.

use std::path::Path;

use grafana_utils::{
	money::{Basis, Ccy, Money},
	rates::Rates,
};
use rusqlite::Connection;

/// `expected` is `[2025, now, eur]`, or `None` where valuing the row must fail.
#[track_caller]
fn check(day: &str, ccy: &str, amount: f64, expected: Option<[f64; 3]>) {
	let rates = Rates::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rates.csv").as_path()).unwrap();
	let money = Money::new(rates, Ccy::EUR, &[Basis::Usd2025, Basis::UsdNow, Basis::EurNow]).unwrap();
	let conn = Connection::open_in_memory().unwrap();
	conn.execute_batch(&format!("CREATE TABLE t (day TEXT, ccy TEXT, amount REAL); INSERT INTO t VALUES ('{day}', '{ccy}', {amount});")).unwrap();
	let valued = money.materialize(&conn, "t", "day", "ccy", &["amount"]);
	let Some(expected) = expected else {
		assert!(valued.is_err(), "{day} {ccy} valued, but precedes the series");
		return;
	};
	valued.unwrap();
	let actual: [f64; 3] = conn.query_row("SELECT amount_2025, amount_now, amount_eur FROM t", [], |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?])).unwrap();
	for (a, e) in actual.iter().zip(expected) {
		assert!((a - e).abs() < 1e-9, "{day} {ccy} {amount}: {actual:?}, expected {expected:?}");
	}
}

#[test]
fn eur_row() {
	check("2026-02-15", "EUR", 100.0, Some([120.0 * 300.0 / 330.0, 120.0, 100.0]));
}

#[test]
fn usd_row() {
	check("2026-01-05", "USD", 50.0, Some([50.0 * 300.0 / 330.0, 50.0, 50.0 / 1.1 * 105.0 / 102.0]));
	check("2025-06-30", "USD", 50.0, Some([50.0, 50.0 * 330.0 / 300.0, 50.0 / 1.05 * 105.0 / 100.0]));
}

/// 2026-03 has no print in either index yet, so 2026-02 HICP and 2026-01 CPI-U stand.
#[test]
fn month_with_no_print() {
	check("2026-03-20", "EUR", 100.0, Some([120.0 * 300.0 / 330.0, 120.0, 100.0]));
}

#[test]
fn before_the_first_rate() {
	check("2025-01-01", "EUR", 100.0, None);
}
