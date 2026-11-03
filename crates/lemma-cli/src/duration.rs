//! Durations as people write them in settings and flags.

use std::time::Duration;

use anyhow::bail;

/// `30s`, `10m`, `2h`, `1d`, or `never`.
pub(crate) fn parse_duration(text: &str) -> anyhow::Result<Option<Duration>> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("never") {
        return Ok(None);
    }
    let (number, unit) = split_number(text);
    let Ok(amount) = number.parse::<u64>() else {
        bail!("`{text}` is not a duration; use a number with s, m, h or d, or `never`");
    };
    let seconds = match unit {
        "s" => amount,
        "m" => amount * 60,
        "h" => amount * 60 * 60,
        "d" => amount * 24 * 60 * 60,
        _ => bail!("`{text}` needs a unit: s, m, h or d"),
    };
    Ok(Some(Duration::from_secs(seconds)))
}

/// The leading digits of `text`, and the rest.
pub(crate) fn split_number(text: &str) -> (&str, &str) {
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    (number, unit.trim())
}
