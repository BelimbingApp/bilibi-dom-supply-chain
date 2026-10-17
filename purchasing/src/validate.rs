//! Input rules in Ecto's words, as Bilimbi's changesets report them: a blank
//! required value, a value over its length, an invalid format. Each field
//! reports its first failing rule, so a page shows one message beside it.
use crate::FieldError;
use rust_decimal::Decimal;

pub(crate) const BLANK: &str = "can't be blank";
pub(crate) const TAKEN: &str = "has already been taken";
pub(crate) const INVALID: &str = "is invalid";
pub(crate) const INVALID_FORMAT: &str = "has invalid format";

type Check<T> = std::result::Result<T, FieldError>;

fn too_long(field: &'static str, value: &str, max: usize) -> Check<()> {
    if value.chars().count() > max {
        Err(FieldError::new(
            field,
            format!("should be at most {max} character(s)"),
        ))
    } else {
        Ok(())
    }
}

/// A required text: trimmed, not blank, at most `max` codepoints.
pub(crate) fn required_text(field: &'static str, value: &str, max: usize) -> Check<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(FieldError::new(field, BLANK));
    }
    too_long(field, value, max)?;
    Ok(value.to_owned())
}

/// An optional text: trimmed, blank becomes `None`, at most `max` codepoints.
pub(crate) fn optional_text(field: &'static str, value: &str, max: usize) -> Check<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    too_long(field, value, max)?;
    Ok(Some(value.to_owned()))
}

/// Bilimbi's email rule, `^[^\s@]+@[^\s@]+$`: one `@` with something on each
/// side and no whitespace.
pub(crate) fn email(field: &'static str, value: &str, max: usize) -> Check<Option<String>> {
    let Some(value) = optional_text(field, value, max)? else {
        return Ok(None);
    };
    let well_formed = match value.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && !domain.contains('@')
                && !value.chars().any(char::is_whitespace)
        }
        None => false,
    };
    if well_formed {
        Ok(Some(value))
    } else {
        Err(FieldError::new(field, INVALID_FORMAT))
    }
}

/// An ISO 3166-1 alpha-2 code, upper-cased as Bilimbi stores it.
pub(crate) fn country_iso(field: &'static str, value: &str) -> Check<Option<String>> {
    let value = value.trim().to_ascii_uppercase();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() == 2 && value.bytes().all(|c| c.is_ascii_uppercase()) {
        Ok(Some(value))
    } else {
        Err(FieldError::new(field, INVALID_FORMAT))
    }
}

/// A decimal with at most `places` decimal places, refused rather than
/// rounded when finer: Ecto's `:decimal` cast and the source's
/// `validate_scale`. `None` is a blank value, which the caller judges.
pub(crate) fn decimal(field: &'static str, value: &str, places: u32) -> Check<Option<Decimal>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let number = Decimal::from_str_exact(value).map_err(|_| FieldError::new(field, INVALID))?;
    if number.round_dp(places) != number {
        return Err(FieldError::new(
            field,
            format!("has more than {places} decimal places"),
        ));
    }
    Ok(Some(number))
}

/// A line quantity in a `numeric(20, 4)` column: required, more than zero,
/// under 10^16 and at most four decimal places.
pub(crate) fn quantity(field: &'static str, value: &str) -> Check<Decimal> {
    let quantity = decimal(field, value, 4)?.ok_or_else(|| FieldError::new(field, BLANK))?;
    if quantity <= Decimal::ZERO {
        return Err(FieldError::new(field, "must be greater than 0"));
    }
    if quantity >= Decimal::from(10_000_000_000_000_000_i64) {
        return Err(FieldError::new(field, "must be less than 1E+16"));
    }
    Ok(quantity)
}

/// An optional ISO 8601 calendar date (`YYYY-MM-DD`, Ecto's `:date` cast),
/// returned in that form; a date that does not exist is invalid.
pub(crate) fn date(field: &'static str, value: &str) -> Check<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let invalid = || FieldError::new(field, INVALID);
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(invalid());
    }
    let number = |range: std::ops::Range<usize>| -> Check<u32> {
        let part = &value[range];
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().map_err(|_| invalid())
        } else {
            Err(invalid())
        }
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(invalid()),
    };
    if year == 0 || day == 0 || day > days {
        return Err(invalid());
    }
    Ok(Some(value.to_owned()))
}

/// A decision or cancellation reason: trimmed, at most 500 codepoints.
/// Required for a rejection or cancellation, optional for an approval.
pub(crate) fn reason(value: &str, required: bool) -> Check<Option<String>> {
    if required {
        required_text("reason", value, 500).map(Some)
    } else {
        optional_text("reason", value, 500)
    }
}

/// Collects one error per failing field and hands back a default in its place,
/// so every field is checked before the refusal is reported.
#[derive(Default)]
pub(crate) struct Checks {
    errors: Vec<FieldError>,
}
impl Checks {
    pub(crate) fn take<T: Default>(&mut self, check: Check<T>) -> T {
        match check {
            Ok(value) => value,
            Err(error) => {
                self.errors.push(error);
                T::default()
            }
        }
    }
    /// Records the errors of one line, numbered from 1, under that line.
    pub(crate) fn take_line<T: Default>(&mut self, line: usize, check: Check<T>) -> T {
        self.take(check.map_err(|error| FieldError {
            line: Some(line),
            ..error
        }))
    }
    pub(crate) fn finish(self) -> std::result::Result<(), Vec<FieldError>> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors)
        }
    }
}
