//! Splitting an over-length embedding input into pieces that rejoin exactly (#1021).
//!
//! Ported from knowledge-base's `split_to_limit`/`_fit` (knowledge-base#615), the plain-text
//! subset: the stored content is never cut, only what one embed request carries.

use memory_engine::error::{EmbedderError, MemoryError};

/// Counts a text's tokens as the server counts an embedding input.
type Count<'c> = dyn FnMut(&str) -> Result<usize, MemoryError> + 'c;

/// `text` as consecutive slices that each count at most `limit`, each with its count; joined,
/// they are `text`.
///
/// When `count(text) <= limit` the text is one piece. Otherwise the pieces but the last aim at
/// an even share of the content, each plus `count("")` (the tokens the server adds to every
/// input), so the last is not a stray word; the last aims at `limit` and takes what is left.
/// The count returned with a piece is the one made for it.
///
/// # Errors
///
/// [`EmbedderError::InputRejected`] when `limit <= count("")` or a first character alone does
/// not fit: not even one character fits. Any error `count` returns.
pub fn split_to_limit<'t>(
    text: &'t str,
    limit: usize,
    count: &mut Count<'_>,
) -> Result<Vec<(&'t str, usize)>, MemoryError> {
    let total = count(text)?;
    if total <= limit {
        return Ok(vec![(text, total)]);
    }
    let overhead = count("")?;
    if limit <= overhead {
        return Err(nothing_fits(limit));
    }
    let planned = (total - overhead).div_ceil(limit - overhead);
    let share = (total - overhead).div_ceil(planned) + overhead;
    let mut pieces = Vec::with_capacity(planned);
    let mut rest = text;
    while !rest.is_empty() {
        let aim = if pieces.len() + 1 < planned {
            share
        } else {
            limit
        };
        let cut = match fit(rest, aim, count)? {
            None if aim != limit => fit(rest, limit, count)?,
            cut => cut,
        };
        let Some((end, n)) = cut else {
            return Err(nothing_fits(limit));
        };
        pieces.push((&rest[..end], n));
        rest = &rest[end..];
    }
    Ok(pieces)
}

fn nothing_fits(limit: usize) -> MemoryError {
    EmbedderError::InputRejected(format!(
        "not even one character fits in the embedder's {limit}-token input limit"
    ))
    .into()
}

/// The longest prefix of `text` (not empty) that counts at most `limit`, as its byte length
/// and its count; `None` when not even one character fits.
///
/// It gallops from `limit` characters (doubling), then bisects, and only returns a length it
/// counted: a count that does not grow with length can shorten a piece, never overfill one.
/// The cut then moves back to just after the last line end, else the last whitespace, when
/// that keeps at least half of it, so a short line is never cut.
fn fit(
    text: &str,
    limit: usize,
    count: &mut Count<'_>,
) -> Result<Option<(usize, usize)>, MemoryError> {
    // The longest prefix counted to fit: (characters, bytes, count).
    let mut best = None;
    let mut probe = limit.max(1);
    loop {
        let (end, chars) = prefix(text, probe);
        probe = chars;
        let n = count(&text[..end])?;
        if n > limit {
            break;
        }
        best = Some((chars, end, n));
        if end == text.len() {
            return Ok(Some((end, n)));
        }
        probe = probe.saturating_mul(2);
    }
    let (mut lo, mut hi) = (best.map_or(0, |(chars, ..)| chars) + 1, probe - 1);
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let (end, _) = prefix(text, mid);
        let n = count(&text[..end])?;
        if n <= limit {
            best = Some((mid, end, n));
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    let Some((chars, end, n)) = best else {
        return Ok(None);
    };
    let half = || text[..end].char_indices().rev().take(chars - chars / 2);
    let after = |(i, c): (usize, char)| i + c.len_utf8();
    let space = half()
        .find(|&(_, c)| c == '\n')
        .or_else(|| half().find(|&(_, c)| c.is_whitespace()))
        .map_or(end, after);
    if space == end {
        return Ok(Some((end, n)));
    }
    let m = count(&text[..space])?;
    Ok(Some(if m <= limit { (space, m) } else { (end, n) }))
}

/// The byte length of `text`'s first `chars` characters, and how many it has: fewer when
/// `text` is shorter.
fn prefix(text: &str, chars: usize) -> (usize, usize) {
    text.char_indices().nth(chars).map_or_else(
        || (text.len(), text.chars().count()),
        |(end, _)| (end, chars),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// One token per whitespace-separated word, plus the EOS the server appends.
    fn words(text: &str) -> usize {
        text.split_whitespace().count() + 1
    }

    fn chars(text: &str) -> usize {
        text.chars().count()
    }

    /// Split with `count` and check what every split keeps: the pieces rejoin, none is
    /// empty, each counts at most `limit` and comes with its own count.
    fn check(text: &str, limit: usize, mut count: impl FnMut(&str) -> usize) -> Vec<&str> {
        let pieces = split_to_limit(text, limit, &mut |t| Ok(count(t))).unwrap();
        assert_eq!(pieces.iter().map(|(p, _)| *p).collect::<String>(), text);
        for &(piece, n) in &pieces {
            assert_ne!(piece, "");
            assert!(n <= limit, "{piece:?} counts {n}");
            assert_eq!(n, count(piece), "{piece:?}");
        }
        pieces.into_iter().map(|(p, _)| p).collect()
    }

    #[test]
    fn a_text_that_fits_comes_back_whole() {
        let pieces = split_to_limit("a b c", 4, &mut |t| Ok(words(t))).unwrap();
        assert_eq!(pieces, [("a b c", 4)]);
    }

    #[test]
    fn pieces_rejoin_exactly_with_newlines_indentation_and_trailing_whitespace() {
        let text = "def f():\n    a = 1\n\n    return a  \n\t\n".repeat(5);
        assert!(check(&text, 6, words).len() > 1);
    }

    #[test]
    fn a_non_monotone_count_still_yields_verified_pieces() {
        let mut probed = Vec::new();
        // 20 characters count far over the limit, though 20 would fit.
        check(&"a ".repeat(30), 20, |t| {
            probed.push(chars(t));
            if chars(t) == 20 { 999 } else { chars(t) }
        });
        assert!(probed.contains(&20));
    }

    #[test]
    fn a_cut_keeps_at_least_half_a_piece() {
        let text = format!("## References\n\n{}", "x".repeat(100));
        let pieces = check(&text, 50, chars);
        assert!(pieces[0].len() > "## References\n\n".len(), "{pieces:?}");
    }

    #[test]
    fn a_cut_prefers_the_end_of_a_line() {
        let cut = fit("one two three\nfour five", 5, &mut |t| Ok(words(t))).unwrap();
        // Not after "four ".
        assert_eq!(cut, Some(("one two three\n".len(), 4)));
        assert_eq!(
            check("one two three\nfour five six seven", 6, words),
            ["one two three\n", "four five six seven"]
        );
    }

    #[test]
    fn no_piece_is_a_stray_tail() {
        for n in [64, 65, 127, 128, 200] {
            let text = vec!["w"; n].join(" ");
            let pieces = check(&text, 64, words);
            assert!(pieces.iter().all(|p| words(p) > 64 / 4), "{n}: {pieces:?}");
        }
    }

    #[test]
    fn a_character_too_big_for_the_even_share_still_fits() {
        // "Y" alone counts 8 with its EOS: over the share of 7, within the limit 11.
        let count = |t: &str| t.matches('Y').count() * 7 + t.matches('a').count() + 1;
        assert_eq!(check("Yaaaa", 11, count), ["Yaaa", "a"]);
    }

    #[test]
    fn a_long_word_after_a_fitting_boundary_is_cut_inside_the_word() {
        let text = format!("aa bb {} cc", "x".repeat(50));
        let pieces = check(&text, 10, chars);
        assert_eq!(pieces[0], "aa bb ");
        assert_eq!(pieces[1], "x".repeat(10));
    }

    #[test]
    fn nothing_fits_is_input_rejected() {
        let rejected = |err: &MemoryError| {
            matches!(err, MemoryError::Embedder(EmbedderError::InputRejected(m))
                if m.contains("not even one character fits"))
        };
        // One character and its EOS count 2.
        let err = split_to_limit("abc", 1, &mut |t| Ok(chars(t) + 1)).unwrap_err();
        assert!(rejected(&err), "{err}");
        // The limit is above the EOS, but the first character alone is over it.
        let err = split_to_limit("Yabc", 5, &mut |t| {
            Ok(t.matches('Y').count() * 7 + chars(t) + 1)
        })
        .unwrap_err();
        assert!(rejected(&err), "{err}");
    }

    #[test]
    fn cuts_fall_on_character_boundaries() {
        // Counted in characters, cut in bytes: three two-byte characters a piece.
        assert_eq!(check("éééééé", 3, chars), ["ééé", "ééé"]);
        let text = "日本語 😀 ñandú\n".repeat(20);
        assert!(check(&text, 7, words).len() > 1);
        assert!(check(&text, 9, |t| t.len() + 2).len() > 1);
    }

    proptest! {
        #[test]
        fn chunk_invariants_hold(
            text in "(\\PC|[\n\t ]){1,300}",
            // At least 6: any one character fits each counter below.
            limit in 6_usize..40,
            counter in 0_usize..3,
        ) {
            let count = |t: &str| match counter {
                0 => words(t),
                1 => chars(t) + 1,
                _ => t.len() + 2,
            };
            let pieces = split_to_limit(&text, limit, &mut |t| Ok(count(t))).unwrap();
            prop_assert_eq!(pieces.iter().map(|(p, _)| *p).collect::<String>(), text.clone());
            let mut at = 0;
            for &(piece, n) in &pieces {
                prop_assert!(!piece.is_empty());
                prop_assert!(n <= limit);
                prop_assert_eq!(n, count(piece));
                at += piece.len();
                prop_assert!(text.is_char_boundary(at));
            }
        }
    }
}
