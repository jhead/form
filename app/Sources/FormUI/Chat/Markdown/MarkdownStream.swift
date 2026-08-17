import Foundation
import FormCore

/// Debounced markdown parsing for a message that is still arriving (spec 10 §2).
///
/// Parsing runs in Rust and a 450-line response arrives in a few hundred deltas, so parsing
/// per delta is the difference between a smooth stream and a slideshow. Two rules keep it
/// both cheap and honest:
///
/// 1. **Debounce ~50 ms.** Deltas that land inside the window coalesce into one parse.
/// 2. **Force on a block boundary.** A blank line, a fence opening or closing, or the start
///    of a heading/list/table row *changes the document's structure*, and waiting 50 ms to
///    show that is what reads as reflow flicker. Those parse immediately.
///
/// The parsed document keeps the core's stable block ids, so the view's `ForEach` re-renders
/// only the tail block rather than rebuilding every block per token.
@MainActor
@Observable
public final class MarkdownStream {
    /// The document the view renders. Empty until the first parse resolves.
    public private(set) var doc = MarkdownDoc()

    /// Spec 10 §2. Not a motion token: this is a parse cadence, not an animation.
    static let debounce = Duration.milliseconds(50)

    @ObservationIgnored private let client: CoreClient
    @ObservationIgnored private var pending: Task<Void, Never>?
    /// The text the current `doc` was parsed from, and whether that parse was a final one.
    @ObservationIgnored private var parsedText = ""
    @ObservationIgnored private var parsedComplete = false
    /// The newest text seen, which may be ahead of `parsedText` inside the debounce window.
    @ObservationIgnored private var latestText = ""

    public init(client: CoreClient) {
        self.client = client
    }

    /// Hand this the message's text on every change. Cheap to call per delta.
    public func update(text: String, isComplete: Bool) {
        guard text != parsedText || isComplete != parsedComplete else {
            pending?.cancel()
            pending = nil
            return
        }
        let previous = latestText
        latestText = text

        // A finished message is authoritative and must not sit behind a timer.
        if isComplete || Self.crossesBlockBoundary(from: previous, to: text) {
            pending?.cancel()
            pending = Task { [weak self] in await self?.parse(text, complete: isComplete) }
            return
        }

        guard pending == nil else { return }
        pending = Task { [weak self] in
            try? await Task.sleep(for: Self.debounce)
            guard !Task.isCancelled, let self else { return }
            await self.parse(self.latestText, complete: false)
        }
    }

    /// Render text that will never change again — a message loaded from the store.
    public func renderOnce(_ text: String) {
        update(text: text, isComplete: true)
    }

    private func parse(_ text: String, complete: Bool) async {
        pending = nil
        // The document is a pure function of the text, so a parse that lost a race to a
        // newer one has nothing to contribute.
        guard text == latestText else { return }
        do {
            let parsed = try await client.query(RenderMarkdown(text: text, complete: complete))
            guard text == latestText else { return }
            doc = parsed
            parsedText = text
            parsedComplete = complete
        } catch {
            Log.ui.error("renderMarkdown failed: \(String(describing: error), privacy: .public)")
        }
    }

    // MARK: - Boundaries

    /// True when the text added since the last parse opened or closed a block.
    ///
    /// Only the appended suffix is examined, plus a short overlap so a boundary split across
    /// two deltas (`"\n"` then `"\n"`) is still caught.
    static func crossesBlockBoundary(from previous: String, to next: String) -> Bool {
        guard next.count > previous.count else { return !previous.isEmpty }
        let overlap = 3
        let start =
            next.index(
                next.startIndex,
                offsetBy: max(0, previous.count - overlap),
                limitedBy: next.endIndex) ?? next.startIndex
        let suffix = next[start...]

        // A blank line closes a paragraph and opens whatever comes next.
        if suffix.contains("\n\n") { return true }
        // A fence toggles the whole tail of the document between prose and code, which is
        // the change most visible if it lags (F7.3).
        if suffix.contains("```") || suffix.contains("~~~") { return true }
        // A newline followed by block-opening punctuation: heading, list, quote, table row,
        // or a rule.
        guard let newline = suffix.lastIndex(of: "\n") else { return false }
        let afterNewline = suffix[suffix.index(after: newline)...]
            .drop { $0 == " " || $0 == "\t" }
        guard let marker = afterNewline.first else { return false }
        return "#-*+>|=_".contains(marker) || marker.isNumber
    }
}
