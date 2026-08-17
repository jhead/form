import CoreGraphics
import FormCore
import FormDesign
import SwiftUI

/// The knobs a *caller* controls. Everything typographic — sizes, spacing, code font, list
/// indents — is derived from `Theme` in `MarkdownMetrics`, which is why this struct is so
/// small: spec 11 §1 says the style comes from the theme, so the only things left here are
/// the user's Editor preferences (spec 13) and the two suppressions the chat transcript
/// needs (a user bubble wants neither a copy button nor its own selection run).
public struct MarkdownStyle: Sendable, Equatable {
    /// `editor.showLineNumbers`.
    public var showLineNumbers: Bool
    /// `editor.wrapCode`. Off means the code block scrolls horizontally instead.
    public var wrapCode: Bool
    /// `editor.fontSize`, when the user has picked one. `nil` keeps the theme's code size.
    public var codeFontSize: CGFloat?
    /// Suppressed for a code block that is still streaming, and for compact contexts.
    public var showsCopyButton: Bool
    /// Off falls back to plain SwiftUI `Text`, which is cheaper and cannot be selected.
    public var isSelectable: Bool

    public static let `default` = MarkdownStyle()

    public init(
        showLineNumbers: Bool = false,
        wrapCode: Bool = false,
        codeFontSize: CGFloat? = nil,
        showsCopyButton: Bool = true,
        isSelectable: Bool = true
    ) {
        self.showLineNumbers = showLineNumbers
        self.wrapCode = wrapCode
        self.codeFontSize = codeFontSize
        self.showsCopyButton = showsCopyButton
        self.isSelectable = isSelectable
    }

    /// Built from the settings document the core echoes back, so the Editor pane's two
    /// markdown-facing switches take effect without a restart (F9.2).
    public init(editor: EditorSettings?, showsCopyButton: Bool = true, isSelectable: Bool = true) {
        self.init(
            showLineNumbers: editor?.showLineNumbers ?? false,
            wrapCode: editor?.wrapCode ?? false,
            codeFontSize: editor?.fontSize.map(CGFloat.init),
            showsCopyButton: showsCopyButton,
            isSelectable: isSelectable
        )
    }
}

/// Every measurement the renderer uses, resolved once from `Theme` + `MarkdownStyle`.
///
/// This type exists so no view in this module ever reaches for a number: it is the single
/// translation from design tokens into markdown-specific geometry, and it is `Equatable` so
/// the render cache can key on it.
struct MarkdownMetrics: Equatable {
    let theme: Theme
    let style: MarkdownStyle

    init(theme: Theme, style: MarkdownStyle) {
        self.theme = theme
        self.style = style
    }

    // MARK: Identity

    /// Cache key component. Two metrics that render identically must produce the same
    /// string, and any token change must produce a different one.
    var cacheKey: String {
        "\(theme.id)/\(theme.typography.scale)/\(style.codeFontSize.map { "\($0)" } ?? "-")"
    }

    // MARK: Type

    var body: TypeStyle { theme.typography.body }
    var codeInline: TypeStyle { theme.typography.codeInline }

    var code: TypeStyle {
        guard let size = style.codeFontSize else { return theme.typography.code }
        return theme.typography.mono(size: size)
    }

    /// The heading ladder is the type scale read downwards — `h1` is the largest style the
    /// transcript column is willing to carry, not a multiple of the body size. Levels past
    /// six clamp, because the core will never emit one but the wire allows it.
    func heading(level: Int) -> TypeStyle {
        switch max(1, min(6, level)) {
        case 1: theme.typography.title
        case 2: theme.typography.heading
        case 3: theme.typography.bodyStrong
        case 4: theme.typography.uiMedium.weighted(.semibold)
        case 5: theme.typography.caption.weighted(.semibold)
        default: theme.typography.micro.weighted(.semibold)
        }
    }

    // MARK: Spacing

    /// Gap between two top-level blocks.
    var blockSpacing: CGFloat { theme.metrics.spacing.lg }
    /// Extra air above a heading, so a section reads as a break rather than a bold line.
    var headingLeading: CGFloat { theme.metrics.spacing.md }
    /// Gap between the items of a tight list.
    var listItemSpacing: CGFloat { theme.metrics.spacing.xxs }
    /// One level of list nesting.
    var listIndent: CGFloat { theme.metrics.spacing.xxl }
    var quoteInset: CGFloat { theme.metrics.spacing.lg }
    var quoteRule: CGFloat { theme.metrics.quoteRuleWidth }
    var codePadding: CGFloat { theme.metrics.spacing.lg }
    var codeHeaderHeight: CGFloat { theme.metrics.controlHeightMedium }
    var cellPaddingH: CGFloat { theme.metrics.spacing.lg }
    var cellPaddingV: CGFloat { theme.metrics.spacing.sm }
    var imageMaxHeight: CGFloat { theme.metrics.imageMaxHeight }
    var radius: CGFloat { theme.metrics.radius.lg }
    var hairline: CGFloat { theme.metrics.hairline }

    /// The inline-code chip's inset, drawn by `MarkdownLayoutManager` rather than by an
    /// attribute, because `NSAttributedString` backgrounds are tight to the glyphs.
    var chipInsetH: CGFloat { theme.metrics.spacing.xs }
    var chipInsetV: CGFloat { theme.metrics.spacing.xxs }
    var chipRadius: CGFloat { theme.metrics.radius.sm }

    /// How long the copy button holds its checkmark (spec 11 §2).
    var copyConfirmation: Double { theme.motion.seconds(.pulse) }

    // MARK: Markers

    /// Bullet by depth (spec 11 §2), cycling past the third level.
    static let bullets = ["•", "◦", "▪"]

    func bullet(depth: Int) -> String {
        Self.bullets[max(0, depth) % Self.bullets.count]
    }

    /// Non-interactive task boxes. Glyphs rather than a control, because these live inside
    /// the text run and must select and copy as text.
    static func checkbox(_ checked: Bool) -> String { checked ? "☑" : "☐" }
}
