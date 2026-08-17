import SwiftUI

// Anchors from spec 08 §2.1. `surfaceSelected` is warm-shifted rather than lifted: the
// anchored `textTertiary` (#736E66) only clears 3:1 up to about L=0.019, so selection reads
// as a hue change against `backgroundSidebar` instead of a brightness step.
private enum Dark {
    static let background = ThemeColor(hex: "#1A1917")
    static let backgroundSidebar = ThemeColor(hex: "#131211")
    static let surface = ThemeColor(hex: "#201E1C")
    static let surfaceRaised = ThemeColor(hex: "#262421")
    static let surfaceHover = ThemeColor(hex: "#232120")
    static let surfaceSelected = ThemeColor(hex: "#2B2320")

    static let border = ThemeColor(hex: "#33302C")
    static let borderStrong = ThemeColor(hex: "#4A453F")

    static let textPrimary = ThemeColor(hex: "#F5F3EF")
    static let textSecondary = ThemeColor(hex: "#A8A39B")
    static let textTertiary = ThemeColor(hex: "#736E66")

    static let accent = ThemeColor(hex: "#D97757")
    static let accentHover = ThemeColor(hex: "#E68B6C")
    static let accentMuted = ThemeColor(hex: "#4A2A20")

    static let success = ThemeColor(hex: "#6FBF84")
    static let warning = ThemeColor(hex: "#E0A44A")
    static let danger = ThemeColor(hex: "#E4756B")
    static let info = ThemeColor(hex: "#6FA8D8")
    static let violet = ThemeColor(hex: "#A79BD6")
    static let teal = ThemeColor(hex: "#5FC2BC")
    static let pink = ThemeColor(hex: "#D889B6")
    static let slate = ThemeColor(hex: "#9AA4B0")
}

public extension Theme {
    static let darkTheme = Theme(
        id: "dark",
        color: ColorTokens(
            background: Dark.background,
            backgroundSidebar: Dark.backgroundSidebar,
            surface: Dark.surface,
            surfaceRaised: Dark.surfaceRaised,
            surfaceSelected: Dark.surfaceSelected,
            surfaceHover: Dark.surfaceHover,
            overlay: ThemeColor(hex: "#000000").opacity(0.5),
            border: Dark.border,
            borderStrong: Dark.borderStrong,
            borderFocus: Dark.accent,
            textPrimary: Dark.textPrimary,
            textSecondary: Dark.textSecondary,
            textTertiary: Dark.textTertiary,
            textInverted: Dark.background,
            accent: Dark.accent,
            accentHover: Dark.accentHover,
            accentMuted: Dark.accentMuted,
            success: Dark.success,
            warning: Dark.warning,
            danger: Dark.danger,
            info: Dark.info,
            diffAdd: ThemeColor(hex: "#63C68C"),
            diffRemove: Dark.danger,
            streaming: Dark.accent,
            thinking: Dark.violet,
            chartSeries: [
                Dark.accent,
                Dark.info,
                Dark.success,
                Dark.warning,
                Dark.violet,
                Dark.teal,
                Dark.pink,
                Dark.slate,
            ],
            chartGrid: Dark.border,
            chartAxis: Dark.textSecondary,
            heatmapScale: [
                ThemeColor(hex: "#2A2724"),
                ThemeColor(hex: "#4A3128"),
                ThemeColor(hex: "#7A4832"),
                ThemeColor(hex: "#AD6042"),
                ThemeColor(hex: "#D97757"),
            ]
        ),
        syntax: SyntaxTokens(
            plain: Dark.textPrimary,
            keyword: ThemeColor(hex: "#E091C0"),
            string: ThemeColor(hex: "#8FCF9E"),
            number: ThemeColor(hex: "#E5B77A"),
            comment: ThemeColor(hex: "#8A857B"),
            function: ThemeColor(hex: "#7FB6E8"),
            type: ThemeColor(hex: "#6FC9C2"),
            variable: ThemeColor(hex: "#E8E4DC"),
            constant: ThemeColor(hex: "#C4A6F0"),
            operator: Dark.textSecondary,
            punctuation: Dark.textTertiary,
            attribute: ThemeColor(hex: "#D9AE8C"),
            invalid: Dark.danger
        )
    )
}
