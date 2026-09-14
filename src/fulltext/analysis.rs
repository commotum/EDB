//! Versioned native fulltext analysis and expression matching. The analyzer
//! follows the documented English defaults, not Lucene's binary/token ABI.
use crate::SemanticError;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const ANALYZER_VERSION: u32 = 1;
const STOP: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub term: String,
    pub position: u32,
}

fn normalize(raw: &str) -> String {
    let lower = raw.to_lowercase().replace('’', "'");
    lower.strip_suffix("'s").unwrap_or(&lower).replace('\'', "")
}

/// Positions count discarded stop words too, so phrase matching never silently
/// collapses a gap. Unicode alphanumeric runs are words; accents are preserved,
/// there is no stemming, and lowercasing is Unicode lowercase, not case-folding.
pub(crate) fn analyze(text: &str) -> Vec<Token> {
    let (mut offset, mut position) = (0, 0);
    std::iter::from_fn(|| next_token(text, &mut offset, &mut position)).collect()
}

/// Streaming form for spill-backed index construction: retains at most one
/// analyzed token, not an expanded document's entire token vector.
pub(crate) fn next_token(text: &str, offset: &mut usize, position: &mut u32) -> Option<Token> {
    let word = |c: char| c.is_alphanumeric() || c == '\'' || c == '’';
    while *offset < text.len() {
        let c = text[*offset..].chars().next()?;
        if !word(c) {
            *offset += c.len_utf8();
            continue;
        }
        let start = *offset;
        while let Some(c) = text[*offset..].chars().next() {
            if !word(c) {
                break;
            }
            *offset += c.len_utf8();
        }
        let raw = &text[start..*offset];
        if !raw.chars().any(char::is_alphanumeric) {
            continue;
        }
        let at = *position;
        *position = position.saturating_add(1);
        let term = normalize(raw);
        if !term.is_empty() && !STOP.contains(&term.as_str()) {
            return Some(Token { term, position: at });
        }
    }
    None
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum SearchTerm {
    Exact(String),
    Prefix(String),
}

#[derive(Clone, Debug)]
enum Expr {
    Empty,
    Term(SearchTerm),
    Phrase(Vec<Token>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Not(Box<Self>),
}

impl Expr {
    fn match_weight(&self) -> usize {
        match self {
            Self::Empty | Self::Term(_) => 1,
            Self::Phrase(tokens) => tokens.len().max(1),
            Self::And(a, b) | Self::Or(a, b) => 1usize
                .saturating_add(a.match_weight())
                .saturating_add(b.match_weight()),
            Self::Not(inner) => 1usize.saturating_add(inner.match_weight()),
        }
    }
    fn combine(expressions: Vec<Self>, and: bool) -> Self {
        // Balance flat user input too: explicit group-depth checks alone don't
        // protect evaluation/drop of thousands of adjacent Boolean clauses.
        let mut level: Vec<_> = expressions
            .into_iter()
            .filter(|expr| !matches!(expr, Self::Empty))
            .collect();
        while level.len() > 1 {
            let mut iter = level.into_iter();
            let mut next = Vec::new();
            while let Some(left) = iter.next() {
                next.push(match iter.next() {
                    Some(right) => Self::binary(left, right, and),
                    None => left,
                });
            }
            level = next;
        }
        level.pop().unwrap_or(Self::Empty)
    }
    fn binary(left: Self, right: Self, and: bool) -> Self {
        match (left, right) {
            (Self::Empty, right) => right,
            (left, Self::Empty) => left,
            (left, right) if and => Self::And(Box::new(left), Box::new(right)),
            (left, right) => Self::Or(Box::new(left), Box::new(right)),
        }
    }

    fn needs_universe(&self) -> bool {
        match self {
            Self::Empty | Self::Term(_) | Self::Phrase(_) => false,
            Self::Not(_) => true,
            Self::And(a, b) => a.needs_universe() && b.needs_universe(),
            Self::Or(a, b) => a.needs_universe() || b.needs_universe(),
        }
    }

    fn positive_terms(&self, negative: bool, terms: &mut BTreeSet<SearchTerm>) {
        match self {
            Self::Term(term) if !negative => {
                terms.insert(term.clone());
            }
            Self::Phrase(tokens) if !negative => {
                terms.extend(
                    tokens
                        .iter()
                        .map(|token| SearchTerm::Exact(token.term.clone())),
                );
            }
            Self::And(a, b) | Self::Or(a, b) => {
                a.positive_terms(negative, terms);
                b.positive_terms(negative, terms);
            }
            Self::Not(inner) => inner.positive_terms(!negative, terms),
            _ => {}
        }
    }

    fn matches(&self, words: &BTreeMap<&str, Vec<u32>>) -> bool {
        match self {
            Self::Empty => false,
            Self::Term(SearchTerm::Exact(term)) => words.contains_key(term.as_str()),
            Self::Term(SearchTerm::Prefix(prefix)) => words
                .range(prefix.as_str()..)
                .next()
                .is_some_and(|(word, _)| word.starts_with(prefix)),
            Self::Phrase(tokens) => {
                let Some(first) = tokens.first() else {
                    return false;
                };
                words.get(first.term.as_str()).is_some_and(|starts| {
                    starts.iter().any(|start| {
                        tokens.iter().skip(1).all(|token| {
                            let offset = token.position - first.position;
                            start.checked_add(offset).is_some_and(|position| {
                                words.get(token.term.as_str()).is_some_and(|positions| {
                                    positions.binary_search(&position).is_ok()
                                })
                            })
                        })
                    })
                })
            }
            Self::And(a, b) => a.matches(words) && b.matches(words),
            Self::Or(a, b) => a.matches(words) || b.matches(words),
            Self::Not(inner) => !inner.matches(words),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledSearch {
    expression: Expr,
    pub terms: BTreeSet<SearchTerm>,
}

impl CompiledSearch {
    pub fn match_weight(&self) -> usize {
        self.expression.match_weight()
    }
    /// Query nesting and token budgets are explicit caller resource policy,
    /// not a parser trick to make malformed trailing input disappear.
    pub fn parse(text: &str, max_tokens: usize, max_depth: usize) -> Result<Self, SemanticError> {
        let tokens = lex(text, max_tokens)?;
        if tokens.is_empty() {
            return Ok(Self {
                expression: Expr::Empty,
                terms: BTreeSet::new(),
            });
        }
        let mut parser = Parser {
            tokens: &tokens,
            at: 0,
            max_depth,
        };
        let expression = parser.or(0)?;
        if parser.at != tokens.len() {
            return Err(syntax("unexpected token after expression"));
        }
        if expression.needs_universe() {
            return Err(syntax(
                "every OR branch needs a positive term; use term AND NOT excluded",
            ));
        }
        let mut terms = BTreeSet::new();
        expression.positive_terms(false, &mut terms);
        Ok(Self { expression, terms })
    }

    pub fn matches(&self, tokens: &[Token]) -> bool {
        let mut words = BTreeMap::<&str, Vec<u32>>::new();
        for token in tokens {
            words.entry(&token.term).or_default().push(token.position);
        }
        self.expression.matches(&words)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Lexeme {
    Word(String),
    Phrase(String),
    And,
    Or,
    Not,
    Open,
    Close,
}

fn lex(text: &str, maximum: usize) -> Result<Vec<Lexeme>, SemanticError> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        let token = match c {
            '(' => Lexeme::Open,
            ')' => Lexeme::Close,
            '"' => {
                let mut phrase = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => {
                            phrase.push(chars.next().ok_or_else(|| syntax("unfinished escape"))?)
                        }
                        Some(c) => phrase.push(c),
                        None => return Err(syntax("unterminated quoted phrase")),
                    }
                }
                Lexeme::Phrase(phrase)
            }
            _ => {
                let mut word = String::new();
                let mut escaped = false;
                let mut c = c;
                loop {
                    if c == '\\' {
                        escaped = true;
                        word.push(chars.next().ok_or_else(|| syntax("unfinished escape"))?);
                    } else {
                        word.push(c);
                    }
                    if chars
                        .peek()
                        .is_none_or(|c| c.is_whitespace() || matches!(c, '(' | ')' | '"'))
                    {
                        break;
                    }
                    c = chars.next().unwrap();
                }
                match word.as_str() {
                    "AND" if !escaped => Lexeme::And,
                    "OR" if !escaped => Lexeme::Or,
                    "NOT" if !escaped => Lexeme::Not,
                    _ => Lexeme::Word(word),
                }
            }
        };
        if tokens.len() >= maximum {
            return Err(syntax("query token budget exceeded"));
        }
        tokens.push(token);
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [Lexeme],
    at: usize,
    max_depth: usize,
}

impl Parser<'_> {
    fn or(&mut self, depth: usize) -> Result<Expr, SemanticError> {
        let mut expressions = vec![self.and(depth)?];
        while let Some(token) = self.tokens.get(self.at) {
            if matches!(token, Lexeme::Close) {
                break;
            }
            if matches!(token, Lexeme::Or) {
                self.at += 1;
            } else if matches!(token, Lexeme::And) {
                return Err(syntax("unexpected AND"));
            }
            // Adjacent expressions use documented native default OR.
            expressions.push(self.and(depth)?);
        }
        Ok(Expr::combine(expressions, false))
    }

    fn and(&mut self, depth: usize) -> Result<Expr, SemanticError> {
        let mut expressions = vec![self.atom(depth)?];
        while self.tokens.get(self.at) == Some(&Lexeme::And) {
            self.at += 1;
            expressions.push(self.atom(depth)?);
        }
        Ok(Expr::combine(expressions, true))
    }

    fn atom(&mut self, depth: usize) -> Result<Expr, SemanticError> {
        if depth >= self.max_depth {
            return Err(syntax("query nesting budget exceeded"));
        }
        let token = self
            .tokens
            .get(self.at)
            .ok_or_else(|| syntax("missing query operand"))?;
        self.at += 1;
        match token {
            Lexeme::Not => Ok(match self.atom(depth + 1)? {
                Expr::Empty => Expr::Empty,
                Expr::Not(inner) => *inner,
                inner => Expr::Not(Box::new(inner)),
            }),
            Lexeme::Open => {
                let inside = self.or(depth + 1)?;
                if self.tokens.get(self.at) != Some(&Lexeme::Close) {
                    return Err(syntax("unclosed group"));
                }
                self.at += 1;
                Ok(inside)
            }
            Lexeme::Phrase(text) => {
                let tokens = analyze(text);
                if tokens.is_empty() {
                    Ok(Expr::Empty)
                } else {
                    Ok(Expr::Phrase(tokens))
                }
            }
            Lexeme::Word(text) => {
                if let Some(prefix) = text.strip_suffix('*') {
                    let term = normalize(prefix);
                    if term.is_empty() || !term.chars().all(char::is_alphanumeric) {
                        return Err(syntax("wildcards require a nonempty word prefix"));
                    }
                    return Ok(Expr::Term(SearchTerm::Prefix(term)));
                }
                if text
                    .chars()
                    .any(|c| matches!(c, '*' | '?' | ':' | '~' | '^' | '[' | ']' | '{' | '}'))
                {
                    return Err(syntax(
                        "unsupported search syntax; use terms, phrases, Boolean groups or trailing prefix *",
                    ));
                }
                Ok(Expr::combine(
                    analyze(text)
                        .into_iter()
                        .map(|token| Expr::Term(SearchTerm::Exact(token.term)))
                        .collect(),
                    false,
                ))
            }
            _ => Err(syntax("expected a term, phrase or group")),
        }
    }
}

pub(crate) fn bm25(
    tf: u32,
    doc_len: u32,
    documents: u64,
    total_length: u64,
    frequency: u64,
) -> f64 {
    if tf == 0 || documents == 0 || frequency == 0 {
        return 0.0;
    }
    let average = (total_length as f64 / documents as f64).max(1.0);
    let idf =
        (1.0 + (documents.saturating_sub(frequency) as f64 + 0.5) / (frequency as f64 + 0.5)).ln();
    let tf = f64::from(tf);
    idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * f64::from(doc_len) / average))
}

fn syntax(message: &str) -> SemanticError {
    SemanticError::incorrect("fulltext/query-syntax", message)
}
