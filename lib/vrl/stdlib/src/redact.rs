use lazy_static::lazy_static;
use std::borrow::Cow;
use std::convert::{TryFrom, TryInto};
use std::str::FromStr;
use vrl::prelude::*;

lazy_static! {
    // Matches Visa, Mastercard, American Express, Diner's Club, Discover Card, and JCB
    static ref CREDIT_CARD_REGEX: regex::Regex = regex::Regex::new(r"(?:4[0-9]{12}(?:[0-9]{3})?|[25][1-7][0-9]{14}|6(?:011|5[0-9][0-9])[0-9]{12}|3[47][0-9]{13}|3(?:0[0-5]|[68][0-9])[0-9]{11}|(?:2131|1800|35\d{3})\d{11})").unwrap();
}

#[derive(Clone, Copy, Debug)]
pub struct Redact;

impl Function for Redact {
    fn identifier(&self) -> &'static str {
        "redact"
    }

    fn parameters(&self) -> &'static [Parameter] {
        &[
            Parameter {
                keyword: "value",
                kind: kind::BYTES | kind::OBJECT | kind::ARRAY,
                required: true,
            },
            Parameter {
                keyword: "filters",
                kind: kind::ARRAY,
                required: true,
            },
        ]
    }

    fn examples(&self) -> &'static [Example] {
        // TODO
        &[]
    }

    fn compile(&self, mut arguments: ArgumentList) -> Compiled {
        let value = arguments.required("value");

        let filters = arguments
            .required_array("filters")?
            .into_iter()
            .map(|value| value.try_into().map_err(Into::into))
            .collect::<Result<Vec<Filter>>>()
            .map_err(|err| {
                dbg!(err);
                vrl::function::Error::UnexpectedExpression {
                    keyword: "TODO",
                    expected: "TODO",
                    expr: expression::Expr::Literal(expression::Literal::String("TODO".into())),
                }
            })?;

        let redactor = Redactor::Full;

        Ok(Box::new(RedactFn {
            value,
            filters,
            redactor,
        }))
    }
}

//-----------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct RedactFn {
    value: Box<dyn Expression>,
    filters: Vec<Filter>,
    redactor: Redactor,
}

fn redact(value: Value, filters: &Vec<Filter>, redactor: &Redactor) -> Value {
    match value {
        Value::Bytes(bytes) => {
            let input = String::from_utf8_lossy(&bytes);
            let output = filters
                .iter()
                .fold(input, |input, filter| filter.redact(input, redactor));
            Value::Bytes(output.into_owned().into())
        }
        Value::Array(values) => {
            let values = values
                .into_iter()
                .map(|value| redact(value, filters, redactor))
                .collect();
            Value::Array(values)
        }
        Value::Object(map) => {
            let map = map
                .into_iter()
                .map(|(key, value)| (key, redact(value, filters, redactor)))
                .collect();
            Value::Object(map)
        }
        _ => value,
    }
}

impl Expression for RedactFn {
    fn resolve(&self, ctx: &mut Context) -> Resolved {
        let value = self.value.resolve(ctx)?;

        Ok(redact(value, &self.filters, &self.redactor))
    }

    fn type_def(&self, state: &state::Compiler) -> TypeDef {
        self.value.type_def(state).infallible()
    }
}

//-----------------------------------------------------------------------------

/// The redaction filter to apply to the given value.
#[derive(Debug, Clone)]
enum Filter {
    Pattern(Vec<Pattern>),
    CreditCard,
}

#[derive(Debug, Clone)]
enum Pattern {
    Regex(regex::Regex),
    String(String),
}

impl TryFrom<expression::Expr> for Filter {
    type Error = &'static str;

    fn try_from(value: expression::Expr) -> std::result::Result<Self, Self::Error> {
        match value {
            expression::Expr::Container(expression::Container {
                variant: expression::Variant::Object(object),
            }) => {
                let r#type = match object
                    .get("type")
                    .ok_or("filters specified as objects must have type paramater")?
                {
                    expression::Expr::Literal(expression::Literal::String(bytes)) => {
                        Ok(bytes.clone())
                    }
                    _ => Err("type key in filters must be a literal string"),
                }?;

                match r#type.as_ref() {
                    b"credit_card" => Ok(Filter::CreditCard),
                    b"pattern" => {
                        let patterns = match object
                            .get("patterns")
                            .ok_or("pattern filter must have `patterns` specified")?
                        {
                            expression::Expr::Container(expression::Container {
                                variant: expression::Variant::Array(array),
                            }) => Ok(array
                                .iter()
                                .map(|expr| match expr {
                                    expression::Expr::Literal(expression::Literal::Regex(
                                        regex,
                                    )) => Ok(Pattern::Regex((**regex).clone())),
                                    expression::Expr::Literal(expression::Literal::String(
                                        bytes,
                                    )) => Ok(Pattern::String(
                                        String::from_utf8_lossy(&bytes).into_owned(),
                                    )),
                                    _ => Err("`patterns` must be regular expressions"),
                                })
                                .collect::<std::result::Result<Vec<_>, _>>()?),
                            _ => Err("`patterns` must be array of regular expression literals"),
                        }?;
                        Ok(Filter::Pattern(patterns))
                    }
                    _ => Err("unknown filter name"),
                }
            }
            expression::Expr::Literal(literal) => match literal {
                expression::Literal::String(bytes) => match bytes.as_ref() {
                    b"pattern" => Err("pattern cannot be used without arguments"),
                    b"credit_card" => Ok(Filter::CreditCard),
                    _ => Err("unknown filter name"),
                },
                expression::Literal::Regex(regex) => {
                    Ok(Filter::Pattern(vec![Pattern::Regex((*regex).clone())]))
                }
                _ => Err("unknown literal for filter, must be a regex, filter name, or object"),
            },
            _ => Err("unknown literal for filter, must be a regex, filter name, or object"),
        }
    }
}

impl Filter {
    fn redact<'t>(&self, input: Cow<'t, str>, redactor: &Redactor) -> Cow<'t, str> {
        match &self {
            Filter::Pattern(patterns) => patterns.iter().fold(input, |input, pattern| {
                // TODO see if we can avoid cloning here
                match pattern {
                    Pattern::Regex(regex) => regex
                        .replace_all(&input, redactor.pattern())
                        .into_owned()
                        .into(),
                    Pattern::String(pattern) => {
                        input.to_owned().replace(pattern, redactor.pattern()).into()
                    }
                }
            }),
            Filter::CreditCard => CREDIT_CARD_REGEX
                .replace_all(&input, redactor.pattern())
                .into_owned()
                .into(),
        }
    }
}

/// The recipe for redacting the matched filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redactor {
    Full,
}

impl Redactor {
    fn pattern(&self) -> &str {
        use Redactor::*;

        match self {
            Full => "[REDACTED]",
        }
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Redactor::Full
    }
}

impl FromStr for Redactor {
    type Err = &'static str;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        use Redactor::*;

        match s {
            "full" => Ok(Full),
            _ => Err("unknown redactor"),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use regex::Regex;

    // TODO test error cases
    test_function![
        redact => Redact;

        regex {
             args: func_args![
                 value: "hello 123456 world",
                 filters: vec![Regex::new(r"\d+").unwrap()],
             ],
             want: Ok("hello [REDACTED] world"),
             tdef: TypeDef::new().infallible().bytes(),
        }

        patterns {
             args: func_args![
                 value: "hello 123456 world",
                 filters: vec![
                     value!({
                         "type": "pattern",
                         "patterns": ["123456"]
                     })
                 ],
             ],
             want: Ok("hello [REDACTED] world"),
             tdef: TypeDef::new().infallible().bytes(),
        }

        credit_card {
             args: func_args![
                 value: "hello 4916155524184782 world",
                 filters: vec!["credit_card"],
             ],
             want: Ok("hello [REDACTED] world"),
             tdef: TypeDef::new().infallible().bytes(),
        }
    ];
}
