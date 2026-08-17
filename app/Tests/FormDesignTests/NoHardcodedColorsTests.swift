import Foundation
import Testing

@testable import FormDesign

/// Spec 08 §5: greps the sibling UI modules for color and font literals and fails on any hit.
///
/// The rule this enforces is the one that makes F5.2 and acceptance criterion 7 true —
/// "toggling appearance repaints every surface with no hardcoded color leaking through".
/// Review cannot catch this reliably across six parallel workstreams; a test can.
///
/// `FormDesign` itself is exempt: it is where the literals are supposed to live.
struct NoHardcodedColorsTests {
    /// Directories, relative to `app/`, that must contain no literals.
    static let scannedDirectories = [
        "Sources/FormUI",
        "Sources/FormMarkdown",
        "Sources/form",
    ]

    /// A line containing this marker is skipped. It exists so a genuine exception — an
    /// `NSColor` handed to AppKit, say — is a visible, greppable decision rather than a
    /// reason to delete the test.
    static let escapeHatch = "FormDesign-allow"

    struct Rule: Sendable {
        let name: String
        let pattern: String
        let guidance: String
    }

    static let rules: [Rule] = [
        Rule(
            name: "Color(",
            pattern: #"\bColor\s*\("#,
            guidance: "read a token off @Environment(\\.theme) instead — theme.color.<token>"
        ),
        Rule(
            name: "Color.<standard>",
            pattern: #"\bColor\.(red|orange|yellow|green|mint|teal|cyan|blue|indigo|purple|pink|brown|white|gray|black|clear|primary|secondary)\b"#,
            guidance: "use a semantic token; there is no `blue` in this design system"
        ),
        Rule(
            // Bare `.red` in a style position. The lookbehind keeps `Color.red` (covered
            // above) and identifiers like `.reduce` / `.redacted` out of the results.
            name: ".<standard> as a style",
            pattern: #"(?<![\w.])\.(red|orange|yellow|green|mint|teal|cyan|blue|indigo|purple|pink|brown|white|gray|black)(?![\w(])"#,
            guidance: "use a semantic token; there is no `blue` in this design system"
        ),
        Rule(
            name: "#colorLiteral",
            pattern: ##"#colorLiteral"##,
            guidance: "delete it — literal colors cannot follow the theme"
        ),
        Rule(
            // Covers both `Font.system(size:` and the shorthand `.font(.system(size:`.
            name: "system(size:",
            pattern: #"\bsystem\s*\(\s*size\s*:"#,
            guidance: "use .typeStyle(theme.typography.<token>)"
        ),
        Rule(
            name: "NSColor(",
            pattern: #"\bNSColor\s*\("#,
            guidance: "use theme.color.<token>.nsColor"
        ),
    ]

    @Test("no color or font literals outside FormDesign")
    func siblingModulesUseTokens() throws {
        let root = try Self.appRoot()
        var violations: [String] = []
        var scannedFiles = 0

        for directory in Self.scannedDirectories {
            let url = root.appending(path: directory)
            guard FileManager.default.fileExists(atPath: url.path) else { continue }
            for file in try Self.swiftFiles(in: url) {
                scannedFiles += 1
                violations.append(contentsOf: try Self.violations(in: file, root: root))
            }
        }

        #expect(
            violations.isEmpty,
            """
            \(violations.count) color/font literal(s) outside FormDesign:

            \(violations.joined(separator: "\n"))

            Spec 08 §5. If one is genuinely unavoidable, put `\(Self.escapeHatch)` on the line \
            with a reason.
            """
        )

        // The lint is worthless if it silently scans nothing — which is exactly what happens
        // if these modules move. Guard the guard.
        #expect(scannedFiles > 0, "scanned no Swift files; did Sources/ move? \(root.path)")
    }

    @Test("the lint actually catches the patterns it claims to")
    func rulesMatchTheirOwnExamples() throws {
        let shouldMatch = [
            "Color(red: 1, green: 0, blue: 0)",
            "        .foregroundStyle(Color.red)",
            "  .foregroundStyle(.orange)",
            "let c = #colorLiteral(red: 1, green: 1, blue: 1, alpha: 1)",
            "  .font(.system(size: 13, weight: .medium))",
            "Font.system(size: 12)",
            "NSColor(named: \"x\")",
        ]
        let shouldNotMatch = [
            "let total = values.reduce(0, +)",
            "  .redacted(reason: .placeholder)",
            "theme.color.textPrimary",
            "  .typeStyle(theme.typography.body)",
            "// the greenfield case",
            "  .foregroundStyle(theme.color.danger)",
            "struct Colorway { }",
        ]

        for line in shouldMatch {
            #expect(Self.matchingRules(for: line).isEmpty == false, "should have flagged: \(line)")
        }
        for line in shouldNotMatch {
            let hits = Self.matchingRules(for: line)
            #expect(hits.isEmpty, "false positive on: \(line) — matched \(hits.map(\.name))")
        }
    }

    @Test("the escape hatch suppresses a line")
    func escapeHatchWorks() {
        let line = "  .foregroundStyle(.red) // \(Self.escapeHatch): AppKit focus ring, not themable"
        #expect(Self.matchingRules(for: line).isEmpty == false, "the rule itself must still match")
        #expect(Self.isSuppressed(line))
    }

    // MARK: Helpers

    private static func matchingRules(for line: String) -> [Rule] {
        rules.filter { rule in
            line.range(of: rule.pattern, options: .regularExpression) != nil
        }
    }

    private static func isSuppressed(_ line: String) -> Bool {
        line.contains(escapeHatch)
    }

    private static func violations(in file: URL, root: URL) throws -> [String] {
        let source = try String(contentsOf: file, encoding: .utf8)
        let relative = file.path.replacingOccurrences(of: root.path + "/", with: "")
        var found: [String] = []

        for (index, line) in source.components(separatedBy: .newlines).enumerated() {
            guard !isSuppressed(line) else { continue }
            let code = stripComment(line)
            guard !code.trimmingCharacters(in: .whitespaces).isEmpty else { continue }
            for rule in matchingRules(for: code) {
                found.append("  \(relative):\(index + 1)  \(rule.name) — \(rule.guidance)\n      \(line.trimmingCharacters(in: .whitespaces))")
            }
        }
        return found
    }

    /// Drops `//` comments so prose about colors does not trip the lint. String literals are
    /// left alone: a color name inside a string is not a color, and the patterns all require
    /// syntax a string would not usually carry.
    private static func stripComment(_ line: String) -> String {
        guard let range = line.range(of: "//") else { return line }
        return String(line[line.startIndex ..< range.lowerBound])
    }

    private static func swiftFiles(in directory: URL) throws -> [URL] {
        guard let enumerator = FileManager.default.enumerator(
            at: directory,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        ) else { return [] }

        return enumerator.compactMap { element in
            guard let url = element as? URL, url.pathExtension == "swift" else { return nil }
            return url
        }
    }

    /// Walks up from this file to `app/`. `#filePath` is the only anchor that survives both
    /// `swift test` and Xcode, whose working directories differ.
    private static func appRoot(from file: StaticString = #filePath) throws -> URL {
        var url = URL(fileURLWithPath: "\(file)")
            .deletingLastPathComponent()  // FormDesignTests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // app
        // Tolerate the test target being relocated: climb until Package.swift shows up.
        var climbs = 0
        while !FileManager.default.fileExists(atPath: url.appending(path: "Package.swift").path), climbs < 4 {
            url = url.deletingLastPathComponent()
            climbs += 1
        }
        guard FileManager.default.fileExists(atPath: url.appending(path: "Package.swift").path) else {
            throw LintError.rootNotFound("\(file)")
        }
        return url
    }

    enum LintError: Error, CustomStringConvertible {
        case rootNotFound(String)

        var description: String {
            switch self {
            case let .rootNotFound(path): "could not locate the package root above \(path)"
            }
        }
    }
}
