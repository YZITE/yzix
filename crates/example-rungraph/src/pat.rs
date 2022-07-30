use anyhow::bail;
use core::mem::replace;

#[derive(Clone, Debug)]
pub enum PatternElem {
    S(String),
    Eval(String),
    UploadFile(String),
}

pub type Pattern = Vec<PatternElem>;

pub fn parse_pattern(s: &str) -> anyhow::Result<Pattern> {
    let mut ret = Vec::new();
    let mut is_eval = false;

    for i in s.chars() {
        if i == '`' {
            is_eval = !is_eval;
            if is_eval {
                ret.push(PatternElem::Eval(String::new()));
            }
            continue;
        }

        if is_eval {
            if let PatternElem::Eval(ref mut x) = ret.last_mut() {
                x.push(i);
            } else {
                unreachable!();
            }
        } else {
            let mut sxr = if let PatternElem::S(ref mut x) = ret.last_mut() {
                x
            } else {
                ret.push(PatternElem::S(String::new()));
                if let PatternElem::S(ref mut x) = ret.last_mut() {
                    x
                } else {
                    unreachable!();
                }
            };
            sxr.push(i);
        }
    }

    if is_eval {
        bail!("pattern {:?} ends with non-closed eval expr", s);
    }

    for i in &mut ret {
        if let PatternElem::S(ref mut x) = i {
            if let Some(upf) = x.strip_prefix('<') {
                *i = PatternElem::UploadFile(upf.to_string());
            }
        }
    }

    Ok(ret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() {
        assert!(parse_pattern("abc def").unwrap() == vec![PatternElem::S("abc def".to_string())]);
    }

    fn e() {
        assert!(parse_pattern("`abc def`").unwrap() == vec![PatternElem::Eval("abc def".to_string())]);
    }

    fn upf() {
        assert!(parse_pattern("`<abc def>`").unwrap() == vec![PatternElem::UploadFile("abc def>".to_string())]);
        assert!(parse_pattern("`<./abc def`").unwrap() == vec![PatternElem::UploadFile("./abc def".to_string())]);
    }

    fn es() {
        assert!(parse_pattern("xytz`abc def`wer").unwrap() == vec![
            PatternElem::S("xytz".to_string()),
            PatternElem::Eval("abc def".to_string()),
            PatternElem::S("wer".to_string()),
        ]);
    }
}
