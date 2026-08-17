import Foundation

/// Maps a range of *rendered* text back onto the *markdown source* that produced it (F7.4).
///
/// ## Why a reconstruction and not a slice of the original string
///
/// The block tree the core hands us (spec 05 §2) carries no source offsets — a block knows
/// its content and its stable id, not where in the input it came from. So the source side of
/// this map is re-emitted from the tree while the rendered side is being built, in one pass,
/// which makes the two exactly consistent by construction. What it is *not* is byte-identical
/// to the user's typing: `_x_` comes back as `*x*`, a setext heading comes back as an ATX
/// one, and a link written as a reference comes back inline. Everything round-trips to the
/// same tree, which is the property that matters for pasting it somewhere else.
///
/// If the core ever grows a `sourceRange` per block and span, this type keeps its shape and
/// the reconstruction is replaced by a slice.
struct MarkdownSourceMap {
    /// The reconstructed markdown for the run this map covers.
    let source: String
    /// Roots, in rendered order.
    let nodes: [SourceNode]

    static let empty = MarkdownSourceMap(source: "", nodes: [])

    /// The markdown for a rendered selection.
    ///
    /// Two rules, both of which fall out of "give the user something they could paste":
    /// * a construct **fully** inside the selection contributes its delimiters — selecting
    ///   the word `bold` yields `**bold**`;
    /// * a construct **straddling** the selection contributes only the selected text —
    ///   selecting half of it must not hand back an unbalanced `**bol`.
    func markdown(for selection: NSRange, rendered: NSString) -> String {
        guard selection.length > 0 else { return "" }
        var out = ""
        let sourceNS = source as NSString
        for node in nodes {
            node.append(to: &out, selection: selection, source: sourceNS, rendered: rendered)
        }
        return out.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// A node of the rendered → source correspondence.
///
/// `leaf` is a run of characters that came from one place; `wrapper` is a construct with
/// delimiters (emphasis, a link, a list marker, a heading's `##`) whose children are the
/// content between them.
indirect enum SourceNode {
    /// `literal` means rendered text and source text are character-identical, so a partial
    /// selection can be sliced at the same offsets. Inline code is not literal — its source
    /// carries backticks — so a partial selection falls back to the rendered characters.
    case leaf(rendered: NSRange, source: NSRange, literal: Bool)
    case wrapper(rendered: NSRange, open: NSRange, close: NSRange, children: [SourceNode])

    var renderedRange: NSRange {
        switch self {
        case let .leaf(rendered, _, _): rendered
        case let .wrapper(rendered, _, _, _): rendered
        }
    }

    func append(to out: inout String, selection: NSRange, source: NSString, rendered: NSString) {
        let range = renderedRange
        let overlap = NSIntersectionRange(range, selection)
        // A zero-length node (a bare delimiter, an emitted newline) still belongs to the
        // selection when it sits strictly inside it.
        let touches = overlap.length > 0
            || (range.length == 0 && range.location > selection.location
                && range.location < NSMaxRange(selection))
        guard touches else { return }

        let contained = selection.location <= range.location
            && NSMaxRange(range) <= NSMaxRange(selection)

        switch self {
        case let .leaf(renderedRange, sourceRange, literal):
            if contained {
                out += source.substring(with: sourceRange)
            } else if literal {
                // Same characters on both sides, so the overlap maps offset for offset.
                let delta = overlap.location - renderedRange.location
                out += source.substring(
                    with: NSRange(location: sourceRange.location + delta, length: overlap.length))
            } else {
                out += rendered.substring(with: overlap)
            }
        case let .wrapper(_, open, close, children):
            if contained { out += source.substring(with: open) }
            for child in children {
                child.append(to: &out, selection: selection, source: source, rendered: rendered)
            }
            if contained { out += source.substring(with: close) }
        }
    }
}

/// A rendered text run plus the map that takes a selection in it back to markdown.
struct RenderedText {
    let attributed: NSAttributedString
    let map: MarkdownSourceMap

    static let empty = RenderedText(attributed: NSAttributedString(), map: .empty)

    var isEmpty: Bool { attributed.length == 0 }

    func markdown(for selection: NSRange) -> String {
        map.markdown(for: selection, rendered: attributed.string as NSString)
    }
}
