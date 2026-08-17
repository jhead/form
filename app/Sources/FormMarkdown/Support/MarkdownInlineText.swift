import FormCore
import FormDesign
import SwiftUI

/// Spans → `AttributedString` for the places that are **not** a text run: table cells, image
/// captions, alt text.
///
/// The text-run path builds `NSAttributedString` instead (`MarkdownAttributedBuilder`),
/// because TextKit is what gives it selection, a source map and the drawn code chip. Rather
/// than bridge one representation into the other — the SwiftUI and AppKit attribute scopes
/// only partly overlap, and which one `Text` honours is not something to guess at — each
/// side builds what it needs from the same tree. The tree is the shared representation; the
/// attributed strings are just two projections of it.
enum MarkdownInlineText {
    static func attributed(
        _ spans: [Span], metrics: MarkdownMetrics, style: TypeStyle? = nil,
        color: ThemeColor? = nil
    ) -> AttributedString {
        var out = AttributedString()
        append(spans, to: &out, metrics: metrics, state: State(
            font: (style ?? metrics.body).font, color: color ?? metrics.textColor))
        return out
    }

    private struct State {
        var font: Font
        var color: ThemeColor
        var strike = false
        var link: URL?
    }

    private static func append(
        _ spans: [Span], to out: inout AttributedString, metrics: MarkdownMetrics, state: State
    ) {
        for span in spans {
            switch span {
            case let .text(text):
                out += run(text, state: state)

            case let .emphasis(inner):
                var next = state
                next.font = state.font.italic()
                append(inner, to: &out, metrics: metrics, state: next)

            case let .strong(inner):
                var next = state
                next.font = state.font.weight(.semibold)
                append(inner, to: &out, metrics: metrics, state: next)

            case let .strike(inner):
                var next = state
                next.strike = true
                append(inner, to: &out, metrics: metrics, state: next)

            case let .code(text):
                var chip = run(text, state: state)
                chip.font = metrics.codeInline.font
                chip.backgroundColor = metrics.theme.color.surfaceRaised.color
                out += chip

            case let .link(url, _, inner):
                var next = state
                next.color = metrics.theme.color.accent
                next.link = MarkdownLink.url(from: url)
                append(inner, to: &out, metrics: metrics, state: next)

            case let .footnoteRef(label):
                var next = state
                next.font = metrics.theme.typography.micro.font
                next.color = metrics.theme.color.accent
                out += run(label, state: next)

            case let .break(hard):
                out += run(hard ? "\n" : " ", state: state)

            case .unknown:
                continue
            }
        }
    }

    private static func run(_ text: String, state: State) -> AttributedString {
        var piece = AttributedString(text)
        piece.font = state.font
        piece.foregroundColor = state.color.color
        if state.strike { piece.strikethroughStyle = .single }
        // SwiftUI renders a `.link` run as a link and routes the click through
        // `EnvironmentValues.openURL`, which `MarkdownBlocksView` points at `MarkdownLink`.
        if let link = state.link { piece.link = link }
        return piece
    }
}
