use crate::{
    parser_error::{LuaParseError, LuaParseErrorKind},
    LuaSyntaxToken,
};

pub fn float_token_value(token: &LuaSyntaxToken) -> Result<f64, LuaParseError> {
    let text = token.text();
    let hex = text.starts_with("0x") || text.starts_with("0X");

    // This section handles the parsing of hexadecimal floating-point numbers.
    // Hexadecimal floating-point literals are of the form 0x1.8p3, where:
    // - "0x1.8" is the significand (integer and fractional parts in hexadecimal)
    // - "p3" is the exponent (in decimal, base 2 exponent)
    let value = if hex {
        let hex_float_text = &text[2..];
        let exponent_position = hex_float_text
            .find('p')
            .or_else(|| hex_float_text.find('P'));
        let (float_part, exponent_part) = if let Some(pos) = exponent_position {
            (&hex_float_text[..pos], &hex_float_text[(pos + 1)..])
        } else {
            (hex_float_text, "")
        };

        let (integer_part, fraction_value) = if let Some(dot_pos) = float_part.find('.') {
            let (int_part, frac_part) = float_part.split_at(dot_pos);
            let int_value = if !int_part.is_empty() {
                i64::from_str_radix(int_part, 16).unwrap_or(0)
            } else {
                0
            };
            let frac_part = &frac_part[1..];
            let frac_value = if !frac_part.is_empty() {
                let frac_part_value = i64::from_str_radix(frac_part, 16).unwrap_or(0);
                frac_part_value as f64 * 16f64.powi(-(frac_part.len() as i32))
            } else {
                0.0
            };
            (int_value, frac_value)
        } else {
            (i64::from_str_radix(float_part, 16).unwrap_or(0), 0.0)
        };

        let mut value = integer_part as f64 + fraction_value;
        if !exponent_part.is_empty() {
            if let Ok(exp) = exponent_part.parse::<i32>() {
                value *= 2f64.powi(exp);
            }
        }
        value
    } else {
        let (float_part, exponent_part) =
            if let Some(pos) = text.find('e').or_else(|| text.find('E')) {
                (&text[..pos], &text[(pos + 1)..])
            } else {
                (text, "")
            };

        let mut value = float_part.parse::<f64>().map_err(|e| {
            LuaParseError::new(
                LuaParseErrorKind::SyntaxError,
                &t!(
                    "The float literal '%{text}' is invalid, %{err}",
                    text = text,
                    err = e
                ),
                token.text_range(),
            )
        })?;

        if !exponent_part.is_empty() {
            if let Ok(exp) = exponent_part.parse::<i32>() {
                value *= 10f64.powi(exp);
            }
        }
        value
    };

    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegerRepr {
    Normal,
    Hex,
    Bin,
}

pub fn int_token_value(token: &LuaSyntaxToken) -> Result<i64, LuaParseError> {
    let text = token.text();
    let repr = if text.starts_with("0x") || text.starts_with("0X") {
        IntegerRepr::Hex
    } else if text.starts_with("0b") || text.starts_with("0B") {
        IntegerRepr::Bin
    } else {
        IntegerRepr::Normal
    };

    let text = text.trim_end_matches(['u', 'l', 'U', 'L']);

    let value = match repr {
        IntegerRepr::Hex => {
            let text = &text[2..];
            i64::from_str_radix(text, 16)
        }
        IntegerRepr::Bin => {
            let text = &text[2..];
            i64::from_str_radix(text, 2)
        }
        IntegerRepr::Normal => text.parse::<i64>(),
    };
    match value {
        Ok(value) => Ok(value),
        Err(e) => {
            let range = token.text_range();
            if *e.kind() == std::num::IntErrorKind::PosOverflow
                || *e.kind() == std::num::IntErrorKind::NegOverflow
            {
                Err(LuaParseError::new(
                    LuaParseErrorKind::SyntaxError,
                    &t!(
                        "The integer literal '%{text}' is too large to be represented in type 'long'",
                        text = text
                    ),
                    range,
                ))
            } else {
                Err(LuaParseError::new(
                    LuaParseErrorKind::SyntaxError,
                    &t!(
                        "The integer literal '%{text}' is invalid, %{err}",
                        text = text,
                        err = e
                    ),
                    range,
                ))
            }
        }
    }
}
