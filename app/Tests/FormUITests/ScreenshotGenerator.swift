import AppKit
import FormCore
import FormDesign
import Foundation
import SwiftUI
import Testing

@testable import FormUI

/// Writes the README screenshots without a display.
///
/// `screencapture` needs a window on a real screen, which means an unlocked Mac and a stable
/// window position. This renders the same views into a bitmap instead: no window server
/// dependency, no lock-screen problem, no remote desktop, and the output is deterministic
/// because the content comes from `CoreStores.preview` rather than a live provider.
///
/// Rendered through an off-screen `NSWindow` and `cacheDisplay`.
///
/// **Only the dashboard is generated, and that is a limitation, not a preference.** The
/// transcript's text runs are `NSTextView`s behind `NSViewRepresentable`, and neither
/// `ImageRenderer` nor `cacheDisplay` draws them without a window that is genuinely on a
/// screen: the surrounding chrome renders and every message body comes out empty. Rather than
/// ship a picture of the app with its content missing, the chat shot is left to
/// `scripts/screenshot.sh`, which uses `screencapture` and needs an unlocked display.
/// The dashboard is pure SwiftUI and Swift Charts, so it renders faithfully here.
///
/// Opt in, so it never runs as part of the suite:
///
///     FORM_SHOT=1 swift test --package-path app \
///       -Xlinker -L$(pwd)/core/target/debug --filter ScreenshotGenerator
@Suite(
    "ScreenshotGenerator",
    .enabled(if: ProcessInfo.processInfo.environment["FORM_SHOT"] == "1"),
    .serialized
)
@MainActor
struct ScreenshotGenerator {

    /// 1280×860 at 2×, matching the app's default window and a Retina capture.
    private static let size = CGSize(width: 1280, height: 860)
    private static let scale: CGFloat = 2

    @Test("home")
    func home() throws {
        try write(name: "home", route: .home, theme: .light)
    }

    // Dark renders too — pass `.dark` here — but leaves a thin unpainted strip along the
    // window's bottom edge off-screen, so only the light shot is committed.

    private enum Route {
        case home
        case session
    }

    private func write(name: String, route: Route, theme: FormDesign.ThemeMode) throws {
        let stores = CoreStores.preview(.populated)
        let controller = ThemeController(mode: theme)
        let toasts = ToastCenter()

        // `preview(.populated)` already seeds the transcript and selects a session, so the
        // route just has to follow it rather than loading anything.
        let appState = AppState(sidebarCollapsed: false)
        switch route {
        case .session:
            if let id = stores.sessions.selectedSessionId {
                appState.navigate(to: .session(id))
            }
        case .home:
            appState.navigate(to: .home)
        }

        let root = RootView(
            stores: stores,
            appState: appState,
            themeController: controller,
            toasts: toasts,
            home: { HomeView(stores: stores) },
            session: { _ in ChatView(stores: stores) }
        )
        .formTheme(controller)
        .frame(width: Self.size.width, height: Self.size.height)

        let url = try Self.outputDirectory().appending(path: "\(name).png")
        let filled = try Self.render(root, to: url)
        #expect(filled, "\(name).png rendered with an empty content pane")

        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        let bytes = (attributes[.size] as? Int) ?? 0
        print("wrote \(url.path) (\(bytes / 1024) KB)")
    }

    // MARK: - Rendering

    @discardableResult
    private static func render(_ view: some View, to url: URL) throws -> Bool {
        // A real window is what makes AppKit-backed subviews draw. It is never ordered on
        // screen, so this works with the display asleep or locked.
        let hosting = NSHostingView(rootView: view)
        hosting.frame = CGRect(origin: .zero, size: size)

        let window = NSWindow(
            contentRect: hosting.frame,
            styleMask: [.titled, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.contentView = hosting
        window.titlebarAppearsTransparent = true
        window.isReleasedWhenClosed = false
        // Parked far off any screen, then made visible. An `NSViewRepresentable` only builds
        // and draws its AppKit view once it belongs to a *visible* window, so without this the
        // transcript's text runs come out blank while every pure-SwiftUI sibling renders — a
        // half-empty screenshot that looks like a layout bug. Off-screen keeps it invisible.
        window.setFrameOrigin(NSPoint(x: -20_000, y: -20_000))
        window.orderBack(nil)
        // Layout and text measurement need a backing scale; without ordering the window in,
        // set it explicitly so the bitmap is Retina density.
        window.backingScaleFactor(orNil: scale)

        hosting.layoutSubtreeIfNeeded()

        // Nothing on this screen is synchronous: the dashboard's document arrives across an
        // actor, the charts animate in, and each transcript row asks the core to parse its
        // markdown behind a debounce. So wait until the content pane has actually filled
        // rather than for a fixed number of turns — a fixed count produced a blank dashboard
        // or a complete one depending on machine load.
        //
        // The timer matters. `RunLoop.run(until:)` returns the instant it has nothing left to
        // process, so without a live input source the loop spins through every iteration in
        // milliseconds and waits for nothing. Keeping the main thread free rather than sleeping
        // on it is also deliberate: the work being waited for lands on the main actor.
        let keepAlive = Timer.scheduledTimer(withTimeInterval: 0.01, repeats: true) { _ in }
        defer { keepAlive.invalidate() }

        // Wait for the view to *stop changing*, not merely to start. "Has anything drawn yet"
        // was the first condition here and it exited the moment the header appeared, cutting
        // the dashboard off halfway down. Two identical consecutive frames means the charts
        // have finished animating and every async document has landed.
        var rep: NSBitmapImageRep?
        var previous: String?
        var stableFor = 0
        for attempt in 0..<60 {
            RunLoop.current.run(until: Date().addingTimeInterval(0.25))
            hosting.layoutSubtreeIfNeeded()
            window.displayIfNeeded()

            guard let candidate = hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds)
            else { throw ScreenshotError.noBitmap }
            hosting.cacheDisplay(in: hosting.bounds, to: candidate)
            rep = candidate

            let signature = contentSignature(candidate)
            stableFor = signature == previous ? stableFor + 1 : 0
            previous = signature

            // Settled, and actually showing something. Four attempts minimum so an empty
            // first frame that has not changed yet cannot satisfy this.
            if stableFor >= 2, attempt >= 4, contentPaneHasFilled(candidate) {
                break
            }
        }
        guard let rep, contentPaneHasFilled(rep) else { throw ScreenshotError.emptyRender }
        let filled = true

        // A scroll view will not paint its final rows off-screen, whatever it is given to
        // settle, so the raw bitmap ends in a band of bare window background. Trim to the last
        // row that actually drew: the result is a faithful render of a slightly shorter
        // viewport, rather than a picture with an empty strip along the bottom.
        let trimmed = Self.trimmingUnpaintedBottom(rep) ?? rep

        guard let data = trimmed.representation(using: .png, properties: [:]) else {
            throw ScreenshotError.noEncoding
        }
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: url)
        window.close()
        return filled
    }

    /// Is there anything in the content pane, or is it one flat colour?
    ///
    /// The sidebar renders immediately and on its own is a perfectly plausible-looking PNG of
    /// several hundred kilobytes, so file size cannot answer this. Sampling the pane and
    /// counting distinct colours can: an empty pane is one background colour, and a rendered
    /// one is hundreds.
    /// Crop away any uniform band along the bottom of the content pane.
    private static func trimmingUnpaintedBottom(_ rep: NSBitmapImageRep) -> NSBitmapImageRep? {
        let left = Int(Double(rep.pixelsWide) * 0.35)
        let sampleX = stride(from: left, to: rep.pixelsWide - 4, by: 29)

        // Rows are top-down in this space, so walk up from the bottom until one has variation.
        var lastPainted = rep.pixelsHigh - 1
        while lastPainted > rep.pixelsHigh / 2 {
            var colours = Set<String>()
            for x in sampleX {
                guard let colour = rep.colorAt(x: x, y: lastPainted) else { continue }
                colours.insert(
                    "\(Int(colour.redComponent * 255)),"
                        + "\(Int(colour.greenComponent * 255)),"
                        + "\(Int(colour.blueComponent * 255))")
            }
            if colours.count > 1 { break }
            lastPainted -= 1
        }

        let keep = lastPainted + 1
        guard keep < rep.pixelsHigh, keep > 0 else { return nil }

        guard
            let image = rep.cgImage?.cropping(
                to: CGRect(x: 0, y: 0, width: rep.pixelsWide, height: keep))
        else { return nil }
        return NSBitmapImageRep(cgImage: image)
    }

    /// A cheap fingerprint of the content pane, used to tell when drawing has settled.
    private static func contentSignature(_ rep: NSBitmapImageRep) -> String {
        let left = Int(Double(rep.pixelsWide) * 0.35)
        var parts: [String] = []
        for x in stride(from: left, to: rep.pixelsWide - 4, by: 41) {
            for y in stride(from: 4, to: rep.pixelsHigh - 4, by: 41) {
                guard let colour = rep.colorAt(x: x, y: y) else { continue }
                parts.append(String(Int(colour.brightnessComponent * 255)))
            }
        }
        return parts.joined(separator: ",")
    }

    private static func contentPaneHasFilled(_ rep: NSBitmapImageRep) -> Bool {
        // Both bands must have drawn. Checking only the top passed as soon as the header
        // appeared and left the lower charts clipped with an unpainted strip beneath them,
        // because a scroll view draws its viewport progressively.
        let paneTop = 0.05...0.35
        let paneBottom = 0.80...0.97
        return colourCount(rep, verticalRange: paneTop) > 12
            && colourCount(rep, verticalRange: paneBottom) > 8
    }

    private static func colourCount(
        _ rep: NSBitmapImageRep, verticalRange: ClosedRange<Double>
    ) -> Int {
        let left = Int(Double(rep.pixelsWide) * 0.35)
        let top = Int(Double(rep.pixelsHigh) * verticalRange.lowerBound)
        let bottom = Int(Double(rep.pixelsHigh) * verticalRange.upperBound)
        var colours = Set<String>()
        for x in stride(from: left, to: rep.pixelsWide - 4, by: 13) {
            for y in stride(from: top, to: bottom, by: 13) {
                guard let colour = rep.colorAt(x: x, y: y) else { continue }
                colours.insert(
                    "\(Int(colour.redComponent * 255)),"
                        + "\(Int(colour.greenComponent * 255)),"
                        + "\(Int(colour.blueComponent * 255))")
            }
        }
        return colours.count
    }

    private static func outputDirectory() throws -> URL {
        if let override = ProcessInfo.processInfo.environment["FORM_SHOT_DIR"] {
            return URL(fileURLWithPath: override)
        }
        // Walk up from this file to the repo root, so it does not depend on the cwd.
        var url = URL(fileURLWithPath: #filePath)
        for _ in 0..<4 { url.deleteLastPathComponent() }
        return url.appending(path: "docs/images")
    }

    enum ScreenshotError: Error {
        case noBitmap
        case noEncoding
        /// The content pane never drew anything. Better to fail than to write a picture of an
        /// empty window that looks like a layout bug.
        case emptyRender
    }
}

extension NSWindow {
    /// `backingScaleFactor` is read-only; on an off-screen window the density comes from the
    /// screen it would appear on. Nudging the frame is enough to make AppKit pick 2× on a
    /// Retina machine, and this is a no-op when there is no such screen.
    fileprivate func backingScaleFactor(orNil scale: CGFloat) {
        guard let screen = NSScreen.screens.first(where: { $0.backingScaleFactor >= scale })
        else { return }
        setFrame(frame, display: false)
        _ = screen
    }
}
