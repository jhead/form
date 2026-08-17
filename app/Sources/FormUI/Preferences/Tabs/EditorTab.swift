import AppKit
import FormCore
import FormDesign
import SwiftUI

struct EditorTab: View {
    @Environment(\.theme) private var theme
    let controller: PreferencesController

    private var settings: Settings { controller.settings }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xxl) {
            PreferenceSection(title: "Code") {
                PreferenceRow(title: "Font") {
                    PreferenceMenu(
                        selection: controller.binding(\.codeFont),
                        options: MonospacedFonts.available.map { PreferenceOption($0, $0) }
                    )
                }
                FormDivider()
                PreferenceRow(title: "Size") {
                    PreferenceSlider(
                        value: controller.binding(\.codeFontSize),
                        range: EditorDefaults.fontSizeRange,
                        ladder: Array(
                            stride(
                                from: EditorDefaults.fontSizeRange.lowerBound,
                                through: EditorDefaults.fontSizeRange.upperBound, by: 1)),
                        format: { "\(Int($0.rounded())) pt" }
                    )
                }
                FormDivider()
                PreferenceRow(title: "Tab width") {
                    PreferenceMenu(
                        selection: controller.binding(\.tabWidth),
                        options: EditorDefaults.tabWidthRange.map {
                            PreferenceOption($0, "\($0) spaces")
                        }
                    )
                }
                FormDivider()
                PreferenceRow(
                    title: "Wrap long lines",
                    help: "Off scrolls a wide line horizontally instead of folding it."
                ) {
                    PreferenceToggle(isOn: controller.binding(\.wrapCode))
                }
                FormDivider()
                PreferenceRow(title: "Show line numbers") {
                    PreferenceToggle(isOn: controller.binding(\.showLineNumbers))
                }
            }

            PreferenceSection(
                title: "Preview",
                footer: "Rendered with the settings above, at the current text size."
            ) {
                CodeSample(
                    font: settings.codeFont,
                    size: settings.codeFontSize,
                    tabWidth: settings.tabWidth,
                    wraps: settings.wrapCode,
                    showsLineNumbers: settings.showLineNumbers
                )
            }
        }
        .preferencePane()
    }
}

/// The live sample (spec 13, Editor).
///
/// It is rendered here rather than handed to `FormMarkdown`: `MarkdownView` renders the block
/// tree the core produces and takes no font, tab width or gutter — which are precisely the
/// things this pane exists to preview. If W11 grows a code-block style parameter, this view
/// becomes a call into it.
private struct CodeSample: View {
    @Environment(\.theme) private var theme

    let font: String
    let size: Double
    let tabWidth: Int
    let wraps: Bool
    let showsLineNumbers: Bool

    private static let lines = [
        "func healthCheck(_ req: Request) async throws -> Response {",
        "\tguard let db = req.application.database else {",
        "\t\treturn Response(status: .serviceUnavailable, body: \"no database\")",
        "\t}",
        "\treturn Response(status: .ok, body: try await db.ping())",
        "}",
    ]

    /// Size, weight and leading come from the token; only the *face* is overridden, because a
    /// user-chosen font is by definition not something `FormDesign` can name. An uninstalled
    /// name falls back to the token's own mono family, so a stale `settings.json` cannot
    /// produce an unreadable sample.
    private var style: TypeStyle { theme.typography.mono(size: size) }

    private var sampleFont: Font {
        guard NSFont(name: font, size: style.size) != nil else { return style.font }
        return .custom(font, fixedSize: style.size)
    }

    var body: some View {
        // A non-wrapping sample has to be able to run off the edge, or "wrap: off" would look
        // identical to "wrap: on".
        Group {
            if wraps {
                listing
            } else {
                ScrollView(.horizontal, showsIndicators: false) { listing }
            }
        }
    }

    private var listing: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(Self.lines.enumerated()), id: \.offset) { index, line in
                HStack(alignment: .firstTextBaseline, spacing: theme.metrics.spacing.lg) {
                    if showsLineNumbers {
                        Text("\(index + 1)")
                            .font(sampleFont)
                            .tabularFigures()
                            .foregroundStyle(theme.color.textTertiary)
                            .frame(width: style.size * 2, alignment: .trailing)
                    }
                    Text(expand(line))
                        .font(sampleFont)
                        .lineSpacing(style.lineSpacing)
                        .foregroundStyle(theme.color.textPrimary)
                        .lineLimit(wraps ? nil : 1)
                        .fixedSize(horizontal: !wraps, vertical: wraps)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(theme.metrics.spacing.lg)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                .fill(theme.color.surfaceRaised)
        )
    }

    /// Tabs are expanded here rather than left to the text system, whose tab stops are a
    /// property of the paragraph style and not of the token the user just picked.
    private func expand(_ line: String) -> String {
        line.replacingOccurrences(
            of: "\t", with: String(repeating: " ", count: max(1, tabWidth)))
    }
}

enum MonospacedFonts {
    /// The fixed-pitch families actually installed, with the token default first so the list
    /// always contains the value the core would fall back to.
    static let available: [String] = {
        let manager = NSFontManager.shared
        let fixed = manager.availableFontFamilies.filter { family in
            guard let font = NSFont(name: family, size: NSFont.systemFontSize) else { return false }
            return font.isFixedPitch
        }
        var names = [EditorDefaults.font]
        names.append(contentsOf: fixed.filter { $0 != EditorDefaults.font })
        return names
    }()
}

#Preview("Editor") {
    PreferencesTabPreview(tab: .editor)
}
