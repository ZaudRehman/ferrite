//! Local arithmetic + unit-conversion answers. No network. Bare `10-3`
//! stays a search (phones and dates collide). Currency is out of scope
//! (needs live rates); everything here is exact constants.

/// Formatted result. None when the input is not an answerable sum.
pub fn instant_math(input: &str) -> Option<String> {
    if let Some(conv) = instant_convert(input) {
        return Some(conv);
    }
    if !is_math_input(input) {
        return None;
    }
    let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let value = Parser::new(&compact).parse()?;
    if !value.is_finite() {
        return None;
    }
    Some(format_number(value))
}

/// Base-unit factors: (spelling, canonical symbol, to-base factor).
/// Temperature converts through dedicated formulas, not factors
/// (see `convert_temp`).
const LENGTH_M: &[(&str, &str, f64)] = &[
    ("mm", "mm", 0.001),
    ("millimeter", "mm", 0.001),
    ("millimeters", "mm", 0.001),
    ("cm", "cm", 0.01),
    ("centimeter", "cm", 0.01),
    ("centimeters", "cm", 0.01),
    ("m", "m", 1.0),
    ("meter", "m", 1.0),
    ("meters", "m", 1.0),
    ("km", "km", 1000.0),
    ("kilometer", "km", 1000.0),
    ("kilometers", "km", 1000.0),
    ("in", "in", 0.0254),
    ("inch", "in", 0.0254),
    ("inches", "in", 0.0254),
    ("ft", "ft", 0.3048),
    ("foot", "ft", 0.3048),
    ("feet", "ft", 0.3048),
    ("yd", "yd", 0.9144),
    ("yard", "yd", 0.9144),
    ("yards", "yd", 0.9144),
    ("mi", "mi", 1609.344),
    ("mile", "mi", 1609.344),
    ("miles", "mi", 1609.344),
];
const MASS_KG: &[(&str, &str, f64)] = &[
    ("mg", "mg", 1e-6),
    ("milligram", "mg", 1e-6),
    ("milligrams", "mg", 1e-6),
    ("g", "g", 1e-3),
    ("gram", "g", 1e-3),
    ("grams", "g", 1e-3),
    ("kg", "kg", 1.0),
    ("kilogram", "kg", 1.0),
    ("kilograms", "kg", 1.0),
    ("t", "t", 1000.0),
    ("ton", "t", 1000.0),
    ("tons", "t", 1000.0),
    ("oz", "oz", 0.028349523125),
    ("ounce", "oz", 0.028349523125),
    ("ounces", "oz", 0.028349523125),
    ("lb", "lb", 0.45359237),
    ("pound", "lb", 0.45359237),
    ("pounds", "lb", 0.45359237),
];
const DATA_B: &[(&str, &str, f64)] = &[
    ("b", "B", 1.0),
    ("byte", "B", 1.0),
    ("bytes", "B", 1.0),
    ("kb", "KB", 1024.0),
    ("kilobyte", "KB", 1024.0),
    ("kilobytes", "KB", 1024.0),
    ("mb", "MB", 1048576.0),
    ("megabyte", "MB", 1048576.0),
    ("megabytes", "MB", 1048576.0),
    ("gb", "GB", 1073741824.0),
    ("gigabyte", "GB", 1073741824.0),
    ("gigabytes", "GB", 1073741824.0),
    ("tb", "TB", 1099511627776.0),
    ("terabyte", "TB", 1099511627776.0),
    ("terabytes", "TB", 1099511627776.0),
];
const VOLUME_L: &[(&str, &str, f64)] = &[
    ("ml", "mL", 0.001),
    ("milliliter", "mL", 0.001),
    ("milliliters", "mL", 0.001),
    ("l", "L", 1.0),
    ("liter", "L", 1.0),
    ("liters", "L", 1.0),
    ("litre", "L", 1.0),
    ("litres", "L", 1.0),
    ("gal", "gal", 3.785411784),
    ("gallon", "gal", 3.785411784),
    ("gallons", "gal", 3.785411784),
    ("floz", "fl oz", 0.0295735295625),
];
const TIME_S: &[(&str, &str, f64)] = &[
    ("ms", "ms", 0.001),
    ("millisecond", "ms", 0.001),
    ("milliseconds", "ms", 0.001),
    ("s", "s", 1.0),
    ("sec", "s", 1.0),
    ("second", "s", 1.0),
    ("seconds", "s", 1.0),
    ("min", "min", 60.0),
    ("minute", "min", 60.0),
    ("minutes", "min", 60.0),
    ("hr", "hr", 3600.0),
    ("hour", "hr", 3600.0),
    ("hours", "hr", 3600.0),
    ("day", "day", 86400.0),
    ("days", "day", 86400.0),
    ("week", "wk", 604800.0),
    ("weeks", "wk", 604800.0),
];
const SPEED_MPS: &[(&str, &str, f64)] = &[
    ("mps", "m/s", 1.0),
    ("mph", "mph", 0.44704),
    ("kmh", "km/h", 1000.0 / 3600.0),
    ("kph", "km/h", 1000.0 / 3600.0),
    ("knot", "kt", 0.514444),
    ("knots", "kt", 0.514444),
];

/// (category, canonical symbol, to-base factor). Temperature returns a
/// dummy factor: it converts via `convert_temp`, matched by category.
fn lookup_unit(unit: &str) -> Option<(&'static str, &'static str, f64)> {
    let u = unit.trim().to_lowercase();
    match u.as_str() {
        "c" | "celsius" | "°c" => return Some(("temp", "°C", 0.0)),
        "f" | "fahrenheit" | "°f" => return Some(("temp", "°F", 0.0)),
        "k" | "kelvin" => return Some(("temp", "K", 0.0)),
        _ => {}
    }
    // Exact table hit, else one trailing-'s' depluralization ("lbs" is
    // not a unit; "pounds" already matched above).
    for (cat, table) in [
        ("length", LENGTH_M),
        ("mass", MASS_KG),
        ("data", DATA_B),
        ("volume", VOLUME_L),
        ("time", TIME_S),
        ("speed", SPEED_MPS),
    ] {
        if let Some((_, canon, f)) = table.iter().find(|(n, _, _)| *n == u) {
            return Some((cat, canon, *f));
        }
        if let Some(stripped) = u.strip_suffix('s')
            && let Some((_, canon, f)) = table.iter().find(|(n, _, _)| *n == stripped)
        {
            return Some((cat, canon, *f));
        }
    }
    None
}

fn convert_temp(value: f64, from: &str, to: &str) -> Option<f64> {
    let c = match from {
        "°C" => value,
        "°F" => (value - 32.0) * 5.0 / 9.0,
        "K" => value - 273.15,
        _ => return None,
    };
    match to {
        "°C" => Some(c),
        "°F" => Some(c * 9.0 / 5.0 + 32.0),
        "K" => Some(c + 273.15),
        _ => None,
    }
}

/// `<number> <unit> in|to|= <unit>`: "72f to c", "5 mi in km",
/// "2kg=lb". Single values only ("5 ft 9 in" stays a search).
/// Mismatched categories return None (never a nonsense answer).
pub fn instant_convert(input: &str) -> Option<String> {
    let t = input.trim();
    if t.is_empty() || t.len() > 64 || !t.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let lower = t.to_lowercase();
    let (left, right) = if let Some(i) = lower.find(" in ") {
        (&t[..i], &t[i + 4..])
    } else if let Some(i) = lower.find(" to ") {
        (&t[..i], &t[i + 4..])
    } else if let Some(i) = t.find('=') {
        (&t[..i], &t[i + 1..])
    } else {
        return None;
    };
    // Leading float, then the unit spelling.
    let l = left.trim_start();
    let bytes = l.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut digits = 0;
    let mut dot = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            digits += 1;
            i += 1;
        } else if c == b'.' && !dot {
            dot = true;
            i += 1;
        } else {
            break;
        }
    }
    if digits == 0 {
        return None;
    }
    let num_end = i;
    let value: f64 = l[..num_end].parse().ok()?;
    if !value.is_finite() || value.abs() > 1e15 {
        return None;
    }
    let from = &l[num_end..];
    let to = right.trim();
    if from.trim().is_empty() || to.is_empty() {
        return None;
    }
    let (fcat, _, ff) = lookup_unit(from)?;
    let (tcat, tcanon, tf) = lookup_unit(to)?;
    if fcat != tcat {
        return None;
    }
    let out = if fcat == "temp" {
        let (_, fcanon, _) = lookup_unit(from)?;
        convert_temp(value, fcanon, tcanon)?
    } else {
        value * ff / tf
    };
    if !out.is_finite() {
        return None;
    }
    Some(format!("{} {tcanon}", format_number(out)))
}

fn is_math_input(input: &str) -> bool {
    let t = input.trim();
    if t.is_empty() || !t.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_digit() || "+-*/%^(). ".contains(c))
    {
        return false;
    }
    t.chars().any(|c| "*/%^()".contains(c)) || t.contains(" + ") || t.contains(" - ")
}

fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.8}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

struct Parser<'a> {
    chars: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            chars: s.as_bytes(),
            pos: 0,
        }
    }

    fn parse(mut self) -> Option<f64> {
        let v = self.expr()?;
        (self.pos == self.chars.len()).then_some(v)
    }

    fn peek(&self) -> Option<u8> {
        self.chars.get(self.pos).copied()
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    v += self.term()?;
                }
                Some(b'-') => {
                    self.pos += 1;
                    v -= self.term()?;
                }
                _ => return Some(v),
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.signed()?;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    v *= self.signed()?;
                }
                Some(b'/') => {
                    self.pos += 1;
                    let d = self.signed()?;
                    if d == 0.0 {
                        return None;
                    }
                    v /= d;
                }
                Some(b'%') => {
                    self.pos += 1;
                    let d = self.signed()?;
                    if d == 0.0 {
                        return None;
                    }
                    v %= d;
                }
                _ => return Some(v),
            }
        }
    }

    fn signed(&mut self) -> Option<f64> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
            Some(-self.signed()?)
        } else {
            self.power()
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.primary()?;
        if self.peek() == Some(b'^') {
            self.pos += 1;
            Some(base.powf(self.signed()?))
        } else {
            Some(base)
        }
    }

    fn primary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                let v = self.expr()?;
                if self.peek() != Some(b')') {
                    return None;
                }
                self.pos += 1;
                Some(v)
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => self.number(),
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.pos;
        let mut dot = false;
        let mut digits = 0;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                digits += 1;
                self.pos += 1;
            } else if c == b'.' && !dot {
                dot = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        if digits == 0 {
            return None;
        }
        std::str::from_utf8(&self.chars[start..self.pos])
            .ok()?
            .parse()
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ans(input: &str) -> Option<String> {
        instant_math(input)
    }

    #[test]
    fn arithmetic_basics() {
        assert_eq!(ans("241*17"), Some("4097".to_owned()));
        assert_eq!(ans("10 - 3"), Some("7".to_owned()));
        assert_eq!(ans("(2+3)*4"), Some("20".to_owned()));
        assert_eq!(ans("2^10"), Some("1024".to_owned()));
        assert_eq!(ans("7%3"), Some("1".to_owned()));
        assert_eq!(ans("10/4"), Some("2.5".to_owned()));
        assert_eq!(ans("-2^2"), Some("-4".to_owned()));
        assert_eq!(ans(" 3 * (4 + 5) "), Some("27".to_owned()));
    }

    #[test]
    fn rejects_non_math() {
        assert_eq!(ans(""), None);
        assert_eq!(ans("hello"), None);
        assert_eq!(ans("example.com"), None);
        assert_eq!(ans("555-1234"), None);
        assert_eq!(ans("2024-01-01"), None);
        assert_eq!(ans("10-3"), None);
        assert_eq!(ans("1.2.3"), None);
    }

    #[test]
    fn rejects_bad_math() {
        assert_eq!(ans("10/0"), None);
        assert_eq!(ans("10%0"), None);
        assert_eq!(ans("2+"), None);
        assert_eq!(ans("(2+3"), None);
        assert_eq!(ans("2^^3"), None);
        assert_eq!(ans("()"), None);
    }

    #[test]
    fn converts_units() {
        assert_eq!(ans("5 mi in km"), Some("8.04672 km".to_owned()));
        assert_eq!(ans("72f to c"), Some("22.22222222 °C".to_owned()));
        assert_eq!(ans("100c to f"), Some("212 °F".to_owned()));
        assert_eq!(ans("-40f to c"), Some("-40 °C".to_owned()));
        assert_eq!(ans("273.15k to c"), Some("0 °C".to_owned()));
        assert_eq!(ans("2kg in lb"), Some("4.40924524 lb".to_owned()));
        assert_eq!(ans("1GB in MB"), Some("1024 MB".to_owned()));
        assert_eq!(ans("10 min in seconds"), Some("600 s".to_owned()));
        assert_eq!(ans("100mph in kmh"), Some("160.9344 km/h".to_owned()));
        assert_eq!(ans("2.5gal to l"), Some("9.46352946 L".to_owned()));
        assert_eq!(ans("3.5kg=lb"), Some("7.71617918 lb".to_owned()));
        assert_eq!(ans("1 week in days"), Some("7 day".to_owned()));
    }

    #[test]
    fn conversions_reject_mismatch_and_prose() {
        // Category mismatch is None, never a nonsense answer.
        assert_eq!(ans("5kg in km"), None);
        assert_eq!(ans("10c in kg"), None);
        // Unknown units, missing halves, compounds stay searches.
        assert_eq!(ans("5 blorps in km"), None);
        assert_eq!(ans("5kg in"), None);
        assert_eq!(ans("in km"), None);
        assert_eq!(ans("5 ft 9 in"), None);
        assert_eq!(ans("5g in town"), None);
        // Prose with numbers never converts (number must lead).
        assert_eq!(ans("top 10 movies of 2024"), None);
        assert_eq!(ans("flight 305 in miles"), None);
        assert_eq!(ans("rain in spain"), None);
        // Arithmetic still wins its own inputs (spaceless `2+2` stays a
        // search by the phones/dates guard; that behavior is unchanged).
        assert_eq!(ans("2 + 2"), Some("4".to_owned()));
        assert_eq!(ans("2+2"), None);
    }
}
