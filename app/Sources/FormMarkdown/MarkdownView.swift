import SwiftUI
import FormCore
import FormDesign

/// Renders the block tree the core produces. Parses nothing.
/// **Owner: W11** — see `docs/specs/11-markdown-rendering.md`.
public struct MarkdownView: View {
    private let text: String
    public init(text: String) { self.text = text }
    public var body: some View {
        // TODO(W11): render MarkdownDoc blocks; this placeholder exists so the module builds.
        Text(text)
    }
}
