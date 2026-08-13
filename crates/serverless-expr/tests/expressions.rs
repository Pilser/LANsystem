use serde_json::json;
use serverless_expr::{evaluate, parse, truthy};

fn ctx() -> serde_json::Value {
    json!({
        "price": 10,
        "qty": 3,
        "name": "Widget",
        "items": [{"qty": 2}, {"qty": 5}],
        "meta": {"a": 1, "b": 2},
        "at": "2024-01-01T00:00:00Z",
        "status": "paid"
    })
}

#[test]
fn arithmetic() {
    assert_eq!(evaluate("$.price * $.qty", &ctx()).unwrap(), json!(30.0));
    assert_eq!(evaluate("1 + 2 * 3", &ctx()).unwrap(), json!(7.0));
    assert_eq!(evaluate("(1 + 2) * 3", &ctx()).unwrap(), json!(9.0));
    assert_eq!(evaluate("10 % 3", &ctx()).unwrap(), json!(1.0));
}

#[test]
fn path_and_index() {
    assert_eq!(evaluate("$.items[1].qty", &ctx()).unwrap(), json!(5));
    assert_eq!(evaluate("$.missing", &ctx()).unwrap(), json!(null));
    assert_eq!(evaluate("$", &ctx()).unwrap(), ctx());
}

#[test]
fn comparison_and_logic() {
    assert_eq!(truthy("$.price >= 10 && $.qty > 1", &ctx()).unwrap(), true);
    assert_eq!(truthy("$.status == 'paid'", &ctx()).unwrap(), true);
    assert_eq!(truthy("$.price > 100", &ctx()).unwrap(), false);
    assert_eq!(truthy("$.price < 100 || $.qty > 100", &ctx()).unwrap(), true);
}

#[test]
fn ternary() {
    assert_eq!(evaluate("$.qty > 0 ? 'yes' : 'no'", &ctx()).unwrap(), json!("yes"));
    assert_eq!(evaluate("$.missing ? 1 : 2", &ctx()).unwrap(), json!(2));
}

#[test]
fn functions() {
    assert_eq!(evaluate("lower($.name)", &ctx()).unwrap(), json!("widget"));
    assert_eq!(evaluate("upper('abc')", &ctx()).unwrap(), json!("ABC"));
    assert_eq!(evaluate("trim('  hi  ')", &ctx()).unwrap(), json!("hi"));
    assert_eq!(evaluate("concat($.name, '!')", &ctx()).unwrap(), json!("Widget!"));
    assert_eq!(evaluate("len($.items)", &ctx()).unwrap(), json!(2));
    assert_eq!(evaluate("if($.qty > 0, 'in', 'out')", &ctx()).unwrap(), json!("in"));
    assert_eq!(evaluate("coalesce($.missing, 'default')", &ctx()).unwrap(), json!("default"));
    assert_eq!(evaluate("round(3.14159, 2)", &ctx()).unwrap(), json!(3.14));
    assert_eq!(evaluate("contains('hello', 'ell')", &ctx()).unwrap(), json!(true));
    assert_eq!(evaluate("startswith('hello', 'he')", &ctx()).unwrap(), json!(true));
    assert_eq!(evaluate("nowunix()", &ctx()).unwrap().as_i64().map(|v| v > 0), Some(true));
}

#[test]
fn wildcard() {
    let v = evaluate("$.meta.*", &ctx()).unwrap();
    assert!(v.is_array() && v.as_array().unwrap().len() == 2);
}

#[test]
fn hostile_input_no_panic() {
    for bad in ["", "$.a..b", "(1", "1 +", "foo(", "$$", "()", "\"unterminated"] {
        let _ = parse(bad);
        let _ = evaluate(bad, &ctx());
    }
}

#[test]
fn deep_nesting_bounded() {
    let mut s = String::new();
    for _ in 0..2000 {
        s.push('(');
    }
    s.push('1');
    for _ in 0..2000 {
        s.push(')');
    }
    let r = evaluate(&s, &ctx());
    assert!(r.is_err());
}
