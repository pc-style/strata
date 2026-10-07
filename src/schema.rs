use crate::{ApiError, Result};
use lucene_rs::analysis::Analyzer;
use lucene_rs::{
    BooleanQuery, Document, Field, MissingValue, PhraseQuery, Query, Sort, SortField, SortOrder,
    StandardAnalyzer, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Bound;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Keyword,
    Long,
    Double,
    Date,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    #[serde(rename = "type")]
    pub kind: FieldKind,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mappings {
    pub properties: BTreeMap<String, Mapping>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateIndex {
    pub mappings: Mappings,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    #[serde(default = "all")]
    pub query: Value,
    #[serde(default)]
    pub sort: Vec<SortSpec>,
    #[serde(default = "page_size")]
    pub size: usize,
    pub search_after: Option<String>,
}
fn all() -> Value {
    serde_json::json!({"match_all":{}})
}
const fn page_size() -> usize {
    10
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortSpec {
    pub field: String,
    #[serde(default)]
    pub order: Direction,
    #[serde(default)]
    pub missing: Missing,
}
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Asc,
    Desc,
}
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Missing {
    First,
    #[default]
    Last,
}

pub fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        || value.starts_with('_')
    {
        return Err(ApiError::bad(
            "names must be 1–64 lowercase letters, digits, underscores or hyphens, not starting with underscore",
        ));
    }
    Ok(())
}

impl Mappings {
    pub fn validate(&self) -> Result<()> {
        if self.properties.is_empty() || self.properties.len() > 256 {
            return Err(ApiError::bad("mappings require 1–256 fields"));
        }
        for field in self.properties.keys() {
            name(field)?;
        }
        Ok(())
    }

    fn kind(&self, field: &str) -> Result<FieldKind> {
        self.properties
            .get(field)
            .map(|m| m.kind)
            .ok_or_else(|| ApiError::bad(format!("unmapped field: {field}")))
    }

    pub fn document(&self, id: &str, source: &Value) -> Result<Document> {
        if id.is_empty() || id.len() > 512 {
            return Err(ApiError::bad("document ID must be 1–512 bytes"));
        }
        let fields = source
            .as_object()
            .ok_or_else(|| ApiError::bad("document must be an object"))?;
        let mut doc = Document::new()
            .with(Field::string("_id", id, Store::Yes))
            .with(Field::stored("_source", source.to_string()));
        for (field, value) in fields {
            let kind = self.kind(field)?;
            if value.is_null() {
                continue;
            }
            let f = match kind {
                FieldKind::Text => Field::text(field, text(value)?, Store::No),
                FieldKind::Keyword => Field::sorted_keyword(field, text(value)?, Store::No),
                FieldKind::Long | FieldKind::Date => {
                    Field::numeric_i64(field, integer(value)?, Store::No)
                }
                FieldKind::Double => Field::numeric_f64(field, double(value)?, Store::No),
            };
            doc.add(f);
        }
        Ok(doc)
    }

    pub fn sort(&self, specs: &[SortSpec]) -> Result<Sort> {
        if specs.is_empty() {
            return Ok(Sort::Score);
        }
        if specs.len() > 8 {
            return Err(ApiError::bad("at most eight sort fields"));
        }
        let fields = specs
            .iter()
            .map(|s| {
                if self.kind(&s.field)? == FieldKind::Text {
                    return Err(ApiError::bad("sort requires keyword or numeric/date field"));
                }
                Ok(SortField {
                    field: s.field.clone(),
                    order: match s.order {
                        Direction::Asc => SortOrder::Asc,
                        Direction::Desc => SortOrder::Desc,
                    },
                    missing: match s.missing {
                        Missing::First => MissingValue::First,
                        Missing::Last => MissingValue::Last,
                    },
                })
            })
            .collect::<Result<_>>()?;
        Ok(Sort::Fields(fields))
    }

    pub fn query(&self, value: &Value, depth: usize) -> Result<Query> {
        if depth > 24 {
            return Err(ApiError::bad("query nesting exceeds 24"));
        }
        let (kind, args) = single(value)?;
        match kind {
            "match_all" if args.as_object().is_some_and(serde_json::Map::is_empty) => {
                Ok(Query::MatchAll)
            }
            "bool" => self.boolean(args, depth.saturating_add(1)),
            "term" | "match" | "match_phrase" => {
                let (field, value) = single(args)?;
                let mapping = self.kind(field)?;
                let value = text(value)?;
                if kind == "term" {
                    if !matches!(mapping, FieldKind::Text | FieldKind::Keyword) {
                        return Err(ApiError::bad(
                            "term requires text or keyword; use range for numbers",
                        ));
                    }
                    return Ok(Query::term(field, value));
                }
                if mapping != FieldKind::Text {
                    return Err(ApiError::bad("match queries require a text field"));
                }
                let mut terms = Vec::new();
                StandardAnalyzer::new()
                    .analyze(field, value, &mut |term, _| terms.push(term.to_string()));
                if terms.len() > 256 {
                    return Err(ApiError::bad("too many query terms"));
                }
                if kind == "match_phrase" {
                    let mut phrase = PhraseQuery::new(field);
                    for term in terms {
                        phrase = phrase.term(term);
                    }
                    Ok(phrase.build())
                } else {
                    let mut boolean = BooleanQuery::new();
                    for term in terms {
                        boolean = boolean.should(Query::term(field, term));
                    }
                    Ok(boolean.build())
                }
            }
            "range" => {
                let (field, value) = single(args)?;
                let bounds: RangeSpec = serde_json::from_value(value.clone())
                    .map_err(|e| ApiError::bad(e.to_string()))?;
                let (lower, upper) = bounds.bounds()?;
                match self.kind(field)? {
                    FieldKind::Long | FieldKind::Date => Ok(Query::I64Range {
                        field: field.into(),
                        lower: convert_bound(lower, integer)?,
                        upper: convert_bound(upper, integer)?,
                    }),
                    FieldKind::Double => Ok(Query::F64Range {
                        field: field.into(),
                        lower: convert_bound(lower, double)?,
                        upper: convert_bound(upper, double)?,
                    }),
                    _ => Err(ApiError::bad("range requires numeric/date field")),
                }
            }
            _ => Err(ApiError::bad(format!("unsupported query: {kind}"))),
        }
    }

    fn boolean(&self, args: &Value, depth: usize) -> Result<Query> {
        let fields = args
            .as_object()
            .ok_or_else(|| ApiError::bad("bool must be an object"))?;
        let mut query = BooleanQuery::new();
        let mut count = 0usize;
        for (kind, value) in fields {
            if kind == "minimum_should_match" {
                let n = value
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| ApiError::bad("minimum_should_match must be an integer"))?;
                query = query.minimum_should_match(n);
                continue;
            }
            let clauses = value
                .as_array()
                .ok_or_else(|| ApiError::bad("bool clauses must be arrays"))?;
            count = count.saturating_add(clauses.len());
            if count > 256 {
                return Err(ApiError::bad("too many bool clauses"));
            }
            for clause in clauses {
                let child = self.query(clause, depth)?;
                query = match kind.as_str() {
                    "must" => query.must(child),
                    "should" => query.should(child),
                    "filter" => query.filter(child),
                    "must_not" => query.must_not(child),
                    _ => return Err(ApiError::bad("unsupported bool clause")),
                };
            }
            if !matches!(kind.as_str(), "must" | "should" | "filter" | "must_not") {
                return Err(ApiError::bad("unsupported bool clause"));
            }
        }
        Ok(query.build())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RangeSpec {
    gt: Option<Value>,
    gte: Option<Value>,
    lt: Option<Value>,
    lte: Option<Value>,
}
impl RangeSpec {
    fn bounds(&self) -> Result<(Bound<&Value>, Bound<&Value>)> {
        if self.gt.is_some() && self.gte.is_some() || self.lt.is_some() && self.lte.is_some() {
            return Err(ApiError::bad("choose one lower and one upper bound"));
        }
        Ok((
            self.gt.as_ref().map_or_else(
                || self.gte.as_ref().map_or(Bound::Unbounded, Bound::Included),
                Bound::Excluded,
            ),
            self.lt.as_ref().map_or_else(
                || self.lte.as_ref().map_or(Bound::Unbounded, Bound::Included),
                Bound::Excluded,
            ),
        ))
    }
}
fn convert_bound<T>(bound: Bound<&Value>, parse: fn(&Value) -> Result<T>) -> Result<Bound<T>> {
    Ok(match bound {
        Bound::Unbounded => Bound::Unbounded,
        Bound::Included(v) => Bound::Included(parse(v)?),
        Bound::Excluded(v) => Bound::Excluded(parse(v)?),
    })
}
fn single(value: &Value) -> Result<(&str, &Value)> {
    let object = value
        .as_object()
        .filter(|v| v.len() == 1)
        .ok_or_else(|| ApiError::bad("expected exactly one key"))?;
    object
        .iter()
        .next()
        .map(|(k, v)| (k.as_str(), v))
        .ok_or_else(|| ApiError::bad("missing key"))
}
fn text(value: &Value) -> Result<&str> {
    value
        .as_str()
        .ok_or_else(|| ApiError::bad("expected string"))
}
fn integer(value: &Value) -> Result<i64> {
    value.as_i64().ok_or_else(|| {
        ApiError::bad("expected signed 64-bit integer (dates use epoch milliseconds)")
    })
}
fn double(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| ApiError::bad("expected finite number"))
}
