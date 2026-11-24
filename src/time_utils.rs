use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetSystemTime;
use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

pub fn now_utc_rfc3339() -> String {
    let value = unsafe { GetSystemTime() };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        value.wYear,
        value.wMonth,
        value.wDay,
        value.wHour,
        value.wMinute,
        value.wSecond,
        value.wMilliseconds
    )
}

pub fn display_local_time(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "暂无".to_string();
    };

    let Some(utc) = parse_utc_system_time(value) else {
        return value.to_string();
    };
    let mut local = SYSTEMTIME::default();
    if unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.is_err() {
        return value.to_string();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
    )
}

fn parse_utc_system_time(value: &str) -> Option<SYSTEMTIME> {
    let bytes = value.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }

    Some(SYSTEMTIME {
        wYear: parse_decimal(&bytes[0..4])?,
        wMonth: parse_decimal(&bytes[5..7])?,
        wDay: parse_decimal(&bytes[8..10])?,
        wHour: parse_decimal(&bytes[11..13])?,
        wMinute: parse_decimal(&bytes[14..16])?,
        wSecond: parse_decimal(&bytes[17..19])?,
        ..Default::default()
    })
}

fn parse_decimal(value: &[u8]) -> Option<u16> {
    value.iter().try_fold(0u16, |result, byte| {
        byte.is_ascii_digit()
            .then(|| result * 10 + u16::from(byte - b'0'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_unicode_timestamp_does_not_panic() {
        assert_eq!(
            display_local_time(Some("校园网不是时间戳字符串")),
            "校园网不是时间戳字符串"
        );
    }

    #[test]
    fn formats_valid_utc_rfc3339_and_parses_back() {
        let timestamp = now_utc_rfc3339();
        assert_eq!(timestamp.len(), 24);
        assert!(timestamp.ends_with('Z'));
        assert_eq!(&timestamp[4..5], "-");
        assert_eq!(&timestamp[7..8], "-");
        assert_eq!(&timestamp[10..11], "T");
        assert_eq!(&timestamp[13..14], ":");
        assert_eq!(&timestamp[16..17], ":");
        assert_eq!(&timestamp[19..20], ".");

        let parsed = parse_utc_system_time(&timestamp).expect("generated timestamp must parse");
        assert!(parsed.wYear >= 2020);
        assert!((1..=12).contains(&parsed.wMonth));
        assert!((1..=31).contains(&parsed.wDay));

        assert_eq!(display_local_time(None), "暂无");
        let displayed = display_local_time(Some("2026-01-15T08:30:45.123Z"));
        assert_eq!(displayed.len(), 19);
        assert_eq!(&displayed[4..5], "-");
        assert_eq!(&displayed[7..8], "-");
        assert_eq!(&displayed[10..11], " ");
        assert_eq!(&displayed[13..14], ":");
        assert_eq!(&displayed[16..17], ":");
    }
}
