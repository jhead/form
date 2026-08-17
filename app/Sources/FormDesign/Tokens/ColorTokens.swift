import SwiftUI

/// Semantic color tokens. No token is named for its hue — a view asks for `textSecondary`,
/// never for a gray. Both themes define every key (spec 08 §2.1).
public struct ColorTokens: Sendable, Equatable, Codable {
    // Surfaces
    public var background: ThemeColor
    public var backgroundSidebar: ThemeColor
    public var surface: ThemeColor
    public var surfaceRaised: ThemeColor
    public var surfaceSelected: ThemeColor
    public var surfaceHover: ThemeColor
    /// Scrim behind sheets and the attachment overlay. Translucent by design.
    public var overlay: ThemeColor

    // Lines
    public var border: ThemeColor
    public var borderStrong: ThemeColor
    public var borderFocus: ThemeColor

    // Text
    public var textPrimary: ThemeColor
    public var textSecondary: ThemeColor
    public var textTertiary: ThemeColor
    /// Text drawn on a filled accent/semantic surface.
    public var textInverted: ThemeColor

    // Brand
    public var accent: ThemeColor
    public var accentHover: ThemeColor
    /// Low-contrast accent wash — find-match highlights, selected chips.
    public var accentMuted: ThemeColor

    // Semantic
    public var success: ThemeColor
    public var warning: ThemeColor
    public var danger: ThemeColor
    public var info: ThemeColor

    // Domain
    public var diffAdd: ThemeColor
    public var diffRemove: ThemeColor
    public var streaming: ThemeColor
    public var thinking: ThemeColor

    // Charts
    /// Eight series colors. Index is stable across every chart so a series keeps its
    /// identity dashboard-wide (spec 12 §3).
    public var chartSeries: [ThemeColor]
    public var chartGrid: ThemeColor
    public var chartAxis: ThemeColor
    /// Five intensity stops, ordered least → most intense.
    public var heatmapScale: [ThemeColor]

    public init(
        background: ThemeColor,
        backgroundSidebar: ThemeColor,
        surface: ThemeColor,
        surfaceRaised: ThemeColor,
        surfaceSelected: ThemeColor,
        surfaceHover: ThemeColor,
        overlay: ThemeColor,
        border: ThemeColor,
        borderStrong: ThemeColor,
        borderFocus: ThemeColor,
        textPrimary: ThemeColor,
        textSecondary: ThemeColor,
        textTertiary: ThemeColor,
        textInverted: ThemeColor,
        accent: ThemeColor,
        accentHover: ThemeColor,
        accentMuted: ThemeColor,
        success: ThemeColor,
        warning: ThemeColor,
        danger: ThemeColor,
        info: ThemeColor,
        diffAdd: ThemeColor,
        diffRemove: ThemeColor,
        streaming: ThemeColor,
        thinking: ThemeColor,
        chartSeries: [ThemeColor],
        chartGrid: ThemeColor,
        chartAxis: ThemeColor,
        heatmapScale: [ThemeColor]
    ) {
        self.background = background
        self.backgroundSidebar = backgroundSidebar
        self.surface = surface
        self.surfaceRaised = surfaceRaised
        self.surfaceSelected = surfaceSelected
        self.surfaceHover = surfaceHover
        self.overlay = overlay
        self.border = border
        self.borderStrong = borderStrong
        self.borderFocus = borderFocus
        self.textPrimary = textPrimary
        self.textSecondary = textSecondary
        self.textTertiary = textTertiary
        self.textInverted = textInverted
        self.accent = accent
        self.accentHover = accentHover
        self.accentMuted = accentMuted
        self.success = success
        self.warning = warning
        self.danger = danger
        self.info = info
        self.diffAdd = diffAdd
        self.diffRemove = diffRemove
        self.streaming = streaming
        self.thinking = thinking
        self.chartSeries = chartSeries
        self.chartGrid = chartGrid
        self.chartAxis = chartAxis
        self.heatmapScale = heatmapScale
    }

    /// Wraps, so a chart with more than eight series still renders.
    public func series(_ index: Int) -> ThemeColor {
        guard !chartSeries.isEmpty else { return accent }
        return chartSeries[((index % chartSeries.count) + chartSeries.count) % chartSeries.count]
    }

    /// `intensity` in 0…1 mapped onto the five heatmap stops.
    public func heatmap(_ intensity: Double) -> ThemeColor {
        guard !heatmapScale.isEmpty else { return accent }
        let clamped = min(1, max(0, intensity))
        let index = Int((clamped * Double(heatmapScale.count - 1)).rounded())
        return heatmapScale[index]
    }
}

// MARK: - Token roles, used by the contrast test and by primitives that switch on tone.

public extension ColorTokens {
    /// Every surface a token may legitimately be drawn on.
    var surfaces: [(name: String, color: ThemeColor)] {
        [
            ("background", background),
            ("backgroundSidebar", backgroundSidebar),
            ("surface", surface),
            ("surfaceRaised", surfaceRaised),
            ("surfaceSelected", surfaceSelected),
            ("surfaceHover", surfaceHover),
        ]
    }

    /// Text tokens that carry running prose and must clear WCAG AA body contrast.
    var bodyTextTokens: [(name: String, color: ThemeColor)] {
        [("textPrimary", textPrimary), ("textSecondary", textSecondary)]
    }

    /// Tokens used for de-emphasised text, glyphs, indicators and chart marks. These sit at
    /// the WCAG "large text / non-text contrast" bar (3:1) rather than the body bar.
    var accentedTokens: [(name: String, color: ThemeColor)] {
        var tokens: [(String, ThemeColor)] = [
            ("textTertiary", textTertiary),
            ("accent", accent),
            ("accentHover", accentHover),
            ("success", success),
            ("warning", warning),
            ("danger", danger),
            ("info", info),
            ("diffAdd", diffAdd),
            ("diffRemove", diffRemove),
            ("streaming", streaming),
            ("thinking", thinking),
            ("chartAxis", chartAxis),
        ]
        for (index, series) in chartSeries.enumerated() {
            tokens.append(("chartSeries[\(index)]", series))
        }
        return tokens
    }

    /// Fills that carry `textInverted`.
    var invertedTextBackings: [(name: String, color: ThemeColor)] {
        [
            ("accent", accent),
            ("accentHover", accentHover),
            ("success", success),
            ("warning", warning),
            ("danger", danger),
            ("info", info),
        ]
    }
}
