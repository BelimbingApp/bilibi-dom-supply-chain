//! Input rules in Ecto's words, as Bilimbi's changesets report them: a blank
//! required value, a value over its length, an invalid format. Each field
//! reports its first failing rule, so a page shows one message beside it.
use crate::FieldError;

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
    pub(crate) fn finish(self) -> std::result::Result<(), Vec<FieldError>> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors)
        }
    }
}
