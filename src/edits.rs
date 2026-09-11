//! Putting what a server answered with into a text: the edits a rename hands back, as ranges
//! of the text they were worked out against.
//!
//! An edit names two places by line and column, which only mean anything against the text the
//! server was looking at. A caller holding a different text - a buffer typed into since, a file
//! somebody else wrote - would have the edit land on whatever sits at those numbers now, so
//! every place is checked against the text it is being put into, and an edit that does not fit
//! it is refused rather than moved somewhere it might.

use std::ops::Range;

use anyhow::{Result, bail};

use crate::payload::{LspPosition, LspTextEdit};

/// How far into `text` a place is, in bytes.
///
/// Refused when the place is not in the text at all: a line past the last one, a column past
/// the end of its line, or a column in the middle of a character. Each of those is an edit
/// worked out against another text, which is the one mistake this exists to catch.
pub fn offset_of(text: &str, at: &LspPosition) -> Result<usize> {
    let mut line_start = 0;
    for _ in 0..at.line {
        let Some(newline) = text[line_start..].find('\n') else {
            bail!("line {} is past the end of the text", at.line + 1);
        };
        line_start += newline + 1;
    }
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |newline| line_start + newline);
    let offset = line_start + at.column;
    if offset > line_end || !text.is_char_boundary(offset) {
        bail!(
            "column {} of line {} is not a place in the text",
            at.column,
            at.line + 1
        );
    }
    Ok(offset)
}

/// The edits as byte ranges of `text` and what goes in each, in the order they sit in it.
///
/// Refused whole when any one of them does not fit the text, or when two of them overlap: the
/// protocol says a server's edits never do, and two that did would have no order to be put in
/// that leaves both of them meaning what they meant.
pub fn byte_ranges(text: &str, edits: &[LspTextEdit]) -> Result<Vec<(Range<usize>, String)>> {
    let mut ranges = edits
        .iter()
        .map(|edit| {
            let start = offset_of(text, &edit.start)?;
            let end = offset_of(text, &edit.end)?;
            if end < start {
                bail!("an edit ends before it starts");
            }
            Ok((start..end, edit.new_text.clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    ranges.sort_by_key(|(range, _)| (range.start, range.end));
    if ranges
        .windows(2)
        .any(|pair| pair[0].0.end > pair[1].0.start)
    {
        bail!("two of the edits overlap");
    }
    Ok(ranges)
}

/// `text` with every edit put in.
pub fn apply(text: &str, edits: &[LspTextEdit]) -> Result<String> {
    let ranges = byte_ranges(text, edits)?;
    let mut edited = String::with_capacity(text.len());
    let mut copied_to = 0;
    for (range, new_text) in ranges {
        edited.push_str(&text[copied_to..range.start]);
        edited.push_str(&new_text);
        copied_to = range.end;
    }
    edited.push_str(&text[copied_to..]);
    Ok(edited)
}
