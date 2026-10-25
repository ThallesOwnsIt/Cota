//! Conversão de texto decimal para inteiro escalado x1000 sem passar por ponto flutuante.

/// "17,2" -> 17_200; ",8" -> 800; "-3.25" -> -3_250; "1010" -> 1_010_000.
/// Aceita vírgula ou ponto. Casas além da terceira são arredondadas (meio para longe de zero).
pub fn parse_milli(text: &str) -> Option<i64> {
    let t = text.trim();
    let (negative, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let (int_part, frac_part) = match t.find([',', '.']) {
        Some(i) => (&t[..i], &t[i + 1..]),
        None => (t, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit()) || !frac_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    let int: i64 = if int_part.is_empty() { 0 } else { int_part.parse().ok()? };
    let mut digits = frac_part.bytes().map(|b| (b - b'0') as i64);
    let mut frac = 0;
    for _ in 0..3 {
        frac = frac * 10 + digits.next().unwrap_or(0);
    }
    if digits.next().is_some_and(|d| d >= 5) {
        frac += 1;
    }
    let value = int.checked_mul(1000)?.checked_add(frac)?;
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::parse_milli;

    #[test]
    fn parses_inmet_style_numbers() {
        assert_eq!(parse_milli("17,2"), Some(17_200));
        assert_eq!(parse_milli(",8"), Some(800));
        assert_eq!(parse_milli("0"), Some(0));
        assert_eq!(parse_milli("1010,6"), Some(1_010_600));
        assert_eq!(parse_milli("-9999"), Some(-9_999_000));
        assert_eq!(parse_milli("12792.81"), Some(12_792_810));
        assert_eq!(parse_milli("0.0005"), Some(1));
        assert_eq!(parse_milli("-3.2504"), Some(-3_250));
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "-", ",", "abc", "1,2,3", "1e3", " , "] {
            assert_eq!(parse_milli(bad), None, "{bad:?}");
        }
    }
}
