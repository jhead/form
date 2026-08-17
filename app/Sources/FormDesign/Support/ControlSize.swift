import SwiftUI

/// The three control heights every interactive primitive shares, so a button, a chip and a
/// text field sitting on one row line up without anyone hand-tuning a frame.
public enum FormControlSize: String, Sendable, CaseIterable {
    case small, medium, large

    public func height(_ metrics: MetricTokens) -> CGFloat {
        switch self {
        case .small: metrics.controlHeightSmall
        case .medium: metrics.controlHeightMedium
        case .large: metrics.controlHeightLarge
        }
    }

    public func horizontalPadding(_ metrics: MetricTokens) -> CGFloat {
        switch self {
        case .small: metrics.spacing.md
        case .medium: metrics.spacing.lg
        case .large: metrics.spacing.xl
        }
    }

    public func radius(_ metrics: MetricTokens) -> CGFloat {
        switch self {
        case .small: metrics.radius.md
        case .medium: metrics.radius.lg
        case .large: metrics.radius.lg
        }
    }

    public func typeStyle(_ typography: TypeTokens) -> TypeStyle {
        switch self {
        case .small: typography.micro
        case .medium: typography.uiMedium
        case .large: typography.uiMedium
        }
    }

    public func iconSize(_ metrics: MetricTokens) -> CGFloat {
        switch self {
        case .small: metrics.iconSmall
        case .medium: metrics.iconMedium
        case .large: metrics.iconMedium
        }
    }
}

/// Semantic tone shared by `Badge`, `Toast` and any other surface that comes in flavours.
public enum FormTone: String, Sendable, CaseIterable {
    case neutral, accent, success, warning, danger, info

    public func foreground(_ color: ColorTokens) -> ThemeColor {
        switch self {
        case .neutral: color.textSecondary
        case .accent: color.accent
        case .success: color.success
        case .warning: color.warning
        case .danger: color.danger
        case .info: color.info
        }
    }

    /// A wash of the tone, for chip and badge fills.
    public func background(_ color: ColorTokens) -> ThemeColor {
        switch self {
        case .neutral: color.surfaceRaised
        default: foreground(color).opacity(0.14)
        }
    }

    public var systemImage: String {
        switch self {
        case .neutral: "info.circle"
        case .accent: "sparkle"
        case .success: "checkmark.circle"
        case .warning: "exclamationmark.triangle"
        case .danger: "xmark.octagon"
        case .info: "info.circle"
        }
    }
}
