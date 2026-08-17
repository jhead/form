import FormCore
import FormDesign
import Foundation
import Testing

@testable import FormMarkdown

/// Spec 11 §5: "a fixture document exercising every block and span type renders correctly in
/// both themes, with a snapshot test".
///
/// Goldens live next to this file in `Tests/FormMarkdownTests/.snapshots/`. The leading dot
/// is not decoration: SwiftPM warns about any file in a target directory it has no rule for,
/// and this module cannot declare them as resources because `Package.swift` is frozen — a
/// hidden directory is skipped instead, which keeps `make test` warning-free. Regenerate with
/// `FORM_UPDATE_SNAPSHOTS=1 swift test`, which mirrors the env-flag convention the core's
/// markdown goldens use (spec 05 §6). Review the diff — a golden that changed because a
/// token changed is fine; one that changed because a color stopped coming from the theme is
/// the regression this test exists to catch.
@MainActor
struct MarkdownSnapshotTests {
    /// Hidden so SwiftPM does not warn about unhandled files in the test target.
    static let goldenDirectory = ".snapshots"

    /// Parameterised on the theme *id* rather than the theme: `Theme` has no short
    /// description, and a failing case that prints the entire token set is unreadable.
    @Test("the fixture renders correctly in both themes", arguments: Theme.all.map(\.id))
    func snapshot(themeId: String) throws {
        let theme = try #require(Theme.all.first { $0.id == themeId })
        let actual = MarkdownSnapshot.describe(MarkdownFixture.everything, theme: theme)
        try assertSnapshot(named: "everything-\(theme.id)", actual: actual)
    }

    @Test("line numbers and soft wrap change the rendering, not the content")
    func editorSettingsSnapshot() throws {
        let style = MarkdownStyle(showLineNumbers: true, wrapCode: true)
        let actual = MarkdownSnapshot.describe(
            MarkdownFixture.codeOnly, theme: .dark, style: style)
        try assertSnapshot(named: "code-dark-wrapped", actual: actual)
    }

    @Test("the two themes agree on structure and disagree on color")
    func themesDifferOnlyInColor() {
        let light = MarkdownSnapshot.describe(MarkdownFixture.everything, theme: .light)
        let dark = MarkdownSnapshot.describe(MarkdownFixture.everything, theme: .dark)

        #expect(light != dark, "the themes must not render identically")
        #expect(
            structure(light) == structure(dark),
            "a theme switch must not move anything — only recolor it")
        #expect(Set(colors(light)) != Set(colors(dark)))
    }

    /// Acceptance criterion 7, from the render side: every color the renderer emits is a
    /// token off the active theme. The `NoHardcodedColors` lint catches a literal in the
    /// source; this catches one that arrived some other way — a system color, an
    /// `NSAttributedString` default, a color smuggled in from the core.
    @Test("every color rendered is a theme token", arguments: Theme.all.map(\.id))
    func everyColorIsAToken(themeId: String) throws {
        let theme = try #require(Theme.all.first { $0.id == themeId })
        var allowed = Set(
            (theme.color.surfaces + theme.color.bodyTextTokens + theme.color.accentedTokens
                + theme.color.invertedTextBackings)
                .map { $0.color.hexString })
        allowed.formUnion(SyntaxScope.allCases.map { theme.syntax.color(for: $0).hexString })

        let used = Set(colors(MarkdownSnapshot.describe(MarkdownFixture.everything, theme: theme)))
        #expect(!used.isEmpty)
        #expect(
            used.subtracting(allowed).isEmpty,
            Comment(
                rawValue:
                    "not theme tokens: \(used.subtracting(allowed).sorted().joined(separator: ", "))"
            ))
    }

    // MARK: Helpers

    /// The snapshot with every `#RRGGBB` replaced, so two themes can be compared for shape.
    private func structure(_ snapshot: String) -> String {
        snapshot
            .replacingOccurrences(
                of: "#[0-9A-F]{6,8}", with: "#color", options: .regularExpression)
            .replacingOccurrences(of: "theme: light", with: "theme: -")
            .replacingOccurrences(of: "theme: dark", with: "theme: -")
    }

    private func colors(_ snapshot: String) -> [String] {
        let pattern = try? NSRegularExpression(pattern: "#[0-9A-F]{6,8}")
        let ns = snapshot as NSString
        return pattern?
            .matches(in: snapshot, range: NSRange(location: 0, length: ns.length))
            .map { ns.substring(with: $0.range) } ?? []
    }

    private func assertSnapshot(
        named name: String, actual: String, file: StaticString = #filePath
    ) throws {
        let directory = URL(fileURLWithPath: "\(file)")
            .deletingLastPathComponent()
            .appending(path: MarkdownSnapshotTests.goldenDirectory)
        let url = directory.appending(path: "\(name).txt")

        if ProcessInfo.processInfo.environment["FORM_UPDATE_SNAPSHOTS"] != nil {
            try FileManager.default.createDirectory(
                at: directory, withIntermediateDirectories: true)
            try actual.write(to: url, atomically: true, encoding: .utf8)
            return
        }

        guard let expected = try? String(contentsOf: url, encoding: .utf8) else {
            Issue.record(
                """
                no golden for \(name). Create it with:
                    FORM_UPDATE_SNAPSHOTS=1 swift test
                """)
            return
        }
        #expect(actual == expected, Comment(rawValue: firstDifference(expected, actual)))
    }

    /// A whole-file diff in a test failure is unreadable; the first differing line is not.
    private func firstDifference(_ expected: String, _ actual: String) -> String {
        let expectedLines = expected.components(separatedBy: "\n")
        let actualLines = actual.components(separatedBy: "\n")
        for index in 0 ..< max(expectedLines.count, actualLines.count)
        where expectedLines.indices.contains(index) != actualLines.indices.contains(index)
            || expectedLines[safe: index] != actualLines[safe: index]
        {
            return """
                snapshot differs at line \(index + 1):
                  expected: \(expectedLines[safe: index] ?? "<end of file>")
                    actual: \(actualLines[safe: index] ?? "<end of file>")
                Regenerate with FORM_UPDATE_SNAPSHOTS=1 swift test once the change is intended.
                """
        }
        return "snapshots differ"
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
