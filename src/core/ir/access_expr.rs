use std::sync::PoisonError;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_while1};
use nom::character::complete::{char, multispace0};
use nom::combinator::{map, value};
use nom::multi::separated_list1;
use nom::sequence::{delimited, preceded};
use nom::{IResult, Parser};

use super::{EvalContext, ResolverContextLike};
use crate::core::path::PathString;

#[derive(Clone, Debug, PartialEq)]
pub enum AccessExpr {
    And(Box<AccessExpr>, Box<AccessExpr>),
    Or(Box<AccessExpr>, Box<AccessExpr>),
    Not(Box<AccessExpr>),
    Eq(Operand, Operand),
    Neq(Operand, Operand),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Path(Vec<String>),
    StringLiteral(String),
    NumberLiteral(i64),
    BoolLiteral(bool),
}

impl AccessExpr {
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn parse(input: &str) -> Result<Self, String> {
        let (remaining, expr) = parse_or_expr(input.trim())
            .map_err(|e| format!("Failed to parse access expression: {e}"))?;
        let remaining = remaining.trim();
        if !remaining.is_empty() {
            return Err(format!("Unexpected trailing input: '{remaining}'"));
        }
        Ok(expr)
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn evaluate<Ctx: ResolverContextLike + Sync>(
        &self,
        ctx: &EvalContext<'_, Ctx>,
    ) -> Result<bool, String> {
        match self {
            AccessExpr::And(left, right) => Ok(left.evaluate(ctx)? && right.evaluate(ctx)?),
            AccessExpr::Or(left, right) => Ok(left.evaluate(ctx)? || right.evaluate(ctx)?),
            AccessExpr::Not(expr) => Ok(!expr.evaluate(ctx)?),
            AccessExpr::Eq(left, right) => {
                let l = resolve_operand(left, ctx);
                let r = resolve_operand(right, ctx);
                Ok(l == r)
            }
            AccessExpr::Neq(left, right) => {
                let l = resolve_operand(left, ctx);
                let r = resolve_operand(right, ctx);
                Ok(l != r)
            }
        }
    }
}

fn resolve_operand<Ctx: ResolverContextLike + Sync>(
    operand: &Operand,
    ctx: &EvalContext<'_, Ctx>,
) -> Option<serde_json::Value> {
    match operand {
        Operand::Path(segments) => {
            // For claims paths, resolve directly from serde_json::Value to
            // preserve types
            if segments.first().map(std::string::String::as_str) == Some("claims")
                && segments.len() > 1
            {
                let guard = ctx
                    .request_ctx
                    .auth_claims
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let claims = guard.as_ref()?;
                let mut current = claims;
                for segment in &segments[1..] {
                    current = current.get(segment.as_str())?;
                }
                Some(current.clone())
            } else {
                ctx.path_string(segments)
                    .map(|s| serde_json::Value::String(s.into_owned()))
            }
        }
        Operand::StringLiteral(s) => Some(serde_json::Value::String(s.clone())),
        Operand::NumberLiteral(n) => Some(serde_json::json!(*n)),
        Operand::BoolLiteral(b) => Some(serde_json::Value::Bool(*b)),
    }
}

// Parser functions using nom

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn parse_path(input: &str) -> IResult<&str, Operand> {
    map(
        separated_list1(char('.'), take_while1(is_ident_char)),
        |parts: Vec<&str>| Operand::Path(parts.into_iter().map(String::from).collect()),
    )
    .parse(input)
}

fn parse_string_literal(input: &str) -> IResult<&str, Operand> {
    let (input, _) = char('\'').parse(input)?;
    let bytes = input.as_bytes();
    let mut result = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'\'' => result.push('\''),
                b'\\' => result.push('\\'),
                other => {
                    result.push('\\');
                    result.push(other as char);
                }
            }
            i += 2;
        } else if bytes[i] == b'\'' {
            return Ok((&input[i + 1..], Operand::StringLiteral(result)));
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Char,
    )))
}

fn parse_number_literal(input: &str) -> IResult<&str, Operand> {
    let (input, neg) = nom::combinator::opt(char('-')).parse(input)?;
    let (input, digits) = take_while1(|c: char| c.is_ascii_digit()).parse(input)?;
    let n: i64 = digits.parse().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;
    let n = if neg.is_some() { -n } else { n };
    Ok((input, Operand::NumberLiteral(n)))
}

fn parse_bool_literal(input: &str) -> IResult<&str, Operand> {
    let (remaining, op) = alt((
        value(Operand::BoolLiteral(true), tag("true")),
        value(Operand::BoolLiteral(false), tag("false")),
    ))
    .parse(input)?;
    // Ensure the keyword is not a prefix of a longer identifier
    if remaining
        .chars()
        .next()
        .is_some_and(|c| is_ident_char(c) || c == '.')
    {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Tag,
        )));
    }
    Ok((remaining, op))
}

fn parse_operand(input: &str) -> IResult<&str, Operand> {
    let (input, _) = multispace0(input)?;

    alt((
        parse_string_literal,
        parse_bool_literal,
        parse_number_literal,
        parse_path,
    ))
    .parse(input)
}

fn parse_comparison(input: &str) -> IResult<&str, AccessExpr> {
    let (input, left) = parse_operand(input)?;
    let (input, _) = multispace0(input)?;
    let (input, op) = alt((tag("!="), tag("=="))).parse(input)?;
    let (input, _) = multispace0(input)?;
    let (input, right) = parse_operand(input)?;
    let (input, _) = multispace0(input)?;
    match op {
        "==" => Ok((input, AccessExpr::Eq(left, right))),
        "!=" => Ok((input, AccessExpr::Neq(left, right))),
        _ => unreachable!(),
    }
}

fn parse_not(input: &str) -> IResult<&str, AccessExpr> {
    let (input, _) = multispace0(input)?;
    let (input, _) = char('!').parse(input)?;
    let (input, _) = multispace0(input)?;
    let (input, expr) = parse_atom(input)?;
    Ok((input, AccessExpr::Not(Box::new(expr))))
}

fn parse_parens(input: &str) -> IResult<&str, AccessExpr> {
    delimited(
        preceded(multispace0, char('(')),
        parse_or_expr,
        preceded(multispace0, char(')')),
    )
    .parse(input)
}

fn parse_atom(input: &str) -> IResult<&str, AccessExpr> {
    let (input, _) = multispace0(input)?;
    alt((parse_parens, parse_not, parse_comparison)).parse(input)
}

fn parse_and_expr(input: &str) -> IResult<&str, AccessExpr> {
    let (input, first) = parse_atom(input)?;
    let (input, rest) =
        nom::multi::many0(preceded((multispace0, tag("&&"), multispace0), parse_atom))
            .parse(input)?;
    Ok((
        input,
        rest.into_iter().fold(first, |acc, expr| {
            AccessExpr::And(Box::new(acc), Box::new(expr))
        }),
    ))
}

fn parse_or_expr(input: &str) -> IResult<&str, AccessExpr> {
    let (input, first) = parse_and_expr(input)?;
    let (input, rest) = nom::multi::many0(preceded(
        (multispace0, tag("||"), multispace0),
        parse_and_expr,
    ))
    .parse(input)?;
    Ok((
        input,
        rest.into_iter().fold(first, |acc, expr| {
            AccessExpr::Or(Box::new(acc), Box::new(expr))
        }),
    ))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    #[test]
    fn test_parse_simple_eq() {
        let expr = AccessExpr::parse("claims.role == 'admin'").unwrap();
        assert_eq!(
            expr,
            AccessExpr::Eq(
                Operand::Path(vec!["claims".into(), "role".into()]),
                Operand::StringLiteral("admin".into()),
            )
        );
    }

    #[test]
    fn test_parse_neq() {
        let expr = AccessExpr::parse("claims.role != 'guest'").unwrap();
        assert_eq!(
            expr,
            AccessExpr::Neq(
                Operand::Path(vec!["claims".into(), "role".into()]),
                Operand::StringLiteral("guest".into()),
            )
        );
    }

    #[test]
    fn test_parse_and() {
        let expr =
            AccessExpr::parse("claims.role == 'admin' && claims.sub == args.userId").unwrap();
        match expr {
            AccessExpr::And(left, right) => {
                assert_eq!(
                    *left,
                    AccessExpr::Eq(
                        Operand::Path(vec!["claims".into(), "role".into()]),
                        Operand::StringLiteral("admin".into()),
                    )
                );
                assert_eq!(
                    *right,
                    AccessExpr::Eq(
                        Operand::Path(vec!["claims".into(), "sub".into()]),
                        Operand::Path(vec!["args".into(), "userId".into()]),
                    )
                );
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_parse_or() {
        let expr =
            AccessExpr::parse("claims.role == 'admin' || claims.role == 'moderator'").unwrap();
        match expr {
            AccessExpr::Or(_, _) => {}
            _ => panic!("Expected Or"),
        }
    }

    #[test]
    fn test_parse_not() {
        let expr = AccessExpr::parse("!(claims.role == 'guest')").unwrap();
        match expr {
            AccessExpr::Not(inner) => {
                assert_eq!(
                    *inner,
                    AccessExpr::Eq(
                        Operand::Path(vec!["claims".into(), "role".into()]),
                        Operand::StringLiteral("guest".into()),
                    )
                );
            }
            _ => panic!("Expected Not"),
        }
    }

    #[test]
    fn test_parse_parens() {
        let expr = AccessExpr::parse(
            "(claims.role == 'admin' || claims.role == 'mod') && claims.active == true",
        )
        .unwrap();
        match expr {
            AccessExpr::And(left, right) => {
                assert!(matches!(*left, AccessExpr::Or(_, _)));
                assert_eq!(
                    *right,
                    AccessExpr::Eq(
                        Operand::Path(vec!["claims".into(), "active".into()]),
                        Operand::BoolLiteral(true),
                    )
                );
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_parse_number() {
        let expr = AccessExpr::parse("claims.level == 42").unwrap();
        assert_eq!(
            expr,
            AccessExpr::Eq(
                Operand::Path(vec!["claims".into(), "level".into()]),
                Operand::NumberLiteral(42),
            )
        );
    }

    #[test]
    fn test_parse_error() {
        assert!(AccessExpr::parse("").is_err());
        assert!(AccessExpr::parse("==").is_err());
        assert!(AccessExpr::parse("claims.role ==").is_err());
    }

    #[test]
    fn test_parse_trailing_input() {
        assert!(AccessExpr::parse("claims.role == 'admin' garbage").is_err());
    }

    #[test]
    fn test_evaluate_eq_literals() {
        use crate::core::http::RequestContext;
        use crate::core::ir::EmptyResolverContext;

        let runtime = crate::core::runtime::test::init(&None);
        let req_ctx = RequestContext::new(runtime);
        req_ctx.set_auth_claims(serde_json::json!({
            "sub": "user123",
            "role": "admin",
        }));

        let res_ctx = EmptyResolverContext {};
        let eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        let expr = AccessExpr::parse("claims.role == 'admin'").unwrap();
        assert!(expr.evaluate(&eval_ctx).unwrap());

        let expr = AccessExpr::parse("claims.role == 'user'").unwrap();
        assert!(!expr.evaluate(&eval_ctx).unwrap());

        let expr = AccessExpr::parse("claims.role != 'user'").unwrap();
        assert!(expr.evaluate(&eval_ctx).unwrap());
    }

    #[test]
    fn test_evaluate_missing_claims() {
        use crate::core::http::RequestContext;
        use crate::core::ir::EmptyResolverContext;

        let runtime = crate::core::runtime::test::init(&None);
        let req_ctx = RequestContext::new(runtime);
        // No claims set

        let res_ctx = EmptyResolverContext {};
        let eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        let expr = AccessExpr::parse("claims.role == 'admin'").unwrap();
        // Missing claims path resolves to None, 'admin' resolves to Some - not
        // equal
        assert!(!expr.evaluate(&eval_ctx).unwrap());
    }

    #[test]
    fn test_evaluate_and_or() {
        use crate::core::http::RequestContext;
        use crate::core::ir::EmptyResolverContext;

        let runtime = crate::core::runtime::test::init(&None);
        let req_ctx = RequestContext::new(runtime);
        req_ctx.set_auth_claims(serde_json::json!({
            "role": "admin",
            "active": true,
        }));

        let res_ctx = EmptyResolverContext {};
        let eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        let expr = AccessExpr::parse("claims.role == 'admin' && claims.active == true").unwrap();
        assert!(expr.evaluate(&eval_ctx).unwrap());

        let expr = AccessExpr::parse("claims.role == 'user' || claims.role == 'admin'").unwrap();
        assert!(expr.evaluate(&eval_ctx).unwrap());

        let expr = AccessExpr::parse("claims.role == 'user' && claims.active == true").unwrap();
        assert!(!expr.evaluate(&eval_ctx).unwrap());
    }

    #[test]
    fn test_parse_bool_boundary() {
        // "trueValue" should parse as a path, not bool literal "true" +
        // leftover "Value"
        let expr = AccessExpr::parse("claims.trueValue == 'yes'").unwrap();
        assert_eq!(
            expr,
            AccessExpr::Eq(
                Operand::Path(vec!["claims".into(), "trueValue".into()]),
                Operand::StringLiteral("yes".into()),
            )
        );

        let expr = AccessExpr::parse("claims.falsePositive == 'no'").unwrap();
        assert_eq!(
            expr,
            AccessExpr::Eq(
                Operand::Path(vec!["claims".into(), "falsePositive".into()]),
                Operand::StringLiteral("no".into()),
            )
        );
    }

    #[test]
    fn test_evaluate_type_aware_comparison() {
        use crate::core::http::RequestContext;
        use crate::core::ir::EmptyResolverContext;

        let runtime = crate::core::runtime::test::init(&None);
        let req_ctx = RequestContext::new(runtime);
        req_ctx.set_auth_claims(serde_json::json!({
            "level": 42,
            "active": true,
            "role": "admin",
        }));

        let res_ctx = EmptyResolverContext {};
        let eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        // Integer claim vs integer literal
        let expr = AccessExpr::parse("claims.level == 42").unwrap();
        assert!(expr.evaluate(&eval_ctx).unwrap());

        // Boolean claim vs boolean literal
        let expr = AccessExpr::parse("claims.active == true").unwrap();
        assert!(expr.evaluate(&eval_ctx).unwrap());

        // Boolean claim vs string literal should NOT match
        let expr = AccessExpr::parse("claims.active == 'true'").unwrap();
        assert!(!expr.evaluate(&eval_ctx).unwrap());

        // Integer claim vs string literal should NOT match
        let expr = AccessExpr::parse("claims.level == '42'").unwrap();
        assert!(!expr.evaluate(&eval_ctx).unwrap());
    }
}
