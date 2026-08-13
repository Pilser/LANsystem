use crate::ast::ExprError;
use chrono::{Datelike, NaiveDateTime, Timelike, Utc, Duration as ChronoDuration};
use serde_json::Value;

pub fn to_num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn to_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub fn truthy(v: &Value) -> bool {
    to_bool(v)
}

fn to_int(v: &Value) -> Option<i64> {
    v.as_f64().map(|f| f as i64)
}

fn parse_ts(v: &Value) -> Option<NaiveDateTime> {
    let s = match v {
        Value::Number(n) => return Some(chrono::DateTime::from_timestamp(n.as_f64().unwrap_or(0.0) as i64, 0)?.naive_utc()),
        Value::String(s) => s.as_str(),
        _ => return None,
    };
    let s = s.trim();
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dZ"))
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d"))
        .ok()
}

pub fn invoke(name: &str, args: &[Value]) -> Result<Value, ExprError> {
    let n = name.to_lowercase();
    match n.as_str() {
        "lower" => one_str(name, args, |s| s.to_lowercase()),
        "upper" => one_str(name, args, |s| s.to_uppercase()),
        "trim" => one_str(name, args, |s| s.trim().to_string()),
        "len" => {
            let a = one(name, args)?;
            Ok(Value::from(match a {
                Value::String(s) => s.chars().count() as i64,
                Value::Array(x) => x.len() as i64,
                Value::Object(x) => x.len() as i64,
                _ => 0,
            }))
        }
        "num" => {
            let a = one(name, args)?;
            match &a {
                Value::Number(_) => Ok(a.clone()),
                Value::String(s) => Ok(s.trim().parse::<f64>().map(Value::from).unwrap_or(Value::Null)),
                Value::Bool(b) => Ok(Value::from(if *b { 1 } else { 0 })),
                _ => Ok(Value::Null),
            }
        }
        "str" => {
            let a = one(name, args)?;
            Ok(match a {
                Value::String(s) => Value::String(s),
                Value::Null => Value::Null,
                other => Value::String(other.to_string()),
            })
        }
        "concat" => {
            let mut out = String::new();
            for a in args {
                if let Value::String(s) = a {
                    out.push_str(s);
                } else if !a.is_null() {
                    out.push_str(&a.to_string());
                }
            }
            Ok(Value::String(out))
        }
        "round" => num_fmt(name, args, |f, d| {
            let m = 10f64.powi(d as i32);
            (f * m).round() / m
        }),
        "floor" => num1(name, args, f64::floor),
        "ceil" => num1(name, args, f64::ceil),
        "abs" => num1(name, args, f64::abs),
        "min" => num2(name, args, f64::min),
        "max" => num2(name, args, f64::max),
        "if" => {
            if args.len() != 3 {
                return Err(arg_err(name, 3, args.len()));
            }
            if truthy(&args[0]) { Ok(args[1].clone()) } else { Ok(args[2].clone()) }
        }
        "coalesce" => {
            for a in args {
                if !a.is_null() {
                    return Ok(a.clone());
                }
            }
            Ok(Value::Null)
        }
        "contains" => {
            if args.len() != 2 {
                return Err(arg_err(name, 2, args.len()));
            }
            match &args[0] {
                Value::String(s) => {
                    if let Value::String(sub) = &args[1] {
                        Ok(Value::from(s.contains(sub.as_str())))
                    } else {
                        Ok(Value::Bool(false))
                    }
                }
                Value::Array(arr) => Ok(Value::Bool(arr.contains(&args[1]))),
                _ => Ok(Value::Bool(false)),
            }
        }
        "startswith" => {
            two_str(name, args, |s, p| s.starts_with(p))
        }
        "endswith" => {
            two_str(name, args, |s, p| s.ends_with(p))
        }
        "join" => {
            if args.len() != 2 {
                return Err(arg_err(name, 2, args.len()));
            }
            let arr = match &args[0] {
                Value::Array(x) => x,
                _ => return Ok(Value::Null),
            };
            let sep = match &args[1] {
                Value::String(s) => s.as_str(),
                _ => "",
            };
            let parts: Vec<String> = arr.iter().map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())).collect();
            Ok(Value::String(parts.join(sep)))
        }
        "daysago" => {
            let a = one(name, args)?;
            if let Some(dt) = parse_ts(&a) {
                let now = Utc::now().naive_utc();
                let days = (now - dt).num_days();
                Ok(Value::from(days))
            } else {
                Ok(Value::Null)
            }
        }
        "nowunix" => Ok(Value::from(Utc::now().timestamp())),
        "dateadd" => {
            if args.len() != 2 && args.len() != 3 {
                return Err(arg_err(name, "2 or 3", args.len()));
            }
            let dt = parse_ts(&args[0]);
            let days = to_int(&args[1]).map(|d| d as i64);
            match (dt, days) {
                (Some(dt), Some(days)) => {
                    let _ = ChronoDuration::days(days);
                    let out = dt + ChronoDuration::days(days);
                    Ok(Value::String(out.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()))
                }
                _ => Ok(Value::Null),
            }
        }
        "year" => date_part(name, args, |d| d.year() as i64),
        "month" => date_part(name, args, |d| d.month() as i64),
        "day" => date_part(name, args, |d| d.day() as i64),
        "hour" => date_part(name, args, |d| d.hour() as i64),
        "minute" => date_part(name, args, |d| d.minute() as i64),
        "second" => date_part(name, args, |d| d.second() as i64),
        _ => Err(ExprError::new(0, format!("unknown function '{name}'"))),
    }
}

fn one_str(name: &str, args: &[Value], f: impl FnOnce(&str) -> String) -> Result<Value, ExprError> {
    let a = one(name, args)?;
    match a {
        Value::String(s) => Ok(Value::String(f(&s))),
        Value::Null => Ok(Value::Null),
        other => {
            let s = other.to_string();
            Ok(Value::String(f(&s)))
        }
    }
}

fn two_str(name: &str, args: &[Value], f: impl FnOnce(&str, &str) -> bool) -> Result<Value, ExprError> {
    if args.len() != 2 {
        return Err(arg_err(name, 2, args.len()));
    }
    let s = args[0].as_str().map(|s| s.to_string()).unwrap_or_else(|| args[0].to_string());
    let p = args[1].as_str().map(|s| s.to_string()).unwrap_or_default();
    Ok(Value::Bool(f(&s, &p)))
}

fn one(name: &str, args: &[Value]) -> Result<Value, ExprError> {
    if args.len() != 1 {
        return Err(arg_err(name, 1, args.len()));
    }
    Ok(args[0].clone())
}

fn num1(name: &str, args: &[Value], f: impl Fn(f64) -> f64) -> Result<Value, ExprError> {
    let a = one(name, args)?;
    let v = to_num(&a).ok_or_else(|| ExprError::new(0, format!("{name} needs a number")))?;
    Ok(Value::from(f(v)))
}

fn num2(name: &str, args: &[Value], f: impl Fn(f64, f64) -> f64) -> Result<Value, ExprError> {
    if args.len() != 2 {
        return Err(arg_err(name, 2, args.len()));
    }
    let a = to_num(&args[0]).ok_or_else(|| ExprError::new(0, format!("{name} needs numbers")))?;
    let b = to_num(&args[1]).ok_or_else(|| ExprError::new(0, format!("{name} needs numbers")))?;
    Ok(Value::from(f(a, b)))
}

fn num_fmt(name: &str, args: &[Value], f: impl Fn(f64, i64) -> f64) -> Result<Value, ExprError> {
    if args.len() != 1 && args.len() != 2 {
        return Err(arg_err(name, "1 or 2", args.len()));
    }
    let a = to_num(&args[0]).ok_or_else(|| ExprError::new(0, format!("{name} needs a number")))?;
    let d = if args.len() == 2 { to_int(&args[1]).unwrap_or(0) } else { 0 };
    Ok(Value::from(f(a, d)))
}

fn date_part(name: &str, args: &[Value], f: impl Fn(NaiveDateTime) -> i64) -> Result<Value, ExprError> {
    let a = one(name, args)?;
    if let Some(dt) = parse_ts(&a) {
        Ok(Value::from(f(dt)))
    } else {
        Ok(Value::Null)
    }
}

fn arg_err(name: &str, want: impl std::fmt::Display, got: usize) -> ExprError {
    ExprError::new(0, format!("{name} expects {want} args, got {got}"))
}
