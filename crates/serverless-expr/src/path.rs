use crate::ast::{ExprError, PathSeg};

pub fn parse_path(raw: &str) -> Result<Vec<PathSeg>, ExprError> {
    if raw == "$" {
        return Ok(vec![]);
    }
    let b = raw.as_bytes();
    if !raw.starts_with('$') {
        return Err(ExprError::new(0, "path must start with '$'"));
    }
    let mut segs: Vec<PathSeg> = Vec::new();
    let mut i = 1;
    let n = b.len();
    let mut key = String::new();
    let mut saw_any = false;

    let flush_key = |key: &mut String, segs: &mut Vec<PathSeg>, saw: &mut bool| {
        if !key.is_empty() {
            segs.push(PathSeg::Key(std::mem::take(key)));
            *saw = true;
        }
    };

    while i < n {
        let c = b[i] as char;
        match c {
            '.' => {
                if i + 1 < n && (b[i + 1] as char) == '.' {
                    return Err(ExprError::new(i, "double dot in path"));
                }
                flush_key(&mut key, &mut segs, &mut saw_any);
                i += 1;
            }
            '*' => {
                flush_key(&mut key, &mut segs, &mut saw_any);
                segs.push(PathSeg::Wild);
                saw_any = true;
                i += 1;
            }
            '[' => {
                flush_key(&mut key, &mut segs, &mut saw_any);
                let mut j = i + 1;
                if j < n && (b[j] as char) == '*' {
                    if j + 1 >= n || (b[j + 1] as char) != ']' {
                        return Err(ExprError::new(i, "malformed [*] in path"));
                    }
                    segs.push(PathSeg::Wild);
                    saw_any = true;
                    i = j + 2;
                    continue;
                }
                let mut num = String::new();
                while j < n && (b[j] as char).is_ascii_digit() {
                    num.push(b[j] as char);
                    j += 1;
                }
                if !num.is_empty() && j < n && (b[j] as char) == ']' {
                    let idx = num.parse::<usize>().map_err(|_| ExprError::new(i, "index out of range"))?;
                    segs.push(PathSeg::Index(idx));
                    saw_any = true;
                    i = j + 1;
                } else {
                    return Err(ExprError::new(i, "malformed index in path"));
                }
            }
            _ => {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    key.push(c);
                    i += 1;
                } else {
                    return Err(ExprError::new(i, format!("invalid character '{c}' in path")));
                }
            }
        }
    }
    flush_key(&mut key, &mut segs, &mut saw_any);
    if !saw_any {
        return Err(ExprError::new(0, "empty path"));
    }
    Ok(segs)
}
