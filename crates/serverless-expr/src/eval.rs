use crate::ast::{BinOp, Expr, ExprError, PathSeg, UnaryOp, MAX_DEPTH};
use crate::funcs;
use serde_json::Value;

pub fn eval(expr: &Expr, data: &Value, depth: usize) -> Result<Value, ExprError> {
    if depth > MAX_DEPTH {
        return Err(ExprError::new(0, "expression too deeply nested at eval"));
    }
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Path(segs) => Ok(resolve_path(data, segs)),
        Expr::Unary(op, inner) => {
            let v = eval(inner, data, depth + 1)?;
            match op {
                UnaryOp::Neg => Ok(Value::from(-as_num(&v))),
                UnaryOp::Not => Ok(Value::Bool(!funcs::truthy(&v))),
            }
        }
        Expr::Ternary(c, a, b) => {
            let cv = eval(c, data, depth + 1)?;
            if funcs::truthy(&cv) {
                eval(a, data, depth + 1)
            } else {
                eval(b, data, depth + 1)
            }
        }
        Expr::Binary(op, l, r) => {
            let lv = eval(l, data, depth + 1)?;
            match op {
                BinOp::And => {
                    if !funcs::truthy(&lv) {
                        return Ok(Value::Bool(false));
                    }
                    let rv = eval(r, data, depth + 1)?;
                    Ok(Value::Bool(funcs::truthy(&rv)))
                }
                BinOp::Or => {
                    if funcs::truthy(&lv) {
                        return Ok(Value::Bool(true));
                    }
                    let rv = eval(r, data, depth + 1)?;
                    Ok(Value::Bool(funcs::truthy(&rv)))
                }
                _ => {
                    let rv = eval(r, data, depth + 1)?;
                    eval_binary(op, &lv, &rv)
                }
            }
        }
        Expr::Call(name, args) => {
            if args.len() > 32 {
                return Err(ExprError::new(0, format!("too many args to {name}")));
            }
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval(a, data, depth + 1)?);
            }
            funcs::invoke(name, &vals)
        }
    }
}

fn eval_binary(op: &BinOp, l: &Value, r: &Value) -> Result<Value, ExprError> {
    match op {
        BinOp::Eq => Ok(Value::Bool(loose_eq(l, r))),
        BinOp::Ne => Ok(Value::Bool(!loose_eq(l, r))),
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
            let a = l.as_f64().or_else(|| l.as_i64().map(|i| i as f64));
            let b = r.as_f64().or_else(|| r.as_i64().map(|i| i as f64));
            match (a, b) {
                (Some(a), Some(b)) => {
                    let result = match op {
                        BinOp::Add => Value::from(a + b),
                        BinOp::Sub => Value::from(a - b),
                        BinOp::Mul => Value::from(a * b),
                        BinOp::Div => {
                            if b == 0.0 {
                                Value::Null
                            } else {
                                Value::from(a / b)
                            }
                        }
                        BinOp::Mod => {
                            if b == 0.0 {
                                Value::Null
                            } else {
                                Value::from(a % b)
                            }
                        }
                        _ => unreachable!(),
                    };
                    Ok(result)
                }
                _ => {
                    if op == &BinOp::Add {
                        Ok(bin_concat(l, r))
                    } else {
                        Ok(Value::Null)
                    }
                }
            }
        }
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            if let (Some(a), Some(b)) = (l.as_f64(), r.as_f64()) {
                let out = match op {
                    BinOp::Lt => a < b,
                    BinOp::Le => a <= b,
                    BinOp::Gt => a > b,
                    BinOp::Ge => a >= b,
                    _ => unreachable!(),
                };
                Ok(Value::Bool(out))
            } else {
                let a = scalar_str(l);
                let b = scalar_str(r);
                let out = match op {
                    BinOp::Lt => a < b,
                    BinOp::Le => a <= b,
                    BinOp::Gt => a > b,
                    BinOp::Ge => a >= b,
                    _ => unreachable!(),
                };
                Ok(Value::Bool(out))
            }
        }
        BinOp::And | BinOp::Or => unreachable!("handled in eval"),
    }
}

fn as_num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn scalar_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        _ => v.to_string(),
    }
}

fn bin_concat(l: &Value, r: &Value) -> Value {
    Value::String(format!("{}{}", scalar_str(l), scalar_str(r)))
}

fn loose_eq(l: &Value, r: &Value) -> bool {
    match (l.as_f64(), r.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => {
            if l.is_number() && r.is_number() {
                false
            } else {
                l == r
            }
        }
    }
}

pub fn resolve_path(root: &Value, segs: &[PathSeg]) -> Value {
    let mut cur = root;
    for seg in segs {
        match seg {
            PathSeg::Key(k) => {
                cur = match cur {
                    Value::Object(m) => m.get(k).unwrap_or(&Value::Null),
                    _ => &Value::Null,
                };
            }
            PathSeg::Index(i) => {
                cur = match cur {
                    Value::Array(a) => a.get(*i).unwrap_or(&Value::Null),
                    _ => &Value::Null,
                };
            }
            PathSeg::Wild => {
                let mut acc = Vec::new();
                collect_wild(cur, &mut acc);
                return Value::Array(acc);
            }
        }
    }
    cur.clone()
}

fn collect_wild(v: &Value, acc: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            for val in m.values() {
                acc.push(val.clone());
            }
        }
        Value::Array(a) => {
            for val in a.iter() {
                acc.push(val.clone());
            }
        }
        _ => acc.push(v.clone()),
    }
}
