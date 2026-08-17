import SwiftUI

// Anchors from spec 08 §2.1. Everything else is derived to stay inside the contrast
// envelope asserted by `ContrastTests` — the surfaces sit close together because
// `textTertiary` has to clear 3:1 on the darkest of them.
private enum Light {
    static let background = ThemeColor(hex: "#FFFFFF")
    static let backgroundSidebar = ThemeColor(hex: "#FAFAF9")
    static let surface = ThemeColor(hex: "#FFFFFF")
    static let surfaceRaised = ThemeColor(hex: "#F2F1EF")
    static let surfaceHover = ThemeColor(hex: "#F5F3F0")
    static let surfaceSelected = ThemeColor(hex: "#EFECE7")

    static let border = ThemeColor(hex: "#E6E4E0")
    static let borderStrong = ThemeColor(hex: "#CFCBC4")

    static let textPrimary = ThemeColor(hex: "#1A1917")
    static let textSecondary = ThemeColor(hex: "#6B6862")
    // Spec anchors this at #9A968E, which measures 2.95:1 on pure white — below the 3:1
    // floor for any text at all. Darkened the minimum needed to clear 3:1 on every surface.
    static let textTertiary = ThemeColor(hex: "#8A867E")

    // Spec 08 §2.1 anchored the accent at #C15F3C, which carries white at only 4.23:1 —
    // and white-on-accent is the resting state of the primary button, the app's highest
    // traffic text-on-color pair. Darkened to the nearest value that clears 4.5:1 (4.71:1).
    // `accentHover` is a further step down the same ramp; `accentMuted` is the accent laid
    // over the background at 22 %.
    static let accent = ThemeColor(hex: "#BA5636")
    static let accentHover = ThemeColor(hex: "#9E4529")
    static let accentMuted = ThemeColor(hex: "#F0DAD3")

    static let success = ThemeColor(hex: "#3F7D4E")
    static let warning = ThemeColor(hex: "#A16A0D")
    static let danger = ThemeColor(hex: "#B4352F")
    static let info = ThemeColor(hex: "#2C6E9B")
    static let violet = ThemeColor(hex: "#6E63A0")
    static let teal = ThemeColor(hex: "#1F7A75")
    static let magenta = ThemeColor(hex: "#96436E")
    static let slate = ThemeColor(hex: "#5C6670")
}

public extension Theme {
    static let lightTheme = Theme(
        id: "light",
        color: ColorTokens(
            background: Light.background,
            backgroundSidebar: Light.backgroundSidebar,
            surface: Light.surface,
            surfaceRaised: Light.surfaceRaised,
            surfaceSelected: Light.surfaceSelected,
            surfaceHover: Light.surfaceHover,
            overlay: Light.textPrimary.opacity(0.32),
            border: Light.border,
            borderStrong: Light.borderStrong,
            borderFocus: Light.accent,
            textPrimary: Light.textPrimary,
            textSecondary: Light.textSecondary,
            textTertiary: Light.textTertiary,
            textInverted: ThemeColor(hex: "#FFFFFF"),
            accent: Light.accent,
            accentHover: Light.accentHover,
            accentMuted: Light.accentMuted,
            success: Light.success,
            warning: Light.warning,
            danger: Light.danger,
            info: Light.info,
            diffAdd: ThemeColor(hex: "#2E7D4F"),
            diffRemove: Light.danger,
            streaming: Light.accent,
            thinking: Light.violet,
            chartSeries: [
                Light.accent,
                Light.info,
                Light.success,
                Light.warning,
                Light.violet,
                Light.teal,
                Light.magenta,
                Light.slate,
            ],
            chartGrid: Light.border,
            chartAxis: Light.textSecondary,
            heatmapScale: [
                ThemeColor(hex: "#F5EFEA"),
                ThemeColor(hex: "#EBD6C8"),
                ThemeColor(hex: "#DDAF92"),
                ThemeColor(hex: "#CC8460"),
                ThemeColor(hex: "#B34F27"),
            ]
        ),
        syntax: SyntaxTokens(
            plain: Light.textPrimary,
            keyword: ThemeColor(hex: "#8B3A62"),
            string: ThemeColor(hex: "#2A6E46"),
            number: ThemeColor(hex: "#8A4B1E"),
            comment: ThemeColor(hex: "#85817A"),
            function: ThemeColor(hex: "#1F5FA8"),
            type: ThemeColor(hex: "#1F7A75"),
            variable: ThemeColor(hex: "#2E3440"),
            constant: ThemeColor(hex: "#7A3E9E"),
            operator: Light.textSecondary,
            punctuation: Light.textTertiary,
            attribute: ThemeColor(hex: "#A1541B"),
            invalid: Light.danger
        )
    )
}
