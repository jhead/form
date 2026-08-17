import AppKit
import FormDesign
import SwiftUI

/// A contiguous run of text blocks, drawn by one `NSTextView` so a selection can span them.
///
/// ## The tradeoff (spec 11 §3)
///
/// SwiftUI's `.textSelection(.enabled)` does not join separate `Text` views: dragging across
/// two of them selects in whichever one the drag started. A single text system per run fixes
/// that for the case that matters — prose, headings and lists, which is most of an assistant
/// message. It cannot fix selection *across* a code block, a table or an image, because those
/// are native subviews with their own behaviour and there is no way to interleave them into
/// one text system short of writing a custom layout and hit-testing engine (`NSTextAttachment`
/// with a view provider gets close, but loses the hover chrome and horizontal scrolling that
/// F7.2 requires). So: selection is contiguous within a run, and "copy message" from the
/// transcript's hover actions (W10) covers "I want the whole thing".
///
/// `⌘C` yields markdown, not rendered text (F7.4) — see `MarkdownSourceMap`.
struct MarkdownTextRun: NSViewRepresentable {
    let rendered: RenderedText
    let metrics: MarkdownMetrics

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> MarkdownNSTextView {
        // An explicit TextKit 1 stack: `NSLayoutManager` is what lets the inline-code chip be
        // drawn with an inset and a radius, and what gives an exact height without a scroll
        // view wrapped around it.
        let storage = NSTextStorage()
        let layout = MarkdownLayoutManager()
        let container = NSTextContainer(size: .zero)
        container.widthTracksTextView = true
        container.lineFragmentPadding = 0
        layout.addTextContainer(container)
        storage.addLayoutManager(layout)

        let view = MarkdownNSTextView(frame: .zero, textContainer: container)
        view.isEditable = false
        view.drawsBackground = false
        view.textContainerInset = .zero
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.delegate = context.coordinator
        apply(to: view)
        return view
    }

    func updateNSView(_ view: MarkdownNSTextView, context: Context) {
        apply(to: view)
    }

    func sizeThatFits(
        _ proposal: ProposedViewSize, nsView: MarkdownNSTextView, context: Context
    ) -> CGSize? {
        let width = proposal.width.flatMap { $0.isFinite && $0 > 0 ? $0 : nil }
            ?? metrics.theme.metrics.contentMaxWidth
        guard let container = nsView.textContainer, let layout = nsView.layoutManager else {
            return nil
        }
        container.size = CGSize(width: width, height: .greatestFiniteMagnitude)
        layout.ensureLayout(for: container)
        return CGSize(width: width, height: ceil(layout.usedRect(for: container).height))
    }

    private func apply(to view: MarkdownNSTextView) {
        view.isSelectable = metrics.style.isSelectable
        view.linkTextAttributes = [
            .foregroundColor: metrics.theme.color.accent.nsColor,
            .cursor: NSCursor.pointingHand,
        ]
        if let layout = view.layoutManager as? MarkdownLayoutManager {
            layout.chipInset = CGSize(width: metrics.chipInsetH, height: metrics.chipInsetV)
            layout.chipRadius = metrics.chipRadius
        }
        // Identical content arrives on every streaming tick for every run but the last;
        // replacing the storage would drop the user's selection, so compare first.
        if view.rendered?.attributed != rendered.attributed {
            view.textStorage?.setAttributedString(rendered.attributed)
        }
        view.rendered = rendered
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        @MainActor
        func textView(_ view: NSTextView, clickedOnLink link: Any, at index: Int) -> Bool {
            let url = (link as? URL) ?? (link as? String).flatMap(MarkdownLink.url(from:))
            guard let url else { return false }
            MarkdownLink.open(url)
            return true
        }
    }
}

/// The text view itself: copy yields markdown, and hovering a link underlines it and shows
/// the URL (spec 11 §2).
final class MarkdownNSTextView: NSTextView {
    var rendered: RenderedText?

    private var underlined: NSRange?

    override func copy(_ sender: Any?) {
        guard let markdown = selectedMarkdown() else {
            super.copy(sender)
            return
        }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(markdown, forType: .string)
    }

    /// Services and drag-out take this path rather than `copy(_:)`.
    override func writeSelection(
        to pasteboard: NSPasteboard, types: [NSPasteboard.PasteboardType]
    ) -> Bool {
        guard types.contains(.string), let markdown = selectedMarkdown() else {
            return super.writeSelection(to: pasteboard, types: types)
        }
        pasteboard.setString(markdown, forType: .string)
        return true
    }

    private func selectedMarkdown() -> String? {
        let range = selectedRange()
        guard range.length > 0, let rendered else { return nil }
        let markdown = rendered.markdown(for: range)
        return markdown.isEmpty ? nil : markdown
    }

    // MARK: Link hover

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.filter { $0.owner === self }.forEach(removeTrackingArea)
        addTrackingArea(
            NSTrackingArea(
                rect: .zero,
                options: [.activeInKeyWindow, .inVisibleRect, .mouseMoved, .mouseExitedAndEntered],
                owner: self))
    }

    override func mouseMoved(with event: NSEvent) {
        super.mouseMoved(with: event)
        let point = convert(event.locationInWindow, from: nil)
        guard let storage = textStorage, storage.length > 0 else { return }
        let index = min(characterIndexForInsertion(at: point), storage.length - 1)
        var range = NSRange(location: 0, length: 0)
        if let url = storage.attribute(.link, at: index, effectiveRange: &range) as? URL {
            highlight(range, url: url)
        } else {
            clearHighlight()
        }
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        clearHighlight()
    }

    private func highlight(_ range: NSRange, url: URL) {
        guard underlined != range else { return }
        clearHighlight()
        layoutManager?.addTemporaryAttributes(
            [.underlineStyle: NSUnderlineStyle.single.rawValue], forCharacterRange: range)
        underlined = range
        toolTip = MarkdownLink.display(url)
    }

    private func clearHighlight() {
        guard let underlined else { return }
        layoutManager?.removeTemporaryAttribute(.underlineStyle, forCharacterRange: underlined)
        self.underlined = nil
        toolTip = nil
    }
}

/// Draws the inline-code chip. `NSAttributedString`'s background attribute paints a rect
/// tight to the glyphs; spec 11 §2 asks for a chip, so the fill hook is where the inset and
/// the radius go — the color still comes from the attribute, which comes from the theme.
final class MarkdownLayoutManager: NSLayoutManager {
    var chipInset: CGSize = .zero
    var chipRadius: CGFloat = 0

    override func fillBackgroundRectArray(
        _ rectArray: UnsafePointer<NSRect>, count: Int, forCharacterRange charRange: NSRange,
        color: NSColor
    ) {
        color.setFill()
        for index in 0 ..< count {
            let rect = rectArray[index].insetBy(dx: -chipInset.width, dy: -chipInset.height)
            NSBezierPath(roundedRect: rect, xRadius: chipRadius, yRadius: chipRadius).fill()
        }
    }
}
