import FormCore
import Foundation

/// Typed views onto settings fields the Swift mirror does not name yet.
///
/// The core's document (spec 04 §2) carries `general.telemetry`, `defaults.toolExecution` and
/// `defaults.queueMode`; `FormCore`'s `Settings` does not have properties for them, so they
/// land in the section's `unknown` bag and survive the round trip untouched. Preferences is
/// the one surface that has to *edit* them, so the accessors live here rather than in a file
/// this workstream does not own. Every default below matches the Rust `Default` impl — a
/// mismatch would show the user a value the core never agreed to.
///
/// The `editor` and `advanced` sections are optional in Swift and every field inside them is
/// optional; these accessors read through to the Rust defaults and materialize the section
/// only when something is actually set.
extension FormCore.Settings {
    // MARK: General

    var telemetry: Bool {
        get { general.unknown["telemetry"]?.boolValue ?? false }
        set { general.unknown["telemetry"] = .bool(newValue) }
    }

    var startupView: StartupView {
        get { StartupView(rawValue: general.startupView) ?? .home }
        set { general.startupView = newValue.rawValue }
    }

    // MARK: Defaults

    var toolExecution: ToolExecutionMode {
        get {
            defaults.unknown["toolExecution"]?.stringValue
                .flatMap(ToolExecutionMode.init(rawValue:)) ?? .parallel
        }
        set { defaults.unknown["toolExecution"] = .string(newValue.rawValue) }
    }

    var queueMode: QueueMode {
        get {
            defaults.unknown["queueMode"]?.stringValue.flatMap(QueueMode.init(rawValue:)) ?? .queue
        }
        set { defaults.unknown["queueMode"] = .string(newValue.rawValue) }
    }

    // MARK: Appearance

    var density: Density {
        get { appearance.density.flatMap(Density.init(rawValue:)) ?? .comfortable }
        set { appearance.density = newValue.rawValue }
    }

    // MARK: Editor

    var codeFont: String {
        get { editor?.font ?? EditorDefaults.font }
        set { mutateEditor { $0.font = newValue } }
    }

    var codeFontSize: Double {
        get { editor?.fontSize ?? EditorDefaults.fontSize }
        set { mutateEditor { $0.fontSize = newValue } }
    }

    var tabWidth: Int {
        get { editor?.tabWidth ?? EditorDefaults.tabWidth }
        set { mutateEditor { $0.tabWidth = newValue } }
    }

    var wrapCode: Bool {
        get { editor?.wrapCode ?? EditorDefaults.wrapCode }
        set { mutateEditor { $0.wrapCode = newValue } }
    }

    var showLineNumbers: Bool {
        get { editor?.showLineNumbers ?? EditorDefaults.showLineNumbers }
        set { mutateEditor { $0.showLineNumbers = newValue } }
    }

    private mutating func mutateEditor(_ body: (inout EditorSettings) -> Void) {
        var section = editor ?? EditorSettings()
        body(&section)
        editor = section
    }

    // MARK: Advanced

    var logLevel: LogLevel {
        get { advanced?.logLevel.flatMap(LogLevel.init(rawValue:)) ?? .info }
        set { mutateAdvanced { $0.logLevel = newValue.rawValue } }
    }

    var harnessSpeed: Double {
        get { advanced?.harnessSpeed ?? AdvancedDefaults.harnessSpeed }
        set { mutateAdvanced { $0.harnessSpeed = newValue } }
    }

    /// Read-only: the core stamps the real directory on every save.
    var dataDir: String { advanced?.dataDir ?? "" }

    private mutating func mutateAdvanced(_ body: (inout AdvancedSettings) -> Void) {
        var section = advanced ?? AdvancedSettings()
        body(&section)
        advanced = section
    }

    // MARK: Shortcuts

    var shortcutOverrides: [String: String] {
        get { shortcuts ?? [:] }
        set { shortcuts = newValue.isEmpty ? nil : newValue }
    }
}

// MARK: - Closed vocabularies

/// Mirrors of the core's `wire_enum!` vocabularies. An unknown string reads as the default,
/// exactly as it does in Rust, so a hand-edited file cannot put the picker in a state that
/// has no label.
enum StartupView: String, CaseIterable, Identifiable, Sendable {
    case home
    case lastSession

    var id: String { rawValue }
    var label: String {
        switch self {
        case .home: "Home"
        case .lastSession: "Last session"
        }
    }
}

enum ToolExecutionMode: String, CaseIterable, Identifiable, Sendable {
    case sequential
    case parallel

    var id: String { rawValue }
    var label: String {
        switch self {
        case .sequential: "One at a time"
        case .parallel: "In parallel"
        }
    }
}

enum QueueMode: String, CaseIterable, Identifiable, Sendable {
    case queue
    case interrupt

    var id: String { rawValue }
    var label: String {
        switch self {
        case .queue: "Queue"
        case .interrupt: "Interrupt"
        }
    }
}

enum Density: String, CaseIterable, Identifiable, Sendable {
    case comfortable
    case compact

    var id: String { rawValue }
    var label: String {
        switch self {
        case .comfortable: "Comfortable"
        case .compact: "Compact"
        }
    }
}

enum LogLevel: String, CaseIterable, Identifiable, Sendable {
    case error, warn, info, debug, trace

    var id: String { rawValue }
    var label: String { rawValue.capitalized }
}

/// The Rust `Default` impls, restated so a nil Swift section renders the value the core
/// would supply rather than a zero.
enum EditorDefaults {
    static let font = "SF Mono"
    static let fontSize: Double = 12
    static let tabWidth = 4
    static let wrapCode = false
    static let showLineNumbers = true
    static let fontSizeRange: ClosedRange<Double> = 9 ... 24
    static let tabWidthRange: ClosedRange<Int> = 1 ... 8
}

enum AdvancedDefaults {
    static let harnessSpeed: Double = 1
    static let harnessSpeedRange: ClosedRange<Double> = 0.05 ... 200
    /// What the picker offers; a hand-edited value outside the list is prepended so it is
    /// visible rather than silently snapped.
    static let harnessSpeedPresets: [Double] = [0.25, 0.5, 1, 2, 4, 10]
}

enum AppearanceLimits {
    static let sidebarWidthRange: ClosedRange<Double> = 220 ... 420
}
