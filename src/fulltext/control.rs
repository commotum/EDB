//! Shared cumulative work/allocation admission and cooperative interruption.
use super::FulltextOptions;
use super::analysis::{CompiledSearch, Token, analyze};
use crate::{ErrorCategory, SemanticError};
use std::sync::atomic::Ordering;
use std::time::Instant;

pub(super) struct Budget<'a> {
    pub(super) options: &'a FulltextOptions,
    pub(super) deadline: Option<Instant>,
    pub(super) work: usize,
    pub(super) bytes: usize,
    pub(super) read_bytes: u64,
    pub(super) external: &'a mut dyn FnMut(usize, usize) -> Result<(), SemanticError>,
}
impl Budget<'_> {
    pub(super) fn charge(&mut self, work: usize, bytes: usize) -> Result<(), SemanticError> {
        self.work = self.work.saturating_add(work);
        self.bytes = self.bytes.saturating_add(bytes);
        (self.external)(work, bytes)?;
        if self.options.cancel.load(Ordering::Relaxed) {
            return Err(resource("fulltext/cancelled", "search cancelled"));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(resource("fulltext/deadline", "search deadline exceeded"));
        }
        if self.work > self.options.max_work || self.bytes > self.options.max_bytes {
            return Err(resource(
                "fulltext/capacity",
                "search exceeded its configured work or byte allowance",
            ));
        }
        Ok(())
    }
    pub(super) fn tokens(&mut self, text: &str) -> Result<Vec<Token>, SemanticError> {
        // An alphanumeric token needs at least one byte and a separator between
        // successive tokens. This conservative preallocation bound charges
        // before the analyzer grows its owned strings/vector, including errors.
        self.charge(
            text.len(),
            text.len().saturating_mul(std::mem::size_of::<Token>() + 1),
        )?;
        Ok(analyze(text))
    }
    pub(super) fn matches(
        &mut self,
        query: &CompiledSearch,
        tokens: &[Token],
    ) -> Result<bool, SemanticError> {
        // Account for the word/position map and the query's expression/phrase
        // work, rather than treating an arbitrarily long phrase as one step.
        self.charge(
            tokens.len().saturating_mul(
                query
                    .match_weight()
                    .saturating_add(query.terms.len())
                    .saturating_add(1),
            ),
            tokens.len().saturating_mul(128),
        )?;
        let matched = query.matches(tokens);
        self.charge(0, 0)?;
        Ok(matched)
    }
}
pub(super) fn resource(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::new(ErrorCategory::Busy, code, message)
}
