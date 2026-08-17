import AppKit
import SwiftUI

/// The spacing ladder (spec 08 §2.3): 2, 4, 6, 8, 12, 16, 20, 24, 32, 40, 48.
public struct SpacingScale: Sendable, Equatable, Codable {
    public var xxs: CGFloat = 2
    public var xs: CGFloat = 4
    public var sm: CGFloat = 6
    public var md: CGFloat = 8
    public var lg: CGFloat = 12
    public var xl: CGFloat = 16
    public var xxl: CGFloat = 20
    public var xxxl: CGFloat = 24
    // Past `xxxl` the x-prefixes stop being readable, so the top of the ladder is numbered.
    public var xl2: CGFloat = 32
    public var xl3: CGFloat = 40
    public var xl4: CGFloat = 48

    public static let standard = SpacingScale()
    public init() {}

    public var all: [CGFloat] { [xxs, xs, sm, md, lg, xl, xxl, xxxl, xl2, xl3, xl4] }
}

/// Corner radii (spec 08 §2.3).
public struct RadiusScale: Sendable, Equatable, Codable {
    public var sm: CGFloat = 4
    public var md: CGFloat = 6
    public var lg: CGFloat = 8
    public var xl: CGFloat = 12
    public var pill: CGFloat = 999

    public static let standard = RadiusScale()
    public init() {}
}

/// Every number a view is allowed to lay out with.
public struct MetricTokens: Sendable, Equatable, Codable {
    public var spacing: SpacingScale = .standard
    public var radius: RadiusScale = .standard

    /// One device pixel. Set from the active screen by `ThemeController`; the default is
    /// the Retina value because that is what every Mac this ships on has.
    public var hairline: CGFloat = 0.5

    // Shell
    public var sidebarWidth: CGFloat = 300
    public var sidebarMinWidth: CGFloat = 220
    public var sidebarMaxWidth: CGFloat = 420
    public var sidebarRowHeight: CGFloat = 32
    public var navRowHeight: CGFloat = 34
    public var sectionHeaderHeight: CGFloat = 24
    public var emptyGroupRowHeight: CGFloat = 32
    public var headerHeight: CGFloat = 44
    /// Leading space the traffic lights need in the first sidebar row.
    public var trafficLightInset: CGFloat = 78
    public var windowMinWidth: CGFloat = 900
    public var windowMinHeight: CGFloat = 600

    // Content
    public var contentMaxWidth: CGFloat = 720
    public var composerMaxWidth: CGFloat = 680
    public var composerMaxLines: Int = 12
    public var dashboardMaxWidth: CGFloat = 1100
    /// User bubbles cap at this fraction of the transcript column (F1.2).
    public var messageMaxWidthFraction: CGFloat = 0.72

    // Controls
    public var iconButton: CGFloat = 28
    public var avatar: CGFloat = 24
    public var chipHeight: CGFloat = 24
    public var toolRowHeight: CGFloat = 28
    public var controlHeightSmall: CGFloat = 22
    public var controlHeightMedium: CGFloat = 28
    public var controlHeightLarge: CGFloat = 34
    public var segmentedHeight: CGFloat = 34
    public var focusRing: CGFloat = 3

    // Glyphs
    public var iconSmall: CGFloat = 13
    public var iconMedium: CGFloat = 15
    public var iconLarge: CGFloat = 18

    // Indicators
    public var statusDot: CGFloat = 6
    public var contextRing: CGFloat = 14
    public var ringLineWidth: CGFloat = 2
    public var progressBarHeight: CGFloat = 3
    public var caretWidth: CGFloat = 2

    // Numbers the consuming specs name explicitly. They live here because `FormDesign` is
    // the only module allowed to hold a layout literal — not because they are general.
    /// Insertion line while dragging a session between groups (spec 09 §3).
    public var dropIndicator: CGFloat = 2
    /// Leading rule on a blockquote (spec 11 §2).
    public var quoteRuleWidth: CGFloat = 2
    /// Zebra striping on table rows, as a tint over `surface` (spec 11 §2).
    public var zebraOpacity: Double = 0.03
    /// Cap on an inline image so streaming does not reflow the transcript (spec 11 §2).
    public var imageMaxHeight: CGFloat = 400
    /// Attachment tray chip and its thumbnail (spec 13, Part B).
    public var attachmentChipHeight: CGFloat = 56
    public var thumbnail: CGFloat = 40
    /// Command palette panel (spec 14 §3).
    public var paletteWidth: CGFloat = 640
    public var paletteTopFraction: CGFloat = 0.2

    // Containers
    public var popoverPadding: CGFloat = 12
    public var popoverRadius: CGFloat = 10
    public var popoverMaxWidth: CGFloat = 320
    public var sheetWidth: CGFloat = 720
    public var sheetHeight: CGFloat = 520
    public var toastWidth: CGFloat = 320

    public static let standard = MetricTokens()
    public init() {}

    /// One device pixel on the screen a window is actually on.
    @MainActor
    public static func currentHairline() -> CGFloat {
        let scale = NSScreen.main?.backingScaleFactor ?? 2
        return scale > 0 ? 1 / scale : 0.5
    }
}
