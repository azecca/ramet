//! Human-readable byte counts, and sizes typed by the user.

const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];

/// One mebibyte: sizes typed by the user are rounded down to it, a multiple
/// of every sector size btrfs uses.
const MIB: u64 = 1 << 20;

/// The units a size may be given in, as `truncate` and `btrfs filesystem
/// resize` read them: binary, `G` meaning a GiB.
const SIZE_UNITS: [(char, u32); 3] = [('M', 20), ('G', 30), ('T', 40)];

/// Formats a byte count with binary units: `512 B`, `1.5 GiB`; `?` when unknown.
pub fn human_bytes(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return "?".to_owned();
    };
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    // Precision loss only affects digits far below the one printed.
    #[allow(clippy::cast_precision_loss)]
    let mut size = bytes as f64 / 1024.0;
    let mut unit = UNITS[0];
    for next in &UNITS[1..] {
        if size < 1024.0 {
            break;
        }
        size /= 1024.0;
        unit = next;
    }
    format!("{size:.1} {unit}")
}

/// Parses a size such as `20G`, `1.5T` or `512M`.
///
/// Units are binary, as for `truncate` (`G` is a GiB), in either case, and
/// may be followed by `iB` or `B`. A unit is required: a bare `20` is more
/// likely a forgotten `G` than 20 bytes. The result is rounded down to the MiB.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let invalid =
        || format!("{text:?} is not a size: give a number and a unit, as in 20G (M, G or T)");
    let text = text.trim();
    let split = text
        .find(|c: char| c.is_ascii_alphabetic())
        .ok_or_else(invalid)?;
    let (number, unit) = text.split_at(split);
    let unit = unit.to_ascii_uppercase();
    let unit = unit
        .strip_suffix("IB")
        .or_else(|| unit.strip_suffix('B'))
        .unwrap_or(&unit);
    let shift = SIZE_UNITS
        .iter()
        .find(|(letter, _)| unit.len() == 1 && unit.starts_with(*letter))
        .map(|(_, shift)| *shift)
        .ok_or_else(invalid)?;
    let number = number.trim();
    let (whole, fraction) = number.split_once('.').unwrap_or((number, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) || fraction.len() > 6 {
        return Err(invalid());
    }
    let scale = 10u128.pow(u32::try_from(fraction.len()).map_err(|_| invalid())?);
    let mantissa: u128 = format!("{whole}{fraction}")
        .parse()
        .map_err(|_| invalid())?;
    let bytes = (mantissa << shift) / scale;
    let bytes = u64::try_from(bytes).map_err(|_| invalid())? / MIB * MIB;
    if bytes == 0 {
        return Err(format!("{text:?} is less than 1 MiB"));
    }
    Ok(bytes)
}

/// `bytes` as `truncate -s` and `btrfs filesystem resize` read it: `20G`,
/// or `1536M`, or a bare byte count.
pub fn size_argument(bytes: u64) -> String {
    SIZE_UNITS
        .iter()
        .rev()
        .find(|(_, shift)| bytes.is_multiple_of(1 << shift))
        .map_or_else(
            || bytes.to_string(),
            |(letter, shift)| format!("{}{letter}", bytes >> shift),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes_with_binary_units() {
        assert_eq!(parse_size("20G"), Ok(20 << 30));
        assert_eq!(parse_size("20g"), Ok(20 << 30));
        assert_eq!(parse_size("20GiB"), Ok(20 << 30));
        assert_eq!(parse_size("20 GB"), Ok(20 << 30));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("1.5T"), Ok(3 << 39));
        assert_eq!(parse_size("1.5G"), Ok(1536 << 20));
    }

    #[test]
    fn rounds_sizes_down_to_the_mebibyte() {
        assert_eq!(parse_size("1.000001G"), Ok(1 << 30));
        assert_eq!(
            parse_size("0.3M").unwrap_err(),
            "\"0.3M\" is less than 1 MiB"
        );
    }

    #[test]
    fn refuses_what_is_not_a_size() {
        for text in [
            "20", "G", "20K", "20X", "-1G", "1.G", ".5G", "1..5G", "20Gb2", "",
        ] {
            assert!(parse_size(text).is_err(), "{text}");
        }
        assert!(parse_size("99999999999T").is_err(), "overflow");
    }

    #[test]
    fn writes_sizes_for_the_command_line() {
        assert_eq!(size_argument(20 << 30), "20G");
        assert_eq!(size_argument(2 << 40), "2T");
        assert_eq!(size_argument(1536 << 20), "1536M");
        assert_eq!(size_argument(4097), "4097");
        for size in ["20G", "1536M", "3T"] {
            assert_eq!(size_argument(parse_size(size).unwrap()), size);
        }
    }

    #[test]
    fn formats_each_magnitude() {
        assert_eq!(human_bytes(None), "?");
        assert_eq!(human_bytes(Some(0)), "0 B");
        assert_eq!(human_bytes(Some(1023)), "1023 B");
        assert_eq!(human_bytes(Some(1024)), "1.0 KiB");
        assert_eq!(human_bytes(Some(1536 * 1024)), "1.5 MiB");
        assert_eq!(human_bytes(Some(1 << 30)), "1.0 GiB");
        assert_eq!(human_bytes(Some(100 << 30)), "100.0 GiB");
        assert_eq!(human_bytes(Some(3 << 40)), "3.0 TiB");
        assert_eq!(human_bytes(Some(2048 << 40)), "2048.0 TiB");
    }
}
