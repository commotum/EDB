use super::*;

/// Sequential reader over a UTF-8 EDN stream. The caller retains the text;
/// parsed values are yielded individually. Limits cover the entire stream,
/// including comments/discards, and the iterator is fused after an error.
pub struct EdnReader<'a> {
    input: &'a str,
    position: usize,
    options: EdnReadOptions,
    budget: Budget,
    finished: bool,
}

impl<'a> EdnReader<'a> {
    pub fn new(input: &'a str) -> Result<Self, SemanticError> {
        Self::with_options(input, &EdnReadOptions::default())
    }

    pub fn with_options(input: &'a str, options: &EdnReadOptions) -> Result<Self, SemanticError> {
        if input.len() > options.limits.max_input_bytes {
            return Err(capacity());
        }
        Ok(Self {
            input,
            position: 0,
            options: options.clone(),
            budget: Budget::new(options.limits),
            finished: false,
        })
    }

    pub fn byte_offset(&self) -> usize {
        self.position
    }

    fn located(&self, error: SemanticError) -> SemanticError {
        let prefix = &self.input[..self.position];
        let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        error
            .detail("byte_offset", self.position.to_string())
            .detail("line", line.to_string())
            .detail("column", column.to_string())
    }

    fn syntax(&self, message: &str) -> SemanticError {
        SemanticError::incorrect("edn/syntax", message)
    }

    fn peek(&self) -> Option<char> {
        self.input[self.position..].chars().next()
    }

    fn bump(&mut self) -> Result<Option<char>, SemanticError> {
        let value = self.peek();
        if let Some(c) = value {
            self.budget.charge(c.len_utf8())?;
            self.position += c.len_utf8();
        }
        Ok(value)
    }

    fn whitespace(&mut self) -> Result<(), SemanticError> {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() || c == ',' => {
                    self.bump()?;
                }
                Some(';') => {
                    while self.peek().is_some_and(|c| c != '\n' && c != '\r') {
                        self.bump()?;
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn next_value(
        &mut self,
        depth: usize,
        discarded: bool,
        end: Option<char>,
    ) -> Result<Option<EdnValue>, SemanticError> {
        loop {
            self.whitespace()?;
            if self.peek().is_none() || self.peek() == end {
                return Ok(None);
            }
            if self.input[self.position..].starts_with("#_") {
                self.budget.node(depth)?;
                self.bump()?;
                self.bump()?;
                let value = self.required(depth + 1, true)?;
                let checked = identity(&value, &mut self.budget, depth + 1, false, true);
                drop_iterative(value);
                checked?;
                continue;
            }
            return self.value(depth, discarded).map(Some);
        }
    }

    fn required(&mut self, depth: usize, discarded: bool) -> Result<EdnValue, SemanticError> {
        self.next_value(depth, discarded, None)?
            .ok_or_else(|| self.syntax("expected an EDN element"))
    }

    fn value(&mut self, depth: usize, discarded: bool) -> Result<EdnValue, SemanticError> {
        self.budget.node(depth)?;
        match self
            .bump()?
            .ok_or_else(|| self.syntax("expected an EDN element"))?
        {
            '"' => self.string().map(EdnValue::String),
            '\\' => self.character().map(EdnValue::Char),
            '[' => self.collection(depth, discarded, ']').map(EdnValue::Vector),
            '(' => self.collection(depth, discarded, ')').map(EdnValue::List),
            '{' => {
                let values = self.collection(depth, discarded, '}')?;
                if values.len() % 2 != 0 {
                    return Err(self.syntax("map requires key/value pairs"));
                }
                let mut values = values.into_iter();
                let mut entries = Vec::new();
                while let Some(key) = values.next() {
                    entries.push((key, values.next().expect("even map length")));
                }
                Ok(EdnValue::Map(entries))
            }
            '#' => {
                if self.peek() == Some('{') {
                    self.bump()?;
                    return self.collection(depth, discarded, '}').map(EdnValue::Set);
                }
                let text = self.token()?;
                let tag = symbol(text)?;
                validate_symbol(&tag, true)?;
                let value = self.required(depth + 1, discarded)?;
                if !discarded {
                    if let Some(handler) = self.options.tag_handlers.get(&tag) {
                        let value = handler(value)?;
                        if let Err(error) = identity(&value, &mut self.budget, depth, true, false) {
                            drop_iterative(value);
                            return Err(error);
                        }
                        return Ok(value);
                    }
                    let builtin =
                        tag.namespace.is_none() && matches!(tag.name.as_str(), "inst" | "uuid");
                    if !builtin && self.options.unknown_tags == EdnUnknownTagPolicy::Reject {
                        return Err(SemanticError::new(
                            ErrorCategory::Unsupported,
                            "edn/unknown-tag",
                            "EDN tag has no registered handler",
                        ));
                    }
                }
                Ok(EdnValue::Tagged(tag, Box::new(value)))
            }
            ')' | ']' | '}' => Err(self.syntax("unexpected closing delimiter")),
            '\'' | '`' | '~' | '^' | '@' => Err(self.syntax("Clojure reader macros are not EDN")),
            first => {
                self.position -= first.len_utf8();
                let text = self.token()?;
                match text {
                    "nil" => Ok(EdnValue::Nil),
                    "true" => Ok(EdnValue::Bool(true)),
                    "false" => Ok(EdnValue::Bool(false)),
                    _ if text.starts_with(':') => {
                        let parsed = symbol(&text[1..])?;
                        if !valid_name(parsed.namespace.as_deref(), &parsed.name, true) {
                            return Err(SemanticError::incorrect(
                                "edn/keyword",
                                "invalid EDN keyword",
                            ));
                        }
                        Ok(EdnValue::Keyword(Keyword {
                            namespace: parsed.namespace,
                            name: parsed.name,
                        }))
                    }
                    _ if starts_number(text) => number(text, &mut self.budget),
                    _ => {
                        let parsed = symbol(text)?;
                        validate_symbol(&parsed, false)?;
                        Ok(EdnValue::Symbol(parsed))
                    }
                }
            }
        }
    }

    fn collection(
        &mut self,
        depth: usize,
        discarded: bool,
        end: char,
    ) -> Result<Vec<EdnValue>, SemanticError> {
        let mut values = Vec::new();
        while let Some(value) = self.next_value(depth + 1, discarded, Some(end))? {
            values.push(value);
        }
        if self.bump()? != Some(end) {
            return Err(self.syntax("unterminated collection"));
        }
        Ok(values)
    }

    fn token(&mut self) -> Result<&'a str, SemanticError> {
        let start = self.position;
        while self.peek().is_some_and(|c| !delimiter(c)) {
            if self.position - start >= self.budget.limits.max_token_bytes {
                return Err(capacity());
            }
            self.bump()?;
        }
        let token = &self.input[start..self.position];
        self.budget.token(token.len())?;
        if token.is_empty() {
            return Err(self.syntax("expected a token"));
        }
        Ok(token)
    }

    fn hex_quad(&mut self) -> Result<u16, SemanticError> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = self
                .bump()?
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| self.syntax("invalid Unicode escape"))?;
            value = value * 16 + digit as u16;
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<String, SemanticError> {
        let mut text = String::new();
        loop {
            let c = match self
                .bump()?
                .ok_or_else(|| self.syntax("unterminated string"))?
            {
                '"' => {
                    self.budget.token(text.len())?;
                    return Ok(text);
                }
                '\\' => match self
                    .bump()?
                    .ok_or_else(|| self.syntax("unterminated string escape"))?
                {
                    '"' => '"',
                    '\\' => '\\',
                    't' => '\t',
                    'r' => '\r',
                    'n' => '\n',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'u' => {
                        let first = self.hex_quad()?;
                        let scalar = if (0xD800..=0xDBFF).contains(&first) {
                            if self.bump()? != Some('\\') || self.bump()? != Some('u') {
                                return Err(
                                    self.syntax("high surrogate requires a low surrogate escape")
                                );
                            }
                            let second = self.hex_quad()?;
                            if !(0xDC00..=0xDFFF).contains(&second) {
                                return Err(self.syntax("invalid low surrogate"));
                            }
                            0x10000 + (u32::from(first) - 0xD800) * 1024 + u32::from(second)
                                - 0xDC00
                        } else {
                            u32::from(first)
                        };
                        char::from_u32(scalar)
                            .ok_or_else(|| self.syntax("invalid Unicode scalar"))?
                    }
                    _ => return Err(self.syntax("unsupported string escape")),
                },
                c => c,
            };
            if text.len() + c.len_utf8() > self.budget.limits.max_token_bytes {
                return Err(capacity());
            }
            text.push(c);
        }
    }

    fn character(&mut self) -> Result<char, SemanticError> {
        let first = self
            .bump()?
            .ok_or_else(|| self.syntax("expected a character"))?;
        if first.is_whitespace() {
            return Err(self.syntax("character literal cannot begin with whitespace"));
        }
        if delimiter(first) {
            return Ok(first);
        }
        let start = self.position - first.len_utf8();
        while self.peek().is_some_and(|c| !delimiter(c)) {
            if self.position - start >= self.budget.limits.max_token_bytes {
                return Err(capacity());
            }
            self.bump()?;
        }
        let text = &self.input[start..self.position];
        self.budget.token(text.len())?;
        let mut chars = text.chars();
        if chars.next().is_some() && chars.next().is_none() {
            return Ok(first);
        }
        match text {
            "newline" => Ok('\n'),
            "return" => Ok('\r'),
            "space" => Ok(' '),
            "tab" => Ok('\t'),
            _ if text.len() == 5 && text.starts_with('u') => u32::from_str_radix(&text[1..], 16)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| self.syntax("invalid Unicode character escape")),
            _ => Err(self.syntax("invalid character literal")),
        }
    }
}

impl Iterator for EdnReader<'_> {
    type Item = Result<EdnValue, SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let result = self.next_value(0, false, None).and_then(|value| {
            if let Some(value) = value {
                identity(&value, &mut self.budget, 0, false, false)?;
                Ok(Some(value))
            } else {
                Ok(None)
            }
        });
        match result {
            Ok(Some(value)) => Some(Ok(value)),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(error) => {
                self.finished = true;
                Some(Err(self.located(error)))
            }
        }
    }
}

impl std::iter::FusedIterator for EdnReader<'_> {}

pub fn read_edn(input: &str) -> Result<EdnValue, SemanticError> {
    read_edn_with_options(input, &EdnReadOptions::default())
}

pub fn read_edn_with_options(
    input: &str,
    options: &EdnReadOptions,
) -> Result<EdnValue, SemanticError> {
    let mut reader = EdnReader::with_options(input, options)?;
    let value = reader
        .next()
        .transpose()?
        .ok_or_else(|| reader.located(reader.syntax("expected one EDN element")))?;
    // Trailing forms are not returned, so they must not invoke tag handlers.
    let trailing = reader
        .next_value(0, true, None)
        .map_err(|error| reader.located(error))?;
    if trailing.is_some() {
        return Err(reader.located(reader.syntax("unexpected trailing EDN element")));
    }
    Ok(value)
}

fn delimiter(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            ',' | '{' | '}' | '[' | ']' | '(' | ')' | '"' | ';' | '\\'
        )
}

fn symbol(text: &str) -> Result<Symbol, SemanticError> {
    let parsed = if text == "/" {
        Symbol::unqualified(text)
    } else if let Some((namespace, name)) = text.split_once('/') {
        Symbol::new(namespace, name)
    } else {
        Symbol::unqualified(text)
    };
    if !valid_name(parsed.namespace.as_deref(), &parsed.name, false) {
        return Err(SemanticError::incorrect(
            "edn/symbol",
            "invalid EDN identifier",
        ));
    }
    Ok(parsed)
}

fn starts_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_digit)
        || (matches!(bytes.first(), Some(b'+' | b'-'))
            && bytes.get(1).is_some_and(u8::is_ascii_digit))
}

fn number(text: &str, budget: &mut Budget) -> Result<EdnValue, SemanticError> {
    let invalid = || SemanticError::incorrect("edn/number", "invalid EDN numeric literal");
    let suffix = text
        .as_bytes()
        .last()
        .copied()
        .filter(|b| matches!(b, b'N' | b'M'));
    let body = if suffix.is_some() {
        &text[..text.len() - 1]
    } else {
        text
    };
    let bytes = body.as_bytes();
    let mut position = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let integer_start = position;
    while bytes.get(position).is_some_and(u8::is_ascii_digit) {
        position += 1;
    }
    if position == integer_start || (position > integer_start + 1 && bytes[integer_start] == b'0') {
        return Err(invalid());
    }
    let mut fraction_digits = 0;
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        let start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        fraction_digits = position - start;
        if fraction_digits == 0 {
            return Err(invalid());
        }
    }
    let coefficient_end = position;
    let mut exponent = 0_i128;
    let has_exponent = matches!(bytes.get(position), Some(b'e' | b'E'));
    if has_exponent {
        position += 1;
        let start = position;
        if matches!(bytes.get(position), Some(b'+' | b'-')) {
            position += 1;
        }
        let digits = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == digits {
            return Err(invalid());
        }
        exponent = body[start..position].parse().map_err(|_| invalid())?;
    }
    if position != bytes.len() || (suffix == Some(b'N') && (fraction_digits > 0 || has_exponent)) {
        return Err(invalid());
    }
    budget.numeric(body.len())?;
    match suffix {
        Some(b'N') => body.parse().map(EdnValue::BigInt).map_err(|_| invalid()),
        Some(b'M') => {
            let scale = i64::try_from(
                (fraction_digits as i128)
                    .checked_sub(exponent)
                    .ok_or_else(invalid)?,
            )
            .map_err(|_| invalid())?;
            let coefficient: String = body[..coefficient_end]
                .chars()
                .filter(|c| *c != '.')
                .collect();
            let coefficient = coefficient.parse().map_err(|_| invalid())?;
            Ok(EdnValue::BigDec(BigDecimal::new(coefficient, scale)))
        }
        _ if fraction_digits > 0 || has_exponent => {
            let value: f64 = body.parse().map_err(|_| invalid())?;
            let nonzero = bytes[..coefficient_end]
                .iter()
                .any(|b| matches!(b, b'1'..=b'9'));
            if !value.is_finite() || (value == 0.0 && nonzero) {
                return Err(SemanticError::incorrect(
                    "edn/number-range",
                    "double literal is outside its finite nonzero range",
                ));
            }
            Ok(EdnValue::Double(value))
        }
        _ => body.parse().map(EdnValue::Long).map_err(|_| {
            SemanticError::incorrect(
                "edn/integer-range",
                "integer exceeds i64; use the N suffix for arbitrary precision",
            )
        }),
    }
}
