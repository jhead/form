import FormCore
import Foundation

extension MarkdownBlock {
    /// True when this block and everything under it can live inside one `NSTextView`.
    ///
    /// Code blocks, tables and images are native SwiftUI views (spec 11 §3) — they need
    /// their own scrolling, hover chrome and image loading, none of which a text run can
    /// give them. A quote is native too: its 2 pt leading rule is a drawn shape, not a text
    /// attribute. A list is textual only while nothing non-textual is nested inside it.
    var isTextual: Bool {
        switch kind {
        case .paragraph, .heading, .html:
            return true
        case let .footnoteDef(_, blocks):
            return blocks.allSatisfy(\.isTextual)
        case let .list(_, _, _, items):
            return items.allSatisfy { $0.blocks.allSatisfy(\.isTextual) }
        case .codeBlock, .table, .image, .quote, .rule, .unknown:
            return false
        }
    }
}

/// One unit of the vertical stack: either a contiguous run of text blocks sharing a single
/// selectable `NSTextView`, or one block that renders natively.
enum MarkdownRun: Identifiable {
    case text([MarkdownBlock])
    case native(MarkdownBlock)

    /// Identity comes from the first block's core id, which is stable across streaming
    /// re-parses (spec 05 §2). Using the whole run's ids would tear the text view down every
    /// time a single token landed in it.
    var id: String {
        switch self {
        case let .text(blocks): blocks.first.map { "t:\($0.id)" } ?? "t:empty"
        case let .native(block): "n:\(block.id)"
        }
    }

    /// What the render cache keys on — changes exactly when the run's content changes,
    /// because a block id is a hash of the block.
    var contentKey: String {
        switch self {
        case let .text(blocks): blocks.map(\.id).joined(separator: ",")
        case let .native(block): block.id
        }
    }

    var blocks: [MarkdownBlock] {
        switch self {
        case let .text(blocks): blocks
        case let .native(block): [block]
        }
    }

    /// Splits a block list into runs, coalescing neighbouring text blocks.
    static func segment(_ blocks: [MarkdownBlock]) -> [MarkdownRun] {
        var runs: [MarkdownRun] = []
        var pending: [MarkdownBlock] = []

        func flush() {
            guard !pending.isEmpty else { return }
            runs.append(.text(pending))
            pending = []
        }

        for block in blocks {
            if block.isTextual {
                pending.append(block)
            } else {
                flush()
                runs.append(.native(block))
            }
        }
        flush()
        return runs
    }
}
