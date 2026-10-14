//! Dashboard source -> what Grafana is served.
//!
//! Grafana has no include mechanism, and nothing in it can derive a value from the
//! data at load time, so a source carries markers and each [`Component`] owns some.

use serde_json::{Map, Value};

pub trait Component {
	fn string(&self, s: String) -> String {
		s
	}
	/// An object replaced whole; the replacement is expanded again.
	fn node(&self, _node: &Map<String, Value>) -> Option<Value> {
		None
	}
	/// An array item replaced by a run of siblings; each is expanded again.
	fn splice(&self, _node: &Map<String, Value>) -> Option<Vec<Value>> {
		None
	}
	/// Every item of a `panels` array, once expanded.
	fn panel(&self, _panel: &mut Value) {}
}

pub fn expand(node: Value, components: &[&dyn Component]) -> Value {
	match node {
		Value::String(s) => Value::String(components.iter().fold(s, |s, c| c.string(s))),
		Value::Array(items) => Value::Array(
			items
				.into_iter()
				.flat_map(|item| match item.as_object().and_then(|o| components.iter().find_map(|c| c.splice(o))) {
					Some(run) => run.into_iter().map(|i| expand(i, components)).collect(),
					None => vec![expand(item, components)],
				})
				.collect(),
		),
		Value::Object(node) => {
			if let Some(replacement) = components.iter().find_map(|c| c.node(&node)) {
				return expand(replacement, components);
			}
			Value::Object(
				node.into_iter()
					.map(|(k, v)| {
						let mut v = expand(v, components);
						if k == "panels" {
							for panel in v.as_array_mut().expect("`panels` is an array") {
								for c in components {
									c.panel(panel);
								}
							}
						}
						(k, v)
					})
					.collect(),
			)
		}
		other => other,
	}
}
