//! Phone numbers in logs: mask everything but the country code and the last 2 to 4 digits.

/// Masks an E.164 number for logs and display: `+19195550102` becomes `+1 ••• ••• 0102`.
///
/// Keeps the country code and the last 4 digits when the national number has at least 8 digits,
/// 3 when it has 6 or 7, and 2 otherwise. The masked middle is always `••• •••`, so the output
/// does not reveal the number's length. Input that is not `+` and digits keeps only its last
/// 2 digits. For US numbers the output matches the contract's `phone_masked` format.
pub fn mask_phone(phone: &str) -> String {
    let trimmed = phone.trim();
    let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
    let e164 = trimmed.strip_prefix('+').is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || c == ' ' || c == '-')
    });
    if !e164 || digits.len() < 4 {
        let keep = digits.len().min(2);
        return format!("••• •••{}", &digits[digits.len() - keep..]);
    }
    let cc_len = country_code_len(&digits).min(digits.len() - 2);
    let national = digits.len() - cc_len;
    let keep = match national {
        8.. => 4,
        6..=7 => 3,
        _ => 2,
    };
    format!(
        "+{} ••• ••• {}",
        &digits[..cc_len],
        &digits[digits.len() - keep..]
    )
}

/// Length of the ITU country code at the start of `digits` (1 to 3).
fn country_code_len(digits: &str) -> usize {
    let b = digits.as_bytes();
    match b[0] {
        b'1' | b'7' => 1,
        _ => {
            let two = &digits[..2];
            let two_digit = matches!(
                two,
                "20" | "27"
                    | "30"
                    | "31"
                    | "32"
                    | "33"
                    | "34"
                    | "36"
                    | "39"
                    | "40"
                    | "41"
                    | "43"
                    | "44"
                    | "45"
                    | "46"
                    | "47"
                    | "48"
                    | "49"
                    | "51"
                    | "52"
                    | "53"
                    | "54"
                    | "55"
                    | "56"
                    | "57"
                    | "58"
                    | "60"
                    | "61"
                    | "62"
                    | "63"
                    | "64"
                    | "65"
                    | "66"
                    | "81"
                    | "82"
                    | "84"
                    | "86"
                    | "90"
                    | "91"
                    | "92"
                    | "93"
                    | "94"
                    | "95"
                    | "98"
            );
            if two_digit { 2 } else { 3 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_number_matches_contract_format() {
        assert_eq!(mask_phone("+19195550102"), "+1 ••• ••• 0102");
    }

    #[test]
    fn country_codes() {
        assert_eq!(mask_phone("+447911123456"), "+44 ••• ••• 3456");
        assert_eq!(mask_phone("+35312345678"), "+353 ••• ••• 5678");
        assert_eq!(mask_phone("+79161234567"), "+7 ••• ••• 4567");
    }

    #[test]
    fn short_numbers_keep_fewer_digits() {
        // Iceland: 7-digit national numbers.
        assert_eq!(mask_phone("+3545512345"), "+354 ••• ••• 345");
        assert_eq!(mask_phone("+2991234"), "+299 ••• ••• 34");
    }

    #[test]
    fn junk_input_keeps_two_digits_at_most() {
        assert_eq!(mask_phone("9195550102"), "••• •••02");
        assert_eq!(mask_phone("+1"), "••• •••1");
        assert_eq!(mask_phone(""), "••• •••");
        assert_eq!(mask_phone("hello"), "••• •••");
    }

    #[test]
    fn never_reveals_the_middle() {
        let masked = mask_phone("+19195550102");
        assert!(!masked.contains("919"));
        assert!(!masked.contains("555"));
    }
}
