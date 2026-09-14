use super::*;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AttributeName {
    Id(u32),
    Ident(Keyword),
}

/// A read-side entity identifier resolved against one exact immutable
/// database value. Lookup refs deliberately carry an attribute name rather
/// than only an integer so historical aliases remain usable at the API edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityIdentifier {
    Id(u64),
    Ident(Keyword),
    Lookup {
        attribute: AttributeName,
        value: Value,
    },
}

impl From<u64> for EntityIdentifier {
    fn from(value: u64) -> Self {
        Self::Id(value)
    }
}

impl From<Keyword> for EntityIdentifier {
    fn from(value: Keyword) -> Self {
        Self::Ident(value)
    }
}

impl From<&Keyword> for EntityIdentifier {
    fn from(value: &Keyword) -> Self {
        Self::Ident(value.clone())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PullDirection {
    Forward(AttributeName),
    Reverse(AttributeName),
    /// Resolve a selector spelling against the captured schema. An installed
    /// attribute named `_name` is forward; otherwise `_name` means reverse
    /// navigation through `name`. Explicit Forward/Reverse never reinterpret.
    SchemaResolved(Keyword),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PullLimit {
    #[default]
    Default,
    Limit(usize),
    Unlimited,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PullNested {
    Pattern(Box<PullPattern>),
    Recursion(Option<usize>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullAttribute {
    pub direction: PullDirection,
    pub alias: Option<QueryValue>,
    pub default: Option<QueryValue>,
    pub limit: PullLimit,
    pub nested: Option<PullNested>,
    pub transform: Option<PullTransform>,
}

impl PullAttribute {
    pub fn forward(attribute: AttributeName) -> Self {
        Self {
            direction: PullDirection::Forward(attribute),
            alias: None,
            default: None,
            limit: PullLimit::Default,
            nested: None,
            transform: None,
        }
    }

    /// Select incoming references. Without explicit nesting, parents are
    /// `:db/id` maps: one map for a component attribute, a collection otherwise.
    /// Component parents are not implicitly expanded by reverse selection.
    pub fn reverse(attribute: AttributeName) -> Self {
        Self {
            direction: PullDirection::Reverse(attribute),
            alias: None,
            default: None,
            limit: PullLimit::Default,
            nested: None,
            transform: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct PullPattern {
    pub wildcard: bool,
    pub attributes: Vec<PullAttribute>,
}

impl Clone for PullPattern {
    fn clone(&self) -> Self {
        enum Task<'a> {
            Visit(&'a PullPattern),
            Finish(&'a PullPattern, usize),
        }
        let mut pending = vec![Task::Visit(self)];
        let mut results = Vec::new();
        while let Some(task) = pending.pop() {
            match task {
                Task::Visit(pattern) => {
                    let children = pattern.attributes.iter().filter_map(|attribute| {
                        if let Some(PullNested::Pattern(child)) = &attribute.nested {
                            Some(child.as_ref())
                        } else {
                            None
                        }
                    });
                    pending.push(Task::Finish(pattern, children.clone().count()));
                    pending.extend(children.rev().map(Task::Visit));
                }
                Task::Finish(pattern, count) => {
                    let mut children = results.split_off(results.len() - count).into_iter();
                    let attributes = pattern
                        .attributes
                        .iter()
                        .map(|attribute| PullAttribute {
                            direction: attribute.direction.clone(),
                            alias: attribute.alias.clone(),
                            default: attribute.default.clone(),
                            limit: attribute.limit,
                            transform: attribute.transform.clone(),
                            nested: match &attribute.nested {
                                Some(PullNested::Pattern(_)) => Some(PullNested::Pattern(
                                    Box::new(children.next().expect("nested pattern result")),
                                )),
                                Some(PullNested::Recursion(limit)) => {
                                    Some(PullNested::Recursion(*limit))
                                }
                                None => None,
                            },
                        })
                        .collect();
                    results.push(Self {
                        wildcard: pattern.wildcard,
                        attributes,
                    });
                }
            }
        }
        results.pop().expect("one pattern clone")
    }
}

impl Drop for PullPattern {
    fn drop(&mut self) {
        let mut pending = std::mem::take(&mut self.attributes);
        while let Some(mut attribute) = pending.pop() {
            if let Some(PullNested::Pattern(mut pattern)) = attribute.nested.take() {
                pending.append(&mut pattern.attributes);
            }
        }
    }
}

impl PartialEq for PullPattern {
    fn eq(&self, other: &Self) -> bool {
        let mut pending = vec![(self, other)];
        while let Some((left, right)) = pending.pop() {
            if left.wildcard != right.wildcard || left.attributes.len() != right.attributes.len() {
                return false;
            }
            for (left, right) in left.attributes.iter().zip(&right.attributes) {
                if left.direction != right.direction
                    || left.alias != right.alias
                    || left.default != right.default
                    || left.limit != right.limit
                    || left.transform != right.transform
                {
                    return false;
                }
                match (&left.nested, &right.nested) {
                    (Some(PullNested::Pattern(left)), Some(PullNested::Pattern(right))) => {
                        pending.push((left, right));
                    }
                    (Some(PullNested::Recursion(left)), Some(PullNested::Recursion(right)))
                        if left == right => {}
                    (None, None) => {}
                    _ => return false,
                }
            }
        }
        true
    }
}

impl Eq for PullPattern {}

impl PullPattern {
    pub fn attributes(attributes: Vec<PullAttribute>) -> Self {
        Self {
            wildcard: false,
            attributes,
        }
    }

    pub fn wildcard() -> Self {
        Self {
            wildcard: true,
            attributes: Vec::new(),
        }
    }
}
