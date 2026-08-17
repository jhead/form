import Foundation

/// The settings document (spec 04 §2). One document, versioned, replaced wholesale by
/// `updateSettings`; the core normalizes it and echoes the result back as
/// `settings_changed`, so the app renders what comes back rather than what it sent.
///
/// **API keys are never in this document and never cross the FFI boundary.** Swift owns
/// Keychain storage (`KeychainStore`); the core records only `hasKey` per provider (F8.5).
///
/// Every level carries an `unknown` bag, mirroring the core's `#[serde(flatten)] extra`. A
/// build that dropped the keys it did not recognize would silently delete a setting made in
/// a newer build the next time the user changed anything.
///
/// Defaults here must match the Rust `Default` impls exactly: this type is also what a
/// preferences control binds to before the first `settings_changed` arrives, so a
/// disagreement would show as a toggle that jumps on load.
public struct Settings: Codable, Sendable, Equatable {
    public var version: Int
    public var general: GeneralSettings
    public var appearance: AppearanceSettings
    public var defaults: DefaultsSettings
    /// Keyed by catalog provider id. The core fills in an entry for every known provider, so
    /// the Providers tab can render without consulting the catalog for presence.
    public var providers: [String: ProviderSettings]
    public var editor: EditorSettings
    public var advanced: AdvancedSettings
    /// Overrides only: action id → key equivalent. An absent entry means "use the default"
    /// (F12.3).
    public var shortcuts: [String: String]
    public var unknown: [String: JSONValue]

    public init(
        version: Int = 1,
        general: GeneralSettings = GeneralSettings(),
        appearance: AppearanceSettings = AppearanceSettings(),
        defaults: DefaultsSettings = DefaultsSettings(),
        providers: [String: ProviderSettings] = [:],
        editor: EditorSettings = EditorSettings(),
        advanced: AdvancedSettings = AdvancedSettings(),
        shortcuts: [String: String] = [:],
        unknown: [String: JSONValue] = [:]
    ) {
        self.version = version
        self.general = general
        self.appearance = appearance
        self.defaults = defaults
        self.providers = providers
        self.editor = editor
        self.advanced = advanced
        self.shortcuts = shortcuts
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey {
        case version, general, appearance, defaults, providers, editor, advanced, shortcuts
    }

    private static let knownKeys: Set<String> = [
        "version", "general", "appearance", "defaults", "providers", "editor", "advanced",
        "shortcuts",
    ]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decodeIfPresent(Int.self, forKey: .version) ?? 1
        general = try c.decodeIfPresent(GeneralSettings.self, forKey: .general) ?? .init()
        appearance = try c.decodeIfPresent(AppearanceSettings.self, forKey: .appearance) ?? .init()
        defaults = try c.decodeIfPresent(DefaultsSettings.self, forKey: .defaults) ?? .init()
        providers = try c.decodeIfPresent([String: ProviderSettings].self, forKey: .providers) ?? [:]
        editor = try c.decodeIfPresent(EditorSettings.self, forKey: .editor) ?? .init()
        advanced = try c.decodeIfPresent(AdvancedSettings.self, forKey: .advanced) ?? .init()
        shortcuts = try c.decodeIfPresent([String: String].self, forKey: .shortcuts) ?? [:]
        unknown = try decodeUnknownKeys(from: decoder, known: Self.knownKeys)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(version, forKey: .version)
        try c.encode(general, forKey: .general)
        try c.encode(appearance, forKey: .appearance)
        try c.encode(defaults, forKey: .defaults)
        try c.encode(providers, forKey: .providers)
        try c.encode(editor, forKey: .editor)
        try c.encode(advanced, forKey: .advanced)
        try c.encode(shortcuts, forKey: .shortcuts)
        try encodeUnknownKeys(unknown, to: encoder)
    }

    public func hasKey(for providerId: String) -> Bool {
        providers[providerId]?.hasKey ?? false
    }
}

public struct GeneralSettings: Codable, Sendable, Equatable {
    public var startupView: StartupView
    public var confirmOnDelete: Bool
    public var autoTitleSessions: Bool
    /// Opt-in, off by default, and nothing reads it yet.
    public var telemetry: Bool
    public var unknown: [String: JSONValue]

    public init(
        startupView: StartupView = .home, confirmOnDelete: Bool = true,
        autoTitleSessions: Bool = true, telemetry: Bool = false,
        unknown: [String: JSONValue] = [:]
    ) {
        self.startupView = startupView
        self.confirmOnDelete = confirmOnDelete
        self.autoTitleSessions = autoTitleSessions
        self.telemetry = telemetry
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey {
        case startupView, confirmOnDelete, autoTitleSessions, telemetry
    }

    private static let knownKeys: Set<String> = [
        "startupView", "confirmOnDelete", "autoTitleSessions", "telemetry",
    ]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        startupView = try c.decodeIfPresent(StartupView.self, forKey: .startupView) ?? .home
        confirmOnDelete = try c.decodeIfPresent(Bool.self, forKey: .confirmOnDelete) ?? true
        autoTitleSessions = try c.decodeIfPresent(Bool.self, forKey: .autoTitleSessions) ?? true
        telemetry = try c.decodeIfPresent(Bool.self, forKey: .telemetry) ?? false
        unknown = try decodeUnknownKeys(from: decoder, known: Self.knownKeys)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(startupView, forKey: .startupView)
        try c.encode(confirmOnDelete, forKey: .confirmOnDelete)
        try c.encode(autoTitleSessions, forKey: .autoTitleSessions)
        try c.encode(telemetry, forKey: .telemetry)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

public struct AppearanceSettings: Codable, Sendable, Equatable {
    public var themeMode: ThemeMode
    public var textSizeMultiplier: Double
    public var sidebarWidth: Double
    public var sidebarCollapsed: Bool
    public var density: Density
    public var showTurnFooters: Bool
    public var unknown: [String: JSONValue]

    /// The core clamps to these; the app uses them for slider bounds so the value never
    /// jumps when the echo comes back.
    public static let textSizeRange: ClosedRange<Double> = 0.85...1.4
    public static let sidebarWidthRange: ClosedRange<Double> = 220...420

    public init(
        themeMode: ThemeMode = .system, textSizeMultiplier: Double = 1.0,
        sidebarWidth: Double = 300, sidebarCollapsed: Bool = false,
        density: Density = .comfortable, showTurnFooters: Bool = true,
        unknown: [String: JSONValue] = [:]
    ) {
        self.themeMode = themeMode
        self.textSizeMultiplier = textSizeMultiplier
        self.sidebarWidth = sidebarWidth
        self.sidebarCollapsed = sidebarCollapsed
        self.density = density
        self.showTurnFooters = showTurnFooters
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey {
        case themeMode, textSizeMultiplier, sidebarWidth, sidebarCollapsed, density
        case showTurnFooters
    }

    private static let knownKeys: Set<String> = [
        "themeMode", "textSizeMultiplier", "sidebarWidth", "sidebarCollapsed", "density",
        "showTurnFooters",
    ]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        themeMode = try c.decodeIfPresent(ThemeMode.self, forKey: .themeMode) ?? .system
        textSizeMultiplier = try c.decodeIfPresent(Double.self, forKey: .textSizeMultiplier) ?? 1
        sidebarWidth = try c.decodeIfPresent(Double.self, forKey: .sidebarWidth) ?? 300
        sidebarCollapsed = try c.decodeIfPresent(Bool.self, forKey: .sidebarCollapsed) ?? false
        density = try c.decodeIfPresent(Density.self, forKey: .density) ?? .comfortable
        showTurnFooters = try c.decodeIfPresent(Bool.self, forKey: .showTurnFooters) ?? true
        unknown = try decodeUnknownKeys(from: decoder, known: Self.knownKeys)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(themeMode, forKey: .themeMode)
        try c.encode(textSizeMultiplier, forKey: .textSizeMultiplier)
        try c.encode(sidebarWidth, forKey: .sidebarWidth)
        try c.encode(sidebarCollapsed, forKey: .sidebarCollapsed)
        try c.encode(density, forKey: .density)
        try c.encode(showTurnFooters, forKey: .showTurnFooters)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

public struct DefaultsSettings: Codable, Sendable, Equatable {
    /// Carries the default thinking level as part of the ref, exactly as a session does.
    public var modelRef: ModelRef
    public var systemPrompt: String
    public var toolExecution: ToolExecution
    public var queueMode: QueueMode
    public var unknown: [String: JSONValue]

    /// A system prompt longer than this is a paste accident, not a preference.
    public static let systemPromptMaxCharacters = 32_000

    public static let defaultModelRef = ModelRef(
        providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high)

    public init(
        modelRef: ModelRef = DefaultsSettings.defaultModelRef,
        systemPrompt: String = "",
        toolExecution: ToolExecution = .parallel,
        queueMode: QueueMode = .queue,
        unknown: [String: JSONValue] = [:]
    ) {
        self.modelRef = modelRef
        self.systemPrompt = systemPrompt
        self.toolExecution = toolExecution
        self.queueMode = queueMode
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey {
        case modelRef, systemPrompt, toolExecution, queueMode
    }

    private static let knownKeys: Set<String> = [
        "modelRef", "systemPrompt", "toolExecution", "queueMode",
    ]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        modelRef = try c.decodeIfPresent(ModelRef.self, forKey: .modelRef) ?? Self.defaultModelRef
        systemPrompt = try c.decodeIfPresent(String.self, forKey: .systemPrompt) ?? ""
        toolExecution =
            try c.decodeIfPresent(ToolExecution.self, forKey: .toolExecution) ?? .parallel
        queueMode = try c.decodeIfPresent(QueueMode.self, forKey: .queueMode) ?? .queue
        unknown = try decodeUnknownKeys(from: decoder, known: Self.knownKeys)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(modelRef, forKey: .modelRef)
        try c.encode(systemPrompt, forKey: .systemPrompt)
        try c.encode(toolExecution, forKey: .toolExecution)
        try c.encode(queueMode, forKey: .queueMode)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

public struct ProviderSettings: Codable, Sendable, Equatable {
    public var enabled: Bool
    public var baseUrlOverride: String?
    /// Presence only. The key itself lives in the Keychain and never crosses the boundary.
    public var hasKey: Bool
    public var unknown: [String: JSONValue]

    public init(
        enabled: Bool = true, baseUrlOverride: String? = nil, hasKey: Bool = false,
        unknown: [String: JSONValue] = [:]
    ) {
        self.enabled = enabled
        self.baseUrlOverride = baseUrlOverride
        self.hasKey = hasKey
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey { case enabled, baseUrlOverride, hasKey }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        baseUrlOverride = try c.decodeIfPresent(String.self, forKey: .baseUrlOverride)
        hasKey = try c.decodeIfPresent(Bool.self, forKey: .hasKey) ?? false
        unknown = try decodeUnknownKeys(
            from: decoder, known: ["enabled", "baseUrlOverride", "hasKey"])
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(enabled, forKey: .enabled)
        try c.encodeIfPresent(baseUrlOverride, forKey: .baseUrlOverride)
        try c.encode(hasKey, forKey: .hasKey)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

public struct EditorSettings: Codable, Sendable, Equatable {
    /// Empty means "whatever `FormDesign` calls the default monospace face".
    public var font: String
    public var fontSize: Double
    public var tabWidth: Int
    public var wrapCode: Bool
    public var showLineNumbers: Bool
    public var unknown: [String: JSONValue]

    public static let fontSizeRange: ClosedRange<Double> = 9...24
    public static let tabWidthRange: ClosedRange<Int> = 1...8

    public init(
        font: String = "SF Mono", fontSize: Double = 12, tabWidth: Int = 4,
        wrapCode: Bool = false, showLineNumbers: Bool = true,
        unknown: [String: JSONValue] = [:]
    ) {
        self.font = font
        self.fontSize = fontSize
        self.tabWidth = tabWidth
        self.wrapCode = wrapCode
        self.showLineNumbers = showLineNumbers
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey {
        case font, fontSize, tabWidth, wrapCode, showLineNumbers
    }

    private static let knownKeys: Set<String> = [
        "font", "fontSize", "tabWidth", "wrapCode", "showLineNumbers",
    ]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        font = try c.decodeIfPresent(String.self, forKey: .font) ?? "SF Mono"
        fontSize = try c.decodeIfPresent(Double.self, forKey: .fontSize) ?? 12
        tabWidth = try c.decodeIfPresent(Int.self, forKey: .tabWidth) ?? 4
        wrapCode = try c.decodeIfPresent(Bool.self, forKey: .wrapCode) ?? false
        showLineNumbers = try c.decodeIfPresent(Bool.self, forKey: .showLineNumbers) ?? true
        unknown = try decodeUnknownKeys(from: decoder, known: Self.knownKeys)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(font, forKey: .font)
        try c.encode(fontSize, forKey: .fontSize)
        try c.encode(tabWidth, forKey: .tabWidth)
        try c.encode(wrapCode, forKey: .wrapCode)
        try c.encode(showLineNumbers, forKey: .showLineNumbers)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

public struct AdvancedSettings: Codable, Sendable, Equatable {
    public var logLevel: LogLevel
    /// Multiplier on stub-harness timings; mirrors `CoreConfig.harnessSpeed`.
    public var harnessSpeed: Double
    /// Read-only display value. The core stamps it on load; the app shows it and echoes it
    /// back unchanged.
    public var dataDir: String
    public var unknown: [String: JSONValue]

    public static let harnessSpeedRange: ClosedRange<Double> = 0.05...200

    public init(
        logLevel: LogLevel = .info, harnessSpeed: Double = 1, dataDir: String = "",
        unknown: [String: JSONValue] = [:]
    ) {
        self.logLevel = logLevel
        self.harnessSpeed = harnessSpeed
        self.dataDir = dataDir
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey { case logLevel, harnessSpeed, dataDir }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        logLevel = try c.decodeIfPresent(LogLevel.self, forKey: .logLevel) ?? .info
        harnessSpeed = try c.decodeIfPresent(Double.self, forKey: .harnessSpeed) ?? 1
        dataDir = try c.decodeIfPresent(String.self, forKey: .dataDir) ?? ""
        unknown = try decodeUnknownKeys(
            from: decoder, known: ["logLevel", "harnessSpeed", "dataDir"])
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(logLevel, forKey: .logLevel)
        try c.encode(harnessSpeed, forKey: .harnessSpeed)
        try c.encode(dataDir, forKey: .dataDir)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}
