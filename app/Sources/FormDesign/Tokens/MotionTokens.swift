import AppKit
import SwiftUI

public enum MotionDuration: String, Sendable, Codable, CaseIterable {
    case instant, fast, normal, slow, pulse
}

public enum MotionCurve: String, Sendable, Codable, CaseIterable {
    case standard    // .easeOut
    case emphasized  // spring
    case linear      // for continuous loops — shimmer, indeterminate bars
}

/// Reduce-motion state, in one place so `MotionTokens.animation(_:)` is the only thing that
/// reads it. Tests drive `override` rather than the user's real system setting.
@MainActor
public enum ReduceMotion {
    public static var override: Bool?

    public static var isEnabled: Bool {
        override ?? NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    }
}

/// Durations and curves (spec 08 §2.4).
///
/// **Every animation in the app is constructed here.** `animation(_:)` returns `nil` when
/// the user has asked for reduced motion, and SwiftUI treats a `nil` animation as "apply the
/// change immediately" — which is how F6.5 is satisfied globally instead of view by view.
public struct MotionTokens: Sendable, Equatable, Codable {
    public var instant: Double = 0.0
    public var fast: Double = 0.12
    public var normal: Double = 0.2
    public var slow: Double = 0.32
    public var pulse: Double = 1.2

    public var springResponse: Double = 0.35
    public var springDamping: Double = 0.82

    public static let standard = MotionTokens()
    public init() {}

    public func seconds(_ duration: MotionDuration) -> Double {
        switch duration {
        case .instant: instant
        case .fast: fast
        case .normal: normal
        case .slow: slow
        case .pulse: pulse
        }
    }

    /// The only sanctioned way to build an `Animation`.
    @MainActor
    public func animation(_ duration: MotionDuration = .normal, curve: MotionCurve = .standard) -> Animation? {
        guard !ReduceMotion.isEnabled else { return nil }
        let seconds = seconds(duration)
        guard seconds > 0 else { return nil }
        switch curve {
        case .standard: return .easeOut(duration: seconds)
        case .linear: return .linear(duration: seconds)
        case .emphasized: return .spring(response: springResponse, dampingFraction: springDamping)
        }
    }

    /// A looping animation for pulses, shimmers and indeterminate bars. `nil` under
    /// reduce-motion, so the caller's driving value simply never changes.
    @MainActor
    public func repeating(
        _ duration: MotionDuration = .pulse,
        curve: MotionCurve = .linear,
        autoreverses: Bool = true
    ) -> Animation? {
        animation(duration, curve: curve)?.repeatForever(autoreverses: autoreverses)
    }

    /// True when motion should be suppressed. Views use this to pick a static presentation
    /// (a filled dot instead of a pulse), not to build their own animations.
    @MainActor
    public var isReduced: Bool { ReduceMotion.isEnabled }
}
