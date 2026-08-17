import Testing

@testable import FormDesign

/// F6.5 — reduce-motion is satisfied once, here, rather than in every view. These tests are
/// the thing that keeps that true.
@MainActor
struct MotionTests {
    /// Restores the real system setting so one test cannot leak into another.
    private func withReduceMotion(_ enabled: Bool, _ body: () -> Void) {
        let previous = ReduceMotion.override
        ReduceMotion.override = enabled
        defer { ReduceMotion.override = previous }
        body()
    }

    @Test("every duration and curve yields an animation when motion is allowed")
    func animationsExistNormally() {
        withReduceMotion(false) {
            let motion = Theme.light.motion
            for duration in MotionDuration.allCases {
                for curve in MotionCurve.allCases {
                    let animation = motion.animation(duration, curve: curve)
                    if duration == .instant {
                        #expect(animation == nil, "instant must not animate, whatever the curve")
                    } else {
                        #expect(animation != nil, "\(duration)/\(curve) should animate")
                    }
                }
            }
        }
    }

    @Test("reduce-motion returns nil for every duration and curve")
    func reduceMotionSuppressesEverything() {
        withReduceMotion(true) {
            let motion = Theme.light.motion
            for duration in MotionDuration.allCases {
                for curve in MotionCurve.allCases {
                    #expect(motion.animation(duration, curve: curve) == nil, "\(duration)/\(curve) leaked an animation")
                }
                #expect(motion.repeating(duration) == nil, "\(duration) leaked a repeating animation")
            }
            #expect(motion.isReduced)
        }
    }

    @Test("both themes carry the same motion scale")
    func durationsMatchTheSpec() {
        for kind in ThemeKind.allCases {
            let motion = kind.theme.motion
            #expect(motion.seconds(.instant) == 0.0)
            #expect(motion.seconds(.fast) == 0.12)
            #expect(motion.seconds(.normal) == 0.2)
            #expect(motion.seconds(.slow) == 0.32)
            #expect(motion.seconds(.pulse) == 1.2)
            #expect(motion.springResponse == 0.35)
            #expect(motion.springDamping == 0.82)
        }
    }

    @Test("the instant duration is a no-op, not a one-frame animation")
    func instantIsNil() {
        withReduceMotion(false) {
            #expect(Theme.light.motion.animation(.instant) == nil)
        }
    }
}
