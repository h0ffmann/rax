//! Output helpers that print what AX's Go CLI prints: aligned tables like
//! `text/tabwriter`, values like `fmt`'s `%v`, and ages like `5m`.

use std::time::Duration;

use serde_json::Value;

/// Lays out rows whose cells were separated by tabs, as Go's tabwriter does
/// with a minimum width of 0 and padding of `padding` spaces: each cell but
/// the last in its row is padded to its column's widest cell plus `padding`.
pub fn table(rows: &[Vec<String>], padding: usize) -> String {
    let mut widths: Vec<usize> = Vec::new();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(row.len().saturating_sub(1)) {
            let w = cell.chars().count();
            if widths.len() <= i {
                widths.push(w);
            } else {
                widths[i] = widths[i].max(w);
            }
        }
    }
    let mut out = String::new();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            out.push_str(cell);
            if i + 1 < row.len() {
                let pad = widths[i] + padding - cell.chars().count();
                out.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.push('\n');
    }
    out
}

/// Formats a duration the way `ax get` shows ages: seconds, minutes, hours
/// or days, truncated.
pub fn format_age(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        _ if s < 60 => format!("{s}s"),
        _ if s < 3600 => format!("{}m", s / 60),
        _ if s < 86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// Formats a JSON value (as decoded from a protobuf Struct) the way Go's
/// `%v` formats the `any` it decodes to.
pub fn go_value(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => go_float(n.as_f64().unwrap_or_default()),
        Value::String(s) => s.clone(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(go_value).collect::<Vec<_>>().join(" ")
        ),
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let parts: Vec<_> = keys
                .iter()
                .map(|k| format!("{k}:{}", go_value(&map[*k])))
                .collect();
            format!("map[{}]", parts.join(" "))
        }
    }
}

/// A float64 as `%v` prints it: the shortest representation, in exponent
/// form below 1e-4 and from 1e21 on.
pub fn go_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf" } else { "-Inf" }.into();
    }
    let abs = f.abs();
    if abs != 0.0 && !(1e-4..1e21).contains(&abs) {
        let s = format!("{f:e}");
        let (mantissa, exp) = s.split_once('e').unwrap();
        let (sign, digits) = match exp.strip_prefix('-') {
            Some(d) => ('-', d),
            None => ('+', exp),
        };
        return format!("{mantissa}e{sign}{digits:0>2}");
    }
    format!("{f}")
}

/// Formats a list of strings the way `%v` formats a `[]string`.
pub fn go_strings(items: &[String]) -> String {
    format!("[{}]", items.join(" "))
}

/// Sorts object keys recursively, as Go's YAML encoder does for maps, and
/// writes whole numbers without a fraction, as it does for float64s.
pub fn sorted(v: &Value) -> Value {
    match v {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => Value::from(f as i64),
            _ => v.clone(),
        },
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| {
                        let v = sorted(&map[&k]);
                        (k, v)
                    })
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tables_align_like_tabwriter() {
        let rows = vec![
            vec!["NAME".into(), "ATESPACE".into(), "AGE".into()],
            vec!["a-long-name".into(), "default".into(), "5s".into()],
        ];
        assert_eq!(
            table(&rows, 3),
            "NAME          ATESPACE   AGE\na-long-name   default    5s\n"
        );
    }

    #[test]
    fn ages() {
        for (secs, want) in [
            (5, "5s"),
            (59, "59s"),
            (60, "1m"),
            (7199, "1h"),
            (86400 * 3, "3d"),
        ] {
            assert_eq!(format_age(Duration::from_secs(secs)), want);
        }
    }

    #[test]
    fn values_like_go() {
        assert_eq!(go_value(&json!(0.2)), "0.2");
        assert_eq!(go_value(&json!(4096.0)), "4096");
        assert_eq!(go_value(&json!(1e21)), "1e+21");
        assert_eq!(go_value(&json!(0.00001)), "1e-05");
        assert_eq!(go_value(&json!(["END", 1.5])), "[END 1.5]");
        assert_eq!(
            go_value(&json!({"b": true, "a": null})),
            "map[a:<nil> b:true]"
        );
        assert_eq!(go_strings(&["sh".into(), "-c".into()]), "[sh -c]");
    }
}
