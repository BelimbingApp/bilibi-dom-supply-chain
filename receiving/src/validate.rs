//! Bilimbi Receipt/ReceiptLine/Setting changeset rules, private to Receiving.
use crate::{FieldError, NewReceiptLine, ReceiptLine, Refusal};
use bilibi_supply_chain_purchasing::Order;
use rust_decimal::Decimal;
use std::collections::HashSet;

type Check<T> = std::result::Result<T, FieldError>;

pub(crate) fn text(
    field: &'static str,
    value: &str,
    max: usize,
    required: bool,
) -> Check<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        return if required {
            Err(FieldError::new(field, "can't be blank"))
        } else {
            Ok(None)
        };
    }
    if value.chars().count() > max {
        return Err(FieldError::new(
            field,
            format!("should be at most {max} character(s)"),
        ));
    }
    Ok(Some(value.into()))
}

pub(crate) fn number(field: &'static str, value: &str, default_zero: bool) -> Check<Decimal> {
    let value = value.trim();
    let n = if value.is_empty() && default_zero {
        Decimal::ZERO
    } else {
        if value.is_empty() {
            return Err(FieldError::new(field, "can't be blank"));
        }
        // Ecto accepts exponent notation, but not Rust's digit separators.
        // Check the mantissa exactly before from_scientific, whose base
        // parser otherwise rounds excess precision before applying the exponent.
        let invalid = || FieldError::new(field, "is invalid");
        if value.contains('_') {
            return Err(invalid());
        }
        let mantissa = value.split(['e', 'E']).next().ok_or_else(invalid)?;
        let exact = Decimal::from_str_exact(mantissa).map_err(|_| invalid())?;
        if mantissa.len() == value.len() {
            exact
        } else {
            let exponent = &value[mantissa.len() + 1..];
            let digits = exponent
                .strip_prefix('+')
                .or_else(|| exponent.strip_prefix('-'))
                .unwrap_or(exponent);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            Decimal::from_scientific(value).map_err(|_| invalid())?
        }
    };
    if n < Decimal::ZERO {
        return Err(FieldError::new(field, "must be greater than or equal to 0"));
    }
    if n >= Decimal::from(10_000_000_000_000_000_i64) {
        return Err(FieldError::new(field, "must be less than 1E+16"));
    }
    if n.round_dp(4) != n {
        return Err(FieldError::new(field, "has more than 4 decimal places"));
    }
    Ok(n)
}

pub(crate) fn tolerance(value: &str) -> Check<Decimal> {
    let n = number("over_receipt_tolerance_percent", value, false)?;
    if n > Decimal::ONE_HUNDRED {
        Err(FieldError::new(
            "over_receipt_tolerance_percent",
            "must be less than or equal to 100",
        ))
    } else {
        Ok(n)
    }
}

/// Validate bytes before slicing: non-ASCII input must refuse, not panic.
pub(crate) fn date(value: &str) -> Check<String> {
    let value = value.trim();
    let invalid = || FieldError::new("received_on", "is invalid");
    if value.is_empty() {
        return Err(FieldError::new("received_on", "can't be blank"));
    }
    let b = value.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || b.iter()
            .enumerate()
            .any(|(i, c)| i != 4 && i != 7 && !c.is_ascii_digit())
    {
        return Err(invalid());
    }
    let y: u32 = value[..4].parse().map_err(|_| invalid())?;
    let m: u32 = value[5..7].parse().map_err(|_| invalid())?;
    let d: u32 = value[8..].parse().map_err(|_| invalid())?;
    let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if y == 0 || d == 0 || d > days {
        Err(invalid())
    } else {
        Ok(value.into())
    }
}

#[derive(Default)]
pub(crate) struct Checks(Vec<FieldError>);
impl Checks {
    pub(crate) fn take<T: Default>(&mut self, check: Check<T>) -> T {
        match check {
            Ok(v) => v,
            Err(e) => {
                self.0.push(e);
                T::default()
            }
        }
    }
    fn line<T: Default>(&mut self, index: usize, check: Check<T>) -> T {
        self.take(check.map_err(|mut e| {
            e.line = Some(index);
            e
        }))
    }
    pub(crate) fn finish(self) -> std::result::Result<(), Refusal> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Refusal::Invalid(self.0))
        }
    }
}

pub(crate) fn lines(
    order: &Order,
    inputs: &[NewReceiptLine],
    tolerance: Decimal,
) -> std::result::Result<Vec<ReceiptLine>, Refusal> {
    if inputs.is_empty() {
        return Err(Refusal::NoLines);
    }
    let mut checks = Checks::default();
    let mut seen = HashSet::new();
    let mut valid = Vec::new();
    for (i, input) in inputs.iter().enumerate() {
        let line_number = i + 1;
        if !seen.insert(input.order_line_id) {
            checks.take(Err::<(), _>(FieldError::new(
                "lines",
                "names each order line once",
            )));
        }
        let Some(line) = order.lines.iter().find(|l| l.id == input.order_line_id) else {
            checks.line(
                line_number,
                Err::<(), _>(FieldError::new(
                    "order_line_id",
                    "is not a current line of this order",
                )),
            );
            continue;
        };
        let received = checks.line(
            line_number,
            number("received_quantity", &input.received_quantity, true),
        );
        let rejected = checks.line(
            line_number,
            number("rejected_quantity", &input.rejected_quantity, true),
        );
        let rejection_reason = checks.line(
            line_number,
            text("rejection_reason", &input.rejection_reason, 500, false),
        );
        if received + rejected <= Decimal::ZERO {
            checks.line(
                line_number,
                Err::<(), _>(FieldError::new(
                    "received_quantity",
                    "or the rejected quantity must be greater than 0",
                )),
            );
        }
        if rejected > Decimal::ZERO && rejection_reason.is_none() {
            checks.line(
                line_number,
                Err::<(), _>(FieldError::new(
                    "rejection_reason",
                    "can't be blank when goods are rejected",
                )),
            );
        } else if rejected == Decimal::ZERO && rejection_reason.is_some() {
            checks.line(
                line_number,
                Err::<(), _>(FieldError::new(
                    "rejection_reason",
                    "applies only to rejected goods",
                )),
            );
        }
        let ceiling = line.quantity * (Decimal::ONE_HUNDRED + tolerance) / Decimal::ONE_HUNDRED;
        if line.received_quantity + received > ceiling {
            let message = if tolerance == Decimal::ZERO {
                "exceeds what the order line has left to receive"
            } else {
                "exceeds what the order line has left to receive, beyond the tolerance"
            };
            checks.line(
                line_number,
                Err::<(), _>(FieldError::new("received_quantity", message)),
            );
        }
        valid.push(ReceiptLine {
            id: 0,
            line_number: i32::try_from(line_number).unwrap_or(i32::MAX),
            order_line_id: line.id,
            order_line_number: line.line_number,
            description: line.description.clone(),
            unit: line.unit.clone(),
            ordered_quantity: line.quantity,
            received_quantity: received,
            rejected_quantity: rejected,
            rejection_reason,
        });
    }
    checks.finish()?;
    Ok(valid)
}
