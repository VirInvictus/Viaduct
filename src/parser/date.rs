use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc};

/// Parse a date string. Returns None for empty input.
pub fn parse_date(date_string: &str) -> Option<DateTime<Utc>> {
    parse_date_bytes(date_string.as_bytes())
}

/// Parse a date from a byte slice — the hot path for the feed parsers.
pub fn parse_date_bytes(bytes: &[u8]) -> Option<DateTime<Utc>> {
    let count = bytes.len();
    if !(6..=150).contains(&count) {
        return None;
    }

    if looks_like_w3c_date(bytes)
        && let Some(date) = parse_w3c_date(bytes)
    {
        return Some(date);
    }
    if looks_like_pub_date(bytes)
        && let Some(date) = parse_pub_date(bytes)
    {
        return Some(date);
    }

    // Fallback: try W3C.
    parse_w3c_date(bytes)
}

// MARK: - Format sniffing

fn looks_like_pub_date(slice: &[u8]) -> bool {
    slice.iter().any(|&b| b == b' ' || b == b',')
}

fn looks_like_w3c_date(slice: &[u8]) -> bool {
    for i in 0..slice.len() {
        let b = slice[i];
        if b == b' ' || b == b'\r' || b == b'\n' || b == b'\t' {
            continue;
        }
        if slice.len() - i < 5 {
            return false;
        }
        let separator = slice[i + 4];
        return is_digit(slice[i])
            && is_digit(slice[i + 1])
            && is_digit(slice[i + 2])
            && is_digit(slice[i + 3])
            && (separator == b'-' || separator == b'/');
    }
    false
}

// MARK: - PubDate (RFC 822 / 2822)

fn parse_pub_date(slice: &[u8]) -> Option<DateTime<Utc>> {
    let mut final_index = 0;

    let mut day = next_numeric_value(slice, 0, 2, &mut final_index).unwrap_or(1);
    if day < 1 {
        day = 1;
    }

    let month = next_month_value(slice, final_index + 1, &mut final_index).unwrap_or(1);

    let mut year = next_numeric_value(slice, final_index + 1, 4, &mut final_index);
    if let Some(y) = year
        && y < 100
    {
        year = Some(y + 2000);
    }

    let mut hour = next_numeric_value(slice, final_index + 1, 2, &mut final_index).unwrap_or(0);
    if hour < 0 {
        hour = 0;
    }

    let mut minute = next_numeric_value(slice, final_index + 1, 2, &mut final_index).unwrap_or(0);
    if minute < 0 {
        minute = 0;
    }

    let mut current_index = final_index + 1;
    let has_seconds = current_index < slice.len() && slice[current_index] == b':';
    let mut second = 0;
    if has_seconds {
        second = next_numeric_value(slice, current_index, 2, &mut final_index).unwrap_or(0);
    }

    current_index = final_index + 1;
    let has_time_zone = current_index < slice.len() && slice[current_index] == b' ';
    let mut time_zone_offset = 0;
    if has_time_zone {
        time_zone_offset = parsed_time_zone_offset(slice, current_index);
    }

    date_with_components(
        year.unwrap_or(1970),
        month as u32,
        day as u32,
        hour as u32,
        minute as u32,
        second as u32,
        0,
        time_zone_offset,
    )
}

// MARK: - W3C / ISO 8601

fn parse_w3c_date(slice: &[u8]) -> Option<DateTime<Utc>> {
    let mut final_index = 0;

    let year = next_numeric_value(slice, 0, 4, &mut final_index).unwrap_or(1970);
    let month = next_numeric_value(slice, final_index + 1, 2, &mut final_index).unwrap_or(1);
    let day = next_numeric_value(slice, final_index + 1, 2, &mut final_index).unwrap_or(1);
    let hour = next_numeric_value(slice, final_index + 1, 2, &mut final_index).unwrap_or(0);
    let minute = next_numeric_value(slice, final_index + 1, 2, &mut final_index).unwrap_or(0);
    let second = next_numeric_value(slice, final_index + 1, 2, &mut final_index).unwrap_or(0);

    let mut current_index = final_index + 1;
    let mut milliseconds = 0;
    let has_milliseconds = current_index < slice.len() && slice[current_index] == b'.';
    if has_milliseconds {
        milliseconds = next_numeric_value(slice, current_index, 3, &mut final_index).unwrap_or(0);
        current_index = final_index + 1;
        while current_index < slice.len() && is_digit(slice[current_index]) {
            current_index += 1;
        }
    } else {
        // even if it didn't have ms, advance
        current_index = final_index + 1;
    }

    let time_zone_offset = parsed_time_zone_offset(slice, current_index);

    date_with_components(
        year,
        month as u32,
        day as u32,
        hour as u32,
        minute as u32,
        second as u32,
        milliseconds as u32,
        time_zone_offset,
    )
}

// MARK: - Components → Date

#[allow(clippy::too_many_arguments)]
fn date_with_components(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    milliseconds: u32,
    time_zone_offset: i32,
) -> Option<DateTime<Utc>> {
    // We're instructed to use chrono
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let time = NaiveTime::from_hms_milli_opt(hour, minute, second, milliseconds)?;
    let naive_dt = date.and_time(time);

    let offset = FixedOffset::east_opt(time_zone_offset)?;
    let dt_with_offset = offset.from_local_datetime(&naive_dt).single()?;
    Some(dt_with_offset.with_timezone(&Utc))
}

// MARK: - Numeric and alphabetic scanning

#[inline(always)]
fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

#[inline(always)]
fn is_alpha(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

fn next_numeric_value(
    slice: &[u8],
    starting_index: usize,
    max_digits: usize,
    final_index: &mut usize,
) -> Option<i32> {
    let limit = if max_digits > 4 { 4 } else { max_digits };
    let end = slice.len();
    let mut i = starting_index;

    while i < end {
        if is_digit(slice[i]) {
            break;
        }
        *final_index = i;
        i += 1;
    }
    if i >= end {
        return None;
    }

    let mut value = 0;
    let mut digits_read = 0;
    while i < end {
        let b = slice[i];
        if !is_digit(b) {
            break;
        }
        value = value * 10 + (b - b'0') as i32;
        digits_read += 1;
        *final_index = i;
        i += 1;
        if digits_read >= limit {
            break;
        }
    }
    Some(value)
}

fn next_month_value(slice: &[u8], starting_index: usize, final_index: &mut usize) -> Option<i32> {
    let mut packed: u32 = 0;
    let mut chars_read = 0;

    let mut i = starting_index;
    while i < slice.len() {
        *final_index = i;
        let b = slice[i];
        if !is_alpha(b) {
            if chars_read == 0 {
                i += 1;
                continue;
            }
            break;
        }

        if chars_read == 0 {
            match b | 0x20 {
                b'f' => return Some(2),
                b's' => return Some(9),
                b'o' => return Some(10),
                b'n' => return Some(11),
                b'd' => return Some(12),
                _ => {}
            }
        }

        packed |= ((b | 0x20) as u32) << (chars_read * 8);
        chars_read += 1;
        if chars_read >= 3 {
            break;
        }
        i += 1;
    }

    if chars_read < 2 {
        return None;
    }

    let c0 = (packed & 0xFF) as u8;
    let c1 = ((packed >> 8) & 0xFF) as u8;
    let c2 = ((packed >> 16) & 0xFF) as u8;

    match c0 {
        b'j' => {
            if c1 == b'a' {
                return Some(1);
            }
            if c1 == b'u' {
                if chars_read >= 3 && c2 == b'n' {
                    return Some(6);
                }
                return Some(7);
            }
            Some(1)
        }
        b'm' => {
            if chars_read >= 3 && c2 == b'y' {
                return Some(5);
            }
            Some(3)
        }
        b'a' => {
            if c1 == b'u' {
                return Some(8);
            }
            Some(4)
        }
        _ => Some(1),
    }
}

// MARK: - Time zones

fn parsed_time_zone_offset(slice: &[u8], starting_index: usize) -> i32 {
    let mut packed: u64 = 0;
    let mut chars_read = 0;
    let mut has_alpha = false;
    let mut first_byte: u8 = 0;

    let mut i = starting_index;
    while i < slice.len() && chars_read < 5 {
        let b = slice[i];
        if b == b':' || b == b' ' {
            i += 1;
            continue;
        }
        if is_digit(b) || is_alpha(b) || b == b'+' || b == b'-' {
            let lower = b | 0x20;
            if chars_read == 0 {
                first_byte = lower;
            }
            if is_alpha(b) {
                has_alpha = true;
            }
            packed |= (lower as u64) << (chars_read * 8);
            chars_read += 1;
        }
        i += 1;
    }

    if chars_read == 0 {
        return 0;
    }
    if first_byte == b'z' {
        return 0;
    }

    if has_alpha {
        if packed == pack_ascii(b"gmt") || packed == pack_ascii(b"utc") {
            return 0;
        }
        return time_zone_offsets(packed).unwrap_or(0);
    }

    offset_for_signed_numeric_offset(packed, chars_read)
}

fn offset_for_signed_numeric_offset(packed: u64, chars_read: usize) -> i32 {
    if chars_read == 0 {
        return 0;
    }
    let first = (packed & 0xFF) as u8;
    let is_plus = first == b'+';

    let digit = |pos: usize| -> Option<i32> {
        if pos < chars_read {
            let b = ((packed >> (pos * 8)) & 0xFF) as u8;
            if is_digit(b) {
                return Some((b - b'0') as i32);
            }
        }
        None
    };

    let mut hours = 0;
    if let (Some(d1), Some(d2)) = (digit(1), digit(2)) {
        hours = d1 * 10 + d2;
    } else if let Some(d1) = digit(1) {
        hours = d1;
    }

    let mut minutes = 0;
    if let (Some(d3), Some(d4)) = (digit(3), digit(4)) {
        minutes = d3 * 10 + d4;
    } else if let Some(d3) = digit(3) {
        minutes = d3;
    }

    if hours == 0 && minutes == 0 {
        return 0;
    }
    let seconds = hours * 3600 + minutes * 60;
    if is_plus { seconds } else { -seconds }
}

fn pack_ascii(s: &[u8]) -> u64 {
    let mut result: u64 = 0;
    let count = std::cmp::min(s.len(), 8);
    for (i, &b) in s.iter().enumerate().take(count) {
        result |= ((b | 0x20) as u64) << (i * 8);
    }
    result
}

#[inline(always)]
fn offset(hours: i32, minutes: i32) -> i32 {
    if hours < 0 {
        hours * 3600 - minutes * 60
    } else {
        hours * 3600 + minutes * 60
    }
}

fn time_zone_offsets(packed: u64) -> Option<i32> {
    // Switch on packed ascii representation.
    match packed {
        p if p == pack_ascii(b"pdt") => Some(offset(-7, 0)),
        p if p == pack_ascii(b"pst") => Some(offset(-8, 0)),
        p if p == pack_ascii(b"est") => Some(offset(-5, 0)),
        p if p == pack_ascii(b"edt") => Some(offset(-4, 0)),
        p if p == pack_ascii(b"mdt") => Some(offset(-6, 0)),
        p if p == pack_ascii(b"mst") => Some(offset(-7, 0)),
        p if p == pack_ascii(b"cst") => Some(offset(-6, 0)),
        p if p == pack_ascii(b"cdt") => Some(offset(-5, 0)),
        p if p == pack_ascii(b"act") => Some(offset(-8, 0)),
        p if p == pack_ascii(b"aft") => Some(offset(4, 30)),
        p if p == pack_ascii(b"amt") => Some(offset(4, 0)),
        p if p == pack_ascii(b"art") => Some(offset(-3, 0)),
        p if p == pack_ascii(b"ast") => Some(offset(3, 0)),
        p if p == pack_ascii(b"azt") => Some(offset(4, 0)),
        p if p == pack_ascii(b"bit") => Some(offset(-12, 0)),
        p if p == pack_ascii(b"bdt") => Some(offset(8, 0)),
        p if p == pack_ascii(b"acst") => Some(offset(9, 30)),
        p if p == pack_ascii(b"aest") => Some(offset(10, 0)),
        p if p == pack_ascii(b"akst") => Some(offset(-9, 0)),
        p if p == pack_ascii(b"amst") => Some(offset(5, 0)),
        p if p == pack_ascii(b"awst") => Some(offset(8, 0)),
        p if p == pack_ascii(b"azost") => Some(offset(-1, 0)),
        p if p == pack_ascii(b"biot") => Some(offset(6, 0)),
        p if p == pack_ascii(b"brt") => Some(offset(-3, 0)),
        p if p == pack_ascii(b"bst") => Some(offset(6, 0)),
        p if p == pack_ascii(b"btt") => Some(offset(6, 0)),
        p if p == pack_ascii(b"cat") => Some(offset(2, 0)),
        p if p == pack_ascii(b"cct") => Some(offset(6, 30)),
        p if p == pack_ascii(b"cet") => Some(offset(1, 0)),
        p if p == pack_ascii(b"cest") => Some(offset(2, 0)),
        p if p == pack_ascii(b"chast") => Some(offset(12, 45)),
        p if p == pack_ascii(b"chst") => Some(offset(10, 0)),
        p if p == pack_ascii(b"cist") => Some(offset(-8, 0)),
        p if p == pack_ascii(b"ckt") => Some(offset(-10, 0)),
        p if p == pack_ascii(b"clt") => Some(offset(-4, 0)),
        p if p == pack_ascii(b"clst") => Some(offset(-3, 0)),
        p if p == pack_ascii(b"cot") => Some(offset(-5, 0)),
        p if p == pack_ascii(b"cost") => Some(offset(-4, 0)),
        p if p == pack_ascii(b"cvt") => Some(offset(-1, 0)),
        p if p == pack_ascii(b"cxt") => Some(offset(7, 0)),
        p if p == pack_ascii(b"east") => Some(offset(-6, 0)),
        p if p == pack_ascii(b"eat") => Some(offset(3, 0)),
        p if p == pack_ascii(b"ect") => Some(offset(-4, 0)),
        p if p == pack_ascii(b"eest") => Some(offset(3, 0)),
        p if p == pack_ascii(b"eet") => Some(offset(2, 0)),
        p if p == pack_ascii(b"fjt") => Some(offset(12, 0)),
        p if p == pack_ascii(b"fkst") => Some(offset(-4, 0)),
        p if p == pack_ascii(b"galt") => Some(offset(-6, 0)),
        p if p == pack_ascii(b"get") => Some(offset(4, 0)),
        p if p == pack_ascii(b"gft") => Some(offset(-3, 0)),
        p if p == pack_ascii(b"gilt") => Some(offset(7, 0)),
        p if p == pack_ascii(b"git") => Some(offset(-9, 0)),
        p if p == pack_ascii(b"gst") => Some(offset(-2, 0)),
        p if p == pack_ascii(b"gyt") => Some(offset(-4, 0)),
        p if p == pack_ascii(b"hast") => Some(offset(-10, 0)),
        p if p == pack_ascii(b"hkt") => Some(offset(8, 0)),
        p if p == pack_ascii(b"hmt") => Some(offset(5, 0)),
        p if p == pack_ascii(b"irkt") => Some(offset(8, 0)),
        p if p == pack_ascii(b"irst") => Some(offset(3, 30)),
        p if p == pack_ascii(b"ist") => Some(offset(2, 0)),
        p if p == pack_ascii(b"jst") => Some(offset(9, 0)),
        p if p == pack_ascii(b"krat") => Some(offset(7, 0)),
        p if p == pack_ascii(b"kst") => Some(offset(9, 0)),
        p if p == pack_ascii(b"lhst") => Some(offset(10, 30)),
        p if p == pack_ascii(b"lint") => Some(offset(14, 0)),
        p if p == pack_ascii(b"magt") => Some(offset(11, 0)),
        p if p == pack_ascii(b"mit") => Some(offset(-9, 30)),
        p if p == pack_ascii(b"msk") => Some(offset(3, 0)),
        p if p == pack_ascii(b"mut") => Some(offset(4, 0)),
        p if p == pack_ascii(b"ndt") => Some(offset(-2, 30)),
        p if p == pack_ascii(b"nft") => Some(offset(11, 30)),
        p if p == pack_ascii(b"npt") => Some(offset(5, 45)),
        p if p == pack_ascii(b"nt") => Some(offset(-3, 30)),
        p if p == pack_ascii(b"omst") => Some(offset(6, 0)),
        p if p == pack_ascii(b"pett") => Some(offset(12, 0)),
        p if p == pack_ascii(b"phot") => Some(offset(13, 0)),
        p if p == pack_ascii(b"pkt") => Some(offset(5, 0)),
        p if p == pack_ascii(b"ret") => Some(offset(4, 0)),
        p if p == pack_ascii(b"samt") => Some(offset(4, 0)),
        p if p == pack_ascii(b"sast") => Some(offset(2, 0)),
        p if p == pack_ascii(b"sbt") => Some(offset(11, 0)),
        p if p == pack_ascii(b"sct") => Some(offset(4, 0)),
        p if p == pack_ascii(b"slt") => Some(offset(5, 30)),
        p if p == pack_ascii(b"sst") => Some(offset(8, 0)),
        p if p == pack_ascii(b"taht") => Some(offset(-10, 0)),
        p if p == pack_ascii(b"tha") => Some(offset(7, 0)),
        p if p == pack_ascii(b"uyt") => Some(offset(-3, 0)),
        p if p == pack_ascii(b"uyst") => Some(offset(-2, 0)),
        p if p == pack_ascii(b"vet") => Some(offset(-4, 30)),
        p if p == pack_ascii(b"vlat") => Some(offset(10, 0)),
        p if p == pack_ascii(b"wat") => Some(offset(1, 0)),
        p if p == pack_ascii(b"wet") => Some(offset(0, 0)),
        p if p == pack_ascii(b"west") => Some(offset(1, 0)),
        p if p == pack_ascii(b"yakt") => Some(offset(9, 0)),
        p if p == pack_ascii(b"yekt") => Some(offset(5, 0)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_w3c() {
        let parsed = parse_date("2020-01-10T14:33:00Z").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2020-01-10T14:33:00+00:00");
    }

    #[test]
    fn test_parse_pubdate() {
        let parsed = parse_date("Fri, 28 May 2010 21:03:38 GMT").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2010-05-28T21:03:38+00:00");
    }
}
