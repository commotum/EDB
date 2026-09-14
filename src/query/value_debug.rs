//! Query result diagnostics must not reintroduce recursion limits after
//! traversal, cloning, comparison, and destruction have become stack-safe.
use super::QueryValue;
use std::fmt;

enum Frame<'a> {
    Value(&'a QueryValue),
    Sequence(&'a [QueryValue], usize),
    Map(&'a [(QueryValue, QueryValue)], usize),
    Text(&'static str),
}

impl fmt::Debug for QueryValue {
    /// Compact diagnostic formatting. Container traversal uses heap frames;
    /// it neither clones values nor recursively formats nested QueryValues.
    /// Scalar formatting retains Value's ordinary diagnostic representation.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut pending = vec![Frame::Value(self)];
        while let Some(frame) = pending.pop() {
            match frame {
                Frame::Value(Self::Nil) => formatter.write_str("Nil")?,
                Frame::Value(Self::Scalar(value)) => write!(formatter, "Scalar({value:?})")?,
                Frame::Value(Self::Char(value)) => write!(formatter, "Char({value:?})")?,
                Frame::Value(Self::Tagged(tag, value)) => {
                    write!(formatter, "Tagged({tag:?}, ")?;
                    pending.push(Frame::Text(")"));
                    pending.push(Frame::Value(value));
                }
                Frame::Value(Self::Set(values)) => {
                    formatter.write_str("Set([")?;
                    pending.push(Frame::Sequence(values, 0));
                }
                Frame::Value(Self::Collection(values)) => {
                    formatter.write_str("Collection([")?;
                    pending.push(Frame::Sequence(values, 0));
                }
                Frame::Value(Self::Tuple(values)) => {
                    formatter.write_str("Tuple([")?;
                    pending.push(Frame::Sequence(values, 0));
                }
                Frame::Value(Self::Map(entries)) => {
                    formatter.write_str("Map([")?;
                    pending.push(Frame::Map(entries, 0));
                }
                Frame::Sequence(values, index) => match values.get(index) {
                    Some(value) => {
                        if index > 0 {
                            formatter.write_str(", ")?;
                        }
                        pending.push(Frame::Sequence(values, index + 1));
                        pending.push(Frame::Value(value));
                        Ok(())
                    }
                    None => formatter.write_str("])"),
                }?,
                Frame::Map(entries, index) => match entries.get(index) {
                    Some((key, value)) => {
                        if index > 0 {
                            formatter.write_str(", ")?;
                        }
                        formatter.write_str("(")?;
                        pending.push(Frame::Map(entries, index + 1));
                        pending.push(Frame::Text(")"));
                        pending.push(Frame::Value(value));
                        pending.push(Frame::Text(", "));
                        pending.push(Frame::Value(key));
                        Ok(())
                    }
                    None => formatter.write_str("])"),
                }?,
                Frame::Text(text) => formatter.write_str(text)?,
            }
        }
        Ok(())
    }
}
