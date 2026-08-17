import Foundation
import Testing

@testable import FormDesign

@MainActor
struct ThemeControllerTests {
    @Test("explicit modes resolve to their theme")
    func explicitModes() {
        #expect(ThemeController(mode: .light).theme.id == "light")
        #expect(ThemeController(mode: .dark).theme.id == "dark")
    }

    @Test("system mode follows the current appearance")
    func systemMode() {
        let controller = ThemeController(mode: .system)
        let expected = ThemeController.systemIsDark() ? "dark" : "light"
        #expect(controller.theme.id == expected)
    }

    @Test("changing mode republishes the theme")
    func modeChangeRepublishes() {
        let controller = ThemeController(mode: .light)
        controller.setMode(.dark)
        #expect(controller.mode == .dark)
        #expect(controller.theme.id == "dark")
        #expect(controller.theme.color.background == Theme.dark.color.background)
    }

    @Test("text size steps through the ladder and clamps at both ends")
    func textSizeLadder() {
        let controller = ThemeController(mode: .light)
        #expect(controller.textScale == 1.0)

        controller.stepTextScale(1)
        #expect(controller.textScale == 1.1)
        #expect(controller.theme.typography.body.size == 14 * 1.1)

        for _ in 0 ..< 10 { controller.stepTextScale(1) }
        #expect(controller.textScale == TypeTokens.maximumScale)

        for _ in 0 ..< 20 { controller.stepTextScale(-1) }
        #expect(controller.textScale == TypeTokens.minimumScale)

        controller.resetTextScale()
        #expect(controller.textScale == 1.0)
        #expect(controller.theme.typography.body.size == 14)
    }

    @Test("out-of-range text sizes are clamped, not rejected")
    func textSizeClamps() {
        let controller = ThemeController(mode: .light)
        controller.setTextScale(4.0)
        #expect(controller.textScale == TypeTokens.maximumScale)
        controller.setTextScale(0.0)
        #expect(controller.textScale == TypeTokens.minimumScale)
    }

    /// `⌘⇧D` must always land somewhere concrete, including from `.system`.
    @Test("toggling appearance always produces an explicit mode")
    func toggleLeavesSystem() {
        #expect(ThemeMode.light.toggled == .dark)
        #expect(ThemeMode.dark.toggled == .light)
        #expect(ThemeMode.system.toggled != .system)
    }

    /// The Appearance tab binds a `Picker` and a `Slider` to these; a binding that wrote the
    /// stored property directly would leave `theme` stale.
    @Test("bindings rebuild the resolved theme, not just the stored value")
    func bindingsRebuildTheTheme() {
        let controller = ThemeController(mode: .light)

        controller.modeBinding.wrappedValue = .dark
        #expect(controller.mode == .dark)
        #expect(controller.theme.id == "dark")
        #expect(controller.modeBinding.wrappedValue == .dark)

        controller.textScaleBinding.wrappedValue = 1.2
        #expect(controller.textScale == 1.2)
        #expect(controller.theme.typography.body.size == 14 * 1.2)
        #expect(controller.textScaleBinding.wrappedValue == 1.2)

        // Clamping applies through the binding too, so a slider cannot exceed the range.
        controller.textScaleBinding.wrappedValue = 99
        #expect(controller.textScale == TypeTokens.maximumScale)
    }

    @Test("the published ladder is what the keyboard steps through")
    func ladderMatchesStepping() {
        let ladder = ThemeController.textScaleLadder
        #expect(ladder.first == TypeTokens.minimumScale)
        #expect(ladder.last == TypeTokens.maximumScale)
        #expect(ladder.contains(1.0))
        #expect(zip(ladder, ladder.dropFirst()).allSatisfy { $0 < $1 })

        let controller = ThemeController(mode: .light)
        controller.setTextScale(ladder[0])
        for expected in ladder.dropFirst() {
            controller.stepTextScale(1)
            #expect(controller.textScale == expected)
        }
    }

    @Test("the resolved theme carries a device-pixel hairline")
    func hairlineIsResolved() {
        let controller = ThemeController(mode: .light)
        #expect(controller.theme.metrics.hairline > 0)
        #expect(controller.theme.metrics.hairline <= 1.0)
    }

    @Test("mode is persistable as a plain string")
    func modeIsCodable() throws {
        for mode in ThemeMode.allCases {
            let data = try JSONEncoder().encode(mode)
            #expect(try JSONDecoder().decode(ThemeMode.self, from: data) == mode)
        }
    }
}
