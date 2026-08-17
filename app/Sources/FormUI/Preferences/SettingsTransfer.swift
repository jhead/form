import AppKit
import FormCore
import Foundation
import UniformTypeIdentifiers

/// What the last import or export did, rendered inline in the Advanced tab.
public enum SettingsTransferReport: Sendable, Equatable {
    case exported(url: URL)
    /// `notes` are the repairs the validator had to make — an import that is *mostly* good
    /// still applies, and says what it changed (F9.3).
    case imported(url: URL, notes: [String])
    case reset
    case failure(String)

    public var isFailure: Bool {
        if case .failure = self { return true }
        return false
    }

    public var summary: String {
        switch self {
        case let .exported(url): "Exported to \(url.lastPathComponent)."
        case let .imported(url, notes):
            notes.isEmpty
                ? "Imported \(url.lastPathComponent)."
                : "Imported \(url.lastPathComponent) with \(notes.count) correction(s)."
        case .reset: "Settings reset to defaults."
        case let .failure(message): message
        }
    }

    public var notes: [String] {
        if case let .imported(_, notes) = self { return notes }
        return []
    }
}

/// A picked file, kept whole so a failed decode can still name what the user chose.
public struct PickedSettingsFile: Sendable {
    public var url: URL
    public var data: Data
}

/// Reading and writing `settings.json` by hand (F9.3).
///
/// The rule this enforces: **a bad file is reported, never silently swallowed and never
/// partially applied.** Decoding produces either a document plus the list of things that had
/// to be corrected, or a message explaining what is wrong with the file. The core does its own
/// normalization on top; this pass exists so the user finds out *before* the document is
/// replaced.
public enum SettingsTransfer {
    public enum Decoded: Sendable {
        case success(FormCore.Settings, notes: [String])
        case invalid(String)
    }

    static func encode(_ settings: FormCore.Settings) -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return (try? encoder.encode(settings)) ?? Data()
    }

    // MARK: - Export

    @MainActor
    static func export(_ settings: FormCore.Settings) throws -> SettingsTransferReport {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "form-settings.json"
        panel.allowedContentTypes = [.json]
        panel.canCreateDirectories = true
        panel.message = "Export the settings document. API keys are not included."

        guard panel.runModal() == .OK, let url = panel.url else {
            return .failure("Export cancelled.")
        }
        let data = encode(settings)
        guard !data.isEmpty else {
            return .failure("Could not encode the settings document.")
        }
        try data.write(to: url, options: .atomic)
        return .exported(url: url)
    }

    // MARK: - Import

    @MainActor
    static func pickImportFile() -> PickedSettingsFile? {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.json]
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.message = "Choose a settings JSON file."

        guard panel.runModal() == .OK, let url = panel.url,
            let data = try? Data(contentsOf: url)
        else { return nil }
        return PickedSettingsFile(url: url, data: data)
    }

    /// Validates without applying. Structural problems are fatal; out-of-range values are
    /// corrections, because refusing a whole file over one bad number is what F9.3 forbids.
    static func decode(_ file: PickedSettingsFile) -> Decoded {
        guard let json = try? JSONValue(data: file.data) else {
            return .invalid("\(file.url.lastPathComponent) is not valid JSON.")
        }
        guard json.objectValue != nil else {
            return .invalid("\(file.url.lastPathComponent) is not a settings document — the top level must be an object.")
        }

        var settings: FormCore.Settings
        do {
            settings = try JSONDecoder().decode(FormCore.Settings.self, from: file.data)
        } catch let DecodingError.typeMismatch(_, context) {
            return .invalid("Wrong type at \(path(context)) in \(file.url.lastPathComponent).")
        } catch let DecodingError.dataCorrupted(context) {
            return .invalid("Malformed value at \(path(context)) in \(file.url.lastPathComponent).")
        } catch {
            return .invalid("Could not read \(file.url.lastPathComponent): \(error.localizedDescription)")
        }

        let notes = normalize(&settings)
        return .success(settings, notes: notes)
    }

    /// The same clamps the core applies, run early so the report can name them. The core
    /// remains the authority — this only decides what to tell the user.
    static func normalize(_ settings: inout FormCore.Settings) -> [String] {
        var notes: [String] = []

        func clamp(
            _ value: inout Double, _ range: ClosedRange<Double>, _ name: String
        ) {
            guard !range.contains(value) else { return }
            let clamped = min(range.upperBound, max(range.lowerBound, value))
            notes.append("\(name) \(format(value)) clamped to \(format(clamped))")
            value = clamped
        }

        clamp(&settings.appearance.textSizeMultiplier, 0.85 ... 1.4, "appearance.textSizeMultiplier")
        clamp(
            &settings.appearance.sidebarWidth, AppearanceLimits.sidebarWidthRange,
            "appearance.sidebarWidth")

        if var size = settings.editor?.fontSize {
            clamp(&size, EditorDefaults.fontSizeRange, "editor.fontSize")
            settings.editor?.fontSize = size
        }
        if let width = settings.editor?.tabWidth,
            !EditorDefaults.tabWidthRange.contains(width) {
            let clamped = min(
                EditorDefaults.tabWidthRange.upperBound,
                max(EditorDefaults.tabWidthRange.lowerBound, width))
            notes.append("editor.tabWidth \(width) clamped to \(clamped)")
            settings.editor?.tabWidth = clamped
        }
        if var speed = settings.advanced?.harnessSpeed {
            clamp(&speed, AdvancedDefaults.harnessSpeedRange, "advanced.harnessSpeed")
            settings.advanced?.harnessSpeed = speed
        }

        // An imported document must never carry a key, even if someone hand-added one. The
        // Keychain is the only place a secret lives (F8.5).
        for (id, provider) in settings.providers where !provider.unknown.isEmpty {
            let secrets = provider.unknown.keys.filter { key in
                let lowered = key.lowercased()
                return lowered.contains("key") || lowered.contains("secret")
                    || lowered.contains("token")
            }
            guard !secrets.isEmpty else { continue }
            for secret in secrets { settings.providers[id]?.unknown[secret] = nil }
            notes.append("dropped \(secrets.count) credential-like field(s) from providers.\(id)")
        }

        return notes
    }

    private static func format(_ value: Double) -> String {
        value == value.rounded() ? String(Int(value)) : String(format: "%.2f", value)
    }

    private static func path(_ context: DecodingError.Context) -> String {
        let keys = context.codingPath.map(\.stringValue)
        return keys.isEmpty ? "the document root" : keys.joined(separator: ".")
    }
}
